use super::*;

#[test]
fn capture_key_toggles_and_escape_releases_before_quitting() {
    let mut state = State::default();
    let toggle = Controls {
        focused: true,
        toggle_capture: true,
        ..default()
    };
    assert!(!state.transition(&toggle));
    assert!(state.captured);
    assert!(!state.transition(&toggle));
    assert!(!state.captured);
    state.transition(&toggle);
    let escape = Controls {
        focused: true,
        escape: true,
        ..default()
    };
    assert!(!state.transition(&escape));
    assert!(!state.captured);
    assert!(state.transition(&escape));
}

#[test]
fn panel_open_close_and_focus_loss_require_explicit_recapture() {
    for release in [
        Controls {
            focused: true,
            panel_open: true,
            toggle_capture: true,
            ..default()
        },
        Controls {
            focused: true,
            panel_toggled: true,
            click_viewport: true,
            ..default()
        },
        Controls {
            focused: false,
            escape: true,
            ..default()
        },
        Controls {
            focused: true,
            focus_lost: true,
            toggle_capture: true,
            ..default()
        },
    ] {
        let mut state = State {
            captured: true,
            ..default()
        };
        assert!(!state.transition(&release));
        assert!(!state.captured);
        state.transition(&Controls {
            focused: true,
            ..default()
        });
        assert!(!state.captured);
        state.transition(&Controls {
            focused: true,
            toggle_capture: true,
            ..default()
        });
        assert!(state.captured);
    }
}

#[test]
fn closing_panel_does_not_capture_the_close_click() {
    let mut state = State {
        panel_was_open: true,
        ..default()
    };
    state.transition(&Controls {
        focused: true,
        click_viewport: true,
        ..default()
    });
    assert!(!state.captured);
    state.transition(&Controls {
        focused: true,
        ..default()
    });
    assert!(!state.captured);
    state.transition(&Controls {
        focused: true,
        click_viewport: true,
        ..default()
    });
    assert!(state.captured);
}

#[test]
fn capture_applies_cursor_options_and_discards_transition_motion() {
    let (mut world, window) = fixture();
    let mut state = State::default();
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::KeyC);
    assert_eq!(state.read(&mut world).aim, Vec2::ZERO);
    let options = world.get::<CursorOptions>(window).unwrap();
    assert_eq!(options.grab_mode, CursorGrabMode::Locked);
    assert!(!options.visible);
    world.resource_mut::<ButtonInput<KeyCode>>().reset_all();
    assert_eq!(state.read(&mut world).aim, Vec2::splat(500.0));
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::Escape);
    assert_eq!(state.read(&mut world).aim, Vec2::ZERO);
    assert!(world.resource::<Messages<AppExit>>().is_empty());
    let options = world.get::<CursorOptions>(window).unwrap();
    assert_eq!(options.grab_mode, CursorGrabMode::None);
    assert!(options.visible);
    world.resource_mut::<ButtonInput<KeyCode>>().reset_all();
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::Escape);
    state.read(&mut world);
    assert_eq!(world.resource::<Messages<AppExit>>().len(), 1);
}

fn fixture() -> (World, Entity) {
    let mut world = World::new();
    world.init_resource::<ButtonInput<KeyCode>>();
    world.init_resource::<ButtonInput<MouseButton>>();
    world.insert_resource(AccumulatedMouseMotion {
        delta: Vec2::splat(500.0),
    });
    world.init_resource::<AccumulatedMouseScroll>();
    world.init_resource::<Messages<AppExit>>();
    world.init_resource::<Messages<WindowFocused>>();
    let window = world
        .spawn((Window::default(), PrimaryWindow, CursorOptions::default()))
        .id();
    (world, window)
}

#[test]
fn focus_loss_and_return_in_one_frame_release_capture() {
    let (mut world, window) = fixture();
    let mut state = State::default();
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::KeyC);
    state.read(&mut world);
    world.resource_mut::<ButtonInput<KeyCode>>().reset_all();
    world.write_message(WindowFocused {
        window,
        focused: false,
    });
    world.write_message(WindowFocused {
        window,
        focused: true,
    });
    assert_eq!(state.read(&mut world).aim, Vec2::ZERO);
    assert!(!state.captured);
    assert!(world.get::<CursorOptions>(window).unwrap().visible);
    state.read(&mut world);
    assert!(!state.captured);
}

