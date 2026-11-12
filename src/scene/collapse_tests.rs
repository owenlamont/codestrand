use super::*;
use bevy::mesh::VertexAttributeValues;
use clap::Parser;

#[test]
fn collapsing_option_defaults_on_and_accepts_only_on_or_off() {
    for command in [vec!["preview"], vec!["export", "unused"]] {
        for (flags, expected) in [
            (vec![], true),
            (vec!["--collapse-directories", "on"], true),
            (vec!["--collapse-directories", "off"], false),
        ] {
            let mut args = vec!["codestrand"];
            args.extend(command.clone());
            args.extend(flags);
            let cli = crate::Cli::try_parse_from(args).unwrap();
            let (crate::Command::Preview { source, .. }
            | crate::Command::Export { source, .. }) = cli.command.unwrap()
            else {
                panic!("playback command")
            };
            assert_eq!(
                serde_json::to_value(source).unwrap()["collapse_directories"],
                expected
            );
        }
        let mut args = vec!["codestrand"];
        args.extend(command);
        args.extend(["--collapse-directories", "sometimes"]);
        assert!(crate::Cli::try_parse_from(args).is_err());
    }
}

fn bounds(mesh: &Mesh, transform: Transform) -> (Vec3, Vec3) {
    let VertexAttributeValues::Float32x3(vertices) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
    else {
        panic!("mesh positions")
    };
    vertices
        .iter()
        .map(|&p| transform.transform_point(Vec3::from(p)))
        .fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(low, high), p| (low.min(p), high.max(p)),
        )
}

#[test]
fn collapsed_folders_fit_files_clear_surfaces_and_never_overlap() {
    let events = crate::timeline::synthetic();
    for name in crate::theme::ThemeName::ALL {
        let theme = name.build();
        let sim = Simulation::new(
            events.clone(),
            &model::Options {
                start: events.len(),
                ..default()
            },
            &theme,
            &crate::contributors::Mappings::default(),
        );
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        let palette = Palette::new(&mut world, &theme);
        let mut pools = Pools::default();
        let mut nested_blend = false;
        for (seconds, distance) in [
            (0.0, 20000.0),
            (0.1, 1.0),
            (0.4, 1.0),
            (0.5, 20000.0),
            (0.6, 20000.0),
            (0.8, 20000.0),
        ] {
            pools.lod.sample(
                sim.layout(),
                seconds,
                lod::View {
                    eye: Vec3::Y * distance,
                    focus: Vec3::ZERO,
                    height: 720.0,
                    fov: std::f32::consts::FRAC_PI_4,
                },
            );
            sync_structure(&mut world, &palette, &mut pools, &sim);
            nested_blend |= pools.lod.glyphs.iter().any(|&index| {
                sim.layout()
                    .dir(index)
                    .parent
                    .is_some_and(|parent| pools.lod.glyph[parent] > 0.0)
            });
            let mesh = world
                .resource::<Assets<Mesh>>()
                .get(&palette.folder)
                .unwrap();
            let boxes: Vec<_> = pools
                .lod
                .glyphs
                .iter()
                .zip(&pools.aggregates)
                .map(|(&index, &entity)| {
                    let dir = sim.layout().dir(index);
                    let (low, high) =
                        bounds(mesh, *world.get::<Transform>(entity).unwrap());
                    let file = theme.glyph_transform().scale * dir.scale;
                    assert!((high.x - low.x) <= file.x + 0.0001, "{name:?}");
                    assert!((high.z - low.z) <= file.y + 0.0001, "{name:?}");
                    assert!(
                        low.y > theme.geometry().file_max.y * dir.scale,
                        "{name:?}"
                    );
                    (low, high)
                })
                .collect();
            for (i, &(low, high)) in boxes.iter().enumerate() {
                for &(other_low, other_high) in &boxes[i + 1..] {
                    assert!(
                        high.x <= other_low.x
                            || other_high.x <= low.x
                            || high.z <= other_low.z
                            || other_high.z <= low.z,
                        "{name:?} at {seconds}: {low:?} {high:?} / {other_low:?} {other_high:?}"
                    );
                }
            }
        }
        assert!(nested_blend, "parent/child blend exercised");
    }
}

