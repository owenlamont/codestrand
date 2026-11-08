use bevy::math::{Vec2, Vec3};

use super::Tile;
use crate::{layout::outline, model::framing};

#[derive(Clone, Copy, Debug)]
pub(super) struct Fit {
    pub origin: Vec2,
    pub scale: f32,
    pub fill: f32,
    pub anchor: Vec2,
}

fn planes(tile: Tile, points: &[Vec2], height: f32) -> [(Vec3, f32); 4] {
    let direction = crate::model::view_direction();
    let vertical = direction.y;
    let horizontal = direction.x.hypot(direction.z);
    let tangent = (framing::FOV * 0.5).tan();
    let x_slope = tangent * framing::VIEWPORT.x / framing::VIEWPORT.y;
    std::array::from_fn(|i| {
        let axis = i % 2;
        let sign = if i < 2 { -1.0 } else { 1.0 };
        let limit = if i < 2 {
            tile.low[axis]
        } else {
            tile.high[axis]
        };
        let (normal, y) = if axis == 0 {
            (
                Vec2::new(sign, sign * limit * x_slope * horizontal),
                sign * limit * x_slope * vertical,
            )
        } else {
            (
                Vec2::new(0.0, sign * (-vertical + limit * tangent * horizontal)),
                sign * (horizontal + limit * tangent * vertical),
            )
        };
        let support = points
            .iter()
            .map(|p| normal.dot(*p))
            .fold(f32::NEG_INFINITY, f32::max)
            + (y * height).max(0.0);
        (
            Vec3::new(normal.x, normal.y, support),
            sign * limit
                * if axis == 0 { x_slope } else { tangent }
                * framing::DISTANCE,
        )
    })
}

fn finish(tile: Tile, points: &[Vec2], origin: Vec2, scale: f32) -> Fit {
    let projected: Vec<_> = points
        .iter()
        .map(|p| framing::project_ground(*p * scale + origin))
        .collect();
    let (low, high) = outline::bounds(points);
    let centre = framing::project_ground((low + high) * (0.5 * scale) + origin);
    Fit {
        origin,
        scale,
        fill: outline::area(&projected) * 0.25 / tile.area().max(1e-12),
        anchor: ((centre - tile.low) / (tile.high - tile.low).max(Vec2::splat(1e-9)))
            .clamp(Vec2::ZERO, Vec2::ONE),
    }
}

pub(super) fn anchored(tile: Tile, points: &[Vec2], height: f32, anchor: Vec2) -> Fit {
    let (low, high) = outline::bounds(points);
    let centre = (low + high) * 0.5;
    let local: Vec<_> = points.iter().map(|p| *p - centre).collect();
    let origin = framing::unproject(tile.low + (tile.high - tile.low) * anchor);
    let scale = planes(tile, &local, height)
        .into_iter()
        .filter(|(p, _)| p.z > 1e-9)
        .map(|(p, bound)| ((bound - p.truncate().dot(origin)) / p.z).max(0.0))
        .fold(f32::INFINITY, f32::min)
        * 0.999;
    finish(tile, points, origin - centre * scale, scale)
}

pub(super) fn best(tile: Tile, points: &[Vec2], height: f32) -> Fit {
    let (low, high) = outline::bounds(points);
    let centre = (low + high) * 0.5;
    let local: Vec<_> = points.iter().map(|p| *p - centre).collect();
    let constraints = planes(tile, &local, height);
    let mut best = anchored(tile, points, height, Vec2::splat(0.5));
    for skip in 0..4 {
        let mut rows = [(Vec3::ZERO, 0.0); 3];
        let mut target = 0;
        for (i, &plane) in constraints.iter().enumerate() {
            if i != skip {
                rows[target] = plane;
                target += 1;
            }
        }
        let [
            (first, first_bound),
            (second, second_bound),
            (third, third_bound),
        ] = rows;
        let determinant = first.dot(second.cross(third));
        if determinant.abs() < 1e-9 {
            continue;
        }
        let solution = (second.cross(third) * first_bound
            + third.cross(first) * second_bound
            + first.cross(second) * third_bound)
            / determinant;
        if solution.z <= 0.0
            || !solution.is_finite()
            || constraints
                .iter()
                .any(|(p, limit)| p.dot(solution) > limit + 0.001)
        {
            continue;
        }
        let scale = solution.z * 0.999;
        let trial = finish(tile, points, solution.truncate() - centre * scale, scale);
        if trial.fill > best.fill {
            best = trial;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitted_footprints_stay_in_the_trapezoid_and_use_free_centre() {
        let tile = Tile::viewport();
        let points: Vec<_> = (0..8_u16)
            .map(|i| {
                Vec2::from_angle(f32::from(i) * std::f32::consts::FRAC_PI_4) * 20.0
            })
            .collect();
        let fit = best(tile, &points, 0.105);
        assert!(
            fit.fill > anchored(tile, &points, 0.105, Vec2::splat(0.5)).fill + 0.05
        );
        for p in points {
            let screen = framing::project_ground(p * fit.scale + fit.origin);
            assert!(screen.cmpge(tile.low - Vec2::splat(1e-5)).all());
            assert!(screen.cmple(tile.high + Vec2::splat(1e-5)).all());
        }
    }
}
