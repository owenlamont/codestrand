use bevy::math::{Vec2, Vec3};

use super::{ground, view_direction};
use crate::layout::RepositoryRegion;

pub(crate) const FOV: f32 = std::f32::consts::FRAC_PI_4;
pub(crate) const VIEWPORT: Vec2 = Vec2::new(1280.0, 720.0);
pub(crate) const GLASS_BOTTOM: f32 = -0.1;
pub(crate) const GLASS_TOP: f32 = 1.1;
pub(crate) const LABEL_Y: f32 = 1.6;
pub(crate) const MARGIN: f32 = 24.0;
pub(crate) const DISTANCE: f32 = 60.0;

pub(crate) fn unproject(point: Vec2) -> Vec2 {
    unproject_at(point, 0.0)
}

pub(crate) fn unproject_at(point: Vec2, height: f32) -> Vec2 {
    let direction = view_direction();
    let horizontal = direction.x.hypot(direction.z);
    let tangent = (FOV * 0.5).tan();
    let z = (height * (horizontal + point.y * tangent * direction.y)
        - point.y * tangent * DISTANCE)
        / (direction.y - point.y * tangent * horizontal);
    let depth = DISTANCE - horizontal * z - direction.y * height;
    Vec2::new(point.x * tangent * (VIEWPORT.x / VIEWPORT.y) * depth, z)
}

pub(crate) fn project_ground(point: Vec2) -> Vec2 {
    let direction = view_direction();
    let depth = DISTANCE - direction.x.hypot(direction.z) * point.y;
    let tangent = (FOV * 0.5).tan();
    Vec2::new(
        point.x / (tangent * VIEWPORT.x / VIEWPORT.y),
        -direction.y * point.y / tangent,
    ) / depth
}

pub(super) fn label_position(region: &RepositoryRegion) -> Vec3 {
    if let Some(tile) = region.tile {
        let limit = crate::layout::packing::Tile::viewport();
        let padding = Vec2::splat(8.0) * 2.0 / VIEWPORT;
        let size = label_size(&region.label, 20.0) * 2.0 / VIEWPORT;
        let anchor = Vec2::new(tile.low.x + padding.x, tile.high.y - padding.y)
            .clamp(limit.low + Vec2::Y * size.y, limit.high - Vec2::X * size.x);
        return ground(crate::layout::world_coordinates(unproject_at(
            anchor, LABEL_Y,
        ))) + Vec3::Y * LABEL_Y;
    }
    let point = crate::layout::world_coordinates(Vec2::new(
        region.low.x.midpoint(region.high.x),
        region.high.y - 1.0,
    ));
    ground(point) + Vec3::Y * LABEL_Y
}

pub(crate) fn label_size(text: &str, height: f32) -> Vec2 {
    Vec2::new(
        8.0 + 8.6 * f32::from(u16::try_from(text.chars().count()).unwrap_or(u16::MAX)),
        height,
    )
}

pub(crate) fn distance(
    regions: &[RepositoryRegion],
    focus: Vec3,
    fov: f32,
    viewport: Vec2,
) -> f32 {
    let forward = -view_direction();
    let right = forward.cross(Vec3::Y).normalize();
    let up = right.cross(forward);
    let tangent = (fov * 0.5).tan();
    let aspect = viewport.x / viewport.y;
    let limit = Vec2::ONE - Vec2::splat(2.0 * MARGIN) / viewport;
    let mut distance = 2.0_f32;
    let mut include = |point: Vec3, low: Vec2, high: Vec2| {
        let offset = point - focus;
        let depth = offset.dot(forward);
        distance = distance.max(0.5 - depth);
        for (coordinate, slope, low, high) in [
            (offset.dot(right), tangent * aspect, low.x, high.x),
            (offset.dot(up), tangent, low.y, high.y),
        ] {
            distance = distance
                .max(coordinate / (slope * high) - depth)
                .max(coordinate / (slope * low) - depth);
        }
    };
    for region in regions.iter().filter(|r| r.live) {
        if let Some(tile) = region.tile {
            for point in tile.corners() {
                include(
                    ground(crate::layout::world_coordinates(unproject_at(
                        point,
                        GLASS_BOTTOM,
                    ))) + Vec3::Y * GLASS_BOTTOM,
                    -limit,
                    limit,
                );
            }
        }
        for corner in region.corners() {
            for height in [GLASS_BOTTOM, GLASS_TOP] {
                include(ground(corner) + Vec3::Y * height, -limit, limit);
            }
        }
        let label_size = label_size(&region.label, 20.0) * 2.0 / viewport;
        // UI labels extend right and down from their projected world anchor.
        include(
            label_position(region),
            -limit + Vec2::Y * label_size.y,
            limit - Vec2::X * label_size.x,
        );
    }
    distance + 0.001
}

