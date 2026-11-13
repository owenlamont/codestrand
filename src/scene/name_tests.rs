use super::*;

#[test]
fn oblique_names_stay_tangent_to_the_projected_portrait() {
    let mut sim = Simulation::new(
        crate::timeline::synthetic(),
        &model::Options::default(),
        &Theme::neon(),
        &crate::contributors::Mappings::default(),
    );
    sim.advance_to(60, 60);
    let avatar = sim.avatars().iter().find(|a| a.presence > 0.0).unwrap();
    let label = sim
        .labels()
        .into_iter()
        .find(|l| l.kind == model::LabelKind::Avatar)
        .unwrap();
    for offset in [Vec3::new(0.0, 20.0, 0.0), Vec3::new(4.0, 1.4, 7.5)] {
        let transform = GlobalTransform::from(
            Transform::from_translation(avatar.position + offset)
                .looking_at(avatar.position, Vec3::NEG_Z),
        );
        let camera = Camera {
            computed: bevy::camera::ComputedCameraValues {
                clip_from_view: bevy::math::proj::perspective_infinite_reverse(
                    model::framing::FOV,
                    1280.0 / 720.0,
                    0.1,
                ),
                target_info: Some(bevy::camera::RenderTargetInfo {
                    physical_size: UVec2::new(1280, 720),
                    scale_factor: 1.0,
                }),
                ..default()
            },
            ..default()
        };
        let point =
            project_avatar_name(&camera, &transform, &sim, avatar, &label).unwrap();
        let body = camera
            .world_to_viewport(&transform, avatar.position)
            .unwrap();
        let radius = sim.theme().avatar_radius
            * avatar.presence
            * sim.scale()
            * sim.avatar_disc_scale()
            * avatar.projection_scale
            * avatar.lifecycle_scale;
        let edge = camera
            .world_to_viewport(&transform, avatar.position + transform.right() * radius)
            .unwrap();
        let rect = model::label_rect(
            point,
            &label.text,
            label.scale,
            sim.theme().label_height,
        );
        let gap = body.distance(body.clamp(rect.min, rect.max)) - body.distance(edge);
        assert!((gap - 2.0).abs() < 0.001, "portrait gap={gap}");
    }
}

#[test]
fn contributor_fonts_shape_accents_and_non_latin_letters() {
    use bevy::text::*;
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        bevy::asset::AssetPlugin::default(),
        TextPlugin,
    ));
    let plex = app
        .world_mut()
        .resource_mut::<Assets<Font>>()
        .add(Font::from_bytes(
            include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf").to_vec(),
        ));
    app.update();
    let font = TextFont {
        font: styling::label_font(Handle::default(), plex),
        ..TextFont::from_font_size(14.0)
    };
    let world = app.world_mut();
    let fonts = world.remove_resource::<Assets<Font>>().unwrap();
    let mut cx = world.remove_resource::<FontCx>().unwrap();
    let texts = [
        "Fernando Pérez-García",
        "João Moreno",
        "Ελληνικά",
        "Кириллица",
    ];
    #[cfg(windows)]
    let texts = texts.into_iter().chain(["अभिजीत बोडस", "中文"]);
    for text in texts {
        let mut computed = ComputedTextBlock::default();
        TextPipeline::default()
            .update_buffer(
                &fonts,
                [(
                    Entity::PLACEHOLDER,
                    0,
                    TextElement::Text {
                        text,
                        font: &font,
                        color: Color::WHITE,
                        line_height: LineHeight::default(),
                        letter_spacing: LetterSpacing::default(),
                    },
                )]
                .into_iter(),
                LineBreak::NoWrap,
                Justify::Left,
                TextBounds::UNBOUNDED,
                1.0,
                &mut computed,
                &mut cx,
                &mut LayoutCx::default(),
                Vec2::new(1280.0, 720.0),
                RemSize::default(),
            )
            .unwrap();
        let glyphs: Vec<_> = computed
            .buffer()
            .lines()
            .flat_map(|line| line.runs())
            .flat_map(|run| {
                run.clusters()
                    .flat_map(|cluster| cluster.glyphs())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(glyphs.len() >= 2);
        assert!(
            glyphs.iter().all(|glyph| glyph.id != 0),
            "missing glyph in {text}"
        );
    }
}

#[test]
fn close_names_cannot_cover_the_caption_or_heading() {
    for y in [0.0, 690.0, 700.0] {
        let chosen = place_labels(
            vec![(
                model::Label {
                    position: Vec3::ZERO,
                    text: "file-ignores.md".into(),
                    kind: model::LabelKind::Detail,
                    scale: 0.7,
                    opacity: 1.0,
                    priority: 0.0,
                },
                Vec2::new(200.0, y),
                1.0,
            )],
            20.0,
            Vec2::new(1280.0, 720.0),
        );
        assert!(chosen.is_empty(), "label at y={y} covers frame text");
    }
}

fn fixture() -> Simulation {
    let mut events = crate::timeline::synthetic();
    events.truncate(1);
    let change = events[0].changes[0].clone();
    events[0].changes = (0..90)
        .map(|index| crate::timeline::FileChange {
            path: format!("tree/branch{index}/leaf{index}.rs"),
            kind: crate::timeline::Kind::Added,
            ..change.clone()
        })
        .collect();
    let mut sim = Simulation::new(
        events,
        &model::Options::default(),
        &Theme::neon(),
        &crate::contributors::Mappings::default(),
    );
    sim.advance_to(3600, 60);
    sim
}

fn view(rotation: Quat) -> names::View {
    let camera =
        Transform::from_translation(rotation * Vec3::Z * 100.0).with_rotation(rotation);
    names::View {
        clip_from_world: bevy::math::proj::orthographic(
            -100.0, 100.0, -100.0, 100.0, 0.1, 1000.0,
        ) * camera.to_matrix().inverse(),
        viewport: Vec2::splat(1_000_000.0),
    }
}

fn collect(sim: &Simulation, view: &names::View) -> Vec<(model::Label, Vec2, f32)> {
    let mut names = names::Names::default();
    names.sample(sim, view);
    sim.labels_with_names(|_| true, &names.targets)
        .into_iter()
        .chain(names.labels)
        .map(|label| {
            let ndc = view.clip_from_world.project_point3(label.position);
            let point = (ndc.truncate() * Vec2::new(1.0, -1.0) + Vec2::ONE)
                * view.viewport
                * 0.5;
            (label, point, 0.0)
        })
        .collect()
}

#[test]
fn close_names_survive_expired_activity_and_camera_rotation() {
    let sim = fixture();
    for rotation in [
        Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        Quat::from_rotation_x(-1.0) * Quat::from_rotation_z(0.7),
    ] {
        let view = view(rotation);
        let labels = collect(&sim, &view);
        assert!(labels.iter().any(|(label, ..)| label.text == "leaf89.rs"));
        assert!(labels.iter().any(|(label, ..)| label.text == "branch89/"));
        assert_eq!(
            labels
                .iter()
                .filter(|(l, ..)| l.text == "leaf89.rs")
                .count(),
            1
        );
    }
}

#[test]
fn close_names_bypass_both_landmark_and_renderer_caps() {
    let sim = fixture();
    let view = view(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2));
    let labels = place_labels(
        collect(&sim, &view),
        sim.theme().label_height,
        view.viewport,
    );
    let files = labels
        .iter()
        .filter(|(l, ..)| l.text.starts_with("leaf"))
        .count();
    let directories = labels
        .iter()
        .filter(|(l, ..)| l.text.starts_with("branch"))
        .count();
    assert_eq!(files, 90);
    assert_eq!(directories, 90);
}

