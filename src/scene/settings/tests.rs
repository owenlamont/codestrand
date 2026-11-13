use super::*;
use bevy::math::DVec2;

#[test]
fn wheel_over_offset_panel_scrolls_while_camera_input_is_captured() {
    let mut app = App::new();
    app.insert_resource(Settings {
        open: true,
        ..default()
    })
    .insert_resource(AccumulatedMouseScroll {
        delta: Vec2::new(0.0, -1.0),
        ..default()
    })
    .add_systems(Update, scroll);
    let mut window = Window::default();
    window.set_physical_cursor_position(Some(DVec2::new(860.0, 280.0)));
    let window = app.world_mut().spawn((window, PrimaryWindow)).id();
    app.world_mut().spawn((
        Camera {
            viewport: Some(Viewport {
                physical_position: UVec2::new(760, 180),
                physical_size: UVec2::new(1920, 1080),
                ..default()
            }),
            ..default()
        },
        AvatarCamera,
    ));
    app.world_mut().spawn((
        Panel,
        ComputedNode {
            size: Vec2::splat(200.0),
            ..default()
        },
        UiGlobalTransform::from_translation(Vec2::splat(100.0)),
    ));
    let list = app
        .world_mut()
        .spawn((
            PresetList,
            ScrollPosition::default(),
            ComputedNode {
                size: Vec2::splat(100.0),
                content_size: Vec2::new(100.0, 500.0),
                inverse_scale_factor: 1.0,
                ..default()
            },
        ))
        .id();
    app.update();
    assert_eq!(app.world().get::<ScrollPosition>(list).unwrap().y, 32.0);
    assert!(captures_camera(app.world()));
    app.world_mut()
        .get_mut::<Window>(window)
        .unwrap()
        .set_physical_cursor_position(Some(DVec2::ZERO));
    app.update();
    assert_eq!(app.world().get::<ScrollPosition>(list).unwrap().y, 32.0);
}

#[test]
fn projected_labels_are_local_to_the_letterboxed_ui() {
    let camera = Camera {
        viewport: Some(Viewport {
            physical_position: UVec2::new(760, 180),
            physical_size: UVec2::new(1920, 1080),
            ..default()
        }),
        computed: bevy::camera::ComputedCameraValues {
            clip_from_view: Mat4::IDENTITY,
            target_info: Some(bevy::camera::RenderTargetInfo {
                physical_size: UVec2::new(3440, 1440),
                scale_factor: 2.0,
            }),
            ..default()
        },
        ..default()
    };
    assert_eq!(
        super::super::project_point(
            &camera,
            &GlobalTransform::IDENTITY,
            Vec3::new(0.0, 0.0, 0.5)
        ),
        Some(Vec2::new(640.0, 360.0))
    );
}

#[test]
fn cameras_and_ui_use_the_native_letterbox_then_monitor_native_fills() {
    let mut app = App::new();
    app.insert_resource(Settings {
        open: true,
        monitor: Some(UVec2::new(3440, 1440)),
        windowed: Some((UVec2::new(1280, 720), WindowPosition::Automatic)),
        chosen: Some(UVec2::new(1920, 1080)),
        ..default()
    })
    .init_resource::<UiScale>()
    .init_resource::<ButtonInput<MouseButton>>()
    .add_systems(Update, (clicks, viewports, super::super::scale_ui).chain());
    let mut window = Window {
        resolution: (3440, 1440).into(),
        ..default()
    };
    window.resolution.set_scale_factor(2.0);
    app.world_mut().spawn((window, PrimaryWindow));
    let scene = app.world_mut().spawn((Camera::default(), SceneCamera)).id();
    let avatar = app
        .world_mut()
        .spawn((Camera::default(), AvatarCamera))
        .id();
    app.world_mut().spawn((
        Action::Native,
        RelativeCursorPosition {
            cursor_over: true,
            normalized: Some(Vec2::ZERO),
        },
        BackgroundColor::default(),
    ));
    app.update();
    for entity in [scene, avatar] {
        let viewport = app
            .world()
            .get::<Camera>(entity)
            .unwrap()
            .viewport
            .as_ref()
            .unwrap();
        assert_eq!(viewport.physical_position, UVec2::new(760, 180));
        assert_eq!(viewport.physical_size, UVec2::new(1920, 1080));
    }
    assert_eq!(app.world().resource::<UiScale>().0, 0.75);
    assert!(matches!(
        app.world().get::<Camera>(scene).unwrap().output_mode,
        CameraOutputMode::Write {
            clear_color: ClearColorConfig::Custom(Color::BLACK),
            ..
        }
    ));
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);
    app.update();
    assert_eq!(
        app.world()
            .get::<Camera>(scene)
            .unwrap()
            .viewport
            .as_ref()
            .unwrap()
            .physical_size,
        UVec2::new(3440, 1440)
    );
    assert!(app.world().resource::<Settings>().windowed.is_some());
    assert!(app.world().resource::<Settings>().chosen.is_none());
}

