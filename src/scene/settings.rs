use bevy::{
    camera::{CameraOutputMode, Viewport},
    input::mouse::{AccumulatedMouseScroll, MouseScrollUnit},
    prelude::*,
    ui::RelativeCursorPosition,
    ui_widgets::{ControlOrientation, Scrollbar, ScrollbarThumb},
    window::{
        Monitor, MonitorSelection, OnMonitor, PrimaryWindow, WindowMode, WindowPosition,
    },
};

use super::{AvatarCamera, SceneCamera};

mod presets;

type PreviewCameras = Or<(With<SceneCamera>, With<AvatarCamera>)>;

pub(super) struct PreviewSettingsPlugin;

impl Plugin for PreviewSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Settings>()
            .add_systems(Startup, setup.after(super::setup))
            .add_systems(PreUpdate, viewports.before(super::scale_ui))
            .add_systems(
                Update,
                (
                    monitor,
                    apply_windowed,
                    clicks,
                    controls,
                    viewports,
                    super::scale_ui,
                    scroll,
                )
                    .chain()
                    .before(super::preview_camera),
            );
    }
}

#[derive(Default, Resource)]
pub(super) struct Settings {
    open: bool,
    windowed: Option<(UVec2, WindowPosition)>,
    pending_windowed: Option<(UVec2, WindowPosition)>,
    monitor: Option<UVec2>,
    chosen: Option<UVec2>,
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
            self.chosen = None;
        }
    }

    fn resize(&mut self, window: &mut Window, size: UVec2) {
        if self.windowed.is_some() {
            self.chosen = Some(size);
            return;
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
            let size = self.monitor.map_or(size, |monitor| size.min(monitor));
            window.resolution.set_physical_resolution(size.x, size.y);
        }
    }

    pub(super) fn logical_height(&self, window: &Window) -> f32 {
        self.rectangle(window).size().as_vec2().y / window.scale_factor()
    }

    fn rectangle(&self, window: &Window) -> URect {
        let chosen = self.windowed.and(self.chosen);
        presets::rectangle(window.resolution.physical_size(), chosen)
    }
}

fn monitor(
    window: Single<Option<&OnMonitor>, With<PrimaryWindow>>,
    monitors: Query<&Monitor>,
    mut settings: ResMut<Settings>,
) {
    settings.monitor = window
        .into_inner()
        .and_then(|on| monitors.get(on.0).ok())
        .map(Monitor::physical_size);
    if let (Some(size), Some(chosen)) = (settings.monitor, settings.chosen)
        && !presets::fits(chosen, size)
    {
        settings.chosen = None;
    }
}

fn viewports(
    settings: Res<Settings>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut cameras: Query<(&mut Camera, Has<SceneCamera>), PreviewCameras>,
) {
    let rect = settings.into_inner().rectangle(window.into_inner());
    if rect.is_empty() {
        return;
    }
    for (mut camera, scene) in &mut cameras {
        let viewport = Some(Viewport {
            physical_position: rect.min,
            physical_size: rect.size(),
            ..default()
        });
        if camera.viewport.as_ref().is_none_or(|current| {
            current.physical_position != rect.min
                || current.physical_size != rect.size()
        }) {
            camera.viewport = viewport;
        }
        camera.output_mode = CameraOutputMode::Write {
            blend_state: None,
            clear_color: if scene {
                ClearColorConfig::Custom(Color::BLACK)
            } else {
                ClearColorConfig::None
            },
        };
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
struct PresetList;

#[derive(Component)]
pub(super) struct SettingsText;

#[derive(Component, Clone, Copy)]
enum Action {
    Resize(UVec2),
    Native,
    Fullscreen,
    Close,
}

fn describe(action: Action, monitor: Option<UVec2>) -> (bool, Option<String>) {
    let size = match action {
        Action::Resize(size) => Some(size),
        Action::Native => monitor,
        _ => return (true, None),
    };
    let Some(size) = size else {
        return (false, Some("Monitor native · detecting monitor".to_owned()));
    };
    let enabled = monitor.is_some_and(|monitor| presets::fits(size, monitor));
    let prefix = if matches!(action, Action::Native) {
        "Monitor native · "
    } else {
        ""
    };
    let reason = if enabled {
        ""
    } else if monitor.is_none() {
        "\nDetecting monitor"
    } else {
        "\nExceeds monitor size"
    };
    (
        enabled,
        Some(format!("{prefix}{}{reason}", presets::label(size))),
    )
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
                width: px(420),
                max_width: percent(95),
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
            panel.spawn(label("Render size · physical pixels", 16.0));
            panel
                .spawn(Node {
                    height: px(390),
                    column_gap: px(8),
                    ..default()
                })
                .with_children(setup_presets);
            for (name, action) in [
                ("Toggle fullscreen · F11", Action::Fullscreen),
                ("Close settings · F10", Action::Close),
            ] {
                panel.spawn(button(name, action));
            }
        });
}

