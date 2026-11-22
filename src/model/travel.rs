use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap},
    mem::size_of,
};

use bevy::math::{Rect, Vec2};
use serde::Serialize;

use super::{
    Simulation, frames_for, slots, tick_time, ticks_duration,
    trace::{Counter, Phase},
};
use crate::progress::Stage;
mod occupancy;
mod planning;
use planning::trips;

const HALF: Vec2 = Vec2::new(640.0, 360.0);
const NETWORK_EPSILON: f32 = 0.01;
const JUNCTION_EPSILON: f32 = 0.001;
const MAX_ROUTE_NODES: usize = 10_000;

const MOVING_DOCK: &str = "moving_dock";

pub(super) fn field_rect(tile: crate::layout::packing::Tile) -> Rect {
    Rect::from_corners(tile.low * HALF, tile.high * HALF)
}

fn ground_position(point: Vec2) -> [f32; 2] {
    super::framing::field_to_world(point / HALF).to_array()
}

pub(super) fn interior(tile: crate::layout::packing::Tile, radius: f32) -> Rect {
    let rect = Rect::from_corners(tile.low * HALF, tile.high * HALF);
    let half = rect.size().min_element() * 0.5;
    let inset = (2.0 * radius + 0.1).min((half - 0.1).max(half * 0.5));
    let guard = rect.inflate(-inset);
    if guard.min.cmple(rect.min).any() || guard.max.cmpge(rect.max).any() {
        Rect::from_center_size(rect.center(), Vec2::ZERO)
    } else {
        guard
    }
}

pub(super) fn in_guard(
    tile: crate::layout::packing::Tile,
    radius: f32,
    point: Vec2,
) -> bool {
    let guard = interior(tile, radius);
    guard.size().cmpgt(Vec2::ZERO).all() && guard.contains(point)
}