#[test]
fn folder_top_has_no_overlapping_tab_faces() {
    let mesh = folder_mesh();
    let VertexAttributeValues::Float32x3(vertices) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
    else {
        panic!("folder positions")
    };
    let indices: Vec<_> = mesh.indices().unwrap().iter().collect();
    let mut area = 0.0;
    for triangle in indices.as_chunks::<3>().0 {
        let [a, b, c] =
            [triangle[0], triangle[1], triangle[2]].map(|i| Vec3::from(vertices[i]));
        if a.y > 0.0 && b.y > 0.0 && c.y > 0.0 {
            area += (b - a).cross(c - a).length() * 0.5;
        }
    }
    assert!((area - (1.4 * 0.9 + 0.55 * 0.2)).abs() < 0.00001);
}

#[test]
fn collapsing_off_restores_files_immediately_through_rewind_and_theme_switch() {
    for command in [vec!["preview"], vec!["export", "unused"]] {
        let mut args = vec!["codestrand"];
        args.extend(command);
        args.extend(["--collapse-directories", "off", "--start", "1000"]);
        let cli = crate::Cli::try_parse_from(args).unwrap();
        let (crate::Command::Preview { source, .. }
        | crate::Command::Export { source, .. }) = cli.command.unwrap()
        else {
            panic!("playback command")
        };
        let (mut sim, _) = source.load().unwrap();
        sim.advance_to(120, 30);
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        let palette = Palette::new(&mut world, sim.theme());
        let mut pools = Pools::default();
        world.insert_resource(PreviewCamera::new((Vec3::ZERO, 20000.0)));
        pools.lod.sample(
            sim.layout(),
            sim.seconds(),
            lod::View {
                eye: Vec3::Y * 20000.0,
                focus: Vec3::ZERO,
                height: 720.0,
                fov: std::f32::consts::FRAC_PI_4,
            },
        );
        assert_ne!(pools.lod.glyphs, [] as [usize; 0]);
        sync_structure(&mut world, &palette, &mut pools, &sim);
        let (_, file) = sim.layout().files().next().unwrap();
        let target = ground(file.position);
        let scale = sim.layout().dir(file.dir).scale;
        world.spawn((
            SceneCamera,
            Camera::default(),
            Transform::from_translation(target + Vec3::Y * scale * 2.0)
                .looking_at(target, Vec3::NEG_Z),
            Projection::Perspective(PerspectiveProjection {
                near: scale * 0.01,
                ..default()
            }),
        ));
        sync_files(&mut world, &palette, &mut pools, &sim);
        assert_eq!(pools.lod.glyphs, [] as [usize; 0]);
        assert_ne!(pools.names.targets, std::collections::BTreeSet::new());
        world.insert_resource(palette);
        world.insert_resource(Playback(sim));
        for name in [
            crate::theme::ThemeName::Neon,
            crate::theme::ThemeName::BevelledDesktop,
        ] {
            switch_theme(&mut world, name);
            let palette = world.remove_resource::<Palette>().unwrap();
            let mut sim = world.remove_resource::<Playback>().unwrap().0;
            sim.advance_to(0, 30);
            sync_files(&mut world, &palette, &mut pools, &sim);
            sync_structure(&mut world, &palette, &mut pools, &sim);
            assert_eq!(pools.lod.detail, vec![1.0; sim.layout().dir_count()]);
            assert_eq!(pools.lod.glyph, vec![0.0; sim.layout().dir_count()]);
            assert_eq!(pools.lod.glyphs, [] as [usize; 0]);
            assert_eq!(pools.files.len(), sim.layout().files().count());
            assert!(
                pools.files.values().all(
                    |&e| world.get::<Visibility>(e) == Some(&Visibility::Inherited)
                )
            );
            assert!(
                pools
                    .aggregates
                    .iter()
                    .all(|&e| world.get::<Visibility>(e) == Some(&Visibility::Hidden))
            );
            world.insert_resource(palette);
            world.insert_resource(Playback(sim));
        }
    }
}
