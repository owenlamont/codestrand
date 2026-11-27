use bevy::{
    asset::RenderAssetUsages, mesh::Indices, prelude::*,
    render::render_resource::PrimitiveTopology,
};
use lyon_tessellation::path::iterator::PathIterator;
use lyon_tessellation::{
    FillGeometryBuilder, FillOptions, FillRule, FillTessellator, FillVertex,
    GeometryBuilder, GeometryBuilderError, VertexId, math::point, path::Path,
};
use parley::{
    FontContext, FontFamily, Layout, LayoutContext, PositionedLayoutItem,
    StyleProperty,
    fontique::{Collection, CollectionOptions, SourceCache},
};
use swash::{
    FontRef, TableProvider,
    scale::{ScaleContext, outline::Outline},
    tag_from_bytes,
    zeno::{Command, PathData},
};

use crate::{layout::packing::Tile, model::Simulation};

use super::names::{CLIP_PLANES, DEPTH_PLANES, View, project_quad};

const MESH_BYTES: usize = 64 * 1024 * 1024;
const MAX_TITLE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TITLE_CHARS: usize = 1024;
const MAX_GLYPH_POINTS: usize = 4096;
const MAX_GLYPH_EDGES: usize = 4096;
const MAX_TITLE_EDGES: usize = 65_536;
const TOLERANCE: f32 = 0.00005;
const BUNDLED_FONT: &[u8] =
    include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf");
// Scaling outlines at one pixel per em loses precision before tessellation.
const SHAPING_SIZE: f32 = 1024.0;
const OPACITY: f32 = 0.18;
const PADDING: f32 = 0.04;
const GLYPH_PADDING: f32 = 0.08;
const MIN_INK_PIXELS: f32 = 4.0;
const MIN_CLEARANCE_PIXELS: f32 = 1.0;
const MIN_INK_CLEARANCE: f32 = 0.06;
const PLANE: f32 = crate::model::framing::GLASS_BOTTOM;

#[derive(Component)]
struct Title {
    repository: usize,
    ink: Vec2,
}

pub(super) fn placement(tile: Tile, ink: Vec2) -> Transform {
    let [a, _, b, _] = tile.ground();
    let size = (b - a).abs();
    let scale = (size * (1.0 - 2.0 * PADDING) / ink)
        .min(size / (ink + Vec2::splat(2.0 * GLYPH_PADDING * ink.y)))
        .min_element();
    Transform::from_translation(super::ground(a.midpoint(b)).with_y(PLANE))
        .with_scale(Vec3::new(scale, 1.0, scale))
}

fn screen_line(a: Vec4, b: Vec4, half_viewport: Vec2) -> Vec3 {
    // Project the edge as a line even when an endpoint is behind the camera.
    let homogeneous = |point: Vec4| (point.xy() * half_viewport).extend(point.w);
    let line = homogeneous(a).cross(homogeneous(b));
    line / line.xy().length()
}

fn screen_span(points: &[Vec2], normal: Vec2) -> f32 {
    let (low, high) = points
        .iter()
        .map(|&point| point.dot(normal))
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), value| {
            (low.min(value), high.max(value))
        });
    high - low
}

