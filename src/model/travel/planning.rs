use super::{
    Capture, HALF, Pose, Rect, Route, Router, Trip, TripKind, Vec2, fade_clearance,
    ticks_duration,
};
use crate::layout::packing::Tile;

impl Capture {
    pub(super) fn pose(&self, author: usize, tick: u64) -> Option<Pose> {
        let poses = self.poses.get(author)?;
        poses
            .partition_point(|p| p.tick <= tick)
            .checked_sub(1)
            .map(|i| poses[i])
    }
    fn row(&self, tick: u64) -> &[Option<Tile>] {
        super::at_tick(&self.gutters, tick.saturating_sub(self.begin))
    }
    fn radius(&self, tick: u64) -> f32 {
        *super::at_tick(&self.radii, tick.saturating_sub(self.begin))
    }
}

fn hard_clearance(capture: &Capture, tick: u64, point: Vec2) -> bool {
    let radius = Vec2::splat(capture.radius(tick));
    point.cmpge(capture.field.min + radius).all()
        && point.cmple(capture.field.max - radius).all()
        && !super::at_tick(&capture.tiles, tick.saturating_sub(capture.begin))
            .iter()
            .flatten()
            .any(|t| super::in_guard(*t, capture.radius(tick), point))
}

fn route_clearance(capture: &Capture, trip: &Trip, tick: u64, point: Vec2) -> bool {
    capture.obstacles.as_ref().is_none_or(|obstacles| {
        !obstacles.affects(trip) || obstacles.clears(trip, tick, point)
    })
}

fn point_on_tile(tile: Tile, fraction: Vec2) -> Vec2 {
    (tile.low + (tile.high - tile.low) * fraction) * HALF
}

pub(super) fn progress(trip: &Trip) -> f32 {
    (trip.departure..trip.arrival)
        .filter_map(|t| {
            let (p, a) = trip.sample(t);
            (a > 0.0).then_some(p.distance(trip.source))
        })
        .fold(0.0, f32::max)
}

#[derive(Clone, Copy)]
struct Coordinate {
    repository: usize,
    fraction: f32,
}
impl Coordinate {
    fn at(self, row: &[Option<Tile>], axis: usize) -> Option<f32> {
        let tile = row.get(self.repository).copied().flatten()?;
        Some(
            (tile.low[axis] + (tile.high[axis] - tile.low[axis]) * self.fraction)
                * HALF[axis],
        )
    }
}
fn anchor(value: f32, row: &[Option<Tile>], axis: usize) -> Coordinate {
    let (repository, fraction) = row
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let t = t.as_ref()?;
            let low = t.low[axis] * HALF[axis];
            let high = t.high[axis] * HALF[axis];
            let f = ((value - low) / (high - low).max(1e-8)).clamp(0.0, 1.0);
            let error = (low + (high - low) * f - value).abs();
            Some((i, f, error, f.min(1.0 - f)))
        })
        .min_by(|a, b| {
            a.2.total_cmp(&b.2)
                .then(a.3.total_cmp(&b.3))
                .then(a.0.cmp(&b.0))
        })
        .map(|(i, f, _, _)| (i, f))
        .expect("network coordinate");
    Coordinate {
        repository,
        fraction,
    }
}

fn route_anchors(
    reference: &[Option<Tile>],
    trip: &Trip,
    points: &[Vec2],
) -> Option<Vec<[Coordinate; 2]>> {
    let mut anchors: Vec<_> = points
        .iter()
        .map(|p| [anchor(p.x, reference, 0), anchor(p.y, reference, 1)])
        .collect();
    for (index, repository) in [
        (0, trip.source_repository),
        (points.len() - 1, trip.destination_repository),
    ] {
        let tile = reference[repository]?;
        let fraction = (points[index] / HALF - tile.low) / (tile.high - tile.low);
        anchors[index] = [
            Coordinate {
                repository,
                fraction: fraction.x,
            },
            Coordinate {
                repository,
                fraction: fraction.y,
            },
        ];
    }
    for (leg, pair) in points.windows(2).enumerate() {
        let axis = usize::from(pair[0].y.to_bits() == pair[1].y.to_bits());
        let midpoint = pair[0].midpoint(pair[1]);
        let on_edge = |repository: usize| {
            let tile = reference[repository].as_ref()?;
            let rect = Rect::from_corners(tile.low * HALF, tile.high * HALF);
            if !rect.inflate(0.001).contains(midpoint) {
                return None;
            }
            let fraction = if (rect.min[axis] - midpoint[axis]).abs() < 0.001 {
                0.0
            } else if (rect.max[axis] - midpoint[axis]).abs() < 0.001 {
                1.0
            } else {
                return None;
            };
            Some(Coordinate {
                repository,
                fraction,
            })
        };
        let preferred = if leg == 0 {
            on_edge(trip.source_repository)
        } else if leg + 2 == points.len() {
            on_edge(trip.destination_repository)
        } else {
            None
        };
        let coordinate = preferred.or_else(|| (0..reference.len()).find_map(on_edge));
        let coordinate = coordinate?;
        anchors[leg][axis] = coordinate;
        anchors[leg + 1][axis] = coordinate;
    }
    Some(anchors)
}

