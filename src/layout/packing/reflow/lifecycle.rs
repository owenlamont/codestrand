use super::{Layout, birth::Birth, held, opening, retirement::Retirement};
use crate::{
    layout::packing::{Packing, Tile, birth_presence},
    model::{BIRTH_TICKS, framing},
};

const MAX_OPERATIONS: usize = 192;

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
    pub(in crate::layout::packing) fn retained_bytes(
        repositories: u128,
        ticks: u128,
    ) -> u128 {
        let layout = repositories * (2 * std::mem::size_of::<Option<Tile>>()) as u128;
        (std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u128
            + layout
            + repositories * std::mem::size_of::<usize>() as u128
            + ((MAX_OPERATIONS as u128).min(2 * repositories)
                + ticks / held::reflow_ticks() as u128)
                * (std::mem::size_of::<Phase>() as u128 + 2 * layout)
    }

    pub(in crate::layout::packing) fn plan(
        packing: &Packing,
        windows: &[(usize, usize)],
        live_files: &[Vec<f32>],
    ) -> Option<Self> {
        let mut events = lifecycle_events(packing, windows)?;
        if packing.labels.len() == 1 {
            let space = packing.space();
            let opening = Layout {
                tiles: vec![Some(space.field(framing::MARGIN))],
                gutters: vec![Some(space.field(super::super::GUTTER_MARGIN))],
            };
            return Self::compile(packing, opening, &events, live_files, false);
        }
        if events.len() > MAX_OPERATIONS
            || (events.is_empty()
                && live_files.windows(2).all(|rows| rows[0] == rows[1]))
        {
            return None;
        }
        events.sort_unstable_by_key(|&(start, end, repo, birth)| {
            (if birth { start } else { end }, start, end, repo)
        });
        let base = packing.layout_at(0, &[], None, None);
        let preferred = opening::plan(packing, &base, |_| true);
        preferred.into_iter().chain([base]).find_map(|mut opening| {
            inset_field(packing, &mut opening)?;
            Self::compile(packing, opening.clone(), &events, live_files, false)
                .or_else(|| Self::compile(packing, opening, &events, live_files, true))
        })
    }

    fn compile(
        packing: &Packing,
        opening: Layout,
        events: &[(usize, usize, usize, bool)],
        live_files: &[Vec<f32>],
        share_archive_windows: bool,
    ) -> Option<Self> {
        let mut current = opening.clone();
        let mut phases = Vec::with_capacity(events.len());
        let mut available = 0;
        let mut events = events.iter().copied().peekable();
        while let Some((start, end, repository, birth)) = events.next() {
            let requested = start;
            insert_shifts(
                packing,
                live_files,
                available..start,
                &mut current,
                &mut phases,
            );
            if birth && start == 0 && current.tiles[repository].is_some() {
                continue;
            }
            if birth && available > start {
                return None;
            }
            let mut start = start.max(available);
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
                let departing;
                (start, departing) = archive_members(
                    &mut events,
                    (requested, end, repository),
                    start,
                    share_archive_windows,
                );
                let fit = opening::glyph_scales(packing, start);
                let motion = if packing.labels.len() == 1 {
                    Retirement::close_last(&current, repository, end - start)?
                } else if departing.len() == 1 {
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
                    motion.finish_in_field(packing.space(), end - start)
                } else {
                    motion
                };
                Action::Archive(motion)
            };
            current = action.sample(1.0);
            available = end;
            phases.push(Phase { start, end, action });
        }
        insert_shifts(
            packing,
            live_files,
            available..live_files.len() - 1,
            &mut current,
            &mut phases,
        );
        Some(Self { opening, phases })
    }

    pub(in crate::layout::packing) fn shifts(
        &self,
    ) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.phases
            .iter()
            .filter(|phase| matches!(phase.action, Action::Shift { .. }))
            .map(|phase| {
                (
                    u64::try_from(phase.start).expect("bounded shift tick"),
                    u64::try_from(phase.end).expect("bounded shift tick"),
                )
            })
    }

    pub(in crate::layout::packing) fn shifting(&self, tick: usize) -> bool {
        let index = self.phases.partition_point(|phase| phase.start <= tick);
        self.phases[..index].last().is_some_and(|phase| {
            tick < phase.end && matches!(phase.action, Action::Shift { .. })
        })
    }

    #[cfg(test)]
    pub(in crate::layout::packing) fn field(&self) -> Tile {
        super::retirement::gutter_bounds(&self.opening).unwrap()
    }

    #[cfg(test)]
    pub(in crate::layout::packing) fn settled_glass_error_px(
        &self,
        space: super::Space,
    ) -> f32 {
        std::iter::once(self.opening.clone())
            .chain(self.phases.iter().map(|phase| phase.action.sample(1.0)))
            .map(|layout| settled_glass_error_px(&self.opening, &layout, space))
            .fold(0.0, f32::max)
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

fn archive_members(
    events: &mut std::iter::Peekable<impl Iterator<Item = (usize, usize, usize, bool)>>,
    (requested, end, repository): (usize, usize, usize),
    mut start: usize,
    share_windows: bool,
) -> (usize, Vec<usize>) {
    let mut departing = vec![repository];
    while events
        .peek()
        .is_some_and(|&(other_start, other_end, _, birth)| {
            let overlap = end.saturating_sub(start.max(other_start));
            !birth
                && ((other_start, other_end) == (requested, end)
                    || (share_windows && overlap > 0 && overlap >= other_end - end))
        })
    {
        let (other_start, _, other_repository, _) = events.next().unwrap();
        start = start.max(other_start);
        departing.push(other_repository);
    }
    (start, departing)
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

fn insert_shifts(
    packing: &Packing,
    live_files: &[Vec<f32>],
    window: std::ops::Range<usize>,
    current: &mut Layout,
    phases: &mut Vec<Phase>,
) {
    let duration = held::reflow_ticks();
    let mut rejected = None;
    let mut start = window.start;
    while window.end.saturating_sub(start) >= duration {
        let end = start + duration;
        let weights = &live_files[end];
        if rejected != Some(weights) {
            if let Some(after) =
                opening::plan_shift(packing, current, weights, duration)
            {
                let before = std::mem::replace(current, after.clone());
                phases.push(Phase {
                    start,
                    end,
                    action: Action::Shift { before, after },
                });
                rejected = None;
            } else {
                rejected = Some(weights);
            }
        }
        start = end;
    }
}

fn inset_field(packing: &Packing, layout: &mut Layout) -> Option<()> {
    use super::retirement::{gutter_bounds, rectangle_bounds, settled};
    use bevy::math::Vec2;

    let outer = gutter_bounds(layout)?;
    let glass = rectangle_bounds(&layout.tiles)?;
    let space = packing.space();
    let field = space.field(framing::MARGIN);
    let scale = ((field.high - field.low) / (outer.high - outer.low)).min_element();
    let origin = outer.low.midpoint(outer.high);
    let centre = field.low.midpoint(field.high);
    let inset = |rect: Tile| Tile {
        low: (centre + (rect.low - origin) * scale).max(field.low),
        high: (centre + (rect.high - origin) * scale).min(field.high),
    };
    let (outer, glass) = (inset(outer), inset(glass));
    let gap = Vec2::splat(space.gap) / framing::VIEWPORT;
    for (tile, cell) in layout.tiles.iter_mut().zip(&mut layout.gutters) {
        *cell = cell.map(inset);
        *tile = cell.map(|cell| glass_in_cell(cell, outer, glass, gap));
    }
    layout
        .tiles
        .iter()
        .flatten()
        .copied()
        .all(settled)
        .then_some(())
}

fn glass_in_cell(cell: Tile, field: Tile, glass: Tile, gap: bevy::math::Vec2) -> Tile {
    use bevy::math::Vec2;

    Tile {
        low: Vec2::select(cell.low.cmpeq(field.low), glass.low, cell.low + gap),
        high: Vec2::select(cell.high.cmpeq(field.high), glass.high, cell.high - gap),
    }
}

#[cfg(test)]
pub(in crate::layout::packing) fn settled_glass_error_px(
    before: &Layout,
    after: &Layout,
    space: super::Space,
) -> f32 {
    use super::retirement::{fills_field, gutter_bounds, rectangle_bounds};
    use bevy::math::Vec2;

    let Some(field) = gutter_bounds(after) else {
        return if after.tiles.iter().any(Option::is_some) {
            f32::INFINITY
        } else {
            0.0
        };
    };
    if !fills_field(after, field) {
        return f32::INFINITY;
    }
    let visible = if Some(field) == gutter_bounds(before) {
        rectangle_bounds(&before.tiles).unwrap()
    } else if field == space.field(super::super::GUTTER_MARGIN) {
        space.field(framing::MARGIN)
    } else {
        return f32::INFINITY;
    };
    let padding = Vec2::splat(space.gap) / framing::VIEWPORT;
    let pixels = framing::VIEWPORT * 0.5;
    after
        .tiles
        .iter()
        .zip(&after.gutters)
        .map(|(&glass, &cell)| match (glass, cell) {
            (Some(glass), Some(cell)) => {
                let expected = glass_in_cell(cell, field, visible, padding);
                let error = (glass.low - expected.low)
                    .abs()
                    .max((glass.high - expected.high).abs());
                (error * pixels).max_element()
            }
            (None, None) => 0.0,
            _ => f32::INFINITY,
        })
        .fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{RepositoryRegion, outline, packing::Inputs};
    use bevy::math::Vec2;

    #[test]
    fn flat_allocation_catches_up_after_a_late_bulk_import() {
        const BIRTH: usize = 1_120;
        const IMPORT: usize = 1_400;
        const END: usize = 2_600;
        let mut inputs = Inputs::new(END + 1);
        for tick in 0..=END {
            inputs.push(
                [25.0, 25.0, if tick < IMPORT { 1.0 } else { 100.0 }]
                    .into_iter()
                    .enumerate()
                    .map(|(repo, items)| RepositoryRegion {
                        label: format!("repo {repo}"),
                        low: -Vec2::ONE,
                        high: Vec2::ONE,
                        live: repo < 2 || tick > BIRTH,
                        items,
                        tile: None,
                        gutter: None,
                        outline: outline::rectangle(-Vec2::ONE, Vec2::ONE),
                        height: 0.1,
                        rotated: false,
                        stretch: Vec2::ONE,
                    })
                    .collect(),
            );
        }
        let packing =
            Packing::from_inputs(inputs, vec![None; 3].into(), 4.0, 1.0, 1).unwrap();
        let mut opening = packing.layout_at(0, &[], None, None);
        inset_field(&packing, &mut opening).unwrap();
        let events = [(BIRTH - 90, BIRTH, 2, true)];
        let live_files: Vec<_> = (0..=END)
            .map(|tick| vec![25.0, 25.0, if tick < IMPORT { 1.0 } else { 100.0 }])
            .collect();
        let plan =
            Program::compile(&packing, opening, &events, &live_files, false).unwrap();
        let arrival = plan.sample(BIRTH, &[]);
        let final_layout = plan.sample(END, &[]);
        let share = |layout: Layout| {
            let area = layout.tiles[2].unwrap().area();
            area / layout
                .tiles
                .into_iter()
                .flatten()
                .map(Tile::area)
                .sum::<f32>()
        };
        let initial = share(arrival);
        assert!(initial < 0.28, "initial share {initial:.3}");
        let actual = share(final_layout.clone());
        assert!(
            (actual - 2.0 / 3.0).abs() < 0.05,
            "import share {actual:.3}, final {final_layout:?}"
        );
    }

    #[test]
    fn nearly_simultaneous_archives_can_share_their_overlapping_window() {
        let mut inputs = Inputs::new(264);
        for _ in 0..264 {
            inputs.push(
                (0..3)
                    .map(|index| RepositoryRegion {
                        label: format!("repository {index}"),
                        low: -Vec2::ONE,
                        high: Vec2::ONE,
                        live: true,
                        items: 20.0,
                        tile: None,
                        gutter: None,
                        outline: outline::rectangle(-Vec2::ONE, Vec2::ONE),
                        height: 0.1,
                        rotated: false,
                        stretch: Vec2::ONE,
                    })
                    .collect(),
            );
        }
        let packing =
            Packing::from_inputs(inputs, vec![None; 3].into(), 4.0, 1.0, 1).unwrap();
        let gap = Vec2::splat(4.0) / framing::VIEWPORT;
        let edges = [-0.9, -0.3, 0.3, 0.9];
        let cells: Vec<_> = edges
            .windows(2)
            .map(|pair| Tile {
                low: Vec2::new(pair[0], -0.6),
                high: Vec2::new(pair[1], 0.6),
            })
            .collect();
        let opening = Layout {
            tiles: cells
                .iter()
                .map(|cell| {
                    Some(Tile {
                        low: cell.low + gap,
                        high: cell.high - gap,
                    })
                })
                .collect(),
            gutters: cells.into_iter().map(Some).collect(),
        };
        let events = [(20, 260, 0, false), (23, 263, 2, false)];
        let live_files = vec![vec![20.0; 3]; 264];
        assert!(
            Program::compile(&packing, opening.clone(), &events, &live_files, false)
                .is_none()
        );
        let plan =
            Program::compile(&packing, opening, &events, &live_files, true).unwrap();
        assert_eq!(plan.counts(), (0, 2));
        assert_eq!(plan.phases.len(), 1);
        let phase = &plan.phases[0];
        assert_eq!((phase.start, phase.end), (23, 260));
        assert!(plan.sample(259, &[]).tiles.iter().all(Option::is_some));
        assert_eq!(
            plan.sample(260, &[])
                .tiles
                .iter()
                .filter(|tile| tile.is_some())
                .count(),
            1
        );
    }
}
