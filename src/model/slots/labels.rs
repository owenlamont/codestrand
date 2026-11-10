use bevy::math::Rect;

#[derive(Clone, Copy)]
pub(super) struct Candidate {
    pub angle: f32,
    pub angular_velocity: f32,
    pub rect: Rect,
    pub goal: Rect,
    pub text_pairs: usize,
    pub text_area: f32,
    pub goal_text_pairs: usize,
    pub soft: f32,
}

pub(super) struct Assignment {
    pub choices: Vec<usize>,
    pub exhausted: bool,
    pub clear: bool,
}

pub(super) const ANGULAR_SPEED: f32 = std::f32::consts::PI;
pub(super) const ANGULAR_ACCELERATION: f32 = 4.0 * std::f32::consts::PI;
pub(super) const EMERGENCY_SPEED: f32 = 4.0 * std::f32::consts::PI;
pub(super) const EMERGENCY_ACCELERATION: f32 = 128.0 * std::f32::consts::PI;

#[cfg(test)]
pub(super) fn advance(angle: f32, velocity: f32, target: f32, dt: f32) -> (f32, f32) {
    steer(angle, velocity, target, dt, false)
}

pub(super) fn steer(
    angle: f32,
    velocity: f32,
    target: f32,
    dt: f32,
    emergency: bool,
) -> (f32, f32) {
    let emergency = emergency || velocity.abs() > ANGULAR_SPEED;
    let (speed, acceleration, gain) = if emergency {
        (EMERGENCY_SPEED, EMERGENCY_ACCELERATION, 30.0)
    } else {
        (ANGULAR_SPEED, ANGULAR_ACCELERATION, 6.0)
    };
    let error = (target - angle + std::f32::consts::PI)
        .rem_euclid(std::f32::consts::TAU)
        - std::f32::consts::PI;
    let desired = error.signum()
        * (2.0 * acceleration * error.abs())
            .sqrt()
            .min(speed - 0.001)
            .min(error.abs() * gain);
    let velocity = desired.clamp(
        velocity - (acceleration - 0.001) * dt,
        velocity + (acceleration - 0.001) * dt,
    );
    (angle + velocity * dt, velocity)
}

pub(super) fn tangent_rect(
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
    emergency: bool,
) -> f32 {
    let mut maximum = 0.0_f32;
    for &body in future {
        (angle, velocity) = steer(
            angle,
            velocity,
            target,
            4.0 * super::super::tick_seconds(),
            emergency,
        );
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
    overlap(a.rect, b.rect) > 0.0 || overlap(a.goal, b.goal) > 0.0
}

pub(super) fn solve(domains: &[Vec<Candidate>]) -> Assignment {
    let mut result = Assignment {
        choices: vec![0; domains.len()],
        exhausted: false,
        clear: true,
    };
    let mut seen = vec![false; domains.len()];
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
        let mut search = Search {
            domains,
            component: &component,
            selected: Vec::new(),
            best: vec![0; component.len()],
            score: (usize::MAX, f32::INFINITY, usize::MAX, f32::INFINITY),
            remaining: 4096,
            exhausted: false,
        };
        let clear = search.visit((0, 0.0, 0, 0.0));
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
    selected: Vec<usize>,
    best: Vec<usize>,
    score: (usize, f32, usize, f32),
    remaining: usize,
    exhausted: bool,
}

impl Search<'_> {
    fn visit(&mut self, score: (usize, f32, usize, f32)) -> bool {
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
                    .then(score.2.cmp(&self.score.2))
                    .then(score.3.total_cmp(&self.score.3))
                    .is_lt()
            {
                self.score = score;
                self.best.clone_from(&self.selected);
            }
            return score.0 == 0 && score.2 == 0;
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
                score.2 + candidate.goal_text_pairs,
                score.3 + candidate.soft,
            );
            for (previous, &selected) in self.selected.iter().enumerate() {
                let area = overlap(
                    candidate.rect,
                    self.domains[self.component[previous]][selected].rect,
                );
                next.0 += usize::from(area > 0.0);
                next.1 += area;
                next.2 += usize::from(
                    overlap(
                        candidate.goal,
                        self.domains[self.component[previous]][selected].goal,
                    ) > 0.0,
                );
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
            angular_velocity: 0.0,
            rect: Rect::from_center_size(Vec2::new(x, 0.0), Vec2::splat(2.0)),
            goal: Rect::from_center_size(Vec2::new(x, 0.0), Vec2::splat(2.0)),
            text_pairs: 0,
            text_area: 0.0,
            goal_text_pairs: 0,
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
    fn equal_next_poses_keep_separate_rotation_targets() {
        let first = candidate(0.0);
        let mut alternate = first;
        alternate.goal = candidate(4.0).rect;
        let mut second = candidate(8.0);
        second.goal = first.goal;
        let result = solve(&[vec![first, alternate], vec![second]]);
        assert_eq!(result.choices, [1, 0]);
        assert!(result.clear);
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
    fn emergency_rotation_integrates_without_jumps_or_velocity_resets() {
        let mut angle = 0.0;
        let mut velocity = 0.0;
        for tick in 0..90 {
            let target = if tick < 15 { std::f32::consts::PI } else { 0.0 };
            let (next, speed) = steer(angle, velocity, target, 1.0 / 60.0, true);
            assert!((next - angle).abs() <= EMERGENCY_SPEED / 60.0 + 1e-6);
            assert!((speed - velocity).abs() <= EMERGENCY_ACCELERATION / 60.0 + 1e-6);
            angle = next;
            velocity = speed;
        }
    }

    #[test]
    fn a_cleared_emergency_still_brakes_before_the_frame_edge() {
        let (angle, velocity) =
            steer(0.1, EMERGENCY_SPEED - 0.001, 0.0, 1.0 / 60.0, false);
        let rect = tangent_rect(
            Vec2::new(-624.0, 80.0),
            Vec2::new(236.76, 23.0),
            15.333_333,
            angle,
        );
        assert!(rect.min.x > -639.5);
        assert!(velocity < 2.0 * ANGULAR_SPEED);
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
            false,
        );
        let clipped = forecast_clip(
            std::f32::consts::PI,
            0.0,
            0.0,
            Vec2::new(240.0, 24.0),
            15.333_333,
            &future,
            false,
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
