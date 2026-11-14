use bevy::math::Rect;

#[derive(Clone, Copy)]
pub(super) struct Candidate {
    pub angle: f32,
    pub rect: Rect,
    pub text_pairs: usize,
    pub text_area: f32,
    pub soft: f32,
}

pub(super) struct Assignment {
    pub choices: Vec<usize>,
    pub exhausted: bool,
    pub clear: bool,
}

pub(super) const ANGULAR_SPEED: f32 = std::f32::consts::PI / 6.0;
pub(super) const ANGULAR_ACCELERATION: f32 = std::f32::consts::PI / 3.0;
const GAIN: f32 = 0.25;
pub(super) const DWELL_TICKS: u64 = 45;
const REVERSAL_TICKS: u64 = 180;

#[cfg(test)]
pub(super) fn advance(angle: f32, velocity: f32, target: f32, dt: f32) -> (f32, f32) {
    steer(angle, velocity, target, dt)
}

pub(super) fn steer(angle: f32, velocity: f32, target: f32, dt: f32) -> (f32, f32) {
    let error = difference(target, angle);
    let desired = error.signum()
        * (2.0 * ANGULAR_ACCELERATION * error.abs())
            .sqrt()
            .min(ANGULAR_SPEED)
            .min(error.abs() * 2.0);
    let velocity = desired.clamp(
        velocity - ANGULAR_ACCELERATION * dt,
        velocity + ANGULAR_ACCELERATION * dt,
    );
    (angle + velocity * dt, velocity)
}

