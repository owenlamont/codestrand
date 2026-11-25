use std::sync::Arc;

use bevy::math::Vec2;

use super::Tile;

const STABLE_TICKS: u64 = 60;
const RESHAPE_TICKS: u16 = 120;
const RESHAPE_DURATION: u64 = RESHAPE_TICKS as u64;

#[derive(Clone, Debug)]
struct Change {
    start: u64,
    stop: u64,
    from: Vec2,
    to: Vec2,
    aspect: f32,
}

impl Change {
    fn sample(&self, tick: u64) -> Vec2 {
        let elapsed = tick.min(self.stop).saturating_sub(self.start);
        let t = f32::from(u16::try_from(elapsed).expect("bounded reshape"))
            / f32::from(RESHAPE_TICKS);
        self.from.lerp(self.to, t * t * (3.0 - 2.0 * t))
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Plan {
    tick: u64,
    tracks: Arc<[Track]>,
}

impl Plan {
    pub(super) fn build(
        repositories: usize,
        end: u64,
        checkpoint: Option<&Self>,
        mut frame: impl FnMut(u64, &mut [Track]),
    ) -> Self {
        let start = checkpoint.map_or(0, |plan| plan.tick + 1);
        let mut tracks = checkpoint.map_or_else(
            || vec![Track::default(); repositories],
            |plan| plan.tracks.to_vec(),
        );
        for tick in start..=end {
            frame(tick, &mut tracks);
        }
        Self {
            tick: end.max(start.saturating_sub(1)),
            tracks: tracks.into(),
        }
    }

    pub(super) fn sample(&self, repository: usize, tick: u64) -> Vec2 {
        self.tracks.get(repository).map_or(Vec2::ONE, |track| {
            let next = track.changes.partition_point(|change| change.start <= tick);
            next.checked_sub(1)
                .map_or(Vec2::ONE, |last| track.changes[last].sample(tick))
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Track {
    reference: Option<Tile>,
    stable_since: u64,
    settled_aspect: Option<f32>,
    changes: Arc<Vec<Change>>,
}

impl Track {
    pub(super) fn observe(
        &mut self,
        tick: u64,
        tile: Option<Tile>,
        target: impl FnOnce(Tile) -> Vec2,
    ) {
        if let Some(last) = self.changes.last()
            && tick == last.start + RESHAPE_DURATION + 1
            && last.stop == last.start + RESHAPE_DURATION
        {
            self.settled_aspect = Some(last.aspect);
        }

        let same = self.reference.zip(tile).is_some_and(|(old, current)| {
            (old.low - current.low).abs().max_element() <= 1e-5
                && (old.high - current.high).abs().max_element() <= 1e-5
        });
        if !same {
            self.reference = tile;
            self.stable_since = tick;
            let stop = tick.saturating_sub(1);
            if self.changes.last().is_some_and(|last| last.stop > stop)
                && let Some(last) = Arc::make_mut(&mut self.changes).last_mut()
            {
                last.stop = stop;
            }
            return;
        }
        if tick.saturating_sub(self.stable_since) != STABLE_TICKS {
            return;
        }

        let tile = tile.expect("unchanged live tile");
        let aspect = tile.usable().ground_aspect();
        let Some(previous) = self.settled_aspect else {
            self.settled_aspect = Some(aspect);
            return;
        };
        if (aspect / previous - 1.0).abs() < 0.02 {
            return;
        }
        let from = self
            .changes
            .last()
            .map_or(Vec2::ONE, |last| last.sample(tick));
        let to = target(tile);
        if from.distance_squared(to) < 1e-8 {
            self.settled_aspect = Some(aspect);
            return;
        }
        Arc::make_mut(&mut self.changes).push(Change {
            start: tick,
            stop: tick + RESHAPE_DURATION,
            from,
            to,
            aspect,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_reshape_holds_until_the_new_tile_has_settled() {
        let square = Tile {
            low: Vec2::ZERO,
            high: Vec2::ONE * 0.4,
        };
        let wide = Tile {
            high: Vec2::new(0.8, 0.4),
            ..square
        };
        let translated = Tile {
            low: wide.low + Vec2::Y * 0.1,
            high: wide.high + Vec2::Y * 0.1,
        };
        let plan = Plan::build(1, 450, None, |tick, tracks| {
            let tile = if tick < 100 {
                square
            } else if tick < 220 {
                wide
            } else {
                translated
            };
            tracks[0].observe(tick, Some(tile), |_| Vec2::new(2.0, 1.0));
        });
        assert_eq!(plan.sample(0, 160), Vec2::ONE);
        assert!(plan.sample(0, 190).x > 1.0);
        let held = plan.sample(0, 219);
        assert!(held.x < 2.0);
        assert_eq!(plan.sample(0, 220), held);
        assert_eq!(plan.sample(0, 280), held);
        assert!(plan.sample(0, 300).x > held.x);
        assert_eq!(plan.sample(0, 400), Vec2::new(2.0, 1.0));
    }
}
