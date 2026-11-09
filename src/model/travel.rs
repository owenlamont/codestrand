use std::{cmp::Reverse, collections::BinaryHeap};

use bevy::math::{Rect, Vec2};
use serde::Serialize;

use super::{Simulation, frames_for, slots, tick_time};
mod planning;
use planning::trips;

const HALF: Vec2 = Vec2::new(640.0, 360.0);
const MAX_POSES: usize = 4_000_000;

#[derive(Clone, Copy, Debug)]
struct Pose {
    tick: u64,
    point: Vec2,
    visible: bool,
}

struct Service {
    author: usize,
    repository: usize,
    tick: u64,
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
    pub stats: Statistics,
}

impl Plan {
    #[cfg(test)]
    pub(super) fn assert_human_motion(&self) {
        let total = self.stats.human_complete
            + self.stats.human_partial
            + self.stats.human_no_route;
        assert!(
            self.stats.human_complete * 2 > total,
            "most human switches must complete"
        );
        assert_eq!(self.stats.human_no_route, 0);
        assert!(
            self.trips
                .iter()
                .filter(|t| !t.bot
                    && t.fade
                    && t.source_repository != t.destination_repository)
                .all(|t| planning::progress(t) >= 1.0),
            "a partial trip must visibly move"
        );
    }
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"statistics":self.stats,"trips":self.trips.iter().map(|t| serde_json::json!({"author":t.author,"commit":t.commit,"bot":t.bot,"source_repository":t.source_repository,"destination_repository":t.destination_repository,"free":tick_time(t.free),"slack":tick_time(t.arrival-t.free),"departure":tick_time(t.departure),"service":tick_time(t.arrival),"fade":t.fade,"failure":t.failure,"proof":t.proof,"source":t.source.to_array(),"target":t.destination.to_array(),"visible_distance":planning::progress(t),"travel_seconds":t.route.duration,"points":t.route.points()})).collect::<Vec<_>>()})
    }
    pub fn sample(&self, author: usize, tick: u64) -> Option<(Vec2, f32)> {
        let trip = self
            .trips
            .iter()
            .find(|t| t.author == author && tick >= t.departure && tick < t.arrival)?;
        Some(trip.sample(tick))
    }

    pub fn travelling(&self, author: usize, tick: u64) -> bool {
        self.trips
            .iter()
            .any(|t| t.author == author && tick >= t.departure && tick < t.arrival)
    }
}

impl Trip {
    fn sample(&self, tick: u64) -> (Vec2, f32) {
        let trip = self;
        if let Some(cutoff) = trip.cutoff
            && tick >= cutoff
        {
            return (
                trip.route
                    .point_at(cutoff.saturating_sub(trip.departure))
                    .unwrap_or(trip.source),
                0.0,
            );
        }
        let available = trip.arrival - trip.departure;
        if !trip.fade {
            return (
                trip.route
                    .point_at(tick - trip.departure)
                    .unwrap_or(trip.destination),
                1.0,
            );
        }
        if trip.route.is_empty() {
            let remaining = trip.fade_ticks.saturating_sub(tick - trip.departure);
            return (trip.source, fade_opacity(remaining, trip.fade_ticks));
        }
        (
            trip.route
                .point_at(tick - trip.departure)
                .expect("nonempty route"),
            fade_opacity(
                trip.cutoff.unwrap_or(trip.arrival) - tick,
                9.min(available / 2).max(1),
            ),
        )
    }
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
    };
    let mut seen = 0;
    let mut poses = 0;
    for frame in 0..=frames {
        simulation.advance_to(frame, 60);
        let state = &simulation.state;
        capture.radii.push(6.5 * state.avatar_scale + 1.0);
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
                    visible: avatar.presence > 0.0,
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
                point: avatar
                    .slot
                    .as_ref()
                    .expect("tile exists before service")
                    .point,
                radius: 6.5 * state.avatar_scale + 1.0,
                bot: contributor.is_bot(&simulation.events[event].author.email),
                dock: avatar.slot.as_ref().expect("service slot").dock,
            });
        }
        seen = state.next_event;
    }
    simulation.restart();
    eprintln!(
        "travel_capture_seconds={:.6} frames={} poses={poses}",
        started.elapsed().as_secs_f64(),
        frames
    );
    capture
}

