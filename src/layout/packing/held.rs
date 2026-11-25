//! Tile weights that follow live item counts in rare, slow reflows rather than every tick.

use crate::model::{TICK_RATE, count};

/// When the held weights may drift from the live counts before a reflow, and how it eases.
#[derive(Clone, Copy, Debug)]
struct Policy {
    /// Field share, as total variation over present tiles, on the wrong tiles.
    drift: f32,
    /// Ticks one reflow eases over.
    reflow: u32,
}

const RAMP_FRACTION: f32 = 0.125;
pub(super) const REFLOW_LINEAR_FRACTION: f32 = 1.0 - RAMP_FRACTION;

const POLICY: Policy = Policy {
    drift: 0.05,
    reflow: 2 * TICK_RATE,
};

/// Weights held at the counts of the last reflow, easing towards the next one's.
pub(super) struct Weights {
    policy: Policy,
    from: Vec<f32>,
    to: Vec<f32>,
    present: Vec<bool>,
    start: usize,
    next: usize,
    due: Option<usize>,
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
            due: None,
        }
    }

    /// The weights on `tick`, given each repository's live item count and presence; ticks
    /// must arrive in order.
    #[cfg(test)]
    pub(super) fn step(
        &mut self,
        tick: usize,
        items: &[f32],
        presence: &[f32],
    ) -> Vec<f32> {
        self.step_towards(tick, items, items, presence, true)
    }

    #[cfg(test)]
    pub(super) fn step_towards(
        &mut self,
        tick: usize,
        items: &[f32],
        future: &[f32],
        presence: &[f32],
        can_reflow: bool,
    ) -> Vec<f32> {
        self.step_with_births(tick, items, future, presence, can_reflow, can_reflow)
    }

    pub(super) fn step_with_births(
        &mut self,
        tick: usize,
        items: &[f32],
        future: &[f32],
        presence: &[f32],
        settled: bool,
        clear_of_archives: bool,
    ) -> Vec<f32> {
        let targets: Vec<_> = items.iter().map(|&count| count.max(1.0)).collect();
        let (mut from, mut to, mut live) = (0.0, 0.0, 0.0);
        for (i, &p) in presence.iter().enumerate() {
            if self.present[i] && p > 0.0 {
                from += self.from[i];
                to += self.to[i];
                live += targets[i];
            }
        }
        let scale = |held: f32| if live > 0.0 { held / live } else { 1.0 };
        let (from, to) = (scale(from), scale(to));
        for (i, &p) in presence.iter().enumerate() {
            // An arriving tile grows from nothing through its presence, so it takes its
            // count at once, at both ends of the scale an ease in flight moves across.
            if p > 0.0 && !self.present[i] {
                self.from[i] = targets[i] * from;
                self.to[i] = targets[i] * to;
            }
            self.present[i] = p > 0.0;
        }
        let overdue = if self.drifted(&targets, presence) {
            tick >= *self.due.get_or_insert(tick) + reflow_ticks()
        } else {
            self.due = None;
            false
        };
        if self.due.is_some()
            && tick >= self.next
            && (settled || (overdue && clear_of_archives))
        {
            self.retarget(tick, future);
        }
        self.at(tick)
    }

    fn at(&self, tick: usize) -> Vec<f32> {
        let eased = progress(tick - self.start, span(self.policy.reflow));
        self.from
            .iter()
            .zip(&self.to)
            .map(|(&from, &to)| from + (to - from) * eased)
            .collect()
    }

    pub(super) fn target_at_start(&self, tick: usize) -> Option<&[f32]> {
        (tick != 0 && tick == self.start).then_some(&self.to)
    }

    pub(super) fn retarget(&mut self, tick: usize, future: &[f32]) {
        self.from = self.at(tick);
        self.to = future.iter().map(|&count| count.max(1.0)).collect();
        self.start = tick;
        self.next = tick + span(self.policy.reflow);
        self.due = None;
    }

    pub(super) fn ready(&self, tick: usize) -> bool {
        tick >= self.next
    }

    pub(super) fn reserve_reflow(&mut self, tick: usize) {
        self.next = tick + reflow_ticks();
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
                if (a - b).abs() > super::DENSITY_TOLERANCE * b {
                    return true;
                }
            }
        }
        variation > self.policy.drift
    }
}

pub(super) fn reflow_ticks() -> usize {
    span(POLICY.reflow)
}

/// Aim at the counts when the ease ends.
pub(super) fn target_tick(tick: usize) -> usize {
    tick + reflow_ticks()
}

pub(super) fn reflow_spacing() -> usize {
    reflow_ticks()
}

pub(super) fn reflow_ease(elapsed: usize) -> f32 {
    progress(elapsed, reflow_ticks())
}

