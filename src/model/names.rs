use bevy::math::{Rect, Vec2};

pub(super) fn scale(active: usize) -> f32 {
    (1.0 / (1.0 + super::count(active.saturating_sub(3)) * 0.035).sqrt()).max(0.72)
}

pub(super) fn elide(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    text.chars()
        .take(limit.saturating_sub(1))
        .chain(['…'])
        .collect()
}

pub(crate) fn rect(point: Vec2, text: &str, scale: f32, height: f32) -> Rect {
    let width = 8.0 + 8.6 * super::count(text.chars().count()) * scale;
    Rect::from_corners(point, point + Vec2::new(width, height * scale + 4.0))
}

#[derive(Clone, Debug, Default)]
pub(super) struct Visibility {
    pub joint_exhausted: bool,
    pub joint_clear: bool,
    pub opacity: f32,
    pub visible: bool,
    pub placement: Option<Placement>,
    pub full_clear_since: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Placement {
    pub point: Vec2,
    pub body: Vec2,
    pub angle: f32,
    pub angular_velocity: f32,
    pub commit: usize,
    pub text: String,
    pub scale: f32,
    pub motion: Motion,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Motion {
    pub goal: f32,
    pub pending: Option<(u8, u64)>,
    pub direction: i8,
    pub reversal: Option<u64>,
}

impl super::State {
    pub(super) fn place_names(
        &mut self,
        dt: f32,
        travel: &super::travel::Plan,
        texts: &[Rect],
    ) {
        #[cfg(test)]
        if super::slots::name_oracle::place_checked(self, dt, travel, texts) {
            return;
        }
        self.place_slot_names(dt, travel, texts);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn simulation() -> super::super::Simulation {
        let events = (0..24)
            .map(|index| {
                let mut event = crate::timeline::synthetic().remove(0);
                event.author.name = format!("Contributor {index}").into_bytes();
                event.author.email = format!("author{index}@example.org").into_bytes();
                event.seconds = index;
                event
            })
            .collect();
        super::super::Simulation::new(
            events,
            &super::super::Options {
                max_idle_seconds: 0.05,
                ..Default::default()
            },
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        )
    }

    #[test]
    fn small_persistent_name_overlaps_clear_at_the_normal_turning_speed() {
        let mut sim = simulation();
        sim.advance_to(180, 60);
        sim.state.avatars.truncate(2);
        sim.state.pulses.clear();
        let scale = sim.state.name_scale();
        let size =
            rect(Vec2::ZERO, "Contributor 0", scale, sim.theme().label_height).size();
        let distance = sim.state.avatar_radius() + 4.0 / 3.0;
        for (index, avatar) in sim.state.avatars.iter_mut().enumerate() {
            let body =
                Vec2::new((super::super::count(index) - 0.5) * size.x * 0.9, 280.0);
            avatar.position = super::super::slots::world(body);
            avatar.name = format!("Contributor {index}");
            avatar.presence = 1.0;
            let angle = std::f32::consts::FRAC_PI_2;
            let bounds =
                super::super::slots::labels::tangent_rect(body, size, distance, angle);
            avatar.name_visibility.placement = Some(Placement {
                point: Vec2::new(bounds.min.x, bounds.max.y),
                body,
                angle,
                angular_velocity: 0.0,
                commit: 0,
                text: avatar.name.clone(),
                scale,
                motion: Motion {
                    goal: angle,
                    ..Default::default()
                },
            });
        }
        let travel = super::super::travel::Plan::default();
        for _ in 0..300 {
            let angles: Vec<_> = sim
                .state
                .avatars
                .iter()
                .map(|a| a.name_visibility.placement.as_ref().unwrap().angle)
                .collect();
            sim.state.tick += 1;
            sim.state.place_names(1.0 / 60.0, &travel, &[]);
            for (avatar, angle) in sim.state.avatars.iter().zip(angles) {
                assert!(
                    (avatar.name_visibility.placement.as_ref().unwrap().angle - angle)
                        .abs()
                        <= 30.0_f32.to_radians() / 60.0 + 1e-6
                );
            }
        }
        let bounds: Vec<_> = sim
            .state
            .avatars
            .iter()
            .map(|a| {
                super::super::titles::label_bounds(&sim, &sim.state.avatar_label(a))
            })
            .collect();
        assert!(
            bounds[0].intersect(bounds[1]).is_empty(),
            "persistent overlap: {bounds:?}"
        );
    }

    #[test]
    fn crowded_long_names_elide_only_after_full_names_cannot_fit() {
        let mut sim = simulation();
        sim.advance_to(180, 60);
        sim.state.avatars.truncate(7);
        sim.state.pulses.clear();
        for (index, avatar) in sim.state.avatars.iter_mut().enumerate() {
            avatar.position = super::super::slots::world(Vec2::new(
                -400.0 + super::super::count(index) * 160.0,
                300.0,
            ));
            avatar.name = format!("Contributor {index} {}", "long name ".repeat(20));
            avatar.presence = 1.0;
            avatar.name_visibility.placement = None;
        }
        let quiet = sim.state.avatars.last_mut().unwrap();
        quiet.name = "Uncrowded contributor".into();
        quiet.position = super::super::slots::world(Vec2::new(0.0, -200.0));
        sim.state.place_names(1.0 / 60.0, &sim.travel, &[]);
        let names: Vec<_> = sim
            .state
            .avatars
            .iter()
            .map(|a| sim.state.avatar_label(a))
            .collect();
        assert!(names.iter().any(|l| l.text.ends_with('…')));
        assert_eq!(names.last().unwrap().text, "Uncrowded contributor");
        let rectangles: Vec<_> = names
            .iter()
            .map(|l| super::super::titles::label_bounds(&sim, l))
            .collect();
        for (index, rect) in rectangles.iter().enumerate() {
            assert_eq!(super::super::caption::outside(*rect, 0.0), 0.0);
            assert!(
                rectangles[..index]
                    .iter()
                    .all(|other| rect.intersect(*other).is_empty())
            );
        }
        sim.state.place_names(1.0 / 60.0, &sim.travel, &[]);
        assert_eq!(
            names.iter().map(|l| &l.text).collect::<Vec<_>>(),
            sim.state
                .avatars
                .iter()
                .map(|a| &a.name_visibility.placement.as_ref().unwrap().text)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn every_visible_avatar_keeps_a_label_when_crowded() {
        let mut sim = simulation();
        for frame in 0..300 {
            sim.advance_to(frame, 60);
            for avatar in sim.avatars().iter().filter(|a| a.presence > 0.0) {
                let label = sim.state.avatar_label(avatar);
                assert!(label.opacity > 0.0 && !label.text.is_empty());
            }
        }
    }
    #[test]
    fn crowd_state_replays_at_different_frame_rates_and_after_rewind() {
        let mut sim = simulation();
        sim.advance_to(90, 30);
        let snapshot = |sim: &super::super::Simulation| {
            (
                sim.avatar_scale(),
                sim.avatars()
                    .iter()
                    .map(|avatar| {
                        (
                            avatar.position,
                            avatar.activity,
                            avatar.name_visibility.opacity,
                            avatar.name_visibility.placement.clone(),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let expected = snapshot(&sim);
        assert!(sim.avatar_scale() < 1.0);
        sim.advance_to(240, 60);
        sim.advance_to(180, 60);
        assert_eq!(snapshot(&sim), expected);
        sim.restart();
        for frame in 0..=72 {
            sim.advance_to(frame, 24);
        }
        assert_eq!(snapshot(&sim), expected);
    }

    #[test]
    fn crowd_scale_is_continuous_bounded_and_preserves_small_groups() {
        assert_eq!(scale(1), 1.0);
        assert_eq!(scale(3), 1.0);
        assert!(scale(20) < scale(10));
        assert!(scale(1000) >= 0.72);
        assert!((scale(4) - scale(3)).abs() < 0.02);
    }

    fn shortened_name() -> super::super::Simulation {
        let mut sim = simulation();
        sim.advance_to(180, 60);
        sim.state.avatars.truncate(1);
        sim.state.pulses.clear();
        let avatar = &mut sim.state.avatars[0];
        avatar.position = super::super::slots::world(Vec2::ZERO);
        avatar.name = "Ada Lovelace".into();
        avatar.presence = 1.0;
        avatar.name_visibility.placement = None;
        sim.state.place_names(1.0 / 60.0, &sim.travel, &[]);
        let placement = sim.state.avatars[0]
            .name_visibility
            .placement
            .as_mut()
            .unwrap();
        assert_eq!(placement.text, "Ada Lovelace");
        placement.text = "Ada…".into();
        sim
    }

    fn full_name_rect(sim: &super::super::Simulation) -> Rect {
        let avatar = &sim.state.avatars[0];
        let placement = avatar.name_visibility.placement.as_ref().unwrap();
        super::super::slots::labels::tangent_rect(
            placement.body,
            rect(
                Vec2::ZERO,
                &avatar.name,
                sim.state.name_scale(),
                sim.state.geometry.label_height,
            )
            .size(),
            sim.state.avatar_radius() + 4.0 / 3.0,
            placement.angle,
        )
    }

    #[test]
    fn elided_names_recover_after_a_sustained_clear_interval() {
        let mut sim = shortened_name();
        for _ in 0..22 {
            sim.state.tick += 1;
            sim.state.place_names(1.0 / 60.0, &sim.travel, &[]);
        }
        let full = full_name_rect(&sim);
        let blocker =
            Rect::from_corners(full.max - Vec2::new(1.0, full.height()), full.max);
        sim.state.tick += 1;
        sim.state.place_names(1.0 / 60.0, &sim.travel, &[blocker]);
        for _ in 0..45 {
            sim.state.tick += 1;
            sim.state.place_names(1.0 / 60.0, &sim.travel, &[]);
            assert_eq!(
                sim.state.avatars[0]
                    .name_visibility
                    .placement
                    .as_ref()
                    .unwrap()
                    .text,
                "Ada…"
            );
        }
        sim.state.tick += 1;
        sim.state.place_names(1.0 / 60.0, &sim.travel, &[]);
        assert_eq!(
            sim.state.avatars[0]
                .name_visibility
                .placement
                .as_ref()
                .unwrap()
                .text,
            "Ada Lovelace"
        );
    }

    #[test]
    fn elided_names_do_not_flicker_at_the_frame_boundary() {
        let mut sim = shortened_name();
        let boundary = 640.0 - 0.5 - full_name_rect(&sim).max.x;
        for tick in 0..180 {
            let offset = if tick % 2 == 0 { -1.0 } else { 1.0 };
            sim.state.avatars[0].position =
                super::super::slots::world(Vec2::new(boundary + offset, 0.0));
            sim.state.tick += 1;
            sim.state.place_names(1.0 / 60.0, &sim.travel, &[]);
            assert_eq!(
                sim.state.avatars[0]
                    .name_visibility
                    .placement
                    .as_ref()
                    .unwrap()
                    .text,
                "Ada…"
            );
            assert_eq!(
                super::super::caption::outside(full_name_rect(&sim), 0.5) == 0.0,
                offset < 0.0
            );
        }
    }
}