pub(super) fn seat_clearance(
    from: Vec2,
    mut target: Vec2,
    radius: f32,
    foreign_tiles: impl Iterator<Item = crate::layout::packing::Tile>,
) -> Vec2 {
    for tile in foreign_tiles {
        let guard = interior(tile, radius);
        if !guard.size().cmpgt(Vec2::ZERO).all()
            || !guard.contains(target)
            || guard.contains(from)
        {
            continue;
        }
        let mut inside = target;
        let mut outside = from;
        for _ in 0..12 {
            let middle = inside.midpoint(outside);
            if guard.contains(middle) {
                inside = middle;
            } else {
                outside = middle;
            }
        }
        target = outside;
    }
    target
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Pose {
    tick: u64,
    point: Vec2,
    dock: slots::Dock,
    visible: bool,
}

#[derive(Clone, Debug)]
pub(super) struct Service {
    pub(super) commit: usize,
    pub(super) author: usize,
    repository: usize,
    tick: u64,
    release: u64,
    point: Vec2,
    radius: f32,
    bot: bool,
    dock: slots::Dock,
}

#[derive(Debug, Default)]
struct Capture {
    begin: u64,
    frozen: usize,
    births: Vec<usize>,
    field: Rect,
    services: Vec<Service>,
    poses: Vec<Vec<Pose>>,
    tiles: Vec<Vec<Option<crate::layout::packing::Tile>>>,
    radii: Vec<f32>,
    gutters: Vec<Vec<Option<crate::layout::packing::Tile>>>,
}

impl Capture {
    fn bytes(&self) -> usize {
        self.gutters
            .iter()
            .chain(&self.tiles)
            .map(|r| {
                r.capacity()
                    * std::mem::size_of::<Option<crate::layout::packing::Tile>>()
            })
            .sum::<usize>()
            + self
                .poses
                .iter()
                .map(|p| p.capacity() * std::mem::size_of::<Pose>())
                .sum::<usize>()
            + self.tiles.capacity()
                * std::mem::size_of::<Vec<Option<crate::layout::packing::Tile>>>()
            + self.services.capacity() * std::mem::size_of::<Service>()
            + self.radii.capacity() * std::mem::size_of::<f32>()
            + self.poses.capacity() * std::mem::size_of::<Vec<Pose>>()
    }
}

#[derive(Clone, Debug)]
struct Leg {
    from: Vec2,
    to: Vec2,
    start: f32,
    duration: f32,
    ramp: f32,
    peak: f32,
}

impl Leg {
    fn new(from: Vec2, to: Vec2, start: f32, speed: f32) -> Self {
        let length = from.distance(to);
        let acceleration = speed * 5.0;
        let peak = speed.min((length * acceleration).sqrt());
        let ramp = peak / acceleration;
        let duration = if peak > 0.0 {
            length / peak + ramp
        } else {
            0.0
        };
        Self {
            from,
            to,
            start,
            duration,
            ramp,
            peak,
        }
    }

    fn sample(&self, elapsed: f32) -> Vec2 {
        let t = (elapsed - self.start).clamp(0.0, self.duration);
        let acceleration = self.peak / self.ramp.max(1e-8);
        let length = self.from.distance(self.to);
        let distance = if t < self.ramp {
            0.5 * acceleration * t * t
        } else if t > self.duration - self.ramp {
            length - 0.5 * acceleration * (self.duration - t).powi(2)
        } else {
            self.peak * (t - self.ramp * 0.5)
        };
        self.from
            .lerp(self.to, (distance / length.max(1e-8)).clamp(0.0, 1.0))
    }
}

#[derive(Clone, Debug, Default)]
struct Route {
    legs: Vec<Leg>,
    duration: f32,
    track: std::sync::Arc<[Vec2]>,
}

impl Route {
    fn retained_bytes(&self) -> usize {
        self.legs.capacity() * size_of::<Leg>() + self.track.len() * size_of::<Vec2>()
    }

    fn is_empty(&self) -> bool {
        self.legs.is_empty() && self.track.is_empty()
    }
    fn point_at(&self, tick: u64) -> Option<Vec2> {
        if self.track.is_empty() {
            self.sample(ticks_duration(tick))
        } else {
            Some(*at_tick(&self.track, tick))
        }
    }
    fn points(&self) -> Vec<[f32; 2]> {
        if self.track.is_empty() {
            self.legs
                .iter()
                .map(|l| l.from.to_array())
                .chain(self.legs.last().map(|l| l.to.to_array()))
                .collect()
        } else {
            self.track
                .iter()
                .step_by((self.track.len() / 64).max(1))
                .chain(self.track.last())
                .map(Vec2::to_array)
                .collect()
        }
    }
    fn new(points: &[Vec2], speed: f32) -> Self {
        let mut route = Self::default();
        for points in points.windows(2) {
            let leg = Leg::new(points[0], points[1], route.duration, speed);
            route.duration += leg.duration;
            route.legs.push(leg);
        }
        route
    }

    fn sample(&self, elapsed: f32) -> Option<Vec2> {
        self.legs
            .iter()
            .find(|l| elapsed < l.start + l.duration)
            .or_else(|| self.legs.last())
            .map(|l| l.sample(elapsed))
    }
}

#[derive(Clone, Debug)]
pub(super) struct DockWalk {
    start: u64,
    route: Route,
}

impl DockWalk {
    pub(super) fn new(
        start: u64,
        [from, source]: [Vec2; 2],
        [target, seat]: [Vec2; 2],
        cells: &[Rect],
        speed: f32,
    ) -> Option<Self> {
        let mut points = Router::default().route(source, target, cells).ok()?;
        if points
            .first()
            .is_some_and(|point| point.distance(from) > 0.001)
        {
            points.insert(0, from);
        }
        if points
            .last()
            .is_some_and(|point| point.distance(seat) > 0.001)
        {
            points.push(seat);
        }
        let route = Route::new(&points, speed);
        (route.duration > 0.0).then_some(Self { start, route })
    }

    pub(super) fn sample(&self, tick: u64) -> Option<Vec2> {
        let elapsed = ticks_duration(tick - self.start);
        (elapsed < self.route.duration)
            .then(|| self.route.sample(elapsed))
            .flatten()
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.route.retained_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TripKind {
    Transfer,
    Birth,
    Relocation,
}

#[derive(Clone, Debug)]
struct Trip {
    author: usize,
    commit: usize,
    departure: u64,
    arrival: u64,
    destination: Vec2,
    source: Vec2,
    fade_ticks: u64,
    route: Route,
    kind: TripKind,
    fade: bool,
    bot: bool,
    source_repository: usize,
    destination_repository: usize,
    free: u64,
    failure: Option<&'static str>,
    cutoff: Option<u64>,
    proof: Option<serde_json::Value>,
}

fn retained_route_bytes(trips: &Vec<Trip>) -> usize {
    trips.capacity() * size_of::<Trip>()
        + trips
            .iter()
            .map(|trip| trip.route.retained_bytes())
            .sum::<usize>()
}

#[derive(Clone, Default, Serialize)]
pub(super) struct Statistics {
    pub human_complete: usize,
    pub human_partial: usize,
    pub human_no_route: usize,
    pub bot_fades: usize,
    pub max_speed: f32,
    pub idle_human_reentries: usize,
    pub routed: usize,
    pub faded: usize,
    pub birth_fades: usize,
    pub blocked: usize,
    pub partial: usize,
    pub graph_epochs: usize,
    pub padding_seconds: f32,
    pub temporary_capture_bytes: usize,
    pub retained_route_bytes: usize,
}

#[derive(Clone, Default)]
pub(super) struct Plan {
    trips: Vec<Trip>,
    /// Planned docks by commit and avatar.
    pub(super) docks: BTreeMap<(usize, usize), slots::Dock>,
    pub(super) replanning: bool,
    pub(super) frozen: usize,
    /// Reserved gutter spans by commit and avatar.
    pub(super) occupancy: BTreeMap<(usize, usize), Vec<(u8, Rect)>>,
    pub stats: Statistics,
}

impl Plan {
    pub(super) fn replay_bytes(&self) -> u128 {
        let trips = self.trips.capacity() * size_of::<Trip>();
        let routes = self
            .trips
            .iter()
            .map(|trip| {
                let proof = trip.proof.as_ref().map_or(0, |proof| {
                    let points = proof["network_points"].as_array().map_or(0, Vec::len);
                    // The JSON object and each nested coordinate array have separate allocations.
                    2048 + points * 256
                });
                (trip.route.retained_bytes() + proof + 32) as u128
            })
            .sum::<u128>();
        let occupancy = self
            .occupancy
            .values()
            .map(|spans| (128 + spans.capacity() * size_of::<(u8, Rect)>()) as u128)
            .sum::<u128>();
        trips as u128 + routes + occupancy + self.docks.len() as u128 * 128
    }

    #[cfg(test)]
    pub(super) fn force_unrouteable(&mut self) -> usize {
        let trip = self.trips.last_mut().expect("fixture has a trip");
        trip.fade = true;
        trip.commit
    }
    pub(super) fn trip_count(&self) -> usize {
        self.trips.len()
    }
    pub(super) fn unsettled_relocation(&self) -> bool {
        self.trips.iter().any(|trip| {
            trip.commit >= self.frozen && trip.fade && trip.kind == TripKind::Relocation
        })
    }
    /// The first commit whose trip fades although its policy walks; a bot under
    /// `--bot-travel fade` fades by design.
    pub(super) fn first_unresolved(&self, bots: super::BotTravel) -> Option<usize> {
        self.trips
            .iter()
            .find(|t| t.fade && (!t.bot || bots == super::BotTravel::Walk))
            .map(|t| t.commit)
    }

    pub(super) fn protect_birth_walks(&self, history: &mut super::Schedule) -> bool {
        let mut changed = false;
        for trip in self.trips.iter().filter(|trip| {
            trip.commit >= self.frozen && trip.fade && trip.failure == Some(MOVING_DOCK)
        }) {
            changed |= history.protect_walk_after(trip.commit);
        }
        changed
    }

    pub(super) fn require_birth_routes(
        &self,
        bots: super::BotTravel,
    ) -> anyhow::Result<()> {
        if let Some(trip) = self.trips.iter().find(|trip| {
            trip.kind == TripKind::Birth
                && trip.fade
                && (!trip.bot || bots == super::BotTravel::Walk)
        }) {
            anyhow::bail!(
                "no safe gutter walk to a repository's first commit (event {})",
                trip.commit
            );
        }
        Ok(())
    }

    pub(super) fn approaching_departure(&self, author: usize, tick: u64) -> bool {
        let horizon = u64::from(seconds_ticks(super::SETTLE_SECONDS, true));
        self.trips.iter().any(|trip| {
            trip.author == author
                && !trip.fade
                && !trip.route.is_empty()
                && trip.free <= tick
                && trip.departure > tick
                && trip.departure - tick <= horizon
        })
    }

    pub(super) fn freeze_through(&mut self, tick: u64, applied: usize) {
        self.frozen = self
            .trips
            .iter()
            .filter(|t| t.departure < tick)
            .map(|t| t.commit + 1)
            .max()
            .unwrap_or(0)
            .max(applied)
            .max(self.frozen);
    }
    pub(super) fn preferences(self) -> Self {
        Self {
            docks: self.docks,
            occupancy: self.occupancy,
            frozen: self.frozen,
            trips: self
                .trips
                .into_iter()
                .filter(|t| t.commit < self.frozen)
                .collect(),
            ..Self::default()
        }
    }
    pub fn report(&self) -> serde_json::Value {
        let mut stats =
            serde_json::to_value(&self.stats).expect("serializable travel statistics");
        stats["max_speed"] = serde_json::json!(
            self.stats.max_speed * super::framing::WORLD_UNITS_PER_REFERENCE_PIXEL
        );
        serde_json::json!({"coordinate_space":"world_ground","statistics":stats,"trips":self.trips.iter().map(|t| serde_json::json!({"author":t.author,"commit":t.commit,"bot":t.bot,"source_repository":t.source_repository,"destination_repository":t.destination_repository,"free":tick_time(t.free).lossy_secs_f32(),"slack":ticks_duration(t.arrival-t.free),"departure":tick_time(t.departure).lossy_secs_f32(),"service":tick_time(t.arrival).lossy_secs_f32(),"fade":t.fade,"failure":t.failure,"proof":t.proof,"source":ground_position(t.source),"target":ground_position(t.destination),"visible_distance":planning::progress(t) * super::framing::WORLD_UNITS_PER_REFERENCE_PIXEL,"travel_seconds":t.route.duration,"points":t.route.points().into_iter().map(|point| ground_position(Vec2::from_array(point))).collect::<Vec<_>>()})).collect::<Vec<_>>()})
    }
    pub fn sample(&self, author: usize, tick: u64) -> Option<(Vec2, f32)> {
        let trip = self.trips.iter().find(|t| t.active(author, tick))?;
        Some(trip.sample(tick))
    }

    pub(super) fn forecast(&self, author: usize, tick: u64) -> Option<Vec<Vec2>> {
        let trips: Vec<_> = self
            .trips
            .iter()
            .filter(|t| t.author == author && t.arrival > tick)
            .collect();
        let trip = trips.first()?;
        if trip.fade || trip.departure > tick + 120 {
            return None;
        }
        Some(
            (1..=30_u64)
                .map(|step| {
                    let sample = tick + step * 4;
                    let Some(trip) = trips.iter().find(|t| t.arrival > sample) else {
                        return trips.last().expect("future trip").destination;
                    };
                    if sample < trip.departure || trip.fade {
                        trip.source
                    } else {
                        trip.sample(sample).0
                    }
                })
                .collect(),
        )
    }
    pub fn travelling(&self, author: usize, tick: u64) -> bool {
        self.trips.iter().any(|t| t.active(author, tick))
    }
    pub(super) fn walking(&self, author: usize, tick: u64) -> bool {
        self.walking_trip(author, tick).is_some()
    }

    pub(super) fn walking_forecast(
        &self,
        author: usize,
        tick: u64,
        ahead: [u64; 3],
    ) -> impl Iterator<Item = (u64, Vec2)> {
        self.walking_trip(author, tick)
            .into_iter()
            .flat_map(move |trip| {
                ahead.map(|offset| {
                    let sample = (tick + offset).min(trip.arrival - 1);
                    (sample - tick, trip.sample(sample).0)
                })
            })
    }

    fn walking_trip(&self, author: usize, tick: u64) -> Option<&Trip> {
        self.trips.iter().find(|t| {
            t.author == author
                && t.departure <= tick
                && tick < t.arrival
                && !t.fade
                && !t.route.is_empty()
        })
    }

    pub(super) fn departure_connector(&self, author: usize, tick: u64) -> Option<Vec2> {
        self.walking_trip(author, tick)
            .filter(|trip| {
                ticks_duration(tick - trip.departure) < super::SETTLE_SECONDS
            })
            .map(|trip| trip.source)
    }
}

impl Trip {
    fn active(&self, author: usize, tick: u64) -> bool {
        self.author == author
            && self.departure <= tick
            && tick < self.arrival
            && !(self.fade && self.kind == TripKind::Relocation)
            && (self.fade || !self.route.is_empty())
    }
    fn sample(&self, tick: u64) -> (Vec2, f32) {
        let trip = self;
        if trip.fade {
            if tick + 1 >= trip.arrival {
                return (trip.source, 0.0);
            }
            let duration = trip
                .fade_ticks
                .clamp(1, 9)
                .min((trip.arrival - trip.departure).max(1));
            return (
                trip.source,
                fade_opacity(duration.saturating_sub(tick - trip.departure), duration),
            );
        }
        (
            trip.route
                .point_at(tick - trip.departure)
                .unwrap_or(trip.destination),
            1.0,
        )
    }
}

pub(super) fn committing_overlap(
    a: Vec2,
    b: Vec2,
    diameter: f32,
    visible: [bool; 2],
    applying: [bool; 2],
) -> bool {
    visible.into_iter().all(|v| v)
        && applying.into_iter().any(|v| v)
        && a.distance(b) < diameter
}
fn fade_opacity(remaining: u64, duration: u64) -> f32 {
    (ticks_duration(remaining.saturating_sub(1)) / ticks_duration(duration.max(1)))
        .min(1.0)
}

fn capture(simulation: &mut Simulation) -> Capture {
    let _phase = simulation.trace.phase(Phase::Capture);
    let started = std::time::Instant::now();
    let frames = frames_for(simulation.duration(), 60).expect("validated travel range");
    let capture = replay(simulation, frames);
    if simulation.trace.on() {
        eprintln!(
            "travel_capture_seconds={:.6} frames={frames} poses={}",
            started.elapsed().as_secs_f64(),
            capture.poses.iter().map(Vec::len).sum::<usize>()
        );
    }
    capture
}

fn replay(simulation: &mut Simulation, frames: u32) -> Capture {
    simulation.restart();
    simulation
        .trace
        .count_replay(u64::from(frames).saturating_sub(simulation.state.tick));
    let mut capture = Capture {
        begin: simulation.state.tick,
        frozen: simulation.travel.frozen,
        births: simulation
            .schedule
            .births()
            .map(|(index, _)| index)
            .collect(),
        field: field_rect(simulation.state.field),
        services: simulation
            .window
            .as_ref()
            .map_or_else(Vec::new, |w| w.services.clone()),
        ..Capture::default()
    };
    let visuals = std::mem::replace(&mut simulation.visuals, super::Visuals::None);
    let docks = if simulation.travel.occupancy.is_empty() && simulation.window.is_none()
    {
        std::mem::take(&mut simulation.travel.docks)
    } else {
        simulation.travel.docks.clone()
    };
    simulation.travel.replanning = true;
    let mut seen = simulation.state.next_event;
    for frame in u32::try_from(capture.begin).expect("bounded window")..=frames {
        simulation.progress.report_range(
            Stage::Travel,
            frame.into(),
            capture.begin,
            frames.into(),
        );
        simulation.advance_to(frame, 60);
        let state = &simulation.state;
        capture.radii.push(state.avatar_radius());
        capture
            .gutters
            .push(state.layout.repositories.iter().map(|r| r.gutter).collect());
        capture
            .tiles
            .push(state.layout.repositories.iter().map(|r| r.tile).collect());
        capture.poses.resize_with(state.avatars.len(), Vec::new);
        for (index, avatar) in state.avatars.iter().enumerate() {
            let visible = avatar.slot_presence > 0.0;
            if let Some(slot) = &avatar.slot
                && capture.poses[index].last().is_none_or(|last| {
                    last.point != slot.centre()
                        || last.dock != slot.dock
                        || last.visible != visible
                })
            {
                capture.poses[index].push(Pose {
                    tick: state.tick,
                    point: slot.centre(),
                    dock: slot.dock,
                    visible,
                });
            }
        }
        for event in seen..state.next_event {
            for contributor in simulation.events[event].participants() {
                let author = state.authors[&contributor.key];
                let avatar = &state.avatars[author];
                let slot = avatar.slot.as_ref().expect("tile exists before service");
                capture.services.push(Service {
                    commit: event,
                    author,
                    repository: avatar.repository,
                    tick: state.tick,
                    release: frames_for(
                        state
                            .pulses
                            .iter()
                            .find(|p| p.commit == event)
                            .expect("new pulse")
                            .end
                            - 0.3,
                        60,
                    )
                    .expect("bounded pulse")
                    .into(),
                    point: slot.centre(),
                    radius: state.avatar_radius(),
                    bot: contributor.is_bot(),
                    dock: slot.dock,
                });
            }
        }
        seen = state.next_event;
    }
    simulation.restart();
    simulation.visuals = visuals;
    simulation.travel.docks = docks;
    simulation.travel.replanning = false;
    simulation.trace.add_ticks(&mut simulation.tick_times);
    capture
}

fn fade_clearance(capture: &Capture, begin: u64, point: Vec2, radius: f32) -> u64 {
    for offset in 0..=9_u64 {
        let tick = begin + offset;
        let row = at_tick(&capture.tiles, tick.saturating_sub(capture.begin));
        let tile_collision = row
            .iter()
            .flatten()
            .any(|tile| in_guard(*tile, radius, point));
        if tile_collision {
            return offset;
        }
    }
    9
}

impl Simulation {
    pub(super) fn align_travel_arrivals(&mut self, options: &super::Options) {
        if !self
            .travel
            .trips
            .iter()
            .any(|trip| planning::needs_alignment(trip, self.travel.frozen))
        {
            return;
        }
        // Service slots and their padding are fixed before choosing later departures.
        let capture = capture(self);
        let _phase = self.trace.phase(Phase::Routes);
        let epochs = planning::align_arrivals(
            &capture,
            &mut self.travel.trips,
            options.avatar_max_speed,
        );
        self.trace.count(Counter::RouteCandidates, epochs);
        self.travel.stats.graph_epochs += epochs;
        self.travel.stats.retained_route_bytes =
            retained_route_bytes(&self.travel.trips);
    }

    fn routes(
        &self,
        capture: &Capture,
        options: &super::Options,
    ) -> (Vec<Trip>, usize) {
        let _phase = self.trace.phase(Phase::Routes);
        let (trips, epochs) = trips(
            capture,
            options.avatar_max_speed,
            options.bot_travel,
            options.travel_cap == super::TravelCap::None,
        );
        self.trace.count(Counter::RouteCandidates, epochs);
        (trips, epochs)
    }

    pub(super) fn adjust_travel_padding(
        &self,
        options: &super::Options,
        padding: &[(usize, u32)],
    ) -> Vec<(usize, u32)> {
        let _phase = self.trace.phase(Phase::AdjustPadding);
        let mut result = padding.to_vec();
        // Co-authors of one commit share its padding, so it covers the largest deficit.
        let mut deficits = std::collections::BTreeMap::<usize, (u32, u32)>::new();
        for trip in self.travel.trips.iter().filter(|t| {
            t.commit >= self.travel.frozen
                && (t.kind == TripKind::Relocation
                    || !t.bot
                    || options.bot_travel == super::BotTravel::Walk)
                && t.fade
        }) {
            let policy = if trip.kind == TripKind::Relocation {
                super::TravelCap::None
            } else {
                options.travel_cap
            };
            let needed = if trip.route.is_empty() {
                policy.blocked_ticks()
            } else {
                seconds_ticks(
                    (trip.route.duration
                        - ticks_duration(trip.arrival - trip.departure))
                    .max(0.0),
                    true,
                )
            };
            let needed = if policy == super::TravelCap::None {
                needed.max(60)
            } else {
                needed
            }
            .min(policy.ticks());
            if needed == 0 {
                continue;
            }
            let (deficit, cap) = deficits.entry(self.offset + trip.commit).or_default();
            *deficit = (*deficit).max(needed);
            *cap = (*cap).max(policy.ticks());
        }
        for (commit, (needed, cap)) in deficits {
            if let Some((_, ticks)) = result.iter_mut().find(|(i, _)| *i == commit) {
                *ticks = ticks.saturating_add(needed).min(cap);
            } else {
                result.push((commit, needed));
            }
        }
        result.sort_unstable_by_key(|p| p.0);
        result
    }
    pub(super) fn compact_travel_padding(
        &self,
        padding: &[(usize, u32)],
    ) -> Vec<(usize, u32)> {
        let _phase = self.trace.phase(Phase::CompactPadding);
        let mut result = padding.to_vec();
        // Only slack every trip of a commit can spare is reclaimed; a faded trip has none.
        let mut slack = std::collections::BTreeMap::<usize, u32>::new();
        for trip in self
            .travel
            .trips
            .iter()
            .filter(|t| t.commit >= self.travel.frozen)
        {
            let surplus = if trip.fade {
                0
            } else {
                let ready = trip.departure
                    + u64::from(seconds_ticks(trip.route.duration, true));
                u32::try_from(trip.arrival.saturating_sub(ready))
                    .expect("bounded slack")
            };
            slack
                .entry(self.offset + trip.commit)
                .and_modify(|spare| *spare = (*spare).min(surplus))
                .or_insert(surplus);
        }
        for (commit, surplus) in slack {
            if let Some((_, ticks)) = result.iter_mut().find(|(i, _)| *i == commit) {
                *ticks = ticks.saturating_sub(surplus);
            }
        }
        result
    }
    pub(super) fn travel_padding(
        &mut self,
        options: &super::Options,
    ) -> Vec<(usize, u32)> {
        let _phase = self.trace.phase(Phase::TravelPadding);
        let capture = capture(self);
        let (routes, _) = self.routes(&capture, options);
        let mut shifts = vec![0_u64; self.events.len()];
        let mut extra = 0_u64;
        let mut padding = Vec::new();
        let mut routes = routes.iter().peekable();
        for commit in 0..self.events.len() {
            // Co-authors of one commit travel together, so it waits for the slowest.
            let mut slowest = None;
            while let Some(trip) = routes.next_if(|t| t.commit == commit) {
                let departure_event = self
                    .schedule
                    .starts
                    .partition_point(|&s| s <= tick_time(trip.free))
                    .saturating_sub(1);
                let policy = options.travel_cap;
                let needed = if trip.bot && options.bot_travel == super::BotTravel::Fade
                {
                    0
                } else if trip.route.is_empty() {
                    policy.blocked_ticks()
                } else {
                    seconds_ticks(
                        (tick_time(trip.departure)
                            + ticks_duration(shifts[departure_event])
                            + trip.route.duration
                            - tick_time(trip.arrival)
                            - ticks_duration(extra))
                        .max(0.0),
                        true,
                    )
                    .min(policy.ticks())
                };
                slowest = slowest.max(Some(needed));
            }
            if let Some(needed) = slowest {
                padding.push((self.offset + commit, needed));
                extra += u64::from(needed);
            }
            shifts[commit] = extra;
        }
        padding
    }

    pub(super) fn plan_travel(
        &mut self,
        options: &super::Options,
        padding: &[(usize, u32)],
    ) {
        let _phase = self.trace.phase(Phase::PlanTravel);
        let capture = capture(self);
        let (mut trips, graph_epochs) = self.routes(&capture, options);
        trips.extend(
            self.travel
                .trips
                .iter()
                .filter(|t| t.commit < self.travel.frozen)
                .cloned(),
        );
        trips.sort_by_key(|t| t.commit);
        let mut previous = vec![None; capture.poses.len()];
        let mut human_switches = 0;
        for service in &capture.services {
            if previous[service.author]
                .replace(service.repository)
                .is_some_and(|r| r != service.repository)
                && !service.bot
            {
                human_switches += 1;
            }
        }
        let humans = |keep: fn(&Trip) -> bool| {
            trips
                .iter()
                .filter(|t| {
                    !t.bot && t.source_repository != t.destination_repository && keep(t)
                })
                .count()
        };
        let stats = Statistics {
            idle_human_reentries: human_switches - humans(|_| true),
            human_complete: humans(|t| !t.fade),
            human_partial: humans(|t| t.fade && !t.route.is_empty()),
            human_no_route: humans(|t| t.fade && t.route.is_empty()),
            bot_fades: trips.iter().filter(|t| t.bot && t.fade).count(),
            max_speed: options.avatar_max_speed,
            temporary_capture_bytes: capture.bytes(),
            retained_route_bytes: retained_route_bytes(&trips),
            graph_epochs,
            routed: trips.iter().filter(|t| !t.fade).count(),
            faded: trips.iter().filter(|t| t.fade).count(),
            birth_fades: trips
                .iter()
                .filter(|t| t.kind == TripKind::Birth && t.fade)
                .count(),
            blocked: trips
                .iter()
                .filter(|t| !t.bot && t.route.is_empty())
                .count(),
            partial: trips
                .iter()
                .filter(|t| !t.bot && t.fade && !t.route.is_empty())
                .count(),
            padding_seconds: padding
                .iter()
                .map(|&(_, t)| ticks_duration(u64::from(t)))
                .sum(),
        };
        let mut docks = self.travel.docks.clone();
        docks.retain(|&(commit, _), _| commit < self.events.len());
        for service in &capture.services {
            docks.insert((service.commit, service.author), service.dock);
        }
        if let Some(window) = &mut self.window {
            window.captured.clone_from(&capture.services);
        }
        self.travel = Plan {
            trips,
            docks,
            stats,
            replanning: false,
            frozen: self.travel.frozen,
            occupancy: std::mem::take(&mut self.travel.occupancy),
        };
    }
}

pub(super) fn seconds_ticks(seconds: f32, round_up: bool) -> u32 {
    let ticks = std::time::Duration::from_secs_f32(seconds).as_nanos() * 60;
    let rounded = if round_up {
        ticks.div_ceil(1_000_000_000)
    } else {
        ticks / 1_000_000_000
    };
    u32::try_from(rounded).expect("bounded travel duration")
}

#[derive(Default)]
struct Router {
    epochs: usize,
}

pub(super) fn slot_seconds(
    source: Vec2,
    target: Vec2,
    cells: &[Rect],
    speed: f32,
) -> f32 {
    Router::default()
        .route(source, target, cells)
        .map_or(f32::INFINITY, |points| {
            Route::new(&points, speed * 0.8).duration
        })
}

#[derive(Debug)]
enum RouteFailure {
    Endpoint,
    Budget,
    Disconnected,
}
impl RouteFailure {
    fn name(&self) -> &'static str {
        match self {
            Self::Endpoint => "endpoint",
            Self::Budget => "search_budget",
            Self::Disconnected => "disconnected",
        }
    }
}

/// The row for `tick`, holding the last row once the capture ends.
fn at_tick<T>(rows: &[T], tick: u64) -> &T {
    &rows[usize::try_from(tick)
        .expect("bounded tick")
        .min(rows.len() - 1)]
}

/// Gutter tiles as reference-viewport pixel rectangles.
pub(super) fn network(
    gutters: impl Iterator<Item = crate::layout::packing::Tile>,
) -> Vec<Rect> {
    gutters
        .map(|t| Rect::from_corners(t.low * HALF, t.high * HALF))
        .collect()
}

pub(super) fn on_network(point: Vec2, cells: &[Rect]) -> bool {
    cells.iter().any(|r| {
        point.cmpge(r.min - Vec2::splat(JUNCTION_EPSILON)).all()
            && point.cmple(r.max + Vec2::splat(JUNCTION_EPSILON)).all()
            && (point - r.min)
                .abs()
                .min((point - r.max).abs())
                .min_element()
                < JUNCTION_EPSILON
    })
}

pub(super) fn join_gutter(
    from: Vec2,
    target: Vec2,
    source: Option<crate::layout::packing::Tile>,
    cells: &[Rect],
    step: f32,
) -> (Vec2, Option<Vec2>) {
    let entrance = if on_network(from, cells) {
        from
    } else if let Some(tile) = source {
        let rect = field_rect(tile);
        let closest = from.clamp(rect.min, rect.max);
        [
            Vec2::new(rect.min.x, closest.y),
            Vec2::new(rect.max.x, closest.y),
            Vec2::new(closest.x, rect.min.y),
            Vec2::new(closest.x, rect.max.y),
        ]
        .into_iter()
        .min_by(|a, b| {
            from.distance_squared(*a)
                .total_cmp(&from.distance_squared(*b))
        })
        .expect("four gutter edges")
    } else {
        return (from, None);
    };
    let Ok(path) = Router::default().route(entrance, target, cells) else {
        return (from, None);
    };
    let next = std::iter::once(entrance)
        .chain(path)
        .find(|point| point.distance_squared(from) > 0.00001)
        .unwrap_or(target);
    (
        from.move_towards(next, step),
        (next == entrance).then_some(entrance),
    )
}

impl Router {
    fn route(
        &mut self,
        source: Vec2,
        target: Vec2,
        cells: &[Rect],
    ) -> Result<Vec<Vec2>, RouteFailure> {
        self.epochs += 1;
        let snap = |mut point: Vec2| {
            for axis in 0..2 {
                if let Some(value) = cells
                    .iter()
                    .flat_map(|r| [r.min[axis], r.max[axis]])
                    .filter(|v| (*v - point[axis]).abs() < NETWORK_EPSILON)
                    .min_by(|a, b| {
                        (a - point[axis]).abs().total_cmp(&(b - point[axis]).abs())
                    })
                {
                    point[axis] = value;
                }
            }
            point
        };
        let source = snap(source);
        let target = snap(target);
        if !on_network(source, cells) || !on_network(target, cells) {
            return Err(RouteFailure::Endpoint);
        }
        let mut axes = [vec![source.x, target.x], vec![source.y, target.y]];
        for r in cells {
            for (axis, values) in axes.iter_mut().enumerate() {
                values.extend([r.min[axis], r.max[axis]]);
            }
        }
        for values in &mut axes {
            values.sort_by(f32::total_cmp);
            // Moving anchors need a shared junction for boundaries within the network tolerance.
            values.dedup_by(|a, b| (*a - *b).abs() < JUNCTION_EPSILON);
        }
        let (points, edges, start, end) = junctions(cells, source, target, &axes)?;
        let nodes = points.len();
        let mut distance = vec![u64::MAX; nodes];
        let mut previous = vec![usize::MAX; nodes];
        let mut queue = BinaryHeap::from([Reverse((0_u64, start))]);
        distance[start] = 0;
        while let Some(Reverse((cost, i))) = queue.pop() {
            if i == end {
                break;
            }
            if cost != distance[i] {
                continue;
            }
            for &next in edges[i].iter().flatten() {
                let a = points[i];
                let b = points[next];
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "bounded viewport length in millionths of a pixel"
                )]
                let candidate =
                    cost + (f64::from(a.distance(b)) * 1_000_000.0).round() as u64;
                if candidate < distance[next] {
                    distance[next] = candidate;
                    previous[next] = i;
                    queue.push(Reverse((candidate, next)));
                }
            }
        }
        if distance[end] == u64::MAX {
            return Err(RouteFailure::Disconnected);
        }
        let mut path = vec![points[end]];
        let mut node = end;
        while node != start {
            node = previous[node];
            path.push(points[node]);
        }
        path.reverse();
        Ok(simplify_path(&path))
    }
}

