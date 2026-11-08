use std::collections::HashMap;

use bevy::{
    asset::RenderAssetUsages,
    camera::{Camera3dDepthLoadOp, Hdr, RenderTarget, visibility::RenderLayers},
    image::ImageSampler,
    input::mouse::{AccumulatedMouseScroll, MouseScrollUnit},
    prelude::*,
    render::{
        RenderPlugin,
        pipelined_rendering::PipelinedRenderingPlugin,
        render_resource::{Extent3d, TextureDimension, TextureFormat},
        view::{DebandDither, Tonemapping},
    },
    ui::UiSystems,
    window::ExitCondition,
    winit::WinitPlugin,
};

use crate::{
    camera::{Input, PreviewCamera},
    layout::Layout,
    model::{self, Simulation},
    theme::Theme,
};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const LABELS: usize = 72;
const GRID_LINES: i16 = 20;
const AVATAR_LAYER: usize = 1;

mod lod;
mod styling;

#[derive(Resource)]
pub(super) struct Frame {
    pub index: u32,
    pub fps: u16,
}

#[derive(Resource)]
pub(super) struct Playback(pub Simulation);

#[derive(Resource)]
struct SceneTarget(RenderTarget);

#[derive(Component)]
struct SceneCamera;

#[derive(Component)]
struct AvatarCamera;

#[derive(Component)]
struct Label;

#[derive(Component)]
struct DiffSample;

#[derive(Component)]
struct Caption;

#[derive(Component)]
struct PortraitDisc;

#[derive(Component)]
struct FileVisual {
    heat_material: Handle<StandardMaterial>,
    membership: Entity,
    glyph: Entity,
    glyph_kind: usize,
    halo: Entity,
    author: Option<usize>,
}

#[derive(Resource)]
struct Palette {
    folder: Handle<Mesh>,
    cube: Handle<Mesh>,
    sphere: Handle<Mesh>,
    disc: Handle<Mesh>,
    beam: Handle<Mesh>,
    edge: Handle<StandardMaterial>,
    floor: Handle<StandardMaterial>,
    grid: Handle<StandardMaterial>,
    descending: Handle<StandardMaterial>,
    rising: Handle<StandardMaterial>,
    membership_mesh: Handle<Mesh>,
    activity_mesh: Handle<Mesh>,
    tile: Option<(Handle<StandardMaterial>, Transform)>,
    glyph_mesh: Handle<Mesh>,
    glyphs: Vec<Handle<StandardMaterial>>,
    heat_glyphs: Vec<Vec<Handle<StandardMaterial>>>,
    glyph_textures: Vec<(crate::theme::ThemeName, Vec<Handle<Image>>)>,
    avatars: Vec<Handle<StandardMaterial>>,
    glass: Vec<Handle<StandardMaterial>>,
    directories: Vec<Vec<Handle<StandardMaterial>>>,
}

impl Palette {
    fn new(world: &mut World, theme: &Theme) -> Self {
        let mut meshes = world.resource_mut::<Assets<Mesh>>();
        let cube = meshes.add(Cuboid::default());
        let folder = meshes.add(folder_mesh());
        let sphere = meshes.add(styling::avatar_mesh(theme));
        let disc = meshes.add(Circle::new(theme.avatar_radius));
        let beam = meshes.add(Cylinder::new(0.035, 1.0));
        let glyph_mesh = meshes.add(Rectangle::default());
        let membership_mesh =
            meshes.add(styling::ring_mesh(theme, theme.membership_radii));
        let activity_mesh = meshes.add(styling::ring_mesh(theme, theme.activity_radii));
        let glyph_textures = Self::preload_glyphs(world, theme);
        let textures = &glyph_textures
            .iter()
            .find(|(name, _)| *name == theme.name)
            .expect("every registered theme has preloaded textures")
            .1;
        let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
        let glyphs: Vec<_> = textures
            .iter()
            .map(|texture| {
                materials.add(StandardMaterial {
                    base_color_texture: Some(texture.clone()),
                    ..theme.glyph_material.clone()
                })
            })
            .collect();
        let heat_glyphs = Self::heat_materials(&mut materials, &glyphs, theme);
        let tile = theme
            .file_tile
            .as_ref()
            .map(|tile| (materials.add(tile.material.clone()), tile.transform));
        let edge = materials.add(theme.edge.clone());
        let floor = materials.add(theme.floor.clone());
        let grid = materials.add(theme.grid.clone());
        let descending = materials.add(theme.descending.clone());
        let rising = materials.add(theme.rising.clone());
        let avatars = theme
            .avatars
            .each_ref()
            .map(|material| materials.add(material.clone()))
            .to_vec();
        let glass = theme
            .glass
            .each_ref()
            .map(|material| materials.add(material.clone()))
            .to_vec();
        let directories = theme
            .directories
            .iter()
            .map(|depths| {
                depths
                    .iter()
                    .map(|material| materials.add(material.clone()))
                    .collect()
            })
            .collect();
        Self {
            folder,
            cube,
            sphere,
            disc,
            beam,
            edge,
            floor,
            grid,
            descending,
            rising,
            membership_mesh,
            activity_mesh,
            tile,
            glyph_mesh,
            glyphs,
            heat_glyphs,
            glyph_textures,
            avatars,
            glass,
            directories,
        }
    }

    fn preload_glyphs(
        world: &mut World,
        theme: &Theme,
    ) -> Vec<(crate::theme::ThemeName, Vec<Handle<Image>>)> {
        vec![(theme.name, raster_glyphs(theme).install(world))]
    }

    fn heat_materials(
        materials: &mut Assets<StandardMaterial>,
        glyphs: &[Handle<StandardMaterial>],
        theme: &Theme,
    ) -> Vec<Vec<Handle<StandardMaterial>>> {
        glyphs
            .iter()
            .map(|handle| {
                let base = materials.get(handle).expect("glyph material").clone();
                (1_u16..32)
                    .map(|bucket| {
                        materials.add(StandardMaterial {
                            base_color: theme.heat_color(f32::from(bucket) / 31.0),
                            emissive: theme.heat_emissive(f32::from(bucket) / 31.0),
                            ..base.clone()
                        })
                    })
                    .collect()
            })
            .collect()
    }

    fn heat_glyph(&self, kind: usize, heat: f32) -> &Handle<StandardMaterial> {
        let bucket = (1_u16..32)
            .take_while(|bucket| heat >= (f32::from(*bucket) - 0.5) / 31.0)
            .count();
        if bucket == 0 {
            &self.glyphs[kind]
        } else {
            &self.heat_glyphs[kind][bucket - 1]
        }
    }

    fn directory(&self, slot: usize, depth: u16) -> &Handle<StandardMaterial> {
        if depth == 0 {
            return &self.edge;
        }
        let depths = &self.directories[slot % self.directories.len()];
        &depths[usize::from(depth - 1).min(depths.len() - 1)]
    }
}

#[derive(Default)]
struct DirectoryHues {
    slots: Vec<usize>,
    top_levels: usize,
}

fn folder_mesh() -> Mesh {
    let mut folder = Mesh::from(Cuboid::new(1.4, 0.1, 0.9));
    folder
        .merge(
            &Mesh::from(Cuboid::new(0.55, 0.1, 0.3))
                .translated_by(Vec3::new(-0.425, 0.0, -0.5)),
        )
        .expect("folder parts share mesh attributes");
    folder
}

impl DirectoryHues {
    fn sync(&mut self, layout: &Layout) {
        // Retired directories count too, so frame sampling cannot shift later hues.
        for index in self.slots.len()..layout.dir_count() {
            let slot = match layout.dir(index).parent {
                None => 0,
                Some(0) => {
                    let slot = self.top_levels;
                    self.top_levels += 1;
                    slot
                }
                Some(parent) => self.slots[parent],
            };
            self.slots.push(slot);
        }
    }
}

#[derive(Resource, Default)]
struct Pools {
    lod: lod::Lod,
    aggregates: Vec<Entity>,
    files: HashMap<u64, Entity>,
    beams: Vec<Entity>,
    fragments: Vec<Entity>,
    edges: Vec<Entity>,
    directories: Vec<Entity>,
    hues: DirectoryHues,
    avatars: Vec<Entity>,
    grid: Vec<Entity>,
}

fn app(simulation: Simulation, title: String) -> App {
    let mut app = App::new();
    let theme = simulation.theme().clone();
    app.insert_resource(ClearColor(theme.clear))
        .insert_resource(Playback(simulation))
        .insert_resource(Title(title))
        .init_resource::<Pools>()
        .add_systems(Startup, setup)
        .add_systems(Update, animate)
        .add_systems(
            PostUpdate,
            (sync_avatar_camera, face_portraits)
                .chain()
                .before(bevy::camera::CameraUpdateSystems)
                .before(bevy::transform::TransformSystems::Propagate),
        )
        .add_systems(
            PostUpdate,
            labels
                .after(bevy::camera::CameraUpdateSystems)
                .after(bevy::transform::TransformSystems::Propagate)
                .before(UiSystems::Prepare),
        );
    app
}

#[derive(Resource)]
struct Title(String);

pub(super) fn preview(simulation: Simulation, title: String) {
    let camera = PreviewCamera::new(simulation.view());
    let mut app = app(simulation, title);
    app.insert_resource(camera)
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Codestrand — layout study".to_owned(),
                resolution: (WIDTH, HEIGHT).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(SceneTarget(RenderTarget::default()))
        .insert_resource(Frame {
            index: 0,
            fps: model::PREVIEW_FPS,
        })
        .add_systems(Update, preview_clock.before(animate))
        .add_systems(Update, preview_camera.after(animate));
    app.run();
}

pub(super) fn headless(
    simulation: Simulation,
    title: String,
    fps: u16,
) -> (App, Handle<Image>) {
    let mut app = app(simulation, title);
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .set(RenderPlugin {
                synchronous_pipeline_compilation: true,
                ..default()
            })
            .disable::<WinitPlugin>()
            .disable::<PipelinedRenderingPlugin>(),
    );
    let mut image =
        Image::new_target_texture(WIDTH, HEIGHT, TextureFormat::Rgba8UnormSrgb, None);
    image.asset_usage = RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD;
    let target = app.world_mut().resource_mut::<Assets<Image>>().add(image);
    app.insert_resource(SceneTarget(target.clone().into()))
        .insert_resource(Frame { index: 0, fps });
    (app, target)
}

