use super::*;
use crate::timeline::{Event, FileChange, Identity, Kind};

fn event(seconds: i64, changes: Vec<FileChange>) -> Event {
    Event {
        repository: 0,
        author: Identity {
            name: b"author".to_vec(),
            email: b"author@example.invalid".to_vec(),
        },
        commit: String::new(),
        contributor: None,
        seconds,
        changes,
    }
}

fn added(path: &str) -> FileChange {
    FileChange {
        path: path.into(),
        ..Default::default()
    }
}

fn fixture(events: Vec<Event>) -> (World, Palette, Pools, Simulation) {
    let theme = Theme::neon();
    let mut simulation = Simulation::new(
        events,
        &model::Options::default(),
        &theme,
        &crate::contributors::Mappings::default(),
    );
    simulation.advance_to(18, 30);
    let mut world = World::new();
    world.init_resource::<Assets<Mesh>>();
    world.init_resource::<Assets<Image>>();
    world.init_resource::<Assets<StandardMaterial>>();
    let palette = Palette::new(&mut world, &theme);
    let mut pools = Pools::default();
    sync_files(&mut world, &palette, &mut pools, &simulation);
    (world, palette, pools, simulation)
}

#[test]
fn glyph_cache_follows_theme_changes_and_reset() {
    let (mut world, palette, mut pools, mut simulation) =
        fixture(vec![event(0, vec![added("main.rs")])]);
    let entity = pools.files[&0];
    let mut theme = crate::theme::ThemeName::Phosphor.build();
    theme.glyphs = &[];
    simulation.set_theme(theme);
    sync_files(&mut world, &palette, &mut pools, &simulation);
    assert_eq!(world.get::<FileVisual>(entity).unwrap().glyph_kind, 0);
    simulation.set_theme(Theme::neon());
    simulation.restart();
    simulation.advance_to(18, 30);
    sync_files(&mut world, &palette, &mut pools, &simulation);
    assert_eq!(
        world.get::<FileVisual>(entity).unwrap().glyph_kind,
        simulation.theme().glyph_kind("main.rs")
    );
    assert_ne!(world.get::<FileVisual>(entity).unwrap().glyph_kind, 0);
    world.get_mut::<FileVisual>(entity).unwrap().glyph_kind = 0;
    world.insert_resource(Playback(simulation));
    world.insert_resource(palette);
    switch_theme(&mut world, crate::theme::ThemeName::Neon);
    let palette = world.remove_resource::<Palette>().unwrap();
    let simulation = world.remove_resource::<Playback>().unwrap().0;
    sync_files(&mut world, &palette, &mut pools, &simulation);
    assert_eq!(
        world.get::<FileVisual>(entity).unwrap().glyph_kind,
        simulation.theme().glyph_kind("main.rs")
    );
}

#[test]
fn retention_removes_replaced_file_when_layout_counts_match() {
    let (mut world, palette, mut pools, mut simulation) = fixture(vec![
        event(0, vec![added("a.rs"), added("b.rs")]),
        event(
            86_400,
            vec![
                FileChange {
                    path: "a.rs".into(),
                    kind: Kind::Moved {
                        from: "b.rs".into(),
                    },
                    ..Default::default()
                },
                added("c.rs"),
            ],
        ),
    ]);
    let removed = pools.files[&0];
    let children = world.get::<Children>(removed).unwrap().to_vec();
    let count = pools.files.len();
    simulation.advance_to(90, 30);
    assert_eq!(simulation.layout().files().count(), count);
    assert!(simulation.layout().file(0).is_none());
    sync_files(&mut world, &palette, &mut pools, &simulation);
    assert_eq!(pools.files.len(), count);
    assert!(!pools.files.contains_key(&0));
    assert!(world.get_entity(removed).is_err());
    assert!(children.into_iter().all(|e| world.get_entity(e).is_err()));
}

#[test]
fn hidden_halo_stays_untouched_and_updates_when_glowing() {
    let (mut world, palette, mut pools, mut simulation) =
        fixture(vec![event(0, vec![added("main.rs")])]);
    let halo = world.get::<FileVisual>(pools.files[&0]).unwrap().halo;
    simulation.advance_to(600, 30);
    sync_files(&mut world, &palette, &mut pools, &simulation);
    assert_eq!(world.get::<Visibility>(halo), Some(&Visibility::Hidden));
    let sentinel = Transform::from_xyz(10.0, 20.0, 30.0);
    world.entity_mut(halo).insert(sentinel);
    world.clear_trackers();
    sync_files(&mut world, &palette, &mut pools, &simulation);
    assert_eq!(world.get::<Transform>(halo), Some(&sentinel));
    assert!(
        !world
            .entity(halo)
            .get_ref::<Transform>()
            .unwrap()
            .is_changed()
    );
    simulation.advance_to(18, 30);
    sync_files(&mut world, &palette, &mut pools, &simulation);
    let file = simulation.layout().file(0).unwrap();
    let grow = 1.0 + simulation.theme().activity_growth * file.activity;
    let mut expected = simulation.theme().activity_transform;
    expected.scale *= Vec3::new(grow, grow, 1.0);
    assert_eq!(world.get::<Visibility>(halo), Some(&Visibility::Inherited));
    assert_eq!(world.get::<Transform>(halo), Some(&expected));
}

#[test]
fn unchanged_file_and_structure_transforms_do_not_change() {
    let (mut world, palette, mut pools, simulation) =
        fixture(vec![event(0, vec![added("src/main.rs")])]);
    pools.grid.push(world.spawn(Transform::default()).id());
    pools.lod.glyphs = vec![0];
    pools.lod.glyph[0] = 1.0;
    sync_structure(&mut world, &palette, &mut pools, &simulation);
    let entities: Vec<_> = pools
        .files
        .values()
        .chain(&pools.directories)
        .chain(&pools.edges)
        .chain(&pools.aggregates)
        .chain(&pools.grid)
        .copied()
        .collect();
    world.clear_trackers();
    sync_files(&mut world, &palette, &mut pools, &simulation);
    pools.lod.glyphs = vec![0];
    pools.lod.glyph[0] = 1.0;
    sync_structure(&mut world, &palette, &mut pools, &simulation);
    for entity in entities {
        assert!(
            !world
                .entity(entity)
                .get_ref::<Transform>()
                .unwrap()
                .is_changed(),
            "{entity}"
        );
    }
}