type Neighbours = [Option<usize>; 4];

fn junctions(
    cells: &[Rect],
    source: Vec2,
    target: Vec2,
    axes: &[Vec<f32>; 2],
) -> Result<(Vec<Vec2>, Vec<Neighbours>, usize, usize), RouteFailure> {
    let address = |p: Vec2| {
        let coordinate = |axis: usize| {
            axes[axis].partition_point(|v| *v <= p[axis] - JUNCTION_EPSILON)
        };
        (coordinate(1), coordinate(0))
    };
    let start = address(source);
    let end = address(target);
    // Every turn on a rectangular partition is at a tile corner or a trip endpoint.
    let mut vertices: Vec<_> = cells
        .iter()
        .flat_map(|r| {
            [
                r.min,
                r.max,
                Vec2::new(r.min.x, r.max.y),
                Vec2::new(r.max.x, r.min.y),
            ]
        })
        .map(address)
        .chain([start, end])
        .collect();
    vertices.sort_unstable();
    vertices.dedup();
    if vertices.len() > MAX_ROUTE_NODES {
        return Err(RouteFailure::Budget);
    }
    let points: Vec<_> = vertices
        .iter()
        .map(|&(y, x)| Vec2::new(axes[0][x], axes[1][y]))
        .collect();
    let mut edges = vec![[None; 4]; points.len()];
    let mut order: Vec<_> = (0..points.len()).collect();
    for axis in 0..2 {
        for pair in order.windows(2) {
            let [a, b] = [pair[0], pair[1]];
            if points[a][1 - axis].to_bits() == points[b][1 - axis].to_bits()
                && cells.iter().any(|cell| {
                    let normal = 1 - axis;
                    let middle = points[a][axis].midpoint(points[b][axis]);
                    let along = (cell.min[axis]..=cell.max[axis]).contains(&middle);
                    let across = points[a][normal];
                    along
                        && [cell.min[normal], cell.max[normal]]
                            .iter()
                            .any(|v| (across - v).abs() < JUNCTION_EPSILON)
                })
            {
                edges[a][axis * 2 + 1] = Some(b);
                edges[b][axis * 2] = Some(a);
            }
        }
        order.sort_unstable_by_key(|&i| (vertices[i].1, vertices[i].0));
    }
    Ok((
        points,
        edges,
        vertices.binary_search(&start).expect("inserted source"),
        vertices.binary_search(&end).expect("inserted target"),
    ))
}