fn difference(target: f32, angle: f32) -> f32 {
    (target - angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
        - std::f32::consts::PI
}

pub(super) fn move_name(
    previous: &super::super::names::Placement,
    proposed: f32,
    gain: f32,
    tick: u64,
    dt: f32,
) -> (f32, f32, super::super::names::Motion) {
    let mut motion = previous.motion.clone();
    let settled = difference(motion.goal, previous.angle).abs() < 0.001
        && previous.angular_velocity.abs() < 0.001;
    if settled && gain >= GAIN {
        let sector = (0..32_u8)
            .find(|i| {
                proposed.rem_euclid(std::f32::consts::TAU)
                    < f32::from(i + 1) * std::f32::consts::TAU / 32.0
            })
            .unwrap_or(31);
        let since = match motion.pending {
            Some((old, since)) if old == sector => since,
            _ => tick,
        };
        motion.pending = Some((sector, since));
        let direction = if difference(proposed, previous.angle) > 0.0 {
            1
        } else {
            -1
        };
        let locked = motion.direction != 0
            && direction != motion.direction
            && motion
                .reversal
                .is_some_and(|last| tick - last < REVERSAL_TICKS);
        if tick - since >= DWELL_TICKS && !locked {
            motion.goal = proposed;
            motion.pending = None;
        }
    } else {
        motion.pending = None;
    }
    let (angle, velocity) =
        steer(previous.angle, previous.angular_velocity, motion.goal, dt);
    let delta = angle - previous.angle;
    if delta.abs() > 1e-7 {
        let direction = if delta > 0.0 { 1 } else { -1 };
        if motion.direction != 0 && direction != motion.direction {
            if motion
                .reversal
                .is_some_and(|last| tick - last < REVERSAL_TICKS)
            {
                return (previous.angle, 0.0, motion);
            }
            motion.reversal = Some(tick);
        }
        motion.direction = direction;
    }
    (angle, velocity, motion)
}

pub(in crate::model) fn tangent_rect(
    body: bevy::math::Vec2,
    size: bevy::math::Vec2,
    distance: f32,
    angle: f32,
) -> Rect {
    let direction = bevy::math::Vec2::from_angle(angle);
    let axis = direction.abs();
    let half = size * 0.5;
    let tx = (half.x + distance) / axis.x.max(1e-8);
    let ty = (half.y + distance) / axis.y.max(1e-8);
    let radius = if tx * axis.y <= half.y {
        tx
    } else if ty * axis.x <= half.x {
        ty
    } else {
        let b = axis.dot(half);
        b + (b * b - half.length_squared() + distance * distance)
            .max(0.0)
            .sqrt()
    };
    Rect::from_center_size(body + direction * radius, size)
}
pub(super) fn forecast_clip(
    mut angle: f32,
    mut velocity: f32,
    target: f32,
    size: bevy::math::Vec2,
    distance: f32,
    future: &[bevy::math::Vec2],
) -> f32 {
    let mut maximum = 0.0_f32;
    for &body in future {
        (angle, velocity) =
            steer(angle, velocity, target, 4.0 * super::super::tick_seconds());
        let rect = tangent_rect(body, size, distance, angle);
        let outside = crate::model::caption::outside(rect, 16.0);
        maximum = maximum.max(outside);
    }
    maximum
}
fn overlap(a: Rect, b: Rect) -> f32 {
    a.intersect(b)
        .size()
        .max(bevy::math::Vec2::ZERO)
        .element_product()
}

fn interacting(a: Candidate, b: Candidate) -> bool {
    overlap(a.rect, b.rect) > 0.0
}

/// The rectangle enclosing every candidate, so names whose reaches are disjoint
/// have no overlapping pair of candidates.
fn reach(domain: &[Candidate]) -> Rect {
    domain
        .iter()
        .map(|candidate| candidate.rect)
        .reduce(|a, b| a.union(b))
        .unwrap_or_default()
}

pub(super) fn solve(domains: &[Vec<Candidate>]) -> Assignment {
    let mut result = Assignment {
        choices: vec![0; domains.len()],
        exhausted: false,
        clear: true,
    };
    let mut seen = vec![false; domains.len()];
    let reaches: Vec<_> = domains.iter().map(|domain| reach(domain)).collect();
    for root in 0..domains.len() {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        let mut component = vec![root];
        let mut cursor = 0;
        while cursor < component.len() {
            let node = component[cursor];
            for other in 0..domains.len() {
                if !seen[other]
                    && overlap(reaches[node], reaches[other]) > 0.0
                    && domains[node]
                        .iter()
                        .any(|a| domains[other].iter().any(|b| interacting(*a, *b)))
                {
                    seen[other] = true;
                    component.push(other);
                }
            }
            cursor += 1;
        }
        component.sort_by_key(|&node| (domains[node].len(), node));
        let neighbours: Vec<Vec<_>> = component
            .iter()
            .enumerate()
            .map(|(depth, &node)| {
                (0..depth)
                    .filter(|&earlier| {
                        overlap(reaches[node], reaches[component[earlier]]) > 0.0
                    })
                    .collect()
            })
            .collect();
        let mut search = Search {
            domains,
            component: &component,
            neighbours: &neighbours,
            selected: Vec::new(),
            best: vec![0; component.len()],
            score: (usize::MAX, f32::INFINITY, f32::INFINITY),
            remaining: 4096,
            exhausted: false,
        };
        let clear = search.visit((0, 0.0, 0.0));
        for (&node, &choice) in component.iter().zip(&search.best) {
            result.choices[node] = choice;
        }
        result.exhausted |= search.exhausted && !clear;
        result.clear &= clear;
    }
    result
}

struct Search<'a> {
    domains: &'a [Vec<Candidate>],
    component: &'a [usize],
    /// For each depth, the earlier depths whose names can overlap this one.
    neighbours: &'a [Vec<usize>],
    selected: Vec<usize>,
    best: Vec<usize>,
    score: (usize, f32, f32),
    remaining: usize,
    exhausted: bool,
}

