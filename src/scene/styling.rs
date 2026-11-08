use crate::{model::Simulation, theme::Theme};
use bevy::{
    asset::RenderAssetUsages,
    image::ImageSampler,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};

use super::{
    Label, Palette, SceneCamera, grow, hide_rest, place_beam, set_material, set_visible,
};
use crate::theme::{AvatarStyle, BeamStyle, FontStyle};

#[derive(Default)]
struct Territories {
    revision: Option<u64>,
    owners: Vec<usize>,
    radii: Vec<f32>,
}

impl Territories {
    fn update(&mut self, layout: &crate::layout::Layout) {
        if self.revision == Some(layout.revision()) {
            return;
        }
        let first = self.owners.len().max(1);
        self.owners.resize(layout.dir_count(), 0);
        self.radii.clear();
        self.radii.resize(layout.dir_count(), 1.0);
        for index in first..layout.dir_count() {
            let parent = layout.dir(index).parent.expect("non-root directory");
            self.owners[index] = if parent == 0 {
                index
            } else {
                self.owners[parent]
            };
        }
        for (_, file) in layout.files() {
            let owner = self.owners[file.dir];
            if owner != 0 {
                self.radii[owner] = self.radii[owner]
                    .max(file.position.distance(layout.dir(owner).position));
            }
        }
        self.revision = Some(layout.revision());
    }
}

pub(super) fn edge_segments(theme: &Theme, from: Vec3, to: Vec3) -> Vec<(Vec3, Vec3)> {
    let count = if theme.edge_dash.is_some() { 12_u16 } else { 1 };
    (0..count)
        .map(|i| {
            let start = f32::from(i) / f32::from(count);
            let end =
                (f32::from(i) + theme.edge_dash.unwrap_or(1.0)) / f32::from(count);
            (from.lerp(to, start), from.lerp(to, end))
        })
        .collect()
}
#[derive(Component)]
pub(super) struct Ground;

#[derive(Resource)]
pub(super) struct Styles {
    backdrops: Vec<(crate::theme::ThemeName, Handle<Image>)>,
    backdrop: Entity,
    backdrop_material: Handle<StandardMaterial>,
    territory_mesh: Handle<Mesh>,
    grounds: Vec<(crate::theme::ThemeName, Handle<Image>)>,
    fonts: Vec<(FontStyle, Handle<Font>)>,
    pub(super) beams: Vec<Handle<StandardMaterial>>,
    pub(super) territories: Vec<Handle<StandardMaterial>>,
    territory_pool: Vec<Entity>,
    territory_layout: Territories,
    leaders: Vec<Entity>,
}

fn svg_image(svg: &str) -> Image {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())
        .expect("theme ground SVG is valid");
    let mut pixels = resvg::tiny_skia::Pixmap::new(1024, 1024).expect("ground size");
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixels.as_mut(),
    );
    let mut image = Image::new_uninit(
        Extent3d {
            width: 1024,
            height: 1024,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(pixels.take());
    image.sampler = ImageSampler::linear();
    image
}

pub(super) fn setup(world: &mut World, theme: &Theme) {
    let territory_mesh = world.resource_mut::<Assets<Mesh>>().add(Annulus::new(
        theme.avatar_radius * 0.99,
        theme.avatar_radius,
    ));
    let backdrops = Vec::new();
    let backdrop_material =
        world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                unlit: true,
                alpha_mode: AlphaMode::Blend,
                ..default()
            });
    let mesh = world.resource::<Palette>().glyph_mesh.clone();
    let backdrop = world
        .spawn((
            Mesh3d(mesh),
            MeshMaterial3d(backdrop_material.clone()),
            Transform::default(),
            Visibility::Hidden,
        ))
        .id();
    let grounds = Vec::new();
    let fonts = [
        (
            FontStyle::Tech,
            include_bytes!("../../assets/fonts/ShareTechMono-Regular.ttf").as_slice(),
        ),
        (
            FontStyle::Plex,
            include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf").as_slice(),
        ),
        (
            FontStyle::Serif,
            include_bytes!("../../assets/fonts/Fraunces[SOFT,WONK,opsz,wght].ttf")
                .as_slice(),
        ),
    ]
    .into_iter()
    .map(|(kind, bytes)| {
        (
            kind,
            world
                .resource_mut::<Assets<Font>>()
                .add(Font::from_bytes(bytes.to_vec())),
        )
    })
    .collect();
    let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
    let beams = (0..16).map(|_| materials.add(theme.edge.clone())).collect();
    let territories = (0..36).map(|_| materials.add(theme.edge.clone())).collect();
    world.insert_resource(Styles {
        territory_mesh,
        backdrops,
        backdrop,
        backdrop_material,
        grounds,
        fonts,
        beams,
        territories,
        territory_pool: Vec::new(),
        territory_layout: Territories::default(),
        leaders: Vec::new(),
    });
    apply(world, theme);
}

