use std::{cmp::Reverse, collections::BinaryHeap};

use bevy::math::{Rect, Vec2};
use serde::Serialize;

use super::{Simulation, frames_for, slots, tick_time};
mod occupancy;
mod planning;
use planning::trips;

const HALF: Vec2 = Vec2::new(640.0, 360.0);
const MAX_POSES: usize = 4_000_000;
const NETWORK_EPSILON: f32 = 0.01;

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

#[derive(Clone, Copy, Debug)]
struct Pose {
    tick: u64,
    point: Vec2,
    dock: slots::Dock,
}

struct Service {
    author: usize,
    repository: usize,
    tick: u64,
    release: u64,
    point: Vec2,
    radius: f32,
    bot: bool,
    dock: slots::Dock,
}

struct Capture {
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
            + self
                .tiles
                .iter()
                .map(|t| {
                    t.capacity()
                        * std::mem::size_of::<Option<crate::layout::packing::Tile>>()
                })
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
    track: Vec<Vec2>,
}

impl Route {
    fn is_empty(&self) -> bool {
        self.legs.is_empty() && self.track.is_empty()
    }
    fn point_at(&self, tick: u64) -> Option<Vec2> {
        if self.track.is_empty() {
            self.sample(tick_time(tick))
        } else {
            Some(
                self.track[usize::try_from(tick)
                    .expect("bounded tick")
                    .min(self.track.len() - 1)],
            )
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
struct Trip {
    author: usize,
    commit: usize,
    departure: u64,
    arrival: u64,
    destination: Vec2,
    source: Vec2,
    fade_ticks: u64,
    route: Route,
    fade: bool,
    bot: bool,
    source_repository: usize,
    destination_repository: usize,
    free: u64,
    failure: Option<&'static str>,
    cutoff: Option<u64>,
    proof: Option<serde_json::Value>,
}

#[derive(Default, Serialize)]
pub(super) struct Statistics {
    pub human_complete: usize,
    pub human_partial: usize,
    pub human_no_route: usize,
    pub bot_fades: usize,
    pub max_speed: f32,
    pub idle_human_reentries: usize,
    pub routed: usize,
    pub faded: usize,
    pub blocked: usize,
    pub partial: usize,
    pub graph_epochs: usize,
    pub padding_seconds: f32,
    pub temporary_capture_bytes: usize,
    pub retained_route_bytes: usize,
}

#[derive(Default)]
pub(super) struct Plan {
    trips: Vec<Trip>,
    pub(super) docks: Vec<Option<slots::Dock>>,
    pub(super) replanning: bool,
    pub(super) occupancy: Vec<Vec<(u8, Rect)>>,
    pub stats: Statistics,
}

impl Plan {
    pub(super) fn preferences(self) -> Self {
        Self {
            docks: self.docks,
            occupancy: self.occupancy,
            ..Self::default()
        }
    }
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"statistics":self.stats,"trips":self.trips.iter().map(|t| serde_json::json!({"author":t.author,"commit":t.commit,"bot":t.bot,"source_repository":t.source_repository,"destination_repository":t.destination_repository,"free":tick_time(t.free),"slack":tick_time(t.arrival-t.free),"departure":tick_time(t.departure),"service":tick_time(t.arrival),"fade":t.fade,"failure":t.failure,"proof":t.proof,"source":t.source.to_array(),"target":t.destination.to_array(),"visible_distance":planning::progress(t),"travel_seconds":t.route.duration,"points":t.route.points()})).collect::<Vec<_>>()})
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
        self.trips.iter().any(|t| {
            t.author == author
                && t.departure <= tick
                && tick < t.arrival
                && !t.route.is_empty()
        })
    }
}

