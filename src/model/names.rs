use bevy::math::{Rect, Vec2};

use crate::contributors::Key;

pub(super) fn priority(activity: f32, visible: bool) -> f32 {
    activity * if visible { 1.25 } else { 1.0 }
}

pub(super) fn scale(active: usize) -> f32 {
    (1.0 / (1.0 + super::count(active.saturating_sub(3)) * 0.035).sqrt()).max(0.72)
}

pub(super) fn height_band(key: &Key) -> f32 {
    let hash_byte =
        |hash: u32, byte: &u8| (hash ^ u32::from(*byte)).wrapping_mul(16_777_619);
    let hash = match key {
        Key::Email(bytes) | Key::Name(bytes) => {
            bytes.iter().fold(2_166_136_261, hash_byte)
        }
        Key::Config(index) => index.to_le_bytes().iter().fold(2_166_136_261, hash_byte),
    };
    f32::from(u8::try_from(hash % 8).expect("eight height bands")) * 0.04
}

pub(crate) fn rect(point: Vec2, text: &str, scale: f32, height: f32) -> Rect {
    let width = 8.0 + 8.6 * super::count(text.chars().count()) * scale;
    Rect::from_corners(point, point + Vec2::new(width, height * scale + 4.0))
}

#[derive(Clone, Debug, Default)]
pub(super) struct Visibility {
    pub opacity: f32,
    pub visible: bool,
    clear: f32,
    fade: f32,
}

impl Visibility {
    pub fn step(&mut self, selected: bool, dt: f32) {
        if selected {
            self.clear += dt;
            if self.clear >= 0.35 {
                self.visible = true;
            }
        } else {
            self.clear = 0.0;
            self.visible = false;
        }
        let direction = if self.visible { 1.0 } else { -1.0 };
        self.fade = (self.fade + direction * dt / 0.25).clamp(0.0, 1.0);
        self.opacity = self.fade * self.fade * (3.0 - 2.0 * self.fade);
    }
}

impl super::State {
    pub(super) fn place_names(&mut self, dt: f32) {
        if !self.layout.repositories.is_empty() {
            self.place_slot_names(dt);
            return;
        }
        let positions: Vec<_> = self
            .avatars
            .iter()
            .map(|avatar| avatar.position + self.geometry.avatar_label_offset)
            .collect();
        let eye = self.focus + super::view_direction() * self.distance;
        let mut candidates: Vec<_> = super::project(eye, self.focus, &positions)
            .enumerate()
            .filter_map(|(index, point)| {
                let avatar = &self.avatars[index];
                (avatar.presence > 0.0).then_some((index, point?))
            })
            .map(|(index, point)| {
                let point = Vec2::new((point.x + 1.0) * 640.0, (1.0 - point.y) * 360.0);
                let avatar = &self.avatars[index];
                let bounds = rect(
                    point,
                    &avatar.name,
                    self.avatar_scale,
                    self.geometry.label_height,
                );
                (
                    index,
                    bounds,
                    priority(avatar.activity, avatar.name_visibility.visible),
                )
            })
            .collect();
        candidates.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
        let mut placed = Vec::new();
        let mut selected = vec![false; self.avatars.len()];
        for (index, bounds, _) in candidates {
            if placed.len() < 72
                && bounds.min.cmpge(Vec2::ZERO).all()
                && bounds.max.cmple(Vec2::new(1280.0, 720.0)).all()
                && !placed
                    .iter()
                    .any(|other: &Rect| !other.intersect(bounds).is_empty())
            {
                selected[index] = true;
                placed.push(bounds);
            }
        }
        for (avatar, selected) in self.avatars.iter_mut().zip(selected) {
            avatar.name_visibility.step(selected, dt);
        }
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
    fn recent_activity_wins_collisions_and_priority_changes_crossfade() {
        let mut sim = simulation();
        sim.advance_to(120, 60);
        for avatar in &mut sim.state.avatars {
            avatar.position = sim.state.focus;
            avatar.presence = 1.0;
            avatar.activity = 1.0;
        }
        sim.state.avatars[7].activity = 10.0;
        for _ in 0..120 {
            sim.state.place_names(1.0 / 60.0);
        }
        assert!(sim.state.avatars[7].name_visibility.opacity > 0.99);
        assert!(sim.state.avatars[0].name_visibility.opacity < 0.001);
        sim.state.avatars[0].activity = 20.0;
        sim.state.place_names(1.0 / 60.0);
        assert!(sim.state.avatars[7].name_visibility.opacity > 0.9);
        for _ in 0..120 {
            sim.state.place_names(1.0 / 60.0);
        }
        assert!(sim.state.avatars[0].name_visibility.opacity > 0.99);
        assert!(sim.state.avatars[7].name_visibility.opacity < 0.001);
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
                            avatar.height_band,
                            avatar.activity,
                            avatar.name_visibility.opacity,
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

    #[test]
    fn identity_bands_are_stable_and_spread_across_heights() {
        let bands: Vec<_> = (0..32)
            .map(|index| height_band(&Key::Config(index)))
            .collect();
        assert!(bands.iter().all(|band| (0.0..=0.28).contains(band)));
        assert!(bands.contains(&0.0));
        assert!(bands.iter().any(|band| *band > 0.25));
    }

    #[test]
    fn collisions_fade_and_repeated_clearance_is_required_to_reappear() {
        let mut visibility = Visibility::default();
        for _ in 0..120 {
            visibility.step(true, 1.0 / 60.0);
        }
        assert!(visibility.opacity > 0.99);
        visibility.step(false, 1.0 / 60.0);
        assert!(visibility.opacity > 0.9);
        for _ in 0..120 {
            visibility.step(true, 1.0 / 60.0);
            visibility.step(false, 1.0 / 60.0);
        }
        assert!(!visibility.visible);
        assert!(visibility.opacity < 0.001);
        for _ in 0..120 {
            visibility.step(true, 1.0 / 60.0);
        }
        assert!(visibility.visible);
        assert!(visibility.opacity > 0.99);
    }

    #[test]
    fn full_label_pool_replacements_finish_fading_before_new_names_appear() {
        let mut old = Visibility::default();
        let mut new = Visibility::default();
        for _ in 0..120 {
            old.step(true, 1.0 / 60.0);
        }
        for _ in 0..120 {
            old.step(false, 1.0 / 60.0);
            new.step(true, 1.0 / 60.0);
            assert!(old.opacity == 0.0 || new.opacity == 0.0);
        }
        assert_eq!(new.opacity, 1.0);
    }
}