fn moving_route(
    capture: &Capture,
    trip: &Trip,
    points: &[Vec2],
    epoch: u64,
    speed: f32,
    animation_delay: u64,
    align: bool,
) -> Option<Route> {
    let shape = &trip.route;
    let duration = shape.duration + ticks_duration(animation_delay);
    let available = ticks_duration(trip.arrival - trip.departure);
    if align && duration > available {
        return None;
    }
    let anchors = route_anchors(capture.row(epoch), trip, points)?;
    let target = capture.row(epoch)[trip.destination_repository]?;
    let target_fraction =
        (*points.last()? / HALF - target.low) / (target.high - target.low);
    let mut track = Vec::new();
    for offset in 0..=trip.arrival - trip.departure {
        let tick = trip.departure + offset;
        let row = capture.row(tick);
        let elapsed = ticks_duration(offset.saturating_sub(animation_delay));
        let elapsed = if align {
            elapsed * (shape.duration / available)
        } else {
            elapsed
        };
        if elapsed >= shape.duration && tick < trip.arrival {
            let target = row[trip.destination_repository]?;
            let point = point_on_tile(target, target_fraction);
            if !hard_clearance(capture, tick, point) {
                return None;
            }
            track.push(point);
            continue;
        }
        let leg = shape
            .legs
            .iter()
            .position(|l| elapsed < l.start + l.duration)
            .unwrap_or(shape.legs.len() - 1);
        let base = shape.legs[leg].sample(elapsed);
        let delta = points[leg + 1] - points[leg];
        let u = ((base - points[leg]).dot(delta) / delta.length_squared().max(1e-8))
            .clamp(0.0, 1.0);
        let b = Vec2::new(
            anchors[leg + 1][0].at(row, 0)?,
            anchors[leg + 1][1].at(row, 1)?,
        );
        let point = if tick == trip.arrival {
            trip.destination
        } else if u >= 1.0 {
            b
        } else {
            Vec2::new(anchors[leg][0].at(row, 0)?, anchors[leg][1].at(row, 1)?)
                .lerp(b, u)
        };
        let cells = super::network(row.iter().flatten().copied());
        if !super::on_network(point, &cells)
            || !hard_clearance(capture, tick, point)
            || (trip.departure < tick
                && tick < trip.arrival
                && !route_clearance(capture, trip, tick, point))
        {
            return None;
        }
        track.push(point);
    }
    if align && pauses_away_from_source(&track, trip.source) {
        return None;
    }
    let peak = track
        .windows(2)
        .map(|p| p[0].distance(p[1]) * 60.0)
        .fold(0.0, f32::max);
    let previous = capture
        .pose(trip.author, trip.departure.saturating_sub(1))?
        .point;
    if peak > speed + 0.05 || track.first()?.distance(previous) * 60.0 > speed + 0.05 {
        return None;
    }
    Some(Route {
        legs: Vec::new(),
        track: track.into(),
        duration: if align { available } else { duration },
    })
}

fn pauses_away_from_source(track: &[Vec2], source: Vec2) -> bool {
    let minimum_step = 0.0001 / super::super::framing::WORLD_UNITS_PER_REFERENCE_PIXEL;
    track
        .windows(2)
        .take(track.len().saturating_sub(2))
        .any(|pair| {
            pair[0].distance_squared(pair[1]) < minimum_step * minimum_step
                && pair[1].distance_squared(source) > super::NETWORK_EPSILON.powi(2)
        })
}

fn route_proof(points: &[Vec2], epoch: u64) -> serde_json::Value {
    let length: f32 = points.windows(2).map(|p| p[0].distance(p[1])).sum();
    let length = length * super::super::framing::WORLD_UNITS_PER_REFERENCE_PIXEL;
    serde_json::json!({"network_epoch":epoch,"shortest_network_length":length,"route_network_length":length,"network_points":points.iter().copied().map(super::ground_position).collect::<Vec<_>>()})
}

#[derive(Clone, Copy)]
enum Search {
    Fit { mandatory_birth_walk: bool },
    Align { blocked: Option<Vec2>, pace: f32 },
}

fn retain_shared_berth(
    capture: &Capture,
    trip: &Trip,
    begin: u64,
    source: Vec2,
) -> Option<Trip> {
    let source_tile = capture.row(trip.arrival)[trip.source_repository]?;
    let dock = capture
        .pose(trip.author, begin)
        .expect("existing author")
        .dock;
    (dock.on_network(source_tile).distance(trip.destination) < super::NETWORK_EPSILON)
        .then(|| Trip {
            departure: begin,
            source,
            fade: false,
            route: Route::default(),
            failure: None,
            cutoff: None,
            proof: None,
            ..trip.clone()
        })
}

