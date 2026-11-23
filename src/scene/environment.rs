use bevy::{
    asset::RenderAssetUsages,
    camera::{Exposure, visibility::NoFrustumCulling},
    color::ColorToPacked,
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor},
    light::Skybox,
    math::Affine2,
    mesh::VertexAttributeValues,
    prelude::*,
    render::render_resource::{
        Extent3d, PrimitiveTopology, TextureDimension, TextureFormat,
        TextureViewDescriptor, TextureViewDimension,
    },
};

use super::{PreviewCamera, SceneCamera, glass_positions};
use crate::{
    model::{Simulation, framing::GLASS_BOTTOM},
    repositories::Boundary,
    theme::ThemeName,
};

const GROUND_Y: f32 = -0.22;
const TILE_FLOOR_Y: f32 = -0.18;
const REPEAT: f32 = 16.0;
const GROUND_SIZE: u32 = 256;
const SUN_AZIMUTH: Vec3 = Vec3::NEG_Z;

#[derive(Resource)]
struct Environment {
    plane: Entity,
    material: Handle<StandardMaterial>,
    tile_floor: Entity,
    tile_mesh: Handle<Mesh>,
    tile_material: Handle<StandardMaterial>,
    textures: Vec<(ThemeName, EnvironmentImages)>,
}

struct EnvironmentImages {
    ground: Handle<Image>,
    sky: Handle<Image>,
    tile_floor: Option<Handle<Image>>,
}

pub(super) struct Textures {
    ground: Image,
    sky: Image,
    tile_floor: Option<Image>,
}

impl Textures {
    pub(super) fn raster(name: ThemeName) -> Self {
        let (base, art) = name.ground();
        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256"><rect width="256" height="256" fill="#{base:06x}"/>{art}</svg>"##
        );
        let mut level = image::RgbaImage::from_raw(
            GROUND_SIZE,
            GROUND_SIZE,
            raster_svg(&svg, GROUND_SIZE),
        )
        .expect("ground RGBA pixels");
        let mut pixels = level.as_raw().clone();
        while level.width() > 1 {
            let size = level.width() / 2;
            level = image::imageops::resize(
                &level,
                size,
                size,
                image::imageops::FilterType::Triangle,
            );
            pixels.extend_from_slice(level.as_raw());
        }
        let mut ground = texture(GROUND_SIZE, 1);
        ground.texture_descriptor.mip_level_count = GROUND_SIZE.ilog2() + 1;
        ground.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            anisotropy_clamp: 8,
            ..ImageSamplerDescriptor::linear()
        });
        ground.data = Some(pixels);
        let far_ground = level.get_pixel(0, 0).0;
        Self {
            ground,
            sky: skybox(name, far_ground),
            tile_floor: name.tile_floor_art().map(|art| {
                let size = 1024;
                let svg = format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024">{art}</svg>"#
                );
                let mut image = texture(size, 1);
                image.sampler = ImageSampler::linear();
                image.data = Some(raster_svg(&svg, size));
                image
            }),
        }
    }
}

fn raster_svg(svg: &str, size: u32) -> Vec<u8> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())
        .expect("environment SVG is valid");
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)
        .expect("environment texture fits memory");
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.take()
}

fn texture(size: u32, layers: u32) -> Image {
    Image::new_uninit(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: layers,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    )
}

