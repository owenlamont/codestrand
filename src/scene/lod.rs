use bevy::prelude::*;

use crate::layout::Layout;

const COLLAPSE_PIXELS: f32 = 32.0;
const EXPAND_PIXELS: f32 = 40.0;
const TRANSITION_SECONDS: f32 = 0.3;

#[derive(Default)]
pub(super) struct Lod {
    seconds: Option<f32>,
    collapsed: Vec<bool>,
    blend: Vec<f32>,
    pub detail: Vec<f32>,
    pub glyph: Vec<f32>,
    pub size: Vec<f32>,
}

impl Lod {
    pub(super) fn sample(&mut self, layout: &Layout, seconds: f32, view: View) {
        let reset = self.seconds.is_none_or(|previous| seconds < previous);
        let dt = self
            .seconds
            .map_or(0.0, |previous| (seconds - previous).max(0.0));
        self.seconds = Some(seconds);
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
        let mut low = vec![Vec2::splat(f32::INFINITY); count];
        let mut high = vec![Vec2::splat(f32::NEG_INFINITY); count];
        let mut active = vec![false; count];
        let mut files = vec![0_usize; count];
        for (_, file) in layout.files() {
            low[file.dir] = low[file.dir].min(file.position - Vec2::splat(0.4));
            high[file.dir] = high[file.dir].max(file.position + Vec2::splat(0.4));
            active[file.dir] |= file.heat > 0.2;
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
        let right = forward.cross(Vec3::Y).normalize();
        let up = right.cross(forward);
        let mut minimum = Vec2::splat(f32::INFINITY);
        let mut maximum = Vec2::splat(f32::NEG_INFINITY);
        for point in [
            low,
            Vec2::new(low.x, high.y),
            high,
            Vec2::new(high.x, low.y),
        ] {
            let offset = Vec3::new(point.x, 0.0, point.y) - self.eye;
            let depth = offset.dot(forward);
            if depth <= 0.5 {
                return f32::INFINITY;
            }
            let projected = Vec2::new(offset.dot(right), offset.dot(up)) * self.height
                / (2.0 * depth * (self.fov * 0.5).tan());
            minimum = minimum.min(projected);
            maximum = maximum.max(projected);
        }
        (maximum - minimum).max_element()
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
    fn nested_collapse_has_one_glyph_and_leaves_layout_unchanged() {
        let layout = layout();
        let positions: Vec<_> = layout.files().map(|(_, file)| file.position).collect();
        let mut lod = Lod::default();
        lod.sample(&layout, 0.0, view(10000.0));
        assert_eq!(lod.glyph.iter().sum::<f32>(), 1.0);
        assert_eq!(lod.detail[0], 1.0);
        assert_eq!(*lod.detail.last().unwrap(), 0.0);
        assert_eq!(
            positions,
            layout
                .files()
                .map(|(_, file)| file.position)
                .collect::<Vec<_>>()
        );
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
        layout.add("a/b/one.rs", 0.0, 0);
        lod.sample(&layout, 0.31, view(10000.0));
        assert_eq!(lod.detail, vec![1.0; lod.detail.len()]);
        for _ in 0..200 {
            layout.step(1.0 / 60.0);
        }
        lod.sample(&layout, 3.64, view(10000.0));
        assert_eq!(lod.glyph.iter().sum::<f32>(), 1.0);
    }

    #[test]
    fn hysteresis_keeps_the_previous_decision_inside_the_band() {
        let layout = layout();
        let mut lod = Lod::default();
        let mut camera = view(10000.0);
        lod.sample(&layout, 0.0, camera);
        let bounds = layout.files().fold(
            (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)),
            |(low, high), (_, file)| {
                (
                    low.min(file.position - Vec2::splat(0.4)),
                    high.max(file.position + Vec2::splat(0.4)),
                )
            },
        );
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
