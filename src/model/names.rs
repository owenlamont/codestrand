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
    pub placement: Option<Placement>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Placement {
    pub point: Vec2,
    pub body: Vec2,
    pub text: String,
    pub scale: f32,
}

impl super::State {
    pub(super) fn place_names(&mut self, dt: f32) {
        self.place_slot_names(dt);
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
                            avatar.height_band,
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

    #[test]
    fn identity_bands_are_stable_and_spread_across_heights() {
        let bands: Vec<_> = (0..32)
            .map(|index| height_band(&Key::Config(index)))
            .collect();
        assert!(bands.iter().all(|band| (0.0..=0.28).contains(band)));
        assert!(bands.contains(&0.0));
        assert!(bands.iter().any(|band| *band > 0.25));
    }
}
