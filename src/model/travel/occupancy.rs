use super::{HALF, Rect, Vec2};
use crate::model::{
    Simulation, titles,
    trace::{Counter, Phase},
};

/// How far apart two reserved rects on one edge must stay to remain separate.
const TOUCH: f32 = 0.00001;

/// One reserved rect on an edge, stamped with when it was last merged.
#[derive(Clone, Copy, Debug)]
struct Reserved {
    rect: Rect,
    stamp: u64,
    /// No other rect on its edge meets its margin, so merging a rect inside it changes
    /// nothing but its stamp.
    alone: bool,
}

/// One commit's reservations as they accumulate, kept apart per edge. The stamps
/// restore the order a single list holds when each merge moves its result to the back.
#[derive(Clone, Debug, Default)]
struct Row {
    stamp: u64,
    edges: [Vec<Reserved>; 4],
}

impl Row {
    /// Joins `rect` with every rect on its edge that it meets, transitively, and moves
    /// the joined rect to the back.
    fn merge(&mut self, edge: u8, mut rect: Rect) {
        self.stamp += 1;
        let stamp = self.stamp;
        let rects = &mut self.edges[usize::from(edge)];
        // Nothing else meets an alone rect's margin, so it absorbs one inside it and
        // meets nothing more.
        if let Some(index) = rects.iter().position(|kept| {
            kept.alone
                && rect.min.cmplt(rect.max).all()
                && kept.rect.min.cmple(rect.min).all()
                && rect.max.cmple(kept.rect.max).all()
        }) {
            let mut kept = rects.remove(index);
            kept.rect = Rect::from_corners(
                rect.min.min(kept.rect.min),
                rect.max.max(kept.rect.max),
            );
            kept.stamp = stamp;
            rects.push(kept);
            return;
        }
        loop {
            let before = rects.len();
            rects.retain(|kept| {
                let r = kept.rect;
                if r.intersect(rect.inflate(TOUCH)).is_empty() {
                    return true;
                }
                rect = Rect::from_corners(rect.min.min(r.min), rect.max.max(r.max));
                false
            });
            if before == rects.len() {
                break;
            }
        }
        for kept in &mut *rects {
            kept.alone &= rect.intersect(kept.rect.inflate(TOUCH)).is_empty();
        }
        rects.push(Reserved {
            rect,
            stamp,
            alone: true,
        });
    }

    fn into_row(self) -> Vec<(u8, Rect)> {
        let mut stamped: Vec<_> = (0_u8..)
            .zip(self.edges)
            .flat_map(|(edge, rects)| {
                rects
                    .into_iter()
                    .map(move |kept| (kept.stamp, edge, kept.rect))
            })
            .collect();
        stamped.sort_unstable_by_key(|&(stamp, ..)| stamp);
        stamped
            .into_iter()
            .map(|(_, edge, rect)| (edge, rect))
            .collect()
    }
}

fn anticipate_service_reservations(
    events: &[crate::timeline::Event],
    starts: &[crate::model::Time],
    occupied: &mut [Row],
    conflicted: &mut [bool],
) {
    let observed = conflicted.to_vec();
    for (commit, conflict) in observed.into_iter().enumerate() {
        if !conflict {
            continue;
        }
        let future = occupied[commit].clone().into_row();
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
                occupied[prior].merge(edge, rect);
            }
        }
    }
}

impl Simulation {
    fn reserve_frame(
        &self,
        occupied: &mut [Row],
        conflicted: &mut [bool],
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
        (conflicts, pairs)
    }