pub(super) fn depth_bound() -> f32 {
    crate::layout::packing::Tile::viewport()
        .ground()
        .into_iter()
        .map(|p| p.length() + LABEL_Y)
        .fold(0.0_f32, f32::max)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contributors::Mappings,
        model::{Options, Simulation, frames_for},
        theme::Theme,
    };
    use bevy::{
        camera::{CameraProjection, PerspectiveProjection},
        prelude::*,
    };

    fn assert_region_in_view(
        region: &RepositoryRegion,
        clip: Mat4,
        viewport: Vec2,
        frame: u32,
    ) {
        let screen = |point| {
            let point = clip.project_point3(point);
            Vec2::new(point.x + 1.0, 1.0 - point.y) * viewport * 0.5
        };
        let inside = |point: Vec2| {
            point.cmpge(Vec2::splat(MARGIN - 0.02)).all()
                && point.cmple(viewport - Vec2::splat(MARGIN - 0.02)).all()
        };
        let tile = region.tile.unwrap();
        for corner in tile.corners() {
            let world = ground(crate::layout::world_coordinates(unproject_at(
                corner,
                GLASS_BOTTOM,
            ))) + Vec3::Y * GLASS_BOTTOM;
            let point = screen(world);
            assert!(
                inside(point),
                "frame {frame} {} {viewport:?}: corner {point:?}",
                region.label
            );
        }
        let anchor = screen(label_position(region));
        assert!(
            inside(anchor) && inside(anchor + label_size(&region.label, 20.0)),
            "frame {frame} {} {viewport:?}: label {anchor:?}",
            region.label
        );
    }
    #[test]
    fn every_frame_contains_live_glass_and_raised_labels_in_real_projections() {
        for name in crate::theme::ThemeName::ALL {
            assert_theme_framing(&name.build());
        }
    }

    fn assert_theme_framing(theme: &Theme) {
        let histories = (0..5)
            .map(|index| {
                let mut events = crate::timeline::synthetic();
                for event in &mut events {
                    event.seconds += i64::try_from(index).unwrap() * 7 * 86_400;
                }
                crate::repositories::namespace(&mut events, index);
                events
            })
            .collect();
        let mut simulation = Simulation::with_repositories(
            crate::repositories::merge(histories),
            &Options {
                seconds_per_day: 0.08,
                max_idle_seconds: 0.2,
                ..default()
            },
            theme,
            &Mappings::default(),
            &[
                "uv-secure".into(),
                "ryl".into(),
                "aiomoto".into(),
                "pyglobegl".into(),
                "codestrand".into(),
            ],
        );
        let mut previous = simulation.view().1;
        let fixed_focus = simulation.view().0;
        for frame in 0..frames_for(simulation.duration(), 60).unwrap() {
            simulation.advance_to(frame, 60);
            assert_eq!(simulation.view().0, fixed_focus, "camera pan at {frame}");
            let distance = simulation.view().1;
            assert!(distance >= previous - 0.0001, "inward zoom at {frame}");
            assert!(
                distance <= previous * 1.03,
                "zoom jump at {frame}: {previous} -> {distance}"
            );
            previous = distance;
            for (viewport, fov) in [
                (VIEWPORT, FOV),
                (Vec2::new(720.0, 1280.0), 0.6),
                (Vec2::new(1920.0, 600.0), 1.0),
            ] {
                let projection = PerspectiveProjection {
                    fov,
                    aspect_ratio: viewport.x / viewport.y,
                    ..default()
                };
                let (focus, distance) = simulation.view_for_projection(fov, viewport);
                let transform =
                    Transform::from_translation(focus + view_direction() * distance)
                        .looking_at(focus, Vec3::Y);
                let clip =
                    projection.get_clip_from_view() * transform.to_matrix().inverse();
                for region in simulation.layout().repositories.iter().filter(|r| r.live)
                {
                    assert_region_in_view(region, clip, viewport, frame);
                }
            }
        }
        assert_eq!(simulation.commit(), simulation.total());
        simulation.advance_to(frames_for(simulation.duration(), 60).unwrap() + 60, 60);
        assert_eq!(simulation.view().0, fixed_focus);
        simulation.advance_to(0, 60);
        assert_eq!(simulation.view().0, fixed_focus);
        simulation.advance_to(frames_for(simulation.duration(), 60).unwrap(), 60);
        assert_eq!(
            simulation
                .layout()
                .repositories
                .iter()
                .filter(|r| r.live)
                .count(),
            5
        );
    }
}