#[test]
fn oversized_preset_clicks_are_disabled_in_windowed_and_fullscreen() {
    let mut app = App::new();
    app.insert_resource(Settings {
        open: true,
        monitor: Some(UVec2::new(3440, 1440)),
        ..default()
    })
    .init_resource::<ButtonInput<MouseButton>>()
    .add_systems(Update, clicks);
    let window = app
        .world_mut()
        .spawn((Window::default(), PrimaryWindow))
        .id();
    let button = app
        .world_mut()
        .spawn((
            Action::Resize(UVec2::new(3840, 1080)),
            RelativeCursorPosition {
                cursor_over: true,
                normalized: Some(Vec2::ZERO),
            },
            BackgroundColor::default(),
        ))
        .id();
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);
    app.update();
    assert!(
        app.world()
            .resource::<Settings>()
            .pending_windowed
            .is_none()
    );
    assert_eq!(
        app.world().get::<Window>(window).unwrap().physical_width(),
        1280
    );
    assert_eq!(
        app.world().get::<BackgroundColor>(button).unwrap().0,
        Color::srgb(0.09, 0.10, 0.12)
    );
    app.world_mut()
        .resource_scope(|world, mut settings: Mut<Settings>| {
            settings.toggle_fullscreen(&mut world.get_mut::<Window>(window).unwrap());
        });
    app.update();
    assert!(app.world().resource::<Settings>().chosen.is_none());
}

#[test]
fn fullscreen_restores_windowed_size_and_position_after_monitor_resize() {
    let mut window = Window {
        resolution: (1536, 864).into(),
        position: WindowPosition::At(IVec2::new(-1500, 120)),
        ..default()
    };
    let mut settings = Settings::default();
    for _ in 0..2 {
        settings.toggle_fullscreen(&mut window);
        assert_eq!(
            window.mode,
            WindowMode::BorderlessFullscreen(MonitorSelection::Current)
        );
        window.resolution.set_physical_resolution(3840, 2160);
        window.position = WindowPosition::At(IVec2::ZERO);
        settings.toggle_fullscreen(&mut window);
        assert_eq!(window.mode, WindowMode::Windowed);
        settings.apply_windowed(&mut window);
        assert_eq!(window.resolution.physical_size(), UVec2::new(1536, 864));
        assert_eq!(window.position, WindowPosition::At(IVec2::new(-1500, 120)));
    }
}

#[test]
fn chosen_physical_resolution_applies_in_both_modes_with_hidpi() {
    let mut window = Window::default();
    window.resolution.set_scale_factor(2.0);
    let mut settings = Settings::default();
    settings.resize(&mut window, UVec2::new(1920, 1080));
    settings.apply_windowed(&mut window);
    assert_eq!(window.resolution.physical_size(), UVec2::new(1920, 1080));
    settings.toggle_fullscreen(&mut window);
    window.resolution.set_physical_resolution(3440, 1440);
    settings.resize(&mut window, UVec2::new(2560, 1440));
    assert_eq!(
        window.mode,
        WindowMode::BorderlessFullscreen(MonitorSelection::Current)
    );
    assert_eq!(settings.rectangle(&window).size(), UVec2::new(2560, 1440));
    assert_eq!(settings.logical_height(&window), 720.0);
    settings.toggle_fullscreen(&mut window);
    settings.apply_windowed(&mut window);
    assert_eq!(window.resolution.physical_size(), UVec2::new(1920, 1080));
}

#[test]
fn windowed_preset_waits_until_after_unmaximizing() {
    let mut window = Window::default();
    let mut settings = Settings::default();
    settings.resize(&mut window, UVec2::new(1920, 1080));
    assert_eq!(window.resolution.physical_size(), UVec2::new(1280, 720));
    window.resolution.set_physical_resolution(800, 600);
    settings.apply_windowed(&mut window);
    assert_eq!(window.resolution.physical_size(), UVec2::new(1920, 1080));
    assert!(settings.pending_windowed.is_none());
}