pub(super) fn avatar_mesh(theme: &Theme) -> Mesh {
    let r = theme.avatar_radius;
    match theme.avatar_style {
        AvatarStyle::Sphere => Sphere::new(r).into(),
        AvatarStyle::Reticle => {
            let mut mesh = Mesh::from(Annulus::new(r * 0.7, r * 0.8));
            for (x, y, sx, sy) in [
                (0.0, r, r * 0.1, r * 0.7),
                (0.0, -r, r * 0.1, r * 0.7),
                (r, 0.0, r * 0.7, r * 0.1),
                (-r, 0.0, r * 0.7, r * 0.1),
            ] {
                mesh.merge(
                    &Mesh::from(Rectangle::new(sx, sy))
                        .translated_by(Vec3::new(x, y, 0.0)),
                )
                .expect("reticle attributes");
            }
            mesh
        }
        AvatarStyle::Pointer => Triangle2d::new(
            Vec2::new(-r, r),
            Vec2::new(-r * 0.5, -r),
            Vec2::new(r, -r * 0.25),
        )
        .into(),
        AvatarStyle::Pin => {
            let mut mesh =
                Mesh::from(Circle::new(r * 0.65)).translated_by(Vec3::Y * r * 0.35);
            mesh.merge(&Mesh::from(Triangle2d::new(
                Vec2::new(-r * 0.5, 0.0),
                Vec2::new(0.0, -r),
                Vec2::new(r * 0.5, 0.0),
            )))
            .expect("pin attributes");
            mesh
        }
    }
}

pub(super) fn ring_mesh(theme: &Theme, radii: Vec2) -> Mesh {
    if !matches!(
        theme.glyph_style,
        crate::theme::GlyphStyle::Outline
            | crate::theme::GlyphStyle::Hatched
            | crate::theme::GlyphStyle::Page
            | crate::theme::GlyphStyle::Iron
    ) {
        return Annulus::new(radii.x, radii.y).into();
    }
    let width = radii.y - radii.x;
    let centre = radii.x + width * 0.5;
    let mut mesh = Mesh::from(Rectangle::new(radii.y * 2.0, width))
        .translated_by(Vec3::Y * centre);
    for (x, y, sx, sy) in [
        (0.0, -centre, radii.y * 2.0, width),
        (-centre, 0.0, width, radii.x * 2.0),
        (centre, 0.0, width, radii.x * 2.0),
    ] {
        mesh.merge(
            &Mesh::from(Rectangle::new(sx, sy)).translated_by(Vec3::new(x, y, 0.0)),
        )
        .expect("outline attributes");
    }
    mesh
}

pub(super) fn raster_images(theme: &Theme) -> (Option<Image>, Option<Image>) {
    (
        theme
            .textured_ground
            .then(|| svg_image(&theme.ground_svg())),
        theme.backdrop_svg().map(svg_image),
    )
}

pub(super) fn install_images(
    world: &mut World,
    name: crate::theme::ThemeName,
    ground: Option<Image>,
    backdrop: Option<Image>,
) {
    let ground = ground.map(|image| world.resource_mut::<Assets<Image>>().add(image));
    let backdrop =
        backdrop.map(|image| world.resource_mut::<Assets<Image>>().add(image));
    let mut styles = world.resource_mut::<Styles>();
    if let Some(image) = ground {
        styles.grounds.push((name, image));
    }
    if let Some(image) = backdrop {
        styles.backdrops.push((name, image));
    }
}

