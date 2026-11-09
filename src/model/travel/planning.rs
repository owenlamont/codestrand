use super::{
    Capture, Costs, Disc, HALF, Pose, Rect, Route, Router, Trip, Vec2, fade_clearance,
    seconds_ticks, tick_time,
};

impl Capture {
    fn pose(&self, author: usize, tick: u64) -> Option<Pose> {
        let poses = self.poses.get(author)?;
        let i = poses.partition_point(|p| p.tick <= tick);
        i.checked_sub(1).map(|i| poses[i])
    }

    fn radius(&self, tick: u64) -> f32 {
        self.radii[usize::try_from(tick)
            .expect("bounded tick")
            .min(self.radii.len() - 1)]
    }

    fn boundaries(&self, tick: u64) -> Vec<Rect> {
        self.tiles[usize::try_from(tick)
            .expect("bounded tick")
            .min(self.tiles.len() - 1)]
        .iter()
        .flatten()
        .map(|t| Rect::from_corners(t.low * HALF, t.high * HALF))
        .collect()
    }
    fn tiles(&self, tick: u64, radius: f32) -> Vec<Rect> {
        self.tiles[usize::try_from(tick)
            .expect("bounded tick")
            .min(self.tiles.len() - 1)]
        .iter()
        .flatten()
        .map(|t| super::interior(*t, radius))
        .collect()
    }
}

fn occupied(
    capture: &Capture,
    trips: &[Trip],
    author: usize,
    tick: u64,
    point: Vec2,
) -> Option<&'static str> {
    let radius = capture.radius(tick);
    if point.abs().cmpgt(HALF - Vec2::splat(radius)).any() {
        return Some("frame_edge");
    }
    if capture
        .tiles(tick, radius)
        .iter()
        .any(|r| r.contains(point))
    {
        return Some("moving_tile");
    }
    for other in 0..capture.poses.len() {
        if other == author {
            continue;
        }
        let reserved = trips
            .iter()
            .find(|t| t.author == other && tick >= t.departure && tick < t.arrival);
        let pose = reserved
            .map(|t| {
                let (point, alpha) = t.sample(tick);
                (point, alpha > 0.0)
            })
            .or_else(|| capture.pose(other, tick).map(|p| (p.point, p.visible)));
        if pose.is_some_and(|(p, visible)| visible && p.distance(point) < 2.0 * radius)
        {
            return Some("occupied_window");
        }
    }
    None
}

fn stationary(
    capture: &Capture,
    trips: &[Trip],
    author: usize,
    begin: u64,
    end: u64,
) -> Vec<Disc> {
    let radius = (begin..=end)
        .map(|t| capture.radius(t))
        .fold(f32::INFINITY, f32::min);
    capture
        .poses
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != author)
        .filter_map(|(other, poses)| {
            if trips
                .iter()
                .any(|t| t.author == other && t.departure < end && t.arrival > begin)
            {
                return None;
            }
            let first = poses.partition_point(|p| p.tick < begin);
            let last = poses.partition_point(|p| p.tick <= end);
            let slice = &poses[first..last];
            let point = slice.first()?.point;
            (u64::try_from(slice.len()).expect("bounded poses") == end - begin + 1
                && slice
                    .iter()
                    .all(|p| p.visible && p.point.distance(point) < 0.01))
            .then_some(Disc {
                point,
                radius: 2.0 * radius - 0.01,
            })
        })
        .collect()
}