impl Trip {
    fn active(&self, author: usize, tick: u64) -> bool {
        self.author == author
            && self.departure <= tick
            && tick < self.arrival
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
    (tick_time(remaining.saturating_sub(1)) / tick_time(duration.max(1))).min(1.0)
}

fn capture(simulation: &mut Simulation) -> Capture {
    let started = std::time::Instant::now();
    let frames = frames_for(simulation.duration(), 60).expect("validated travel range");
    let mut capture = Capture {
        services: Vec::new(),
        poses: Vec::new(),
        tiles: Vec::new(),
        radii: Vec::new(),
        gutters: Vec::new(),
    };
    let visual_updates = simulation.visual_updates;
    let docks = if simulation.travel.occupancy.is_empty() {
        std::mem::take(&mut simulation.travel.docks)
    } else {
        simulation.travel.docks.clone()
    };
    simulation.travel.replanning = true;
    simulation.visual_updates = false;
    let mut seen = 0;
    let mut poses = 0;
    for frame in 0..=frames {
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
            if let Some(slot) = &avatar.slot {
                assert!(poses < MAX_POSES, "validated travel pose bound");
                capture.poses[index].push(Pose {
                    tick: state.tick,
                    point: slot.point,
                    dock: slot.dock,
                });
                poses += 1;
            }
        }
        for event in seen..state.next_event {
            let contributor = simulation.events[event]
                .contributor
                .as_ref()
                .expect("resolved contributor");
            let author = state.authors[&contributor.key];
            let avatar = &state.avatars[author];
            capture.services.push(Service {
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
                point: avatar
                    .slot
                    .as_ref()
                    .expect("tile exists before service")
                    .point,
                radius: state.avatar_radius(),
                bot: contributor.is_bot(&simulation.events[event].author.email),
                dock: avatar.slot.as_ref().expect("service slot").dock,
            });
        }
        seen = state.next_event;
    }
    simulation.restart();
    simulation.visual_updates = visual_updates;
    simulation.travel.docks = docks;
    simulation.travel.replanning = false;
    eprintln!(
        "travel_capture_seconds={:.6} frames={} poses={poses}",
        started.elapsed().as_secs_f64(),
        frames
    );
    capture
}