fn search(
    capture: &Capture,
    template: &Trip,
    release: u64,
    speed: f32,
    router: &mut Router,
    mode: Search,
) -> Trip {
    let (align, blocked, pace) = match mode {
        Search::Align { blocked, pace } => (true, blocked, pace),
        Search::Fit { .. } => (false, None, 0.8),
    };
    let mut best = template.clone();
    let first = release.min(template.arrival - 1).max(template.free);
    let Some(target_tile) =
        capture.row(template.arrival)[template.destination_repository]
    else {
        return best;
    };
    let target_fraction = (template.destination / HALF - target_tile.low)
        / (target_tile.high - target_tile.low);
    for offset in 0..template.arrival - first {
        let begin = if align {
            template.arrival - 1 - offset
        } else {
            first + offset
        };
        let row = capture.row(begin);
        let Some(target_tile) = row[template.destination_repository] else {
            continue;
        };
        let source = capture
            .pose(template.author, begin)
            .expect("existing author")
            .point;
        if source.distance(template.destination)
            > speed * ticks_duration(template.arrival - begin)
        {
            continue;
        }
        let target = point_on_tile(target_tile, target_fraction);
        let cells = super::network(row.iter().flatten().copied());
        let points = match router.route_avoiding(source, target, &cells, blocked) {
            Ok(points) => points,
            Err(e) => {
                best.failure = Some(e.name());
                continue;
            }
        };
        if points.len() < 2 {
            if (!align || template.kind == TripKind::Relocation)
                && let Some(parked) =
                    retain_shared_berth(capture, template, begin, source)
            {
                return parked;
            }
            continue;
        }
        let mut candidate = template.clone();
        candidate.departure = begin;
        candidate.source = source;
        let animation_delay = release.saturating_sub(begin);
        candidate.route = Route::new(&points, speed * pace);
        if let Some(route) = moving_route(
            capture,
            &candidate,
            &points,
            begin,
            speed,
            animation_delay,
            align,
        ) {
            candidate.route = route;
            candidate.proof = Some(route_proof(&points, begin));
            candidate.fade = false;
            candidate.failure = None;
            return candidate;
        }
        if !align && best.route.is_empty() {
            best = candidate;
            best.route.duration += ticks_duration(animation_delay);
            best.proof = Some(route_proof(&points, begin));
            best.fade = true;
            best.failure = Some("moving_network");
            best.cutoff = Some(begin);
        }
    }
    let Search::Fit {
        mandatory_birth_walk,
    } = mode
    else {
        return template.clone();
    };
    if best.fade
        && mandatory_birth_walk
        && let Some(approach) =
            fixed_dock_approach(capture, template, first, speed, router, None)
    {
        return approach;
    }
    if mandatory_birth_walk
        && template.arrival - first >= crate::model::BIRTH_TICKS * 2
        && dock_outpaces_walker(capture, template, speed, target_fraction)
    {
        best.failure = Some(super::MOVING_DOCK);
    }
    best
}

fn dock_outpaces_walker(
    capture: &Capture,
    trip: &Trip,
    speed: f32,
    fraction: Vec2,
) -> bool {
    let first = trip.arrival.saturating_sub(crate::model::BIRTH_TICKS + 1);
    let point = |tick| {
        capture.row(tick)[trip.destination_repository]
            .map(|tile| point_on_tile(tile, fraction))
    };
    (first..trip.arrival).any(|tick| {
        point(tick)
            .zip(point(tick + 1))
            .is_some_and(|(from, to)| from.distance(to) * 60.0 > speed + 0.05)
    })
}

fn fixed_dock_approach(
    capture: &Capture,
    template: &Trip,
    first: u64,
    speed: f32,
    router: &mut Router,
    blocked: Option<(u64, Vec2)>,
) -> Option<Trip> {
    let late = (blocked.is_some() || template.kind == TripKind::Relocation)
        .then_some(template.arrival.saturating_sub(1))
        .filter(|&tick| tick > first);
    let epochs = blocked.map(|(tick, _)| tick).into_iter();
    for tick in epochs.chain([first, template.arrival]) {
        let cells = super::network(capture.row(tick).iter().flatten().copied());
        for from in late.into_iter().chain([first]) {
            let source = capture.pose(template.author, from)?.point;
            if let Some(approach) = fixed_approach(
                capture,
                template,
                first,
                speed,
                router,
                source,
                &cells,
                tick,
                blocked.map(|(_, point)| point),
            ) {
                return Some(approach);
            }
        }
    }
    None
}