fn reserve(capture: &Capture, prior: &[Trip], trip: &mut Trip) {
    let mut delay = 0;
    for index in 0..trip.route.legs.len() {
        loop {
            let leg = &trip.route.legs[index];
            let begin = trip.departure + u64::from(seconds_ticks(leg.start, true));
            let end = (trip.departure
                + u64::from(seconds_ticks(leg.start + leg.duration, true)))
            .min(trip.arrival - 1);
            let collision = (begin..=end).find(|&t| {
                occupied(
                    capture,
                    prior,
                    trip.author,
                    t,
                    leg.sample(tick_time(t - trip.departure)),
                )
                .is_some()
            });
            if collision.is_none() {
                break;
            }
            let wait_clear = (trip.departure..=begin.min(trip.arrival - 1)).all(|t| {
                let point = trip
                    .route
                    .sample(tick_time(t - trip.departure))
                    .unwrap_or(trip.source);
                occupied(capture, prior, trip.author, t, point).is_none()
            });
            if !wait_clear || delay >= 30 {
                break;
            }
            for leg in &mut trip.route.legs[index..] {
                leg.start += 1.0 / 60.0;
            }
            trip.route.duration += 1.0 / 60.0;
            delay += 1;
        }
    }
    trip.fade = trip.route.duration > tick_time(trip.arrival - trip.departure);
    for tick in trip.departure..trip.arrival {
        let (point, alpha) = trip.sample(tick);
        if alpha > 0.0
            && let Some(reason) = occupied(capture, prior, trip.author, tick, point)
        {
            trip.cutoff = Some(tick);
            trip.failure = Some(reason);
            trip.fade = true;
            break;
        }
    }
}

pub(super) fn progress(trip: &Trip) -> f32 {
    (trip.departure..trip.arrival)
        .filter_map(|t| {
            let (p, a) = trip.sample(t);
            (a > 0.0).then_some(p.distance(trip.source))
        })
        .fold(0.0, f32::max)
}

fn follow_gutter(capture: &Capture, trip: &Trip, speed: f32) -> Option<Route> {
    let begin = trip.departure;
    let end = trip.arrival;
    let last = capture.pose(trip.author, end - 1)?.point;
    let previous = capture.pose(trip.author, end.saturating_sub(2))?.point;
    let extrapolated = last + (last - previous);
    let correction = trip.destination - extrapolated;
    let mut points = Vec::new();
    for tick in begin..=end {
        let base = if tick == end {
            extrapolated
        } else {
            capture.pose(trip.author, tick)?.point
        };
        let u = tick_time(tick - begin) / tick_time(end - begin);
        points.push(base + correction * (u * u * (3.0 - 2.0 * u)));
    }
    let mut velocity = (capture.pose(trip.author, begin)?.point
        - capture.pose(trip.author, begin.saturating_sub(1))?.point)
        * 60.0;
    for pair in points.windows(2) {
        let next = (pair[1] - pair[0]) * 60.0;
        if next.length() > speed + 0.001
            || (next - velocity).length() * 60.0 > speed * 5.0 + 0.01
        {
            return None;
        }
        velocity = next;
    }
    Some(Route {
        legs: Vec::new(),
        duration: tick_time(end - begin),
        track: points,
    })
}

fn swept_content(capture: &Capture, begin: u64, end: u64, radius: f32) -> Vec<Rect> {
    let mut bounds = std::collections::BTreeMap::<usize, Rect>::new();
    for tick in begin..=end {
        let Some(row) = capture
            .content
            .get(usize::try_from(tick).expect("bounded tick"))
        else {
            continue;
        };
        for &(repository, rect) in row {
            bounds
                .entry(repository)
                .and_modify(|bound| {
                    bound.min = bound.min.min(rect.min);
                    bound.max = bound.max.max(rect.max);
                })
                .or_insert(rect);
        }
    }
    bounds.into_values().map(|r| r.inflate(radius)).collect()
}

struct Search<'a> {
    capture: &'a Capture,
    reservations: &'a [Trip],
    speed: f32,
    router: &'a mut Router,
    wait_clearance: bool,
}

fn better(best: Trip, candidate: Trip) -> Trip {
    if best.route.is_empty()
        || (!candidate.route.is_empty() && progress(&candidate) > progress(&best))
    {
        candidate
    } else {
        best
    }
}

