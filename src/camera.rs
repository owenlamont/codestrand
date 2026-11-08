use bevy::{
    math::{Vec2, Vec3},
    prelude::Resource,
};

use crate::{
    contributors::Key,
    model::{self, Avatar},
};

const MIN_PITCH: f32 = 0.05;
const MAX_PITCH: f32 = std::f32::consts::FRAC_PI_2 - 0.05;

#[derive(Debug, PartialEq)]
enum Mode {
    Fit,
    Free,
    Follow {
        key: Key,
        name: String,
    },
    Reset {
        focus: Vec3,
        distance: f32,
        yaw: f32,
        pitch: f32,
        elapsed: f32,
    },
}

#[derive(Default)]
pub(super) struct Input {
    pub pan: Vec2,
    pub orbit: Vec2,
    pub zoom: f32,
    pub cycle: bool,
    pub reset: bool,
}

#[derive(Resource)]
pub(super) struct PreviewCamera {
    mode: Mode,
    focus: Vec3,
    distance: f32,
    yaw: f32,
    pitch: f32,
}

impl PreviewCamera {
    pub(super) fn new(view: (Vec3, f32)) -> Self {
        let direction = model::view_direction();
        Self {
            mode: Mode::Fit,
            focus: view.0,
            distance: view.1,
            yaw: direction.x.atan2(direction.z),
            pitch: direction.y.asin(),
        }
    }

    pub(super) fn update(
        &mut self,
        input: &Input,
        view: (Vec3, f32),
        avatars: &[Avatar],
        dt: f32,
    ) {
        if input.reset {
            self.mode = Mode::Reset {
                focus: self.focus,
                distance: self.distance,
                yaw: self.yaw,
                pitch: self.pitch,
                elapsed: 0.0,
            };
        } else {
            if input.cycle {
                self.cycle(avatars);
            }
            if input.pan != Vec2::ZERO
                || ((input.orbit != Vec2::ZERO || input.zoom != 0.0)
                    && matches!(self.mode, Mode::Fit | Mode::Reset { .. }))
            {
                self.mode = Mode::Free;
            }
        }
        match &mut self.mode {
            Mode::Fit => *self = Self::new(view),
            Mode::Reset {
                focus,
                distance,
                yaw,
                pitch,
                elapsed,
            } => {
                *elapsed += dt;
                let weight = (*elapsed / 0.5).min(1.0);
                let ease = weight * weight * (3.0 - 2.0 * weight);
                let default = Self::new(view);
                self.focus = focus.lerp(default.focus, ease);
                self.distance = *distance + (default.distance - *distance) * ease;
                let yaw_delta =
                    (default.yaw - *yaw).sin().atan2((default.yaw - *yaw).cos());
                self.yaw = *yaw + yaw_delta * ease;
                self.pitch = *pitch + (default.pitch - *pitch) * ease;
                if weight >= 1.0 {
                    *self = default;
                }
            }
            Mode::Free | Mode::Follow { .. } => {
                self.yaw -= input.orbit.x * 0.004;
                self.pitch =
                    (self.pitch + input.orbit.y * 0.004).clamp(MIN_PITCH, MAX_PITCH);
                self.distance =
                    (self.distance * (-input.zoom * 0.12).exp()).clamp(1.0, 20_000.0);
                if let Mode::Follow { key, .. } = &self.mode {
                    if let Some(avatar) = avatars
                        .iter()
                        .find(|avatar| &avatar.key == key && avatar.presence > 0.0)
                    {
                        self.focus =
                            self.focus.lerp(avatar.position, 1.0 - (-dt / 0.3).exp());
                    } else {
                        self.mode = Mode::Free;
                    }
                } else {
                    let right = Vec3::new(self.yaw.cos(), 0.0, -self.yaw.sin());
                    let forward = Vec3::new(-self.yaw.sin(), 0.0, -self.yaw.cos());
                    self.focus += (right * input.pan.x + forward * input.pan.y)
                        * self.distance
                        * 0.7
                        * dt.min(0.1);
                }
            }
        }
    }

