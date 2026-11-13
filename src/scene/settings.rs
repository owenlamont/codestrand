use bevy::{
    prelude::*,
    ui::RelativeCursorPosition,
    window::{MonitorSelection, PrimaryWindow, WindowMode, WindowPosition},
};

use super::AvatarCamera;

pub(super) struct PreviewSettingsPlugin;

impl Plugin for PreviewSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Settings>()
            .add_systems(Startup, setup.after(super::setup))
            .add_systems(
                Update,
                (apply_windowed, clicks, controls)
                    .chain()
                    .before(super::preview_camera),
            );
    }
}

#[derive(Default, Resource)]
struct Settings {
    open: bool,
    windowed: Option<(UVec2, WindowPosition)>,
    pending_windowed: Option<(UVec2, WindowPosition)>,
}

impl Settings {
    fn toggle_fullscreen(&mut self, window: &mut Window) {
        if let Some((size, position)) = self.windowed.take() {
            window.mode = WindowMode::Windowed;
            window.set_maximized(false);
            self.pending_windowed = Some((size, position));
        } else {
            self.windowed = Some(self.pending_windowed.take().unwrap_or_else(|| {
                (window.resolution.physical_size(), window.position)
            }));
            window.mode = WindowMode::BorderlessFullscreen(MonitorSelection::Current);
        }
    }

    fn resize(&mut self, window: &mut Window, size: UVec2) {
        if self.windowed.is_some() {
            self.toggle_fullscreen(window);
        }
        window.set_maximized(false);
        let position = self
            .pending_windowed
            .map_or(window.position, |(_, position)| position);
        self.pending_windowed = Some((size, position));
    }

    fn apply_windowed(&mut self, window: &mut Window) {
        if window.mode == WindowMode::Windowed
            && let Some((size, position)) = self.pending_windowed.take()
        {
            window.position = position;
            window.resolution.set_physical_resolution(size.x, size.y);
        }
    }
}

// Apply size after Winit restores placement on fullscreen exit or unmaximizing.
fn apply_windowed(
    mut settings: ResMut<Settings>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
) {
    settings.apply_windowed(&mut window);
}

pub(super) fn captures_camera(world: &World) -> bool {
    world
        .get_resource::<Settings>()
        .is_some_and(|state| state.open)
}

#[derive(Component)]
struct Panel;

#[derive(Component)]
struct Status;

#[derive(Component)]
pub(super) struct SettingsText;

#[derive(Component, Clone, Copy)]
enum Action {
    Resize(UVec2),
    Fullscreen,
    Close,
}

fn setup(mut commands: Commands, camera: Single<Entity, With<AvatarCamera>>) {
    let camera = camera.into_inner();
    commands
        .spawn((
            Panel,
            UiTargetCamera(camera),
            GlobalZIndex(100),
            Node {
                display: Display::None,
                position_type: PositionType::Absolute,
                top: px(50),
                left: px(24),
                width: px(300),
                padding: UiRect::all(px(16)),
                row_gap: px(10),
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(Color::srgb(0.04, 0.05, 0.08)),
        ))
        .with_children(|panel| {
            panel.spawn(label("Display settings · F10", 22.0));
            panel.spawn((label("", 16.0), Status));
            panel.spawn(label("Window size (physical pixels)", 16.0));
            for (name, action) in [
                ("1280 × 720", Action::Resize(UVec2::new(1280, 720))),
                ("1920 × 1080", Action::Resize(UVec2::new(1920, 1080))),
                ("2560 × 1440", Action::Resize(UVec2::new(2560, 1440))),
                ("3840 × 2160", Action::Resize(UVec2::new(3840, 2160))),
                ("Toggle fullscreen · F11", Action::Fullscreen),
                ("Close settings · F10", Action::Close),
            ] {
                panel.spawn((
                    action,
                    RelativeCursorPosition::default(),
                    Node {
                        padding: UiRect::all(px(10)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.15, 0.19, 0.26)),
                    children![label(name, 18.0)],
                ));
            }
        });
}

fn label(text: &str, size: f32) -> impl Bundle {
    (
        Text::new(text),
        TextFont::from_font_size(size),
        TextColor(Color::WHITE),
        SettingsText,
    )
}

fn clicks(
    mouse: Res<ButtonInput<MouseButton>>,
    mut settings: ResMut<Settings>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    mut buttons: Query<(&Action, &RelativeCursorPosition, &mut BackgroundColor)>,
) {
    let mouse = mouse.into_inner();
    if !settings.open || !window.focused {
        return;
    }
    for (action, cursor, mut color) in &mut buttons {
        color.0 = if cursor.cursor_over() {
            Color::srgb(0.25, 0.32, 0.43)
        } else {
            Color::srgb(0.15, 0.19, 0.26)
        };
        if cursor.cursor_over() && mouse.just_pressed(MouseButton::Left) {
            match *action {
                Action::Resize(size) => settings.resize(&mut window, size),
                Action::Fullscreen => settings.toggle_fullscreen(&mut window),
                Action::Close => settings.open = false,
            }
            break;
        }
    }
}

fn controls(
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<Settings>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    mut panel: Single<&mut Node, With<Panel>>,
    mut status: Single<&mut Text, With<Status>>,
) {
    let keys = keys.into_inner();
    if window.focused && keys.just_pressed(KeyCode::F10) {
        settings.open = !settings.open;
    }
    if window.focused && keys.just_pressed(KeyCode::F11) {
        settings.toggle_fullscreen(&mut window);
    }
    let display = if settings.open {
        Display::Flex
    } else {
        Display::None
    };
    if panel.display != display {
        panel.display = display;
    }
    if settings.open {
        let mode = if settings.windowed.is_some() {
            "Fullscreen"
        } else {
            "Windowed"
        };
        status.set_if_neq(Text::new(format!(
            "{mode}: {} × {}",
            window.physical_width(),
            window.physical_height(),
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        settings.resize(&mut window, UVec2::new(2560, 1440));
        assert_eq!(window.mode, WindowMode::Windowed);
        settings.apply_windowed(&mut window);
        assert_eq!(window.resolution.physical_size(), UVec2::new(2560, 1440));
        settings.toggle_fullscreen(&mut window);
        settings.toggle_fullscreen(&mut window);
        settings.apply_windowed(&mut window);
        assert_eq!(window.resolution.physical_size(), UVec2::new(2560, 1440));
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
    fn fullscreen_sized_preset_applies_on_the_update_after_windowed_restore() {
        let saved_position = WindowPosition::At(IVec2::new(120, 80));
        let mut window = Window {
            position: saved_position,
            ..default()
        };
        let mut settings = Settings {
            open: true,
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
        let mut window = app.world_mut().get_mut::<Window>(entity).unwrap();
        assert_eq!(window.mode, WindowMode::Windowed);
        assert_eq!(window.resolution.physical_size(), UVec2::new(1920, 1080));
        window.resolution.set_physical_resolution(1280, 720);
        window.position = saved_position;
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .reset_all();
        app.update();
        let window = app.world().get::<Window>(entity).unwrap();
        assert_eq!(window.resolution.physical_size(), UVec2::new(1920, 1080));
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
        app.init_resource::<Settings>()
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
}