fn setup(world: &mut World) {
    let target = world.resource::<SceneTarget>().0.clone();
    let camera = world
        .spawn((
            Camera3d::default(),
            Hdr,
            target.clone(),
            Tonemapping::None,
            DebandDither::Disabled,
            Transform::from_xyz(6.0, 9.5, 12.0).looking_at(Vec3::ZERO, Vec3::Y),
            SceneCamera,
        ))
        .id();
    world.spawn((
        Camera3d {
            depth_load_op: Camera3dDepthLoadOp::Clear(0.0),
            ..default()
        },
        Hdr,
        Camera {
            order: 1,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        target,
        RenderLayers::layer(AVATAR_LAYER),
        Tonemapping::None,
        DebandDither::Disabled,
        AvatarCamera,
    ));
    let theme = world.resource::<Playback>().0.theme().clone();
    let palette = Palette::new(world, &theme);
    world.spawn((
        Mesh3d(palette.glyph_mesh.clone()),
        MeshMaterial3d(palette.floor.clone()),
        Transform::from_xyz(0.0, -0.18, 0.0)
            .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
            .with_scale(Vec3::splat(4000.0)),
        styling::Ground,
    ));
    let grid: Vec<_> = (0..(4 * GRID_LINES + 2))
        .map(|_| {
            world
                .spawn((
                    Mesh3d(palette.cube.clone()),
                    MeshMaterial3d(palette.grid.clone()),
                    Transform::default(),
                ))
                .id()
        })
        .collect();
    world.resource_mut::<Pools>().grid = grid;
    world.insert_resource(palette);
    for _ in 0..LABELS {
        world.spawn((
            Text::new(""),
            TextFont::from_font_size(14.0),
            TextColor(theme.label),
            BackgroundColor::default(),
            BorderColor::default(),
            Node {
                position_type: PositionType::Absolute,
                padding: UiRect::axes(px(3.0), px(1.0)),
                ..default()
            },
            theme.label_shadow,
            ZIndex::default(),
            Visibility::Hidden,
            UiTargetCamera(camera),
            Label,
        ));
    }
    for _ in 0..crate::diff_samples::PER_COMMIT {
        world.spawn((
            Text::new(""),
            TextFont::from_font_size(4.0),
            TextColor(theme.diff_added),
            theme.label_shadow,
            TextLayout::new(Justify::Left, bevy::text::LineBreak::NoWrap),
            Node {
                position_type: PositionType::Absolute,
                border: UiRect::all(px(1.0)),
                ..default()
            },
            BackgroundColor(theme.clear),
            BorderColor::all(theme.diff_added),
            ZIndex(1),
            Visibility::Hidden,
            UiTargetCamera(camera),
            DiffSample,
        ));
    }
    let title = world.resource::<Title>().0.clone();
    heading(world, camera, "CODESTRAND", 28.0, 28.0, 30.0);
    heading(world, camera, &title, 28.0, 68.0, 16.0);
    let caption = heading(world, camera, " ", 28.0, 668.0, 14.0);
    world.entity_mut(caption).insert(Caption);
    world.init_resource::<Assets<Font>>();
    styling::setup(world, &theme);
}

fn heading(
    world: &mut World,
    camera: Entity,
    text: &str,
    left: f32,
    top: f32,
    size: f32,
) -> Entity {
    let heading_color = world.resource::<Playback>().0.theme().heading;
    world
        .spawn((
            Text::new(text),
            TextFont::from_font_size(size),
            TextColor(heading_color),
            Node {
                position_type: PositionType::Absolute,
                left: px(left),
                top: px(top),
                ..default()
            },
            UiTargetCamera(camera),
        ))
        .id()
}

fn preview_clock(world: &mut World, mut elapsed: Local<std::time::Duration>) {
    preview_theme(world);
    let keys = world.resource::<ButtonInput<KeyCode>>();
    let factor = if keys.just_pressed(KeyCode::BracketRight) {
        Some(2.0)
    } else if keys.just_pressed(KeyCode::BracketLeft) {
        Some(0.5)
    } else {
        None
    };
    if let Some(factor) = factor {
        world.resource_mut::<Playback>().0.change_rate(factor);
    }
    let multiplier = world.resource::<Playback>().0.playback_multiplier();
    *elapsed += world.resource::<Time<Real>>().delta().mul_f32(multiplier);
    let duration = world.resource::<Playback>().0.duration();
    let fps = world.resource::<Frame>().fps;
    let frames = model::frames_for(duration, fps).unwrap_or(u32::MAX);
    let period_nanos = (u128::from(frames) * 1_000_000_000).div_ceil(u128::from(fps));
    if elapsed.as_nanos() >= period_nanos {
        *elapsed = std::time::Duration::from_nanos(
            u64::try_from(elapsed.as_nanos() % period_nanos)
                .expect("loop period fits in u64 nanoseconds"),
        );
        world.resource_mut::<Playback>().0.restart();
    }
    world.resource_mut::<Frame>().index = model::preview_frame(*elapsed, fps);
    if world
        .resource::<ButtonInput<KeyCode>>()
        .just_pressed(KeyCode::Escape)
    {
        world.write_message(AppExit::Success);
    }
}

fn preview_direction(
    keys: &ButtonInput<KeyCode>,
) -> Option<crate::theme::ThemeDirection> {
    if keys.just_pressed(KeyCode::Period) {
        Some(crate::theme::ThemeDirection::Next)
    } else if keys.just_pressed(KeyCode::Comma) {
        Some(crate::theme::ThemeDirection::Previous)
    } else {
        None
    }
}

#[derive(Resource)]
struct PendingTheme {
    name: crate::theme::ThemeName,
    task: bevy::tasks::Task<(RasterGlyphs, Option<Image>, Option<Image>)>,
}

struct RasterGlyphs {
    images: Vec<Image>,
    indices: Vec<usize>,
}

impl RasterGlyphs {
    fn install(self, world: &mut World) -> Vec<Handle<Image>> {
        let mut assets = world.resource_mut::<Assets<Image>>();
        let handles: Vec<_> = self
            .images
            .into_iter()
            .map(|image| assets.add(image))
            .collect();
        self.indices
            .into_iter()
            .map(|index| handles[index].clone())
            .collect()
    }
}

fn raster_glyphs(theme: &Theme) -> RasterGlyphs {
    let mut shared = HashMap::new();
    let mut images = Vec::new();
    let indices = theme
        .glyphs
        .iter()
        .enumerate()
        .map(|(kind, glyph)| {
            let svg = theme.glyph_svg(glyph.svg, kind);
            *shared.entry(svg).or_insert_with_key(|svg| {
                let index = images.len();
                images.push(themed_glyph_image(svg, theme));
                index
            })
        })
        .collect();
    RasterGlyphs { images, indices }
}

fn preview_theme(world: &mut World) {
    if let Some(mut pending) = world.get_resource_mut::<PendingTheme>() {
        if let Some((glyphs, ground, backdrop)) =
            bevy::tasks::futures::check_ready(&mut pending.task)
        {
            let name = pending.name;
            let handles = glyphs.install(world);
            world
                .resource_mut::<Palette>()
                .glyph_textures
                .push((name, handles));
            styling::install_images(world, name, ground, backdrop);
            world.remove_resource::<PendingTheme>();
            switch_theme(world, name);
        }
        return;
    }
    let Some(direction) = preview_direction(world.resource::<ButtonInput<KeyCode>>())
    else {
        return;
    };
    let name = world.resource::<Playback>().0.theme().name.cycle(direction);
    if world.contains_resource::<PreviewCamera>()
        && !world
            .resource::<Palette>()
            .glyph_textures
            .iter()
            .any(|(loaded, _)| *loaded == name)
    {
        let task = bevy::tasks::AsyncComputeTaskPool::get().spawn(async move {
            let theme = name.build();
            let (ground, backdrop) = styling::raster_images(&theme);
            (raster_glyphs(&theme), ground, backdrop)
        });
        world.insert_resource(PendingTheme { name, task });
        return;
    }
    switch_theme(world, name);
}

fn cache_glyphs(world: &mut World, theme: &Theme) {
    world.resource_scope(|world, mut palette: Mut<Palette>| {
        if !palette
            .glyph_textures
            .iter()
            .any(|(name, _)| *name == theme.name)
        {
            palette
                .glyph_textures
                .extend(Palette::preload_glyphs(world, theme));
        }
    });
}

pub(super) fn switch_theme(world: &mut World, name: crate::theme::ThemeName) {
    let theme = name.build();
    cache_glyphs(world, &theme);
    world.resource_mut::<Playback>().0.set_theme(theme.clone());
    world.resource_scope(|world, palette: Mut<Palette>| {
        let mut meshes = world.resource_mut::<Assets<Mesh>>();
        *meshes
            .get_mut(&palette.sphere)
            .expect("palette avatar mesh remains allocated") =
            styling::avatar_mesh(&theme);
        *meshes
            .get_mut(&palette.disc)
            .expect("palette portrait mesh remains allocated") =
            Circle::new(theme.avatar_radius).into();
        *meshes
            .get_mut(&palette.membership_mesh)
            .expect("palette membership mesh remains allocated") =
            styling::ring_mesh(&theme, theme.membership_radii);
        *meshes
            .get_mut(&palette.activity_mesh)
            .expect("palette activity mesh remains allocated") =
            styling::ring_mesh(&theme, theme.activity_radii);
        let replacements = [
            (&palette.edge, &theme.edge),
            (&palette.floor, &theme.floor),
            (&palette.grid, &theme.grid),
            (&palette.descending, &theme.descending),
            (&palette.rising, &theme.rising),
        ]
        .into_iter()
        .chain(palette.avatars.iter().zip(&theme.avatars))
        .chain(palette.glass.iter().zip(&theme.glass))
        .chain(
            palette
                .directories
                .iter()
                .flatten()
                .zip(theme.directories.iter().flatten()),
        );
        let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
        for (handle, replacement) in replacements {
            *materials
                .get_mut(handle)
                .expect("palette material remains allocated") = replacement.clone();
        }
        if let (Some((handle, _)), Some(tile)) = (&palette.tile, &theme.file_tile) {
            *materials
                .get_mut(handle)
                .expect("palette tile material remains allocated") =
                tile.material.clone();
        }
        let textures = &palette
            .glyph_textures
            .iter()
            .find(|(name, _)| *name == theme.name)
            .expect("every registered theme has preloaded textures")
            .1;
        for (handles, texture) in palette.heat_glyphs.iter().zip(textures) {
            for (bucket, handle) in (1_u16..32).zip(handles) {
                *world
                    .resource_mut::<Assets<StandardMaterial>>()
                    .get_mut(handle)
                    .expect("heat bucket") = StandardMaterial {
                    base_color: theme.heat_color(f32::from(bucket) / 31.0),
                    emissive: theme.heat_emissive(f32::from(bucket) / 31.0),
                    base_color_texture: Some(texture.clone()),
                    ..theme.glyph_material.clone()
                };
            }
        }
        for (handle, texture) in palette.glyphs.iter().zip(textures) {
            *world
                .resource_mut::<Assets<StandardMaterial>>()
                .get_mut(handle)
                .expect("palette glyph material remains allocated") =
                StandardMaterial {
                    base_color_texture: Some(texture.clone()),
                    ..theme.glyph_material.clone()
                };
        }
    });
    world.insert_resource(ClearColor(theme.clear));
    if world.contains_resource::<styling::Styles>() {
        styling::apply(world, &theme);
    }
    for mut color in world
        .query_filtered::<&mut TextColor, Without<Label>>()
        .iter_mut(world)
    {
        color.0 = theme.heading;
    }
    for mut shadow in world
        .query_filtered::<&mut TextShadow, With<Label>>()
        .iter_mut(world)
    {
        *shadow = theme.label_shadow;
    }
}

fn themed_glyph_image(svg: &str, theme: &Theme) -> Image {
    let mut image = glyph_image(svg, theme.glyph_texture);
    if let Some(ink) = theme.glyph_ink {
        let pixels = image.data.as_mut().expect("glyph pixels");
        let mut offset = 0;
        for level in 0..image.texture_descriptor.mip_level_count {
            let size = u32::from(theme.glyph_texture.size) >> level;
            let length = usize::try_from(size * size * 4).expect("small glyph");
            let mip = &mut pixels[offset..offset + length];
            let source = image::RgbaImage::from_raw(size, size, mip.to_vec())
                .expect("glyph dimensions");
            let glow = image::imageops::blur(&source, 1.5);
            for (pixel, blurred) in
                mip.as_chunks_mut::<4>().0.iter_mut().zip(glow.pixels())
            {
                let luminance = (54 * u32::from(pixel[0])
                    + 183 * u32::from(pixel[1])
                    + 19 * u32::from(pixel[2]))
                    / 256;
                let strength = 48 + 207 * luminance / 255;
                for (channel, tint) in pixel[..3].iter_mut().zip(ink) {
                    *channel = u8::try_from(u32::from(tint) * strength / 255)
                        .expect("tinted channel fits in a byte");
                }
                pixel[3] = pixel[3].max(blurred[3] / 2);
            }
            offset += length;
        }
    }
    image
}

fn preview_camera(world: &mut World, mut drag_from: Local<Option<Vec2>>) {
    let mut windows = world.query::<&Window>();
    let cursor = windows
        .iter(world)
        .find(|window| window.focused)
        .map(Window::cursor_position);
    let previous = drag_from.take();
    let mut input = Input::default();
    if let Some(cursor) = cursor {
        let keys = world.resource::<ButtonInput<KeyCode>>();
        let axis = |positive, negative| {
            f32::from(u8::from(keys.pressed(positive)))
                - f32::from(u8::from(keys.pressed(negative)))
        };
        input.pan = Vec2::new(
            axis(KeyCode::KeyD, KeyCode::KeyA),
            axis(KeyCode::KeyW, KeyCode::KeyS),
        )
        .normalize_or_zero();
        input.cycle = keys.just_pressed(KeyCode::Tab);
        input.reset = keys.just_pressed(KeyCode::Space);
        let dragging = world
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Right);
        // WSLg reports raw device motion as absolute positions, so orbit follows the cursor.
        if let (true, Some(from), Some(to)) = (dragging, previous, cursor) {
            input.orbit = to - from;
        }
        *drag_from = cursor.filter(|_| dragging);
        let scroll = world.resource::<AccumulatedMouseScroll>();
        input.zoom = scroll.delta.y
            * match scroll.unit {
                MouseScrollUnit::Line => 1.0,
                MouseScrollUnit::Pixel => 0.01,
            };
    }
    let dt = world.resource::<Time<Real>>().delta_secs();
    world.resource_scope(|world, mut camera: Mut<PreviewCamera>| {
        let simulation = &world.resource::<Playback>().0;
        camera.update(&input, simulation.view(), simulation.avatars(), dt);
        let (eye, focus) = camera.pose();
        let mut query = world.query_filtered::<&mut Transform, With<SceneCamera>>();
        if let Ok(mut transform) = query.single_mut(world) {
            *transform = Transform::from_translation(eye).looking_at(focus, Vec3::Y);
        }
    });
    world.resource_scope(|world, playback: Mut<Playback>| {
        styling::place_ground(world, &playback.0);
    });
}