fn button(name: &str, action: Action) -> impl Bundle {
    (
        action,
        RelativeCursorPosition::default(),
        Node {
            padding: UiRect::all(px(8)),
            flex_shrink: 0.0,
            ..default()
        },
        BackgroundColor(Color::srgb(0.15, 0.19, 0.26)),
        children![label(name, 16.0)],
    )
}

fn scroll(
    settings: Res<Settings>,
    wheel: Res<AccumulatedMouseScroll>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<&Camera, With<AvatarCamera>>,
    panel: Single<(&ComputedNode, &UiGlobalTransform), With<Panel>>,
    mut list: Single<(&mut ScrollPosition, &ComputedNode), With<PresetList>>,
) {
    let settings = settings.into_inner();
    let wheel = wheel.into_inner();
    let window = window.into_inner();
    let camera = camera.into_inner();
    let (node, transform) = panel.into_inner();
    let offset = camera
        .viewport
        .as_ref()
        .map_or(Vec2::ZERO, |viewport| viewport.physical_position.as_vec2());
    if settings.open
        && window.focused
        && window
            .physical_cursor_position()
            .is_some_and(|cursor| node.contains_point(*transform, cursor - offset))
    {
        let (ref mut position, node) = *list;
        let maximum = ((node.content_size().y - node.size().y)
            * node.inverse_scale_factor())
        .max(0.0);
        let step = match wheel.unit {
            MouseScrollUnit::Line => 32.0,
            MouseScrollUnit::Pixel => 1.0,
        };
        position.y = (position.y - wheel.delta.y * step).clamp(0.0, maximum);
    }
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
    mut buttons: Query<(
        &Action,
        &RelativeCursorPosition,
        &mut BackgroundColor,
        Option<&Children>,
    )>,
    mut texts: Query<(&mut Text, &mut TextColor), With<SettingsText>>,
) {
    let mouse = mouse.into_inner();
    if !settings.open || !window.focused {
        return;
    }
    for (action, cursor, mut color, children) in &mut buttons {
        let (enabled, caption) = describe(*action, settings.monitor);
        let ink_color = if enabled {
            Color::WHITE
        } else {
            Color::srgb(0.5, 0.5, 0.5)
        };
        for child in children.into_iter().flatten() {
            let Ok((mut text, mut ink)) = texts.get_mut(*child) else {
                continue;
            };
            ink.0 = ink_color;
            if let Some(caption) = &caption {
                text.set_if_neq(Text::new(caption));
            }
        }
        color.0 = if !enabled {
            Color::srgb(0.09, 0.10, 0.12)
        } else if cursor.cursor_over() {
            Color::srgb(0.25, 0.32, 0.43)
        } else {
            Color::srgb(0.15, 0.19, 0.26)
        };
        if enabled && cursor.cursor_over() && mouse.just_pressed(MouseButton::Left) {
            match *action {
                Action::Resize(size) => settings.resize(&mut window, size),
                Action::Native => {
                    settings.chosen = None;
                    if settings.windowed.is_none()
                        && let Some(size) = settings.monitor
                    {
                        settings.resize(&mut window, size);
                    }
                }
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
        let size = settings.rectangle(&window).size();
        let mode = if settings.windowed.is_some() {
            "Fullscreen"
        } else {
            "Windowed"
        };
        status.set_if_neq(Text::new(format!("{mode}: {} × {}", size.x, size.y)));
    }
}

#[cfg(test)]
mod tests;

fn setup_presets(row: &mut ChildSpawnerCommands) {
    let list = row
        .spawn((
            PresetList,
            ScrollPosition::default(),
            Node {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                min_width: px(0),
                overflow: Overflow::scroll_y(),
                row_gap: px(4),
                ..default()
            },
        ))
        .with_children(|list| {
            list.spawn(button("Monitor native", Action::Native));
            for &(width, height, _) in presets::PRESETS {
                let size = UVec2::new(width, height);
                list.spawn(button(&presets::label(size), Action::Resize(size)));
            }
        })
        .id();
    row.spawn((
        Node {
            width: px(14),
            flex_shrink: 0.0,
            ..default()
        },
        BackgroundColor(Color::srgb(0.10, 0.12, 0.16)),
        Scrollbar {
            target: list,
            orientation: ControlOrientation::Vertical,
            min_thumb_length: 24.0,
        },
        children![(
            ScrollbarThumb::default(),
            BackgroundColor(Color::srgb(0.5, 0.6, 0.7))
        )],
    ));
}
