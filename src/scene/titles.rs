use bevy::{
    asset::RenderAssetUsages, mesh::Indices, prelude::*,
    render::render_resource::PrimitiveTopology, text::FontCx,
};
use lyon_tessellation::{
    FillGeometryBuilder, FillOptions, FillRule, FillTessellator, FillVertex,
    GeometryBuilder, GeometryBuilderError, VertexId, math::point, path::Path,
};
use parley::{FontFamily, Layout, LayoutContext, PositionedLayoutItem, StyleProperty};
use swash::{
    FontRef,
    scale::{ScaleContext, outline::Outline},
    zeno::{Command, PathData},
};

use crate::{layout::packing::Tile, model::Simulation};

const MESH_BYTES: usize = 64 * 1024 * 1024;
const MAX_TITLE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TITLE_CHARS: usize = 1024;
const MAX_GLYPH_POINTS: usize = 4096;
// Scaling outlines at one pixel per em loses precision before tessellation.
const SHAPING_SIZE: f32 = 1024.0;
const OPACITY: f32 = 0.18;
const PADDING: f32 = 0.04;
const PLANE: f32 = crate::model::framing::GLASS_BOTTOM;

#[derive(Component)]
struct Title {
    repository: usize,
    ink: Vec2,
}

fn placement(tile: Tile, ink: Vec2) -> Transform {
    let [a, _, b, _] = tile.ground();
    let size = (b - a).abs();
    let padding = size.min_element() * PADDING;
    let scale = ((size - Vec2::splat(2.0 * padding)) / ink).min_element();
    Transform::from_translation(super::ground(a.midpoint(b)).with_y(PLANE))
        .with_scale(Vec3::new(scale, 1.0, scale))
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
    world.init_resource::<FontCx>();
    world.resource_scope(|world, mut fonts: Mut<FontCx>| {
        fonts.collection.register_fonts(
            include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf")
                .to_vec()
                .into(),
            None,
        );
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
            let material = world.resource_mut::<Assets<StandardMaterial>>().add(
                StandardMaterial {
                    base_color: color.with_alpha(OPACITY),
                    alpha_mode: AlphaMode::Blend,
                    unlit: true,
                    cull_mode: None,
                    ..default()
                },
            );
            world.spawn((
                Title { repository, ink },
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::default(),
                Visibility::Hidden,
            ));
        }
        if omitted > 0 {
            warn!("{omitted} repository titles had no usable outline or exceeded the mesh budget");
        }
    });
}

pub(super) fn sync(world: &mut World, simulation: &Simulation) {
    let mut query = world.query::<(
        &Title,
        &MeshMaterial3d<StandardMaterial>,
        &mut Transform,
        &mut Visibility,
    )>();
    world.resource_scope(|world, mut materials: Mut<Assets<StandardMaterial>>| {
        for (title, material, mut transform, mut visibility) in query.iter_mut(world) {
            let region = &simulation.layout().repositories[title.repository];
            let Some(tile) = region.tile.filter(|_| region.live) else {
                visibility.set_if_neq(Visibility::Hidden);
                continue;
            };
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
            transform.set_if_neq(placement(tile, title.ink));
            visibility.set_if_neq(Visibility::Visible);
        }
    });
}

fn title_mesh(
    text: &str,
    fonts: &mut FontCx,
    shaper: &mut LayoutContext<()>,
    scale: &mut ScaleContext,
    tessellator: &mut FillTessellator,
    limit: usize,
) -> Option<(Mesh, Vec2, usize)> {
    if limit == 0 || text.chars().nth(MAX_TITLE_CHARS).is_some() {
        return None;
    }
    let text = text.replace(['\r', '\n', '\t'], " ");
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
    for item in layout.lines().flat_map(|line| line.items()) {
        let PositionedLayoutItem::GlyphRun(glyphs) = item else {
            continue;
        };
        let run = glyphs.run();
        let font = run.font();
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
            geometry.offset = Vec2::new(glyph.x, -glyph.y) / SHAPING_SIZE;
            tessellator
                .tessellate_path(
                    &path,
                    &FillOptions::tolerance(0.00005).with_fill_rule(FillRule::NonZero),
                    &mut geometry,
                )
                .ok()?;
            if geometry.exhausted {
                return None;
            }
        }
    }

    let first = *geometry.vertices.first()?;
    let (low, high) = geometry
        .vertices
        .iter()
        .fold((first, first), |(lo, hi), &p| (lo.min(p), hi.max(p)));
    let ink = high - low;
    if !ink.is_finite() || ink.min_element() <= 0.0 {
        return None;
    }
    let bytes = limit - geometry.remaining;
    let center = low.midpoint(high);
    let positions: Vec<_> = geometry
        .vertices
        .into_iter()
        .map(|p| {
            let p = p - center;
            [p.x, 0.0, -p.y]
        })
        .collect();
    let normals = vec![Vec3::Y.to_array(); positions.len()];
    let mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_indices(Indices::U32(geometry.indices));
    Some((mesh, ink, bytes))
}

#[derive(Default)]
struct Geometry {
    vertices: Vec<Vec2>,
    indices: Vec<u32>,
    offset: Vec2,
    remaining: usize,
    exhausted: bool,
}

impl GeometryBuilder for Geometry {
    fn add_triangle(&mut self, a: VertexId, b: VertexId, c: VertexId) {
        if let Some(remaining) = self.remaining.checked_sub(size_of::<[u32; 3]>()) {
            self.indices.extend([a.0, b.0, c.0]);
            self.remaining = remaining;
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
        let remaining = self
            .remaining
            .checked_sub(size_of::<[f32; 6]>())
            .filter(|_| !self.exhausted)
            .ok_or(GeometryBuilderError::TooManyVertices)?;
        self.remaining = remaining;
        let index =
            u32::try_from(self.vertices.len()).expect("mesh budget fits u32 indices");
        self.vertices
            .push(Vec2::from_array(vertex.position().to_array()) + self.offset);
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
            (before.min_element() - PADDING * (b - a).abs().min_element()).abs() < 1e-5
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
                Transform::from_xyz(0.0, 60.0, 0.0).looking_at(Vec3::ZERO, Vec3::NEG_Z),
            ))
            .id();
        world.resource_scope(|world, playback: Mut<Playback>| sync(world, &playback.0));
        let mut query = world.query::<(&Title, &Transform, &Visibility)>();
        let before: Vec<_> = query
            .iter(&world)
            .map(|(_, transform, visible)| (*transform, *visible))
            .collect();
        assert!(
            before
                .iter()
                .any(|(_, visibility)| *visibility == Visibility::Visible)
        );

        *world.get_mut::<Transform>(camera).unwrap() =
            Transform::from_xyz(17.0, 3.0, 9.0).looking_at(Vec3::ZERO, Vec3::Y);
        world.resource_scope(|world, playback: Mut<Playback>| sync(world, &playback.0));
        assert!(
            query
                .iter(&world)
                .map(|(_, transform, visible)| (*transform, *visible))
                .eq(before)
        );
    }
}
