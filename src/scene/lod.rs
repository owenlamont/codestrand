use bevy::prelude::*;

use crate::layout::Layout;

const COLLAPSE_PIXELS: f32 = 32.0;
const EXPAND_PIXELS: f32 = 40.0;
const TRANSITION_SECONDS: f32 = 0.3;

#[derive(Default)]
pub(super) struct Lod {
    seconds: Option<f32>,
    transition_seconds: f32,
    collapsed: Vec<bool>,
    blend: Vec<f32>,
    pub detail: Vec<f32>,
    pub glyph: Vec<f32>,
    pub size: Vec<f32>,
    low: Vec<Vec2>,
    high: Vec<Vec2>,
    active: Vec<bool>,
    files: Vec<usize>,
    pub glyphs: Vec<usize>,
}

impl Lod {
    #[cfg(test)]
    pub(super) fn sample(&mut self, layout: &Layout, seconds: f32, view: View) {
        self.sample_clocks(layout, seconds, seconds, view);
    }

    pub(super) fn sample_clocks(
        &mut self,
        layout: &Layout,
        seconds: f32,
        transition_seconds: f32,
        view: View,
    ) {
        let reset = self.seconds.is_none_or(|previous| seconds < previous);
        let dt = (transition_seconds - self.transition_seconds).max(0.0);
        self.seconds = Some(seconds);
        self.transition_seconds = transition_seconds;
        let count = layout.dir_count();
        if reset {
            self.collapsed.clear();
            self.blend.clear();
        }
        self.collapsed.resize(count, false);
        self.blend.resize(count, 0.0);
        self.detail.resize(count, 1.0);
        self.glyph.resize(count, 0.0);
        self.size.resize(count, 1.0);
        self.low.resize(count, Vec2::ZERO);
        self.low.fill(Vec2::splat(f32::INFINITY));
        self.high.resize(count, Vec2::ZERO);
        self.high.fill(Vec2::splat(f32::NEG_INFINITY));
        self.active.resize(count, false);
        self.active.fill(false);
        self.files.resize(count, 0);
        self.files.fill(0);
        self.glyphs.clear();
        let (low, high, active, files) = (
            &mut self.low,
            &mut self.high,
            &mut self.active,
            &mut self.files,
        );
        for (_, file) in layout.files() {
            low[file.dir] = low[file.dir].min(file.position - Vec2::splat(0.4));
            high[file.dir] = high[file.dir].max(file.position + Vec2::splat(0.4));
            active[file.dir] |= file.touch_age < 2.5;
            files[file.dir] += 1;
        }
        for index in (0..count).rev() {
            let dir = layout.dir(index);
            if dir.live {
                low[index] = low[index].min(dir.position - Vec2::splat(0.4));
                high[index] = high[index].max(dir.position + Vec2::splat(0.4));
            }
            if let Some(parent) = dir.parent {
                low[parent] = low[parent].min(low[index]);
                high[parent] = high[parent].max(high[index]);
                active[parent] |= active[index];
                files[parent] += files[index];
            }
        }
        for index in 0..count {
            let dir = layout.dir(index);
            let pixels = view.footprint(low[index], high[index]);
            self.collapsed[index] = dir.live
                && index != 0
                && files[index] > 1
                && !active[index]
                && pixels
                    < if self.collapsed[index] {
                        EXPAND_PIXELS
                    } else {
                        COLLAPSE_PIXELS
                    };
            let target = if self.collapsed[index] { 1.0 } else { 0.0 };
            self.blend[index] = if reset {
                target
            } else {
                self.blend[index]
                    + (target - self.blend[index])
                        .clamp(-dt / TRANSITION_SECONDS, dt / TRANSITION_SECONDS)
            };
            let parent_detail = dir.parent.map_or(1.0, |parent| self.detail[parent]);
            let weight =
                self.blend[index] * self.blend[index] * (3.0 - 2.0 * self.blend[index]);
            self.glyph[index] = parent_detail * weight;
            if self.glyph[index] > 0.0 {
                self.glyphs.push(index);
            }
            self.detail[index] = parent_detail * (1.0 - weight);
            let point = Vec3::new(dir.position.x, 0.0, dir.position.y);
            let depth = (point - view.eye).dot((view.focus - view.eye).normalize());
            self.size[index] =
                (8.0 * 2.0 * depth * (view.fov * 0.5).tan() / view.height).max(1.0);
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct View {
    pub eye: Vec3,
    pub focus: Vec3,
    pub height: f32,
    pub fov: f32,
}

impl View {
    fn footprint(self, low: Vec2, high: Vec2) -> f32 {
        if !low.is_finite() || !high.is_finite() {
            return f32::INFINITY;
        }
        let forward = (self.focus - self.eye).normalize();
        let center = (low + high) * 0.5;
        let radius = (high - low).length() * 0.5;
        let depth = (Vec3::new(center.x, 0.0, center.y) - self.eye).dot(forward);
        if depth - radius <= 0.5 {
            return f32::INFINITY;
        }
        radius * self.height / (depth * (self.fov * 0.5).tan())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(distance: f32) -> View {
        View {
            eye: Vec3::new(0.0, distance, distance),
            focus: Vec3::ZERO,
            height: 720.0,
            fov: std::f32::consts::FRAC_PI_4,
        }
    }

    fn layout() -> Layout {
        let mut layout = Layout::planned_paths(&["a/b/one.rs", "a/b/two.rs"], 0);
        layout.add("a/b/one.rs", 0.0, 0);
        layout.add("a/b/two.rs", 0.0, 0);
        layout.settle(120, 1.0 / 60.0);
        layout
    }

    #[test]
    fn projection_uses_pixels_and_expands_near_the_camera() {
        let low = Vec2::splat(-1.0);
        let high = Vec2::splat(1.0);
        assert!(view(1000.0).footprint(low, high) < COLLAPSE_PIXELS);
        assert!(view(3.0).footprint(low, high) > EXPAND_PIXELS);
        assert!(view(0.1).footprint(low, high).is_infinite());
        let mut doubled = view(1000.0);
        doubled.height *= 2.0;
        assert_eq!(
            doubled.footprint(low, high),
            2.0 * view(1000.0).footprint(low, high)
        );
    }

    #[test]
    fn nested_collapse_has_one_glyph() {
        let layout = layout();
        let mut lod = Lod::default();
        lod.sample(&layout, 0.0, view(10000.0));
        assert_eq!(lod.glyph.iter().sum::<f32>(), 1.0);
        assert_eq!(lod.detail[0], 1.0);
        assert_eq!(*lod.detail.last().unwrap(), 0.0);
    }

    #[test]
    fn zoom_transition_uses_playback_time_and_repeated_capture_is_idempotent() {
        let layout = layout();
        let mut lod = Lod::default();
        lod.sample(&layout, 0.0, view(10000.0));
        lod.sample(&layout, 0.15, view(1.0));
        let half = lod.detail.clone();
        assert!(half[1] > 0.0 && half[1] < 1.0);
        lod.sample(&layout, 0.15, view(1.0));
        assert_eq!(half, lod.detail);
        lod.sample(&layout, 0.31, view(1.0));
        assert_eq!(lod.detail, vec![1.0; lod.detail.len()]);
        lod.sample(&layout, 0.0, view(10000.0));
        assert_eq!(lod.glyph.iter().sum::<f32>(), 1.0);
    }

    #[test]
    fn activity_expands_ancestors() {
        let mut layout = layout();
        let mut lod = Lod::default();
        lod.sample(&layout, 0.0, view(10000.0));
        let id = layout.touch("a/b/one.rs", 0);
        layout.heat(id, 0.12 + 0.08 * 2.0_f32.ln());
        assert!(layout.file(id).unwrap().heat < 0.2);
        lod.sample(&layout, 0.31, view(10000.0));
        assert_eq!(lod.detail, vec![1.0; lod.detail.len()]);
        layout.heat(id, 1.0);
        for _ in 0..200 {
            layout.cool_heat(1.0 / 60.0, 3.0);
        }
        assert!(layout.file(id).unwrap().heat > 0.2);
        lod.sample(&layout, 3.64, view(10000.0));
        assert_eq!(lod.glyph.iter().sum::<f32>(), 1.0);
    }

    #[test]
    fn preview_transition_ignores_playback_speed() {
        let layout = layout();
        for speed in [0.125, 8.0] {
            let mut lod = Lod::default();
            lod.sample_clocks(&layout, 0.0, 0.0, view(10000.0));
            lod.sample_clocks(&layout, 0.15 * speed, 0.15, view(1.0));
            assert_eq!(lod.detail[1], 0.5);
            lod.sample_clocks(&layout, 0.3 * speed, 0.3, view(1.0));
            assert_eq!(lod.detail[1], 1.0);
        }
    }

    #[test]
    fn bounding_circle_is_stable_during_orbit() {
        let camera = view(100.0);
        let expected = camera.footprint(Vec2::splat(-2.0), Vec2::splat(2.0));
        for angle in [0.2, 0.7, 1.4, 2.5] {
            let rotated = View {
                eye: Quat::from_rotation_y(angle) * camera.eye,
                ..camera
            };
            assert!(
                (rotated.footprint(Vec2::splat(-2.0), Vec2::splat(2.0)) - expected)
                    .abs()
                    < 0.001
            );
        }
    }

    #[test]
    fn hysteresis_keeps_the_previous_decision_inside_the_band() {
        let layout = layout();
        let mut lod = Lod::default();
        let mut camera = view(10000.0);
        lod.sample(&layout, 0.0, camera);
        let bounds = (lod.low[1], lod.high[1]);
        camera.height *= 36.0 / camera.footprint(bounds.0, bounds.1);
        lod.sample(&layout, 0.4, camera);
        assert!(lod.collapsed[1]);
        camera.height *= 2.0;
        lod.sample(&layout, 0.8, camera);
        assert!(!lod.collapsed[1]);
        camera.height *= 0.5;
        lod.sample(&layout, 1.2, camera);
        assert!(!lod.collapsed[1]);
    }
}