#[test]
fn fullscreen_preset_keeps_mode_then_f11_defers_restoration() {
    let saved_position = WindowPosition::At(IVec2::new(120, 80));
    let mut window = Window {
        position: saved_position,
        ..default()
    };
    let mut settings = Settings {
        open: true,
        monitor: Some(UVec2::new(1920, 1080)),
        ..default()
    };
    settings.toggle_fullscreen(&mut window);
    window.resolution.set_physical_resolution(1920, 1080);
    window.position = WindowPosition::At(IVec2::ZERO);
    let mut app = App::new();
    app.insert_resource(settings)
        .init_resource::<ButtonInput<MouseButton>>()
        .add_systems(Update, (apply_windowed, clicks).chain());
    let entity = app.world_mut().spawn((window, PrimaryWindow)).id();
    app.world_mut().spawn((
        Action::Resize(UVec2::new(1920, 1080)),
        RelativeCursorPosition {
            cursor_over: true,
            normalized: Some(Vec2::ZERO),
        },
        BackgroundColor::default(),
    ));
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);
    app.update();
    let window = app.world_mut().get_mut::<Window>(entity).unwrap();
    assert_eq!(
        window.mode,
        WindowMode::BorderlessFullscreen(MonitorSelection::Current)
    );
    assert_eq!(window.resolution.physical_size(), UVec2::new(1920, 1080));
    assert_eq!(window.position, WindowPosition::At(IVec2::ZERO));
    app.world_mut()
        .resource_scope(|world, mut settings: Mut<Settings>| {
            settings.toggle_fullscreen(&mut world.get_mut::<Window>(entity).unwrap());
        });
    assert_eq!(
        app.world()
            .get::<Window>(entity)
            .unwrap()
            .resolution
            .physical_size(),
        UVec2::new(1920, 1080)
    );
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .reset_all();
    app.update();
    let window = app.world().get::<Window>(entity).unwrap();
    assert_eq!(window.resolution.physical_size(), UVec2::new(1280, 720));
    assert_eq!(window.position, saved_position);
    assert!(
        app.world()
            .resource::<Settings>()
            .pending_windowed
            .is_none()
    );
}

#[test]
fn preset_click_requires_open_panel_and_applies_without_playback_resources() {
    let mut app = App::new();
    app.insert_resource(Settings {
        monitor: Some(UVec2::new(3440, 1440)),
        ..default()
    })
    .init_resource::<ButtonInput<MouseButton>>()
    .add_systems(Update, (apply_windowed, clicks).chain());
    let window = app
        .world_mut()
        .spawn((Window::default(), PrimaryWindow))
        .id();
    app.world_mut().spawn((
        Action::Resize(UVec2::new(1920, 1080)),
        RelativeCursorPosition {
            cursor_over: true,
            normalized: Some(Vec2::ZERO),
        },
        BackgroundColor::default(),
    ));
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);
    app.update();
    assert_eq!(
        app.world().get::<Window>(window).unwrap().physical_width(),
        1280
    );
    app.world_mut().resource_mut::<Settings>().open = true;
    app.update();
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .reset_all();
    app.update();
    assert_eq!(
        app.world()
            .get::<Window>(window)
            .unwrap()
            .resolution
            .physical_size(),
        UVec2::new(1920, 1080)
    );
}

#[test]
fn panel_toggle_captures_camera_without_a_window_backend() {
    let mut app = App::new();
    app.init_resource::<Settings>()
        .init_resource::<ButtonInput<KeyCode>>()
        .add_systems(Update, controls);
    app.world_mut().spawn((Window::default(), PrimaryWindow));
    let panel = app.world_mut().spawn((Node::default(), Panel)).id();
    app.world_mut().spawn((Text::default(), Status));
    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::F10);
    app.update();
    assert!(captures_camera(app.world()));
    assert_eq!(
        app.world().get::<Node>(panel).unwrap().display,
        Display::Flex
    );
    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .reset_all();
    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::F10);
    app.update();
    assert!(!captures_camera(app.world()));
    assert_eq!(
        app.world().get::<Node>(panel).unwrap().display,
        Display::None
    );
    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .reset_all();
    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::F11);
    app.update();
    assert!(app.world().resource::<Settings>().windowed.is_some());
    assert!(!captures_camera(app.world()));
}