#[expect(
    clippy::too_many_arguments,
    reason = "a late approach checks a route against the sampled network"
)]
fn fixed_approach(
    capture: &Capture,
    template: &Trip,
    first: u64,
    speed: f32,
    router: &mut Router,
    source: Vec2,
    cells: &[Rect],
    epoch: u64,
    blocked: Option<Vec2>,
) -> Option<Trip> {
    let points = router
        .route_avoiding(source, template.destination, cells, blocked)
        .ok()?;
    if points.len() < 2 {
        return None;
    }
    let route = Route::new(&points, speed);
    let duration = u64::from(super::seconds_ticks(route.duration, true));
    let departure = template.arrival.checked_sub(duration)?;
    if departure < first || capture.pose(template.author, departure)?.point != source {
        return None;
    }
    let track: Vec<_> = (departure..=template.arrival)
        .map(|tick| {
            route
                .sample(ticks_duration(tick - departure))
                .expect("nonempty route")
        })
        .collect();
    if !track.iter().enumerate().all(|(offset, &point)| {
        let tick = departure + offset as u64;
        hard_clearance(capture, tick, point)
            && (tick == departure
                || tick == template.arrival
                || route_clearance(capture, template, tick, point))
            && super::on_network(
                point,
                &super::network(capture.row(tick).iter().flatten().copied()),
            )
    }) {
        return None;
    }
    if pauses_away_from_source(&track, source) {
        return None;
    }
    let mut trip = template.clone();
    trip.departure = departure;
    trip.source = source;
    trip.fade = false;
    trip.failure = None;
    trip.proof = Some(route_proof(&points, epoch));
    trip.route = Route {
        track: track.into(),
        ..route
    };
    Some(trip)
}

fn needs_trip(
    capture: &Capture,
    prior: &super::Service,
    next: &super::Service,
    must_walk: bool,
) -> bool {
    let visible = capture
        .pose(next.author, next.tick - 1)
        .is_none_or(|pose| pose.visible);
    if prior.repository == next.repository {
        next.authoring
            && prior.dock != next.dock
            && prior.arrivals_land < next.tick - 1
            && visible
    } else {
        must_walk || visible
    }
}

pub(super) fn trips(
    capture: &Capture,
    speed: f32,
    bots: crate::model::BotTravel,
    walk: bool,
) -> (Vec<Trip>, usize) {
    let mut previous = vec![None; capture.poses.len()];
    let mut result = Vec::<Trip>::new();
    let mut router = Router::default();
    for (index, service) in capture.services.iter().enumerate() {
        let Some(prior) = previous[service.author]
            .replace(index)
            .map(|i| &capture.services[i])
        else {
            continue;
        };
        let commit = service.commit;
        if commit < capture.frozen {
            continue;
        }
        let must_walk = walk && (!service.bot || bots != crate::model::BotTravel::Fade);
        if !needs_trip(capture, prior, service, must_walk) {
            continue;
        }
        let relocation = prior.repository == service.repository;
        let earliest = capture.begin + if capture.begin == 0 { 0 } else { 120 };
        let free = (prior.tick + 1)
            .max(earliest)
            .max(prior.arrivals_land)
            .min(service.tick - 1);
        let source = capture
            .pose(service.author, free)
            .expect("existing author")
            .point;
        let kind = if relocation {
            TripKind::Relocation
        } else if capture.births.binary_search(&commit).is_ok() {
            TripKind::Birth
        } else {
            TripKind::Transfer
        };
        let mut best = Trip {
            author: service.author,
            commit,
            departure: free,
            arrival: service.tick,
            destination: service.point,
            source,
            fade_ticks: 0,
            route: Route::default(),
            kind,
            fade: true,
            bot: service.bot,
            source_repository: prior.repository,
            destination_repository: service.repository,
            free,
            failure: Some("endpoint"),
            cutoff: None,
            proof: None,
        };
        if relocation || !service.bot || bots == crate::model::BotTravel::Walk {
            best = search(
                capture,
                &best,
                prior.release,
                speed,
                &mut router,
                Search::Fit {
                    mandatory_birth_walk: relocation
                        || (walk && kind == TripKind::Birth),
                },
            );
        }
        if best.route.is_empty() {
            if relocation {
                continue;
            }
            best.departure = service
                .tick
                .saturating_sub(super::FADE_TICKS)
                .max(prior.release.min(service.tick - 1))
                .max(free);
            best.source = capture
                .pose(service.author, best.departure)
                .expect("existing author")
                .point;
            best.fade_ticks = fade_clearance(
                capture,
                best.departure,
                best.source,
                prior.radius.max(service.radius),
            )
            .min(best.arrival - best.departure);
        }
        result.push(best);
    }
    (result, router.epochs)
}

pub(super) fn needs_alignment(trip: &Trip, frozen: usize) -> bool {
    trip.commit >= frozen
        && !trip.fade
        && !trip.route.is_empty()
        && trip.departure + u64::from(super::seconds_ticks(trip.route.duration, true))
            < trip.arrival
}

fn last_release(capture: &Capture, trip: &Trip) -> u64 {
    let earlier = capture
        .services
        .partition_point(|service| service.commit < trip.commit);
    capture.services[..earlier]
        .iter()
        .rev()
        .find(|service| service.author == trip.author)
        .map_or(trip.free, |service| service.release.max(trip.free))
}

