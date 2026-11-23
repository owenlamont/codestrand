use std::collections::{BTreeMap, BTreeSet};

use bevy::math::Vec3;

use super::{HALF, Rect, Vec2};
use crate::model::{
    Simulation, occlusion,
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

type Reservations = BTreeMap<(usize, usize), Row>;

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
        let future = occupied
            .get(&(commit, avatar))
            .cloned()
            .unwrap_or_default()
            .into_row();
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
                row.merge(edge, rect);
            }
        }
    }
}

impl Simulation {
    fn reserve_missed_dock(
        &mut self,
        commit: usize,
        author: usize,
        repository: usize,
    ) -> bool {
        let avatar = &self.state.avatars[author];
        let gutter = self.state.layout.repositories[repository].gutter;
        avatar.presence > 0.001
            && !avatar.beside_gutter(gutter, self.state.avatar_radius())
            && self.travel.unreached_docks.insert((commit, author))
    }

    fn reserve_blocked_walks(&mut self, previous: &mut Vec<Option<Vec3>>) -> usize {
        let mut blocked = Vec::new();
        let mut walking = Vec::new();
        for (author, avatar) in self.state.avatars.iter().enumerate() {
            if !avatar.travelling || avatar.presence <= 0.001 {
                continue;
            }
            let Some(trip) = self.travel.walking_trip(author, self.state.tick) else {
                continue;
            };
            let key = (trip.commit, author);
            if trip.kind == super::TripKind::Relocation
                || trip.commit < self.travel.frozen
                || self.travel.unreached_docks.contains(&key)
            {
                continue;
            }
            if previous
                .get(author)
                .copied()
                .flatten()
                .is_some_and(|before| {
                    before.distance_squared(avatar.position) < 1e-8
                        && super::slots::field_point(avatar.position)
                            .distance(trip.source)
                            > super::NETWORK_EPSILON
                })
            {
                blocked.push(key);
            } else {
                walking.push(key);
            }
        }
        blocked.extend(occlusion::walk_contacts(self, &walking));
        previous.clear();
        previous.extend(
            self.state
                .avatars
                .iter()
                .map(|avatar| (avatar.presence > 0.001).then_some(avatar.position)),
        );
        blocked
            .into_iter()
            .filter(|&key| self.travel.unreached_docks.insert(key))
            .count()
    }