#[test]
fn clicks_capture_only_inside_the_letterboxed_viewport() {
    let (mut world, window) = fixture();
    world.spawn((
        super::super::SceneCamera,
        Camera {
            viewport: Some(bevy::camera::Viewport {
                physical_position: UVec2::new(100, 50),
                physical_size: UVec2::new(400, 200),
                ..default()
            }),
            ..default()
        },
    ));
    let mut state = State::default();
    world
        .get_mut::<Window>(window)
        .unwrap()
        .set_physical_cursor_position(Some(Vec2::new(50.0, 100.0).as_dvec2()));
    world
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);
    state.read(&mut world);
    assert!(!state.captured);
    world.resource_mut::<ButtonInput<MouseButton>>().reset_all();
    world
        .get_mut::<Window>(window)
        .unwrap()
        .set_physical_cursor_position(Some(Vec2::new(150.0, 100.0).as_dvec2()));
    world
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);
    state.read(&mut world);
    assert!(state.captured);
    world.resource_mut::<ButtonInput<MouseButton>>().clear();
    assert_eq!(state.read(&mut world).lift, 0.0);
    assert_eq!(state.read(&mut world).lift, 0.0);
    world.resource_mut::<ButtonInput<MouseButton>>().reset_all();
    state.read(&mut world);
    world
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);
    assert_eq!(state.read(&mut world).lift, 1.0);
}

#[test]
fn same_frame_release_and_repress_rearms_captured_lift() {
    for (button, expected) in [(MouseButton::Left, 1.0), (MouseButton::Right, -1.0)] {
        let (mut world, _) = fixture();
        let mut state = State::default();
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(button);
        world
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyC);
        state.read(&mut world);
        world.resource_mut::<ButtonInput<KeyCode>>().reset_all();
        world.resource_mut::<ButtonInput<MouseButton>>().clear();
        state.read(&mut world);
        let mut mouse = world.resource_mut::<ButtonInput<MouseButton>>();
        mouse.release(button);
        mouse.press(button);
        assert!(mouse.just_released(button));
        assert!(mouse.pressed(button));
        assert_eq!(state.read(&mut world).lift, expected);
    }
}

#[test]
fn release_and_repress_on_a_new_capture_still_suppresses_the_capture_click() {
    let (mut world, _) = fixture();
    let mut state = State::default();
    let mut mouse = world.resource_mut::<ButtonInput<MouseButton>>();
    mouse.press(MouseButton::Left);
    mouse.release(MouseButton::Left);
    mouse.press(MouseButton::Left);
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::KeyC);
    assert_eq!(state.read(&mut world).lift, 0.0);
    assert!(state.captured);
    world.resource_mut::<ButtonInput<KeyCode>>().reset_all();
    world.resource_mut::<ButtonInput<MouseButton>>().clear();
    assert_eq!(state.read(&mut world).lift, 0.0);
}

#[test]
fn captured_buttons_move_continuously_and_both_cancel() {
    let (mut world, _) = fixture();
    let mut state = State::default();
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::KeyC);
    state.read(&mut world);
    world.resource_mut::<ButtonInput<KeyCode>>().reset_all();
    for (buttons, expected) in [
        (vec![MouseButton::Left], 1.0),
        (vec![MouseButton::Right], -1.0),
        (vec![MouseButton::Left, MouseButton::Right], 0.0),
        (vec![], 0.0),
    ] {
        world.resource_mut::<ButtonInput<MouseButton>>().reset_all();
        state.read(&mut world);
        for button in buttons {
            world
                .resource_mut::<ButtonInput<MouseButton>>()
                .press(button);
        }
        assert_eq!(state.read(&mut world).lift, expected);
        world.resource_mut::<ButtonInput<MouseButton>>().clear();
        assert_eq!(state.read(&mut world).lift, expected);
    }
}

#[test]
fn released_buttons_orbit_without_lift() {
    let (mut world, _) = fixture();
    let mut state = State::default();
    world
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);
    assert_eq!(state.camera_input(&world, Some(Vec2::ZERO)).lift, 0.0);
    world
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Right);
    state.camera_input(&world, Some(Vec2::ZERO));
    let input = state.camera_input(&world, Some(Vec2::new(12.0, -8.0)));
    assert_eq!(input.lift, 0.0);
    assert_eq!(input.orbit, Vec2::new(12.0, -8.0));
}

#[test]
fn viewport_click_captures_but_idle_and_panel_clicks_do_not() {
    let mut state = State::default();
    state.transition(&Controls {
        focused: true,
        ..default()
    });
    assert!(!state.captured);
    state.transition(&Controls {
        focused: true,
        panel_open: true,
        click_viewport: true,
        ..default()
    });
    assert!(!state.captured);
    state.transition(&Controls {
        focused: true,
        ..default()
    });
    state.transition(&Controls {
        focused: true,
        click_viewport: true,
        ..default()
    });
    assert!(state.captured);
}