fn separate_coarrival(
    capture: &Capture,
    trip: &Trip,
    lead: Vec2,
    release: u64,
    speed: f32,
    router: &mut Router,
) -> Option<Trip> {
    // Leave space for a co-author's first step into the ring during service.
    let separation = 2.0 * (capture.radius(trip.arrival) + speed / 60.0);
    let cells = super::network(capture.row(trip.arrival).iter().flatten().copied());
    let clear = (trip.departure..trip.arrival)
        .rev()
        .map(|tick| trip.sample(tick).0)
        .find(|&point| {
            point.distance_squared(lead) >= separation.powi(2)
                && super::on_network(point, &cells)
        })?;
    let mut template = trip.clone();
    template.destination = clear;
    let aligned = search(
        capture,
        &template,
        release,
        speed,
        router,
        Search::Align {
            blocked: None,
            pace: 0.8,
        },
    );
    if aligned.route.track == trip.route.track {
        fixed_dock_approach(capture, &template, release, speed, router, None)
    } else {
        Some(aligned)
    }
}

pub(super) fn realign_blocked_walks(
    capture: &Capture,
    trips: &mut [Trip],
    speed: f32,
) -> (usize, bool) {
    let Some(obstacles) = &capture.obstacles else {
        return (0, false);
    };
    let mut router = Router::default();
    let mut changed = false;
    for trip in trips.iter_mut().filter(|trip| obstacles.affects(trip)) {
        let release = last_release(capture, trip).max(capture.begin);
        if release >= trip.arrival {
            continue;
        }
        let different = |candidate: &Trip| {
            candidate.departure != trip.departure
                || candidate.route.track != trip.route.track
        };
        let key = (trip.commit, trip.author);
        let approach = if let Some(&lead) = obstacles.coarrivals.get(&key) {
            separate_coarrival(capture, trip, lead, release, speed, &mut router)
        } else {
            repair_walk(capture, trip, release, speed, obstacles, &mut router)
        };
        if let Some(approach) = approach.filter(different) {
            *trip = approach;
            changed = true;
        }
    }
    (router.epochs, changed)
}

fn repair_walk(
    capture: &Capture,
    trip: &Trip,
    release: u64,
    speed: f32,
    obstacles: &super::WalkObstacles,
    router: &mut Router,
) -> Option<Trip> {
    if let Some(direct) = timed_approach(capture, trip, release, speed, router, None) {
        return Some(direct);
    }
    let failed_sample = (trip.departure + 1..trip.arrival).find_map(|tick| {
        let (point, _) = trip.sample(tick);
        (!route_clearance(capture, trip, tick, point)).then_some((tick, point))
    });
    let key = (trip.commit, trip.author);
    let blocked = obstacles
        .blocked
        .get(&key)
        .copied()
        .into_iter()
        .chain(obstacles.contacted.get(&key).map(|contact| contact.first))
        .chain(failed_sample)
        .min_by_key(|&(tick, _)| tick);
    blocked
        .and_then(|(_, point)| {
            timed_approach(capture, trip, release, speed, router, Some(point))
        })
        .or_else(|| fixed_dock_approach(capture, trip, release, speed, router, blocked))
}

fn timed_approach(
    capture: &Capture,
    trip: &Trip,
    release: u64,
    speed: f32,
    router: &mut Router,
    blocked: Option<Vec2>,
) -> Option<Trip> {
    [0.8, 1.0].into_iter().find_map(|pace| {
        let aligned = search(
            capture,
            trip,
            release,
            speed,
            router,
            Search::Align { blocked, pace },
        );
        (aligned.departure != trip.departure || aligned.route.track != trip.route.track)
            .then_some(aligned)
    })
}

pub(super) fn align_arrivals<'a>(
    capture: &Capture,
    trips: impl IntoIterator<Item = &'a mut Trip>,
    speed: f32,
) -> usize {
    let mut router = Router::default();
    for trip in trips
        .into_iter()
        .filter(|trip| needs_alignment(trip, capture.frozen))
    {
        let release = last_release(capture, trip).max(trip.departure);
        if release < trip.arrival {
            let aligned = search(
                capture,
                trip,
                release,
                speed,
                &mut router,
                Search::Align {
                    blocked: None,
                    pace: 0.8,
                },
            );
            *trip = if aligned.kind == TripKind::Relocation
                && needs_alignment(&aligned, capture.frozen)
            {
                let first = release.max(aligned.free);
                fixed_dock_approach(capture, &aligned, first, speed, &mut router, None)
                    .unwrap_or(aligned)
            } else {
                aligned
            };
        }
    }
    router.epochs
}

#[cfg(test)]
mod tests {
    use super::super::{Plan, Service, Statistics, slots::Dock};
    use super::*;