impl Search<'_> {
    fn geometry(&mut self, template: &Trip, epoch: u64, bodies: &[Disc]) -> Trip {
        let mut candidate = template.clone();
        let radius = (candidate.departure..=candidate.arrival)
            .map(|tick| self.capture.radius(tick))
            .fold(0.0_f32, f32::max);
        let tiles = self.capture.tiles(epoch, radius + 0.0002);
        let content =
            swept_content(self.capture, candidate.departure, candidate.arrival, radius);
        let boundaries = self.capture.boundaries(epoch);
        match self.router.cost_route(
            candidate.source,
            candidate.destination,
            &tiles,
            bodies,
            Costs {
                content: &content,
                tiles: &boundaries,
                radius,
            },
        ) {
            Ok(points) => {
                candidate.failure = None;
                candidate.route = Route::new(&points, self.speed);
                reserve(self.capture, self.reservations, &mut candidate);
            }
            Err(e) => {
                candidate.failure = Some(e.name());
                candidate.proof = Some(
                    serde_json::json!({"epoch":epoch,"begin":candidate.departure,"source":candidate.source.to_array(),"target":candidate.destination.to_array(),"radius":radius,"tiles":tiles.iter().map(|r|[r.min.to_array(),r.max.to_array()]).collect::<Vec<_>>(),"bodies":bodies.iter().map(|b|serde_json::json!({"point":b.point.to_array(),"radius":b.radius})).collect::<Vec<_>>()}),
                );
            }
        }
        candidate
    }

    fn run(&mut self, template: &Trip) -> Trip {
        let mut best = template.clone();
        let mut starts = vec![
            template.free,
            template.free.midpoint(template.arrival - 1),
            template.arrival - 1,
        ];
        if self.wait_clearance {
            let mut blocked = false;
            for tick in template.free..template.arrival {
                let point = self
                    .capture
                    .pose(template.author, tick)
                    .expect("existing contributor")
                    .point;
                if occupied(
                    self.capture,
                    self.reservations,
                    template.author,
                    tick,
                    point,
                )
                .is_some()
                {
                    blocked = true;
                } else if blocked {
                    starts.push(tick);
                    break;
                }
            }
        }
        starts.sort_unstable();
        starts.dedup();
        for begin in starts {
            let mut at = template.clone();
            at.departure = begin;
            at.source = self
                .capture
                .pose(at.author, begin)
                .expect("existing contributor")
                .point;
            let mut following = at.clone();
            if let Some(route) = follow_gutter(self.capture, &following, self.speed) {
                following.route = route;
                following.failure = None;
                reserve(self.capture, self.reservations, &mut following);
                if !following.fade {
                    return following;
                }
                best = better(best, following);
            }
            let bodies = stationary(
                self.capture,
                self.reservations,
                at.author,
                begin,
                at.arrival - 1,
            );
            for epoch in [begin, begin.midpoint(at.arrival), at.arrival] {
                let candidate = self.geometry(&at, epoch, &bodies);
                if !candidate.fade {
                    return candidate;
                }
                best = better(best, candidate);
            }
        }
        best
    }
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
        let mut free = (prior.tick + 1).min(service.tick - 1);
        if keep_idle {
            free = free
                .max(capture.services[commit - 1].tick)
                .min(service.tick - 1);
        }
        let source = capture
            .pose(service.author, free)
            .expect("existing contributor")
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
            let reservations = if keep_idle {
                &result[result.partition_point(|t| t.arrival <= free)..]
            } else {
                &result[..]
            };
            best = Search {
                capture,
                reservations,
                speed,
                router: &mut router,
                wait_clearance: keep_idle,
            }
            .run(&best);
        }
        if best.route.is_empty() {
            best.departure = service.tick.saturating_sub(9).max(free);
            best.source = capture
                .pose(service.author, best.departure)
                .expect("existing contributor")
                .point;
            best.fade_ticks = fade_clearance(
                capture,
                best.departure,
                best.source,
                prior.radius.max(service.radius),
                service.author,
            )
            .min(best.arrival - best.departure);
            for tick in best.departure..best.arrival {
                let (p, a) = best.sample(tick);
                if a > 0.0 && occupied(capture, &result, best.author, tick, p).is_some()
                {
                    best.cutoff = Some(tick);
                    break;
                }
            }
        }
        result.push(best);
    }
    (result, router.epochs)
}