pub(super) fn apply(world: &mut World, theme: &Theme) {
    let styles = world.resource::<Styles>();
    if (theme.textured_ground
        && !styles.grounds.iter().any(|(name, _)| *name == theme.name))
        || (theme.backdrop_svg().is_some()
            && !styles.backdrops.iter().any(|(name, _)| *name == theme.name))
    {
        let (ground, backdrop) = raster_images(theme);
        install_images(world, theme.name, ground, backdrop);
    }
    let cameras: Vec<_> = world
        .query_filtered::<Entity, With<SceneCamera>>()
        .iter(world)
        .collect();
    for entity in cameras {
        if let Some(bloom) = theme.bloom() {
            world.entity_mut(entity).insert(bloom);
        } else {
            world
                .entity_mut(entity)
                .remove::<bevy::post_process::bloom::Bloom>();
        }
    }
    world.resource_scope(|world, styles: Mut<Styles>| {
        let font = styles.fonts.iter().find(|(kind, _)| *kind == theme.font).map(|(_, handle)| handle.clone()).unwrap_or_default();
        for mut text in world.query::<&mut TextFont>().iter_mut(world) {
            text.font = font.clone().into();
        }
        for (mut node, mut background, mut border) in world.query_filtered::<(&mut Node, &mut BackgroundColor, &mut BorderColor), With<Label>>().iter_mut(world) {
            node.border = UiRect::all(px(if theme.label_style == crate::theme::LabelStyle::Bevelled { 2.0 } else { 0.0 }));
            background.0 = theme.label_background();
            *border = theme.label_border();
        }
        let floor = world.resource::<Palette>().floor.clone();
        let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
        materials.get_mut(&styles.backdrop_material).expect("backdrop").base_color_texture = styles.backdrops.iter().find(|(name, _)| *name == theme.name).map(|(_, texture)| texture.clone());
        materials.get_mut(&floor).expect("floor").base_color_texture = styles.grounds.iter().find(|(name, _)| *name == theme.name).map(|(_, image)| image.clone());
        for (index, handle) in styles.beams.iter().enumerate() {
            let mut material = materials.get_mut(handle).expect("beam style");
            *material = theme.beam_material(u16::try_from(index).expect("16 segments"));
        }
        for (index, handle) in styles.territories.iter().enumerate() {
            let mut material = materials.get_mut(handle).expect("territory");
            *material = theme.territory_material(index / 3, index % 3 != 0);
        }
    });
}

pub(super) fn beam_segments(
    theme: &Theme,
    from: Vec3,
    to: Vec3,
) -> Vec<(Vec3, Vec3, usize)> {
    let count = match theme.beam_style {
        BeamStyle::Arch | BeamStyle::Ants => 16_u16,
        _ => 1,
    };
    let point = |t: f32| {
        from.lerp(to, t)
            + Vec3::Y
                * match theme.beam_style {
                    BeamStyle::Arch => 4.0 * t * (1.0 - t) * from.distance(to) * 0.25,
                    _ => 0.0,
                }
    };
    let mut segments: Vec<_> = (0..count)
        .map(|i| {
            (
                point(f32::from(i) / f32::from(count)),
                point(f32::from(i + 1) / f32::from(count)),
                usize::from(i),
            )
        })
        .collect();
    if matches!(theme.beam_style, BeamStyle::Construction) {
        let tick = (to - from).cross(Vec3::Y).normalize_or(Vec3::X) * 0.13;
        segments.extend([(from - tick, from + tick, 0), (to - tick, to + tick, 0)]);
    }
    segments
}

pub(super) fn ground(
    world: &mut World,
    palette: &Palette,
    hues: &[usize],
    simulation: &Simulation,
) {
    place_ground(world, simulation);
    let theme = simulation.theme();
    world.resource_scope(|world, mut styles: Mut<Styles>| {
        territories(world, palette, hues, simulation, &mut styles, theme);
    });
}

pub(super) fn place_ground(world: &mut World, simulation: &Simulation) {
    let theme = simulation.theme();
    let (focus, distance) = simulation.view();
    let (eye, focus) = world.get_resource::<crate::camera::PreviewCamera>().map_or(
        (focus + crate::model::view_direction() * distance, focus),
        crate::camera::PreviewCamera::pose,
    );
    let distance = eye.distance(focus);
    let direction = (eye - focus).normalize();
    let (ground_focus, coverage) =
        if world.contains_resource::<crate::camera::PreviewCamera>() {
            let far = world
                .query_filtered::<&Projection, With<SceneCamera>>()
                .iter(world)
                .find_map(|projection| match projection {
                    Projection::Perspective(p) => Some(p.far),
                    _ => None,
                })
                .unwrap_or(1000.0);
            (eye, far * 4.0)
        } else {
            (focus, distance * 2.5)
        };
    for mut transform in world
        .query_filtered::<&mut Transform, With<Ground>>()
        .iter_mut(world)
    {
        *transform = Transform::from_xyz(ground_focus.x, -0.18, ground_focus.z)
            .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
            .with_scale(Vec3::splat(coverage));
    }
    world.resource_scope(|world, styles: Mut<Styles>| {
        let backdrop_visible = theme.backdrop_svg().is_some();
        let horizontal = Vec3::new(direction.x, 0.0, direction.z).normalize();
        let position = focus - horizontal * distance * 0.8 + Vec3::Y * distance * 0.12;
        *world
            .get_mut::<Transform>(styles.backdrop)
            .expect("backdrop transform") = Transform::from_translation(position)
            .looking_to(position - eye, Vec3::Y)
            .with_scale(Vec3::splat(distance * 0.6));
        set_visible(world, styles.backdrop, backdrop_visible);
    });
}