    fn reserve_blocked_ray_sources(
        &self,
        occupied: &mut Reservations,
        conflicted: &mut BTreeSet<(usize, usize)>,
    ) -> usize {
        let state = &self.state;
        let radius = state.avatar_radius();
        let mut blocked = 0;
        for pulse in &state.pulses {
            let avatar = &state.avatars[pulse.author];
            if !avatar.transmitting
                || super::super::tick_time(state.tick) > avatar.arrivals_land
                || avatar.repository != pulse.repository
            {
                continue;
            }
            let Some(tile) = state.layout.repositories[pulse.repository].gutter else {
                continue;
            };
            let Some(corners) =
                state.service_corners(pulse.repository, &self.envelopes[pulse.commit])
            else {
                continue;
            };
            if corners.into_iter().any(|to| {
                self.clear_arrival_start(pulse.author, pulse.repository, to)
                    .is_none()
            }) {
                let owner = avatar
                    .group
                    .filter(|group| state.in_party(pulse.author, group.lead))
                    .map_or(pulse.author, |group| group.lead);
                let service = (state.avatars[owner].commit, owner);
                conflicted.insert(service);
                reserve_other(
                    occupied.entry(service).or_default(),
                    tile.low * HALF,
                    (tile.high - tile.low) * HALF,
                    super::super::slots::field_point(avatar.position),
                    radius,
                );
                blocked += 1;
            }
        }
        blocked
    }

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
            .filter_map(|(i, a)| {
                let travelling = self.travel.travelling(i, state.tick);
                if travelling && self.visuals != crate::model::Visuals::Seating {
                    return None;
                }
                let position = if travelling && self.travel.walking(i, state.tick) {
                    self.travel
                        .sample(i, state.tick)
                        .map_or(a.position, |(point, _)| {
                            super::super::slots::world(point)
                        })
                } else {
                    a.position
                };
                let point = occlusion::screen(self, position);
                let parked = !travelling
                    && a.slot.as_ref().is_some_and(|slot| {
                        a.transmitting
                            || !a.travelling
                                && a.settle.is_none()
                                && a.position.distance(slot.world()) < 0.001
                    });
                Some((i, point, parked, travelling))
            })
            .collect();
        for &(i, point, applying, _) in &live {
            if !applying {
                continue;
            }
            let avatar = &state.avatars[i];
            let Some(tile) = state.layout.repositories[avatar.repository].gutter else {
                continue;
            };
            let low = tile.low * HALF;
            let span = (tile.high - tile.low) * HALF;
            let ring_seat = avatar
                .slot
                .as_ref()
                .is_some_and(|slot| slot.offset != Vec2::ZERO);
            pairs += live.len() - 1;
            // A group parks around its author's dock, so its author makes room for it.
            let owner = avatar
                .group
                .map(|group| group.lead)
                .filter(|&lead| state.in_party(i, lead))
                .unwrap_or(i);
            let mut foreign = live
                .iter()
                .filter(|&&(j, other, _, travelling)| {
                    i != j
                        && !state.in_party(j, owner)
                        && !(ring_seat && self.shares_ring_participant(i, j))
                        && (!travelling || point.distance(other) < 2.0 * radius)
                })
                .peekable();
            if foreign.peek().is_none() {
                continue;
            }
            let row = occupied.entry((avatar.commit, owner)).or_default();
            for &(_, other, _, _) in foreign {
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
                reserve_other(row, low, span, other, radius);
            }
        }
        (conflicts, pairs)
    }

    /// Whether another ring seat in this repository serves a commit that one
    /// of the two avatars shares.
    fn shares_ring_participant(&self, a: usize, b: usize) -> bool {
        let avatars = &self.state.avatars;
        let joins = |commit: usize, avatar: usize| {
            self.events
                .get(commit)
                .is_some_and(|e| e.participants().any(|c| c.key == avatars[avatar].key))
        };
        avatars[a].repository == avatars[b].repository
            && avatars[b]
                .slot
                .as_ref()
                .is_some_and(|slot| slot.offset != Vec2::ZERO)
            && (joins(avatars[a].commit, b) || joins(avatars[b].commit, a))
    }

    pub(in crate::model) fn reserve_service_slots(
        &mut self,
        check_walks: bool,
    ) -> bool {
        let _phase = self.trace.phase(Phase::Reserve);
        let mut previous = std::mem::take(&mut self.travel.occupancy);
        previous.retain(|&(commit, _), _| commit < self.events.len());
        let mut occupied = Reservations::new();
        let mut conflicted = BTreeSet::new();
        let visuals =
            std::mem::replace(&mut self.visuals, crate::model::Visuals::Seating);
        self.restart();
        let mut conflicts = 0;
        let mut blocked = 0;
        let mut pairs = 0;
        let frames = crate::model::frames_for(self.duration(), 60)
            .expect("bounded schedule")
            + 144;
        let begin = u32::try_from(self.state.tick).expect("bounded window");
        let mut arrivals: Vec<_> = self
            .travel
            .trips
            .iter()
            .filter(|trip| {
                check_walks
                    && trip.kind != super::TripKind::Relocation
                    && !trip.fade
                    && !trip.route.is_empty()
                    && trip.commit >= self.travel.frozen
                    && trip.arrival >= u64::from(begin)
            })
            .map(|trip| {
                (
                    trip.arrival,
                    trip.commit,
                    trip.author,
                    trip.destination_repository,
                )
            })
            .collect();
        arrivals.sort_unstable();
        let mut arrivals = arrivals.into_iter().peekable();
        let mut missed = 0;
        let mut positions = Vec::new();
        self.trace.count_replay((frames - begin).into());
        for frame in begin..=frames {
            self.progress.report_range(
                crate::progress::Stage::Reservations,
                frame.into(),
                begin.into(),
                frames.into(),
            );
            self.advance_to(frame, 60);
            if check_walks {
                missed += self.reserve_blocked_walks(&mut positions);
            }
            while let Some((_, commit, author, repository)) =
                arrivals.next_if(|&(tick, ..)| tick <= self.state.tick)
            {
                missed +=
                    usize::from(self.reserve_missed_dock(commit, author, repository));
            }
            let (conflicting, compared) =
                self.reserve_frame(&mut occupied, &mut conflicted);
            conflicts += conflicting;
            blocked += self.reserve_blocked_ray_sources(&mut occupied, &mut conflicted);
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
                let kept = previous.entry(key).or_default();
                let mut combined = Row::default();
                for (edge, rect) in kept.iter().copied().chain(row.into_row()) {
                    combined.merge(edge, rect);
                }
                *kept = combined.into_row();
            }
        }
        self.travel.occupancy = previous;
        if self.trace.on() {
            eprintln!("committing_overlap_observations={conflicts}");
            eprintln!("blocked_ray_source_observations={blocked}");
        }
        conflicts > 0 || blocked > 0 || missed > 0
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
            "{:?}{:?}{:?}{:?}{:?}",
            self.trips,
            self.docks,
            self.occupancy,
            self.unreached_docks,
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
            self.align_travel_arrivals(options);
            if !self.reserve_service_slots(
                options.travel_cap != crate::model::TravelCap::None,
            ) {
                break;
            }
            self.plan_travel(options, padding);
            let adjusted = self.adjust_travel_padding(options, padding);
            if adjusted != padding
                && (options.travel_cap == crate::model::TravelCap::None
                    || self.travel.unsettled_relocation())
            {
                return adjusted;
            }
            if options.replay_everything {
                continue;
            }
            let round = self.travel.round_state(self.window.as_ref());
            if repeated_round(previous.as_deref(), &round) {
                self.trace.count(Counter::SettledRefinements, 1);
                break;
            }
            previous = Some(round);
        }
        padding.to_vec()
    }
}

