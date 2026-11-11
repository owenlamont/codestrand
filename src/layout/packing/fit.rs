use bevy::math::{Vec2, Vec3};

use super::Tile;
use crate::{layout::outline, model::framing};

#[derive(Clone, Copy, Debug)]
pub(super) struct Fit {
    pub origin: Vec2,
    pub scale: f32,
    pub fill: f32,
}

fn planes(tile: Tile, points: &[Vec2], height: f32) -> [(Vec3, f32); 8] {
    let corners = tile.ground();
    let (low, high) = outline::bounds(&corners);
    let tangent = (framing::FOV * 0.5).tan();
    std::array::from_fn(|i| {
        let axis = i % 2;
        let sign = if i % 4 < 2 { -1.0 } else { 1.0 };
        let mut normal = Vec2::ZERO;
        normal[axis] = sign;
        let (bound, height_support) = if i < 4 {
            (sign * if i < 2 { low[axis] } else { high[axis] }, 0.0)
        } else {
            let limit = if i < 6 {
                tile.low[axis]
            } else {
                tile.high[axis]
            } * framing::GROUND_FACTOR;
            let slope = tangent
                * if axis == 0 {
                    framing::VIEWPORT.x / framing::VIEWPORT.y
                } else {
                    1.0
                };
            if axis == 1 {
                normal.y = -sign;
            }
            (
                sign * limit * slope * framing::DISTANCE,
                (sign * limit * slope * height).max(0.0),
            )
        };
        let support = points
            .iter()
            .map(|p| normal.dot(*p))
            .fold(f32::NEG_INFINITY, f32::max)
            + height_support;
        (Vec3::new(normal.x, normal.y, support), bound)
    })
}

fn finish(tile: Tile, points: &[Vec2], origin: Vec2, scale: f32) -> Fit {
    let projected: Vec<_> = points
        .iter()
        .map(|p| framing::world_to_field(*p * scale + origin))
        .collect();
    Fit {
        origin,
        scale,
        fill: outline::area(&projected) * 0.25 / tile.area().max(1e-12),
    }
}

pub(super) fn anchored(tile: Tile, points: &[Vec2], height: f32, anchor: Vec2) -> Fit {
    let (low, high) = outline::bounds(points);
    let centre = (low + high) * 0.5;
    let local: Vec<_> = points.iter().map(|p| *p - centre).collect();
    let origin = framing::field_to_world(tile.low + (tile.high - tile.low) * anchor);
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
    let triples = (0..8)
        .flat_map(|i| (i + 1..8).flat_map(move |j| (j + 1..8).map(move |k| [i, j, k])));
    for [i, j, k] in triples {
        let rows = [constraints[i], constraints[j], constraints[k]];
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
    fn scaled_height_and_ground_footprint_both_fit_the_tile() {
        let tile = Tile {
            low: Vec2::new(-0.9, -0.8),
            high: Vec2::new(-0.3, -0.2),
        };
        let points = outline::rectangle(Vec2::splat(-0.25), Vec2::splat(0.25));
        let height = 0.5;
        let fit = anchored(tile, &points, height, Vec2::splat(0.5));
        assert!(fit.scale > 1.0);
        for p in points {
            let world = p * fit.scale + fit.origin;
            for projected in [
                framing::world_to_field(world),
                framing::project_field_at(world, 0.0),
                framing::project_field_at(world, height * fit.scale),
            ] {
                assert!(projected.cmpge(tile.low - Vec2::splat(1e-5)).all());
                assert!(projected.cmple(tile.high + Vec2::splat(1e-5)).all());
            }
        }
    }

    #[test]
    fn thin_birth_tiles_use_their_world_projection() {
        let tile = Tile {
            low: Vec2::new(0.8, 0.8),
            high: Vec2::new(0.8001, 0.8001),
        };
        let points = outline::rectangle(Vec2::splat(-1.0), Vec2::splat(1.0));
        let fit = anchored(tile, &points, 0.0, Vec2::splat(0.5));
        assert!(fit.scale > 0.0);
        for p in points {
            let projected = framing::project_field_at(p * fit.scale + fit.origin, 0.0);
            assert!(projected.cmpge(tile.low - Vec2::splat(1e-6)).all());
            assert!(projected.cmple(tile.high + Vec2::splat(1e-6)).all());
        }
    }

    #[test]
    fn fitted_footprints_stay_in_world_rectangle() {
        let tile = Tile::viewport();
        let points: Vec<_> = (0..8_u16)
            .map(|i| {
                Vec2::from_angle(f32::from(i) * std::f32::consts::FRAC_PI_4) * 20.0
            })
            .collect();
        let fit = best(tile, &points, 0.105);
        assert!(
            fit.fill + 1e-5 >= anchored(tile, &points, 0.105, Vec2::splat(0.5)).fill
        );
        for p in points {
            let screen = framing::world_to_field(p * fit.scale + fit.origin);
            assert!(screen.cmpge(tile.low - Vec2::splat(1e-5)).all());
            assert!(screen.cmple(tile.high + Vec2::splat(1e-5)).all());
        }
    }
}