fn territories(
    world: &mut World,
    palette: &Palette,
    hues: &[usize],
    simulation: &Simulation,
    styles: &mut Styles,
    theme: &Theme,
) {
    let mut used = 0;
    if theme.territories {
        styles.territory_layout.update(simulation.layout());
        let mesh = palette.disc.clone();
        let material = styles.territories[0].clone();
        let dirs: Vec<_> = simulation
            .layout()
            .dirs()
            .filter(|(_, dir)| dir.parent == Some(0))
            .collect();
        grow(
            world,
            &mut styles.territory_pool,
            dirs.len() * 3,
            &mesh,
            &material,
        );
        for (index, dir) in dirs {
            let slot = hues[index] % 12;
            let radius = styles.territory_layout.radii[index] + 0.5;
            for ring in 0..3 {
                let entity = styles.territory_pool[used];
                used += 1;
                set_material(world, entity, &styles.territories[slot * 3 + ring]);
                let meshes = [&mesh, &styles.territory_mesh, &styles.territory_mesh];
                world
                    .entity_mut(entity)
                    .insert(Mesh3d(meshes[ring].clone()));
                *world
                    .get_mut::<Transform>(entity)
                    .expect("territory transform") = Transform::from_translation(
                    super::ground(dir.position)
                        - Vec3::Y
                            * (0.14
                                + f32::from(u16::try_from(ring).expect("3 rings"))
                                    * 0.005),
                )
                .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
                .with_scale(Vec3::splat(
                    radius / theme.avatar_radius
                        * (1.0
                            + f32::from(u16::try_from(ring).expect("3 rings")) * 0.07),
                ));
                set_visible(world, entity, true);
            }
        }
    }
    hide_rest(world, &styles.territory_pool, used);
}

pub(super) fn leaders(
    world: &mut World,
    points: &[Vec2],
    theme: &Theme,
    camera: Entity,
) {
    world.resource_scope(|world, mut styles: Mut<Styles>| {
        let count = if theme.label_style == crate::theme::LabelStyle::Leader {
            points.len()
        } else {
            0
        };
        while styles.leaders.len() < count {
            styles.leaders.push(
                world
                    .spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            width: px(18.0),
                            height: px(1.0),
                            ..default()
                        },
                        BackgroundColor(theme.label),
                        UiTargetCamera(camera),
                        Visibility::Hidden,
                    ))
                    .id(),
            );
        }
        for (&entity, point) in styles.leaders.iter().zip(points).take(count) {
            let mut node = world.get_mut::<Node>(entity).expect("leader node");
            node.left = px(point.x - 18.0);
            node.top = px(point.y + 10.0);
            set_visible(world, entity, true);
        }
        hide_rest(world, &styles.leaders, count);
    });
}

