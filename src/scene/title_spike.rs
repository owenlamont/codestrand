use std::collections::BTreeMap;

use bevy::{
    asset::RenderAssetUsages,
    prelude::{
        AlphaMode, Assets, Camera, Color, Entity, GlobalTransform, Handle, IVec2,
        Image, Mesh3d, MeshMaterial3d, Quat, Rect, Resource, StandardMaterial,
        Transform, UVec2, Vec2, Vec3, Visibility, World,
    },
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use swash::{
    FontRef,
    scale::{Render, ScaleContext, Source, image::Image as GlyphImage},
    shape::ShapeContext,
};

use super::{
    Frame, Palette, Playback, Pools, camera_ui_scale, folder_transform, ground,
    project_point, reference_viewport, set_visible,
};
use crate::model;

const FIT_INTERVAL: u32 = 12;
const HOLD_SECONDS: f32 = 0.8;
const GRID_COLS: u16 = 48;
const GRID_ROWS: u16 = 32;
const FONT_PIXELS: f32 = 96.0;
const INITIAL_INK_HEIGHT: f32 = 16.0;

#[derive(Clone, Copy, PartialEq, Eq, Resource)]
pub(crate) enum Mode {
    Fitted,
    Backdrop,
}

impl Mode {
    pub(super) fn selected() -> Option<Self> {
        match std::env::var("CODESTRAND_SPIKE_TITLES").as_deref() {
            Ok("fitted") => Some(Self::Fitted),
            Ok("backdrop") => Some(Self::Backdrop),
            _ => None,
        }
    }
}

#[derive(Resource, Default)]
pub(crate) struct Samples(pub(crate) Vec<Sample>);

pub(crate) struct Sample {
    pub(crate) repository: usize,
    pub(crate) status: &'static str,
    pub(crate) center: Vec2,
    pub(crate) em: f32,
    pub(crate) retargeted: bool,
    pub(crate) reflow: f32,
}

#[derive(Resource, Default)]
struct Titles(BTreeMap<usize, TileTitle>);

struct TileTitle {
    entity: Entity,
    material: Handle<StandardMaterial>,
    dimensions: Vec2,
    frame: u32,
    bounds: Rect,
    target: Rect,
    shown: Rect,
    pending: Option<(Rect, u32)>,
}

struct Tile {
    index: usize,
    name: String,
    bounds: Rect,
    alpha: f32,
    obstacles: Vec<Rect>,
}

type LabelPlacement = (model::Label, Vec2, f32);

pub(super) fn update(
    world: &mut World,
    camera: &Camera,
    view: &GlobalTransform,
    chosen: &[LabelPlacement],
) {
    let mode = world.get_resource::<Mode>().copied();
    let metrics = world.contains_resource::<Samples>();
    if mode.is_none() && !metrics {
        return;
    }
    let factor = camera.physical_viewport_size().map_or(1.0, |size| {
        model::count_u32(size.y) / reference_viewport(camera).y
    });
    let frame = world.resource::<Frame>();
    let (index, fps) = (frame.index, frame.fps);
    let mut mesh_state = world.remove_resource::<Titles>().unwrap_or_default();
    let fit_now = mode == Some(Mode::Fitted)
        && (index.is_multiple_of(FIT_INTERVAL) || mesh_state.0.is_empty());
    let tiles = tiles(world, camera, view, chosen, fit_now, &mesh_state);
    let samples = if let Some(mode) = mode {
        update_meshes(
            world,
            camera,
            view,
            &mut mesh_state,
            tiles,
            (index, fps),
            mode,
        )
    } else {
        baseline_samples(
            tiles,
            chosen,
            world.resource::<Playback>().0.theme().label_height,
        )
    };
    if metrics {
        world.resource_mut::<Samples>().0 = samples
            .into_iter()
            .map(|sample| Sample {
                center: sample.center * factor,
                em: sample.em * factor,
                reflow: sample.reflow * factor,
                ..sample
            })
            .collect();
    }
    world.insert_resource(mesh_state);
}

fn tiles(
    world: &World,
    camera: &Camera,
    view: &GlobalTransform,
    chosen: &[LabelPlacement],
    fit_now: bool,
    mesh_state: &Titles,
) -> Vec<Tile> {
    let sim = &world.resource::<Playback>().0;
    let layout = sim.layout();
    let mut tiles: BTreeMap<usize, Tile> = layout
        .repositories
        .iter()
        .enumerate()
        .filter(|(_, region)| region.live)
        .filter_map(|(index, region)| {
            let (_, alpha, _) = sim.repository_effect(index);
            let corners = region
                .tile?
                .corners()
                .map(model::framing::field_to_world)
                .map(|point| ground(point) + Vec3::Y * model::framing::GLASS_BOTTOM);
            let bounds = projected(camera, view, corners)?;
            (alpha > 0.001 && !bounds.is_empty()).then(|| {
                (
                    index,
                    Tile {
                        index,
                        name: region.label.clone(),
                        bounds,
                        alpha,
                        obstacles: Vec::new(),
                    },
                )
            })
        })
        .collect();
    let needs_fit = |index: usize| fit_now || !mesh_state.0.contains_key(&index);
    if world.get_resource::<Mode>() == Some(&Mode::Fitted)
        && tiles.keys().any(|&index| needs_fit(index))
    {
        add_obstacles(world, camera, view, chosen, &mut tiles, needs_fit);
    }
    tiles.into_values().collect()
}

fn add_obstacles(
    world: &World,
    camera: &Camera,
    view: &GlobalTransform,
    chosen: &[LabelPlacement],
    tiles: &mut BTreeMap<usize, Tile>,
    needs_fit: impl Fn(usize) -> bool,
) {
    let sim = &world.resource::<Playback>().0;
    let layout = sim.layout();
    let lod = &world.resource::<Pools>().lod;
    let geometry = sim.theme().geometry();
    for (id, file) in layout.files() {
        let dir = layout.dir(file.dir);
        let Some(tile) = tiles
            .get_mut(&dir.repository)
            .filter(|tile| needs_fit(tile.index))
        else {
            continue;
        };
        let scale = file.draw_scale(dir) * lod.detail[file.dir];
        let (position, _, alpha) = sim.file_effect(id, file);
        if scale > 0.0
            && alpha > 0.01
            && let Some(rect) = projected(
                camera,
                view,
                model::box_corners(
                    position + geometry.file_min * scale,
                    position + geometry.file_max * scale,
                ),
            )
        {
            tile.obstacles.push(rect.inflate(1.0));
        }
    }
    for (index, dir) in layout.dirs() {
        let Some(tile) = tiles
            .get_mut(&dir.repository)
            .filter(|tile| needs_fit(tile.index))
        else {
            continue;
        };
        let detail = lod.detail[index];
        let glyph = lod.glyph[index];
        let (_, _, rise) = sim.repository_effect(dir.repository);
        if detail > 0.0 {
            let center = ground(dir.position) + Vec3::Y * (rise - 0.05);
            let half = sim.theme().directory_node_scale * (dir.scale * detail * 0.5);
            if let Some(rect) = projected(
                camera,
                view,
                model::box_corners(center - half, center + half),
            ) {
                tile.obstacles.push(rect.inflate(1.0));
            }
        }
        if glyph > 0.0 {
            let folder =
                folder_transform(sim.theme(), dir.position, lod.size[index], glyph);
            let center = folder.translation + Vec3::Y * rise;
            let half = folder.scale;
            if let Some(rect) = projected(
                camera,
                view,
                model::box_corners(center - half, center + half),
            ) {
                tile.obstacles.push(rect.inflate(1.0));
            }
        }
    }
    for (label, point, _) in chosen.iter().filter(|(label, ..)| {
        matches!(
            label.kind,
            model::LabelKind::Landmark
                | model::LabelKind::Activity
                | model::LabelKind::Detail
        )
    }) {
        let rect = model::label_rect(
            *point,
            &label.text,
            label.scale,
            sim.theme().label_height,
        );
        for tile in tiles.values_mut().filter(|tile| needs_fit(tile.index)) {
            if !tile.bounds.intersect(rect).is_empty() {
                tile.obstacles.push(rect.inflate(1.0));
            }
        }
    }
}

fn projected(
    camera: &Camera,
    view: &GlobalTransform,
    points: impl IntoIterator<Item = Vec3>,
) -> Option<Rect> {
    let mut points = points
        .into_iter()
        .filter_map(|point| project_point(camera, view, point));
    let first = points.next()?;
    Some(
        points.fold(Rect::from_corners(first, first), |bounds, point| {
            Rect::from_corners(bounds.min.min(point), bounds.max.max(point))
        }),
    )
}

fn baseline_samples(
    tiles: Vec<Tile>,
    chosen: &[LabelPlacement],
    height: f32,
) -> Vec<Sample> {
    tiles
        .into_iter()
        .map(|tile| {
            let placement = chosen
                .iter()
                .filter(|(label, point, _)| {
                    label.kind == model::LabelKind::Repository
                        && point.cmpge(tile.bounds.min - Vec2::splat(3.0)).all()
                        && point.cmple(tile.bounds.max + Vec2::splat(3.0)).all()
                        && (label.text == tile.name
                            || label
                                .text
                                .strip_suffix('…')
                                .is_some_and(|prefix| tile.name.starts_with(prefix)))
                })
                .max_by(|(a, ..), (b, ..)| a.opacity.total_cmp(&b.opacity));
            let Some((label, point, _)) = placement else {
                return Sample {
                    repository: tile.index,
                    status: "hidden",
                    center: Vec2::ZERO,
                    em: 0.0,
                    retargeted: false,
                    reflow: 0.0,
                };
            };
            Sample {
                repository: tile.index,
                status: if label.text == tile.name {
                    "full"
                } else {
                    "elided"
                },
                center: model::label_rect(*point, &label.text, label.scale, height)
                    .center(),
                em: 14.0 * label.scale,
                retargeted: false,
                reflow: 0.0,
            }
        })
        .collect()
}

fn update_meshes(
    world: &mut World,
    camera: &Camera,
    view: &GlobalTransform,
    mesh_state: &mut Titles,
    tiles: Vec<Tile>,
    (frame, fps): (u32, u16),
    mode: Mode,
) -> Vec<Sample> {
    for (&index, title) in &mesh_state.0 {
        if !tiles.iter().any(|tile| tile.index == index) {
            set_visible(world, title.entity, false);
        }
    }
    let mut samples = Vec::with_capacity(tiles.len());
    for tile in tiles {
        let title = mesh_state
            .0
            .entry(tile.index)
            .or_insert_with(|| spawn(world, &tile, mode, frame));
        let bounds = tile
            .bounds
            .inflate(-2.0 * tile.bounds.size().min_element().min(24.0) / 24.0);
        let aspect = title.dimensions.x / title.dimensions.y;
        let changed_frame = title.frame != frame;
        let previous_center = title.shown.center();
        if changed_frame {
            title.shown = rebase(title.shown, title.bounds, bounds);
            title.target = rebase(title.target, title.bounds, bounds);
            if let Some((pending, _)) = &mut title.pending {
                *pending = rebase(*pending, title.bounds, bounds);
            }
            title.bounds = bounds;
        }
        let reflow = title.shown.center().distance(previous_center);
        let resample = changed_frame
            && (mode == Mode::Backdrop || frame.is_multiple_of(FIT_INTERVAL));
        let mut retargeted = false;
        if resample {
            let next = target(&tile, bounds, aspect, mode);
            let threshold = if mode == Mode::Backdrop { 3.0 } else { 8.0 };
            let different = (next.center() - title.target.center()).length()
                > threshold
                || (next.height() - title.target.height()).abs()
                    > 2.0_f32.max(title.target.height() * 0.10);
            if different {
                let (pending, since) = title.pending.get_or_insert((next, frame));
                if mode == Mode::Fitted
                    && next.center().distance(pending.center()) > 12.0
                {
                    (*pending, *since) = (next, frame);
                }
                if model::count_u32(frame.saturating_sub(*since))
                    >= HOLD_SECONDS * f32::from(fps)
                {
                    title.target = next;
                    title.pending = None;
                    retargeted = true;
                }
            } else {
                title.pending = None;
            }
        }
        if changed_frame {
            let dt =
                model::count_u32(frame.saturating_sub(title.frame)) / f32::from(fps);
            let ease = 1.0 - (-2.5 * dt).exp();
            let movement = (title.target.center() - title.shown.center()) * ease;
            let center = title.shown.center() + movement.clamp_length_max(38.0 * dt);
            let size_change = (title.target.height() - title.shown.height()) * ease;
            let height =
                title.shown.height() + size_change.clamp(-60.0 * dt, 60.0 * dt);
            title.shown =
                Rect::from_center_size(center, Vec2::new(height * aspect, height));
            title.frame = frame;
        }
        let center = title.shown.center();
        let half = title.shown.size() * 0.5;
        let room = (center - bounds.min).min(bounds.max - center);
        let scale = (room / half.max(Vec2::splat(0.001)))
            .min_element()
            .clamp(0.0, 1.0);
        let visible = scale > 0.0;
        let shown = Rect::from_center_size(center, title.shown.size() * scale);
        if visible && let Some(transform) = mesh_transform(camera, view, shown, mode) {
            world
                .entity_mut(title.entity)
                .insert((transform, GlobalTransform::from(transform)));
            let color = Color::srgba(0.69, 0.79, 0.93, tile.alpha * alpha(mode));
            world
                .resource_mut::<Assets<StandardMaterial>>()
                .get_mut(&title.material)
                .expect("title material")
                .base_color = color;
        }
        set_visible(world, title.entity, visible);
        samples.push(Sample {
            repository: tile.index,
            status: if visible { "full" } else { "hidden" },
            center,
            em: shown.height() * FONT_PIXELS / title.dimensions.y,
            retargeted,
            reflow,
        });
    }
    samples
}

fn rebase(rect: Rect, old: Rect, new: Rect) -> Rect {
    let relative = (rect.center() - old.min) / old.size();
    Rect::from_center_size(new.min + relative * new.size(), rect.size())
}

const fn alpha(mode: Mode) -> f32 {
    match mode {
        Mode::Fitted => 0.91,
        Mode::Backdrop => 0.24,
    }
}

fn mesh_transform(
    camera: &Camera,
    view: &GlobalTransform,
    rect: Rect,
    mode: Mode,
) -> Option<Transform> {
    let height = match mode {
        Mode::Fitted => 2.2,
        Mode::Backdrop => -0.075,
    };
    let origin = camera.logical_viewport_rect()?.min;
    let plane = |point| {
        let ray = camera
            .viewport_to_world(view, origin + point * camera_ui_scale(camera))
            .ok()?;
        Some(ray.get_point((height - ray.origin.y) / ray.direction.y))
    };
    let a = plane(rect.min)?;
    let b = plane(rect.max)?;
    Some(
        Transform::from_translation(a.midpoint(b))
            .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
            .with_scale(Vec3::new((b.x - a.x).abs(), (b.z - a.z).abs(), 1.0)),
    )
}

fn spawn(world: &mut World, tile: &Tile, mode: Mode, frame: u32) -> TileTitle {
    let image = raster(&tile.name);
    let dimensions = Vec2::new(
        model::count_u32(image.width()),
        model::count_u32(image.height()),
    );
    let texture = world.resource_mut::<Assets<Image>>().add(image);
    let material =
        world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color: Color::srgba(0.69, 0.79, 0.93, alpha(mode)),
                base_color_texture: Some(texture),
                unlit: true,
                alpha_mode: AlphaMode::Blend,
                ..Default::default()
            });
    let mesh = world.resource::<Palette>().glyph_mesh.clone();
    let entity = world
        .spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material.clone()),
            Transform::default(),
            Visibility::Hidden,
        ))
        .id();
    let bounds = tile
        .bounds
        .inflate(-2.0 * tile.bounds.size().min_element().min(24.0) / 24.0);
    let target = target(tile, bounds, dimensions.x / dimensions.y, mode);
    let shown = Rect::from_center_size(
        target.center(),
        target.size() * (INITIAL_INK_HEIGHT / target.height()).min(1.0),
    );
    TileTitle {
        entity,
        material,
        dimensions,
        frame,
        bounds,
        target,
        shown,
        pending: None,
    }
}

