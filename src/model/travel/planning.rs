use super::{
    Capture, HALF, Pose, Rect, Route, Router, Trip, Vec2, fade_clearance, tick_time,
};
use crate::layout::packing::Tile;

impl Capture {
    fn pose(&self, author: usize, tick: u64) -> Option<Pose> {
        let poses = self.poses.get(author)?;
        poses
            .partition_point(|p| p.tick <= tick)
            .checked_sub(1)
            .map(|i| poses[i])
    }
    fn row(&self, tick: u64) -> &[Option<Tile>] {
        &self.gutters[usize::try_from(tick)
            .expect("bounded tick")
            .min(self.gutters.len() - 1)]
    }
    fn radius(&self, tick: u64) -> f32 {
        self.radii[usize::try_from(tick)
            .expect("bounded tick")
            .min(self.radii.len() - 1)]
    }
}

fn hard_clearance(capture: &Capture, tick: u64, point: Vec2) -> bool {
    point
        .abs()
        .cmple(HALF - Vec2::splat(capture.radius(tick)))
        .all()
        && point.y - capture.radius(tick) >= super::super::caption::BOTTOM
        && point.y + capture.radius(tick) <= super::super::caption::TOP
        && !capture.tiles[usize::try_from(tick)
            .expect("bounded tick")
            .min(capture.tiles.len() - 1)]
        .iter()
        .flatten()
        .any(|t| super::in_guard(*t, capture.radius(tick), point))
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
        let coordinate = reference.iter().enumerate().find_map(|(repository, t)| {
            let tile = t.as_ref()?;
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
        });
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
) -> Option<Route> {
    let anchors = route_anchors(capture.row(epoch), trip, points)?;
    let shape = Route::new(points, speed * 0.8);
    let duration = shape.duration + tick_time(animation_delay);
    let mut track = Vec::new();
    for offset in 0..=trip.arrival - trip.departure {
        let tick = trip.departure + offset;
        let row = capture.row(tick);
        let elapsed = tick_time(offset.saturating_sub(animation_delay));
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
        let point = if u >= 1.0 {
            b
        } else {
            Vec2::new(anchors[leg][0].at(row, 0)?, anchors[leg][1].at(row, 1)?)
                .lerp(b, u)
        };
        let cells: Vec<_> = row
            .iter()
            .flatten()
            .map(|t| Rect::from_corners(t.low * HALF, t.high * HALF))
            .collect();
        if !super::on_network(point, &cells) || !hard_clearance(capture, tick, point) {
            return None;
        }
        track.push(point);
    }
    let peak = track
        .windows(2)
        .map(|p| p[0].distance(p[1]) * 60.0)
        .fold(0.0, f32::max);
    let previous = capture.pose(trip.author, epoch.saturating_sub(1))?.point;
    if peak > speed + 0.05
        || track.first()?.distance(previous) * 60.0 > speed + 0.05
        || track.last()?.distance(trip.destination) * 60.0 > speed + 0.05
    {
        return None;
    }
    Some(Route {
        legs: Vec::new(),
        track,
        duration,
    })
}