fn screen_fit(
    view: &View,
    tile: Tile,
    placed: Transform,
    ink: Vec2,
) -> Option<(f32, f32)> {
    let clip = |point: Vec3| view.clip_from_world * point.extend(1.0);
    let pane = tile.ground().map(|p| clip(super::ground(p).with_y(PLANE)));
    let half = ink * 0.5;
    let corners = [
        Vec2::new(-half.x, half.y),
        half,
        Vec2::new(half.x, -half.y),
        -half,
    ];
    let ink = corners.map(|p| clip(placed.transform_point(super::ground(p))));
    let half_viewport = view.viewport * 0.5;
    let mut visible = project_quad(ink, &CLIP_PLANES)?;
    visible.map(|point| point * half_viewport);
    let visible = visible.points();
    let line = |a, b| screen_line(a, b, half_viewport);
    let top = line(ink[0], ink[1]);
    let bottom = line(ink[2], ink[3]);
    let projected_height =
        |points| screen_span(points, top.xy()).min(screen_span(points, bottom.xy()));
    let mut height = projected_height(visible);
    if ink
        .iter()
        .all(|&point| DEPTH_PLANES.iter().all(|plane| plane.dot(point) >= 0.0))
    {
        // Preserve the minimum edge separation for fully projectable ink.
        let distance = |line: Vec3, point: Vec4| {
            line.dot((point.xy() / point.w * half_viewport).extend(1.0))
                .abs()
        };
        height = height
            .min(distance(top, ink[2]))
            .min(distance(top, ink[3]))
            .min(distance(bottom, ink[0]))
            .min(distance(bottom, ink[1]));
    }
    // Viewport and near-plane cuts do not form physical pane edges.
    let pane_edges: [Vec3; 4] =
        std::array::from_fn(|i| line(pane[i], pane[(i + 1) % 4]));
    let clearance = pane_edges
        .into_iter()
        .fold(f32::INFINITY, |clearance, edge| {
            visible.iter().fold(clearance, |clearance, &point| {
                clearance.min(edge.dot(point.extend(1.0)).abs())
            })
        });
    let required = minimum_clearance(height);
    if clearance < required
        && ink
            .iter()
            .any(|&point| CLIP_PLANES.iter().any(|plane| plane.dot(point) < 0.0))
    {
        // A grazing view can put one end too close to a pane edge while leaving
        // plenty of readable space elsewhere. Measure the visible region with clearance.
        let mut planes = [Vec4::ZERO; 10];
        planes[..6].copy_from_slice(&CLIP_PLANES);
        for (plane, edge) in planes[6..].iter_mut().zip(pane_edges) {
            let inside = edge.dot(visible[0].extend(1.0)).signum();
            let edge = edge * inside;
            let normal = edge.xy() * half_viewport;
            *plane = Vec4::new(normal.x, normal.y, 0.0, edge.z - required);
        }
        let mut inset = project_quad(ink, &planes)?;
        inset.map(|point| point * half_viewport);
        return Some((height.min(projected_height(inset.points())), required));
    }
    Some((height, clearance))
}

fn minimum_clearance(height: f32) -> f32 {
    (height * MIN_INK_CLEARANCE).max(MIN_CLEARANCE_PIXELS)
}

fn legible(view: &View, tile: Tile, placed: Transform, ink: Vec2) -> bool {
    screen_fit(view, tile, placed, ink).is_some_and(|(height, clearance)| {
        height >= MIN_INK_PIXELS && clearance >= minimum_clearance(height)
    })
}

pub(super) fn setup(world: &mut World) {
    let labels = world
        .resource::<super::Playback>()
        .0
        .layout()
        .region_labels();
    if labels.is_empty() {
        return;
    }
    let mut fonts = title_fonts();
    let mut shaper = LayoutContext::new();
    let mut scaler = ScaleContext::new();
    let mut tessellator = FillTessellator::new();
    let mut remaining = MESH_BYTES;
    let mut omitted = 0;
    for (repository, label) in labels.iter().enumerate() {
        let Some((mesh, ink, bytes)) = title_mesh(
            label,
            &mut fonts,
            &mut shaper,
            &mut scaler,
            &mut tessellator,
            remaining.min(MAX_TITLE_BYTES),
        ) else {
            omitted += 1;
            continue;
        };
        remaining -= bytes;
        let mesh = world.resource_mut::<Assets<Mesh>>().add(mesh);
        let color = world.resource::<super::Playback>().0.theme().label;
        let material =
            world
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial {
                    base_color: color.with_alpha(OPACITY),
                    alpha_mode: AlphaMode::Blend,
                    unlit: true,
                    cull_mode: None,
                    ..default()
                });
        super::spawn_in_field(
            world,
            (
                Title { repository, ink },
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::default(),
                Visibility::Hidden,
            ),
        );
    }
    if omitted > 0 {
        warn!(
            "{omitted} repository titles had no usable outline or exceeded the mesh budget"
        );
    }
}

fn title_fonts() -> FontContext {
    let mut collection = Collection::new(CollectionOptions {
        shared: false,
        system_fonts: false,
    });
    collection.register_fonts(BUNDLED_FONT.to_vec().into(), None);
    FontContext {
        collection,
        source_cache: SourceCache::default(),
    }
}