#[cfg(test)]
pub(crate) fn assert_gutter_routes(
    gutters: impl Iterator<Item = crate::layout::packing::Tile>,
) -> Vec<Rect> {
    let cells = network(gutters);
    let target = cells[0].min;
    for cell in &cells {
        for point in [
            cell.min,
            cell.max,
            Vec2::new(cell.min.x, cell.max.y),
            Vec2::new(cell.max.x, cell.min.y),
        ] {
            Router::default()
                .route(point, target, &cells)
                .expect("connected gutters");
        }
    }
    cells
}

#[cfg(test)]
pub(super) fn assert_edge_connectivity(simulation: &Simulation) {
    let cells = assert_gutter_routes(
        simulation
            .state
            .layout
            .repositories
            .iter()
            .filter_map(|r| r.gutter),
    );
    let target = cells[0].min;
    for (author, avatar) in
        simulation
            .state
            .avatars
            .iter()
            .enumerate()
            .filter(|(i, a)| {
                a.presence > 0.0
                    && !simulation.travel.travelling(*i, simulation.state.tick)
            })
    {
        let point = avatar.slot.as_ref().expect("parked slot").point;
        assert!(
            on_network(point, &cells),
            "parked author {author} on gutter"
        );
        Router::default()
            .route(point, target, &cells)
            .expect("parked slot connects to every gutter");
    }
}
fn simplify_path(path: &[Vec2]) -> Vec<Vec2> {
    let mut result = vec![path[0]];
    for (index, &point) in path.iter().enumerate().skip(1) {
        let collinear = index + 1 < path.len() && {
            let before = path[index - 1];
            let after = path[index + 1];
            (before.x.to_bits() == point.x.to_bits()
                && point.x.to_bits() == after.x.to_bits())
                || (before.y.to_bits() == point.y.to_bits()
                    && point.y.to_bits() == after.y.to_bits())
        };
        if !collinear {
            result.push(point);
        }
    }
    result
}