pub(super) fn beams(
    world: &mut World,
    palette: &Palette,
    pools: &mut super::Pools,
    simulation: &Simulation,
) {
    let beams = simulation.beams();
    let segments: Vec<_> = beams
        .iter()
        .flat_map(|beam| {
            beam_segments(simulation.theme(), beam.from, beam.to)
                .into_iter()
                .map(move |(from, to, segment)| (beam, from, to, segment))
        })
        .collect();
    let mut pool = std::mem::take(&mut pools.beams);
    while pool.len() < segments.len() {
        pool.push(
            world
                .spawn((
                    Mesh3d(palette.beam.clone()),
                    MeshMaterial3d(palette.glass[0].clone()),
                    Transform::default(),
                    Visibility::Hidden,
                ))
                .id(),
        );
    }
    for (entity, (beam, from, to, segment)) in pool.iter().zip(&segments) {
        let material = if matches!(
            simulation.theme().beam_style,
            crate::theme::BeamStyle::Straight
        ) {
            palette.glass[beam.author % palette.glass.len()].clone()
        } else {
            world.resource::<Styles>().beams[*segment].clone()
        };
        world.entity_mut(*entity).insert(MeshMaterial3d(material));
        place_beam(
            world,
            *entity,
            *from,
            *to,
            beam.width * simulation.scale().sqrt() * simulation.theme().beam_width(),
        );
    }
    hide_rest(world, &pool, segments.len());
    pools.beams = pool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeName;

    #[test]
    fn preview_ground_covers_far_plane_and_backdrop_faces_orbited_camera() {
        let simulation = Simulation::new(
            crate::timeline::synthetic(),
            &crate::model::Options::default(),
            &ThemeName::Outrun.build(),
            &crate::contributors::Mappings::default(),
        );
        let mut world = World::new();
        world.insert_resource(super::super::SceneTarget(
            Handle::<Image>::default().into(),
        ));
        world.insert_resource(super::super::Title("test".to_owned()));
        world.insert_resource(super::super::Playback(simulation));
        world.init_resource::<super::super::Pools>();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        super::super::setup(&mut world);
        let view = world.resource::<super::super::Playback>().0.view();
        let mut camera = crate::camera::PreviewCamera::new(view);
        camera.update(
            &crate::camera::Input {
                pan: Vec2::new(200.0, -100.0),
                orbit: Vec2::new(1.0, -0.3),
                zoom: -4.0,
                ..default()
            },
            view,
            &[],
            1.0,
        );
        let (eye, _) = camera.pose();
        world.insert_resource(camera);
        world.resource_scope(|world, playback: Mut<super::super::Playback>| {
            place_ground(world, &playback.0);
        });
        let ground = world
            .query_filtered::<&Transform, With<Ground>>()
            .single(&world)
            .unwrap();
        assert_eq!(ground.translation.x, eye.x);
        assert_eq!(ground.translation.z, eye.z);
        assert!(ground.scale.x >= 4000.0);
        let backdrop = world
            .get::<Transform>(world.resource::<Styles>().backdrop)
            .unwrap();
        assert!(
            backdrop
                .local_z()
                .dot((eye - backdrop.translation).normalize())
                > 0.999
        );
        assert_ne!(eye, view.0 + crate::model::view_direction() * view.1);
    }

    fn reference_owner(layout: &crate::layout::Layout, mut owner: usize) -> usize {
        while let Some(parent) = layout.dir(owner).parent.filter(|&parent| parent != 0)
        {
            owner = parent;
        }
        owner
    }

    #[test]
    fn territory_cache_tracks_nested_moves_and_reuses_unchanged_layouts() {
        let mut layout = crate::layout::Layout::planned_paths(
            &["a/deep/one.rs", "b/two.rs", "root.rs"],
            0,
        );
        for path in ["a/deep/one.rs", "b/two.rs", "root.rs"] {
            layout.add(path, 0.0, 0);
        }
        layout.settle(10, 1.0 / 60.0);
        let mut cache = Territories::default();
        for moved in [false, true] {
            if moved {
                layout.rename("a/deep/one.rs", "b/one.rs", 0);
                layout.settle(10, 1.0 / 60.0);
            }
            cache.update(&layout);
            for (index, dir) in layout.dirs().filter(|(_, d)| d.parent == Some(0)) {
                let expected = layout
                    .files()
                    .filter(|(_, f)| {
                        let owner = reference_owner(&layout, f.dir);
                        owner == index
                    })
                    .map(|(_, f)| f.position.distance(dir.position))
                    .fold(1.0_f32, f32::max);
                assert_eq!(cache.radii[index], expected);
            }
            cache.radii[0] = 123.0;
            cache.update(&layout);
            assert_eq!(cache.radii[0], 123.0);
        }
    }

    #[test]
    fn styled_beams_keep_endpoints_and_static_dash_positions() {
        let from = Vec3::new(-2.0, 3.0, 1.0);
        let to = Vec3::new(4.0, 0.0, -1.0);
        for name in ThemeName::ALL {
            let theme = name.build();
            let segments = beam_segments(&theme, from, to);
            assert_eq!(segments[0].0, from);
            let endpoint = if matches!(theme.beam_style, BeamStyle::Construction) {
                0
            } else {
                segments.len() - 1
            };
            assert_eq!(segments[endpoint].1, to);
            assert_eq!(segments, beam_segments(&theme, from, to));
            assert!(
                segments
                    .iter()
                    .all(|(a, b, _)| a.is_finite() && b.is_finite())
            );
            assert!(
                beam_segments(&theme, from, from)
                    .iter()
                    .all(|(a, b, _)| a.is_finite() && b.is_finite())
            );
        }
        let arch = beam_segments(&ThemeName::Outrun.build(), from, to);
        assert!(arch[7].1.y > from.lerp(to, 0.5).y);
    }

    #[test]
    fn grounds_render_opaque_and_have_spatial_detail() {
        for name in ThemeName::ALL.into_iter().skip(2) {
            let image = svg_image(&name.build().ground_svg());
            let pixels = image.data.unwrap();
            assert!(
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel[3] == 255)
            );
            assert!(
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel != &pixels[..4])
            );
        }
    }
}
