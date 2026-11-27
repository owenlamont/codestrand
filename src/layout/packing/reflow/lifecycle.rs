use super::{Layout, birth::Birth, held, opening, retirement::Retirement};
use crate::{
    layout::packing::{Packing, Tile, birth_presence},
    model::{BIRTH_TICKS, framing},
};

const MAX_OPERATIONS: usize = 192;
const MAX_SHIFTS: usize = 8;

#[derive(Debug)]
pub(in crate::layout::packing) struct Program {
    opening: Layout,
    phases: Vec<Phase>,
}

#[derive(Debug)]
struct Phase {
    start: usize,
    end: usize,
    action: Action,
}

#[derive(Debug)]
enum Action {
    Birth(Birth),
    Archive(Retirement),
    Shift { before: Layout, after: Layout },
}

impl Program {
    pub(in crate::layout::packing) fn retained_bytes(repositories: u128) -> u128 {
        let layout = repositories * (2 * std::mem::size_of::<Option<Tile>>()) as u128;
        (std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u128
            + layout
            + repositories * std::mem::size_of::<usize>() as u128
            + (MAX_OPERATIONS as u128).min(2 * repositories + MAX_SHIFTS as u128)
                * (std::mem::size_of::<Phase>() as u128 + 2 * layout)
    }

    pub(in crate::layout::packing) fn plan(
        packing: &Packing,
        windows: &[(usize, usize)],
    ) -> Option<Self> {
        let mut events = lifecycle_events(packing, windows)?;
        if events.is_empty() || events.len() > MAX_OPERATIONS {
            return None;
        }
        events.sort_unstable_by_key(|&(start, end, repo, birth)| {
            (if birth { start } else { end }, start, end, repo)
        });
        let base = packing.layout_at(0, &[], None, None);
        let preferred = opening::plan(packing, &base, |_| true);
        preferred.into_iter().chain([base]).find_map(|mut opening| {
            inset_field(packing, &mut opening)?;
            Self::compile(packing, opening, &events)
        })
    }

    fn compile(
        packing: &Packing,
        opening: Layout,
        events: &[(usize, usize, usize, bool)],
    ) -> Option<Self> {
        let mut current = opening.clone();
        let budget = (MAX_OPERATIONS - events.len()).min(MAX_SHIFTS);
        let mut phases = Vec::with_capacity(events.len() + budget);
        let mut shifts = packing
            .reflows
            .iter()
            .filter(|shift| {
                !events.iter().any(|&(start, end, _, _)| {
                    shift.tick < end && shift.tick + shift.duration() > start
                })
            })
            .take(budget)
            .peekable();
        let mut available = 0;
        let mut events = events.iter().copied().peekable();
        while let Some((start, end, repository, birth)) = events.next() {
            let requested = start;
            while shifts
                .peek()
                .is_some_and(|shift| shift.tick + shift.duration() <= start)
            {
                let shift = shifts.next().unwrap();
                insert_shift(packing, shift, available, &mut current, &mut phases);
            }
            if birth && start == 0 && current.tiles[repository].is_some() {
                continue;
            }
            if birth && available > start {
                return None;
            }
            let start = start.max(available);
            if start >= end {
                return None;
            }
            let ready = end;
            let end = if birth {
                events
                    .peek()
                    .filter(|&&(_, _, _, birth)| birth)
                    .map_or(end, |&(next_start, _, _, _)| end.min(next_start))
            } else {
                end
            };
            let action = if birth {
                if end <= start {
                    return None;
                }
                let row = packing.samples.get(ready)?;
                Action::Birth(Birth::plan(
                    &current,
                    packing.space(),
                    repository,
                    &row.weights,
                    end - start,
                )?)
            } else {
                packing.samples.get(end)?;
                let fit = opening::glyph_scales(packing, start);
                let mut departing = vec![repository];
                while events.peek().is_some_and(
                    |&(other_start, other_end, _, birth)| {
                        !birth && (other_start, other_end) == (requested, end)
                    },
                ) {
                    departing.push(events.next().unwrap().2);
                }
                let motion = if departing.len() == 1 {
                    Retirement::plan(
                        current.clone(),
                        repository,
                        packing.space(),
                        end - start,
                        fit,
                    )?
                } else {
                    Retirement::together(
                        &current,
                        &departing,
                        packing.space(),
                        end - start,
                        fit,
                    )?
                };
                let motion = if events.peek().is_none() {
                    motion.finish_in_field(packing.space(), end - start)?
                } else {
                    motion
                };
                Action::Archive(motion)
            };
            current = action.sample(1.0);
            available = end;
            phases.push(Phase { start, end, action });
        }
        for shift in shifts {
            insert_shift(packing, shift, available, &mut current, &mut phases);
        }
        Some(Self { opening, phases })
    }

    #[cfg(test)]
    pub(in crate::layout::packing) fn field(&self) -> Tile {
        super::retirement::gutter_bounds(&self.opening).unwrap()
    }

    #[cfg(test)]
    pub(in crate::layout::packing) fn counts(&self) -> (usize, usize) {
        self.phases
            .iter()
            .fold((0, 0), |(births, archives), phase| match phase.action {
                Action::Birth(_) => (births + 1, archives),
                Action::Archive(ref archive) => (births, archives + archive.count()),
                Action::Shift { .. } => (births, archives),
            })
    }

    pub(in crate::layout::packing) fn sample(
        &self,
        tick: usize,
        births: &[(usize, u64)],
    ) -> Layout {
        let index = self.phases.partition_point(|phase| phase.start <= tick);
        if let Some((birth, elapsed)) = self.overridden_birth(tick, index, births) {
            return birth.sample(birth_presence(elapsed));
        }
        let Some(phase) = self.phases[..index].last() else {
            return self.opening.clone();
        };
        let progress = if tick >= phase.end {
            1.0
        } else {
            let elapsed = tick.saturating_sub(phase.start);
            let duration = phase.end - phase.start;
            match phase.action {
                Action::Birth(_) => held::birth_ease(elapsed, duration),
                Action::Archive(_) | Action::Shift { .. } => {
                    held::progress(elapsed, duration)
                }
            }
        };
        phase.action.sample(progress)
    }
    fn overridden_birth(
        &self,
        tick: usize,
        next: usize,
        births: &[(usize, u64)],
    ) -> Option<(&Birth, u64)> {
        if births.is_empty() {
            return None;
        }
        self.phases.iter().enumerate().find_map(|(index, phase)| {
            let Action::Birth(birth) = &phase.action else {
                return None;
            };
            let &(_, elapsed) =
                births.iter().find(|&&(repo, _)| repo == birth.guest)?;
            let early = index == next
                && elapsed > 0
                && self.phases[..next]
                    .last()
                    .is_none_or(|prior| tick >= prior.end);
            let delayed =
                index + 1 == next && (elapsed < BIRTH_TICKS || tick < phase.end);
            (early || delayed).then_some((birth, elapsed))
        })
    }
}

impl Action {
    fn sample(&self, progress: f32) -> Layout {
        match self {
            Self::Birth(birth) => birth.sample(progress),
            Self::Archive(archive) => archive.sample(1.0 - progress),
            Self::Shift { before, after } => {
                if progress >= 1.0 {
                    after.clone()
                } else {
                    before.blend_edges(after, progress)
                }
            }
        }
    }
}

fn lifecycle_events(
    packing: &Packing,
    windows: &[(usize, usize)],
) -> Option<Vec<(usize, usize, usize, bool)>> {
    let mut events = Vec::new();
    for (repo, &(start, end)) in windows.iter().enumerate() {
        if start < end {
            events.push((start, end, repo, true));
        }
    }
    for (repo, &exit) in packing.exits.iter().enumerate() {
        if let Some(exit) = exit
            && exit.start < exit.end
        {
            events.push((
                usize::try_from(exit.start).ok()?,
                usize::try_from(exit.end).ok()?,
                repo,
                false,
            ));
        }
    }
    Some(events)
}

fn insert_shift(
    packing: &Packing,
    shift: &super::Change,
    available: usize,
    current: &mut Layout,
    phases: &mut Vec<Phase>,
) {
    let start = shift.tick;
    let end = (start + shift.duration()).min(packing.samples.len() - 1);
    if start < available
        || end <= start
        || phases.last().is_some_and(|phase| phase.end > start)
    {
        return;
    }
    let Some(after) = opening::plan_shift(packing, current, end, end - start) else {
        return;
    };
    let before = std::mem::replace(current, after.clone());
    phases.push(Phase {
        start,
        end,
        action: Action::Shift { before, after },
    });
}

fn inset_field(packing: &Packing, layout: &mut Layout) -> Option<()> {
    let outer = super::retirement::gutter_bounds(layout)?;
    let field = packing.space().field(framing::MARGIN);
    let scale = ((field.high - field.low) / (outer.high - outer.low)).min_element();
    let origin = outer.low.midpoint(outer.high);
    let centre = field.low.midpoint(field.high);
    for rect in layout.tiles.iter_mut().chain(&mut layout.gutters).flatten() {
        rect.low = (centre + (rect.low - origin) * scale).max(field.low);
        rect.high = (centre + (rect.high - origin) * scale).min(field.high);
    }
    Some(())
}
