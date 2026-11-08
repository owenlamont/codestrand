use std::ops::Range;

use bevy::math::{Vec2, Vec3};

use super::{Envelope, Target, ground, place_point};
use crate::{layout::packing::Packing, model::State};

const MAX_POINTS: usize = 4_194_304;
const MAX_FRAMES: usize = 65_536;

struct ServiceFrame {
    tick: u64,
    commit: usize,
    repository: usize,
    points: Range<usize>,
    touched: usize,
}

#[derive(Default)]
pub(in crate::model) struct Snapshots {
    points: Vec<Vec2>,
    frames: Vec<ServiceFrame>,
}

impl Snapshots {
    pub(in crate::model) fn capture(&mut self, state: &State) -> bool {
        let now = crate::model::tick_time(state.tick);
        for pulse in state.pulses.iter().filter(|p| now < p.end) {
            if self.frames.len() == MAX_FRAMES
                || self.points.len() + pulse.touched.len() + pulse.targets.len()
                    > MAX_POINTS
            {
                return false;
            }
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
        true
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
                let origin =
                    crate::layout::world_coordinates(origins[frame.repository]);
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
    fn snapshot_limit_rejects_growth_before_collecting_more_points() {
        let mut events = crate::timeline::synthetic();
        crate::repositories::namespace(&mut events, 0);
        let mut sim = crate::model::Simulation::with_repositories(
            events,
            &crate::model::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &["Only".to_owned()],
        );
        sim.advance_to(31, 60);
        let mut snapshots = Snapshots::default();
        snapshots.points.resize(MAX_POINTS, Vec2::ZERO);
        let bytes = snapshots.allocated_bytes();
        assert!(!snapshots.capture(&sim.state));
        assert_eq!(snapshots.points.len(), MAX_POINTS);
        assert_eq!(snapshots.allocated_bytes(), bytes);
    }
}
