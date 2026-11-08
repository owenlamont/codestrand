use std::{cmp::Reverse, collections::BinaryHeap};

use bevy::math::{Rect, Vec2};
use serde::Serialize;

use super::{Simulation, frames_for, slots, tick_time};

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
        let acceleration = speed * 2.0;
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
}

impl Route {
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
}

#[derive(Default, Serialize)]
pub(super) struct Statistics {
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
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"statistics":self.stats,"trips":self.trips.iter().map(|t| serde_json::json!({"author":t.author,"commit":t.commit,"source_repository":t.source_repository,"destination_repository":t.destination_repository,"departure":tick_time(t.departure),"service":tick_time(t.arrival),"fade":t.fade,"travel_seconds":t.route.duration,"points":t.route.legs.iter().map(|l| l.from.to_array()).chain(t.route.legs.last().map(|l| l.to.to_array())).collect::<Vec<_>>()})).collect::<Vec<_>>()})
    }
    pub fn sample(&self, author: usize, tick: u64) -> Option<(Vec2, f32)> {
        let trip = self
            .trips
            .iter()
            .find(|t| t.author == author && tick >= t.departure && tick < t.arrival)?;
        let elapsed = tick_time(tick - trip.departure);
        let available = trip.arrival - trip.departure;
        if !trip.fade {
            return Some((trip.route.sample(elapsed).unwrap_or(trip.destination), 1.0));
        }
        if trip.route.legs.is_empty() {
            let remaining = trip.fade_ticks.saturating_sub(tick - trip.departure);
            return Some((trip.source, fade_opacity(remaining, trip.fade_ticks)));
        }
        Some((
            trip.route.sample(elapsed).expect("nonempty route"),
            fade_opacity(trip.arrival - tick, 9.min(available / 2).max(1)),
        ))
    }

    pub fn travelling(&self, author: usize, tick: u64) -> bool {
        self.trips
            .iter()
            .any(|t| t.author == author && tick >= t.departure && tick < t.arrival)
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
    };
    let mut seen = 0;
    let mut poses = 0;
    for frame in 0..=frames {
        simulation.advance_to(frame, 60);
        let state = &simulation.state;
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
                bot: contributor.name.to_ascii_lowercase().contains("[bot]"),
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

fn swept(
    capture: &Capture,
    begin: u64,
    end: u64,
    radius: f32,
    author: usize,
) -> (Vec<Rect>, usize) {
    let mut bounds: Vec<Option<Rect>> = vec![None; capture.tiles[0].len()];
    for row in &capture.tiles[usize::try_from(begin).expect("bounded tick")
        ..=usize::try_from(end)
            .expect("bounded tick")
            .min(capture.tiles.len() - 1)]
    {
        for (bound, tile) in bounds.iter_mut().zip(row) {
            if let Some(tile) = tile {
                let rect = Rect::from_corners(tile.low * HALF, tile.high * HALF);
                *bound = Some(bound.map_or(rect, |b| b.union(rect)));
            }
        }
    }
    let mut obstacles: Vec<_> = bounds
        .into_iter()
        .flatten()
        .map(|r| r.inflate(radius + 0.1))
        .collect();
    let tiles = obstacles.len();
    for (_, poses) in capture
        .poses
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != author)
    {
        let first = poses.partition_point(|p| p.tick < begin);
        let last = poses.partition_point(|p| p.tick <= end);
        let mut bound: Option<Rect> = None;
        for p in poses[first..last].iter().filter(|p| p.visible) {
            let rect = Rect::from_center_size(p.point, Vec2::splat(4.0 * radius + 0.2));
            bound = Some(bound.map_or(rect, |b| b.union(rect)));
        }
        obstacles.extend(bound);
    }
    (obstacles, tiles)
}