pub(super) fn sync(world: &mut World, simulation: &Simulation) {
    let view = world
        .query_filtered::<(&Camera, &Transform, &Projection), With<super::SceneCamera>>(
        )
        .single(world)
        .ok()
        .map(|(camera, transform, projection)| super::names::View {
            clip_from_world: projection.get_clip_from_view()
                * transform.to_matrix().inverse()
                * simulation.field_space().transform().to_matrix(),
            viewport: camera
                .physical_viewport_size()
                .map_or(crate::model::framing::VIEWPORT, UVec2::as_vec2),
        });
    let mut query = world.query::<(
        &Title,
        &MeshMaterial3d<StandardMaterial>,
        &mut Transform,
        &mut Visibility,
    )>();
    world.resource_scope(|world, mut materials: Mut<Assets<StandardMaterial>>| {
        for (title, material, mut transform, mut visibility) in query.iter_mut(world) {
            let Some(tile) = simulation.visible_repository_tile(title.repository)
            else {
                visibility.set_if_neq(Visibility::Hidden);
                continue;
            };
            let placed = placement(tile, title.ink);
            let (_, opacity, _) = simulation.repository_effect(title.repository);
            let color = simulation.theme().label.with_alpha(OPACITY * opacity);
            if let Some(current) = materials.get(&material.0)
                && current.base_color != color
            {
                materials
                    .get_mut(&material.0)
                    .expect("title material")
                    .base_color = color;
            }
            transform.set_if_neq(placed);
            visibility.set_if_neq(
                if view
                    .as_ref()
                    .is_some_and(|view| legible(view, tile, placed, title.ink))
                {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                },
            );
        }
    });
}

fn title_mesh(
    text: &str,
    fonts: &mut FontContext,
    shaper: &mut LayoutContext<()>,
    scale: &mut ScaleContext,
    tessellator: &mut FillTessellator,
    limit: usize,
) -> Option<(Mesh, Vec2, usize)> {
    if limit == 0 || text.chars().nth(MAX_TITLE_CHARS).is_some() {
        return None;
    }
    let bundled = FontRef::from_index(BUNDLED_FONT, 0)?;
    check_outline_limits(bundled, MAX_GLYPH_POINTS)?;
    let charmap = bundled.charmap();
    let text: String = text
        .chars()
        .map(|ch| match ch {
            '\r' | '\n' | '\t' => ' ',
            ch if charmap.map(ch) == 0 => '?',
            ch => ch,
        })
        .collect();
    let mut builder = shaper.ranged_builder(fonts, &text, 1.0, false);
    builder.push_default(StyleProperty::FontFamily(FontFamily::named(
        "IBM Plex Mono",
    )));
    builder.push_default(StyleProperty::FontSize(SHAPING_SIZE));
    let mut layout: Layout<()> = builder.build(&text);
    layout.break_all_lines(None);

    let mut geometry = Geometry {
        remaining: limit,
        ..default()
    };
    let mut edges = MAX_TITLE_EDGES;
    for item in layout.lines().flat_map(|line| line.items()) {
        let PositionedLayoutItem::GlyphRun(glyphs) = item else {
            continue;
        };
        let run = glyphs.run();
        let font = run.font();
        if run.font_size() != SHAPING_SIZE {
            return None;
        }
        let font_ref = FontRef::from_index(font.data.as_ref(), font.index as usize)?;
        let mut scaler = scale
            .builder_with_id(font_ref, [font.data.id(), font.index.into()])
            .size(run.font_size())
            .normalized_coords(run.normalized_coords())
            .build();
        for glyph in glyphs.positioned_glyphs() {
            let Some(outline) = u16::try_from(glyph.id)
                .ok()
                .and_then(|id| scaler.scale_outline(id))
            else {
                continue;
            };
            if outline.points().len() > MAX_GLYPH_POINTS
                || outline.points().iter().any(|p| {
                    !Vec2::new(p.x, p.y).is_finite()
                        || p.x.abs().max(p.y.abs()) > 8.0 * SHAPING_SIZE
                })
            {
                return None;
            }
            let path = path(&outline);
            let flattened = || path.iter().flattened(TOLERANCE);
            let allowed = edges.min(MAX_GLYPH_EDGES);
            let used = flattened().take(allowed + 1).count();
            edges = edges.checked_sub(used).filter(|_| used <= allowed)?;
            geometry.offset = Vec2::new(glyph.x, -glyph.y) / SHAPING_SIZE;
            tessellator
                .tessellate(
                    flattened(),
                    &FillOptions::tolerance(TOLERANCE)
                        .with_fill_rule(FillRule::NonZero),
                    &mut geometry,
                )
                .ok()?;
            if geometry.exhausted {
                return None;
            }
        }
    }

    mesh_from_geometry(geometry, limit)
}

