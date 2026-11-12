use std::collections::BTreeSet;

use bevy::prelude::*;

use crate::model::{Label, LabelKind, Simulation, Target};

const ENTER_PIXELS: f32 = 48.0;
const EXIT_PIXELS: f32 = 40.0;

#[derive(Default)]
pub(super) struct Names {
    seconds: Option<f32>,
    pub targets: BTreeSet<Target>,
    pub labels: Vec<Label>,
}

pub(super) struct View {
    pub clip_from_world: Mat4,
    pub rotation: Quat,
    pub viewport: Vec2,
}

impl View {
    fn footprint(&self, faces: impl IntoIterator<Item = [Vec3; 4]>) -> f32 {
        faces
            .into_iter()
            .filter_map(|face| self.face_bounds(face))
            .reduce(|a, b| Rect::from_corners(a.min.min(b.min), a.max.max(b.max)))
            .map_or(0.0, |bounds| {
                (bounds.size() * self.viewport * 0.5).min_element()
            })
    }

    fn face_bounds(&self, corners: [Vec3; 4]) -> Option<Rect> {
        let mut polygon: Vec<_> = corners
            .into_iter()
            .map(|p| self.clip_from_world * p.extend(1.0))
            .collect();
        for plane in [
            Vec4::new(0.0, 0.0, -1.0, 1.0),
            Vec4::Z,
            Vec4::new(1.0, 0.0, 0.0, 1.0),
            Vec4::new(-1.0, 0.0, 0.0, 1.0),
            Vec4::new(0.0, 1.0, 0.0, 1.0),
            Vec4::new(0.0, -1.0, 0.0, 1.0),
        ] {
            let mut clipped = Vec::new();
            for (&a, &b) in polygon
                .iter()
                .zip(polygon.iter().cycle().skip(1))
                .take(polygon.len())
            {
                let (da, db) = (plane.dot(a), plane.dot(b));
                if da >= 0.0 {
                    clipped.push(a);
                }
                if (da >= 0.0) != (db >= 0.0) {
                    clipped.push(a.lerp(b, da / (da - db)));
                }
            }
            polygon = clipped;
            if polygon.is_empty() {
                return None;
            }
        }
        let mut low = Vec2::splat(f32::INFINITY);
        let mut high = Vec2::splat(f32::NEG_INFINITY);
        for p in polygon {
            if !p.is_finite() || p.w <= 0.0 {
                return None;
            }
            let point = p.truncate().truncate() / p.w;
            low = low.min(point);
            high = high.max(point);
        }
        Some(Rect::from_corners(low, high))
    }
}

fn corners(center: Vec3, right: Vec3, up: Vec3) -> [Vec3; 4] {
    [
        center - right - up,
        center + right - up,
        center + right + up,
        center - right + up,
    ]
}

fn cuboid_faces(center: Vec3, half: Vec3) -> [[Vec3; 4]; 6] {
    let (x, y, z) = (Vec3::X * half.x, Vec3::Y * half.y, Vec3::Z * half.z);
    [
        corners(center + x, y, z),
        corners(center - x, y, z),
        corners(center + y, x, z),
        corners(center - y, x, z),
        corners(center + z, x, y),
        corners(center - z, x, y),
    ]
}

