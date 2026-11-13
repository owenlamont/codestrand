use super::*;

fn fixture(options: &model::Options) -> World {
    let mut events = crate::timeline::synthetic();
    events.truncate(1);
    events[0].changes.push(crate::timeline::FileChange {
        path: "src/second.rs".into(),
        ..default()
    });
    let mut simulation = Simulation::new(
        events,
        options,
        &Theme::neon(),
        &crate::contributors::Mappings::default(),
    );
    simulation.collapse_directories = false;
    let mut world = World::new();
    world.insert_resource(Playback(simulation));
    world.insert_resource(SceneTarget(Handle::<Image>::default().into()));
    world.insert_resource(Title("test".into()));
    world.insert_resource(Frame { index: 30, fps: 30 });
    world.init_resource::<Pools>();
    world.init_resource::<Assets<Mesh>>();
    world.init_resource::<Assets<Image>>();
    world.init_resource::<Assets<StandardMaterial>>();
    setup(&mut world);
    animate(&mut world);
    world
}

#[test]
fn static_frame_performs_no_file_writes() {
    let mut world = fixture(&model::Options::default());
    let file = *world.resource::<Pools>().files.values().next().unwrap();
    let halo = world.get::<FileVisual>(file).unwrap().halo;
    world.clear_trackers();
    animate(&mut world);
    for entity in [file, halo] {
        assert!(
            !world
                .entity(entity)
                .get_ref::<Transform>()
                .unwrap()
                .is_changed()
        );
    }
    assert!(
        world
            .query_filtered::<Entity, Changed<MeshMaterial3d<StandardMaterial>>>()
            .iter(&world)
            .all(|entity| entity != world.get::<FileVisual>(file).unwrap().membership)
    );
}

#[test]
fn static_theme_switch_restyles_files() {
    let mut world = fixture(&model::Options::default());
    let file = *world.resource::<Pools>().files.values().next().unwrap();
    switch_theme(&mut world, crate::theme::ThemeName::Phosphor);
    animate(&mut world);
    assert_eq!(
        world.get::<FileVisual>(file).unwrap().glyph_theme,
        Some(crate::theme::ThemeName::Phosphor)
    );
}

#[test]
fn static_lod_change_runs_sync() {
    let mut world = fixture(&model::Options::default());
    world.resource_mut::<Playback>().0.collapse_directories = true;
    world.resource_mut::<Frame>().index = 300;
    let view = world.resource::<Playback>().0.view();
    world.insert_resource(PreviewCamera::new(view));
    world.init_resource::<Time<Real>>();
    world.insert_resource(ScriptedCamera(
        Transform::from_xyz(0.0, 5.0, 0.0).looking_at(Vec3::ZERO, Vec3::NEG_Z),
    ));
    animate(&mut world);
    let id = world
        .resource::<Playback>()
        .0
        .layout()
        .files()
        .find(|(_, file)| file.name() == "main.rs")
        .unwrap()
        .0;
    let file = world.resource::<Pools>().files[&id];
    assert_eq!(world.get::<Visibility>(file), Some(&Visibility::Inherited));
    world.insert_resource(ScriptedCamera(
        Transform::from_xyz(0.0, 100_000.0, 0.0).looking_at(Vec3::ZERO, Vec3::NEG_Z),
    ));
    world
        .resource_mut::<Time<Real>>()
        .advance_by(std::time::Duration::from_secs(1));
    animate(&mut world);
    assert_eq!(world.get::<Visibility>(file), Some(&Visibility::Hidden));
}

#[test]
fn consecutive_static_lifecycle_frames_keep_the_tint() {
    let mut world = fixture(&model::Options {
        repository_archived: vec![true],
        repository_ends: vec![None],
        ..default()
    });
    let file = *world.resource::<Pools>().files.values().next().unwrap();
    let glyph = world.get::<FileVisual>(file).unwrap().glyph;
    let original = world
        .get::<MeshMaterial3d<StandardMaterial>>(glyph)
        .unwrap()
        .0
        .clone();
    world.resource_mut::<Frame>().index = 100;
    animate(&mut world);
    let frame = world.resource::<Frame>().index;
    let tinted = world
        .get::<MeshMaterial3d<StandardMaterial>>(glyph)
        .unwrap()
        .0
        .clone();
    assert!(world.get::<LifecycleMaterial>(glyph).is_some());
    assert_ne!(tinted, original);
    animate(&mut world);
    assert_eq!(world.resource::<Frame>().index, frame);
    assert_eq!(
        world
            .get::<MeshMaterial3d<StandardMaterial>>(glyph)
            .unwrap()
            .0,
        tinted
    );
}