#[test]
fn review_directory_bounds_include_visible_sides() {
    let sim = fixture();
    let (index, dir) = sim.layout().dirs().find(|(_, d)| d.depth > 0).unwrap();
    let center = ground(dir.position) - Vec3::Y * 0.05;
    let rotation = Quat::from_rotation_x(-26.0_f32.to_radians());
    let eye = center + Vec3::new(0.0, 1.25, 1.98) * dir.scale;
    assert!(eye.y >= crate::camera::EYE_FLOOR);
    let camera = Transform::from_translation(eye).with_rotation(rotation);
    let view = names::View {
        clip_from_world: bevy::math::proj::perspective_infinite_reverse(
            std::f32::consts::FRAC_PI_4,
            1280.0 / 720.0,
            0.1,
        ) * camera.to_matrix().inverse(),
        viewport: Vec2::new(1280.0, 720.0),
    };
    let half = sim.theme().directory_node_scale * dir.scale * 0.5;
    let mut low = Vec2::splat(f32::INFINITY);
    let mut high = Vec2::splat(f32::NEG_INFINITY);
    for x in [-half.x, half.x] {
        for y in [-half.y, half.y] {
            for z in [-half.z, half.z] {
                let ndc = view
                    .clip_from_world
                    .project_point3(center + Vec3::new(x, y, z));
                assert!(
                    ndc.x.abs() < 1.0
                        && ndc.y.abs() < 1.0
                        && ndc.z > 0.0
                        && ndc.z < 1.0
                );
                let p = ndc.truncate() * view.viewport * 0.5;
                low = low.min(p);
                high = high.max(p);
            }
        }
    }
    let pixels = (high - low).min_element();
    let mut names = names::Names::default();
    names.sample(&sim, &view);
    println!(
        "directory={} scale={} eye={eye:?} cube_footprint={pixels}px permanent={}",
        dir.name(),
        dir.scale,
        names.targets.contains(&model::Target::Dir(index))
    );
    assert!(pixels > 48.0);
    assert!(
        names.targets.contains(&model::Target::Dir(index)),
        "visible directory above threshold has no permanent name"
    );
}

#[test]
fn tilted_camera_measures_the_flat_file_footprint() {
    let sim = fixture();
    let (id, file) = sim.layout().files().next().unwrap();
    let dir = sim.layout().dir(file.dir);
    let scale = file.draw_scale(dir);
    let glyph = sim.theme().glyph_transform();
    let (point, _, _) = sim.file_effect(file);
    let center = point + glyph.translation * scale;
    let rotation = Quat::from_rotation_x(-15.0_f32.to_radians());
    let camera = Transform::from_translation(center + rotation * Vec3::Z * 10.0)
        .with_rotation(rotation);
    let view = names::View {
        clip_from_world: bevy::math::proj::orthographic(
            -2.0 * scale,
            2.0 * scale,
            -2.0 * scale,
            2.0 * scale,
            0.1,
            1000.0,
        ) * camera.to_matrix().inverse(),
        viewport: Vec2::splat(720.0),
    };
    let mut names = names::Names::default();
    names.sample(&sim, &view);
    assert!(
        !names.targets.contains(&model::Target::File(id)),
        "foreshortened flat glyph should be below the 48px name threshold"
    );
}
