use std::f32::consts::{FRAC_PI_2, PI, TAU};

use bevy::{
    math::{Quat, Vec2, Vec3},
    prelude::Resource,
};

use crate::{
    contributors::Key,
    model::{self, Avatar},
};

pub(super) const EYE_FLOOR: f32 = model::framing::GLASS_TOP + 0.1;

#[derive(Debug, PartialEq)]
enum Mode {
    Fit,
    Free,
    Follow {
        key: Key,
        name: String,
    },
    Reset {
        eye: Vec3,
        distance: f32,
        yaw: f32,
        pitch: f32,
        elapsed: f32,
    },
}

#[derive(Default)]
pub(super) struct Input {
    pub pan: Vec2,
    pub lift: f32,
    pub aim: Vec2,
    pub first_person: bool,
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
        Self {
            mode: Mode::Fit,
            focus: view.0,
            distance: view.1,
            yaw: 0.0,
            pitch: -FRAC_PI_2,
        }
    }

    pub(super) fn update(
        &mut self,
        input: &Input,
        view: (Vec3, f32),
        avatars: &[Avatar],
        dt: f32,
    ) {
        let previous_eye = self.pose().0;
        let was_following = matches!(self.mode, Mode::Follow { .. });
        if input.reset {
            self.mode = Mode::Reset {
                eye: self.pose().0,
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
                || input.lift != 0.0
                || ((input.orbit != Vec2::ZERO
                    || input.aim != Vec2::ZERO
                    || input.zoom != 0.0)
                    && matches!(self.mode, Mode::Fit | Mode::Reset { .. }))
            {
                self.mode = Mode::Free;
            }
        }
        if was_following && self.mode == Mode::Free {
            self.focus = previous_eye - self.rotation() * Vec3::Z * self.distance;
        }
        let old_eye = self.pose().0;
        match &mut self.mode {
            Mode::Fit => *self = Self::new(view),
            Mode::Reset {
                eye,
                distance,
                yaw,
                pitch,
                elapsed,
            } => {
                *elapsed += dt;
                let weight = (*elapsed / 0.5).min(1.0);
                let ease = weight * weight * (3.0 - 2.0 * weight);
                let default = Self::new(view);
                self.distance = *distance + (default.distance - *distance) * ease;
                self.yaw = *yaw * (1.0 - ease);
                self.pitch = *pitch + (-FRAC_PI_2 - *pitch) * ease;
                let eye = eye.lerp(default.pose().0, ease);
                self.focus = eye - self.rotation() * Vec3::Z * self.distance;
                if weight >= 1.0 {
                    *self = default;
                }
            }
            Mode::Free | Mode::Follow { .. } => {
                let old_distance = self.distance;
                self.yaw = (self.yaw - input.orbit.x * 0.004 - input.aim.x * 0.003
                    + PI)
                    .rem_euclid(TAU)
                    - PI;
                self.pitch = (self.pitch + input.orbit.y * 0.004 - input.aim.y * 0.003)
                    .clamp(-FRAC_PI_2, FRAC_PI_2);
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
                        self.focus =
                            old_eye - self.rotation() * Vec3::Z * self.distance;
                    }
                } else {
                    self.move_free(input, old_eye, old_distance, dt);
                }
            }
        }
    }

    fn move_free(&mut self, input: &Input, old_eye: Vec3, old_distance: f32, dt: f32) {
        let back = self.rotation() * Vec3::Z;
        let orbit_eye = if input.first_person {
            old_eye
        } else {
            self.focus + back * old_distance
        };
        let eye = if orbit_eye.y < EYE_FLOOR {
            old_eye.with_y(EYE_FLOOR)
        } else {
            orbit_eye
        };
        let eye = eye + back * (self.distance - old_distance);
        self.focus = eye.with_y(eye.y.max(EYE_FLOOR)) - back * self.distance;
        let yaw = Quat::from_rotation_y(self.yaw);
        let movement = yaw * Vec3::new(input.pan.x, input.lift, -input.pan.y);
        self.focus +=
            movement.clamp_length_max(1.0) * self.distance * 0.7 * dt.min(0.1);
        let eye = self.focus + back * self.distance;
        self.focus.y += (EYE_FLOOR - eye.y).max(0.0);
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
        self.focus.y = self.focus.y.max(EYE_FLOOR + 0.001);
    }

    pub(super) fn pose(&self) -> (Vec3, Vec3) {
        let direction = if self.mode == Mode::Fit {
            model::view_direction()
        } else {
            self.rotation() * Vec3::Z
        };
        let distance = if matches!(self.mode, Mode::Follow { .. }) && direction.y < 0.0
        {
            self.distance
                .min((self.focus.y - EYE_FLOOR).max(0.0) / -direction.y)
        } else {
            self.distance
        };
        let eye = self.focus + direction * distance;
        (eye.with_y(eye.y.max(EYE_FLOOR)), self.focus)
    }

    pub(super) fn rotation(&self) -> Quat {
        Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(self.pitch)
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

    #[test]
    fn captured_lift_follows_world_y_at_wasd_speed() {
        let view = (Vec3::Y * 20.0, 10.0);
        for lift in [-1.0, 1.0] {
            let mut camera = PreviewCamera::new(view);
            camera.mode = Mode::Free;
            camera.yaw = 0.7;
            camera.pitch = -0.4;
            let eye = camera.pose().0;
            for step in [1.0, 2.0] {
                camera.update(
                    &Input {
                        first_person: true,
                        lift,
                        ..Default::default()
                    },
                    view,
                    &[],
                    0.1,
                );
                let displacement = camera.pose().0 - eye;
                assert!(displacement.abs_diff_eq(Vec3::Y * lift * 0.7 * step, 1e-5));
            }
        }
    }

    #[test]
    fn captured_descent_stops_at_clearance_and_lift_releases_follow() {
        let view = (Vec3::Y * EYE_FLOOR, 10.0);
        let mut camera = PreviewCamera::new(view);
        camera.mode = Mode::Free;
        camera.pitch = std::f32::consts::FRAC_PI_4;
        camera.focus = Vec3::Y * EYE_FLOOR - camera.rotation() * Vec3::Z * 10.0;
        let eye = camera.pose().0;
        camera.update(
            &Input {
                first_person: true,
                lift: -1.0,
                ..Default::default()
            },
            view,
            &[],
            0.1,
        );
        assert!((camera.pose().0.y - EYE_FLOOR).abs() < 1e-5);
        assert!(camera.pose().0.abs_diff_eq(eye, 1e-5));
        assert_eq!(
            camera.rotation(),
            Quat::from_rotation_x(std::f32::consts::FRAC_PI_4)
        );

        let simulation = avatars();
        camera.update(
            &Input {
                cycle: true,
                ..Default::default()
            },
            view,
            simulation.avatars(),
            1.0,
        );
        let eye = camera.pose().0;
        camera.update(
            &Input {
                first_person: true,
                lift: 1.0,
                ..Default::default()
            },
            view,
            simulation.avatars(),
            0.0,
        );
        assert_eq!(camera.mode, Mode::Free);
        assert!(camera.pose().0.abs_diff_eq(eye, 1e-5));
    }

    #[test]
    fn captured_aim_turns_about_the_eye_without_frame_scaling() {
        let view = (Vec3::Y * 20.0, 10.0);
        for dt in [0.0, 0.01, 0.1] {
            let mut camera = PreviewCamera::new(view);
            camera.mode = Mode::Free;
            camera.pitch = 0.0;
            let eye = camera.pose().0;
            let aim = Vec2::new(200.0, 200.0);
            camera.update(
                &Input {
                    aim,
                    first_person: true,
                    ..Default::default()
                },
                view,
                &[],
                dt,
            );
            let expected = Quat::from_rotation_y(-aim.x * 0.003)
                * Quat::from_rotation_x(-aim.y * 0.003);
            assert!(camera.rotation().abs_diff_eq(expected, 1e-6));
            assert!(camera.pose().0.abs_diff_eq(eye, 1e-5));
            assert!((camera.rotation().length() - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn captured_movement_follows_new_yaw_and_stays_above_the_floor() {
        let view = (Vec3::Y * 20.0, 10.0);
        for pan in [Vec2::X, Vec2::Y, Vec2::ONE.normalize()] {
            let mut camera = PreviewCamera::new(view);
            let eye = camera.pose().0;
            camera.update(
                &Input {
                    aim: Vec2::new(100.0, -200.0),
                    first_person: true,
                    pan,
                    ..Default::default()
                },
                view,
                &[],
                0.1,
            );
            let expected =
                Quat::from_rotation_y(camera.yaw) * Vec3::new(pan.x, 0.0, -pan.y) * 0.7;
            assert!(camera.pose().0.abs_diff_eq(eye + expected, 1e-5));
        }
        let mut camera = PreviewCamera::new((Vec3::ZERO, 2.0));
        for _ in 0..100 {
            camera.update(
                &Input {
                    first_person: true,
                    pan: Vec2::Y,
                    ..Default::default()
                },
                view,
                &[],
                0.1,
            );
            assert!(camera.pose().0.y >= EYE_FLOOR);
        }
    }

    #[test]
    fn captured_aim_keeps_follow_and_movement_releases_follow() {
        let simulation = avatars();
        let view = (Vec3::ZERO, 60.0);
        let mut camera = PreviewCamera::new(view);
        camera.update(
            &Input {
                cycle: true,
                ..Default::default()
            },
            view,
            simulation.avatars(),
            10.0,
        );
        camera.update(
            &Input {
                first_person: true,
                aim: Vec2::new(100.0, 200.0),
                ..Default::default()
            },
            view,
            simulation.avatars(),
            0.1,
        );
        assert_eq!(camera.mode, following(&simulation.avatars()[0]));
        assert!(
            camera
                .pose()
                .1
                .abs_diff_eq(simulation.avatars()[0].position, 1e-5)
        );
        let eye = camera.pose().0;
        camera.update(
            &Input {
                first_person: true,
                pan: Vec2::X,
                ..Default::default()
            },
            view,
            simulation.avatars(),
            0.0,
        );
        assert_eq!(camera.mode, Mode::Free);
        assert!(camera.pose().0.abs_diff_eq(eye, 1e-5));
    }

    #[test]
    fn horizontal_movement_uses_yaw_at_both_poles() {
        let view = (Vec3::Y * 20.0, 10.0);
        for pitch in [-FRAC_PI_2, 0.0, FRAC_PI_2] {
            for first_person in [false, true] {
                for (yaw, pan, expected) in [
                    (0.0, Vec2::X, Vec3::X),
                    (0.0, Vec2::Y, Vec3::NEG_Z),
                    (FRAC_PI_2, Vec2::X, Vec3::NEG_Z),
                    (FRAC_PI_2, Vec2::Y, Vec3::NEG_X),
                ] {
                    let mut camera = PreviewCamera::new(view);
                    camera.mode = Mode::Free;
                    camera.yaw = yaw;
                    camera.pitch = pitch;
                    let eye = camera.pose().0;
                    camera.update(
                        &Input {
                            pan,
                            first_person,
                            ..Default::default()
                        },
                        view,
                        &[],
                        0.1,
                    );
                    let displacement = camera.pose().0 - eye;
                    assert!(displacement.abs_diff_eq(expected * 0.7, 1e-5));
                    assert!(displacement.y.abs() < 1e-5);
                }
            }
        }
    }
    #[test]
    fn floor_turns_about_the_eye_and_follow_releases_without_a_jump() {
        let view = (Vec3::ZERO, 60.0);
        let mut camera = PreviewCamera::new(view);
        camera.update(
            &Input {
                orbit: Vec2::Y * 500.0,
                ..Default::default()
            },
            view,
            &[],
            0.1,
        );
        let eye = camera.pose().0;
        let rotation = camera.rotation();
        camera.update(
            &Input {
                orbit: Vec2::Y,
                ..Default::default()
            },
            view,
            &[],
            0.1,
        );
        assert!(camera.pose().0.abs_diff_eq(eye, 1e-5));
        assert!(rotation.angle_between(camera.rotation()) > 0.003);

        let simulation = avatars();
        for pan in [Vec2::ZERO, Vec2::X] {
            let mut camera = PreviewCamera::new(view);
            camera.update(
                &Input {
                    cycle: true,
                    orbit: Vec2::Y * 500.0,
                    ..Default::default()
                },
                view,
                simulation.avatars(),
                10.0,
            );
            let pose = camera.pose();
            let rotation = camera.rotation();
            camera.update(
                &Input {
                    pan,
                    ..Default::default()
                },
                view,
                &[],
                0.0,
            );
            assert_eq!(camera.mode, Mode::Free);
            assert!(camera.pose().0.abs_diff_eq(pose.0, 1e-5));
            assert_eq!(camera.rotation(), rotation);
        }
    }

    #[test]
    fn eye_floor_clears_every_theme_file_bound_and_glass() {
        for name in crate::theme::ThemeName::ALL {
            let theme = name.build();
            assert!(theme.geometry().file_max.y < EYE_FLOOR, "{name:?}");
        }
    }

    #[test]
    fn eye_floor_survives_orbit_zoom_pan_and_follow() {
        let view = (Vec3::ZERO, 60.0);
        let simulation = avatars();
        for (follow, direction) in
            [(false, -1.0), (false, 1.0), (true, -1.0), (true, 1.0)]
        {
            let mut camera = PreviewCamera::new(view);
            camera.update(
                &Input {
                    cycle: follow,
                    ..Default::default()
                },
                view,
                simulation.avatars(),
                0.1,
            );
            let mut previous = camera.rotation();
            for step in 0..4000 {
                camera.update(
                    &Input {
                        orbit: Vec2::new(direction * 0.7, direction * 4.0),
                        zoom: if step % 200 < 100 { 0.1 } else { -0.1 },
                        pan: if follow {
                            Vec2::ZERO
                        } else {
                            Vec2::new(direction, -direction)
                        },
                        ..Default::default()
                    },
                    view,
                    simulation.avatars(),
                    1.0 / 60.0,
                );
                let q = camera.rotation();
                let (eye, aim) = camera.pose();
                assert!(eye.y >= 1.2, "eye below file clearance: {eye:?}");
                assert!(eye.is_finite() && aim.is_finite() && q.is_finite());
                let [right, up, back] = [Vec3::X, Vec3::Y, Vec3::Z].map(|v| q * v);
                assert!((right.cross(up) - back).length() < 1e-5);
                assert!((q.length() - 1.0).abs() < 1e-5);
                assert!(previous.angle_between(q) < 0.017);
                assert!((eye - aim).normalize().abs_diff_eq(back, 1e-4));
                if follow {
                    assert_eq!(camera.mode, following(&simulation.avatars()[0]));
                    assert!(
                        aim.distance(simulation.avatars()[0].position) < 0.001
                            || step < 200
                    );
                }
                previous = q;
            }
        }
    }

    #[test]
    fn top_down_default_aims_at_the_field_centre() {
        let view = (Vec3::ZERO, model::framing::DISTANCE);
        let camera = PreviewCamera::new(view);
        let (eye, focus) = camera.pose();
        assert_eq!(focus, view.0);
        assert!((eye - focus).normalize().abs_diff_eq(Vec3::Y, 1e-6));
        assert_eq!(camera.yaw, 0.0);
        assert_eq!(camera.pitch, -FRAC_PI_2);
        assert_eq!(camera.rotation(), Quat::from_rotation_x(-FRAC_PI_2));
        assert!((camera.rotation() * Vec3::Y).abs_diff_eq(Vec3::NEG_Z, 1e-6));
    }

    #[test]
    fn top_down_free_pitch_and_yaw_orbit() {
        let view = (Vec3::ZERO, 60.0);
        let mut camera = PreviewCamera::new(view);
        let before = (camera.pose().0 - camera.pose().1).normalize();
        camera.update(
            &Input {
                orbit: Vec2::new(40.0, 100.0),
                ..Input::default()
            },
            view,
            &[],
            0.1,
        );
        assert_eq!(camera.mode, Mode::Free);
        assert!(
            ((camera.pose().0 - camera.pose().1).normalize().y - before.y).abs() > 0.05
        );
        let (eye, focus) = camera.pose();
        assert!((eye.distance(focus) - 60.0).abs() < 1e-4);
        assert!(eye.z.abs() > 0.1);
    }

    #[test]
    fn top_down_follow_pitch_and_yaw_keep_the_target() {
        let simulation = avatars();
        let avatars = simulation.avatars();
        let view = (Vec3::ZERO, 60.0);
        let mut camera = PreviewCamera::new(view);
        camera.update(
            &Input {
                cycle: true,
                ..Input::default()
            },
            view,
            avatars,
            0.1,
        );
        let before = (camera.pose().0 - camera.pose().1).normalize();

        camera.update(
            &Input {
                orbit: Vec2::new(40.0, 100.0),
                ..Input::default()
            },
            view,
            avatars,
            0.1,
        );
        assert_eq!(camera.mode, following(&avatars[0]));
        assert!(
            ((camera.pose().0 - camera.pose().1).normalize().y - before.y).abs() > 0.05
        );
        assert!((camera.pose().0 - camera.pose().1).x.abs() > 0.1);
        let (eye, focus) = camera.pose();
        assert!((eye.distance(focus) - 60.0).abs() < 1e-4);
        assert!(focus.distance(avatars[0].position) < avatars[0].position.length());
    }

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
    fn orbit_remains_finite_for_large_drags() {
        let view = (Vec3::ZERO, 10.0);
        let mut camera = PreviewCamera::new(view);
        for motion in [-10000.0, 10000.0] {
            camera.update(
                &Input {
                    orbit: Vec2::new(0.0, motion),
                    ..Input::default()
                },
                view,
                &[],
                0.1,
            );
            assert!(camera.rotation().is_finite());
            let (eye, focus) = camera.pose();
            assert!(eye.is_finite());
            assert!(eye.distance(focus) > 9.99);
            assert!(eye.x.is_finite() && eye.z.is_finite());
        }
    }

    #[test]
    fn pitch_clamps_and_world_yaw_still_turns_at_both_poles_in_every_mode() {
        let view = (Vec3::Y * 20.0, 10.0);
        let simulation = avatars();
        for (follow, captured) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            for direction in [-1.0, 1.0] {
                let mut camera = PreviewCamera::new(view);
                camera.update(
                    &Input {
                        cycle: follow,
                        ..Default::default()
                    },
                    view,
                    simulation.avatars(),
                    1.0,
                );
                let motion = Vec2::Y * direction * 10_000.0;
                let mut input = Input {
                    first_person: captured,
                    ..Default::default()
                };
                if captured {
                    input.aim = -motion;
                } else {
                    input.orbit = motion;
                }
                camera.update(&input, view, simulation.avatars(), 0.1);
                assert_eq!(camera.pitch, direction * FRAC_PI_2);
                camera.update(&input, view, simulation.avatars(), 0.1);
                assert_eq!(camera.pitch, direction * FRAC_PI_2);
                let before = camera.rotation();
                input.aim = Vec2::ZERO;
                input.orbit = Vec2::ZERO;
                if captured {
                    input.aim.x = 100.0;
                } else {
                    input.orbit.x = 100.0;
                }
                camera.update(&input, view, simulation.avatars(), 0.1);
                assert!(before.angle_between(camera.rotation()) > 0.29);
                assert_eq!(camera.pitch, direction * FRAC_PI_2);
                assert!(
                    (camera.rotation() * Vec3::NEG_Z)
                        .abs_diff_eq(Vec3::Y * direction, 1e-6)
                );
                let right = camera.rotation() * Vec3::X;
                assert!(right.abs_diff_eq(
                    Vec3::new(camera.yaw.cos(), 0.0, -camera.yaw.sin()),
                    1e-6
                ));
                assert_upright(&camera);
                assert_eq!(matches!(camera.mode, Mode::Follow { .. }), follow);
            }
        }
    }

    fn assert_upright(camera: &PreviewCamera) {
        let q = camera.rotation();
        assert!((-FRAC_PI_2..=FRAC_PI_2).contains(&camera.pitch));
        assert!((q * Vec3::X).y.abs() < 1e-6);
        assert!((q * Vec3::Y).y >= -1e-6);
        assert!((q.length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn arbitrary_aim_orbit_and_reset_sequences_never_introduce_roll() {
        let view = (Vec3::Y * 20.0, 10.0);
        let simulation = avatars();
        for follow in [false, true] {
            let mut camera = PreviewCamera::new(view);
            camera.update(
                &Input {
                    cycle: follow,
                    ..Default::default()
                },
                view,
                simulation.avatars(),
                1.0,
            );
            for aim in [
                Vec2::new(2000.0, -3000.0),
                Vec2::new(-1200.0, 400.0),
                Vec2::new(100.0, 5000.0),
            ] {
                for orbit in [Vec2::new(1800.0, 40.0), Vec2::new(-4000.0, -800.0)] {
                    camera.update(
                        &Input {
                            aim,
                            orbit,
                            ..Default::default()
                        },
                        view,
                        simulation.avatars(),
                        0.1,
                    );
                    assert_upright(&camera);
                }
            }
            let eye = camera.pose().0;
            let rotation = camera.rotation();
            camera.update(
                &Input {
                    reset: true,
                    ..Default::default()
                },
                view,
                simulation.avatars(),
                0.0,
            );
            assert!(camera.pose().0.abs_diff_eq(eye, 1e-5));
            assert_eq!(camera.rotation(), rotation);
            for _ in 0..31 {
                camera.update(
                    &Input::default(),
                    view,
                    simulation.avatars(),
                    1.0 / 60.0,
                );
                assert_upright(&camera);
            }
            assert_eq!(camera.yaw, 0.0);
            assert_eq!(camera.pitch, -FRAC_PI_2);
            assert!((camera.rotation() * Vec3::Y).abs_diff_eq(Vec3::NEG_Z, 1e-6));
        }
    }
    #[test]
    fn reset_from_floor_eases_to_overhead_without_a_jump() {
        let view = (Vec3::ZERO, 60.0);
        let mut camera = PreviewCamera::new(view);
        camera.update(
            &Input {
                orbit: Vec2::Y * (std::f32::consts::PI / 0.004),
                ..Input::default()
            },
            view,
            &[],
            0.1,
        );
        assert!((camera.pose().0.y - EYE_FLOOR).abs() < 1e-5);
        let before = camera.rotation();
        camera.update(
            &Input {
                reset: true,
                ..Input::default()
            },
            view,
            &[],
            0.0,
        );
        assert_eq!(before, camera.rotation());
        for _ in 0..31 {
            camera.update(&Input::default(), view, &[], 1.0 / 60.0);
            assert!(camera.pose().0.y >= EYE_FLOOR);
        }
        assert_eq!(camera.mode, Mode::Fit);
        assert_eq!(camera.rotation(), PreviewCamera::new(view).rotation());
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
        camera.yaw = FRAC_PI_2;
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
        assert!(camera.focus.abs_diff_eq(focus, 1e-5));
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
    fn reset_finishes_within_half_a_second_during_field_playback() {
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
            assert_eq!(simulation.view(), initial_view);
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
        assert!(camera.pose().0.abs_diff_eq(pose.0, 1e-5));
        assert!(camera.pose().1.abs_diff_eq(pose.1, 1e-5));
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