fn repeated_round(previous: Option<&str>, round: &str) -> bool {
    previous == Some(round) && !round.contains("NaN")
}

#[cfg(test)]
mod tests {
    use super::{Rect, Vec2};

    #[test]
    fn refinement_settles_only_when_a_comparable_round_repeats() {
        assert!(!super::repeated_round(None, "dock 1"));
        assert!(!super::repeated_round(Some("dock 1"), "dock 2"));
        assert!(super::repeated_round(Some("dock 1"), "dock 1"));
        assert!(!super::repeated_round(Some("dock NaN"), "dock NaN"));
    }

    #[test]
    fn a_passing_walker_does_not_move_a_parked_ring() {
        let mut events = crate::timeline::synthetic();
        events.truncate(3);
        events[0].co_authors.push(crate::timeline::Identity {
            name: b"Pair".to_vec(),
            email: b"pair@example.invalid".to_vec(),
        });
        let mut sim = super::Simulation::new(
            events,
            &crate::model::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        sim.advance_to(45, 60);
        let avatars = &mut sim.state.avatars;
        let lead = avatars
            .iter()
            .position(|avatar| avatar.name == "Ada")
            .unwrap();
        assert_ne!(avatars[lead].slot.as_ref().unwrap().offset, Vec2::ZERO);
        let mut passer = avatars[lead].clone();
        passer.key = sim.events[2].participants().next().unwrap().key.clone();
        passer.commit = 2;
        passer.group = None;
        passer.slot = None;
        let walker = avatars.len();
        avatars.push(passer);
        let point = super::occlusion::screen(&sim, sim.state.avatars[lead].position);
        let reservations = |sim: &super::Simulation| {
            let mut occupied = super::Reservations::new();
            let (conflicts, _) =
                sim.reserve_frame(&mut occupied, &mut super::BTreeSet::new());
            let rows: Vec<_> = occupied
                .into_iter()
                .map(|(key, row)| (key, row.into_row()))
                .collect();
            (conflicts, rows)
        };
        assert!(
            reservations(&sim).0 > 0,
            "a stationary avatar still conflicts"
        );
        let presence = sim.state.avatars[walker].presence;
        sim.state.avatars[walker].presence = 0.0;
        let absent = reservations(&sim);
        sim.state.avatars[walker].presence = presence;

        sim.travel.trips.push(super::super::Trip {
            author: walker,
            commit: 2,
            departure: sim.state.tick,
            arrival: sim.state.tick + 2,
            destination: point + Vec2::X,
            source: point,
            fade_ticks: 0,
            route: super::super::Route::new(&[point, point + Vec2::X], 480.0),
            kind: super::super::TripKind::Transfer,
            fade: false,
            bot: false,
            source_repository: 0,
            destination_repository: 0,
            free: sim.state.tick,
            failure: None,
            cutoff: None,
            proof: None,
        });
        let passing = reservations(&sim);
        assert_eq!(passing.0, 0);
        assert_eq!(passing, absent);
    }

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
    fn a_flat_rect_whose_margin_rounds_away_stays_apart() {
        // At 512 the whole margin rounds away, so a flat rect meets nothing.
        let wide = Rect::new(511.0, 0.0, 513.0, 1.0);
        let flat = Rect::new(512.0, 0.0, 512.0, 1.0);
        let mut row = super::Row::default();
        let mut reference = Vec::new();
        for rect in [wide, flat] {
            row.merge(0, rect);
            reference_merge(&mut reference, 0, rect);
        }
        assert_eq!(reference.len(), 2);
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
        let keys = [events[0].contributor.clone().unwrap().key];
        let delayed = || {
            let mut row = super::Row::default();
            row.merge(reservation.0, reservation.1);
            (
                super::Reservations::from([((3, 0), row)]),
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
        assert!(
            rows.into_values()
                .all(|row| row.into_row() == [reservation])
        );

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
    fn a_parked_avatar_reserves_for_each_foreign_avatar() {
        let (events, labels) = crate::model::tests::short_repository_history(1);
        let mut sim = super::Simulation::with_repositories(
            events,
            &crate::model::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &labels,
        );
        sim.advance_to(120, 60);
        let parked = sim
            .state
            .avatars
            .iter()
            .position(|avatar| avatar.slot.is_some())
            .unwrap();
        let position = sim.state.avatars[parked].position;
        let point = super::occlusion::screen(&sim, position);
        let commit = sim.state.avatars[parked].commit;
        let tile = sim.state.layout.repositories[0].gutter.unwrap();
        let mut rows = super::Reservations::new();
        let mut conflicted = super::BTreeSet::new();
        for (index, avatar) in sim.state.avatars.iter_mut().enumerate() {
            avatar.presence = f32::from(u8::from(index == parked));
            if index == parked {
                avatar.slot.as_mut().unwrap().point = point;
            } else {
                avatar.slot = None;
            }
        }
        assert_eq!(sim.reserve_frame(&mut rows, &mut conflicted), (0, 0));
        assert!(rows.is_empty());
        for avatar in &mut sim.state.avatars {
            avatar.presence = 1.0;
            avatar.position = position;
        }
        assert_eq!(sim.reserve_frame(&mut rows, &mut conflicted), (2, 2));
        assert_eq!(conflicted, super::BTreeSet::from([(commit, parked)]));
        let mut expected = super::Row::default();
        for _ in 0..2 {
            super::reserve_other(
                &mut expected,
                tile.low * super::HALF,
                (tile.high - tile.low) * super::HALF,
                point,
                sim.state.avatar_radius(),
            );
        }
        assert_eq!(rows.len(), 1);
        let actual = rows.remove(&(commit, parked)).unwrap();
        assert!(expected.stamp > 0);
        assert_eq!(actual.stamp, expected.stamp);
        assert_eq!(bits(actual.into_row()), bits(expected.into_row()));
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
