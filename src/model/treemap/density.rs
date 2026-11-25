use serde::Serialize;

use super::{Repository, Simulation, framing};

const TOLERANCE: f64 = crate::layout::packing::DENSITY_TOLERANCE as f64;

#[derive(Serialize)]
pub(super) struct Sample {
    items_per_world_area: f64,
    relative_error: f64,
    eased_relative_error: Option<f64>,
}

pub(super) fn measure(repositories: &mut [Repository]) -> Option<f64> {
    let world_area = f64::from(framing::VIEWPORT.element_product())
        * f64::from(framing::WORLD_UNITS_PER_REFERENCE_PIXEL).powi(2);
    let active = |repo: &&Repository| repo.active && repo.tile_area > 0.0;
    let area: f64 = repositories
        .iter()
        .filter(active)
        .map(|repo| f64::from(repo.tile_area) * world_area)
        .sum();
    let items = |repo: &Repository| {
        f64::from(crate::model::count(repo.files + repo.directories))
    };
    let total_items: f64 = repositories.iter().filter(active).map(items).sum();
    let eased_items: f64 = repositories
        .iter()
        .filter(active)
        .map(|repo| f64::from(repo.eased_items))
        .sum();
    if area <= 0.0 || total_items <= 0.0 {
        return None;
    }
    let collection = total_items / area;
    for repo in repositories {
        if repo.active && repo.tile_area > 0.0 && items(repo) > 0.0 {
            let tile_area = f64::from(repo.tile_area) * world_area;
            let density = items(repo) / tile_area;
            repo.density = Some(Sample {
                items_per_world_area: density,
                relative_error: density / collection - 1.0,
                eased_relative_error: (eased_items > 0.0).then(|| {
                    f64::from(repo.eased_items) / tile_area / (eased_items / area) - 1.0
                }),
            });
        }
    }
    Some(collection)
}

pub(super) fn in_transition(sim: &Simulation) -> bool {
    let tick = sim.state.tick;
    let (tiles, _) = sim.clock.rows(tick);
    sim.births.iter().any(|&(_, ready, _)| {
        (ready.saturating_sub(super::super::BIRTH_TICKS + 1)..=ready).contains(&tick)
    }) || sim
        .repository_exits
        .iter()
        .flatten()
        .any(|exit| (exit.start..exit.end).contains(&tiles))
        || sim.packing.reflowing(tiles)
}

#[derive(Default)]
pub(super) struct Audit {
    pub report: Report,
    breaches: Vec<Option<Breach>>,
}

#[derive(Serialize)]
pub(super) struct Report {
    tolerance: f64,
    peak_settled: Option<Extremum>,
    peak_transition: Option<Extremum>,
    longest_breach: Option<Extremum>,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            tolerance: TOLERANCE,
            peak_settled: None,
            peak_transition: None,
            longest_breach: None,
        }
    }
}

#[derive(Serialize)]
struct Extremum {
    repository: usize,
    label: String,
    first_frame: u32,
    last_frame: u32,
    relative_error: f64,
    eased_relative_error: Option<f64>,
}

impl Extremum {
    fn new(
        repository: &Repository,
        sample: &Sample,
        first_frame: u32,
        last_frame: u32,
    ) -> Self {
        Self {
            repository: repository.index,
            label: repository.label.clone(),
            first_frame,
            last_frame,
            relative_error: sample.relative_error,
            eased_relative_error: sample.eased_relative_error,
        }
    }
}

#[derive(Clone, Copy)]
struct Breach {
    first: u32,
    last: u32,
    positive: bool,
}

impl Audit {
    pub(super) fn observe(
        &mut self,
        frame: u32,
        transition: bool,
        repositories: &[Repository],
    ) {
        for repo in repositories {
            let Some(sample) = &repo.density else {
                continue;
            };
            let peak = if transition {
                &mut self.report.peak_transition
            } else {
                &mut self.report.peak_settled
            };
            if peak.as_ref().is_none_or(|old| {
                sample.relative_error.abs() > old.relative_error.abs()
            }) {
                *peak = Some(Extremum::new(repo, sample, frame, frame));
            }
            if transition || sample.relative_error.abs() <= TOLERANCE {
                continue;
            }
            if self.breaches.len() <= repo.index {
                self.breaches.resize(repo.index + 1, None);
            }
            let positive = sample.relative_error > 0.0;
            let previous = &mut self.breaches[repo.index];
            let first = previous.map_or(frame, |before| {
                if before.last + 1 == frame && before.positive == positive {
                    before.first
                } else {
                    frame
                }
            });
            *previous = Some(Breach {
                first,
                last: frame,
                positive,
            });
            let longest = &mut self.report.longest_breach;
            if longest
                .as_ref()
                .is_none_or(|old| frame - first > old.last_frame - old.first_frame)
            {
                *longest = Some(Extremum::new(repo, sample, first, frame));
            }
        }
    }
}