fn fade_clearance(
    capture: &Capture,
    begin: u64,
    point: Vec2,
    radius: f32,
    author: usize,
) -> u64 {
    for offset in 0..=9_u64 {
        let tick = begin + offset;
        let row = &capture.tiles[usize::try_from(tick)
            .expect("bounded fade tick")
            .min(capture.tiles.len() - 1)];
        let tile_collision = row.iter().flatten().any(|tile| {
            point.distance(point.clamp(tile.low * HALF, tile.high * HALF))
                <= radius + 0.01
        });
        let avatar_collision = capture
            .poses
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != author)
            .any(|(_, poses)| {
                let i = poses.partition_point(|p| p.tick < tick);
                poses.get(i).is_some_and(|p| {
                    p.tick == tick
                        && p.visible
                        && p.point.distance(point) < 2.0 * radius + 0.01
                })
            });
        if tile_collision || avatar_collision {
            return offset;
        }
    }
    9
}

impl Simulation {
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
                    .partition_point(|&s| s <= tick_time(trip.departure))
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
                        && t.route.is_empty()
                })
                .count(),
            bot_fades: trips.iter().filter(|t| t.bot && t.fade).count(),
            max_speed: options.avatar_max_speed,
            temporary_capture_bytes: capture
                .poses
                .iter()
                .map(|p| p.capacity() * std::mem::size_of::<Pose>())
                .sum::<usize>()
                + capture
                    .tiles
                    .iter()
                    .map(|t| {
                        t.capacity()
                            * std::mem::size_of::<Option<crate::layout::packing::Tile>>(
                            )
                    })
                    .sum::<usize>()
                + capture.tiles.capacity()
                    * std::mem::size_of::<Vec<Option<crate::layout::packing::Tile>>>()
                + capture.services.capacity() * std::mem::size_of::<Service>()
                + capture.radii.capacity() * std::mem::size_of::<f32>()
                + capture.poses.capacity() * std::mem::size_of::<Vec<Pose>>(),
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
        self.travel = Plan { trips, stats };
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
    tiles: Vec<Rect>,
    epochs: usize,
}