fn mesh_from_geometry(
    mut geometry: Geometry,
    limit: usize,
) -> Option<(Mesh, Vec2, usize)> {
    let to_ink = |&[x, _, z]: &[f32; 3]| Vec2::new(x, z);
    let first = to_ink(geometry.positions.first()?);
    let (low, high) = geometry
        .positions
        .iter()
        .map(to_ink)
        .fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p)));
    let ink = high - low;
    if !ink.is_finite() || ink.min_element() <= 0.0 {
        return None;
    }
    let center = low.midpoint(high);
    for [x, _, z] in &mut geometry.positions {
        *x -= center.x;
        *z -= center.y;
    }
    let mut normals = Vec::new();
    reserve(
        &mut normals,
        geometry.positions.len(),
        &mut geometry.remaining,
    )?;
    normals.resize(geometry.positions.len(), Vec3::Y.to_array());
    // Bevy packs these slices into staging buffers when it uploads the mesh.
    let upload = geometry.positions.len() * size_of::<[f32; 6]>()
        + geometry.indices.len() * size_of::<u32>();
    let bytes = limit - geometry.remaining.checked_sub(upload)?;
    let mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, geometry.positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_indices(Indices::U32(geometry.indices));
    Some((mesh, ink, bytes))
}

fn check_outline_limits(font: FontRef<'_>, max_points: usize) -> Option<()> {
    // The embedded TrueType font's maxp table bounds simple and expanded composite glyphs.
    let maxp = font.table_by_tag(tag_from_bytes(b"maxp"))?;
    let head = font.table_by_tag(tag_from_bytes(b"head"))?;
    let read = |table: &[u8], offset| {
        Some(u16::from_be_bytes(
            table.get(offset..offset + 2)?.try_into().ok()?,
        ))
    };
    let points = read(maxp, 6)?.max(read(maxp, 10)?);
    let em = read(head, 18)?;
    let (bounds, _) = head.get(36..44)?.as_chunks::<2>();
    (maxp.starts_with(&[0, 1, 0, 0])
        && font.table_by_tag(tag_from_bytes(b"glyf")).is_some()
        && usize::from(points) <= max_points
        && em != 0
        && bounds.iter().all(|&bytes| {
            u32::from(i16::from_be_bytes(bytes).unsigned_abs()) <= u32::from(em) * 8
        }))
    .then_some(())
}

fn reserve<T>(
    values: &mut Vec<T>,
    additional: usize,
    remaining: &mut usize,
) -> Option<()> {
    let required = values.len() + additional;
    let old = values.capacity();
    if required <= old {
        return Some(());
    }
    let capacity = required.max(old * 2);
    // Growth can require both allocations until the old contents have moved.
    remaining.checked_sub(capacity * size_of::<T>())?;
    values.try_reserve_exact(capacity - values.len()).ok()?;
    *remaining = remaining.checked_sub((values.capacity() - old) * size_of::<T>())?;
    Some(())
}

#[derive(Default)]
struct Geometry {
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
    offset: Vec2,
    remaining: usize,
    exhausted: bool,
}

impl GeometryBuilder for Geometry {
    fn add_triangle(&mut self, a: VertexId, b: VertexId, c: VertexId) {
        if !self.exhausted
            && reserve(&mut self.indices, 3, &mut self.remaining).is_some()
        {
            self.indices.extend([a.0, b.0, c.0]);
        } else {
            self.exhausted = true;
        }
    }
}