impl Names {
    pub(super) fn sample(&mut self, simulation: &Simulation, view: &View) {
        if self
            .seconds
            .is_none_or(|previous| simulation.seconds() < previous)
        {
            self.targets.clear();
        }
        self.seconds = Some(simulation.seconds());
        self.labels.clear();
        let mut selected = BTreeSet::new();
        let layout = simulation.layout();
        let theme = simulation.theme();
        for (id, file) in layout.files() {
            let dir = layout.dir(file.dir);
            let scale = file.draw_scale(dir);
            let (point, _, opacity) = simulation.file_effect(file);
            let rotation = Quat::from_rotation_y(
                if layout
                    .repositories
                    .get(dir.repository)
                    .is_some_and(|r| r.rotated)
                {
                    -std::f32::consts::FRAC_PI_2
                } else {
                    0.0
                },
            );
            let center =
                point + rotation * theme.file_glyph_transform.translation * scale;
            let half = theme.file_glyph_transform.scale * scale * 0.5;
            let bounds = corners(
                center,
                view.rotation * Vec3::X * half.x,
                view.rotation * Vec3::Y * half.y,
            );
            self.include(
                (
                    Target::File(id),
                    point + Vec3::new(-0.2, 0.0, 0.45) * dir.scale,
                    opacity,
                ),
                view.footprint([bounds]),
                &mut selected,
                || file.name().to_owned(),
            );
        }
        for (index, dir) in layout.dirs().filter(|(_, dir)| dir.depth > 0) {
            let (_, opacity, rise) = simulation.repository_effect(dir.repository);
            let center = super::ground(dir.position) + Vec3::Y * (rise - 0.05);
            let half = theme.directory_node_scale * dir.scale * 0.5;
            let bounds = cuboid_faces(center, half);
            self.include(
                (
                    Target::Dir(index),
                    center + Vec3::new(-half.x, 0.1, half.z),
                    opacity,
                ),
                view.footprint(bounds),
                &mut selected,
                || format!("{}/", dir.name()),
            );
        }
        self.targets = selected;
    }

    fn include(
        &mut self,
        entry: (Target, Vec3, f32),
        pixels: f32,
        selected: &mut BTreeSet<Target>,
        text: impl FnOnce() -> String,
    ) {
        let (target, position, opacity) = entry;
        let threshold = if self.targets.contains(&target) {
            EXIT_PIXELS
        } else {
            ENTER_PIXELS
        };
        if opacity > 0.001 && pixels >= threshold {
            selected.insert(target);
            self.labels.push(Label {
                position,
                text: text(),
                kind: LabelKind::Detail,
                scale: 0.7,
                opacity,
                priority: 0.0,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footprint_clips_offscreen_behind_and_near_plane_geometry() {
        let view = View {
            clip_from_world: bevy::math::proj::perspective_infinite_reverse(
                std::f32::consts::FRAC_PI_4,
                1.0,
                0.1,
            ),
            rotation: Quat::IDENTITY,
            viewport: Vec2::splat(720.0),
        };
        let quad = |center| corners(center, Vec3::X * 0.2, Vec3::Y * 0.2);
        assert!(view.footprint([quad(Vec3::new(0.0, 0.0, -1.0))]) > ENTER_PIXELS);
        assert_eq!(view.footprint([quad(Vec3::new(100.0, 0.0, -1.0))]), 0.0);
        assert_eq!(view.footprint([quad(Vec3::Z)]), 0.0);
        let crossing = corners(
            Vec3::new(0.0, 0.0, -0.1),
            Vec3::new(0.2, 0.0, 0.2),
            Vec3::Y * 0.2,
        );
        let pixels = view.footprint([crossing]);
        assert!(pixels.is_finite() && pixels > 0.0 && pixels <= 720.0);
        assert_eq!(
            view.footprint(cuboid_faces(Vec3::new(0.0, 0.0, -0.15), Vec3::splat(0.2))),
            720.0,
        );
        assert_eq!(view.footprint(cuboid_faces(Vec3::Z, Vec3::splat(0.2))), 0.0);
        assert_eq!(
            view.footprint(cuboid_faces(Vec3::new(100.0, 0.0, -1.0), Vec3::splat(0.2))),
            0.0,
        );
    }

    #[test]
    fn name_zoom_band_keeps_identity_and_world_anchor() {
        let mut names = Names::default();
        let anchor = Vec3::new(3.0, 0.5, -2.0);
        for (pixels, expected) in
            [(49.0, true), (45.0, true), (39.0, false), (45.0, false)]
        {
            let mut selected = BTreeSet::new();
            names.labels.clear();
            names.include(
                (Target::File(1), anchor, 1.0),
                pixels,
                &mut selected,
                || "inactive.rs".into(),
            );
            assert_eq!(selected.contains(&Target::File(1)), expected);
            for label in &names.labels {
                assert_eq!(label.position, anchor);
            }
            names.targets = selected;
        }
    }
}