fn skybox(name: ThemeName, ground: [u8; 4]) -> Image {
    let size = 256_u16;
    let (horizon, zenith) = name.sky();
    let [r, g, b, a] = ground;
    let far_ground = Srgba::rgba_u8(r, g, b, a);
    let pixel = |direction: Vec3| {
        let height = direction.y;
        // The ground-coloured band keeps the cubemap filter off the plane edge.
        let blend = ((height - 0.006) / 0.019).clamp(0.0, 1.0);
        if blend <= 0.0 {
            return ground;
        }
        let sky = horizon.mix(&zenith, height.sqrt());
        let sky = if name == ThemeName::Outrun {
            sunset(direction, sky)
        } else {
            sky
        };
        far_ground.mix(&sky, blend).to_u8_array()
    };
    let coordinates =
        (0..size).map(|i| (f32::from(i) + 0.5) * 2.0 / f32::from(size) - 1.0);
    let mut pixels = Vec::with_capacity(6 * usize::from(size).pow(2) * 4);
    for face in 0..6 {
        for v in coordinates.clone() {
            for u in coordinates.clone() {
                // Bevy flips world Z before sampling the cubemap.
                let direction = match face {
                    0 => Vec3::new(1.0, -v, u),
                    1 => Vec3::new(-1.0, -v, -u),
                    2 => Vec3::new(u, 1.0, -v),
                    3 => Vec3::new(u, -1.0, v),
                    4 => Vec3::new(u, -v, -1.0),
                    _ => Vec3::new(-u, -v, 1.0),
                }
                .normalize();
                pixels.extend_from_slice(&pixel(direction));
            }
        }
    }
    let mut image = texture(u32::from(size), 6);
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..default()
    });
    image.sampler = ImageSampler::linear();
    image.data = Some(pixels);
    image
}

fn sunset(direction: Vec3, sky: Srgba) -> Srgba {
    let centre = (SUN_AZIMUTH + Vec3::Y * 0.12).normalize();
    let rim = ((0.15 - direction.distance(centre)) / 0.009).clamp(0.0, 1.0);
    let height = direction.y;
    let band = ((height / 0.025).fract() * 8.0 - 1.0).clamp(0.0, 1.0);
    let stripes = band.max(((height - 0.10) / 0.012).clamp(0.0, 1.0));
    let pink = Srgba::new(1.0, 88.0 / 255.0, 158.0 / 255.0, 1.0);
    let gold = Srgba::new(1.0, 211.0 / 255.0, 107.0 / 255.0, 1.0);
    let disc = pink.mix(&gold, (height / 0.27).min(1.0));
    sky.mix(&disc, rim * stripes)
}

pub(super) fn setup(world: &mut World, name: ThemeName, mesh: Handle<Mesh>) {
    let material =
        world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                unlit: true,
                ..default()
            });
    let plane = world
        .spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material.clone()),
            NoFrustumCulling,
        ))
        .id();
    let tile_material =
        world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                unlit: true,
                ..default()
            });
    let tile_mesh = world.resource_mut::<Assets<Mesh>>().add(
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0; 2]; 3]),
    );
    let tile_floor = world
        .spawn((
            Mesh3d(tile_mesh.clone()),
            MeshMaterial3d(tile_material.clone()),
            Transform::IDENTITY,
            Visibility::Hidden,
            NoFrustumCulling,
        ))
        .id();
    world.insert_resource(Environment {
        plane,
        material,
        tile_floor,
        tile_mesh,
        tile_material,
        textures: Vec::new(),
    });
    apply(world, name);
    place_ground(world);
}

pub(super) fn install(world: &mut World, name: ThemeName, textures: Textures) {
    let mut images = world.resource_mut::<Assets<Image>>();
    let handles = EnvironmentImages {
        ground: images.add(textures.ground),
        sky: images.add(textures.sky),
        tile_floor: textures.tile_floor.map(|image| images.add(image)),
    };
    world
        .resource_mut::<Environment>()
        .textures
        .push((name, handles));
}

pub(super) fn apply(world: &mut World, name: ThemeName) {
    let Some(environment) = world.get_resource::<Environment>() else {
        return;
    };
    if !environment
        .textures
        .iter()
        .any(|(loaded, ..)| *loaded == name)
    {
        install(world, name, Textures::raster(name));
    }
    let environment = world.resource::<Environment>();
    let material = environment.material.clone();
    let tile_material = environment.tile_material.clone();
    let (_, images) = environment
        .textures
        .iter()
        .find(|(loaded, ..)| *loaded == name)
        .expect("theme environment is loaded");
    let (ground, sky, tile_floor) = (
        images.ground.clone(),
        images.sky.clone(),
        images.tile_floor.clone(),
    );
    let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
    materials
        .get_mut(&material)
        .expect("ground material")
        .base_color_texture = Some(ground);
    *materials
        .get_mut(&tile_material)
        .expect("tile floor material") = StandardMaterial {
        base_color: match name {
            ThemeName::Neon => Color::srgb(0.02, 0.036, 0.06),
            ThemeName::Phosphor => Color::BLACK,
            _ => Color::WHITE,
        },
        base_color_texture: tile_floor,
        unlit: true,
        ..default()
    };
    let mut camera = world.query_filtered::<Entity, With<SceneCamera>>();
    if let Ok(entity) = camera.single(world) {
        world.entity_mut(entity).insert(Skybox {
            // The skybox applies exposure, unlike the unlit ground texture.
            brightness: Exposure::default().exposure().recip(),
            image: Some(sky),
            ..default()
        });
    }
}

