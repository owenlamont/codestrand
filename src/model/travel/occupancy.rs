use super::{HALF, Rect, Vec2};
use crate::model::{Simulation, titles};

fn merge(row: &mut Vec<(u8, Rect)>, edge: u8, mut rect: Rect) {
    loop {
        let before = row.len();
        row.retain(|(side, r)| {
            if *side != edge || r.intersect(rect.inflate(0.00001)).is_empty() {
                return true;
            }
            rect = Rect::from_corners(rect.min.min(r.min), rect.max.max(r.max));
            false
        });
        if before == row.len() {
            break;
        }
    }
    row.push((edge, rect));
}
fn anticipate_service_reservations(
    events: &[crate::timeline::Event],
    starts: &[f32],
    occupied: &mut [Vec<(u8, Rect)>],
    conflicted: &mut [bool],
) {
    let observed = conflicted.to_vec();
    for (commit, conflict) in observed.into_iter().enumerate() {
        if !conflict {
            continue;
        }
        let future = occupied[commit].clone();
        let author = events[commit]
            .contributor
            .as_ref()
            .expect("resolved contributor");
        for prior in (0..commit).rev() {
            if starts[commit] - starts[prior] > 8.0 {
                break;
            }
            let event = &events[prior];
            if event
                .contributor
                .as_ref()
                .expect("resolved contributor")
                .key
                != author.key
            {
                continue;
            }
            if event.repository != events[commit].repository {
                break;
            }
            conflicted[prior] = true;
            for &(edge, rect) in &future {
                merge(&mut occupied[prior], edge, rect);
            }
        }
    }
}

impl Simulation {
    fn reserve_frame(
        &self,
        occupied: &mut [Vec<(u8, Rect)>],
        conflicted: &mut [bool],
    ) -> usize {
        let mut conflicts = 0;
        let state = &self.state;
        let radius = state.avatar_radius();
        let live: Vec<_> = state
            .avatars
            .iter()
            .enumerate()
            .filter(|(_, a)| a.presence > 0.0)
            .map(|(i, a)| {
                let point = titles::screen(self, a.position);
                let parked = !self.travel.travelling(i, state.tick)
                    && a.slot
                        .as_ref()
                        .is_some_and(|s| point.distance(s.point) < 0.1);
                (i, point, parked)
            })
            .collect();
        for &(i, point, applying) in &live {
            if !applying {
                continue;
            }
            let avatar = &state.avatars[i];
            let Some(tile) = state.layout.repositories[avatar.repository].gutter else {
                continue;
            };
            let low = tile.low * HALF;
            let span = (tile.high - tile.low) * HALF;
            for &(j, other, _) in &live {
                if i == j {
                    continue;
                }
                if super::committing_overlap(
                    point,
                    other,
                    2.0 * radius,
                    [true, true],
                    [true, false],
                ) {
                    conflicts += 1;
                    conflicted[avatar.commit] = true;
                }
                reserve_other(&mut occupied[avatar.commit], low, span, other, radius);
            }
        }
        conflicts
    }

