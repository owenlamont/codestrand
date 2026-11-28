use bevy::math::Vec3;

use super::{Simulation, Time};
use crate::layout::Presence;

const GLYPH_RATIO_GRACE: f32 = 2.5;
const RATIO: f32 = 2.0;
pub(super) const GLYPH_RATIO_MAX_EXEMPT: f32 = 10.0;

#[derive(Clone, Copy, Default)]
struct Repository {
    visible: bool,
    exemption: Option<Exemption>,
}

#[derive(Clone, Copy)]
struct Exemption {
    start: u64,
    last_placement: u64,
}

impl Repository {
    fn sample(&mut self, tick: u64, visible: bool, placing: bool, grace: u64) {
        if !visible {
            *self = Self::default();
        } else if placing || !self.visible {
            let episode = self.exemption.get_or_insert(Exemption {
                start: tick,
                last_placement: tick,
            });
            episode.last_placement = tick;
        } else if self
            .exemption
            .is_some_and(|episode| tick - episode.last_placement >= grace)
        {
            self.exemption = None;
        }
        self.visible = visible;
    }

    fn eligible(self, tick: u64, limit: u64) -> bool {
        self.visible
            && self
                .exemption
                .is_none_or(|episode| tick - episode.start >= limit)
    }
}

#[derive(Default)]
pub(crate) struct GlyphScale {
    repositories: Vec<Repository>,
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
        let grace = Time::seconds(GLYPH_RATIO_GRACE).ticks(true);
        let tile_tick = sim.clock.rows(tick).0;
        for (repo, (state, region)) in self
            .repositories
            .iter_mut()
            .zip(&layout.repositories)
            .enumerate()
        {
            let placing = sim
                .births
                .iter()
                .any(|&(birth, ready, _)| birth == repo && tick <= ready)
                || sim.packing.exchanging(tile_tick, repo);
            state.sample(tick, region.tile.is_some(), placing, grace);
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
        self.repositories.get(repository).is_some_and(|state| {
            state.eligible(
                self.tick.unwrap_or(0),
                Time::seconds(GLYPH_RATIO_MAX_EXEMPT).ticks(true),
            )
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_placement_loses_its_exemption_at_the_cap() {
        let grace = Time::seconds(GLYPH_RATIO_GRACE).ticks(true);
        let cap = Time::seconds(GLYPH_RATIO_MAX_EXEMPT).ticks(true);
        let mut repo = Repository::default();
        for tick in 0..=cap {
            repo.sample(tick, true, true, grace);
            assert_eq!(repo.eligible(tick, cap), tick == cap);
        }
        repo.sample(cap + grace, true, false, grace);
        repo.sample(cap + grace + 1, true, true, grace);
        assert!(!repo.eligible(cap + grace + 1, cap));
    }
}