pub(super) fn place_tile_floor(world: &mut World, simulation: &Simulation) {
    let Some(environment) = world.get_resource::<Environment>() else {
        return;
    };
    let entity = environment.tile_floor;
    let mesh = environment.tile_mesh.clone();
    let mut camera =
        world.query_filtered::<(&Transform, &Projection), With<SceneCamera>>();
    let Ok((pose, Projection::Perspective(projection))) = camera.single(world) else {
        return;
    };
    let eye = pose.translation;
    if !matches!(simulation.repository_boundary, Boundary::Glass)
        || eye.y <= GLASS_BOTTOM
    {
        super::set_visible(world, entity, false);
        return;
    }
    let (focus, distance) = simulation.view();
    let (centre, width) = world
        .get_resource::<PreviewCamera>()
        .map_or((focus.xz(), distance * 2.5), |camera| {
            (camera.pose().0.xz(), projection.far * 4.0)
        });
    let positions: Vec<_> = (0..simulation.layout().repositories.len())
        .filter_map(|index| simulation.visible_repository_tile(index))
        .flat_map(|tile| glass_positions(Some(tile)))
        .map(|[x, _, z]| {
            // Project the finish onto the floor along sightlines through the glass edges.
            let scale = (eye.y - TILE_FLOOR_Y) / (eye.y - GLASS_BOTTOM);
            let point = eye.xz() + (Vec2::new(x, z) - eye.xz()) * scale;
            [point.x, TILE_FLOOR_Y, point.y]
        })
        .collect();
    super::set_visible(world, entity, !positions.is_empty());
    if positions.is_empty() {
        return;
    }
    let uvs: Vec<_> = positions
        .iter()
        .map(|&[x, _, z]| {
            ((Vec2::new(x, z) - centre) / width + Vec2::splat(0.5)).to_array()
        })
        .collect();
    let meshes = world.resource::<Assets<Mesh>>();
    let current = meshes.get(&mesh).expect("tile floor mesh");
    if matches!(
        (current.attribute(Mesh::ATTRIBUTE_POSITION), current.attribute(Mesh::ATTRIBUTE_UV_0)),
        (Some(VertexAttributeValues::Float32x3(old_positions)), Some(VertexAttributeValues::Float32x2(old_uvs)))
            if *old_positions == positions && *old_uvs == uvs
    ) {
        return;
    }
    let mut meshes = world.resource_mut::<Assets<Mesh>>();
    let mut mesh = meshes.get_mut(&mesh).expect("tile floor mesh");
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0.0, 1.0, 0.0]; positions.len()],
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
}