#[cfg(test)]
mod tests {
    use super::super::{Service, slots};
    use super::*;

    #[test]
    fn walking_bots_use_the_same_reserved_routes_as_humans() {
        let mut capture = fixture();
        let (humans, _) = trips(&capture, 240.0, crate::model::BotTravel::Walk, false);
        for service in &mut capture.services {
            service.bot = true;
        }
        let (bots, _) = trips(&capture, 240.0, crate::model::BotTravel::Walk, false);
        let (fades, _) = trips(&capture, 240.0, crate::model::BotTravel::Fade, false);
        assert!(!bots.is_empty());
        assert!(bots.iter().all(|t| !t.fade));
        assert!(fades.iter().all(|t| t.fade && t.route.is_empty()));
        for (human, bot) in humans.iter().zip(bots) {
            assert_eq!(human.route.points(), bot.route.points());
            assert_eq!(human.departure, bot.departure);
            assert_eq!(human.arrival, bot.arrival);
        }
    }

    fn fixture() -> Capture {
        Capture {
            services: [(0, 0, 10), (1, 1, 20), (0, 2, 100), (1, 3, 120)]
                .into_iter()
                .map(|(author, repository, tick)| Service {
                    author,
                    repository,
                    tick,
                    point: Vec2::new(
                        if tick < 30 { -100.0 } else { 100.0 },
                        if author == 0 { -60.0 } else { 60.0 },
                    ),
                    radius: 7.5,
                    bot: false,
                    dock: slots::Dock::middle_top(),
                })
                .collect(),
            poses: (0..2)
                .map(|author| {
                    (0..=120)
                        .map(|tick| Pose {
                            tick,
                            point: Vec2::new(
                                if tick < (if author == 0 { 100 } else { 120 }) {
                                    -100.0
                                } else {
                                    100.0
                                },
                                if author == 0 { -60.0 } else { 60.0 },
                            ),
                            visible: true,
                        })
                        .collect()
                })
                .collect(),
            tiles: vec![vec![]; 121],
            radii: vec![7.5; 121],
            content: Vec::new(),
        }
    }

    #[test]
    fn disjoint_trips_use_their_own_service_slack_concurrently() {
        let capture = fixture();
        let (trips, _) =
            super::trips(&capture, 240.0, crate::model::BotTravel::Fade, false);
        assert_eq!(trips.len(), 2);
        assert_eq!(trips[0].departure, 11);
        assert_eq!(trips[1].departure, 21);
        assert!(trips.iter().all(|t| !t.fade));
        assert!(trips[1].departure < trips[0].arrival);
    }

    #[test]
    fn uncapped_departure_uses_a_cleared_window_between_coarse_candidates() {
        let source = Vec2::new(-100.0, -60.0);
        let target = Vec2::new(100.0, -60.0);
        let capture = Capture {
            services: [
                (0, 0, 10, source),
                (1, 1, 600, source),
                (0, 2, 1200, target),
            ]
            .into_iter()
            .map(|(author, repository, tick, point)| super::super::Service {
                author,
                repository,
                tick,
                point,
                radius: 7.5,
                bot: false,
                dock: slots::Dock::middle_top(),
            })
            .collect(),
            poses: (0..2)
                .map(|author| {
                    (0..=1200)
                        .map(|tick| Pose {
                            tick,
                            point: if author == 0 && tick == 1200 {
                                target
                            } else {
                                source
                            },
                            visible: if author == 0 {
                                tick < 400 || tick == 1200
                            } else {
                                (600..990).contains(&tick)
                            },
                        })
                        .collect()
                })
                .collect(),
            tiles: vec![vec![]; 1201],
            radii: vec![7.5; 1201],
            content: Vec::new(),
        };
        let (walking, _) = trips(&capture, 240.0, crate::model::BotTravel::Walk, true);
        assert_eq!(walking.len(), 1);
        assert!(!walking[0].fade);
        assert!(walking[0].departure >= 990);
        let mut template = walking[0].clone();
        template.departure = template.free;
        template.route = Route::default();
        template.fade = true;
        let mut router = Router::default();
        assert!(
            Search {
                capture: &capture,
                reservations: &[],
                speed: 240.0,
                router: &mut router,
                wait_clearance: false
            }
            .run(&template)
            .fade
        );
    }