pub(super) fn progress(elapsed: usize, duration: usize) -> f32 {
    ease(elapsed, duration, RAMP_FRACTION)
}

pub(super) fn birth_ease(elapsed: usize, duration: usize) -> f32 {
    ease(elapsed, duration, 0.25)
}

fn ease(elapsed: usize, duration: usize, ramp_fraction: f32) -> f32 {
    let phase = count(elapsed.min(duration)) / count(duration);
    let integrate = |phase: f32| {
        let ramp = phase.min(ramp_fraction);
        (phase - ramp + 0.5 * ramp * ramp / ramp_fraction) / (1.0 - ramp_fraction)
    };
    if phase <= 0.5 {
        integrate(phase)
    } else {
        1.0 - integrate(1.0 - phase)
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
    fn motion_accelerates_symmetrically_and_holds_steady_speed() {
        let duration = 240;
        let speed = |tick| progress(tick + 1, duration) - progress(tick, duration);
        let middle = speed(duration / 2);
        assert!(speed(0) < middle / 10.0);
        for tick in 0..duration / 8 {
            assert!(speed(tick) < speed(tick + 1));
            assert!((speed(tick) - speed(duration - tick - 1)).abs() < 1e-6);
        }
        for tick in duration / 8..duration * 7 / 8 {
            assert!((speed(tick) - middle).abs() < 1e-6);
        }
    }

    #[test]
    fn counts_within_the_drift_keep_their_weights() {
        let mut weights = Weights::new(2);
        let first = weights.step(0, &[100.0, 100.0], &BOTH);
        for tick in 1..1000 {
            assert_eq!(weights.step(tick, &[104.0, 100.0], &BOTH), first);
        }
    }

    #[test]
    fn a_drift_eases_to_the_new_counts_before_the_next_reflow() {
        let mut weights = Weights::new(2);
        weights.step(0, &[100.0, 100.0], &BOTH);
        let reflow = span(POLICY.reflow);
        let path: Vec<_> = (1..=reflow + 1)
            .map(|tick| weights.step(tick, &[100.0, 300.0], &BOTH)[1])
            .collect();
        assert!(path.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(path[0] < 101.0);
        assert_eq!(path[reflow], 300.0);
        let late = reflow + 2;
        assert_eq!(weights.step(late, &[100.0, 900.0], &BOTH)[1], 300.0);
        assert!(weights.step(late + 2, &[100.0, 900.0], &BOTH)[1] > 300.0);
    }

    #[test]
    fn a_small_tile_outside_density_tolerance_reflows_below_the_drift() {
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
    fn a_reflow_eases_towards_the_counts_at_its_end() {
        let mut weights = Weights::new(2);
        weights.step(0, &[100.0, 100.0], &BOTH);
        assert_eq!(
            weights.step_towards(1, &[100.0, 100.0], &[100.0, 400.0], &BOTH, true),
            [100.0, 100.0],
        );
        assert_eq!(
            weights.step_towards(2, &[100.0, 200.0], &[100.0, 400.0], &BOTH, true),
            [100.0, 100.0],
        );
        assert_eq!(
            weights.step(2 + reflow_ticks(), &[100.0, 300.0], &BOTH),
            [100.0, 400.0,]
        );
    }

    #[test]
    fn an_arriving_tile_joins_at_the_scale_the_others_are_held_at() {
        let mut weights = Weights::new(3);
        weights.step(0, &[10.0, 10.0, 0.0], &[1.0, 1.0, 0.0]);
        let joined = weights.step(1, &[1000.0, 1000.0, 100.0], &[1.0, 1.0, 0.5]);
        assert_eq!(joined, [10.0, 10.0, 1.0]);
    }

    #[test]
    fn a_tile_arriving_during_a_reflow_keeps_its_share_through_the_ease() {
        let mut weights = Weights::new(3);
        weights.step(0, &[100.0, 100.0, 0.0], &[1.0, 1.0, 0.0]);
        weights.step(1, &[100.0, 300.0, 0.0], &[1.0, 1.0, 0.0]);
        for tick in 2..=span(POLICY.reflow) {
            let row = weights.step(tick, &[1.0, 3.0, 2.0], &[1.0, 1.0, 1.0]);
            assert!((row[2] / (row[0] + row[1]) - 0.5).abs() < 1e-5, "{row:?}");
        }
    }

    #[test]
    fn an_arriving_tile_takes_its_count_at_once() {
        let mut weights = Weights::new(2);
        weights.step(0, &[100.0, 0.0], &[1.0, 0.0]);
        assert_eq!(weights.step(1, &[100.0, 80.0], &[1.0, 0.1])[1], 80.0);
    }
}
