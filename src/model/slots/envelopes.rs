use std::{mem::size_of, ops::Range};

use bevy::math::{Vec2, Vec3};

use super::{Envelope, Target, ground, place_point};
use crate::{
    layout::packing::Packing,
    model::{FragmentPace, State},
    timeline::Event,
};

impl Envelope {
    pub(in crate::model) fn ray_bytes(&self) -> usize {
        self.rays.capacity() * size_of::<Vec2>()
    }
}

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
    /// The most ticks one pulse is captured on: one for each started tick of its
    /// longest lifetime, and a spare.
    pub(in crate::model) fn pulse_ticks(pace: FragmentPace) -> usize {
        usize::try_from(
            crate::model::frames_for(pace.pulse_end(crate::model::Time::ZERO), 60)
                .expect("bounded pulse"),
        )
        .expect("pulse ticks fit usize")
            + 1
    }

    fn capacity(events: &[Event], pace: FragmentPace) -> (usize, usize) {
        let ticks = Self::pulse_ticks(pace);
        let points = events
            .iter()
            .map(|event| event.changes.len() * 2)
            .sum::<usize>();
        (points * ticks, events.len() * ticks)
    }

    pub(in crate::model) fn estimated_bytes(
        events: &[Event],
        pace: FragmentPace,
    ) -> u128 {
        let (points, frames) = Self::capacity(events, pace);
        points as u128 * std::mem::size_of::<Vec2>() as u128
            + frames as u128 * std::mem::size_of::<ServiceFrame>() as u128
    }

    pub(in crate::model) fn new(events: &[Event], pace: FragmentPace) -> Self {
        let (points, frames) = Self::capacity(events, pace);
        Self {
            points: Vec::with_capacity(points),
            frames: Vec::with_capacity(frames),
        }
    }

    pub(in crate::model) fn capture(&mut self, state: &State) {
        let now = crate::model::tick_time(state.tick);
        for pulse in state.pulses.iter().filter(|p| now < p.end) {
            assert!(
                self.frames.len() < self.frames.capacity(),
                "each commit's pulse is live for at most `pulse_ticks` ticks"
            );
            assert!(
                self.points.len() + pulse.touched.len() + pulse.targets.len()
                    <= self.points.capacity(),
                "a pulse touches and targets at most one point per changed file"
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

    fn commit(seconds: i64, paths: impl IntoIterator<Item = String>) -> Event {
        Event::at(
            seconds,
            paths
                .into_iter()
                .map(|path| crate::timeline::FileChange {
                    path,
                    insertions: 1,
                    ..Default::default()
                })
                .collect(),
        )
    }

    /// Commits a tick apart whose pulses overlap: one beam per file, one file listed
    /// twice, and enough directories to aggregate the beams.
    fn crowded() -> Vec<Event> {
        vec![
            commit(0, (0..24).map(|file| format!("{file}.rs"))),
            commit(1, ["twice.rs".to_owned(), "twice.rs".to_owned()]),
            commit(2, (0..60).map(|file| format!("{}/{file}.rs", file % 30))),
        ]
    }

    #[test]
    fn run_sized_snapshot_cache_does_not_grow_during_capture() {
        let theme = crate::theme::Theme::neon();
        let mappings = crate::contributors::Mappings::default();
        let waits: [&[(usize, u32)]; 2] = [&[], &[(1, 7), (2, 400)]];
        for (mut events, start) in [
            (crate::timeline::synthetic(), 0),
            (crate::timeline::synthetic(), 3),
            (crowded(), 0),
        ] {
            crate::repositories::namespace(&mut events, 0);
            let options = crate::model::Options {
                start,
                ..Default::default()
            };
            for padding in waits {
                let mut sim = crate::model::Simulation::build(
                    events.clone(),
                    &options,
                    &theme,
                    &mappings,
                    padding,
                    &["Only".to_owned()],
                    &mut None,
                )
                .unwrap();
                let mut snapshots =
                    Snapshots::new(&sim.events, options.fragment_pace());
                let bytes = snapshots.allocated_bytes();
                assert_eq!(
                    Snapshots::estimated_bytes(
                        &events[start..],
                        options.fragment_pace()
                    ),
                    bytes as u128
                );
                for tick in 0..=crate::model::frames_for(sim.duration(), 60).unwrap() {
                    sim.advance_to(tick, 60);
                    snapshots.capture(&sim.state);
                }
                assert!(!snapshots.frames.is_empty());
                assert_eq!(snapshots.allocated_bytes(), bytes);
            }
        }
    }
}
