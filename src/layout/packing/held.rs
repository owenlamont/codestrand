//! Tile weights that follow live item counts in rare, slow reflows rather than every tick.

use crate::model::{TICK_RATE, count};

/// When the held weights may drift from the live counts before a reflow, and how it eases.
#[derive(Clone, Copy, Debug)]
struct Policy {
    /// Field share, as total variation over present tiles, on the wrong tiles.
    drift: f32,
    /// Ratio between one tile's live and held share, so small tiles also follow.
    ratio: f32,
    /// Ticks one reflow eases over.
    reflow: u32,
    /// Ticks after a reflow ends before the next may start.
    hold: u32,
}

const POLICY: Policy = Policy {
    drift: 0.05,
    ratio: 1.5,
    reflow: 2 * TICK_RATE,
    hold: 4 * TICK_RATE,
};

/// Weights held at the counts of the last reflow, easing towards the next one's.
pub(super) struct Weights {
    policy: Policy,
    from: Vec<f32>,
    to: Vec<f32>,
    present: Vec<bool>,
    start: usize,
    next: usize,
}

impl Weights {
    pub(super) fn new(repositories: usize) -> Self {
        Self::with_policy(repositories, POLICY)
    }

    fn with_policy(repositories: usize, policy: Policy) -> Self {
        Self {
            policy,
            from: vec![1.0; repositories],
            to: vec![1.0; repositories],
            present: vec![false; repositories],
            start: 0,
            next: 0,
        }
    }

    /// The weights on `tick`, given each repository's live item count and presence; ticks
    /// must arrive in order.
    pub(super) fn step(
        &mut self,
        tick: usize,
        items: &[f32],
        presence: &[f32],
    ) -> Vec<f32> {
        let targets: Vec<_> = items.iter().map(|&count| count.max(1.0)).collect();
        for (i, &p) in presence.iter().enumerate() {
            // An arriving tile grows from nothing through its presence, so it takes its
            // count at once instead of waiting for the next reflow.
            if p > 0.0 && !self.present[i] {
                self.from[i] = targets[i];
                self.to[i] = targets[i];
            }
            self.present[i] = p > 0.0;
        }
        if tick >= self.next && self.drifted(&targets, presence) {
            self.from = self.at(tick);
            self.to = targets;
            self.start = tick;
            self.next = tick + span(self.policy.reflow + self.policy.hold);
        }
        self.at(tick)
    }

    fn at(&self, tick: usize) -> Vec<f32> {
        let reflow = span(self.policy.reflow).max(1);
        let u = count((tick - self.start).min(reflow)) / count(reflow);
        let eased = u * u * (3.0 - 2.0 * u);
        self.from
            .iter()
            .zip(&self.to)
            .map(|(&from, &to)| from + (to - from) * eased)
            .collect()
    }

    fn drifted(&self, targets: &[f32], presence: &[f32]) -> bool {
        let total = |weights: &[f32]| -> f32 {
            weights.iter().zip(presence).map(|(w, p)| w * p).sum()
        };
        let (live, held) = (total(targets), total(&self.to));
        let mut variation = 0.0;
        for ((&target, &to), &p) in targets.iter().zip(&self.to).zip(presence) {
            if p > 0.0 {
                let (a, b) = (target / live, to / held);
                variation += 0.5 * p * (a - b).abs();
                if a.max(b) > self.policy.ratio * a.min(b) {
                    return true;
                }
            }
        }
        variation > self.policy.drift
    }
}

fn span(ticks: u32) -> usize {
    usize::try_from(ticks).expect("tick spans fit usize")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: [f32; 2] = [1.0, 1.0];

    #[test]
    fn counts_within_the_drift_keep_their_weights() {
        let mut weights = Weights::new(2);
        let first = weights.step(0, &[100.0, 100.0], &BOTH);
        for tick in 1..1000 {
            assert_eq!(weights.step(tick, &[104.0, 100.0], &BOTH), first);
        }
    }

    #[test]
    fn a_drift_eases_once_to_the_new_counts_then_holds() {
        let mut weights = Weights::new(2);
        weights.step(0, &[100.0, 100.0], &BOTH);
        let reflow = span(POLICY.reflow);
        let path: Vec<_> = (1..=reflow + 1)
            .map(|tick| weights.step(tick, &[100.0, 300.0], &BOTH)[1])
            .collect();
        assert!(path.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(path[0] < 101.0);
        assert_eq!(path[reflow], 300.0);
        let late = span(POLICY.reflow + POLICY.hold);
        assert_eq!(weights.step(late, &[100.0, 900.0], &BOTH)[1], 300.0);
        assert_eq!(weights.step(late + 1, &[100.0, 900.0], &BOTH)[1], 300.0);
        assert!(weights.step(late + 2, &[100.0, 900.0], &BOTH)[1] > 300.0);
    }

    #[test]
    fn a_small_tile_growing_past_the_ratio_reflows_below_the_drift() {
        let policy = Policy {
            drift: 0.5,
            ..POLICY
        };
        let mut weights = Weights::with_policy(2, policy);
        weights.step(0, &[1000.0, 10.0], &BOTH);
        let reflow = span(POLICY.reflow);
        assert_eq!(weights.step(reflow + 1, &[1000.0, 100.0], &BOTH)[1], 10.0);
        assert_eq!(
            weights.step(2 * reflow + 1, &[1000.0, 100.0], &BOTH)[1],
            100.0
        );
    }

    #[test]
    fn an_arriving_tile_takes_its_count_at_once() {
        let mut weights = Weights::new(2);
        weights.step(0, &[100.0, 0.0], &[1.0, 0.0]);
        assert_eq!(weights.step(1, &[100.0, 80.0], &[1.0, 0.1])[1], 80.0);
    }
}