fn route_proof(points: &[Vec2], epoch: u64) -> serde_json::Value {
    let length: f32 = points.windows(2).map(|p| p[0].distance(p[1])).sum();
    serde_json::json!({"network_epoch":epoch,"shortest_network_length":length,"route_network_length":length,"network_points":points.iter().map(Vec2::to_array).collect::<Vec<_>>()})
}
fn search(
    capture: &Capture,
    template: &Trip,
    release: u64,
    speed: f32,
    router: &mut Router,
) -> Trip {
    let mut best = template.clone();
    let first = release.min(template.arrival - 1).max(template.free);
    let Some(target_tile) =
        capture.row(template.arrival)[template.destination_repository]
    else {
        return best;
    };
    let target_fraction = (template.destination / HALF - target_tile.low)
        / (target_tile.high - target_tile.low);
    for begin in first..template.arrival {
        let row = capture.row(begin);
        let Some(target_tile) = row[template.destination_repository] else {
            continue;
        };
        let source = capture
            .pose(template.author, begin)
            .expect("existing author")
            .point;
        let target = (target_tile.low
            + (target_tile.high - target_tile.low) * target_fraction)
            * HALF;
        let cells: Vec<_> = row
            .iter()
            .flatten()
            .map(|t| Rect::from_corners(t.low * HALF, t.high * HALF))
            .collect();
        let points = match router.route(source, target, &cells) {
            Ok(points) => points,
            Err(e) => {
                best.failure = Some(e.name());
                continue;
            }
        };
        if points.len() < 2 {
            let source_tile = capture.row(template.arrival)[template.source_repository]
                .expect("existing source");
            let dock = capture
                .pose(template.author, begin)
                .expect("existing author")
                .dock;
            if dock.on_network(source_tile).distance(template.destination)
                < super::NETWORK_EPSILON
            {
                best.fade = false;
                best.route = Route::default();
                best.failure = None;
                return best;
            }
            continue;
        }
        let mut candidate = template.clone();
        candidate.proof = Some(route_proof(&points, begin));
        candidate.departure = begin;
        candidate.source = source;
        let animation_delay = release.saturating_sub(begin);
        candidate.route = Route::new(&points, speed * 0.8);
        candidate.route.duration += tick_time(animation_delay);
        candidate.fade =
            candidate.route.duration > tick_time(candidate.arrival - begin);
        candidate.failure = Some("moving_network");
        if let Some(route) =
            moving_route(capture, &candidate, &points, begin, speed, animation_delay)
        {
            candidate.route = route;
            candidate.failure = None;
            return candidate;
        }
        if best.route.is_empty() {
            best = candidate;
            best.fade = true;
            best.cutoff = Some(begin);
        }
    }
    best
}
pub(super) fn trips(
    capture: &Capture,
    speed: f32,
    bots: crate::model::BotTravel,
    keep_idle: bool,
) -> (Vec<Trip>, usize) {
    let mut previous = vec![None; capture.poses.len()];
    let mut result = Vec::<Trip>::new();
    let mut router = Router::default();
    for (commit, service) in capture.services.iter().enumerate() {
        let Some(prior) = previous[service.author]
            .replace(commit)
            .map(|i| &capture.services[i])
        else {
            continue;
        };
        if (prior.repository == service.repository && prior.dock == service.dock)
            || ((!keep_idle || (service.bot && bots == crate::model::BotTravel::Fade))
                && tick_time(service.tick - prior.tick)
                    >= crate::model::AVATAR_IDLE + 1.2)
        {
            continue;
        }
        let free = (prior.tick + 1).min(service.tick - 1);
        let source = capture
            .pose(service.author, free)
            .expect("existing author")
            .point;
        let mut best = Trip {
            author: service.author,
            commit,
            departure: free,
            arrival: service.tick,
            destination: service.point,
            source,
            fade_ticks: 0,
            route: Route::default(),
            fade: true,
            bot: service.bot,
            source_repository: prior.repository,
            destination_repository: service.repository,
            free,
            failure: Some("endpoint"),
            cutoff: None,
            proof: None,
        };
        if !service.bot || bots == crate::model::BotTravel::Walk {
            best = search(capture, &best, prior.release, speed, &mut router);
        }
        if best.route.is_empty() {
            best.departure = service
                .tick
                .saturating_sub(9)
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
    fn a_calendar_departure_fades_even_during_an_uncapped_trip() {
        let mut events = crate::timeline::synthetic();
        events.truncate(1);
        let mut sim = crate::model::Simulation::new(
            events,
            &crate::model::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        sim.departures[0] = 1.2;
        let (trips, _) = trips(&fixture(), 480.0, crate::model::BotTravel::Walk, true);
        sim.travel = Plan {
            trips,
            ..Default::default()
        };
        sim.advance_to(180, 60);
        assert!(sim.travel.sample(0, sim.state.tick).is_some());
        assert_eq!(sim.state.avatars[0].presence, 0.0);
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
            services: [
                (0, 0, 10, 70, Vec2::new(-100.0, 0.0)),
                (1, 1, 90, 150, Vec2::new(100.0, 0.0)),
                (0, 1, 400, 460, Vec2::new(100.0, 0.0)),
            ]
            .into_iter()
            .map(|(author, repository, tick, release, point)| Service {
                author,
                repository,
                tick,
                release,
                point,
                radius: 7.5,
                bot: false,
                dock: dock(if author == 0 && tick < 400 { 3 } else { 1 }, 0.5),
            })
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
                        })
                        .collect()
                })
                .collect(),
            tiles: vec![vec![]; 461],
            gutters: vec![vec![Some(a), Some(b)]; 461],
            radii: vec![7.5; 461],
        }
    }
    #[test]
    fn departure_follows_own_fragments_and_overlaps_another_commit() {
        let capture = fixture();
        let (trips, _) = trips(&capture, 480.0, crate::model::BotTravel::Walk, true);
        assert_eq!(trips.len(), 1);
        assert_eq!(trips[0].departure, 70);
        assert!(!trips[0].fade);
        assert!(trips[0].route.duration + tick_time(70) > tick_time(90));
        let plan = Plan {
            trips,
            docks: Vec::new(),
            replanning: false,
            occupancy: Vec::new(),
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
            docks: Vec::new(),
            replanning: false,
            occupancy: Vec::new(),
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
            docks: Vec::new(),
            replanning: false,
            occupancy: Vec::new(),
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
                docks: Vec::new(),
                replanning: false,
                occupancy: Vec::new(),
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
            docks: Vec::new(),
            replanning: false,
            occupancy: Vec::new(),
            stats: Statistics::default(),
        };
        assert!(!plan.travelling(0, 11));
        assert!(!plan.travelling(0, 12));
        assert!(plan.travelling(0, 13));
    }
}
