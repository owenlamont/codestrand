use bevy::math::{Vec2, Vec3};

use super::{ground, tick_seconds, view_direction};
use crate::layout::{RepositoryRegion, packing::Packing};

pub(crate) const FOV: f32 = std::f32::consts::FRAC_PI_4;
pub(crate) const VIEWPORT: Vec2 = Vec2::new(1280.0, 720.0);
pub(crate) const GLASS_BOTTOM: f32 = -0.1;
pub(crate) const GLASS_TOP: f32 = 1.1;
pub(crate) const LABEL_Y: f32 = 1.6;
pub(crate) const MARGIN: f32 = 24.0;

#[derive(Clone, Copy, Debug)]
pub(super) struct View {
    pub focus: Vec3,
    pub distance: f32,
    pub depth_bound: f32,
}

pub(super) fn label_position(region: &RepositoryRegion) -> Vec3 {
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

pub(super) fn distance(
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

pub(super) fn plan(packing: &Packing, ticks: u32) -> Vec<View> {
    let mut views = Vec::new();
    let mut focus = Vec3::ZERO;
    let ease = 1.0 - (-tick_seconds() / 1.5).exp();
    for tick in 0..=ticks {
        let Some((regions, _, _)) = packing.sample(u64::from(tick)) else {
            break;
        };
        let mut low = Vec2::splat(f32::INFINITY);
        let mut high = Vec2::splat(f32::NEG_INFINITY);
        for region in regions.iter().filter(|r| r.live) {
            for corner in region.corners() {
                low = low.min(corner);
                high = high.max(corner);
            }
        }
        if low.is_finite() {
            focus = focus.lerp(ground((low + high) * 0.5), ease);
        }
        let depth_bound = regions
            .iter()
            .filter(|r| r.live)
            .flat_map(RepositoryRegion::corners)
            .map(|p| (ground(p) - focus).length() + LABEL_Y)
            .fold(0.0_f32, f32::max);
        views.push(View {
            focus,
            distance: distance(&regions, focus, FOV, VIEWPORT).max(24.0),
            depth_bound,
        });
    }
    let weight = (-tick_seconds() / 2.0).exp();
    for tick in (0..views.len().saturating_sub(1)).rev() {
        views[tick].distance = views[tick].distance.max(
            views[tick].distance
                + (views[tick + 1].distance - views[tick].distance) * weight,
        );
    }
    for tick in 1..views.len() {
        views[tick].distance = views[tick].distance.max(views[tick - 1].distance);
    }
    let depth_bound = views.iter().map(|v| v.depth_bound).fold(0.0_f32, f32::max);
    for view in &mut views {
        view.depth_bound = depth_bound;
    }
    views
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
        let centre = crate::layout::world_coordinates((region.low + region.high) * 0.5);
        let size = region.high - region.low;
        let mesh = Transform::from_xyz(centre.x, 0.5, centre.y)
            .with_scale(Vec3::new(size.x, 1.2, size.y))
            .with_rotation(Quat::from_rotation_y(
                view_direction().x.atan2(view_direction().z),
            ));
        for corner in super::super::box_corners(-Vec3::splat(0.5), Vec3::splat(0.5)) {
            let point = screen(mesh.transform_point(corner));
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
    fn every_frame_contains_live_prisms_and_raised_labels_in_real_projections() {
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
            &Theme::neon(),
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
        for frame in 0..frames_for(simulation.duration(), 60).unwrap() {
            simulation.advance_to(frame, 60);
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
