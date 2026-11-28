use bevy::math::{Vec2, Vec3};

use super::{Simulation, Time};
use crate::layout::{Presence, packing::Tile};

const GLYPH_RATIO_GRACE: f32 = 2.5;
const RATIO: f32 = 2.0;
const GEOMETRY_EPSILON: f32 = 1e-5;

#[derive(Clone, Copy, Default)]
struct Repository {
    tile: Option<Tile>,
    last_motion: u64,
    settled: bool,
}

#[derive(Clone, Copy, Default)]
struct Directory {
    live: bool,
    position: Vec2,
    radius: f32,
}

#[derive(Default)]
pub(crate) struct GlyphScale {
    repositories: Vec<Repository>,
    directories: Vec<Directory>,
    tick: Option<u64>,
    maximum: f32,
    eye_height: f32,
    glyph_height: f32,
}

impl GlyphScale {
    pub(crate) fn sample(&mut self, sim: &Simulation) {
        let tick = sim.state.tick;
        if self.tick == Some(tick) {
            return;
        }
        if self.tick.is_some_and(|previous| tick < previous) {
            *self = Self::default();
        }
        self.tick = Some(tick);
        let layout = sim.layout();
        self.repositories
            .resize(layout.repositories.len(), Repository::default());
        self.directories
            .resize(layout.dir_count(), Directory::default());
        for (state, region) in self.repositories.iter_mut().zip(&layout.repositories) {
            if state.tile != region.tile {
                state.last_motion = tick;
                state.tile = region.tile;
            }
        }
        for (id, previous) in self.directories.iter_mut().enumerate() {
            let dir = layout.dir(id);
            let after = Directory {
                live: dir.live,
                position: dir.position,
                radius: dir.radius(),
            };
            if (previous.live != after.live
                || after.live
                    && (previous.position.distance_squared(after.position)
                        > GEOMETRY_EPSILON * GEOMETRY_EPSILON
                        || (previous.radius - after.radius).abs() > GEOMETRY_EPSILON))
                && let Some(state) = self.repositories.get_mut(dir.repository)
            {
                state.last_motion = tick;
            }
            *previous = after;
        }
        let grace = Time::seconds(GLYPH_RATIO_GRACE).ticks(true);
        for state in &mut self.repositories {
            state.settled =
                state.tile.is_some() && tick.saturating_sub(state.last_motion) > grace;
        }
        let minimum = layout
            .files()
            .filter_map(|(_, file)| {
                let dir = layout.dir(file.dir);
                (file.state != Presence::Leaving
                    && self.settled(dir.repository)
                    && dir.scale > 0.0)
                    .then_some(dir.scale)
            })
            .fold(f32::INFINITY, f32::min);
        let (focus, distance) = sim.world_view();
        self.eye_height = sim.field_space().to_planner(focus + Vec3::Y * distance).y;
        self.glyph_height = sim.theme().glyph_transform().translation.y;
        self.maximum = if minimum.is_finite() {
            // A larger glyph sits higher in the overview perspective.
            RATIO * minimum * self.eye_height
                / (self.eye_height + (RATIO - 1.0) * self.glyph_height * minimum)
        } else {
            f32::INFINITY
        };
    }

    pub(crate) fn settled(&self, repository: usize) -> bool {
        self.repositories
            .get(repository)
            .is_some_and(|state| state.settled)
    }

    #[cfg(test)]
    pub(super) fn last_motion(&self, repository: usize) -> u64 {
        self.repositories[repository].last_motion
    }

    pub(crate) fn scale(&self, repository: usize, nominal: f32) -> f32 {
        if self.settled(repository) {
            nominal.min(self.maximum)
        } else {
            nominal
        }
    }

    #[cfg(test)]
    pub(super) fn overview_width(&self, scale: f32) -> f32 {
        scale / (self.eye_height - self.glyph_height * scale)
    }
}