impl Search<'_> {
    fn visit(&mut self, score: (usize, f32, f32)) -> bool {
        if score.0 > self.score.0 || (score.0 == self.score.0 && score.1 > self.score.1)
        {
            return false;
        }
        let depth = self.selected.len();
        if depth == self.component.len() {
            if score.0 < self.score.0
                || score
                    .1
                    .total_cmp(&self.score.1)
                    .then(score.2.total_cmp(&self.score.2))
                    .is_lt()
            {
                self.score = score;
                self.best.clone_from(&self.selected);
            }
            return score.0 == 0;
        }
        if self.remaining == 0 {
            self.exhausted = true;
            return false;
        }
        self.remaining -= 1;
        let node = self.component[depth];
        for choice in 0..self.domains[node].len() {
            let candidate = self.domains[node][choice];
            let mut next = (
                score.0 + candidate.text_pairs,
                score.1 + candidate.text_area,
                score.2 + candidate.soft,
            );
            for &previous in &self.neighbours[depth] {
                let area = overlap(
                    candidate.rect,
                    self.domains[self.component[previous]][self.selected[previous]]
                        .rect,
                );
                next.0 += usize::from(area > 0.0);
                next.1 += area;
            }
            self.selected.push(choice);
            let clear = self.visit(next);
            self.selected.pop();
            if clear {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec2;

    fn candidate(x: f32) -> Candidate {
        Candidate {
            angle: 0.0,
            rect: Rect::from_center_size(Vec2::new(x, 0.0), Vec2::splat(2.0)),
            text_pairs: 0,
            text_area: 0.0,
            soft: 0.0,
        }
    }

    #[test]
    fn joint_assignment_escapes_a_greedy_dead_end() {
        let domains = vec![vec![candidate(0.0), candidate(4.0)], vec![candidate(0.0)]];
        let result = solve(&domains);
        assert_eq!(result.choices, [1, 0]);
        assert!(!result.exhausted);
    }

    #[test]
    fn three_interacting_labels_use_distinct_positions() {
        let options = vec![candidate(0.0), candidate(4.0), candidate(8.0)];
        let result = solve(&[options.clone(), options.clone(), options]);
        assert_eq!(result.choices, [0, 1, 2]);
        assert!(!result.exhausted);
    }

    #[test]
    fn static_text_collisions_outrank_all_soft_costs() {
        let mut blocked = candidate(0.0);
        blocked.text_pairs = 1;
        blocked.text_area = 4.0;
        let mut clear = candidate(4.0);
        clear.soft = 1e20;
        assert_eq!(solve(&[vec![blocked, clear]]).choices, [1]);
    }

    #[test]
    fn rotation_integrates_without_jumps_or_velocity_resets() {
        let mut angle = 0.0;
        let mut velocity = 0.0;
        for tick in 0..90 {
            let target = if tick < 15 { std::f32::consts::PI } else { 0.0 };
            let (next, speed) = steer(angle, velocity, target, 1.0 / 60.0);
            assert!((next - angle).abs() <= ANGULAR_SPEED / 60.0 + 1e-6);
            assert!((speed - velocity).abs() <= ANGULAR_ACCELERATION / 60.0 + 1e-6);
            angle = next;
            velocity = speed;
        }
    }

    #[test]
    fn impossible_assignment_reports_the_least_overlap() {
        let result = solve(&[
            vec![candidate(0.0)],
            vec![candidate(0.0)],
            vec![candidate(8.0)],
        ]);
        assert_eq!(result.choices, [0, 0, 0]);
        assert!(!result.exhausted);
    }
    #[test]
    fn dense_impossible_search_stops_at_its_budget() {
        let domains = vec![vec![candidate(0.0); 8]; 16];
        let result = solve(&domains);
        assert!(result.exhausted);
        assert_eq!(result.choices.len(), 16);
    }
    #[test]
    fn rotation_is_tangent_and_has_bounded_speed_and_acceleration() {
        let mut angle = 0.0;
        let mut velocity = 0.0;
        for _ in 0..180 {
            let (next, speed) =
                advance(angle, velocity, std::f32::consts::PI, 1.0 / 60.0);
            assert!((next - angle).abs() <= ANGULAR_SPEED / 60.0 + 1e-6);
            assert!((speed - velocity).abs() <= ANGULAR_ACCELERATION / 60.0 + 1e-6);
            let rect =
                tangent_rect(Vec2::ZERO, Vec2::new(240.0, 24.0), 15.333_333, next);
            assert!(
                (Vec2::ZERO.distance(Vec2::ZERO.clamp(rect.min, rect.max))
                    - 15.333_333)
                    .abs()
                    < 0.0001
            );
            angle = next;
            velocity = speed;
        }
    }
    #[test]
    fn forecast_rejects_turning_into_an_outer_frame_edge() {
        let future = vec![Vec2::new(626.0, 0.0); 30];
        let clear = forecast_clip(
            std::f32::consts::PI,
            0.0,
            std::f32::consts::PI,
            Vec2::new(240.0, 24.0),
            15.333_333,
            &future,
        );
        let clipped = forecast_clip(
            std::f32::consts::PI,
            0.0,
            0.0,
            Vec2::new(240.0, 24.0),
            15.333_333,
            &future,
        );
        assert!(clear <= 0.001);
        assert!(clipped > 100.0);
    }

    #[test]
    fn caption_buffer_rejects_an_attachment_that_fits_the_window() {
        let body = Vec2::new(0.0, crate::model::caption::BOTTOM + 16.0);
        let size = Vec2::new(236.0, 24.0);
        let down = tangent_rect(body, size, 15.333_333, -0.1);
        let up = tangent_rect(body, size, 15.333_333, std::f32::consts::FRAC_PI_2);
        assert!(down.min.y > -359.5);
        assert!(crate::model::caption::outside(down, 0.5) > 0.0);
        assert_eq!(crate::model::caption::outside(up, 0.5), 0.0);
    }
}
