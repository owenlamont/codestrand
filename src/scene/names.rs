use std::collections::BTreeSet;

use bevy::prelude::*;

use crate::model::{Label, LabelKind, Simulation, Target};

const ENTER_PIXELS: f32 = 48.0;
const EXIT_PIXELS: f32 = 40.0;

#[derive(Default)]
pub(super) struct Names {
    sampled: Option<Sample>,
    #[cfg(test)]
    samples: usize,
    seconds: Option<f32>,
    pub targets: BTreeSet<Target>,
    pub labels: Vec<Label>,
}

#[derive(PartialEq)]
struct Sample {
    seconds: u32,
    theme: crate::theme::ThemeName,
    rate: u32,
    clip_from_world: Mat4,
    viewport: Vec2,
}

pub(super) struct View {
    pub clip_from_world: Mat4,
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
    pub(super) fn invalidate(&mut self) {
        self.sampled = None;
    }

    pub(super) fn sample(&mut self, simulation: &Simulation, view: &View) {
        let sampled = Sample {
            seconds: simulation.seconds().to_bits(),
            theme: simulation.theme().name,
            rate: simulation.playback_multiplier().to_bits(),
            clip_from_world: view.clip_from_world,
            viewport: view.viewport,
        };
        if self.sampled.as_ref() == Some(&sampled) {
            return;
        }
        self.sampled = Some(sampled);
        #[cfg(test)]
        {
            self.samples += 1;
        }
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
        let glyph = theme.glyph_transform();
        for (id, file) in layout.files() {
            let dir = layout.dir(file.dir);
            let scale = file.draw_scale(dir);
            let (point, _, opacity) = simulation.file_effect(id, file);
            let center = point + glyph.translation * scale;
            let half = glyph.scale * scale * 0.5;
            if opacity <= 0.001 || broad.rejects(center, half.truncate().length()) {
                continue;
            }
            let bounds = corners(
                center,
                glyph.rotation * Vec3::X * half.x,
                glyph.rotation * Vec3::Y * half.y,
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

    fn sampled() -> (Names, Simulation, View) {
        let mut simulation = Simulation::new(
            crate::timeline::synthetic(),
            &crate::model::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        simulation.advance_to(60, 60);
        let (id, file) = simulation.layout().files().next().unwrap();
        let point = simulation.file_effect(id, file).0;
        let view = View {
            clip_from_world: bevy::math::proj::perspective_infinite_reverse(
                std::f32::consts::FRAC_PI_4,
                1.0,
                0.01,
            ) * Transform::from_translation(point + Vec3::Y * 4.0)
                .looking_at(point, Vec3::NEG_Z)
                .to_matrix()
                .inverse(),
            viewport: Vec2::splat(720.0),
        };
        let mut names = Names::default();
        names.sample(&simulation, &view);
        (names, simulation, view)
    }

    #[test]
    fn unchanged_sample_reuses_targets_and_labels() {
        let (mut names, simulation, view) = sampled();
        assert!(!names.labels.is_empty());
        let targets = names.targets.clone();
        let labels: Vec<_> = names
            .labels
            .iter()
            .map(|label| label.text.clone())
            .collect();
        names.sample(&simulation, &view);
        assert_eq!(names.samples, 1);
        assert_eq!(names.targets, targets);
        assert_eq!(
            names
                .labels
                .iter()
                .map(|label| label.text.clone())
                .collect::<Vec<_>>(),
            labels
        );
    }

    #[test]
    fn theme_change_resamples_static_names() {
        let (mut names, mut simulation, view) = sampled();
        simulation.set_theme(crate::theme::ThemeName::Phosphor.build());
        names.sample(&simulation, &view);
        assert_eq!(names.samples, 2);
    }

    #[test]
    fn viewport_change_resamples_static_names() {
        let (mut names, simulation, mut view) = sampled();
        view.viewport *= 2.0;
        names.sample(&simulation, &view);
        assert_eq!(names.samples, 2);
    }

    #[test]
    fn camera_change_resamples_static_names() {
        let (mut names, simulation, mut view) = sampled();
        view.clip_from_world = Mat4::from_translation(Vec3::X);
        names.sample(&simulation, &view);
        assert_eq!(names.samples, 2);
    }

    #[test]
    fn tick_change_resamples_names() {
        let (mut names, mut simulation, view) = sampled();
        simulation.advance_to(61, 60);
        names.sample(&simulation, &view);
        assert_eq!(names.samples, 2);
    }

    #[test]
    fn rate_change_resamples_names() {
        let (mut names, mut simulation, view) = sampled();
        simulation.change_rate(2.0);
        names.sample(&simulation, &view);
        assert_eq!(names.samples, 2);
    }

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