fn trips(capture: &Capture, speed: f32) -> (Vec<Trip>, usize) {
    let mut previous: Vec<Option<usize>> = vec![None; capture.poses.len()];
    let mut result = Vec::new();
    let mut reserved_until = 0_u64;
    let mut router = Router::default();
    for (commit, service) in capture.services.iter().enumerate() {
        let prior = previous[service.author].replace(commit);
        let Some(prior) = prior.map(|i| &capture.services[i]) else {
            continue;
        };
        if (prior.repository == service.repository && prior.dock == service.dock)
            || tick_time(service.tick - prior.tick) >= super::AVATAR_IDLE + 1.2
        {
            continue;
        }
        let arrival = service.tick;
        let mut departure = (prior.tick + 1).max(reserved_until).min(arrival - 1);
        let begin = departure;
        let poses = &capture.poses[service.author];
        let pose = poses.partition_point(|p| p.tick <= begin);
        let mut source = poses[pose.saturating_sub(1)].point;
        let (obstacles, tiles) = swept(
            capture,
            begin,
            service.tick,
            prior.radius.max(service.radius),
            service.author,
        );
        let points = if service.bot {
            None
        } else {
            router.route(source, service.point, &obstacles, tiles)
        };
        let route = points
            .as_ref()
            .map_or_else(Route::default, |p| Route::new(p, speed));
        let fade = service.bot
            || points.is_none()
            || route.duration > tick_time(arrival - departure);
        if points.is_none() {
            departure = arrival
                .saturating_sub(9)
                .max(prior.tick + 1)
                .min(arrival - 1);
            let pose = poses.partition_point(|p| p.tick <= departure);
            source = poses[pose.saturating_sub(1)].point;
        }
        let fade_ticks = fade_clearance(
            capture,
            departure,
            source,
            prior.radius.max(service.radius),
            service.author,
        )
        .min(arrival - departure);
        result.push(Trip {
            author: service.author,
            commit,
            departure,
            arrival,
            destination: service.point,
            source,
            fade_ticks,
            route: if service.bot { Route::default() } else { route },
            fade,
            bot: service.bot,
            source_repository: prior.repository,
            destination_repository: service.repository,
        });
        if !service.bot && points.is_some() {
            reserved_until = arrival;
        }
    }
    (result, router.epochs)
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
    pub(super) fn travel_padding(
        &mut self,
        options: &super::Options,
    ) -> Vec<(usize, u32)> {
        let capture = capture(self);
        let (routes, _) = trips(&capture, options.avatar_max_speed);
        let cap = seconds_ticks(options.travel_cap, false);
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
                let needed = if trip.route.legs.is_empty() {
                    0
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
        let (trips, graph_epochs) = trips(&capture, options.avatar_max_speed);
        let stats = Statistics {
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
                + capture.services.capacity() * std::mem::size_of::<Service>(),
            retained_route_bytes: trips.capacity() * std::mem::size_of::<Trip>()
                + trips
                    .iter()
                    .map(|t| t.route.legs.capacity() * std::mem::size_of::<Leg>())
                    .sum::<usize>(),
            graph_epochs,
            routed: trips.iter().filter(|t| !t.fade).count(),
            faded: trips.iter().filter(|t| t.fade).count(),
            blocked: trips
                .iter()
                .filter(|t| !t.bot && t.route.legs.is_empty())
                .count(),
            partial: trips
                .iter()
                .filter(|t| !t.bot && t.fade && !t.route.legs.is_empty())
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
    axes: [Vec<f32>; 2],
    epochs: usize,
}

impl Router {
    fn route(
        &mut self,
        source: Vec2,
        target: Vec2,
        obstacles: &[Rect],
        tiles: usize,
    ) -> Option<Vec<Vec2>> {
        let changed = self.tiles.len() != tiles
            || self.tiles.iter().zip(&obstacles[..tiles]).any(|(a, b)| {
                (a.min - b.min)
                    .abs()
                    .max_element()
                    .max((a.max - b.max).abs().max_element())
                    >= 1.0
            });
        if changed || self.epochs == 0 {
            self.tiles = obstacles[..tiles].to_vec();
            self.axes = [vec![-632.0, 632.0], vec![-352.0, 352.0]];
            add_axes(&mut self.axes, &self.tiles);
            self.epochs += 1;
        }
        let mut axes = self.axes.clone();
        add_axes(&mut axes, &obstacles[tiles..]);
        axes[0].extend([source.x, target.x]);
        axes[1].extend([source.y, target.y]);
        graph_route(source, target, obstacles, axes)
    }
}

fn add_axes(axes: &mut [Vec<f32>; 2], obstacles: &[Rect]) {
    let limit = HALF - Vec2::splat(8.0);
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
    mut axes: [Vec<f32>; 2],
) -> Option<Vec<Vec2>> {
    let limit = HALF - Vec2::splat(8.0);
    if [source, target]
        .iter()
        .any(|p| p.abs().cmpgt(limit).any() || obstacles.iter().any(|r| r.contains(*p)))
    {
        return None;
    }
    let clear = |a, b| !obstacles.iter().any(|&r| slots::segment(a, b, r));
    if clear(source, target) {
        return Some(vec![source, target]);
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
        return None;
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
        return None;
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
    Some(simplified)
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
                travel_cap: cap,
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
            assert!(
                a.schedule.padding(event) <= seconds_ticks(options.travel_cap, false)
            );
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
        let route = router.route(a, b, &obstacles, 1).unwrap();
        assert_eq!(Some(route.clone()), router.route(a, b, &obstacles, 1));
        assert_eq!(router.epochs, 1);
        assert!(route.len() > 2);
        assert!(
            route
                .windows(2)
                .all(|p| !slots::segment(p[0], p[1], obstacles[0]))
        );
        assert!(router.route(Vec2::ZERO, b, &obstacles, 1).is_none());
    }

    #[test]
    fn acceleration_profiles_stop_at_corners_and_respect_speed() {
        for length in [0.01, 20.0, 600.0] {
            let route = Route::new(
                &[Vec2::ZERO, Vec2::X * length, Vec2::new(length, length)],
                240.0,
            );
            let mut prior = route.sample(0.0).unwrap();
            for frame in 1..=super::frames_for(route.duration + 0.1, 60).unwrap() {
                let point = route.sample(tick_time(u64::from(frame))).unwrap();
                assert!(point.distance(prior) * 60.0 <= 240.01);
                prior = point;
            }
            assert!(prior.distance(Vec2::splat(length)) < 0.001);
        }
    }
}