impl FillGeometryBuilder for Geometry {
    fn add_fill_vertex(
        &mut self,
        vertex: FillVertex,
    ) -> Result<VertexId, GeometryBuilderError> {
        if self.exhausted {
            return Err(GeometryBuilderError::TooManyVertices);
        }
        reserve(&mut self.positions, 1, &mut self.remaining)
            .ok_or(GeometryBuilderError::TooManyVertices)?;
        let index =
            u32::try_from(self.positions.len()).expect("mesh budget fits u32 indices");
        let [x, y] =
            (Vec2::from_array(vertex.position().to_array()) + self.offset).to_array();
        self.positions.push([x, 0.0, -y]);
        Ok(VertexId(index))
    }
}

fn path(outline: &Outline) -> Path {
    let mut builder = Path::builder();
    let point = |p: swash::zeno::Point| point(p.x / SHAPING_SIZE, p.y / SHAPING_SIZE);
    let mut open = false;
    for command in outline.path().commands() {
        match command {
            Command::MoveTo(p) => {
                if open {
                    builder.end(true);
                }
                builder.begin(point(p));
                open = true;
            }
            Command::LineTo(p) => {
                builder.line_to(point(p));
            }
            Command::QuadTo(a, b) => {
                builder.quadratic_bezier_to(point(a), point(b));
            }
            Command::CurveTo(a, b, c) => {
                builder.cubic_bezier_to(point(a), point(b), point(c));
            }
            Command::Close => {
                builder.end(true);
                open = false;
            }
        }
    }
    if open {
        builder.end(true);
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{Playback, SceneCamera};

    #[test]
    fn thin_title_tiles_keep_a_buffer_on_both_axes() {
        let tile = Tile {
            low: Vec2::ZERO,
            high: Vec2::new(1.0, 0.05),
        };
        let ink = Vec2::new(40.0, 1.0);
        let [a, _, b, _] = tile.ground();
        let size = (b - a).abs();
        let drawn = ink * placement(tile, ink).scale.x;
        assert!(
            (size - drawn)
                .cmpge(size * (2.0 * PADDING) - Vec2::splat(1e-5))
                .all()
        );
        assert!((size - drawn).min_element() >= 2.0 * GLYPH_PADDING * drawn.y - 1e-5);
    }

    fn mesh_for_test(text: &str, budget: usize) -> Option<(Mesh, Vec2, usize)> {
        title_mesh(
            text,
            &mut title_fonts(),
            &mut LayoutContext::new(),
            &mut ScaleContext::new(),
            &mut FillTessellator::new(),
            budget,
        )
    }

    #[test]
    fn uncovered_characters_use_the_bundled_replacement() {
        let positions = |text| {
            let (mesh, _, _) = mesh_for_test(text, MAX_TITLE_BYTES).unwrap();
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
                .unwrap()
                .as_float3()
                .unwrap()
                .to_vec()
        };
        assert_eq!(positions("a漢b"), positions("a?b"));
    }

    #[test]
    fn title_budget_covers_allocated_capacity() {
        let (mesh, _, _) = mesh_for_test("Repository", MAX_TITLE_BYTES).unwrap();
        let payload = mesh.count_vertices() * size_of::<[f32; 6]>()
            + mesh.indices().unwrap().len() * size_of::<u32>();
        assert!(mesh_for_test("Repository", payload).is_none());
    }

    #[test]
    fn outline_and_flattened_edge_limits_omit_oversize_geometry() {
        let font = FontRef::from_index(BUNDLED_FONT, 0).unwrap();
        assert!(check_outline_limits(font, MAX_GLYPH_POINTS).is_some());
        assert!(check_outline_limits(font, 1).is_none());
        assert!(mesh_for_test("@", MAX_TITLE_BYTES).is_some());
        assert!(mesh_for_test(&"@".repeat(MAX_TITLE_CHARS), usize::MAX).is_none());
        assert!(mesh_for_test("next title", MAX_TITLE_BYTES).is_some());
    }

    #[test]
    fn visible_ink_fills_the_tile_with_equal_opposite_margins() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<StandardMaterial>>();
        world.insert_resource(Playback(Simulation::two_synthetic_repositories()));
        setup(&mut world);

        let ink = world
            .query::<(&Title, &Mesh3d)>()
            .iter(&world)
            .next()
            .map(|(title, handle)| (title.ink, handle.0.clone()))
            .unwrap();
        let mesh = world.resource::<Assets<Mesh>>().get(&ink.1).unwrap();
        let positions = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .unwrap()
            .as_float3()
            .unwrap();
        let tile = Tile::viewport();
        let [a, _, b, _] = tile.ground();
        let transform = placement(tile, ink.0);
        let (low, high) = positions
            .iter()
            .map(|p| transform.transform_point(Vec3::from_array(*p)))
            .fold(
                (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
                |(low, high), p| (low.min(p), high.max(p)),
            );
        let before = low.xz() - a.min(b);
        let after = a.max(b) - high.xz();
        assert!(before.abs_diff_eq(after, 1e-5));
        assert!(
            (before - ink.0.y * transform.scale.x * GLYPH_PADDING)
                .cmpge(Vec2::splat(-1e-5))
                .all()
        );
        assert_eq!(low.y, PLANE);
        assert_eq!(high.y, PLANE);
    }

    #[test]
    fn changing_camera_yaw_and_pitch_keeps_every_title_fixed_to_the_tile() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<StandardMaterial>>();
        world.insert_resource(Playback(Simulation::two_synthetic_repositories()));
        setup(&mut world);
        let camera = world
            .spawn((
                SceneCamera,
                Camera::default(),
                Projection::Perspective(bevy::camera::PerspectiveProjection::default()),
                Transform::from_xyz(0.0, 60.0, 0.0).looking_at(Vec3::ZERO, Vec3::NEG_Z),
            ))
            .id();
        world.resource_scope(|world, playback: Mut<Playback>| sync(world, &playback.0));
        let mut query = world.query::<(&Title, &Transform, &Visibility)>();
        let before: Vec<_> = query
            .iter(&world)
            .map(|(_, transform, _)| *transform)
            .collect();
        assert!(
            query
                .iter(&world)
                .any(|(_, _, visibility)| *visibility == Visibility::Visible)
        );

        *world.get_mut::<Transform>(camera).unwrap() =
            Transform::from_xyz(17.0, 3.0, 9.0).looking_at(Vec3::ZERO, Vec3::Y);
        world.resource_scope(|world, playback: Mut<Playback>| sync(world, &playback.0));
        assert!(
            query
                .iter(&world)
                .map(|(_, transform, _)| *transform)
                .eq(before)
        );
    }

    #[test]
    fn title_visibility_requires_displayed_ink_and_clearance() {
        let mut world = World::new();
        world.init_resource::<Assets<StandardMaterial>>();
        let simulation = Simulation::two_synthetic_repositories();
        let tile = simulation.visible_repository_tile(0).unwrap();
        let [a, _, b, _] = tile.ground();
        let size = (b - a).abs();
        let ink = Vec2::new(size.x / size.y, 1.0);
        let material = world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let entity = world
            .spawn((
                Title { repository: 0, ink },
                MeshMaterial3d(material),
                Transform::default(),
                Visibility::Hidden,
            ))
            .id();
        let camera = world
            .spawn((
                SceneCamera,
                Camera::default(),
                Projection::Perspective(bevy::camera::PerspectiveProjection::default()),
                Transform::from_translation(placement(tile, ink).translation + Vec3::Y)
                    .looking_to(Vec3::NEG_Y, Vec3::NEG_Z),
            ))
            .id();

        let drawn_height = ink.y * placement(tile, ink).scale.z;
        let distance =
            drawn_height * 360.0 / (4.1 * (std::f32::consts::FRAC_PI_8).tan());
        let camera_height = PLANE + distance;
        world.get_mut::<Transform>(camera).unwrap().translation.y = camera_height;
        sync(&mut world, &simulation);
        assert_eq!(
            *world.get::<Visibility>(entity).unwrap(),
            Visibility::Hidden
        );
        let placement = *world.get::<Transform>(entity).unwrap();

        world.get_mut::<Transform>(camera).unwrap().translation.y =
            PLANE + distance * 0.25;
        sync(&mut world, &simulation);
        assert_eq!(
            *world.get::<Visibility>(entity).unwrap(),
            Visibility::Visible
        );
        assert_eq!(*world.get::<Transform>(entity).unwrap(), placement);
    }

    #[test]
    fn a_birth_hides_title_until_the_displayed_ink_fits() {
        let mut simulation = Simulation::two_synthetic_repositories();
        let birth = (0..simulation.frames(60).unwrap())
            .find_map(|frame| {
                simulation.advance_to(frame, 60);
                simulation
                    .visible_repository_tile(1)
                    .filter(|tile| (0.5..=4.0).contains(&tile.ground_aspect()))
            })
            .expect("the second repository grows into view");
        let ink = Vec2::new(8.0, 1.0);
        let born = placement(birth, ink);
        let [low, _, high, _] = birth.ground();
        let pane_height = (high.y - low.y).abs();
        let distance = pane_height * 360.0 / (3.0 * std::f32::consts::FRAC_PI_8.tan());
        let camera = Transform::from_translation(born.translation + Vec3::Y * distance)
            .looking_to(Vec3::NEG_Y, Vec3::NEG_Z);
        let projection =
            Projection::Perspective(bevy::camera::PerspectiveProjection::default());
        let view = super::super::names::View {
            clip_from_world: projection.get_clip_from_view()
                * camera.to_matrix().inverse(),
            viewport: crate::model::framing::VIEWPORT,
        };
        assert!(!legible(&view, birth, born, ink));
        let zoom =
            Transform::from_translation(born.translation + Vec3::Y * distance * 0.025)
                .looking_to(Vec3::NEG_Y, Vec3::NEG_Z);
        let zoomed = super::super::names::View {
            clip_from_world: projection.get_clip_from_view()
                * zoom.to_matrix().inverse(),
            ..view
        };
        assert!(legible(&zoomed, birth, born, ink));
    }

    #[test]
    fn title_ink_stays_legible_across_camera_angles() {
        let simulation = Simulation::two_synthetic_repositories();
        let tile = simulation.visible_repository_tile(0).unwrap();
        let ink = Vec2::new(tile.ground_aspect(), 1.0);
        let placed = placement(tile, ink);
        let projection =
            Projection::Perspective(bevy::camera::PerspectiveProjection::default());
        for (angle, camera) in super::super::pane_visibility_views(tile) {
            let view = super::super::names::View {
                clip_from_world: projection.get_clip_from_view()
                    * camera.to_matrix().inverse(),
                viewport: crate::model::framing::VIEWPORT,
            };
            let behind_camera = tile.ground().into_iter().any(|point| {
                let point = super::super::ground(point).with_y(PLANE);
                (view.clip_from_world * point.extend(1.0)).w < 0.0
            });
            assert_eq!(behind_camera, matches!(angle, "oblique" | "low"), "{angle}");
            assert!(legible(&view, tile, placed, ink), "{angle}");
        }
    }

    #[test]
    fn zoom_reveals_small_ink_without_resizing_the_title() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<StandardMaterial>>();
        world.insert_resource(Playback(Simulation::two_synthetic_repositories()));
        setup(&mut world);
        let camera = world
            .spawn((
                SceneCamera,
                Camera::default(),
                Projection::Perspective(bevy::camera::PerspectiveProjection::default()),
                Transform::from_xyz(0.0, 20_000.0, 0.0)
                    .looking_at(Vec3::ZERO, Vec3::NEG_Z),
            ))
            .id();
        let mut query = world.query::<(&Transform, &Visibility, &Title)>();
        world.resource_scope(|world, playback: Mut<Playback>| sync(world, &playback.0));
        let (transform, visibility, title) = query
            .iter(&world)
            .find(|(_, _, title)| {
                world.resource::<Playback>().0.layout().repositories[title.repository]
                    .live
            })
            .unwrap();
        let before = *transform;
        assert_eq!(*visibility, Visibility::Hidden);
        let ink_height = before.scale.z * title.ink.y;
        let distance = ink_height * crate::model::framing::VIEWPORT.y
            / (16.0 * std::f32::consts::FRAC_PI_8.tan());
        world.get_mut::<Transform>(camera).unwrap().translation =
            before.translation + Vec3::Y * distance;
        world.resource_scope(|world, playback: Mut<Playback>| sync(world, &playback.0));
        let (transform, visibility, _) = query
            .iter(&world)
            .find(|(_, _, title)| {
                world.resource::<Playback>().0.layout().repositories[title.repository]
                    .live
            })
            .unwrap();
        assert_eq!(*visibility, Visibility::Visible);
        assert_eq!(*transform, before);
    }
}