fn target(tile: &Tile, bounds: Rect, aspect: f32, mode: Mode) -> Rect {
    let empty = if mode == Mode::Fitted {
        largest_empty(bounds, &tile.obstacles, aspect)
    } else {
        bounds
    };
    let height = empty.height().min(empty.width() / aspect);
    Rect::from_center_size(empty.center(), Vec2::new(height * aspect, height))
}

fn largest_empty(bounds: Rect, obstacles: &[Rect], aspect: f32) -> Rect {
    if obstacles
        .iter()
        .all(|obstacle| bounds.intersect(*obstacle).is_empty())
    {
        return bounds;
    }
    let cell = bounds.size() / Vec2::new(f32::from(GRID_COLS), f32::from(GRID_ROWS));
    let columns = (1_u64 << GRID_COLS) - 1;
    let mut available = [columns; GRID_ROWS as usize];
    for rect in obstacles
        .iter()
        .map(|rect| bounds.intersect(*rect))
        .filter(|rect| !rect.is_empty())
    {
        let low = ((rect.min - bounds.min) / cell).floor();
        let high = ((rect.max - bounds.min) / cell).ceil();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clipped to the finite grid"
        )]
        let (left, right, top, bottom) = (
            low.x as u16,
            (high.x as u16).min(GRID_COLS),
            low.y as u16,
            (high.y as u16).min(GRID_ROWS),
        );
        let blocked = ((1_u64 << right) - 1) ^ ((1_u64 << left) - 1);
        for row in &mut available[usize::from(top)..usize::from(bottom)] {
            *row &= !blocked;
        }
    }
    let mut best = Rect::from_center_size(bounds.center(), Vec2::ZERO);
    let mut best_height = 0.0;
    let mut best_displacement = f32::INFINITY;
    for top in 0..GRID_ROWS {
        let mut open = columns;
        for bottom in top..GRID_ROWS {
            open &= available[usize::from(bottom)];
            if open == 0 {
                break;
            }
            let height = f32::from(bottom + 1 - top) * cell.y;
            let mut runs = open;
            while runs != 0 {
                let left = runs.trailing_zeros();
                let width = (runs >> left).trailing_ones();
                runs &= !(((1_u64 << width) - 1) << left);
                let fitted = height.min(model::count_u32(width) * cell.x / aspect);
                let edges = Rect::from_corners(
                    bounds.min
                        + cell * Vec2::new(model::count_u32(left), f32::from(top)),
                    bounds.min
                        + cell
                            * Vec2::new(
                                model::count_u32(left + width),
                                f32::from(bottom + 1),
                            ),
                );
                let displacement = edges.center().distance_squared(bounds.center());
                if fitted > best_height
                    || (fitted.to_bits() == best_height.to_bits()
                        && displacement < best_displacement)
                {
                    (best, best_height, best_displacement) =
                        (edges, fitted, displacement);
                }
            }
        }
    }
    if best.is_empty() {
        return Rect::from_center_size(bounds.center(), bounds.size() * 0.2);
    }
    best
}