    fn cycle(&mut self, avatars: &[Avatar]) {
        let visible: Vec<_> = avatars
            .iter()
            .filter(|avatar| avatar.presence > 0.0)
            .collect::<Vec<_>>();
        if visible.is_empty() {
            return;
        }
        let next = match &self.mode {
            Mode::Follow { key, .. } => visible
                .iter()
                .position(|avatar| &avatar.key == key)
                .map_or(0, |index| (index + 1) % visible.len()),
            _ => 0,
        };
        self.mode = following(visible[next]);
    }

    pub(super) fn pose(&self) -> (Vec3, Vec3) {
        let direction = if self.mode == Mode::Fit {
            model::view_direction()
        } else {
            Vec3::new(
                self.yaw.sin() * self.pitch.cos(),
                self.pitch.sin(),
                self.yaw.cos() * self.pitch.cos(),
            )
        };
        (self.focus + direction * self.distance, self.focus)
    }

    pub(super) fn caption(&self) -> String {
        match &self.mode {
            Mode::Fit => "Camera: fit".to_owned(),
            Mode::Free => "Camera: free".to_owned(),
            Mode::Follow { name, .. } => format!("Camera: follow {name}"),
            Mode::Reset { .. } => "Camera: returning to fit".to_owned(),
        }
    }
}