pub(super) fn animate(world: &mut World) {
    let frame = world.resource::<Frame>();
    let (index, fps) = (frame.index, frame.fps);
    world.resource_scope(|world, mut playback: Mut<Playback>| {
        playback.0.advance_to(index, fps);
        world.resource_scope(|world, palette: Mut<Palette>| {
            world.resource_scope(|world, mut pools: Mut<Pools>| {
                let simulation = &playback.0;
                sync_files(world, &palette, &mut pools, simulation);
                sync_structure(world, &palette, &mut pools, simulation);
                sync_activity(world, &palette, &mut pools, simulation);
                place_camera(world, simulation);
            });
        });
    });
}

fn sync_lod(world: &mut World, pools: &mut Pools, simulation: &Simulation) {
    let layout = simulation.layout();
    let (focus, distance) = simulation.view();
    let (eye, focus) = world.get_resource::<PreviewCamera>().map_or(
        (focus + model::view_direction() * distance, focus),
        PreviewCamera::pose,
    );
    let height = world
        .query::<&Window>()
        .iter(world)
        .next()
        .map_or(720.0, Window::height);
    let fov = world
        .query_filtered::<&Projection, With<SceneCamera>>()
        .iter(world)
        .next()
        .and_then(|projection| match projection {
            Projection::Perspective(perspective) => Some(perspective.fov),
            _ => None,
        })
        .unwrap_or(std::f32::consts::FRAC_PI_4);
    let seconds = if world.contains_resource::<PreviewCamera>() {
        world
            .get_resource::<Time<Real>>()
            .map_or(0.0, Time::elapsed_secs)
    } else {
        simulation.seconds()
    };
    pools.lod.sample_clocks(
        layout,
        simulation.seconds(),
        seconds,
        lod::View {
            eye,
            focus,
            height,
            fov,
        },
    );
}

fn sync_files(
    world: &mut World,
    palette: &Palette,
    pools: &mut Pools,
    simulation: &Simulation,
) {
    sync_lod(world, pools, simulation);
    let layout = simulation.layout();
    pools.hues.sync(layout);
    pools.files.retain(|id, entity| {
        let keep = layout.file(*id).is_some();
        if !keep {
            world.despawn(*entity);
        }
        keep
    });
    for (id, file) in layout.files() {
        let detail = pools.lod.detail[file.dir];
        if detail == 0.0 {
            if let Some(&entity) = pools.files.get(&id) {
                set_visible(world, entity, false);
            }
            continue;
        }
        let entity = *pools.files.entry(id).or_insert_with(|| {
            spawn_file(
                world,
                palette,
                simulation.theme(),
                simulation.theme().glyph_kind(file.name()),
            )
        });
        set_visible(world, entity, true);
        let position = Vec3::new(file.position.x, 0.0, file.position.y);
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.translation = position;
            transform.scale =
                Vec3::splat(file.visible_presence(layout.dir(file.dir)) * detail);
        }
        let visual = world
            .get::<FileVisual>(entity)
            .expect("file entities are visuals");
        let (halo, current, membership) =
            (visual.halo, visual.author, visual.membership);
        let heat_material = visual.heat_material.clone();
        let glyph = visual.glyph;
        let glyph_kind = simulation.theme().glyph_kind(file.name());
        if visual.glyph_kind != glyph_kind {
            world
                .get_mut::<FileVisual>(entity)
                .expect("file entities are visuals")
                .glyph_kind = glyph_kind;
        }
        let material = palette.heat_glyph(glyph_kind, file.heat);
        if heat_material != *material {
            set_material(world, glyph, material);
            world
                .get_mut::<FileVisual>(entity)
                .expect("file visual")
                .heat_material = material.clone();
        }
        let dir = layout.dir(file.dir);
        set_material(
            world,
            membership,
            palette.directory(pools.hues.slots[file.dir], dir.depth),
        );
        let glowing = if simulation.theme().blackbody {
            file.heat > 0.3
        } else {
            file.activity > 0.03
        };
        if glowing && current != Some(file.author) {
            world.entity_mut(halo).insert(MeshMaterial3d(
                palette.glass[file.author % palette.glass.len()].clone(),
            ));
            world
                .get_mut::<FileVisual>(entity)
                .expect("file entities are visuals")
                .author = Some(file.author);
        }
        if let Some(mut transform) = world.get_mut::<Transform>(halo) {
            let grow = 1.0 + simulation.theme().activity_growth * file.activity;
            *transform = simulation.theme().activity_transform;
            transform.scale *= Vec3::new(grow, grow, 1.0);
        }
        set_visible(world, halo, glowing);
    }
}

fn glyph_image(svg: &str, settings: crate::theme::GlyphTexture) -> Image {
    assert!(
        settings.size.is_power_of_two(),
        "glyph size is a positive power of two"
    );
    assert!(
        (0.0..0.5).contains(&settings.padding),
        "glyph padding leaves space for artwork"
    );
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())
        .expect("vendored SVG glyphs are valid");
    let probe_size = settings.size.max(32);
    let probe_scale =
        f32::from(probe_size) / tree.size().width().max(tree.size().height());
    let mut probe =
        resvg::tiny_skia::Pixmap::new(u32::from(probe_size), u32::from(probe_size))
            .expect("glyph probe dimensions fit memory");
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(probe_scale, probe_scale),
        &mut probe.as_mut(),
    );
    let mut low = (probe_size, probe_size);
    let mut high = (0, 0);
    for (index, pixel) in probe.pixels().iter().enumerate() {
        if pixel.alpha() == 0 {
            continue;
        }
        let x =
            u16::try_from(index % usize::from(probe_size)).expect("probe column fits");
        let y = u16::try_from(index / usize::from(probe_size)).expect("probe row fits");
        low = (low.0.min(x), low.1.min(y));
        high = (high.0.max(x + 1), high.1.max(y + 1));
    }
    assert!(
        high.0 > low.0 && high.1 > low.1,
        "glyph has visible artwork"
    );
    let bounds = Rect::from_corners(
        Vec2::new(f32::from(low.0), f32::from(low.1)) / probe_scale,
        Vec2::new(f32::from(high.0), f32::from(high.1)) / probe_scale,
    );
    let levels = if settings.mipmaps {
        settings.size.ilog2() + 1
    } else {
        1
    };
    let mut pixels = Vec::new();
    for level in 0..levels {
        let size = settings.size >> level;
        let extent = f32::from(size);
        let scale = extent * (1.0 - 2.0 * settings.padding)
            / bounds.width().max(bounds.height());
        let transform = resvg::tiny_skia::Transform::from_row(
            scale,
            0.0,
            0.0,
            scale,
            (extent - bounds.width() * scale) * 0.5 - bounds.min.x * scale,
            (extent - bounds.height() * scale) * 0.5 - bounds.min.y * scale,
        );
        let mut pixmap =
            resvg::tiny_skia::Pixmap::new(u32::from(size), u32::from(size))
                .expect("glyph texture dimensions fit memory");
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        for pixel in pixmap.pixels() {
            let color = pixel.demultiply();
            pixels.extend_from_slice(&[
                color.red(),
                color.green(),
                color.blue(),
                color.alpha(),
            ]);
        }
    }
    let size = u32::from(settings.size);
    let mut image = Image::new_uninit(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.mip_level_count = levels;
    image.sampler = ImageSampler::linear();
    image.data = Some(pixels);
    image
}

fn spawn_file(
    world: &mut World,
    palette: &Palette,
    theme: &Theme,
    glyph_kind: usize,
) -> Entity {
    let heat_material = palette.glyphs[glyph_kind].clone();
    let membership = world
        .spawn((
            Mesh3d(palette.membership_mesh.clone()),
            MeshMaterial3d(palette.edge.clone()),
            theme.membership_transform,
        ))
        .id();
    let glyph = world
        .spawn((
            Mesh3d(palette.glyph_mesh.clone()),
            MeshMaterial3d(heat_material.clone()),
            theme.file_glyph_transform,
        ))
        .id();
    let halo = world
        .spawn((
            Mesh3d(palette.activity_mesh.clone()),
            MeshMaterial3d(palette.glass[0].clone()),
            theme.activity_transform,
            Visibility::Hidden,
        ))
        .id();
    let mut children = vec![membership, glyph, halo];
    if let Some((material, transform)) = &palette.tile {
        children.push(
            world
                .spawn((
                    Mesh3d(palette.cube.clone()),
                    MeshMaterial3d(material.clone()),
                    *transform,
                ))
                .id(),
        );
    }
    world
        .spawn((
            Transform::default(),
            Visibility::default(),
            FileVisual {
                heat_material,
                membership,
                glyph,
                glyph_kind,
                halo,
                author: None,
            },
        ))
        .add_children(&children)
        .id()
}