pub(super) fn place_ground(world: &mut World) {
    let Some(environment) = world.get_resource::<Environment>() else {
        return;
    };
    let (plane, material) = (environment.plane, environment.material.clone());
    let mut camera =
        world.query_filtered::<(&Transform, &Projection), With<SceneCamera>>();
    let Ok((pose, Projection::Perspective(projection))) = camera.single(world) else {
        return;
    };
    let eye = pose.translation;
    // Whole texture repeats keep the grid fixed as the plane follows the camera.
    let centre = (eye.xz() / REPEAT).round() * REPEAT;
    let repeats = (projection.far.max((eye.y - GROUND_Y).abs() * 64.0) * 4.0 / REPEAT)
        .log2()
        .ceil()
        .exp2();
    let transform = Transform::from_xyz(centre.x, GROUND_Y, centre.y)
        .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
        .with_scale(Vec3::new(repeats * REPEAT, repeats * REPEAT, 1.0));
    world
        .get_mut::<Transform>(plane)
        .expect("ground plane")
        .set_if_neq(transform);
    let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
    let uv = Affine2::from_scale(Vec2::splat(repeats));
    if materials
        .get(&material)
        .expect("ground material")
        .uv_transform
        != uv
    {
        materials
            .get_mut(&material)
            .expect("ground material")
            .uv_transform = uv;
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::{FRAC_PI_2, PI};

    use bevy::{camera::PerspectiveProjection, color::Luminance};

    use super::*;

    #[test]
    fn horizon_differs_from_opaque_ground_by_at_least_025_linear_luminance() {
        for name in ThemeName::ALL {
            let textures = Textures::raster(name);
            let bytes = textures.ground.data.as_deref().unwrap().as_chunks::<4>().0;
            assert!(bytes.iter().all(|pixel| pixel[3] == 255), "{name:?}");
            assert!(bytes.iter().any(|pixel| pixel != &bytes[0]), "{name:?}");
            let &[r, g, b, _] = bytes.last().unwrap();
            let ground = Color::srgb_u8(r, g, b).to_linear().luminance();
            let horizon = Color::from(name.sky().0).to_linear().luminance();
            assert!((ground - horizon).abs() >= 0.025, "{name:?}");
        }
    }

    #[test]
    fn cubemap_texels_at_the_horizon_match_far_ground() {
        for name in ThemeName::ALL {
            let textures = Textures::raster(name);
            let ground = textures.ground.data.as_deref().unwrap().as_chunks::<4>().0;
            let sky = textures.sky.data.as_deref().unwrap().as_chunks::<4>().0;
            let size = usize::try_from(textures.sky.width()).unwrap();
            for face in [0, 1, 4, 5] {
                let middle = face * size * size + size * (size / 2);
                assert!(
                    sky[middle - size..middle + size]
                        .iter()
                        .all(|pixel| Some(pixel) == ground.last()),
                    "{name:?}, face {face}",
                );
            }
        }
    }

    #[test]
    fn ground_stays_below_glass_and_sunset_stays_world_aligned_as_camera_turns() {
        let mut world = World::new();
        world.init_resource::<Assets<StandardMaterial>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<Mesh>>();
        let camera = world
            .spawn((
                SceneCamera,
                Projection::Perspective(PerspectiveProjection::default()),
                Transform::default(),
            ))
            .id();
        setup(&mut world, ThemeName::Outrun, Handle::default());
        let sky = world.get::<Skybox>(camera).unwrap().image.clone().unwrap();
        assert_eq!(skybox_pixel(&world, &sky, 4)[0], 255);
        assert!(skybox_pixel(&world, &sky, 0)[0] < 128);
        assert!(skybox_pixel(&world, &sky, 5)[0] < 128);

        let plane = world.resource::<Environment>().plane;
        let material = world.resource::<Environment>().material.clone();
        for yaw in [0.0, FRAC_PI_2, PI] {
            *world.get_mut::<Transform>(camera).unwrap() =
                Transform::from_xyz(203.0, 6.0, -104.0)
                    .with_rotation(Quat::from_rotation_y(yaw));
            place_ground(&mut world);
            let transform = world.get::<Transform>(plane).unwrap();
            assert!(transform.translation.y < crate::model::framing::GLASS_BOTTOM);
            assert!(transform.local_z().dot(Vec3::Y) > 0.999);
            assert!(transform.scale.x >= 4000.0);
            let skybox = world.get::<Skybox>(camera).unwrap();
            assert_eq!(skybox.rotation, Quat::IDENTITY);
            assert_eq!(skybox.image.as_ref(), Some(&sky));
        }
        let materials = world.resource::<Assets<StandardMaterial>>();
        assert_eq!(
            materials.get(&material).unwrap().alpha_mode,
            AlphaMode::Opaque
        );
    }

    fn skybox_pixel(world: &World, sky: &Handle<Image>, face: usize) -> [u8; 4] {
        let image = world.resource::<Assets<Image>>().get(sky).unwrap();
        let size = usize::try_from(image.width()).unwrap();
        let row_above_horizon = 110;
        let texel = face * size * size + row_above_horizon * size + size / 2;
        image.data.as_deref().unwrap().as_chunks::<4>().0[texel]
    }
}
