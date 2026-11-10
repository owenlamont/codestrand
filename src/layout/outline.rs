use bevy::math::Vec2;

const S: f32 = std::f32::consts::FRAC_1_SQRT_2;
pub(crate) const NORMALS: [Vec2; 8] = [
    Vec2::X,
    Vec2::new(S, S),
    Vec2::Y,
    Vec2::new(-S, S),
    Vec2::NEG_X,
    Vec2::new(-S, -S),
    Vec2::NEG_Y,
    Vec2::new(S, -S),
];

pub(crate) fn include(supports: &mut [f32; 8], centre: Vec2, radius: f32) {
    for (support, normal) in supports.iter_mut().zip(NORMALS) {
        *support = support.max(centre.dot(normal) + radius);
    }
}

pub(crate) fn from_supports(supports: [f32; 8]) -> Vec<Vec2> {
    if !supports[0].is_finite() {
        return rectangle(Vec2::splat(-0.05), Vec2::splat(0.05));
    }
    let mut points = rectangle(
        Vec2::new(-supports[4], -supports[6]),
        Vec2::new(supports[0], supports[2]),
    );
    for i in [1, 3, 5, 7] {
        points = clip(&points, NORMALS[i], supports[i]);
    }
    points
}

pub(crate) fn rectangle(low: Vec2, high: Vec2) -> Vec<Vec2> {
    vec![
        low,
        Vec2::new(high.x, low.y),
        high,
        Vec2::new(low.x, high.y),
    ]
}

pub(crate) fn clip(points: &[Vec2], normal: Vec2, bound: f32) -> Vec<Vec2> {
    let mut result = Vec::with_capacity(points.len() + 1);
    let Some(&last) = points.last() else {
        return result;
    };
    let mut before = last;
    let mut distance = before.dot(normal) - bound;
    for &point in points {
        let next = point.dot(normal) - bound;
        if (distance > 0.0) != (next > 0.0) {
            result.push(before.lerp(point, distance / (distance - next)));
        }
        if next <= 0.0 {
            result.push(point);
        }
        before = point;
        distance = next;
    }
    result
}

pub(crate) fn area(points: &[Vec2]) -> f32 {
    let Some(&last) = points.last() else {
        return 0.0;
    };
    points
        .iter()
        .fold((last, 0.0_f32), |(before, sum), &p| {
            (p, sum + before.perp_dot(p))
        })
        .1
        .abs()
        * 0.5
}

pub(crate) fn bounds(points: &[Vec2]) -> (Vec2, Vec2) {
    points.iter().fold(
        (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)),
        |(low, high), &p| (low.min(p), high.max(p)),
    )
}
