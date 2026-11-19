use std::collections::{BTreeMap, BTreeSet};

use super::{HALF, Rect, Vec2};
use crate::model::{
    Simulation, titles,
    trace::{Counter, Phase},
};

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
type Reservations = BTreeMap<(usize, usize), Vec<(u8, Rect)>>;

/// Extends each conflicted service's reservation back over the same avatar's
/// services in that repository during the preceding eight seconds.
fn anticipate_service_reservations(
    events: &[crate::timeline::Event],
    starts: &[crate::model::Time],
    keys: &[crate::contributors::Key],
    occupied: &mut Reservations,
    conflicted: &mut BTreeSet<(usize, usize)>,
) {
    let observed: Vec<_> = conflicted.iter().copied().collect();
    for (commit, avatar) in observed {
        let future = occupied.get(&(commit, avatar)).cloned().unwrap_or_default();
        for prior in (0..commit).rev() {
            if starts[commit] - starts[prior] > 8.0 {
                break;
            }
            let event = &events[prior];
            if event.participants().all(|c| c.key != keys[avatar]) {
                continue;
            }
            if event.repository != events[commit].repository {
                break;
            }
            conflicted.insert((prior, avatar));
            let row = occupied.entry((prior, avatar)).or_default();
            for &(edge, rect) in &future {
                merge(row, edge, rect);
            }
        }
    }
}

impl Simulation {
    fn reserve_frame(
        &self,
        occupied: &mut Reservations,
        conflicted: &mut BTreeSet<(usize, usize)>,
    ) -> (usize, usize) {
        let mut conflicts = 0;
        let mut pairs = 0;
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
            pairs += live.len() - 1;
            // A group parks around its author's dock, so its author makes room for it.
            let owner = avatar
                .group
                .map(|group| group.lead)
                .filter(|&lead| state.in_party(i, lead))
                .unwrap_or(i);
            for &(j, other, _) in &live {
                if i == j || state.in_party(j, owner) {
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
                    conflicted.insert((avatar.commit, owner));
                }
                let row = occupied.entry((avatar.commit, owner)).or_default();
                reserve_other(row, low, span, other, radius);
            }
        }
        (conflicts, pairs)
    }

    pub(in crate::model) fn reserve_service_slots(&mut self) -> bool {
        let _phase = self.trace.phase(Phase::Reserve);
        let mut previous = std::mem::take(&mut self.travel.occupancy);
        previous.retain(|&(commit, _), _| commit < self.events.len());
        let mut occupied = Reservations::new();
        let mut conflicted = BTreeSet::new();
        let visuals = std::mem::replace(&mut self.visuals, crate::model::Visuals::None);
        self.restart();
        let mut conflicts = 0;
        let mut pairs = 0;
        let frames = crate::model::frames_for(self.duration(), 60)
            .expect("bounded schedule")
            + 144;
        let begin = u32::try_from(self.state.tick).expect("bounded window");
        self.trace.count_replay((frames - begin).into());
        for frame in begin..=frames {
            self.progress.report_range(
                crate::progress::Stage::Reservations,
                frame.into(),
                begin.into(),
                frames.into(),
            );
            self.advance_to(frame, 60);
            let (conflicting, compared) =
                self.reserve_frame(&mut occupied, &mut conflicted);
            conflicts += conflicting;
            pairs += compared;
        }
        self.trace.count(Counter::PairComparisons, pairs);
        self.trace.add_ticks(&mut self.tick_times);
        let keys: Vec<_> = self.state.avatars.iter().map(|a| a.key.clone()).collect();
        self.restart();
        self.visuals = visuals;
        anticipate_service_reservations(
            &self.events,
            &self.schedule.starts,
            &keys,
            &mut occupied,
            &mut conflicted,
        );
        for (key, row) in occupied {
            if conflicted.contains(&key) && key.0 >= self.travel.frozen {
                previous.insert(key, row);
            }
        }
        self.travel.occupancy = previous;
        if self.trace.on() {
            eprintln!("committing_overlap_observations={conflicts}");
        }
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
    /// Everything a refinement round leaves for the next, with floats printed exactly.
    pub(in crate::model) fn round_state(
        &self,
        window: Option<&crate::model::windows::Planning>,
    ) -> String {
        format!(
            "{:?}{:?}{:?}{:?}",
            self.trips,
            self.docks,
            self.occupancy,
            window.map(|window| &window.captured)
        )
    }

    pub(in crate::model) fn reserved(
        &self,
        commit: usize,
        avatar: usize,
        tile: Option<crate::layout::packing::Tile>,
        choosing: bool,
    ) -> Vec<Rect> {
        if !choosing {
            return Vec::new();
        }
        tile.into_iter()
            .flat_map(|tile| {
                self.occupancy
                    .get(&(commit, avatar))
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
        let _phase = self.trace.phase(Phase::RefineServiceSlots);
        let mut previous = None;
        for _ in 0..8 {
            if !self.reserve_service_slots() {
                break;
            }
            self.plan_travel(options, padding);
            let adjusted = self.adjust_travel_padding(options, padding);
            if adjusted != padding {
                return adjusted;
            }
            let round = self.travel.round_state(self.window.as_ref());
            // A round that ends where it began repeats unchanged until the cap.
            if !options.replay_everything
                && previous.as_ref() == Some(&round)
                && !round.contains("NaN")
            {
                self.trace.count(Counter::SettledRefinements, 1);
                break;
            }
            previous = Some(round);
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
        let keys = [events[0].contributor.clone().unwrap().key];
        let delayed = || {
            (
                super::Reservations::from([((3, 0), vec![reservation])]),
                super::BTreeSet::from([(3, 0)]),
            )
        };
        let (mut rows, mut flags) = delayed();
        super::anticipate_service_reservations(
            &events,
            &[0.0, 2.0, 4.0, 6.0].map(crate::model::Time::seconds),
            &keys,
            &mut rows,
            &mut flags,
        );
        assert_eq!(
            flags,
            super::BTreeSet::from([(0, 0), (1, 0), (2, 0), (3, 0)])
        );
        assert!(rows.values().all(|row| row.as_slice() == [reservation]));

        let (mut rows, mut flags) = delayed();
        super::anticipate_service_reservations(
            &events,
            &[0.0, 2.0, 4.0, 20.0].map(crate::model::Time::seconds),
            &keys,
            &mut rows,
            &mut flags,
        );
        assert_eq!(flags, super::BTreeSet::from([(3, 0)]));
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