#[derive(Clone, Copy)]
struct Disc {
    point: Vec2,
    radius: f32,
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

impl Router {
    fn route(
        &mut self,
        source: Vec2,
        target: Vec2,
        obstacles: &[Rect],
        bodies: &[Disc],
    ) -> Result<Vec<Vec2>, RouteFailure> {
        let changed = self.tiles.len() != obstacles.len()
            || self.tiles.iter().zip(obstacles).any(|(a, b)| {
                (a.min - b.min)
                    .abs()
                    .max_element()
                    .max((a.max - b.max).abs().max_element())
                    >= 1.0
            });
        if changed || self.epochs == 0 {
            self.tiles = obstacles.to_vec();
            self.epochs += 1;
        }
        let mut axes = [vec![-634.2, 634.2], vec![-354.2, 354.2]];
        add_axes(&mut axes, obstacles);
        add_axes(
            &mut axes,
            &bodies
                .iter()
                .map(|b| Rect::from_center_size(b.point, Vec2::splat(2.0 * b.radius)))
                .collect::<Vec<_>>(),
        );
        axes[0].extend([source.x, target.x]);
        axes[1].extend([source.y, target.y]);
        graph_route(source, target, obstacles, bodies, axes)
    }
}

fn add_axes(axes: &mut [Vec<f32>; 2], obstacles: &[Rect]) {
    let limit = HALF - Vec2::splat(5.8);
    for r in obstacles {
        for (axis, values) in axes.iter_mut().enumerate() {
            values.extend(
                [r.min[axis] - 0.01, r.max[axis] + 0.01]
                    .into_iter()
                    .filter(|v| v.abs() <= limit[axis]),
            );
        }
    }
}

fn graph_route(
    source: Vec2,
    target: Vec2,
    obstacles: &[Rect],
    bodies: &[Disc],
    mut axes: [Vec<f32>; 2],
) -> Result<Vec<Vec2>, RouteFailure> {
    let limit = HALF - Vec2::splat(5.8);
    if [source, target].iter().any(|p| {
        p.abs().cmpgt(limit).any()
            || obstacles.iter().any(|r| r.contains(*p))
            || bodies.iter().any(|b| b.point.distance(*p) < b.radius)
    }) {
        return Err(RouteFailure::Endpoint);
    }
    let clear = |a: Vec2, b: Vec2| {
        !obstacles.iter().any(|&r| slots::segment(a, b, r))
            && !bodies.iter().any(|body| {
                let delta = b - a;
                let t = ((body.point - a).dot(delta)
                    / delta.length_squared().max(1e-8))
                .clamp(0.0, 1.0);
                body.point.distance(a + delta * t) < body.radius
            })
    };
    if clear(source, target) {
        return Ok(vec![source, target]);
    }
    for values in &mut axes {
        values.sort_by(f32::total_cmp);
        values.dedup();
    }
    let width = axes[0].len();
    let index = |p: Vec2| {
        axes[1]
            .binary_search_by(|v| v.partial_cmp(&p.y).expect("finite coordinate"))
            .expect("inserted y")
            * width
            + axes[0]
                .binary_search_by(|v| v.partial_cmp(&p.x).expect("finite coordinate"))
                .expect("inserted x")
    };
    let point = |i: usize| Vec2::new(axes[0][i % width], axes[1][i / width]);
    let nodes = width * axes[1].len();
    if nodes > 10_000 {
        return Err(RouteFailure::Budget);
    }
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
            if !clear(point(i), point(next)) {
                continue;
            }
            let delta = point(i).distance(point(next));
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "finite viewport distances quantized to 1/1024 reference pixel"
            )]
            let candidate = cost + (delta * 1024.0).ceil() as u64;
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
    let mut simplified = vec![source];
    let mut i = 0;
    while i + 1 < path.len() {
        let next = (i + 1..path.len())
            .rev()
            .find(|&j| clear(path[i], path[j]))
            .expect("connected path");
        simplified.push(path[next]);
        i = next;
    }
    Ok(simplified)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn routes_are_deterministic_and_clear_the_complete_avatar_disc() {
        let obstacles =
            [
                Rect::from_corners(Vec2::new(-100.0, -200.0), Vec2::new(100.0, 200.0))
                    .inflate(8.0),
            ];
        let a = Vec2::new(-300.0, 0.0);
        let b = -a;
        let mut router = Router::default();
        let route = router.route(a, b, &obstacles, &[]).unwrap();
        assert_eq!(route, router.route(a, b, &obstacles, &[]).unwrap());
        assert_eq!(router.epochs, 1);
        assert!(route.len() > 2);
        assert!(
            route
                .windows(2)
                .all(|p| !slots::segment(p[0], p[1], obstacles[0]))
        );
        assert!(router.route(Vec2::ZERO, b, &obstacles, &[]).is_err());
    }

    #[test]
    fn exact_disc_contacts_are_allowed_and_geometric_failures_are_distinct() {
        let mut router = Router::default();
        let body = Disc {
            point: Vec2::ZERO,
            radius: 15.0,
        };
        assert!(
            router
                .route(Vec2::new(-15.0, 0.0), Vec2::new(-50.0, 0.0), &[], &[body])
                .is_ok()
        );
        let wall = Rect::from_corners(Vec2::new(-10.0, -360.0), Vec2::new(10.0, 360.0));
        assert!(matches!(
            router.route(Vec2::new(-100.0, 0.0), Vec2::new(100.0, 0.0), &[wall], &[]),
            Err(RouteFailure::Disconnected)
        ));
        assert!(matches!(
            router.route(Vec2::ZERO, Vec2::new(100.0, 0.0), &[wall], &[]),
            Err(RouteFailure::Endpoint)
        ));
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
}
