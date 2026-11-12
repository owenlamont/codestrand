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

const CLIP_PLANES: [Vec4; 6] = [
    Vec4::new(0.0, 0.0, -1.0, 1.0),
    Vec4::Z,
    Vec4::new(1.0, 0.0, 0.0, 1.0),
    Vec4::new(-1.0, 0.0, 0.0, 1.0),
    Vec4::new(0.0, 1.0, 0.0, 1.0),
    Vec4::new(0.0, -1.0, 0.0, 1.0),
];

struct BroadPhase {
    planes: [(Vec4, f32); 6],
    rows: [Vec4; 3],
    lengths: Vec3,
    viewport: Vec2,
}

impl BroadPhase {
    fn new(view: &View) -> Self {
        let transpose = view.clip_from_world.transpose();
        let rows = [transpose.x_axis, transpose.y_axis, transpose.w_axis];
        Self {
            planes: CLIP_PLANES.map(|p| {
                let plane = transpose * p;
                (plane, plane.truncate().length())
            }),
            rows,
            lengths: Vec3::new(
                rows[0].truncate().length(),
                rows[1].truncate().length(),
                rows[2].truncate().length(),
            ),
            viewport: view.viewport,
        }
    }

    fn rejects(&self, center: Vec3, radius: f32) -> bool {
        let center = center.extend(1.0);
        // Inflate support to retain tangencies despite floating-point roundoff.
        let radius = radius * 1.0001 + 0.0001;
        if self
            .planes
            .iter()
            .any(|&(p, length)| p.dot(center) < -radius * length)
        {
            return true;
        }
        let c = Vec3::new(
            self.rows[0].dot(center),
            self.rows[1].dot(center),
            self.rows[2].dot(center),
        );
        let extent = self.lengths * radius;
        let near_w = c.z - extent.z;
        if near_w <= 0.0 {
            return false;
        }
        // Clipping only shrinks the rectangle bounded by
        // |x/w - cx/cw| <= (rx + |cx/cw| rw) / (cw - rw).
        let diameter = (extent.truncate() + (c.truncate() / c.z).abs() * extent.z)
            * self.viewport
            / near_w;
        diameter.min_element() < EXIT_PIXELS - 0.01
    }
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
        // A convex quad gains at most one vertex per clipping plane: 4 + 6 < 16.
        let mut polygon = [Vec4::ZERO; 16];
        let mut clipped = [Vec4::ZERO; 16];
        for (slot, p) in polygon.iter_mut().zip(corners) {
            *slot = self.clip_from_world * p.extend(1.0);
        }
        let mut len = 4;
        for plane in CLIP_PLANES {
            let mut count = 0;
            for index in 0..len {
                let (a, b) = (polygon[index], polygon[(index + 1) % len]);
                let (da, db) = (plane.dot(a), plane.dot(b));
                if da >= 0.0 {
                    clipped[count] = a;
                    count += 1;
                }
                if (da >= 0.0) != (db >= 0.0) {
                    clipped[count] = a.lerp(b, da / (da - db));
                    count += 1;
                }
            }
            if count == 0 {
                return None;
            }
            std::mem::swap(&mut polygon, &mut clipped);
            len = count;
        }
        let mut low = Vec2::splat(f32::INFINITY);
        let mut high = Vec2::splat(f32::NEG_INFINITY);
        for &p in &polygon[..len] {
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
        let broad = BroadPhase::new(view);
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
            if opacity <= 0.001 || broad.rejects(center, half.truncate().length()) {
                continue;
            }
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
            if opacity <= 0.001 || broad.rejects(center, half.length()) {
                continue;
            }
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
    fn broad_phase_rejects_only_subthreshold_footprints() {
        for (pitch, roll, aspect) in [(0.0, 0.0, 3.0), (1.5, 0.0, 1.8), (0.8, 0.6, 1.0)]
        {
            let rotation = Quat::from_euler(EulerRot::YXZ, 0.3, pitch, roll);
            let view = View {
                clip_from_world: bevy::math::proj::perspective_infinite_reverse(
                    std::f32::consts::FRAC_PI_4,
                    aspect,
                    0.1,
                ) * Mat4::from_quat(rotation).inverse(),
                rotation,
                viewport: Vec2::new(720.0 * aspect, 720.0),
            };
            let broad = BroadPhase::new(&view);
            let (mut rejected, mut selectable) = (0, 0);
            let bounds =
                [-100.0, -1.0, 0.0, 1.0, 100.0].into_iter().flat_map(|x| {
                    [-100.0, -10.0, -1.0, -0.1, 0.0, 1.0].into_iter().flat_map(
                        move |z| [0.001, 0.04, 0.2, 1.0, 10.0].map(|size| (x, z, size)),
                    )
                });
            for (x, z, size) in bounds {
                let center = rotation * Vec3::new(x, 0.0, z);
                let half = Vec3::splat(size);
                let quad = corners(
                    center,
                    rotation * Vec3::X * size,
                    rotation * Vec3::Y * size,
                );
                for (radius, pixels) in [
                    (half.truncate().length(), view.footprint([quad])),
                    (half.length(), view.footprint(cuboid_faces(center, half))),
                ] {
                    let skip = broad.rejects(center, radius);
                    assert!(
                        !skip || pixels < EXIT_PIXELS,
                        "pitch={pitch} roll={roll} aspect={aspect} center={center:?} radius={radius} pixels={pixels}"
                    );
                    rejected += usize::from(skip);
                    selectable += usize::from(pixels >= EXIT_PIXELS);
                }
            }
            assert!(rejected > 0 && selectable > 0);
        }
    }

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
