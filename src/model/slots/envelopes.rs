use std::ops::Range;

use bevy::math::{Vec2, Vec3};

use super::{Envelope, Target, ground, place_point};
use crate::{layout::packing::Packing, model::State, timeline::Event};

struct ServiceFrame {
    tick: u64,
    commit: usize,
    repository: usize,
    points: Range<usize>,
    touched: usize,
}

pub(in crate::model) struct Snapshots {
    points: Vec<Vec2>,
    frames: Vec<ServiceFrame>,
}

impl Snapshots {
    fn capacity(events: &[Event]) -> (usize, usize) {
        let ticks = usize::try_from(
            crate::model::frames_for(
                crate::model::PULSE_SECONDS + crate::model::FRAGMENT_FLIGHT,
                60,
            )
            .expect("bounded pulse"),
        )
        .expect("pulse ticks fit usize")
            + 1;
        let points = events
            .iter()
            .map(|event| event.changes.len() * 2)
            .sum::<usize>();
        (points * ticks, events.len() * ticks)
    }

    pub(in crate::model) fn estimated_bytes(events: &[Event]) -> u128 {
        let (points, frames) = Self::capacity(events);
        points as u128 * std::mem::size_of::<Vec2>() as u128
            + frames as u128 * std::mem::size_of::<ServiceFrame>() as u128
    }

    pub(in crate::model) fn new(events: &[Event]) -> Self {
        let (points, frames) = Self::capacity(events);
        Self {
            points: Vec::with_capacity(points),
            frames: Vec::with_capacity(frames),
        }
    }

    pub(in crate::model) fn capture(&mut self, state: &State) {
        let now = crate::model::tick_time(state.tick);
        for pulse in state.pulses.iter().filter(|p| now < p.end) {
            assert!(self.frames.len() < self.frames.capacity());
            assert!(
                self.points.len() + pulse.touched.len() + pulse.targets.len()
                    <= self.points.capacity()
            );
            let start = self.points.len();
            self.points.extend(
                pulse
                    .touched
                    .iter()
                    .filter_map(|&id| state.point(Target::File(id)))
                    .map(|p| Vec2::new(p.x, p.z)),
            );
            let touched = self.points.len() - start;
            self.points.extend(
                pulse
                    .targets
                    .iter()
                    .filter_map(|&(target, ..)| state.point(target))
                    .map(|p| Vec2::new(p.x, p.z)),
            );
            self.frames.push(ServiceFrame {
                tick: state.tick,
                commit: pulse.commit,
                repository: pulse.repository,
                points: start..self.points.len(),
                touched,
            });
        }
    }

    pub(super) fn project(self, packing: &Packing, events: usize) -> Vec<Envelope> {
        eprintln!(
            "envelope_snapshot_frames={} envelope_snapshot_points={}",
            self.frames.len(),
            self.points.len()
        );
        let mut envelopes = vec![Envelope::default(); events];
        for frames in self.frames.chunk_by(|a, b| a.tick == b.tick) {
            let mut needed: Vec<_> = frames.iter().map(|f| f.repository).collect();
            needed.sort_unstable();
            needed.dedup();
            let (regions, origins, scales) = packing
                .sample_repositories(frames[0].tick, &needed)
                .expect("planned repository samples");
            for frame in frames {
                let Some(tile) = regions[frame.repository].tile else {
                    continue;
                };
                let origin = origins[frame.repository];
                let mut points: Vec<Vec3> = self.points[frame.points.clone()]
                    .iter()
                    .map(|&p| {
                        place_point(
                            ground(p),
                            regions[frame.repository].rotated,
                            scales[frame.repository],
                            origin,
                        )
                    })
                    .collect();
                envelopes[frame.commit].include_service(
                    &mut points,
                    frame.touched,
                    tile,
                );
            }
        }
        envelopes
    }

    #[cfg(test)]
    pub(in crate::model) fn allocated_bytes(&self) -> usize {
        self.points.capacity() * std::mem::size_of::<Vec2>()
            + self.frames.capacity() * std::mem::size_of::<ServiceFrame>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_sized_snapshot_cache_does_not_grow_during_capture() {
        let mut events = crate::timeline::synthetic();
        crate::repositories::namespace(&mut events, 0);
        let mut sim = crate::model::Simulation::with_repositories(
            events,
            &crate::model::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &["Only".to_owned()],
        );
        let mut snapshots = Snapshots::new(&sim.events);
        let bytes = snapshots.allocated_bytes();
        for tick in 0..=crate::model::frames_for(sim.duration(), 60).unwrap() {
            sim.advance_to(tick, 60);
            snapshots.capture(&sim.state);
        }
        assert!(!snapshots.frames.is_empty());
        assert_eq!(snapshots.allocated_bytes(), bytes);
    }
}