fn sync_structure(
    world: &mut World,
    palette: &Palette,
    pools: &mut Pools,
    simulation: &Simulation,
) {
    let layout = simulation.layout();
    pools.hues.sync(layout);
    let edges: Vec<_> = layout
        .dirs()
        .filter_map(|(index, dir)| {
            if dir
                .parent
                .is_some_and(|parent| pools.lod.detail[parent] == 0.0)
            {
                return None;
            }
            let parent = layout.dir(dir.parent?);
            Some((
                ground(parent.position) - Vec3::Y * 0.05,
                ground(dir.position) - Vec3::Y * 0.05,
                palette.directory(pools.hues.slots[index], dir.depth),
                simulation.theme().directory_edge_width(dir.depth)
                    * dir.parent.map_or(1.0, |parent| pools.lod.detail[parent]),
            ))
        })
        .flat_map(|(from, to, material, width)| {
            styling::edge_segments(simulation.theme(), from, to)
                .into_iter()
                .map(move |(a, b)| (a, b, material, width))
        })
        .collect();
    let pool = grow(
        world,
        &mut pools.edges,
        edges.len(),
        &palette.cube,
        &palette.edge,
    );
    for (entity, (from, to, material, width)) in pool.iter().zip(&edges) {
        if let Some(mut transform) = world.get_mut::<Transform>(*entity) {
            let delta = *to - *from;
            *transform = Transform::from_translation(from.midpoint(*to))
                .looking_to(delta.normalize_or(Vec3::Z), Vec3::Y)
                .with_scale(Vec3::new(
                    *width,
                    simulation.theme().directory_edge_height,
                    delta.length(),
                ));
        }
        set_visible(world, *entity, true);
        set_material(world, *entity, material);
    }
    hide_rest(world, pool, edges.len());
    let dirs: Vec<_> = layout.dirs().collect();
    let pool = grow(
        world,
        &mut pools.directories,
        dirs.len(),
        &palette.cube,
        &palette.edge,
    );
    for (entity, &(index, dir)) in pool.iter().zip(&dirs) {
        set_material(
            world,
            *entity,
            palette.directory(pools.hues.slots[index], dir.depth),
        );
        if let Some(mut transform) = world.get_mut::<Transform>(*entity) {
            *transform =
                Transform::from_translation(ground(dir.position) - Vec3::Y * 0.05)
                    .with_scale(
                        simulation.theme().directory_node_scale
                            * pools.lod.detail[index],
                    );
        }
        set_visible(world, *entity, pools.lod.detail[index] > 0.0);
    }
    hide_rest(world, pool, dirs.len());
    let glyphs = &pools.lod.glyphs;
    let pool = grow(
        world,
        &mut pools.aggregates,
        glyphs.len(),
        &palette.folder,
        &palette.edge,
    );
    for (&entity, &index) in pool.iter().zip(glyphs) {
        let dir = layout.dir(index);
        set_material(
            world,
            entity,
            palette.directory(pools.hues.slots[index], dir.depth),
        );
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            *transform = Transform::from_translation(ground(dir.position)).with_scale(
                Vec3::new(1.0, 0.1, 1.0)
                    * pools.lod.size[index]
                    * pools.lod.glyph[index],
            );
        }
        set_visible(world, entity, true);
    }
    hide_rest(world, pool, glyphs.len());
    sync_grid(world, palette, pools, simulation);
}

fn sync_grid(
    world: &mut World,
    palette: &Palette,
    pools: &Pools,
    simulation: &Simulation,
) {
    if world.contains_resource::<styling::Styles>() {
        styling::ground(world, palette, &pools.hues.slots, simulation);
    }

    let (focus, distance) = simulation.view();
    let extent = distance * 0.5;
    let spacing = 2.0_f32.powf((extent / 4.0).log2().ceil().max(0.0));
    let centre = (focus / spacing).round() * spacing;
    let half = spacing * f32::from(GRID_LINES);
    let thickness = 0.012 * spacing.sqrt();
    for (index, &entity) in pools.grid.iter().enumerate() {
        set_visible(world, entity, !simulation.theme().textured_ground);
        let line = f32::from(
            i16::try_from(index).expect("grid pool is small") % (2 * GRID_LINES + 1)
                - GRID_LINES,
        );
        let along_x = index
            >= usize::try_from(2 * GRID_LINES + 1).expect("grid size is positive");
        let (translation, scale) = if along_x {
            (
                Vec3::new(centre.x, -0.13, centre.z + line * spacing),
                Vec3::new(2.0 * half, 0.01, thickness),
            )
        } else {
            (
                Vec3::new(centre.x + line * spacing, -0.13, centre.z),
                Vec3::new(thickness, 0.01, 2.0 * half),
            )
        };
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            *transform = Transform::from_translation(translation).with_scale(scale);
        }
    }
}

fn set_material(
    world: &mut World,
    entity: Entity,
    material: &Handle<StandardMaterial>,
) {
    if world
        .get::<MeshMaterial3d<StandardMaterial>>(entity)
        .is_some_and(|current| current.0 != *material)
    {
        world
            .entity_mut(entity)
            .insert(MeshMaterial3d(material.clone()));
    }
}

fn attach_portrait(
    world: &mut World,
    palette: &Palette,
    entity: Entity,
    avatar: &model::Avatar,
) {
    if let Some(portrait) = &avatar.portrait {
        let mut image = Image::new_uninit(
            Extent3d {
                width: 128,
                height: 128,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        );
        let mut pixels = portrait.pixels.clone();
        let mut level = image::RgbaImage::from_raw(128, 128, portrait.pixels.clone())
            .expect("portrait pixels are 128 by 128 RGBA");
        while level.width() > 1 {
            level = image::imageops::resize(
                &level,
                level.width() / 2,
                level.height() / 2,
                image::imageops::FilterType::Triangle,
            );
            pixels.extend_from_slice(level.as_raw());
        }
        image.texture_descriptor.mip_level_count = 8;
        image.data = Some(pixels);
        image.sampler = ImageSampler::linear();
        let texture = world.resource_mut::<Assets<Image>>().add(image);
        let material =
            world
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial {
                    base_color_texture: Some(texture),
                    unlit: true,
                    alpha_mode: AlphaMode::Mask(0.5),
                    ..default()
                });
        let face = world
            .spawn((
                Mesh3d(palette.disc.clone()),
                MeshMaterial3d(material),
                Transform::from_xyz(0.0, 0.0, 0.002).with_scale(Vec3::splat(0.9)),
                RenderLayers::layer(AVATAR_LAYER),
            ))
            .id();
        world
            .entity_mut(entity)
            .insert(PortraitDisc)
            .add_child(face);
    }
}

fn sync_activity(
    world: &mut World,
    palette: &Palette,
    pools: &mut Pools,
    simulation: &Simulation,
) {
    let avatars = simulation.avatars();
    while pools.avatars.len() < avatars.len() {
        let index = pools.avatars.len();
        let entity = world
            .spawn((
                Mesh3d(if avatars[index].portrait.is_some() {
                    palette.disc.clone()
                } else {
                    palette.sphere.clone()
                }),
                MeshMaterial3d(palette.avatars[index % palette.avatars.len()].clone()),
                Transform::default(),
                Visibility::Hidden,
                RenderLayers::layer(AVATAR_LAYER),
                PortraitDisc,
            ))
            .id();
        attach_portrait(world, palette, entity, &avatars[index]);
        pools.avatars.push(entity);
    }
    for (index, &entity) in pools.avatars.iter().enumerate() {
        let avatar = avatars.get(index);
        let presence = avatar.map_or(0.0, |avatar| avatar.presence);
        if let (Some(avatar), Some(mut transform)) =
            (avatar, world.get_mut::<Transform>(entity))
        {
            transform.translation = avatar.position;
            transform.scale = Vec3::splat((presence * simulation.scale()).max(0.001));
        }
        set_visible(world, entity, presence > 0.0);
    }
    styling::beams(world, palette, pools, simulation);
    let fragments = simulation.fragments();
    let mut pool = std::mem::take(&mut pools.fragments);
    while pool.len() < fragments.len() {
        let entity = block(
            world,
            &palette.cube,
            &palette.descending,
            Vec3::ZERO,
            Vec3::ONE,
        );
        pool.push(entity);
    }
    for (entity, fragment) in pool.iter().zip(&fragments) {
        let material = if fragment.rising {
            &palette.rising
        } else {
            &palette.descending
        };
        world
            .entity_mut(*entity)
            .insert(MeshMaterial3d(material.clone()));
        if let Some(mut transform) = world.get_mut::<Transform>(*entity) {
            *transform = Transform::from_translation(fragment.position).with_scale(
                Vec3::new(0.12, 0.035, 0.2)
                    * (fragment.scale * simulation.scale().sqrt()).max(0.001),
            );
        }
        set_visible(world, *entity, fragment.scale > 0.0);
    }
    hide_rest(world, &pool, fragments.len());
    pools.fragments = pool;
}

fn place_camera(world: &mut World, simulation: &Simulation) {
    let (focus, distance) = simulation.view();
    let mut query = world.query_filtered::<&mut Transform, With<SceneCamera>>();
    if let Ok(mut transform) = query.single_mut(world) {
        *transform =
            Transform::from_translation(focus + model::view_direction() * distance)
                .looking_at(focus, Vec3::Y);
    }
}

fn grow<'a>(
    world: &mut World,
    pool: &'a mut Vec<Entity>,
    needed: usize,
    mesh: &Handle<Mesh>,
    material: &Handle<StandardMaterial>,
) -> &'a [Entity] {
    while pool.len() < needed {
        pool.push(
            world
                .spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(material.clone()),
                    Transform::default(),
                    Visibility::Hidden,
                ))
                .id(),
        );
    }
    pool
}

fn hide_rest(world: &mut World, pool: &[Entity], used: usize) {
    for &entity in &pool[used.min(pool.len())..] {
        set_visible(world, entity, false);
    }
}