    #[test]
    fn uncapped_walks_do_not_turn_into_idle_reentries() {
        let mut capture = fixture();
        capture.services.retain(|s| s.author == 0);
        capture.services[1].tick = 600;
        capture.poses.truncate(1);
        capture.poses[0] = (0..=600)
            .map(|tick| Pose {
                tick,
                point: if tick < 600 {
                    Vec2::new(-100.0, -60.0)
                } else {
                    Vec2::new(100.0, -60.0)
                },
                visible: true,
            })
            .collect();
        capture.tiles = vec![vec![]; 601];
        capture.radii = vec![7.5; 601];
        for service in &mut capture.services {
            service.bot = true;
        }
        let (idle, _) = trips(&capture, 240.0, crate::model::BotTravel::Walk, false);
        assert!(idle.is_empty());
        let (walking, _) = trips(&capture, 240.0, crate::model::BotTravel::Walk, true);
        assert_eq!(walking.len(), 1);
        assert!(!walking[0].fade);
        assert_eq!(walking[0].sample(500).1, 1.0);
    }

    #[test]
    fn crossing_routes_wait_without_overlapping() {
        let capture = fixture();
        let (mut trips, _) =
            super::trips(&capture, 240.0, crate::model::BotTravel::Fade, false);
        trips[0].source = Vec2::new(-100.0, 0.0);
        trips[0].destination = Vec2::new(100.0, 0.0);
        trips[0].departure = 0;
        trips[0].arrival = 120;
        trips[0].route = Route::new(&[trips[0].source, trips[0].destination], 240.0);
        trips[1].source = Vec2::new(0.0, -100.0);
        trips[1].destination = Vec2::new(0.0, 100.0);
        trips[1].departure = 0;
        trips[1].arrival = 120;
        trips[1].route = Route::new(&[trips[1].source, trips[1].destination], 240.0);
        let (first, second) = trips.split_at_mut(1);
        reserve(&capture, first, &mut second[0]);
        assert!(!second[0].fade);
        assert!(second[0].route.legs[0].start > 0.0);
        for tick in 0..120 {
            assert!(first[0].sample(tick).0.distance(second[0].sample(tick).0) >= 15.0);
        }
    }

    #[test]
    fn moving_neighbours_do_not_become_static_walls() {
        let mut capture = fixture();
        for pose in &mut capture.poses[1] {
            pose.point = Vec2::new(0.0, tick_time(pose.tick) * 80.0 - 80.0);
        }
        assert!(stationary(&capture, &[], 0, 0, 120).is_empty());
    }

    #[test]
    fn a_moving_gutter_is_followed_with_bounded_acceleration() {
        let mut capture = fixture();
        capture.poses.truncate(1);
        for pose in &mut capture.poses[0] {
            pose.point = Vec2::new(tick_time(pose.tick) * 0.6, 0.0);
        }
        let (mut trips, _) =
            super::trips(&fixture(), 240.0, crate::model::BotTravel::Fade, false);
        let trip = &mut trips[0];
        trip.departure = 1;
        trip.arrival = 120;
        trip.destination = Vec2::new(1.2, 30.0);
        let route = follow_gutter(&capture, trip, 240.0).unwrap();
        assert!(route.point_at(119).unwrap().distance(trip.destination) < 0.001);
        assert_ne!(route.track, [] as [Vec2; 0]);
    }
}