impl Plan {
    pub(super) fn discard_before(
        &mut self,
        boundary: u64,
        next: usize,
        keep: &std::collections::BTreeSet<usize>,
    ) {
        self.trips.retain(|trip| trip.arrival > boundary);
        self.trips.shrink_to_fit();
        let kept =
            |&(commit, _): &(usize, usize)| commit >= next || keep.contains(&commit);
        self.docks.retain(|key, _| kept(key));
        self.occupancy.retain(|key, _| kept(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parked_avatars_stay_clear_during_seat_transitions() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        events[1].author = events[0].author.clone();
        events[1].seconds = events[0].seconds + 6 * 86_400;
        events[0].co_authors.push(crate::timeline::Identity::new(
            "Grace",
            "grace@example.test",
        ));
        let mut theme = crate::theme::Theme::neon();
        theme.avatar_radius = 0.29;
        let mut sim = Simulation::new(
            events,
            &super::super::Options {
                max_idle_seconds: 3.0,
                ..Default::default()
            },
            &theme,
            &crate::contributors::Mappings::default(),
        );
        let first = sim.schedule.commit_ticks().next().unwrap();
        sim.advance_to(u32::try_from(first + 24).unwrap(), 60);
        let slot = sim.state.avatars[1].slot.as_ref().unwrap();
        let normal = slots::world(slot.centre()) - slots::world(slot.point);
        assert!(normal.length() > 0.0);
        let source = slot.centre();
        let destination = source + slot.offset.normalize() * 80.0;
        let departure = first + 65;
        sim.travel.trips.push(Trip {
            author: 1,
            commit: 1,
            departure,
            arrival: departure + 40,
            destination,
            source,
            fade_ticks: 0,
            route: Route::new(&[source, destination], sim.state.avatar_max_speed),
            kind: TripKind::Relocation,
            fade: false,
            bot: false,
            source_repository: 0,
            destination_repository: 0,
            free: first + 25,
            failure: None,
            cutoff: None,
            proof: None,
        });
        sim.restart();

        let mut audit = super::super::occlusion::Audit::default();
        for frame in 0..=u32::try_from(sim.end_tick()).unwrap() {
            sim.advance_to(frame, 60);
            audit.observe(&sim);
            assert_eq!(audit.parked_avatar_observations, 0, "frame {frame}");
        }
    }

    #[test]
    fn holding_files_changes_no_capture_or_reservation() {
        let (events, labels) = super::super::tests::short_repository_history(6);
        let mut sim = Simulation::with_repositories(
            events,
            &super::super::Options {
                travel_cap: super::super::TravelCap::None,
                ..super::super::Options::default()
            },
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &labels,
        );
        let planned = sim.travel.clone();
        let [held, advanced] = [false, true].map(|advance| {
            sim.advance_all_files = advance;
            sim.travel = planned.clone();
            let captured = format!("{:?}", capture(&mut sim));
            let conflicts = sim.reserve_service_slots();
            (captured, conflicts, sim.travel.round_state(None))
        });
        assert_eq!(held, advanced);
    }

    #[test]
    fn routing_rejects_a_network_above_the_junction_budget() {
        let cells: Vec<_> = (0..2501_u16)
            .map(|i| {
                let low = Vec2::splat(f32::from(i) * 10.0);
                Rect::from_corners(low, low + Vec2::ONE)
            })
            .collect();
        let result = Router::default().route(Vec2::ZERO, Vec2::ONE, &cells);
        assert!(matches!(result, Err(RouteFailure::Budget)));
    }

    #[test]
    fn near_coincident_gutters_keep_route_segments_axis_aligned() {
        let cells = [
            Rect::from_corners(Vec2::new(-20.0, -10.0), Vec2::ZERO),
            Rect::from_corners(Vec2::new(0.0, -10.0), Vec2::new(20.0, 0.0004)),
        ];
        let points = Router::default()
            .route(Vec2::new(-10.0, 0.0), Vec2::new(10.0, 0.0004), &cells)
            .unwrap();
        assert!(points.windows(2).all(|pair| {
            pair[0].x.to_bits() == pair[1].x.to_bits()
                || pair[0].y.to_bits() == pair[1].y.to_bits()
        }));
        assert!(points.iter().all(|&point| on_network(point, &cells)));
        assert!(
            points.last().unwrap().distance(Vec2::new(10.0, 0.0004)) < JUNCTION_EPSILON
        );
    }
    #[test]
    fn a_slower_fragment_speed_holds_the_trip_until_the_last_fragment_lands() {
        let mut events = Vec::new();
        // The second tile is born before the trip's release and still reflows at it.
        for (index, (repository, day)) in
            [(0, 0), (1, 1), (1, 6)].into_iter().enumerate()
        {
            let mut event = crate::timeline::synthetic().remove(0);
            event.seconds = day * 86_400;
            if index == 1 {
                event.author.email = b"other@example.invalid".to_vec();
            }
            let mut history = vec![event];
            crate::repositories::namespace(&mut history, repository);
            events.extend(history);
        }
        let departures = [1.0, 0.5].map(|scale| {
            let options = super::super::Options {
                seconds_per_day: 1.0,
                max_idle_seconds: 30.0,
                travel_cap: super::super::TravelCap::None,
                fragment_speed: scale,
                ..Default::default()
            };
            let mut simulation = Simulation::build(
                events.clone(),
                &options,
                &crate::theme::Theme::neon(),
                &crate::contributors::Mappings::default(),
                &[],
                &["first".into(), "second".into()],
                &mut None,
            )
            .unwrap();
            let captured = capture(&mut simulation);
            let first = &captured.services[0];
            assert_eq!(first.author, 0);
            let end = options
                .fragment_pace()
                .pulse_end(simulation.schedule.starts[0]);
            assert_eq!(first.release, u64::from(frames_for(end - 0.3, 60).unwrap()));
            let (routes, _) = trips(&captured, 480.0, options.bot_travel, true);
            let route = routes.iter().find(|route| route.author == 0).unwrap();
            assert!(!route.fade);
            assert_eq!(route.departure, first.release);
            route.departure - first.tick
        });
        assert!(departures[1] > departures[0], "{departures:?}");
    }
    #[test]
    fn faded_contributor_returns_from_the_moved_gutter() {
        let mut events = Vec::new();
        for (index, repository) in [0, 1, 1].into_iter().enumerate() {
            let mut event = crate::timeline::synthetic().remove(0);
            event.seconds = i64::try_from(index).unwrap() * 20 * 86_400;
            if index == 1 {
                event.author.email = b"other@example.invalid".to_vec();
                let change = event.changes[0].clone();
                event.changes = (0..40)
                    .map(|file| crate::timeline::FileChange {
                        path: format!("large/file{file}.rs"),
                        ..change.clone()
                    })
                    .collect();
            }
            let mut history = vec![event];
            crate::repositories::namespace(&mut history, repository);
            events.extend(history);
        }
        let options = super::super::Options {
            seconds_per_day: 1.0,
            max_idle_seconds: 30.0,
            contributor_inactivity_days: 5,
            travel_cap: super::super::TravelCap::None,
            ..Default::default()
        };
        let mut simulation = Simulation::build(
            events,
            &options,
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &[],
            &["first".into(), "second".into()],
            &mut None,
        )
        .unwrap();
        let captured = capture(&mut simulation);
        let mut faded_move = false;
        let mut previous = None;
        for frame in 0..=frames_for(simulation.duration(), 60).unwrap() {
            simulation.advance_to(frame, 60);
            if let Some(avatar) = simulation.state.avatars.first()
                && let Some(slot) = &avatar.slot
            {
                let pose = captured.pose(0, u64::from(frame)).unwrap();
                assert_eq!(pose.point, slot.point);
                assert_eq!(pose.dock, slot.dock);
                faded_move |= avatar.presence == 0.0
                    && previous.is_some_and(|point| point != slot.point);
                previous = Some(slot.point);
            }
        }
        assert!(faded_move);
        let (routes, _) = trips(&captured, 480.0, options.bot_travel, true);
        let route = routes.iter().find(|route| route.author == 0).unwrap();
        assert!(!route.fade);
        assert_ne!(route.source, captured.poses[0][0].point);
        assert_eq!(
            route.source,
            captured.pose(0, route.departure).unwrap().point
        );
        assert!(captured.poses[0].len() < captured.radii.len() / 2);
    }

    #[test]
    fn nearby_gutter_boundaries_do_not_create_unanchorable_micro_legs() {
        let cells = [
            Rect::from_corners(Vec2::new(0.0, -300.0), Vec2::new(400.0, -0.000_005_2)),
            Rect::from_corners(Vec2::new(-400.0, -300.0), Vec2::new(0.0, 0.000_037_7)),
            Rect::from_corners(Vec2::new(-400.0, 0.000_037_7), Vec2::new(400.0, 300.0)),
        ];
        let source = Vec2::new(145.787_22, -0.000_005_2);
        let target = Vec2::new(-270.0, 300.0);
        let points = Router::default().route(source, target, &cells).unwrap();
        assert!(
            points
                .windows(2)
                .all(|p| p[0].distance(p[1]) >= JUNCTION_EPSILON),
            "{points:?}"
        );
        assert!(points.iter().all(|p| on_network(*p, &cells)));
    }

    #[test]
    fn projection_roundoff_does_not_create_a_false_endpoint_failure() {
        let cells = [Rect::from_corners(
            Vec2::new(-200.0, -100.0),
            Vec2::new(200.0, 100.0),
        )];
        let target = Vec2::new(200.0, 50.0);
        let points = Router::default()
            .route(Vec2::new(199.998, 50.0), target, &cells)
            .unwrap();
        assert_eq!(points, [target]);
        assert_eq!(
            slot_seconds(Vec2::new(199.998, 50.0), target, &cells, 480.0),
            0.0
        );
    }
    #[test]
    fn gutters_take_the_shortest_perimeter_instead_of_crossing_blank_tiles() {
        let cells = [
            Rect::from_corners(Vec2::new(-100.0, -50.0), Vec2::new(0.0, 50.0)),
            Rect::from_corners(Vec2::new(0.0, -50.0), Vec2::new(100.0, 50.0)),
        ];
        let mut router = Router::default();
        let route = router
            .route(Vec2::new(-100.0, 0.0), Vec2::new(100.0, 0.0), &cells)
            .unwrap();
        let length: f32 = route.windows(2).map(|p| p[0].distance(p[1])).sum();
        assert!((length - 300.0).abs() < 0.001);
        for pair in route.windows(2) {
            assert!(on_network(pair[0].midpoint(pair[1]), &cells));
        }
        let direct = router
            .route(Vec2::new(0.0, -50.0), Vec2::new(0.0, 50.0), &cells)
            .unwrap();
        assert_eq!(direct, [Vec2::new(0.0, -50.0), Vec2::new(0.0, 50.0)]);
        assert_eq!(
            route,
            router
                .route(Vec2::new(-100.0, 0.0), Vec2::new(100.0, 0.0), &cells)
                .unwrap()
        );
    }
    #[test]
    fn uncapped_blocked_routes_receive_another_wait_instead_of_fading() {
        let mut sim = Simulation::new(
            crate::timeline::synthetic(),
            &super::super::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        sim.travel.trips.push(Trip {
            author: 0,
            commit: 0,
            departure: 1,
            arrival: 10,
            destination: Vec2::X,
            source: Vec2::ZERO,
            fade_ticks: 0,
            route: Route::default(),
            kind: TripKind::Transfer,
            fade: true,
            bot: false,
            source_repository: 0,
            destination_repository: 1,
            free: 1,
            failure: Some("endpoint"),
            cutoff: None,
            proof: None,
        });
        let options = super::super::Options {
            travel_cap: super::super::TravelCap::None,
            ..Default::default()
        };
        assert_eq!(sim.adjust_travel_padding(&options, &[(0, 120)]), [(0, 180)]);
        sim.travel.trips[0].route = Route::new(&[Vec2::ZERO, Vec2::X], 480.0);
        sim.travel.trips[0].failure = Some("occupied_window");
        assert_eq!(sim.adjust_travel_padding(&options, &[(0, 120)]), [(0, 180)]);
        sim.travel.trips[0].route = Route::new(&[Vec2::ZERO, Vec2::X * 1000.0], 480.0);
        let capped = super::super::Options {
            travel_cap: super::super::TravelCap::Seconds(0.0),
            ..Default::default()
        };
        assert_eq!(sim.adjust_travel_padding(&capped, &[]), []);
    }

    #[test]
    fn a_new_commit_keeps_live_beams_in_the_same_repository() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        crate::repositories::namespace(&mut events, 0);
        let mut sim = Simulation::with_repositories(
            events,
            &super::super::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &["A".into(), "B".into()],
        );
        let frame = frames_for(sim.schedule.starts[1], 60).unwrap();
        sim.advance_to(frame, 60);
        assert_eq!(sim.commit(), 2);
        assert!(
            sim.state
                .pulses
                .iter()
                .any(|p| p.commit == 0 && sim.pulse_beams_visible(p))
        );
        assert!(!sim.beams().is_empty());
    }

    #[test]
    fn cap_quantization_never_rounds_above_the_requested_wait() {
        for cap in [0.0, 0.001, 0.1, 0.4, 0.999, 3600.0] {
            assert!(ticks_duration(u64::from(seconds_ticks(cap, false))) <= cap + 1e-5);
        }
    }

    #[test]
    fn fade_cutoffs_are_exact_on_canonical_ticks() {
        assert_eq!(fade_opacity(0, 9).to_bits(), 0.0_f32.to_bits());
        assert_eq!(fade_opacity(1, 9).to_bits(), 0.0_f32.to_bits());
        assert!(fade_opacity(2, 9) > 0.0);
        assert_eq!(fade_opacity(10, 9).to_bits(), 1.0_f32.to_bits());
    }

    #[test]
    fn capped_births_fade_and_bots_keep_their_policy() {
        for (cap, bot) in [(0.0, false), (0.4, true)] {
            let mut events = Vec::new();
            for i in 0..4 {
                let mut history = vec![crate::timeline::synthetic().remove(0)];
                history[0].seconds = i64::try_from(i).unwrap() * 86_400;
                if bot {
                    history[0].author.name = b"Automation[bot]".to_vec();
                }
                crate::repositories::namespace(&mut history, i % 2);
                events.extend(history);
            }
            let options = super::super::Options {
                travel_cap: super::super::TravelCap::Seconds(cap),
                seconds_per_day: 0.3,
                ..Default::default()
            };
            let nominal = super::super::Schedule::for_replay(&events, &options);
            let sim = Simulation::with_repositories(
                events,
                &options,
                &crate::theme::Theme::neon(),
                &crate::contributors::Mappings::default(),
                &["A".into(), "B".into()],
            );
            if bot {
                assert_eq!(sim.schedule.starts, nominal.starts);
            } else {
                assert_eq!(sim.schedule.starts, nominal.starts);
                assert_eq!(sim.travel.stats.birth_fades, 1);
            }
        }
    }

    #[test]
    fn padded_multi_repository_playback_replays_at_thirty_and_sixty_fps() {
        let mut events = Vec::new();
        for i in 0..4 {
            let mut history = vec![crate::timeline::synthetic().remove(0)];
            history[0].seconds = i64::try_from(i).unwrap() * 86_400;
            crate::repositories::namespace(&mut history, i % 2);
            events.extend(history);
        }
        let options = super::super::Options {
            seconds_per_day: 0.3,
            ..Default::default()
        };
        let make = || {
            Simulation::with_repositories(
                events.clone(),
                &options,
                &crate::theme::Theme::neon(),
                &crate::contributors::Mappings::default(),
                &["A".into(), "B".into()],
            )
        };
        let mut a = make();
        let mut b = make();
        assert_eq!(a.travel.report(), b.travel.report());
        assert!(a.schedule.starts.windows(2).all(|s| s[0] < s[1]));
        for event in 0..a.events.len() {
            let birth = a.clock.history.padding(event);
            assert!(a.schedule.padding(event) - birth <= options.travel_cap.ticks());
        }
        for frame in [0, 15, 30, 45, 60, 90, 120, 20, 80, 0, 110] {
            a.advance_to(frame, 30);
            b.advance_to(frame * 2, 60);
            assert_eq!(
                format!("{:?}", a.state.avatars),
                format!("{:?}", b.state.avatars)
            );
            assert_eq!(a.caption(), b.caption());
            for event in 1..a.events.len() {
                if a.seconds() >= a.schedule.starts[event - 1]
                    && a.seconds() < a.schedule.starts[event]
                {
                    assert!(
                        a.caption()
                            .contains(&super::super::date(a.events[event - 1].seconds))
                    );
                }
            }
        }
    }

    #[test]
    fn acceleration_profiles_stop_at_corners_and_respect_speed() {
        for length in [0.01, 20.0, 600.0] {
            let route = Route::new(
                &[Vec2::ZERO, Vec2::X * length, Vec2::new(length, length)],
                240.0,
            );
            let mut prior = route.sample(0.0).unwrap();
            let mut prior_velocity = Vec2::ZERO;
            for frame in 1..=super::frames_for(
                crate::model::Time::seconds(route.duration + 0.1),
                60,
            )
            .unwrap()
            {
                let point = route.sample(ticks_duration(u64::from(frame))).unwrap();
                assert!(point.distance(prior) * 60.0 <= 240.01);
                let velocity = (point - prior) * 60.0;
                let acceleration = (velocity - prior_velocity).length() * 60.0;
                assert!(
                    acceleration <= 1200.5,
                    "length={length} frame={frame} acceleration={acceleration}"
                );
                prior_velocity = velocity;
                prior = point;
            }
            assert!(prior.distance(Vec2::splat(length)) < 0.001);
        }
    }
    #[test]
    fn capped_fade_never_walks_a_partial_route() {
        let trip = Trip {
            author: 0,
            commit: 0,
            departure: 10,
            arrival: 34,
            destination: Vec2::new(500.0, 100.0),
            source: Vec2::ZERO,
            fade_ticks: 9,
            route: Route::new(&[Vec2::ZERO, Vec2::new(500.0, 100.0)], 480.0),
            kind: TripKind::Transfer,
            fade: true,
            bot: false,
            source_repository: 0,
            destination_repository: 1,
            free: 10,
            failure: None,
            cutoff: Some(30),
            proof: None,
        };
        for tick in trip.departure..trip.arrival {
            assert_eq!(trip.sample(tick).0, trip.source);
        }
        assert!(trip.sample(trip.departure).1 > trip.sample(trip.departure + 4).1);
        assert_eq!(trip.sample(trip.departure + 9).1, 0.0);
    }

    #[test]
    fn label_forecast_sees_the_trip_after_an_imminent_arrival() {
        let first = Trip {
            author: 0,
            commit: 0,
            departure: 0,
            arrival: 30,
            source: Vec2::new(-100.0, 0.0),
            destination: Vec2::ZERO,
            route: Route::new(&[Vec2::new(-100.0, 0.0), Vec2::ZERO], 480.0),
            kind: TripKind::Transfer,
            fade: false,
            bot: false,
            fade_ticks: 9,
            source_repository: 0,
            destination_repository: 1,
            free: 0,
            failure: None,
            cutoff: None,
            proof: None,
        };
        let second = Trip {
            commit: 1,
            departure: 30,
            arrival: 90,
            source: Vec2::ZERO,
            destination: Vec2::new(0.0, 200.0),
            route: Route::new(&[Vec2::ZERO, Vec2::new(0.0, 200.0)], 480.0),
            ..first.clone()
        };
        let plan = Plan {
            trips: vec![first, second],
            ..Default::default()
        };
        assert!(plan.forecast(0, 29).unwrap()[0].y > 0.0);
    }
}