fn following(avatar: &Avatar) -> Mode {
    Mode::Follow {
        key: avatar.key.clone(),
        name: avatar.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contributors,
        model::{Options, Simulation},
        theme::Theme,
        timeline::{self, Event},
    };

    fn avatars() -> Simulation {
        simulation(&[("Ada", ""), ("Grace", "")])
    }

    fn simulation(authors: &[(&str, &str)]) -> Simulation {
        let events = authors
            .iter()
            .map(|(name, email)| Event {
                author: timeline::Identity {
                    name: name.as_bytes().to_vec(),
                    email: email.as_bytes().to_vec(),
                },
                repository: 0,
                contributor: None,
                commit: String::new(),
                seconds: 0,
                changes: Vec::new(),
            })
            .collect::<Vec<_>>();
        let mut simulation = Simulation::new(
            events,
            &Options::default(),
            &Theme::neon(),
            &contributors::Mappings::default(),
        );
        simulation.advance_to(2, 1);
        simulation
    }

    #[test]
    fn cycle_distinguishes_contributors_sharing_a_name() {
        let simulation =
            simulation(&[("Sam", "a@example.invalid"), ("Sam", "b@example.invalid")]);
        let avatars = simulation.avatars();
        let view = (Vec3::ZERO, 10.0);
        let mut camera = PreviewCamera::new(view);
        let input = Input {
            cycle: true,
            ..Input::default()
        };
        for index in [0, 1, 0] {
            camera.update(&input, view, avatars, 0.1);
            assert_eq!(camera.mode, following(&avatars[index]));
        }
    }

    #[test]
    fn default_tracks_fit_until_manual_input() {
        let mut camera = PreviewCamera::new((Vec3::ZERO, 10.0));
        let view = (Vec3::X, 20.0);
        camera.update(&Input::default(), view, &[], 0.1);
        assert_eq!(camera.mode, Mode::Fit);
        assert_eq!(camera.focus, view.0);
        assert_eq!(camera.distance, view.1);
        for input in [
            Input {
                pan: Vec2::X,
                ..Input::default()
            },
            Input {
                orbit: Vec2::X,
                ..Input::default()
            },
            Input {
                zoom: 1.0,
                ..Input::default()
            },
        ] {
            let mut camera = PreviewCamera::new(view);
            camera.update(&input, view, &[], 0.1);
            assert_eq!(camera.mode, Mode::Free);
        }
    }

    #[test]
    fn orbit_clamps_above_floor_and_below_pole() {
        let view = (Vec3::ZERO, 10.0);
        let mut camera = PreviewCamera::new(view);
        for (motion, pitch) in [(-10000.0, MIN_PITCH), (10000.0, MAX_PITCH)] {
            camera.update(
                &Input {
                    orbit: Vec2::new(0.0, motion),
                    ..Input::default()
                },
                view,
                &[],
                0.1,
            );
            assert_eq!(camera.pitch, pitch);
            let (eye, focus) = camera.pose();
            assert!(eye.y > 0.0);
            assert!(eye.distance(focus) > 9.99);
            assert!(eye.x.is_finite() && eye.z.is_finite());
        }
    }

    #[test]
    fn pan_is_planar_and_relative_to_yaw_and_zoom_is_bounded() {
        let view = (Vec3::ZERO, 10.0);
        let mut camera = PreviewCamera::new(view);
        camera.yaw = 0.0;
        camera.update(
            &Input {
                pan: Vec2::Y,
                ..Input::default()
            },
            view,
            &[],
            0.5,
        );
        assert!(camera.focus.abs_diff_eq(Vec3::new(0.0, 0.0, -0.7), 0.0001));
        camera.yaw = std::f32::consts::FRAC_PI_2;
        camera.update(
            &Input {
                pan: Vec2::X,
                ..Input::default()
            },
            view,
            &[],
            0.5,
        );
        assert!(camera.focus.abs_diff_eq(Vec3::new(0.0, 0.0, -1.4), 0.0001));
        for (zoom, distance) in [
            (1.0, 10.0 * (-0.12_f32).exp()),
            (10000.0, 1.0),
            (-10000.0, 20_000.0),
        ] {
            camera.update(
                &Input {
                    zoom,
                    ..Input::default()
                },
                view,
                &[],
                0.1,
            );
            assert!((camera.distance - distance).abs() < 0.0001);
        }
    }

    #[test]
    fn cycle_follows_visible_contributors_and_wraps() {
        let simulation = avatars();
        let avatars = simulation.avatars();
        assert_eq!(avatars.len(), 2);
        let view = (Vec3::ZERO, 10.0);
        let mut camera = PreviewCamera::new(view);
        let input = Input {
            cycle: true,
            ..Input::default()
        };
        camera.update(&input, view, &[], 0.1);
        assert_eq!(camera.mode, Mode::Fit);
        for index in [0, 1, 0] {
            camera.update(&input, view, avatars, 0.1);
            assert_eq!(camera.mode, following(&avatars[index]));
        }
        assert!(camera.focus.y > 0.0);
        assert!(camera.focus.y < avatars[0].position.y);
        let focus = camera.focus;
        camera.update(&Input::default(), view, &[], 0.1);
        assert_eq!(camera.focus, focus);
        assert_eq!(camera.mode, Mode::Free);
        camera.update(
            &Input {
                pan: Vec2::X,
                ..Input::default()
            },
            view,
            avatars,
            0.1,
        );
        assert_eq!(camera.mode, Mode::Free);
    }

    #[test]
    fn follow_keeps_identity_when_avatars_reorder_and_skips_inactive_targets() {
        let simulation = avatars();
        let mut avatars = simulation.avatars().to_vec();
        let view = (Vec3::ZERO, 10.0);
        let mut camera = PreviewCamera::new(view);
        camera.update(
            &Input {
                cycle: true,
                ..Input::default()
            },
            view,
            &avatars,
            0.1,
        );
        let target = following(&avatars[0]);
        avatars[0].position = Vec3::new(8.0, 4.0, -2.0);
        avatars.reverse();
        avatars[0].presence = 0.0;
        let start = camera.focus;
        camera.update(
            &Input {
                orbit: Vec2::X,
                zoom: 1.0,
                ..Input::default()
            },
            view,
            &avatars,
            0.3,
        );
        assert_eq!(camera.mode, target);
        assert!(camera.focus.abs_diff_eq(
            start.lerp(avatars[1].position, 1.0 - (-1.0_f32).exp()),
            0.0001
        ));
        camera.update(
            &Input {
                cycle: true,
                ..Input::default()
            },
            view,
            &avatars,
            0.1,
        );
        assert_eq!(camera.mode, target);
        camera.update(
            &Input {
                reset: true,
                ..Input::default()
            },
            view,
            &avatars,
            0.1,
        );
        assert!(matches!(camera.mode, Mode::Reset { .. }));
        camera.update(
            &Input {
                orbit: Vec2::X,
                ..Input::default()
            },
            view,
            &avatars,
            0.1,
        );
        assert_eq!(camera.mode, Mode::Free);
    }

    #[test]
    fn reset_finishes_within_half_a_second_while_playback_moves() {
        for start in [10, 60, 120] {
            let mut simulation = Simulation::new(
                crate::timeline::synthetic(),
                &Options::default(),
                &Theme::neon(),
                &contributors::Mappings::default(),
            );
            simulation.advance_to(start, 30);
            let initial_view = simulation.view();
            let mut camera = PreviewCamera::new(initial_view);
            camera.update(
                &Input {
                    orbit: Vec2::splat(50.0),
                    zoom: 2.0,
                    ..Input::default()
                },
                initial_view,
                simulation.avatars(),
                0.0,
            );
            let start_pose = camera.pose();
            camera.update(
                &Input {
                    reset: true,
                    ..Input::default()
                },
                initial_view,
                simulation.avatars(),
                0.0,
            );
            assert_eq!(camera.pose(), start_pose);
            for frame in 1..=15 {
                simulation.advance_to(start + frame, 30);
                camera.update(
                    &Input::default(),
                    simulation.view(),
                    simulation.avatars(),
                    1.0 / 30.0,
                );
            }
            assert_ne!(simulation.view(), initial_view);
            assert_eq!(camera.mode, Mode::Fit);
            assert_eq!(camera.pose(), PreviewCamera::new(simulation.view()).pose());
            simulation.advance_to(start + 16, 30);
            camera.update(
                &Input::default(),
                simulation.view(),
                simulation.avatars(),
                1.0 / 30.0,
            );
            assert_eq!(camera.pose(), PreviewCamera::new(simulation.view()).pose());
        }
    }

    #[test]
    fn stalled_frames_cap_pan_without_scaling_mouse_input() {
        let view = (Vec3::ZERO, 10.0);
        let input = Input {
            pan: Vec2::X,
            orbit: Vec2::splat(10.0),
            zoom: 1.0,
            ..Input::default()
        };
        let mut regular = PreviewCamera::new(view);
        let mut stalled = PreviewCamera::new(view);
        regular.update(&input, view, &[], 0.1);
        stalled.update(&input, view, &[], 5.0);
        assert_eq!(stalled.mode, Mode::Free);
        assert_eq!(stalled.pose(), regular.pose());
        assert!(stalled.focus.length() > 0.0);
        assert!(stalled.focus.length() < 0.7);
    }

    #[test]
    fn faded_follow_target_releases_camera_without_chasing_or_switching() {
        let simulation = avatars();
        let mut avatars = simulation.avatars().to_vec();
        let view = (Vec3::ZERO, 10.0);
        let mut camera = PreviewCamera::new(view);
        camera.update(
            &Input {
                cycle: true,
                ..Input::default()
            },
            view,
            &avatars,
            0.1,
        );
        let pose = camera.pose();
        avatars[0].presence = 0.0;
        avatars[0].position += Vec3::splat(100.0);
        assert!(avatars[1].presence > 0.0);
        camera.update(&Input::default(), view, &avatars, 0.1);
        assert_eq!(camera.mode, Mode::Free);
        assert_eq!(camera.pose(), pose);
    }

    #[test]
    fn reset_eases_then_resumes_exact_default_tracking() {
        let view = (Vec3::ZERO, 10.0);
        let mut camera = PreviewCamera::new(view);
        camera.update(
            &Input {
                pan: Vec2::X,
                orbit: Vec2::splat(50.0),
                zoom: 2.0,
                ..Input::default()
            },
            view,
            &[],
            1.0,
        );
        let old_focus = camera.focus;
        camera.update(
            &Input {
                reset: true,
                pan: Vec2::X,
                ..Input::default()
            },
            view,
            &[],
            0.1,
        );
        assert!(matches!(camera.mode, Mode::Reset { .. }));
        assert!(camera.focus.length() < old_focus.length());
        assert!(camera.focus.length() > 0.0);
        for _ in 0..4 {
            camera.update(&Input::default(), view, &[], 0.1);
        }
        assert_eq!(camera.mode, Mode::Fit);
        assert_eq!(camera.focus, view.0);
        assert_eq!(camera.distance, view.1);
        assert_eq!(camera.pose(), PreviewCamera::new(view).pose());
    }
}