    #[test]
    fn completed_travel_survives_the_source_repository_disappearing() {
        let mut capture = fixture();
        for row in &mut capture.gutters[300..] {
            row[0] = None;
        }
        let (trips, _) = trips(&capture, 480.0, crate::model::BotTravel::Walk, true);
        assert_eq!(trips.len(), 1);
        assert_eq!(trips[0].departure, 70);
        assert!(!trips[0].fade);
        assert_eq!(trips[0].route.track.last(), Some(&Vec2::new(100.0, 0.0)));
    }

    #[test]
    fn a_relocation_within_the_same_repository_waits_for_the_last_arrival() {
        let mut capture = fixture();
        capture.services[0].arrivals_land = 410;
        capture.services[2].repository = 0;
        capture.services[2].point = Vec2::ZERO;

        let (planned, _) = trips(&capture, 480.0, crate::model::BotTravel::Walk, true);

        assert!(
            planned.iter().all(|trip| {
                trip.kind != TripKind::Relocation
                    || trip.departure >= capture.services[0].arrivals_land
            }),
            "{planned:?}",
        );
    }

    #[test]
    fn a_same_repository_co_author_keeps_their_dock() {
        let mut capture = fixture();
        capture.services[2].repository = 0;
        capture.services[2].point = Vec2::ZERO;
        capture.services[2].authoring = false;

        let (planned, _) = trips(&capture, 480.0, crate::model::BotTravel::Walk, true);

        assert!(planned.is_empty());
    }