    pub(in crate::model) fn reserve_service_slots(&mut self) -> bool {
        let _phase = self.trace.phase(Phase::Reserve);
        let mut previous = std::mem::take(&mut self.travel.occupancy);
        previous.resize_with(self.events.len(), Vec::new);
        let mut occupied = vec![Row::default(); self.events.len()];
        let mut conflicted = vec![false; self.events.len()];
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
        self.restart();
        self.visuals = visuals;
        anticipate_service_reservations(
            &self.events,
            &self.schedule.starts,
            &mut occupied,
            &mut conflicted,
        );
        for (index, row) in occupied.into_iter().enumerate() {
            if conflicted[index] && index >= self.travel.frozen {
                previous[index] = row.into_row();
            }
        }
        self.travel.occupancy = previous;
        if self.trace.on() {
            eprintln!("committing_overlap_observations={conflicts}");
        }
        conflicts > 0
    }
}
fn reserve_other(row: &mut Row, low: Vec2, span: Vec2, other: Vec2, radius: f32) {
    let rect = Rect::from_corners(
        (other - Vec2::splat(2.0 * radius + 1.0) - low) / span,
        (other + Vec2::splat(2.0 * radius + 1.0) - low) / span,
    );
    for edge in 0..4 {
        let axis = usize::from(edge % 2 == 0);
        let coordinate = if edge < 2 { 1.0 } else { 0.0 };
        if ((other - low)[axis] - coordinate * span[axis]).abs() <= 2.0 * radius + 1.0 {
            row.merge(edge, rect);
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
    use super::{Rect, Vec2};

    /// The merge as first shipped, one list scanned whole, which [`super::Row`] must
    /// reproduce bit for bit.
    fn reference_merge(row: &mut Vec<(u8, Rect)>, edge: u8, mut rect: Rect) {
        loop {
            let before = row.len();
            row.retain(|(side, r)| {
                if *side != edge || r.intersect(rect.inflate(super::TOUCH)).is_empty() {
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

    fn bits(row: Vec<(u8, Rect)>) -> Vec<(u8, [u32; 4])> {
        row.into_iter()
            .map(|(edge, r)| {
                (edge, [r.min.x, r.min.y, r.max.x, r.max.y].map(f32::to_bits))
            })
            .collect()
    }

    #[test]
    fn a_margin_that_rounds_one_way_only_still_joins_both_rects() {
        // At 256 the margin rounds away above the edge but not below it, so the
        // second rect meets the first one's margin while the first misses its own.
        let first = Rect::new(256.0, 0.0, 257.0, 1.0);
        let second = Rect::new(255.0, 0.0, 256.0, 1.0);
        let mut row = super::Row::default();
        let mut reference = Vec::new();
        for rect in [first, second, first] {
            row.merge(0, rect);
            reference_merge(&mut reference, 0, rect);
        }
        assert_eq!(reference, [(0, Rect::new(255.0, 0.0, 257.0, 1.0))]);
        assert_eq!(bits(row.into_row()), bits(reference));
    }

    #[test]
    fn rows_match_a_single_list_merge_bit_for_bit() {
        let mut seed = 0x2545_f491_u32;
        let mut next = |limit: u16| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            usize::try_from((seed >> 8) % u32::from(limit)).unwrap()
        };
        // Gaps just inside and just outside the margin, and none at all.
        let nudges = [0.0, super::TOUCH * 0.5, super::TOUCH * 2.0];
        for _ in 0..400 {
            let mut row = super::Row::default();
            let mut reference = Vec::new();
            for _ in 0..60 {
                // Few edges and a coarse grid, so rects repeat, nest, abut and chain.
                let edge = [0, 1, 3][next(3)];
                let nudge = Vec2::new(nudges[next(3)], nudges[next(3)]);
                let mut grid =
                    |limit| f32::from(u8::try_from(next(limit)).unwrap()) / 8.0;
                let corner = Vec2::new(grid(24), grid(24)) + nudge;
                let size = Vec2::new(grid(3), grid(3)) + 0.125;
                let rect = Rect::from_corners(corner, corner + size);
                row.merge(edge, rect);
                reference_merge(&mut reference, edge, rect);
                assert_eq!(bits(row.clone().into_row()), bits(reference.clone()));
            }
        }
    }

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
        let mut rows = vec![super::Row::default(); 4];
        rows[3].merge(reservation.0, reservation.1);
        let mut flags = vec![false, false, false, true];
        super::anticipate_service_reservations(
            &events,
            &[0.0, 2.0, 4.0, 6.0].map(crate::model::Time::seconds),
            &mut rows,
            &mut flags,
        );
        assert!(flags.iter().all(|f| *f));
        assert!(rows.into_iter().all(|row| row.into_row() == [reservation]));

        let mut rows = vec![super::Row::default(); 4];
        rows[3].merge(reservation.0, reservation.1);
        let mut flags = vec![false, false, false, true];
        super::anticipate_service_reservations(
            &events,
            &[0.0, 2.0, 4.0, 20.0].map(crate::model::Time::seconds),
            &mut rows,
            &mut flags,
        );
        assert_eq!(flags, [false, false, false, true]);
    }
    #[test]
    fn reservations_include_both_future_radii_before_the_discs_grow() {
        let mut row = super::Row::default();
        super::reserve_other(
            &mut row,
            super::Vec2::ZERO,
            super::Vec2::splat(100.0),
            super::Vec2::ZERO,
            14.0,
        );
        let row = row.into_row();
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