fn fade_clearance(capture: &Capture, begin: u64, point: Vec2, radius: f32) -> u64 {
    for offset in 0..=9_u64 {
        let tick = begin + offset;
        let row = &capture.tiles[usize::try_from(tick)
            .expect("bounded fade tick")
            .min(capture.tiles.len() - 1)];
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
    fn pad_births(&self, padding: &mut Vec<(usize, u32)>) {
        let mut born = vec![false; self.initial.layout.repositories.len()];
        for (_, file) in self.initial.layout.files() {
            born[self.initial.layout.dir(file.dir).repository] = true;
        }
        if let Some(first) = self.events.first() {
            born[first.repository] = true;
        }
        for (index, event) in self.events.iter().enumerate() {
            if std::mem::replace(&mut born[event.repository], true) {
                continue;
            }
            let commit = self.offset + index;
            let ticks = crate::layout::packing::BIRTH_TICKS;
            if let Some((_, wait)) = padding.iter_mut().find(|(i, _)| *i == commit) {
                *wait = (*wait).max(ticks);
            } else {
                padding.push((commit, ticks));
            }
        }
        padding.sort_unstable_by_key(|p| p.0);
    }
    pub(super) fn adjust_travel_padding(
        &self,
        options: &super::Options,
        padding: &[(usize, u32)],
    ) -> Vec<(usize, u32)> {
        let mut result = padding.to_vec();
        let cap = options.travel_cap.ticks();
        for trip in self.travel.trips.iter().filter(|t| {
            (!t.bot || options.bot_travel == super::BotTravel::Walk) && t.fade
        }) {
            let needed = if trip.route.is_empty() {
                options.travel_cap.blocked_ticks()
            } else {
                seconds_ticks(
                    (trip.route.duration - tick_time(trip.arrival - trip.departure))
                        .max(0.0),
                    true,
                )
            };
            let needed = if options.travel_cap == super::TravelCap::None {
                needed.max(60)
            } else {
                needed
            };
            let commit = self.offset + trip.commit;
            if let Some((_, ticks)) = result.iter_mut().find(|(i, _)| *i == commit) {
                *ticks = ticks.saturating_add(needed).min(cap);
            } else if needed > 0 {
                result.push((commit, needed.min(cap)));
            }
        }
        result.sort_unstable_by_key(|p| p.0);
        result
    }
    pub(super) fn compact_travel_padding(
        &self,
        options: &super::Options,
        padding: &[(usize, u32)],
    ) -> Vec<(usize, u32)> {
        let mut result = padding.to_vec();
        for trip in self.travel.trips.iter().filter(|t| !t.fade) {
            let ready =
                trip.departure + u64::from(seconds_ticks(trip.route.duration, true));
            let surplus = u32::try_from(trip.arrival.saturating_sub(ready))
                .expect("bounded slack");
            if let Some((_, ticks)) = result
                .iter_mut()
                .find(|(i, _)| *i == self.offset + trip.commit)
            {
                *ticks = ticks.saturating_sub(surplus);
            }
        }
        if options.travel_cap == super::TravelCap::None {
            self.pad_births(&mut result);
        }
        result
    }
    pub(super) fn travel_padding(
        &mut self,
        options: &super::Options,
    ) -> Vec<(usize, u32)> {
        let capture = capture(self);
        let (routes, _) = trips(
            &capture,
            options.avatar_max_speed,
            options.bot_travel,
            options.travel_cap == super::TravelCap::None,
        );
        let cap = options.travel_cap.ticks();
        let mut shifts = vec![0_u64; self.events.len()];
        let mut extra = 0_u64;
        let mut padding = Vec::new();
        let mut routes = routes.iter().peekable();
        for commit in 0..self.events.len() {
            if let Some(trip) = routes.peek().filter(|t| t.commit == commit) {
                let departure_event = self
                    .schedule
                    .starts
                    .partition_point(|&s| s <= tick_time(trip.free))
                    .saturating_sub(1);
                let needed = if trip.bot && options.bot_travel == super::BotTravel::Fade
                {
                    0
                } else if trip.route.is_empty() {
                    options.travel_cap.blocked_ticks()
                } else {
                    seconds_ticks(
                        (tick_time(trip.departure)
                            + tick_time(shifts[departure_event])
                            + trip.route.duration
                            - tick_time(trip.arrival)
                            - tick_time(extra))
                        .max(0.0),
                        true,
                    )
                    .min(cap)
                };
                padding.push((self.offset + trip.commit, needed));
                extra += u64::from(needed);
                routes.next();
            }
            shifts[commit] = extra;
        }
        if options.travel_cap == super::TravelCap::None {
            self.pad_births(&mut padding);
        }
        padding
    }

    pub(super) fn plan_travel(
        &mut self,
        options: &super::Options,
        padding: &[(usize, u32)],
    ) {
        let capture = capture(self);
        let (trips, graph_epochs) = trips(
            &capture,
            options.avatar_max_speed,
            options.bot_travel,
            options.travel_cap == super::TravelCap::None,
        );
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
        let stats = Statistics {
            idle_human_reentries: human_switches
                - trips
                    .iter()
                    .filter(|t| {
                        !t.bot && t.source_repository != t.destination_repository
                    })
                    .count(),
            human_complete: trips
                .iter()
                .filter(|t| {
                    !t.bot && t.source_repository != t.destination_repository && !t.fade
                })
                .count(),
            human_partial: trips
                .iter()
                .filter(|t| {
                    !t.bot
                        && t.source_repository != t.destination_repository
                        && t.fade
                        && !t.route.is_empty()
                })
                .count(),
            human_no_route: trips
                .iter()
                .filter(|t| {
                    !t.bot
                        && t.source_repository != t.destination_repository
                        && t.fade
                        && t.route.is_empty()
                })
                .count(),
            bot_fades: trips.iter().filter(|t| t.bot && t.fade).count(),
            max_speed: options.avatar_max_speed,
            temporary_capture_bytes: capture.bytes(),
            retained_route_bytes: trips.capacity() * std::mem::size_of::<Trip>()
                + trips
                    .iter()
                    .map(|t| {
                        t.route.legs.capacity() * std::mem::size_of::<Leg>()
                            + t.route.track.capacity() * std::mem::size_of::<Vec2>()
                    })
                    .sum::<usize>(),
            graph_epochs,
            routed: trips.iter().filter(|t| !t.fade).count(),
            faded: trips.iter().filter(|t| t.fade).count(),
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
                .map(|&(_, t)| tick_time(u64::from(t)))
                .sum(),
        };
        let docks = capture.services.iter().map(|s| Some(s.dock)).collect();
        self.travel = Plan {
            trips,
            docks,
            stats,
            replanning: false,
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

pub(super) fn on_network(point: Vec2, cells: &[Rect]) -> bool {
    cells.iter().any(|r| {
        point.cmpge(r.min - Vec2::splat(0.001)).all()
            && point.cmple(r.max + Vec2::splat(0.001)).all()
            && (point - r.min)
                .abs()
                .min((point - r.max).abs())
                .min_element()
                < 0.001
    })
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
            values.dedup();
        }
        let width = axes[0].len();
        let nodes = width * axes[1].len();
        if nodes > 10_000 {
            return Err(RouteFailure::Budget);
        }
        let index = |p: Vec2| {
            axes[1]
                .binary_search_by(|v| v.total_cmp(&p.y))
                .expect("inserted y")
                * width
                + axes[0]
                    .binary_search_by(|v| v.total_cmp(&p.x))
                    .expect("inserted x")
        };
        let point = |i: usize| Vec2::new(axes[0][i % width], axes[1][i / width]);
        let start = index(source);
        let end = index(target);
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
            let x = i % width;
            let y = i / width;
            let neighbors = [
                (x > 0).then(|| i - 1),
                (x + 1 < width).then_some(i + 1),
                (y > 0).then(|| i - width),
                (y + 1 < axes[1].len()).then_some(i + width),
            ];
            for next in neighbors.into_iter().flatten() {
                let a = point(i);
                let b = point(next);
                if !on_network(a.midpoint(b), cells) {
                    continue;
                }
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
        let mut path = vec![target];
        let mut node = end;
        while node != start {
            node = previous[node];
            path.push(point(node));
        }
        path.reverse();
        Ok(simplify_path(&path))
    }
}

#[cfg(test)]
pub(super) fn assert_edge_connectivity(simulation: &Simulation) {
    let cells: Vec<_> = simulation
        .state
        .layout
        .repositories
        .iter()
        .filter_map(|r| {
            r.gutter
                .map(|t| Rect::from_corners(t.low * HALF, t.high * HALF))
        })
        .collect();
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
                .expect("connected guillotine gutters");
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
            assert!(tick_time(u64::from(seconds_ticks(cap, false))) <= cap + 1e-5);
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
    fn bots_and_zero_cap_preserve_nominal_service_times() {
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
            let nominal = super::super::Schedule::new(
                &events,
                options.seconds_per_day,
                options.max_idle_seconds,
            );
            let sim = Simulation::with_repositories(
                events,
                &options,
                &crate::theme::Theme::neon(),
                &crate::contributors::Mappings::default(),
                &["A".into(), "B".into()],
            );
            assert_eq!(sim.schedule.starts, nominal.starts);
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
            assert!(a.schedule.padding(event) <= options.travel_cap.ticks());
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
            for frame in 1..=super::frames_for(route.duration + 0.1, 60).unwrap() {
                let point = route.sample(tick_time(u64::from(frame))).unwrap();
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