    #[test]
    fn a_calendar_departure_fades_even_during_an_uncapped_trip() {
        let mut events = crate::timeline::synthetic();
        events.truncate(1);
        let mut sim = crate::model::Simulation::new(
            events,
            &crate::model::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        sim.departures[0] = crate::model::Time::seconds(1.2);
        let (trips, _) = trips(&fixture(), 480.0, crate::model::BotTravel::Walk, true);
        sim.travel = Plan {
            trips,
            ..Default::default()
        };
        sim.advance_to(180, 60);
        assert!(sim.travel.sample(0, sim.state.tick).is_some());
        assert_eq!(sim.state.avatars[0].presence, 0.0);
    }

    #[test]
    fn a_late_relocation_leaves_from_the_reflowed_dock() {
        let mut capture = fixture();
        let (mut planned, _) =
            trips(&capture, 480.0, crate::model::BotTravel::Walk, true);
        let mut trip = planned.pop().unwrap();
        trip.kind = TripKind::Relocation;
        let reflowed = Vec2::new(-100.0, 16.0);
        for pose in &mut capture.poses[0][300..400] {
            pose.point = reflowed;
        }

        let aligned = fixed_dock_approach(
            &capture,
            &trip,
            70,
            480.0,
            &mut Router::default(),
            None,
        )
        .expect("the source has settled before the late departure");

        assert_eq!(aligned.source, reflowed);
        assert!(!needs_alignment(&aligned, capture.frozen));
    }

    #[test]
    fn a_relocation_uses_extra_ticks_for_motion_instead_of_early_parking() {
        let capture = fixture();
        let (mut planned, _) =
            trips(&capture, 480.0, crate::model::BotTravel::Walk, true);
        let mut trip = planned.pop().unwrap();
        trip.kind = TripKind::Relocation;
        let cells = super::super::network(
            capture.row(trip.departure).iter().flatten().copied(),
        );
        let points = Router::default()
            .route(trip.source, trip.destination, &cells)
            .unwrap();
        trip.route = Route::new(&points, 384.0);
        trip.arrival = trip.departure
            + u64::from(super::super::seconds_ticks(trip.route.duration, true))
            + 4;

        let aligned =
            moving_route(&capture, &trip, &points, trip.departure, 480.0, 0, true)
                .expect("a slightly slower walk can arrive on time");

        assert!(aligned.track.windows(2).all(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn a_late_walk_chooses_a_clear_departure_when_a_transmitter_blocks_the_gutter() {
        let mut capture = fixture();
        let speed = 480.0;
        let (mut planned, _) =
            trips(&capture, speed, crate::model::BotTravel::Walk, true);
        let template = planned.pop().unwrap();
        let mut latest = template.clone();
        align_arrivals(&capture, [&mut latest], speed);
        let blocked_tick = latest.departure.midpoint(latest.arrival);
        let (blocked_point, _) = latest.sample(blocked_tick);
        let transmitter = super::super::Transmitters::at(1, blocked_point);
        assert!(!transmitter.clears(template.author, blocked_point));
        capture.obstacles = Some(super::super::WalkObstacles {
            begin: blocked_tick,
            frames: vec![transmitter],
            blocked: [(
                (template.commit, template.author),
                (blocked_tick, blocked_point),
            )]
            .into(),
            ..Default::default()
        });

        let mut clear = template;
        realign_blocked_walks(&capture, std::slice::from_mut(&mut clear), speed);

        assert!(clear.departure < latest.departure);
        assert_eq!(clear.arrival, latest.arrival);
        assert!(!needs_alignment(&clear, capture.frozen));
        assert!(route_clearance(
            &capture,
            &clear,
            blocked_tick,
            clear.sample(blocked_tick).0,
        ));
    }

    #[test]
    fn a_coauthor_finishes_the_walk_clear_of_the_author() {
        let capture = fixture();
        let speed = 480.0;
        let (mut planned, _) =
            trips(&capture, speed, crate::model::BotTravel::Walk, true);
        align_arrivals(&capture, &mut planned, speed);
        let trip = planned.pop().unwrap();
        let lead = trip.destination;

        let clear = separate_coarrival(
            &capture,
            &trip,
            lead,
            last_release(&capture, &trip),
            speed,
            &mut Router::default(),
        )
        .expect("the coauthor can approach along the same gutter");

        assert_eq!(clear.arrival, trip.arrival);
        assert!(
            clear.sample(clear.arrival - 1).0.distance(lead)
                >= 2.0 * capture.radius(clear.arrival)
        );
        assert!(
            clear
                .route
                .track
                .windows(2)
                .all(|pair| { pair[0].distance(pair[1]) * 60.0 <= speed + 0.05 })
        );
    }

    #[test]
    fn a_detour_avoids_the_first_blockage_even_with_a_later_contact() {
        let mut capture = fixture();
        let speed = 480.0;
        let (mut planned, _) =
            trips(&capture, speed, crate::model::BotTravel::Walk, true);
        align_arrivals(&capture, &mut planned, speed);
        let trip = &planned[0];
        let key = (trip.commit, trip.author);
        let tick = trip.departure.midpoint(trip.arrival);
        let blocked_point = trip.sample(tick).0;
        capture.obstacles = Some(super::super::WalkObstacles {
            frames: (0..=trip.arrival)
                .map(|_| super::super::Transmitters::at(1, blocked_point))
                .collect(),
            blocked: [(key, (tick, blocked_point))].into(),
            contacted: [(
                key,
                super::super::WalkContact {
                    first: (trip.arrival - 1, trip.destination),
                    peers: std::collections::BTreeSet::new(),
                },
            )]
            .into(),
            ..Default::default()
        });
        let arrival = trip.arrival;
        let destination = trip.destination;

        let (_, repaired) = realign_blocked_walks(&capture, &mut planned, speed);

        assert!(repaired);
        let detour = &planned[0];
        assert_eq!(detour.arrival, arrival);
        assert_eq!(detour.destination, destination);
        assert!((detour.departure + 1..arrival).all(|tick| route_clearance(
            &capture,
            detour,
            tick,
            detour.sample(tick).0
        )));
    }

    #[test]
    fn a_failed_walk_tries_an_earlier_departure() {
        let mut capture = fixture();
        for pose in &mut capture.poses[0][201..400] {
            pose.point = Vec2::new(-100.0, 51.0);
        }
        let speed = 480.0;
        let (mut planned, _) =
            trips(&capture, speed, crate::model::BotTravel::Walk, true);
        align_arrivals(&capture, &mut planned, speed);
        let trip = &planned[0];
        assert_eq!(trip.departure, 200);

        let retry = timed_approach(
            &capture,
            trip,
            last_release(&capture, trip),
            speed,
            &mut Router::default(),
            None,
        )
        .unwrap();

        assert!(retry.departure < trip.departure);
        assert_eq!(retry.arrival, trip.arrival);
    }

    #[test]
    fn an_idle_contributor_can_return_to_a_different_seat_without_a_walk() {
        let mut capture = fixture();
        let next = capture.services.last_mut().unwrap();
        next.repository = 0;
        next.point = Vec2::new(-100.0, 25.0);
        next.dock = dock(3, 0.75);
        capture.poses[0][399].visible = false;

        let (trips, _) = trips(&capture, 480.0, crate::model::BotTravel::Walk, true);

        assert_eq!(trips.len(), 0);
    }

    #[test]
    fn a_relocation_that_reaches_the_same_berth_needs_no_walk() {
        let capture = fixture();
        let (mut planned, _) =
            trips(&capture, 480.0, crate::model::BotTravel::Walk, true);
        let trip = &mut planned[0];
        trip.kind = TripKind::Relocation;
        trip.destination_repository = trip.source_repository;
        trip.destination = trip.source;

        align_arrivals(&capture, &mut planned, 480.0);

        assert!(planned[0].route.is_empty());
    }

    fn dock(edge: u8, fraction: f32) -> Dock {
        Dock::new(edge, fraction)
    }
    fn fixture() -> Capture {
        let a = Tile {
            low: Vec2::new(-100.0, -50.0) / HALF,
            high: Vec2::new(0.0, 50.0) / HALF,
        };
        let b = Tile {
            low: Vec2::new(0.0, -50.0) / HALF,
            high: Vec2::new(100.0, 50.0) / HALF,
        };
        Capture {
            begin: 0,
            frozen: 0,
            field: super::super::field_rect(Tile::inset(0.0)),
            services: [
                (0, 0, 10, 70, Vec2::new(-100.0, 0.0)),
                (1, 1, 90, 150, Vec2::new(100.0, 0.0)),
                (0, 1, 400, 460, Vec2::new(100.0, 0.0)),
            ]
            .into_iter()
            .enumerate()
            .map(
                |(commit, (author, repository, tick, release, point))| Service {
                    commit,
                    author,
                    authoring: true,
                    repository,
                    tick,
                    release,
                    arrivals_land: 0,
                    point,
                    radius: 7.5,
                    bot: false,
                    dock: dock(if author == 0 && tick < 400 { 3 } else { 1 }, 0.5),
                },
            )
            .collect(),
            poses: (0..2)
                .map(|author| {
                    (0..=460)
                        .map(|tick| Pose {
                            tick,
                            point: Vec2::new(
                                if author == 0 && tick < 400 {
                                    -100.0
                                } else {
                                    100.0
                                },
                                0.0,
                            ),
                            dock: dock(
                                if author == 0 && tick < 400 { 3 } else { 1 },
                                0.5,
                            ),
                            visible: true,
                        })
                        .collect()
                })
                .collect(),
            tiles: vec![vec![]; 461],
            gutters: vec![vec![Some(a), Some(b)]; 461],
            radii: vec![7.5; 461],
            births: vec![],
            obstacles: None,
        }
    }
    #[test]
    fn departure_follows_own_fragments_and_overlaps_another_commit() {
        let capture = fixture();
        let (trips, _) = trips(&capture, 480.0, crate::model::BotTravel::Walk, true);
        assert_eq!(trips.len(), 1);
        assert_eq!(trips[0].departure, 70);
        assert!(!trips[0].fade);
        assert!(trips[0].route.duration + ticks_duration(70) > ticks_duration(90));
        let plan = Plan {
            trips,
            docks: std::collections::BTreeMap::new(),
            replanning: false,
            frozen: 0,
            occupancy: std::collections::BTreeMap::new(),
            unreached_docks: std::collections::BTreeMap::new(),
            stats: Statistics::default(),
        };
        assert!(plan.sample(0, 69).is_none());
        assert!(plan.walking(0, 90));
    }
    #[test]
    fn a_shared_parking_point_does_not_create_a_trip_or_hide_a_pulse() {
        let mut capture = fixture();
        for service in &mut capture.services {
            service.point = Vec2::ZERO;
        }
        for pose in &mut capture.poses[0] {
            pose.point = Vec2::ZERO;
            pose.dock = dock(1, 0.5);
        }
        let (trips, _) = trips(&capture, 480.0, crate::model::BotTravel::Walk, true);
        let plan = Plan {
            trips,
            docks: std::collections::BTreeMap::new(),
            replanning: false,
            frozen: 0,
            occupancy: std::collections::BTreeMap::new(),
            unreached_docks: std::collections::BTreeMap::new(),
            stats: Statistics::default(),
        };
        assert!(!plan.travelling(0, 90));
        assert!(plan.sample(0, 90).is_none());
    }
    #[test]
    fn other_travellers_cannot_change_a_route_or_its_departure() {
        let mut capture = fixture();
        let (trips, _) = trips(&capture, 480.0, crate::model::BotTravel::Walk, true);
        let expected = Plan {
            trips,
            docks: std::collections::BTreeMap::new(),
            replanning: false,
            frozen: 0,
            occupancy: std::collections::BTreeMap::new(),
            unreached_docks: std::collections::BTreeMap::new(),
            stats: Statistics::default(),
        }
        .report();
        for pose in &mut capture.poses[1] {
            pose.point = Vec2::new(-100.0, 0.0);
        }
        let (trips, _) =
            super::trips(&capture, 480.0, crate::model::BotTravel::Walk, true);
        assert_eq!(
            expected,
            Plan {
                trips,
                docks: std::collections::BTreeMap::new(),
                replanning: false,
                frozen: 0,
                occupancy: std::collections::BTreeMap::new(),
                unreached_docks: std::collections::BTreeMap::new(),
                stats: Statistics::default()
            }
            .report()
        );
    }
    #[test]
    fn a_capped_bot_fade_leaves_time_to_show_the_previous_commit() {
        let mut capture = fixture();
        capture.services[1].tick = 12;
        capture.services[2].tick = 14;
        capture.services[2].bot = true;
        let (trips, _) = trips(&capture, 480.0, crate::model::BotTravel::Fade, false);
        assert_eq!(trips[0].departure, 13);
        let plan = Plan {
            trips,
            docks: std::collections::BTreeMap::new(),
            replanning: false,
            frozen: 0,
            occupancy: std::collections::BTreeMap::new(),
            unreached_docks: std::collections::BTreeMap::new(),
            stats: Statistics::default(),
        };
        assert!(!plan.travelling(0, 11));
        assert!(!plan.travelling(0, 12));
        assert!(plan.travelling(0, 13));
    }
}
