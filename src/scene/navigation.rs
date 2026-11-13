use bevy::{
    ecs::message::MessageCursor,
    input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit},
    prelude::*,
    window::{CursorGrabMode, CursorOptions, PrimaryWindow, WindowFocused},
};

use crate::camera::Input;

#[derive(Default)]
pub(super) struct State {
    captured: bool,
    suppress_left: bool,
    panel_was_open: bool,
    drag_from: Option<Vec2>,
    focus_events: MessageCursor<WindowFocused>,
}

#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "input events can occur simultaneously"
)]
struct Controls {
    focused: bool,
    focus_lost: bool,
    panel_open: bool,
    panel_toggled: bool,
    escape: bool,
    toggle_capture: bool,
    click_viewport: bool,
}

impl State {
    fn transition(&mut self, controls: &Controls) -> bool {
        let was_captured = self.captured;
        let blocked = !controls.focused
            || controls.focus_lost
            || controls.panel_open
            || self.panel_was_open
            || controls.panel_toggled;
        self.panel_was_open = controls.panel_open;
        if blocked || controls.escape {
            self.captured = false;
        } else if controls.toggle_capture {
            self.captured = !self.captured;
        } else if controls.click_viewport {
            self.captured = true;
        }
        controls.focused && !controls.focus_lost && controls.escape && !was_captured
    }

    pub(super) fn read(&mut self, world: &mut World) -> Input {
        let panel_open = super::settings::captures_camera(world);
        let mut windows =
            world.query_filtered::<(Entity, &Window), With<PrimaryWindow>>();
        let Ok((entity, window)) = windows.single(world) else {
            self.captured = false;
            self.drag_from = None;
            return Input::default();
        };
        let cursor = window.cursor_position();
        let physical_cursor = window.physical_cursor_position();
        let focused = window.focused;
        let focus_lost =
            world
                .get_resource::<Messages<WindowFocused>>()
                .is_some_and(|events| {
                    self.focus_events
                        .read(events)
                        .any(|event| event.window == entity && !event.focused)
                });
        let in_viewport = physical_cursor.is_some_and(|position| {
            world
                .query_filtered::<&Camera, With<super::SceneCamera>>()
                .single(world)
                .ok()
                .and_then(|camera| camera.viewport.as_ref())
                .is_some_and(|viewport| {
                    Rect::from_corners(
                        viewport.physical_position.as_vec2(),
                        (viewport.physical_position + viewport.physical_size).as_vec2(),
                    )
                    .contains(position)
                })
        });
        let keys = world.resource::<ButtonInput<KeyCode>>();
        let controls = Controls {
            focused,
            focus_lost,
            panel_open,
            panel_toggled: keys.just_pressed(KeyCode::F10),
            escape: keys.just_pressed(KeyCode::Escape),
            toggle_capture: keys.just_pressed(KeyCode::KeyC),
            click_viewport: in_viewport
                && world
                    .resource::<ButtonInput<MouseButton>>()
                    .just_pressed(MouseButton::Left),
        };
        let was_captured = self.captured;
        let exit = self.transition(&controls);
        let left_held = world
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Left);
        self.suppress_left =
            left_held && (self.suppress_left || (self.captured && !was_captured));
        if let Some(mut options) = world.get_mut::<CursorOptions>(entity) {
            options.grab_mode = if self.captured {
                CursorGrabMode::Locked
            } else {
                CursorGrabMode::None
            };
            options.visible = !self.captured;
        }
        if exit {
            world.write_message(AppExit::Success);
        }
        if !controls.focused
            || controls.focus_lost
            || panel_open
            || controls.panel_toggled
            || controls.escape
            || self.captured != was_captured
        {
            self.drag_from = None;
            return Input::default();
        }
        self.camera_input(world, cursor)
    }

    fn camera_input(&mut self, world: &World, cursor: Option<Vec2>) -> Input {
        let previous = self.drag_from.take();
        let keys = world.resource::<ButtonInput<KeyCode>>();
        let axis = |positive, negative| {
            f32::from(u8::from(keys.pressed(positive)))
                - f32::from(u8::from(keys.pressed(negative)))
        };
        let mut input = Input {
            pan: Vec2::new(
                axis(KeyCode::KeyD, KeyCode::KeyA),
                axis(KeyCode::KeyW, KeyCode::KeyS),
            )
            .normalize_or_zero(),
            cycle: keys.just_pressed(KeyCode::Tab),
            reset: keys.just_pressed(KeyCode::Space),
            first_person: self.captured,
            ..default()
        };
        if self.captured {
            input.aim = world.resource::<AccumulatedMouseMotion>().delta;
            let mouse = world.resource::<ButtonInput<MouseButton>>();
            input.lift = f32::from(u8::from(mouse.pressed(MouseButton::Left)))
                - f32::from(u8::from(mouse.pressed(MouseButton::Right)));
            if self.suppress_left {
                input.lift = input.lift.min(0.0);
            }
        } else if world
            .resource::<ButtonInput<MouseButton>>()
            .pressed(MouseButton::Right)
        {
            // WSLg reports raw device motion as absolute positions, so orbit follows the cursor.
            if let (Some(from), Some(to)) = (previous, cursor) {
                input.orbit = to - from;
            }
            self.drag_from = cursor;
        }
        let scroll = world.resource::<AccumulatedMouseScroll>();
        input.zoom = scroll.delta.y
            * match scroll.unit {
                MouseScrollUnit::Line => 1.0,
                MouseScrollUnit::Pixel => 0.01,
            };
        input
    }
}

#[cfg(test)]
mod tests;