fn raster(text: &str) -> Image {
    let font = FontRef::from_index(
        include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf"),
        0,
    )
    .expect("bundled font");
    let normalized: String = text
        .chars()
        .map(|ch| {
            if font.charmap().map(ch) == 0 {
                '�'
            } else {
                ch
            }
        })
        .collect();
    let mut context = ShapeContext::new();
    let mut shaper = context.builder(font).size(FONT_PIXELS).build();
    shaper.add_str(&normalized);
    let mut scaler = ScaleContext::new();
    let mut scaler = scaler.builder(font).size(FONT_PIXELS).build();
    let mut cursor = 0.0;
    let mut glyphs = Vec::<(IVec2, GlyphImage)>::new();
    shaper.shape_with(|cluster| {
        for glyph in cluster.glyphs {
            if let Some(image) =
                Render::new(&[Source::Outline]).render(&mut scaler, glyph.id)
            {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "raster glyphs land at integer texels"
                )]
                let offset = IVec2::new(
                    (cursor + glyph.x).round() as i32,
                    -glyph.y.round() as i32,
                );
                glyphs.push((
                    offset + IVec2::new(image.placement.left, -image.placement.top),
                    image,
                ));
            }
            cursor += glyph.advance;
        }
    });
    let mut low = IVec2::ZERO;
    let mut high = IVec2::ONE;
    for (offset, glyph) in &glyphs {
        low = low.min(*offset);
        high = high.max(
            *offset
                + UVec2::new(glyph.placement.width, glyph.placement.height).as_ivec2(),
        );
    }
    let size = (high - low).as_uvec2();
    let mut rgba = vec![0; (size.x * size.y * 4) as usize];
    for (offset, glyph) in glyphs {
        let start = (offset - low).as_uvec2();
        for y in 0..glyph.placement.height {
            for x in 0..glyph.placement.width {
                let pixel = (((start.y + y) * size.x + start.x + x) * 4) as usize;
                rgba[pixel..pixel + 4].copy_from_slice(&[
                    255,
                    255,
                    255,
                    glyph.data[(y * glyph.placement.width + x) as usize],
                ]);
            }
        }
    }
    Image::new(
        Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}