fn set_visible(world: &mut World, entity: Entity, visible: bool) {
    let wanted = if visible {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if let Some(mut visibility) = world.get_mut::<Visibility>(entity)
        && *visibility != wanted
    {
        *visibility = wanted;
    }
}

fn place_beam(world: &mut World, entity: Entity, from: Vec3, to: Vec3, width: f32) {
    let delta = to - from;
    if let Some(mut transform) = world.get_mut::<Transform>(entity) {
        *transform = Transform::from_translation((from + to) * 0.5)
            .with_rotation(Quat::from_rotation_arc(
                Vec3::Y,
                delta.normalize_or(Vec3::Y),
            ))
            .with_scale(Vec3::new(
                width.max(0.001),
                delta.length().max(0.001),
                width.max(0.001),
            ));
    }
    set_visible(world, entity, width > 0.0);
}

fn block(
    world: &mut World,
    mesh: &Handle<Mesh>,
    material: &Handle<StandardMaterial>,
    position: Vec3,
    size: Vec3,
) -> Entity {
    world
        .spawn((
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(position).with_scale(size),
            Visibility::default(),
        ))
        .id()
}

fn ground(point: Vec2) -> Vec3 {
    Vec3::new(point.x, 0.0, point.y)
}

fn place_labels(
    mut wanted: Vec<(model::Label, Vec2, f32)>,
    height: f32,
) -> Vec<(model::Label, Vec2, f32)> {
    wanted.sort_by_key(|(label, ..)| label.kind);
    let mut placed: Vec<Rect> = Vec::new();
    let mut chosen = Vec::new();
    for (label, point, distance) in wanted {
        let width = 8.0
            + 8.6
                * f32::from(
                    u16::try_from(label.text.chars().count()).unwrap_or(u16::MAX),
                );
        let rect = Rect::from_corners(point, point + Vec2::new(width, height));
        if rect.min.x < 0.0
            || rect.min.y < 0.0
            || rect.max.x > 1280.0
            || rect.max.y > 720.0
            || (label.kind != model::LabelKind::Avatar
                && placed.iter().any(|other| !other.intersect(rect).is_empty()))
        {
            continue;
        }
        placed.push(rect);
        chosen.push((label, point, distance));
        if chosen.len() == LABELS {
            break;
        }
    }
    chosen.sort_by(|(a, _, a_distance), (b, _, b_distance)| {
        let a_avatar = a.kind == model::LabelKind::Avatar;
        let b_avatar = b.kind == model::LabelKind::Avatar;
        a_avatar
            .cmp(&b_avatar)
            .then_with(|| b_distance.total_cmp(a_distance))
    });
    chosen
}
fn labels(world: &mut World) {
    let mut camera_query =
        world.query_filtered::<(&Camera, &GlobalTransform), With<SceneCamera>>();
    let Ok((camera, transform)) = camera_query.single(world) else {
        return;
    };
    let (camera, transform) = (camera.clone(), *transform);
    diff_samples(world, &camera, &transform);
    let playback = world.resource::<Playback>();
    let mut caption = playback.0.caption();
    caption.push_str("  |  ");
    caption.push_str(playback.0.theme().name.title());
    if let Some(camera) = world.get_resource::<PreviewCamera>() {
        caption.push_str("  |  ");
        caption.push_str(&camera.caption());
    }
    let geometry = playback.0.theme();
    let lod = &world.resource::<Pools>().lod;
    let wanted = playback
        .0
        .labels_for_detail(|index| {
            lod.detail.get(index).is_none_or(|&weight| weight > 0.0)
                || lod.glyph.get(index).is_some_and(|&weight| weight > 0.0)
        })
        .into_iter()
        .filter_map(|label| {
            let point = camera.world_to_viewport(&transform, label.position).ok()?;
            let anchor = if label.kind == model::LabelKind::Avatar {
                label.position - geometry.avatar_label_offset
            } else {
                label.position
            };
            let distance = transform.translation().distance_squared(anchor);
            Some((label, point, distance))
        });
    let chosen = place_labels(wanted.collect(), geometry.label_height);
    let theme = geometry.clone();
    let points: Vec<_> = chosen.iter().map(|(_, point, _)| *point).collect();
    let camera_entity = world
        .query_filtered::<Entity, With<SceneCamera>>()
        .single(world)
        .expect("scene camera");
    if world.contains_resource::<styling::Styles>() {
        styling::leaders(world, &points, &theme, camera_entity);
    }
    let mut query = world.query_filtered::<(
        &mut Text,
        &mut Node,
        &mut Visibility,
        &mut TextColor,
        &mut ZIndex,
    ), With<Label>>();
    let theme = world.resource::<Playback>().0.theme().clone();
    let (emphasis_color, landmark_color) = (theme.label_emphasis, theme.label_landmark);
    let mut chosen = chosen.into_iter();
    for (index, (mut text, mut node, mut visibility, mut color, mut z_index)) in
        query.iter_mut(world).enumerate()
    {
        if let Some((label, point, _)) = chosen.next() {
            z_index.set_if_neq(ZIndex(
                i32::try_from(index).expect("label pool fits in i32"),
            ));
            let emphasis = label.kind != model::LabelKind::Landmark;
            let label = if theme.label_style == crate::theme::LabelStyle::SmallCaps {
                label.text.to_uppercase()
            } else {
                label.text
            };
            if text.0 != label {
                text.0 = label;
            }
            if node.left != px(point.x) || node.top != px(point.y) {
                node.left = px(point.x);
                node.top = px(point.y);
            }
            let wanted = if emphasis {
                emphasis_color
            } else {
                landmark_color
            };
            if color.0 != wanted {
                color.0 = wanted;
            }
            visibility.set_if_neq(Visibility::Inherited);
        } else {
            visibility.set_if_neq(Visibility::Hidden);
        }
    }
    let mut query = world.query_filtered::<&mut Text, With<Caption>>();
    if let Ok(mut text) = query.single_mut(world)
        && text.0 != caption
    {
        text.0 = caption;
    }
}

fn diff_samples(world: &mut World, camera: &Camera, transform: &GlobalTransform) {
    let playback = &world.resource::<Playback>().0;
    let theme = playback.theme();
    let wanted: Vec<_> = playback
        .diff_lines()
        .into_iter()
        .filter_map(|line| {
            let point = camera.world_to_viewport(transform, line.position).ok()?;
            let top = camera
                .world_to_viewport(
                    transform,
                    line.position + transform.up() * (0.12 * line.scale),
                )
                .ok()?;
            let projected = point.distance(top);
            let visibility = crate::diff_samples::sample_visibility(projected);
            if visibility == 0.0 {
                return None;
            }
            let size = projected.round().clamp(1.0, 28.0);
            let color = if line.sample.added {
                theme.diff_added
            } else {
                theme.diff_removed
            };
            let prefix = if line.sample.added { '+' } else { '-' };
            let width = f32::from(
                u8::try_from(line.sample.text.len() + 2).expect("bounded ASCII sample"),
            ) * size
                * 0.6;
            Some((
                format!("{prefix} {}", line.sample.text),
                point - Vec2::new(width * 0.5, size * 0.6),
                size,
                color.with_alpha(line.alpha * visibility),
                theme.clear.with_alpha(line.alpha * visibility * 0.94),
            ))
        })
        .collect();
    let mut wanted = wanted.into_iter();
    let mut query = world.query_filtered::<(
        &mut Text,
        &mut Node,
        &mut TextFont,
        &mut TextColor,
        &mut BackgroundColor,
        &mut BorderColor,
        &mut Visibility,
    ), With<DiffSample>>();
    for (
        mut text,
        mut node,
        mut font,
        mut color,
        mut background,
        mut border,
        mut visibility,
    ) in query.iter_mut(world)
    {
        if let Some((value, point, size, ink, fill)) = wanted.next() {
            text.set_if_neq(Text::new(value));
            node.left = px(point.x);
            node.top = px(point.y);
            let font_size = bevy::text::FontSize::Px(size);
            if font.font_size != font_size {
                font.font_size = font_size;
            }
            color.set_if_neq(TextColor(ink));
            background.set_if_neq(BackgroundColor(fill));
            border.set_if_neq(BorderColor::all(ink));
            visibility.set_if_neq(Visibility::Inherited);
        } else {
            visibility.set_if_neq(Visibility::Hidden);
        }
    }
}

fn sync_avatar_camera(world: &mut World) {
    let Ok((transform, projection)) = world
        .query_filtered::<(&Transform, &Projection), With<SceneCamera>>()
        .single(world)
    else {
        return;
    };
    let (transform, projection) = (*transform, projection.clone());
    for (mut avatar_transform, mut avatar_projection) in world
        .query_filtered::<(&mut Transform, &mut Projection), With<AvatarCamera>>()
        .iter_mut(world)
    {
        *avatar_transform = transform;
        *avatar_projection = projection.clone();
    }
}

fn face_portraits(world: &mut World) {
    let rotation = world
        .query_filtered::<&Transform, With<SceneCamera>>()
        .single(world)
        .map(|transform| transform.rotation)
        .ok();
    if let Some(rotation) = rotation {
        for mut transform in world
            .query_filtered::<&mut Transform, With<PortraitDisc>>()
            .iter_mut(world)
        {
            transform.rotation = rotation;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_switch_loads_uncached_art_off_thread_before_applying_theme() {
        bevy::tasks::AsyncComputeTaskPool::get_or_init(bevy::tasks::TaskPool::new);
        let simulation = Simulation::new(
            crate::timeline::synthetic(),
            &model::Options::default(),
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        let camera = PreviewCamera::new(simulation.view());
        let mut world = World::new();
        world.insert_resource(Playback(simulation));
        world.insert_resource(SceneTarget(Handle::<Image>::default().into()));
        world.insert_resource(Title("test".to_owned()));
        world.insert_resource(camera);
        world.init_resource::<Pools>();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        world.init_resource::<ButtonInput<KeyCode>>();
        setup(&mut world);
        assert_eq!(world.resource::<Palette>().glyph_textures.len(), 1);
        world
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Period);
        preview_theme(&mut world);
        world.resource_mut::<ButtonInput<KeyCode>>().reset_all();
        assert!(world.contains_resource::<PendingTheme>());
        assert_eq!(
            world.resource::<Playback>().0.theme().name,
            crate::theme::ThemeName::Neon
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while world.contains_resource::<PendingTheme>() {
            assert!(
                std::time::Instant::now() < deadline,
                "theme worker timed out"
            );
            preview_theme(&mut world);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            world.resource::<Playback>().0.theme().name,
            crate::theme::ThemeName::Phosphor
        );
        assert_eq!(world.resource::<Palette>().glyph_textures.len(), 2);
        assert_eq!(world.resource::<Playback>().0.seconds(), 0.0);
    }

    #[test]
    fn preview_loop_preserves_overshoot_across_multiple_cycles() {
        let mut simulation = Simulation::new(
            crate::timeline::synthetic().into_iter().take(1).collect(),
            &model::Options::default(),
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        assert_eq!(simulation.duration(), 3.0);
        simulation.change_rate(8.0);
        let mut app = App::new();
        app.insert_resource(Playback(simulation))
            .insert_resource(Frame { index: 0, fps: 30 })
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<Time<Real>>()
            .add_systems(Update, preview_clock);
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .update_with_duration(std::time::Duration::ZERO);
        let mut total = std::time::Duration::ZERO;
        let deltas = [
            400_000_000,
            1_250_000_000,
            225_000_000,
            1_000_000,
            4_000_000,
        ]
        .into_iter()
        .chain(std::iter::repeat_n(33_333_333, 200));
        for nanos in deltas {
            let delta = std::time::Duration::from_nanos(nanos);
            total += delta.mul_f32(8.0);
            app.world_mut()
                .resource_mut::<Time<Real>>()
                .update_with_duration(delta);
            app.update();
            let remainder = std::time::Duration::from_nanos(
                u64::try_from(total.as_nanos() % 3_000_000_000).unwrap(),
            );
            let expected = model::preview_frame(remainder, 30);
            let world = app.world_mut();
            let frame = world.resource::<Frame>().index;
            assert_eq!(frame, expected, "total timeline time: {total:?}");
            world.resource_mut::<Playback>().0.advance_to(frame, 30);
            assert_eq!(
                world.resource::<Playback>().0.seconds(),
                model::tick_time(u64::from(expected) * 2),
            );
        }
    }

    #[test]
    fn avatars_use_a_later_depth_cleared_pass_with_the_same_view() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        crate::contributors::Mappings::default().apply(&mut events);
        events[0].contributor = Some(crate::contributors::Contributor {
            key: crate::contributors::Key::Config(0),
            name: "Portrait".to_owned(),
            accounts: default(),
            portrait: Some(std::sync::Arc::new(crate::avatars::Portrait {
                pixels: vec![255; 128 * 128 * 4],
            })),
        });
        events[1].seconds = events[0].seconds;
        let simulation = Simulation::new(
            events,
            &model::Options::default(),
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        let mut world = World::new();
        world.insert_resource(Playback(simulation));
        world.insert_resource(SceneTarget(Handle::<Image>::default().into()));
        world.insert_resource(Title("test".to_owned()));
        world.insert_resource(Frame { index: 30, fps: 30 });
        world.init_resource::<Pools>();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        setup(&mut world);
        animate(&mut world);
        sync_avatar_camera(&mut world);
        let scene = world
            .query_filtered::<Entity, With<SceneCamera>>()
            .single(&world)
            .unwrap();
        let overlay = world
            .query_filtered::<Entity, With<AvatarCamera>>()
            .single(&world)
            .unwrap();
        assert!(
            world.entity(scene).contains::<Hdr>()
                && world.entity(overlay).contains::<Hdr>()
        );
        assert!(
            world.get::<Camera>(overlay).unwrap().order
                > world.get::<Camera>(scene).unwrap().order
        );
        assert!(matches!(
            world.get::<Camera>(overlay).unwrap().clear_color,
            ClearColorConfig::None
        ));
        assert!(matches!(
            world.get::<Camera3d>(overlay).unwrap().depth_load_op,
            Camera3dDepthLoadOp::Clear(0.0)
        ));
        assert_eq!(
            world.get::<Transform>(scene),
            world.get::<Transform>(overlay)
        );
        assert_eq!(
            world.get::<RenderTarget>(scene).unwrap().as_image(),
            world.get::<RenderTarget>(overlay).unwrap().as_image()
        );
        let layer = RenderLayers::layer(AVATAR_LAYER);
        assert_eq!(world.get::<RenderLayers>(overlay), Some(&layer));
        let pools = world.resource::<Pools>();
        assert_eq!(pools.avatars.len(), 2);
        for &avatar in &pools.avatars {
            assert_eq!(world.get::<RenderLayers>(avatar), Some(&layer));
        }
        let portrait = pools.avatars[0];
        let face = world.get::<Children>(portrait).unwrap()[0];
        assert_eq!(world.get::<RenderLayers>(face), Some(&layer));
        assert!(world.get::<Children>(pools.avatars[1]).is_none());
        for &beam in &pools.beams {
            assert!(world.get::<RenderLayers>(beam).is_none());
        }
        let changed =
            Transform::from_xyz(12.0, 18.0, -6.0).looking_at(Vec3::ZERO, Vec3::Y);
        world.entity_mut(scene).insert((
            changed,
            Projection::Perspective(PerspectiveProjection {
                fov: 0.8,
                ..default()
            }),
        ));
        sync_avatar_camera(&mut world);
        assert_eq!(world.get::<Transform>(overlay), Some(&changed));
        let Projection::Perspective(projection) =
            world.get::<Projection>(overlay).unwrap()
        else {
            panic!("avatar camera must use the scene projection");
        };
        assert_eq!(projection.fov, 0.8);
        assert_labels_stay_below_avatars_without_redundant_updates(&mut world, scene);
        assert_switches_preserve_playback_and_portraits(&mut world, scene, face);
    }

    fn assert_switches_preserve_playback_and_portraits(
        world: &mut World,
        scene: Entity,
        face: Entity,
    ) {
        let layer = RenderLayers::layer(AVATAR_LAYER);
        assert_eq!(world.resource::<Palette>().glyph_textures.len(), 1);
        let initial = world.resource::<Playback>().0.theme().name;
        for name in crate::theme::ThemeName::ALL {
            switch_theme(world, name);
        }
        switch_theme(world, initial);
        world.init_resource::<ButtonInput<KeyCode>>();
        let transforms: Vec<_> = world
            .query::<(Entity, &Transform)>()
            .iter(world)
            .map(|(entity, transform)| (entity, *transform))
            .collect();
        let positions: Vec<_> = world
            .resource::<Playback>()
            .0
            .layout()
            .files()
            .map(|(id, file)| (id, file.position))
            .collect();
        let seconds = world.resource::<Playback>().0.seconds();
        let starts = world.resource::<Playback>().0.scheduled_starts().to_vec();
        let portrait_material = world
            .get::<MeshMaterial3d<StandardMaterial>>(face)
            .unwrap()
            .0
            .clone();
        let portrait_texture = world
            .resource::<Assets<StandardMaterial>>()
            .get(&portrait_material)
            .unwrap()
            .base_color_texture
            .clone();
        for key in
            std::iter::repeat_n(KeyCode::Period, crate::theme::ThemeName::ALL.len())
                .chain(std::iter::repeat_n(
                    KeyCode::Comma,
                    crate::theme::ThemeName::ALL.len(),
                ))
        {
            let asset_counts = (
                world.resource::<Assets<Mesh>>().len(),
                world.resource::<Assets<Image>>().len(),
                world.resource::<Assets<StandardMaterial>>().len(),
            );
            world.resource_mut::<ButtonInput<KeyCode>>().press(key);
            preview_theme(world);
            world.resource_mut::<ButtonInput<KeyCode>>().reset_all();
            assert_eq!(world.resource::<Playback>().0.seconds(), seconds);
            assert_eq!(world.resource::<Playback>().0.scheduled_starts(), starts);
            assert_eq!(world.resource::<Playback>().0.playback_multiplier(), 1.0);
            assert_eq!(world.resource::<Frame>().index, 30);
            for &(entity, transform) in &transforms {
                assert_eq!(world.get::<Transform>(entity), Some(&transform));
            }
            assert_eq!(
                world
                    .resource::<Playback>()
                    .0
                    .layout()
                    .files()
                    .map(|(id, file)| (id, file.position))
                    .collect::<Vec<_>>(),
                positions
            );
            assert_eq!(
                (
                    world.resource::<Assets<Mesh>>().len(),
                    world.resource::<Assets<Image>>().len(),
                    world.resource::<Assets<StandardMaterial>>().len()
                ),
                asset_counts
            );
            assert_eq!(
                world
                    .resource::<Assets<StandardMaterial>>()
                    .get(&portrait_material)
                    .unwrap()
                    .base_color_texture,
                portrait_texture
            );
            assert_eq!(world.get::<RenderLayers>(face), Some(&layer));
            assert_theme_textures(world);
            assert_theme_materials(world);
            assert_labels_stay_below_avatars_without_redundant_updates(world, scene);
            let caption = world
                .query_filtered::<&Text, With<Caption>>()
                .single(world)
                .unwrap();
            assert!(
                caption
                    .0
                    .contains(world.resource::<Playback>().0.theme().name.title())
            );
            assert_fresh_file_uses_theme(world);
            for &(entity, transform) in &transforms {
                world.entity_mut(entity).insert(transform);
            }
        }
    }

    fn assert_fresh_file_uses_theme(world: &mut World) {
        let (id, old) = world
            .resource::<Pools>()
            .files
            .iter()
            .next()
            .map(|(id, entity)| (*id, *entity))
            .unwrap();
        world.resource_mut::<Pools>().files.remove(&id);
        animate(world);
        let fresh = world.resource::<Pools>().files[&id];
        assert_ne!(fresh, old);
        let visual = world.get::<FileVisual>(fresh).unwrap();
        let material = world
            .get::<MeshMaterial3d<StandardMaterial>>(visual.glyph)
            .unwrap();
        let theme = world.resource::<Playback>().0.theme();
        assert_eq!(
            world
                .resource::<Assets<StandardMaterial>>()
                .get(&material.0)
                .unwrap()
                .base_color,
            world
                .resource::<Assets<StandardMaterial>>()
                .get(
                    world.resource::<Palette>().heat_glyph(
                        visual.glyph_kind,
                        world
                            .resource::<Playback>()
                            .0
                            .layout()
                            .file(id)
                            .unwrap()
                            .heat
                    )
                )
                .unwrap()
                .base_color
        );
        assert_eq!(
            world.get::<Transform>(visual.glyph),
            Some(&theme.file_glyph_transform)
        );
        let palette = world.resource::<Palette>();
        let textures = &palette
            .glyph_textures
            .iter()
            .find(|(name, _)| *name == theme.name)
            .unwrap()
            .1;
        assert_eq!(
            world
                .resource::<Assets<StandardMaterial>>()
                .get(&material.0)
                .unwrap()
                .base_color_texture
                .as_ref(),
            Some(&textures[visual.glyph_kind])
        );
        let halo = world
            .get::<MeshMaterial3d<StandardMaterial>>(visual.halo)
            .unwrap();
        assert_eq!(
            world
                .resource::<Assets<StandardMaterial>>()
                .get(&halo.0)
                .unwrap()
                .base_color,
            theme.glass[visual.author.unwrap_or(0) % theme.glass.len()].base_color
        );
        world.despawn(fresh);
        world.resource_mut::<Pools>().files.insert(id, old);
    }

    #[test]
    fn punctuation_selects_the_expected_direction() {
        for (key, expected) in [
            (KeyCode::Comma, crate::theme::ThemeDirection::Previous),
            (KeyCode::Period, crate::theme::ThemeDirection::Next),
        ] {
            let mut keys = ButtonInput::default();
            keys.press(key);
            assert_eq!(preview_direction(&keys), Some(expected));
        }
    }

    fn assert_theme_materials(world: &World) {
        let palette = world.resource::<Palette>();
        let theme = world.resource::<Playback>().0.theme();
        let materials = world.resource::<Assets<StandardMaterial>>();
        assert_eq!(world.resource::<ClearColor>().0, theme.clear);
        for (handle, expected) in [
            (&palette.edge, &theme.edge),
            (&palette.floor, &theme.floor),
            (&palette.grid, &theme.grid),
            (&palette.descending, &theme.descending),
            (&palette.rising, &theme.rising),
        ]
        .into_iter()
        .chain(palette.avatars.iter().zip(&theme.avatars))
        .chain(palette.glass.iter().zip(&theme.glass))
        .chain(
            palette
                .directories
                .iter()
                .flatten()
                .zip(theme.directories.iter().flatten()),
        ) {
            assert_eq!(
                materials.get(handle).unwrap().base_color,
                expected.base_color
            );
        }
    }

    fn assert_theme_textures(world: &World) {
        let palette = world.resource::<Palette>();
        let textures = &palette
            .glyph_textures
            .iter()
            .find(|(name, _)| *name == world.resource::<Playback>().0.theme().name)
            .unwrap()
            .1;
        for (handles, texture) in palette.heat_glyphs.iter().zip(textures) {
            for (bucket, handle) in (1_u16..32).zip(handles) {
                let material = world
                    .resource::<Assets<StandardMaterial>>()
                    .get(handle)
                    .unwrap();
                assert_eq!(
                    material.base_color,
                    world
                        .resource::<Playback>()
                        .0
                        .theme()
                        .heat_color(f32::from(bucket) / 31.0)
                );
                assert_eq!(material.base_color_texture.as_ref(), Some(texture));
            }
        }
        for (material, texture) in palette.glyphs.iter().zip(textures) {
            assert_eq!(
                world
                    .resource::<Assets<StandardMaterial>>()
                    .get(material)
                    .unwrap()
                    .base_color_texture
                    .as_ref(),
                Some(texture)
            );
        }
    }

    fn assert_labels_stay_below_avatars_without_redundant_updates(
        world: &mut World,
        scene: Entity,
    ) {
        let portrait_label = world
            .resource::<Playback>()
            .0
            .labels()
            .into_iter()
            .find(|label| label.text == "Portrait")
            .unwrap();
        *world.get_mut::<GlobalTransform>(scene).unwrap() =
            GlobalTransform::from_translation(portrait_label.position - Vec3::Z * 50.0);
        world.get_mut::<Camera>(scene).unwrap().computed =
            bevy::camera::ComputedCameraValues {
                clip_from_view: Mat4::from_scale(Vec3::splat(0.01)),
                target_info: Some(bevy::camera::RenderTargetInfo {
                    physical_size: UVec2::new(WIDTH, HEIGHT),
                    scale_factor: 1.0,
                }),
                ..default()
            };
        labels(world);
        let expected_name = if world.resource::<Playback>().0.theme().label_style
            == crate::theme::LabelStyle::SmallCaps
        {
            "PORTRAIT"
        } else {
            "Portrait"
        };
        let mut label_query = world
            .query_filtered::<(&Text, &Visibility, &UiTargetCamera), With<Label>>();
        assert!(label_query.iter(world).any(|(text, visibility, _)| {
            text.0 == expected_name && *visibility == Visibility::Inherited
        }));
        for (_, _, target) in label_query.iter(world) {
            assert_eq!(target.0, scene);
        }
        world.clear_trackers();
        labels(world);
        let mut changed_labels = world.query_filtered::<Entity, (
            With<Label>,
            Or<(
                Changed<Text>,
                Changed<Node>,
                Changed<Visibility>,
                Changed<TextColor>,
                Changed<ZIndex>,
                Changed<UiTargetCamera>,
            )>,
        )>();
        assert_eq!(changed_labels.iter(world).count(), 0);
    }

    #[test]
    fn preview_keys_change_rate_without_seeking_and_loop_at_the_new_rate() {
        let simulation = Simulation::new(
            crate::timeline::synthetic(),
            &model::Options::default(),
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        let mut app = App::new();
        app.insert_resource(Playback(simulation))
            .insert_resource(Frame { index: 0, fps: 60 })
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<Time<Real>>()
            .add_systems(Update, preview_clock);
        let world = app.world_mut();
        let mut time = world.resource_mut::<Time<Real>>();
        time.update_with_duration(std::time::Duration::ZERO);
        time.update_with_duration(std::time::Duration::from_millis(750));
        app.update();
        assert_eq!(app.world().resource::<Frame>().index, 45);
        app.world_mut()
            .resource_mut::<Playback>()
            .0
            .advance_to(45, 60);
        let seconds = app.world().resource::<Playback>().0.seconds();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::BracketRight);
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .update_with_duration(std::time::Duration::ZERO);
        app.update();
        assert_eq!(app.world().resource::<Frame>().index, 45);
        assert_eq!(app.world().resource::<Playback>().0.seconds(), seconds);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset_all();
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .update_with_duration(std::time::Duration::from_millis(100));
        app.update();
        assert_eq!(app.world().resource::<Frame>().index, 57);
        assert_eq!(app.world().resource::<Playback>().0.seconds(), seconds);
        assert!(
            app.world()
                .resource::<Playback>()
                .0
                .caption()
                .contains("2x")
        );
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset_all();
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .update_with_duration(std::time::Duration::from_secs(30));
        app.update();
        assert_eq!(app.world().resource::<Frame>().index, 87);
        assert_eq!(app.world().resource::<Playback>().0.commit(), 0);
        assert_eq!(
            app.world().resource::<Playback>().0.scheduled_starts(),
            model::event_starts(&crate::timeline::synthetic(), 0.5, 1.0)
        );
        assert!(
            app.world()
                .resource::<Playback>()
                .0
                .caption()
                .contains("2x")
        );
    }

    fn projected_label(
        text: &str,
        kind: model::LabelKind,
        point: Vec2,
        distance: f32,
    ) -> (model::Label, Vec2, f32) {
        (
            model::Label {
                text: text.to_owned(),
                kind,
                position: Vec3::ZERO,
            },
            point,
            distance,
        )
    }

    #[test]
    fn overlapping_avatar_names_survive_in_far_to_near_order() {
        let point = Vec2::splat(100.0);
        let chosen = place_labels(
            vec![
                projected_label("near", model::LabelKind::Avatar, point, 4.0),
                projected_label("far", model::LabelKind::Avatar, point, 25.0),
            ],
            20.0,
        );
        assert_eq!(
            chosen
                .iter()
                .map(|(label, ..)| label.text.as_str())
                .collect::<Vec<_>>(),
            ["far", "near"]
        );
    }

    #[test]
    fn directory_and_activity_labels_still_declutter_around_names() {
        let point = Vec2::splat(100.0);
        let separate = Vec2::splat(300.0);
        let chosen = place_labels(
            vec![
                projected_label(
                    "blocked directory",
                    model::LabelKind::Landmark,
                    point,
                    1.0,
                ),
                projected_label(
                    "blocked activity",
                    model::LabelKind::Activity,
                    point,
                    1.0,
                ),
                projected_label("name", model::LabelKind::Avatar, point, 25.0),
                projected_label("directory", model::LabelKind::Landmark, separate, 1.0),
                projected_label("activity", model::LabelKind::Activity, separate, 1.0),
                projected_label(
                    "visible activity",
                    model::LabelKind::Activity,
                    Vec2::splat(500.0),
                    1.0,
                ),
            ],
            20.0,
        );
        assert_eq!(
            chosen
                .iter()
                .map(|(label, ..)| label.text.as_str())
                .collect::<Vec<_>>(),
            ["directory", "visible activity", "name"]
        );
    }

    #[test]
    fn label_bounds_capacity_and_equal_distance_order_stay_stable() {
        let mut wanted = vec![projected_label(
            "outside",
            model::LabelKind::Avatar,
            Vec2::splat(-1.0),
            1.0,
        )];
        wanted.extend((0..=LABELS).map(|index| {
            projected_label(
                &index.to_string(),
                model::LabelKind::Avatar,
                Vec2::splat(100.0),
                1.0,
            )
        }));
        let chosen = place_labels(wanted, 20.0);
        assert_eq!(chosen.len(), LABELS);
        assert_eq!(chosen[0].0.text, "0");
        assert_eq!(chosen[LABELS - 1].0.text, (LABELS - 1).to_string());
    }

    #[test]
    fn svg_glyphs_render_into_complete_colour_mip_chains() {
        for glyph in Theme::neon().glyphs {
            let image = glyph_image(glyph.svg, Theme::neon().glyph_texture);
            assert_eq!((image.width(), image.height()), (256, 256));
            assert_eq!(image.texture_descriptor.mip_level_count, 9);
            assert_eq!(image.data.as_ref().expect("glyph pixels").len(), 349_524);
            assert_eq!(
                image.texture_descriptor.format,
                TextureFormat::Rgba8UnormSrgb
            );
        }
        let theme = Theme::neon();
        let rust = glyph_image(
            theme.glyphs[theme.glyph_kind("main.rs")].svg,
            theme.glyph_texture,
        );
        assert!(
            rust.data
                .expect("Rust pixels")
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[0] > pixel[1]
                    && pixel[1] > pixel[2]
                    && pixel[3] == 255)
        );
    }

    #[test]
    fn svg_texture_settings_control_resolution_mips_and_straight_alpha() {
        let mut settings = Theme::neon().glyph_texture;
        settings.size = 8;
        settings.mipmaps = false;
        settings.padding = 0.0;
        let image = glyph_image(
            r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 16 16">
            <path d="M0 0h24v24H0z"/>
            <path d="M1 1h14v14H1z" fill="#ff0000" opacity="0.5"/></svg>"##,
            settings,
        );
        assert_eq!((image.width(), image.height()), (8, 8));
        assert_eq!(image.texture_descriptor.mip_level_count, 1);
        let pixels = image.data.expect("SVG pixels");
        assert_eq!(pixels.len(), 8 * 8 * 4);
        assert_eq!(&pixels[..4], &[255, 0, 0, 128]);
    }

    #[test]
    fn theme_can_choose_different_art_tint_and_tiles() {
        let mut theme = Theme::neon();
        theme.glyphs = &theme.glyphs[..1];
        theme.glyph_material.base_color = Color::BLACK;
        theme.file_tile = Some(crate::theme::FileTile {
            material: StandardMaterial {
                base_color: Color::WHITE,
                unlit: true,
                ..default()
            },
            transform: Transform::from_xyz(0.0, 0.05, 0.0)
                .with_scale(Vec3::new(0.46, 0.06, 0.58)),
        });
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        let palette = Palette::new(&mut world, &theme);
        let entity =
            spawn_file(&mut world, &palette, &theme, theme.glyph_kind("main.rs"));
        let children = world.get::<Children>(entity).expect("file children");
        assert_eq!(children.len(), 4);
        let tile = *children.last().expect("tile child");
        assert_eq!(
            world.get::<Transform>(tile),
            Some(&theme.file_tile.as_ref().expect("tile style").transform)
        );
        let material = world
            .resource::<Assets<StandardMaterial>>()
            .get(&palette.glyphs[0])
            .expect("palette glyph material remains allocated");
        assert_eq!(material.base_color, Color::BLACK);
        assert_eq!(palette.glyphs.len(), 1);
    }

    #[test]
    fn subtree_slots_include_retired_directories_and_survive_reappearance() {
        let mut layout = Layout::planned_paths(
            &[
                "early/deep/a.rs",
                "later/a.rs",
                "later/deeper/b.rs",
                "new/a.rs",
            ],
            0,
        );
        layout.add("early/deep/a.rs", 0.0, 0);
        layout.settle(120, 1.0 / 60.0);
        layout.remove("early/deep/a.rs", 0.0, 0);
        layout.settle(120, 1.0 / 60.0);
        layout.add("later/a.rs", 0.0, 0);
        let mut hues = DirectoryHues::default();
        hues.sync(&layout);
        assert_eq!(hues.slots, [0, 0, 0, 1, 1, 2]);
        assert_eq!(layout.dirs().count(), 2);

        layout.add("early/deep/b.rs", 0.0, 0);
        layout.add("later/deeper/b.rs", 0.0, 0);
        layout.add("new/a.rs", 0.0, 0);
        hues.sync(&layout);
        assert_eq!(hues.slots, [0, 0, 0, 1, 1, 2]);

        let mut rewind = Layout::planned_paths(
            &[
                "early/deep/a.rs",
                "later/a.rs",
                "later/deeper/b.rs",
                "new/a.rs",
            ],
            0,
        );
        rewind.add("early/deep/a.rs", 0.0, 0);
        hues.sync(&rewind);
        rewind.add("later/a.rs", 0.0, 0);
        hues.sync(&rewind);
        assert_eq!(&hues.slots[..rewind.dir_count()], &[0, 0, 0, 1, 1, 2]);
    }

    #[test]
    fn structure_reuses_materials_across_depth_growth_pool_reuse_and_rewind() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        let theme = Theme::neon();
        let palette = Palette::new(&mut world, &theme);
        let materials = world.resource::<Assets<StandardMaterial>>().len();
        let mut pools = Pools::default();
        let mut simulation = Simulation::new(
            crate::timeline::synthetic(),
            &model::Options::default(),
            &theme,
            &crate::contributors::Mappings::default(),
        );
        for frame in [0, 18, 120, 240, 0, 240] {
            simulation.advance_to(frame, 30);
            sync_files(&mut world, &palette, &mut pools, &simulation);
            sync_structure(&mut world, &palette, &mut pools, &simulation);
            assert_eq!(
                world.resource::<Assets<StandardMaterial>>().len(),
                materials
            );
            for (&entity, (index, dir)) in
                pools.directories.iter().zip(simulation.layout().dirs())
            {
                assert_eq!(
                    &world
                        .get::<MeshMaterial3d<StandardMaterial>>(entity)
                        .expect("pooled nodes have materials")
                        .0,
                    palette.directory(pools.hues.slots[index], dir.depth),
                );
                assert_ne!(world.get::<Visibility>(entity), Some(&Visibility::Hidden));
            }
            for (&entity, (index, dir)) in pools.edges.iter().zip(
                simulation
                    .layout()
                    .dirs()
                    .filter(|(_, dir)| dir.parent.is_some()),
            ) {
                assert_eq!(
                    &world
                        .get::<MeshMaterial3d<StandardMaterial>>(entity)
                        .expect("pooled edges have materials")
                        .0,
                    palette.directory(pools.hues.slots[index], dir.depth),
                );
            }
            for &entity in &pools.directories[simulation.layout().dirs().count()..] {
                assert_eq!(world.get::<Visibility>(entity), Some(&Visibility::Hidden));
            }
        }
        assert_eq!(palette.directory(0, 0), &palette.edge);
        assert_eq!(palette.directory(0, u16::MAX), palette.directory(0, 4));
        assert_eq!(
            palette.directory(0, 1),
            palette.directory(theme.directories.len(), 1)
        );
    }

    #[test]
    fn renamed_files_switch_icons_without_replacing_the_file() {
        use crate::timeline::{Event, FileChange, Kind};

        let paths = ["file.rs", "file.py", "unknown.xyz", "Dockerfile"];
        let events: Vec<_> = paths
            .iter()
            .enumerate()
            .map(|(index, path)| Event {
                author: crate::timeline::Identity {
                    name: b"Ada".to_vec(),
                    email: b"ada@example.invalid".to_vec(),
                },
                contributor: None,
                commit: String::new(),
                seconds: 86_400 * i64::try_from(index).expect("small fixture"),
                changes: vec![FileChange {
                    path: (*path).to_owned(),
                    kind: if index == 0 {
                        Kind::Added
                    } else {
                        Kind::Moved {
                            from: paths[index - 1].to_owned(),
                        }
                    },
                    insertions: 0,
                    removals: 0,
                    samples: Vec::new(),
                }],
            })
            .collect();
        let mut simulation = Simulation::new(
            events,
            &model::Options::default(),
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        let palette = Palette::new(&mut world, &Theme::neon());
        let material_count = world.resource::<Assets<StandardMaterial>>().len();
        let image_count = world.resource::<Assets<Image>>().len();
        let mut pools = Pools::default();
        let mut original = None;
        for (frame, name) in [
            (18, paths[0]),
            (33, paths[1]),
            (48, paths[2]),
            (63, paths[3]),
            (18, paths[0]),
        ] {
            simulation.advance_to(frame, 30);
            sync_files(&mut world, &palette, &mut pools, &simulation);
            let entity = pools.files[&0];
            assert_eq!(*original.get_or_insert(entity), entity);
            let visual = world.get::<FileVisual>(entity).expect("file visual");
            let kind = simulation.theme().glyph_kind(name);
            assert_eq!(visual.glyph_kind, kind);
            assert_eq!(
                world
                    .get::<MeshMaterial3d<StandardMaterial>>(visual.glyph)
                    .expect("palette glyph material remains allocated")
                    .0,
                visual.heat_material
            );
            assert_eq!(
                world.get::<Transform>(visual.glyph),
                Some(&simulation.theme().file_glyph_transform)
            );
            assert_eq!(
                world.get::<Children>(entity).expect("file children").len(),
                3
            );
            assert_eq!(
                world.resource::<Assets<StandardMaterial>>().len(),
                material_count
            );
            assert_eq!(world.resource::<Assets<Image>>().len(), image_count);
        }
    }

    #[test]
    fn collapsed_files_restore_visibility_after_zoom_and_theme_switch() {
        let events = crate::timeline::synthetic();
        let simulation = Simulation::new(
            events.clone(),
            &model::Options {
                start: events.len(),
                ..default()
            },
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        let palette = Palette::new(&mut world, &Theme::neon());
        let mut pools = Pools::default();
        world.insert_resource(Time::<Real>::default());
        sync_files(&mut world, &palette, &mut pools, &simulation);
        let (focus, distance) = simulation.view();
        let mut fallback = lod::Lod::default();
        fallback.sample(
            simulation.layout(),
            simulation.seconds(),
            lod::View {
                eye: focus + model::view_direction() * distance,
                focus,
                height: 720.0,
                fov: std::f32::consts::FRAC_PI_4,
            },
        );
        assert_eq!(pools.lod.detail, fallback.detail);
        assert_eq!(pools.lod.glyph, fallback.glyph);
        assert_eq!(pools.lod.size, fallback.size);
        world.insert_resource(PreviewCamera::new((Vec3::ZERO, 1.0)));
        world
            .resource_mut::<Time<Real>>()
            .advance_by(std::time::Duration::from_secs(1));
        sync_files(&mut world, &palette, &mut pools, &simulation);
        assert_eq!(pools.files.len(), simulation.layout().files().count());
        world.insert_resource(PreviewCamera::new((Vec3::ZERO, 20000.0)));
        world
            .resource_mut::<Time<Real>>()
            .advance_by(std::time::Duration::from_secs(1));
        sync_files(&mut world, &palette, &mut pools, &simulation);
        assert!(pools.lod.glyph.iter().any(|&weight| weight > 0.0));
        let hidden: Vec<_> = pools
            .files
            .values()
            .copied()
            .filter(|&entity| {
                world.get::<Visibility>(entity) == Some(&Visibility::Hidden)
            })
            .collect();
        assert_ne!(hidden, []);
        world.insert_resource(palette);
        world.insert_resource(Playback(simulation));
        switch_theme(&mut world, crate::theme::ThemeName::Phosphor);
        let palette = world.remove_resource::<Palette>().unwrap();
        let simulation = world.remove_resource::<Playback>().unwrap().0;
        world.insert_resource(PreviewCamera::new((Vec3::ZERO, 1.0)));
        world
            .resource_mut::<Time<Real>>()
            .advance_by(std::time::Duration::from_secs(1));
        sync_files(&mut world, &palette, &mut pools, &simulation);
        assert_eq!(pools.files.len(), simulation.layout().files().count());
        assert!(
            hidden.iter().all(|&entity| world.get::<Visibility>(entity)
                == Some(&Visibility::Inherited))
        );
        for &entity in &hidden {
            let visual = world.get::<FileVisual>(entity).unwrap();
            let material = world
                .resource::<Assets<StandardMaterial>>()
                .get(&visual.heat_material)
                .unwrap();
            let textures = &palette
                .glyph_textures
                .iter()
                .find(|(name, _)| *name == crate::theme::ThemeName::Phosphor)
                .unwrap()
                .1;
            assert_eq!(
                material.base_color_texture.as_ref(),
                Some(&textures[visual.glyph_kind])
            );
            assert_eq!(
                world.get::<Transform>(visual.glyph),
                Some(&simulation.theme().file_glyph_transform)
            );
        }
        sync_structure(&mut world, &palette, &mut pools, &simulation);
        assert!(pools.aggregates.iter().all(
            |&entity| world.get::<Visibility>(entity) == Some(&Visibility::Hidden)
        ));
    }

    #[test]
    fn membership_rings_follow_subtree_moves_and_keep_file_icons() {
        use crate::timeline::{Event, FileChange, Kind};

        let events = vec![
            Event {
                author: crate::timeline::Identity {
                    name: b"Ada".to_vec(),
                    email: b"ada@example.invalid".to_vec(),
                },
                contributor: None,
                commit: String::new(),
                seconds: 0,
                changes: vec![FileChange {
                    path: "alpha/a.rs".to_owned(),
                    kind: Kind::Added,
                    insertions: 1,
                    removals: 0,
                    samples: Vec::new(),
                }],
            },
            Event {
                author: crate::timeline::Identity {
                    name: b"Ada".to_vec(),
                    email: b"ada@example.invalid".to_vec(),
                },
                contributor: None,
                commit: String::new(),
                seconds: 86_400,
                changes: vec![FileChange {
                    path: "beta/a.rs".to_owned(),
                    kind: Kind::Moved {
                        from: "alpha/a.rs".to_owned(),
                    },
                    insertions: 0,
                    removals: 0,
                    samples: Vec::new(),
                }],
            },
        ];
        let mut simulation = Simulation::new(
            events,
            &model::Options::default(),
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        let palette = Palette::new(&mut world, &Theme::neon());
        let materials = world.resource::<Assets<StandardMaterial>>().len();
        let mut pools = Pools::default();
        for (frame, slot) in [(18, 0), (45, 1), (18, 0), (45, 1)] {
            simulation.advance_to(frame, 30);
            sync_files(&mut world, &palette, &mut pools, &simulation);
            assert_eq!(pools.files.len(), 1);
            let entity = pools.files[&0];
            let visual = world.get::<FileVisual>(entity).expect("file has a visual");
            assert_eq!(
                &world
                    .get::<MeshMaterial3d<StandardMaterial>>(visual.membership)
                    .expect("membership ring material")
                    .0,
                palette.directory(slot, 1),
            );
            assert_eq!(
                world
                    .get::<MeshMaterial3d<StandardMaterial>>(visual.glyph)
                    .expect("file icon material")
                    .0,
                visual.heat_material
            );
            assert_eq!(
                world.resource::<Assets<StandardMaterial>>().len(),
                materials,
            );
        }
    }

    #[test]
    fn cold_scene_does_not_mutate_materials() {
        use bevy::asset::{AssetApp, AssetEvent, AssetPlugin};
        let mut events = crate::timeline::synthetic();
        events.truncate(1);
        events[0].changes = (0..3000)
            .map(|id| crate::timeline::FileChange {
                path: format!("files/f{id}.rs"),
                kind: crate::timeline::Kind::Added,
                insertions: 10,
                removals: 0,
                ..default()
            })
            .collect();
        let mut simulation = Simulation::new(
            events,
            &model::Options {
                heat_half_life: 0.01,
                ..default()
            },
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        simulation.advance_to(60, 60);
        assert_eq!(simulation.layout().files().count(), 3000);
        assert!(
            simulation
                .layout()
                .files()
                .all(|(_, file)| file.heat < 0.001)
        );
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<Image>()
            .init_asset::<StandardMaterial>();
        let palette = Palette::new(app.world_mut(), &Theme::neon());
        let mut pools = Pools::default();
        sync_files(app.world_mut(), &palette, &mut pools, &simulation);
        app.update();
        app.world_mut()
            .resource_mut::<Messages<AssetEvent<StandardMaterial>>>()
            .clear();
        let material_count = app.world().resource::<Assets<StandardMaterial>>().len();
        let mut mutations = 0;
        for _ in 0..60 {
            sync_files(app.world_mut(), &palette, &mut pools, &simulation);
            app.update();
            mutations += app
                .world_mut()
                .resource_mut::<Messages<AssetEvent<StandardMaterial>>>()
                .drain()
                .filter(|event| matches!(event, AssetEvent::Modified { .. }))
                .count();
        }
        println!(
            "3000 files, 60 unchanged cold frames: {mutations} material modifications; {material_count} allocated materials"
        );
        assert_eq!(mutations, 0);
    }
}