    pub(in crate::model) fn reserve_service_slots(&mut self) -> bool {
        let mut previous = std::mem::take(&mut self.travel.occupancy);
        previous.resize_with(self.events.len(), Vec::new);
        let mut occupied = vec![Vec::new(); self.events.len()];
        let mut conflicted = vec![false; self.events.len()];
        let visual = self.visual_updates;
        self.visual_updates = false;
        self.restart();
        let mut conflicts = 0;
        let frames = crate::model::frames_for(self.duration(), 60)
            .expect("bounded schedule")
            + 144;
        for frame in 0..=frames {
            self.advance_to(frame, 60);
            conflicts += self.reserve_frame(&mut occupied, &mut conflicted);
        }
        self.restart();
        self.visual_updates = visual;
        anticipate_service_reservations(
            &self.events,
            &self.schedule.starts,
            &mut occupied,
            &mut conflicted,
        );
        for (index, row) in occupied.into_iter().enumerate() {
            if conflicted[index] {
                previous[index] = row;
            }
        }
        self.travel.occupancy = previous;
        eprintln!("committing_overlap_observations={conflicts}");
        conflicts > 0
    }
}
fn reserve_other(
    row: &mut Vec<(u8, Rect)>,
    low: Vec2,
    span: Vec2,
    other: Vec2,
    radius: f32,
) {
    let rect = Rect::from_corners(
        (other - Vec2::splat(2.0 * radius + 1.0) - low) / span,
        (other + Vec2::splat(2.0 * radius + 1.0) - low) / span,
    );
    for edge in 0..4 {
        let axis = usize::from(edge % 2 == 0);
        let coordinate = if edge < 2 { 1.0 } else { 0.0 };
        if ((other - low)[axis] - coordinate * span[axis]).abs() <= 2.0 * radius + 1.0 {
            merge(row, edge, rect);
        }
    }
}
impl super::Plan {
    pub(in crate::model) fn reserved(
        &self,
        commit: usize,
        tile: Option<crate::layout::packing::Tile>,
        choosing: bool,
    ) -> Vec<Rect> {
        if !choosing {
            return Vec::new();
        }
        tile.into_iter()
            .flat_map(|tile| {
                self.occupancy
                    .get(commit)
                    .into_iter()
                    .flatten()
                    .map(move |(_, r)| {
                        Rect::from_corners(
                            (tile.low + r.min * (tile.high - tile.low)) * HALF,
                            (tile.low + r.max * (tile.high - tile.low)) * HALF,
                        )
                    })
            })
            .collect()
    }
}
impl Simulation {
    pub(in crate::model) fn refine_service_slots(
        &mut self,
        options: &crate::model::Options,
        padding: &[(usize, u32)],
    ) -> Vec<(usize, u32)> {
        for _ in 0..8 {
            if !self.reserve_service_slots() {
                break;
            }
            self.plan_travel(options, padding);
            let adjusted = self.adjust_travel_padding(options, padding);
            if adjusted != padding {
                return adjusted;
            }
        }
        padding.to_vec()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reservations_reach_the_stationary_commits_before_a_delayed_service() {
        let mut events = crate::timeline::synthetic();
        events.truncate(4);
        let author = events[0].author.clone();
        for event in &mut events {
            event.author = author.clone();
            event.contributor = None;
            event.repository = 0;
        }
        crate::contributors::Mappings::default().apply(&mut events);
        let reservation = (
            0,
            super::Rect::from_center_size(super::Vec2::ZERO, super::Vec2::ONE),
        );
        let mut rows = vec![vec![], vec![], vec![], vec![reservation]];
        let mut flags = vec![false, false, false, true];
        super::anticipate_service_reservations(
            &events,
            &[0.0, 2.0, 4.0, 6.0],
            &mut rows,
            &mut flags,
        );
        assert!(flags.iter().all(|f| *f));
        assert!(rows.iter().all(|row| row.as_slice() == [reservation]));

        let mut rows = vec![vec![], vec![], vec![], vec![reservation]];
        let mut flags = vec![false, false, false, true];
        super::anticipate_service_reservations(
            &events,
            &[0.0, 2.0, 4.0, 20.0],
            &mut rows,
            &mut flags,
        );
        assert_eq!(flags, [false, false, false, true]);
    }
    #[test]
    fn reservations_include_both_future_radii_before_the_discs_grow() {
        let mut row = Vec::new();
        super::reserve_other(
            &mut row,
            super::Vec2::ZERO,
            super::Vec2::splat(100.0),
            super::Vec2::ZERO,
            14.0,
        );
        assert!(row.iter().any(|(_, r)| r.contains(super::Vec2::X * 0.27)));
        assert!(!row.iter().any(|(_, r)| r.contains(super::Vec2::X * 0.30)));
    }
    #[test]
    fn committing_disc_checks_every_visible_other_state() {
        let check = |visible, applying| {
            super::super::committing_overlap(
                super::Vec2::ZERO,
                super::Vec2::X,
                28.0,
                visible,
                applying,
            )
        };
        assert!(check([true, true], [true, false]));
        assert!(check([true, true], [false, true]));
        assert!(!check([true, false], [true, false]));
        assert!(!check([true, true], [false, false]));
        assert!(!super::super::committing_overlap(
            super::Vec2::ZERO,
            super::Vec2::X * 28.0,
            28.0,
            [true, true],
            [true, false]
        ));
    }
}
