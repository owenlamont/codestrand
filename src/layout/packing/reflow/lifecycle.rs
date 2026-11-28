use super::{
    Layout,
    birth::{Birth, BirthBatch},
    held, opening,
    retirement::Retirement,
    swap::Swap,
};
use crate::{
    layout::packing::{Packing, Tile, birth_presence},
    model::{BIRTH_TICKS, framing},
};

#[derive(Debug)]
pub(in crate::layout::packing) struct Program {
    opening: Layout,
    phases: Vec<Phase>,
}

#[derive(Clone, Copy)]
pub(in crate::layout::packing) struct AspectEpisode {
    pub repository: usize,
    pub start: usize,
    pub end: usize,
    pub peak: f32,
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
    Births(BirthBatch),
    Archive(Retirement),
    Shift { before: Layout, after: Layout },
    Swap(Box<Swap>),
}

#[derive(Clone, Copy)]
enum CompilePolicy {
    Serial,
    SharedArchives,
    SharedLifecycle,
    JointBirths,
    ArchiveAspect,
    JointBirthsWithArchiveAspect,
}

impl Program {
    pub(in crate::layout::packing) fn stationary(opening: Layout) -> Self {
        Self {
            opening,
            phases: Vec::new(),
        }
    }

    pub(in crate::layout::packing) fn retained_bytes(
        repositories: u128,
        ticks: u128,
    ) -> u128 {
        let layout = repositories * (2 * std::mem::size_of::<Option<Tile>>()) as u128;
        (std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u128
            + layout
            + repositories * std::mem::size_of::<usize>() as u128
            + (2 * repositories + ticks / held::reflow_ticks() as u128)
                * (std::mem::size_of::<Phase>() as u128 + 2 * layout)
            + (ticks / held::reflow_ticks() as u128)
                * std::mem::size_of::<Swap>() as u128
    }

    pub(in crate::layout::packing) fn plan(
        packing: &Packing,
        mut opening: Layout,
        windows: &[(usize, usize)],
        live_files: &[Vec<f32>],
    ) -> Option<Self> {
        let mut events = lifecycle_events(packing, windows)?;
        if events.is_empty() && live_files.windows(2).all(|rows| rows[0] == rows[1]) {
            return Some(Self::stationary(opening));
        }
        events.sort_unstable_by_key(|&(start, end, repo, birth)| {
            (if birth { start } else { end }, start, end, repo)
        });
        let mut inset = opening.clone();
        let fields = if packing.labels.len() > 1 {
            [
                clip_gutters(packing, &mut opening).map(|()| opening),
                inset_field(packing, &mut inset).map(|()| inset),
            ]
        } else {
            [Some(opening), None]
        };
        fields.into_iter().flatten().find_map(|opening| {
            [
                CompilePolicy::Serial,
                CompilePolicy::SharedArchives,
                CompilePolicy::SharedLifecycle,
                CompilePolicy::JointBirths,
                CompilePolicy::ArchiveAspect,
                CompilePolicy::JointBirthsWithArchiveAspect,
            ]
            .into_iter()
            .find_map(|policy| {
                Self::compile(packing, opening.clone(), &events, live_files, policy)
            })
        })
    }

    fn compile(
        packing: &Packing,
        opening: Layout,
        events: &[(usize, usize, usize, bool)],
        live_files: &[Vec<f32>],
        policy: CompilePolicy,
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
            let overlap = matches!(
                policy,
                CompilePolicy::SharedLifecycle
                    | CompilePolicy::JointBirths
                    | CompilePolicy::ArchiveAspect
                    | CompilePolicy::JointBirthsWithArchiveAspect
            );
            if birth && available > start && !overlap {
                return None;
            }
            let start = start.max(available);
            if start >= end {
                return None;
            }
            if birth
                && overlap
                && events
                    .peek()
                    .is_some_and(|&(next, _, _, birth)| birth && next < end)
            {
                let phase = batch_births(
                    packing,
                    current,
                    &mut events,
                    (start, end, repository),
                    policy,
                )?;
                current = phase.action.sample(1.0);
                available = phase.end;
                phases.push(phase);
                continue;
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
            let phase = if birth {
                if end <= start {
                    return None;
                }
                let row = packing.samples.get(ready)?;
                let action = Action::Birth(Birth::plan(
                    &current,
                    packing.space(),
                    repository,
                    &row.weights,
                    end - start,
                )?);
                Phase { start, end, action }
            } else {
                archive_phase(
                    packing,
                    &current,
                    &mut events,
                    (requested, end, repository),
                    start,
                    policy,
                )?
            };
            current = phase.action.sample(1.0);
            available = phase.end;
            phases.push(phase);
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
            .filter(|phase| {
                matches!(phase.action, Action::Shift { .. } | Action::Swap(_))
            })
            .map(|phase| {
                (
                    u64::try_from(phase.start).expect("bounded shift tick"),
                    u64::try_from(phase.end).expect("bounded shift tick"),
                )
            })
    }

    pub(in crate::layout::packing) fn exchanges(
        &self,
        tick: usize,
        repository: usize,
    ) -> bool {
        let index = self.phases.partition_point(|phase| phase.start <= tick);
        self.phases[..index].last().is_some_and(|phase| {
            tick < phase.end && matches!(&phase.action, Action::Swap(swap) if swap.repositories().contains(&repository))
        })
    }

    pub(in crate::layout::packing) fn swap_gaps(
        &self,
    ) -> impl Iterator<Item = (usize, usize, [usize; 2], f32, usize)> + '_ {
        self.phases.iter().filter_map(|phase| {
            let Action::Swap(swap) = &phase.action else {
                return None;
            };
            Some((
                phase.start,
                phase.end,
                swap.repositories(),
                swap.peak_gap,
                swap.uncovered_ticks,
            ))
        })
    }

    pub(in crate::layout::packing) fn last_tick(&self) -> usize {
        self.phases.last().map_or(0, |phase| phase.end)
    }

    pub(in crate::layout::packing) fn aspect_episodes(
        &self,
        end: usize,
    ) -> Vec<AspectEpisode> {
        let mut eligible = vec![false; self.opening.tiles.len()];
        let mut active = vec![None::<AspectEpisode>; eligible.len()];
        let mut episodes = Vec::new();
        for phase in self.phases.iter().take_while(|phase| phase.start <= end) {
            if let Action::Archive(archive) = &phase.action {
                episodes.extend(archive.departing().iter().filter_map(|&repo| {
                    eligible[repo] = false;
                    active[repo].take().map(|entry| AspectEpisode {
                        end: phase.start,
                        ..entry
                    })
                }));
                for repo in archive.over_cap_survivors() {
                    eligible[repo] = true;
                }
            }
            if !eligible.iter().any(|&tracked| tracked) {
                continue;
            }
            for tick in phase.start..=phase.end.min(end) {
                observe_aspects(
                    &self.sample(tick, &[]),
                    tick,
                    end,
                    &mut eligible,
                    &mut active,
                    &mut episodes,
                );
            }
        }
        episodes.extend(active.into_iter().flatten());
        episodes
    }

    pub(in crate::layout::packing) fn shifting(&self, tick: usize) -> bool {
        let index = self.phases.partition_point(|phase| phase.start <= tick);
        self.phases[..index].last().is_some_and(|phase| {
            tick < phase.end
                && matches!(phase.action, Action::Shift { .. } | Action::Swap(_))
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
                Action::Births(ref batch) => (births + batch.count(), archives),
                Action::Archive(ref archive) => (births, archives + archive.count()),
                Action::Shift { .. } | Action::Swap(_) => (births, archives),
            })
    }

    pub(in crate::layout::packing) fn sample(
        &self,
        tick: usize,
        births: &[(usize, u64)],
    ) -> Layout {
        let index = self.phases.partition_point(|phase| phase.start <= tick);
        if !births.is_empty()
            && let Some(batch) = self.phases[..index].iter().find_map(|phase| {
                let Action::Births(batch) = &phase.action else {
                    return None;
                };
                batch
                    .has_override(births, |elapsed| elapsed < BIRTH_TICKS)
                    .then_some(batch)
            })
        {
            return batch.sample(tick, births);
        }
        if let Some(Phase {
            action: Action::Births(batch),
            ..
        }) = self.phases.get(index)
            && self.phases[..index]
                .last()
                .is_none_or(|prior| tick >= prior.end)
            && batch.has_override(births, |elapsed| elapsed > 0)
        {
            return batch.sample(tick, births);
        }
        if let Some((birth, elapsed)) = self.overridden_birth(tick, index, births) {
            return birth.sample(birth_presence(elapsed));
        }
        let Some(phase) = self.phases[..index].last() else {
            return self.opening.clone();
        };
        if let Action::Births(batch) = &phase.action {
            return if tick >= phase.end && births.is_empty() {
                batch.settled()
            } else {
                batch.sample(tick, births)
            };
        }
        if let Action::Swap(swap) = &phase.action {
            return swap.sample(tick - phase.start);
        }
        let progress = if tick >= phase.end {
            1.0
        } else {
            let elapsed = tick.saturating_sub(phase.start);
            let duration = phase.end - phase.start;
            match phase.action {
                Action::Birth(_) => held::birth_ease(elapsed, duration),
                Action::Births(_) | Action::Swap(_) => {
                    unreachable!("sampled at its own phase ticks")
                }
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

fn observe_aspects(
    layout: &Layout,
    tick: usize,
    end: usize,
    eligible: &mut [bool],
    active: &mut [Option<AspectEpisode>],
    episodes: &mut Vec<AspectEpisode>,
) {
    for (repo, (tracked, entry)) in eligible.iter_mut().zip(active).enumerate() {
        if !*tracked {
            continue;
        }
        let peak = layout.tiles[repo].map_or(1.0, |tile| {
            let aspect = tile.ground_aspect();
            aspect.max(aspect.recip())
        });
        if peak > super::super::BOUND + 1e-6 {
            let entry = entry.get_or_insert(AspectEpisode {
                repository: repo,
                start: tick,
                end,
                peak,
            });
            entry.peak = entry.peak.max(peak);
        } else if let Some(finished) = entry.take() {
            episodes.push(AspectEpisode {
                end: tick,
                ..finished
            });
            *tracked = false;
        }
    }
}

fn batch_births(
    packing: &Packing,
    opening: Layout,
    events: &mut std::iter::Peekable<
        impl Iterator<Item = (usize, usize, usize, bool)> + Clone,
    >,
    (start, mut end, repository): (usize, usize, usize),
    policy: CompilePolicy,
) -> Option<Phase> {
    let mut group = vec![(start, end, repository)];
    let mut probe = (*events).clone();
    while probe
        .peek()
        .is_some_and(|&(next, _, _, birth)| birth && next < end)
    {
        let (begin, ready, guest, _) = probe.next()?;
        end = end.max(ready);
        group.push((begin.max(start), ready, guest));
    }
    let weights = |tick: usize| {
        packing
            .samples
            .get(tick)
            .map(|sample| sample.weights.as_slice())
    };
    let space = packing.space();
    let (batch, consumed, end) = if matches!(
        policy,
        CompilePolicy::JointBirths | CompilePolicy::JointBirthsWithArchiveAspect
    ) {
        BirthBatch::plan(opening.clone(), space, &group, weights, false)
            .filter(|&(_, consumed, _)| consumed == group.len())
            .or_else(|| {
                BirthBatch::plan_together(opening.clone(), space, &group, weights)
            })
            .or_else(|| {
                BirthBatch::plan(opening.clone(), space, &group, weights, true)
                    .filter(|&(_, consumed, _)| consumed == group.len())
            })?
    } else {
        BirthBatch::plan(opening, space, &group, weights, false)?
    };
    for _ in 1..consumed {
        events.next()?;
    }
    Some(Phase {
        start,
        end,
        action: Action::Births(batch),
    })
}

fn archive_phase(
    packing: &Packing,
    current: &Layout,
    events: &mut std::iter::Peekable<impl Iterator<Item = (usize, usize, usize, bool)>>,
    (requested, end, repository): (usize, usize, usize),
    start: usize,
    policy: CompilePolicy,
) -> Option<Phase> {
    let (start, departing) = archive_members(
        events,
        (requested, end, repository),
        start,
        !matches!(policy, CompilePolicy::Serial),
    );
    let space = packing.space();
    let ticks = end - start;
    let fit = opening::glyph_scales(packing, start.min(packing.samples.len() - 1));
    let motion = if current.tiles.iter().flatten().count() == departing.len() {
        Retirement::close_all(current, &departing, ticks)?
    } else if departing.len() == 1 {
        Retirement::plan(current.clone(), repository, space, ticks, fit).or_else(
            || {
                matches!(
                    policy,
                    CompilePolicy::ArchiveAspect
                        | CompilePolicy::JointBirthsWithArchiveAspect
                )
                .then(|| Retirement::lowest_peak(current, repository, space, ticks))
                .flatten()
            },
        )?
    } else {
        Retirement::together(current, &departing, space, ticks, fit).or_else(|| {
            matches!(
                policy,
                CompilePolicy::ArchiveAspect
                    | CompilePolicy::JointBirthsWithArchiveAspect
            )
            .then(|| {
                Retirement::together_lowest_peak(current, &departing, space, ticks)
            })
            .flatten()
        })?
    };
    let motion = if events.peek().is_none() {
        motion.finish_in_field(space, ticks)
    } else {
        motion
    };
    Some(Phase {
        start,
        end,
        action: Action::Archive(motion),
    })
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
            Self::Births(batch) => batch.settled(),
            Self::Swap(swap) => swap.settled(),
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
            if let Some(action) = plan_shift(packing, current, weights, duration) {
                *current = action.sample(1.0);
                phases.push(Phase { start, end, action });
                rejected = None;
            } else {
                rejected = Some(weights);
            }
        }
        start = end;
    }
}

fn plan_shift(
    packing: &Packing,
    current: &Layout,
    weights: &[f32],
    duration: usize,
) -> Option<Action> {
    let after = opening::plan_shift(packing, current, weights, duration);
    if let Some(swap) = Swap::plan(
        current,
        after.as_ref().unwrap_or(current),
        packing.space(),
        weights,
    ) {
        return Some(Action::Swap(Box::new(swap)));
    }
    after.map(|after| Action::Shift {
        before: current.clone(),
        after,
    })
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

fn clip_gutters(packing: &Packing, layout: &mut Layout) -> Option<()> {
    use super::retirement::{contained, fills_field, settled};
    use bevy::math::Vec2;
    let space = packing.space();
    let field = space.field(framing::MARGIN);
    let padding = Vec2::splat(space.gap) / framing::VIEWPORT;
    for (tile, cell) in layout.tiles.iter_mut().zip(&mut layout.gutters) {
        *cell = cell.map(|cell| Tile {
            low: cell.low.max(field.low),
            high: cell.high.min(field.high),
        });
        *tile = cell.map(|cell| glass_in_cell(cell, field, field, padding));
    }
    (fills_field(layout, field)
        && contained(layout)
        && layout.tiles.iter().flatten().copied().all(settled))
    .then_some(())
}

pub(super) fn glass_in_cell(
    cell: Tile,
    field: Tile,
    glass: Tile,
    gap: bevy::math::Vec2,
) -> Tile {
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
    use crate::layout::{
        RepositoryRegion, outline,
        packing::{Input, Inputs, Sample, Space},
    };
    use bevy::math::Vec2;

    #[test]
    fn left_tile_exits_beside_two_stacked_tiles() {
        let half = Vec2::new(800.0, 400.0) / framing::VIEWPORT;
        let field = Tile {
            low: -half,
            high: half,
        };
        let cells = [
            Tile {
                high: Vec2::new(0.0, half.y),
                ..field
            },
            Tile {
                low: Vec2::new(0.0, -half.y),
                high: Vec2::new(half.x, 0.0),
            },
            Tile {
                low: Vec2::ZERO,
                ..field
            },
        ];
        let opening = Layout {
            tiles: cells.map(Some).into(),
            gutters: cells.map(Some).into(),
        };
        let input = Input {
            low: -Vec2::ONE,
            high: Vec2::ONE,
            outline: outline::rectangle(-Vec2::ONE, Vec2::ONE).into_boxed_slice(),
            height: 0.1,
            live: true,
        };
        let counts = vec![1.0; 3];
        let packing = Packing {
            growth: 1.0,
            inputs: vec![vec![input; 3]].into(),
            samples: vec![Sample {
                weights: counts.clone(),
                presence: counts.clone(),
            }]
            .into(),
            rotated: vec![false; 3],
            ..Packing::default()
        };
        let plan = Program::compile(
            &packing,
            opening,
            &[(0, 240, 0, false)],
            &vec![counts; 241],
            CompilePolicy::ArchiveAspect,
        )
        .unwrap();
        assert_eq!(plan.counts(), (0, 1));
        assert!(plan.sample(120, &[]).tiles[0].unwrap().area() < cells[0].area());
        assert!(plan.sample(240, &[]).tiles[0].is_none());
        let episodes = plan.aspect_episodes(240);
        assert_eq!(episodes.len(), 2);
        assert!(episodes.iter().all(|episode| {
            episode.start < 240
                && episode.end == 240
                && (episode.peak - 4.0).abs() < 0.01
        }));
    }

    #[test]
    fn playback_exchanges_neighbours_when_a_seam_cannot_match_density() {
        let pixels = framing::VIEWPORT * 0.5;
        let field = Tile {
            low: Vec2::new(-180.0, -280.0) / pixels,
            high: Vec2::new(180.0, 280.0) / pixels,
        };
        let mut cells = [field, field];
        cells[0].high.x = 0.0;
        cells[1].low.x = 0.0;
        let padding = Vec2::splat(4.0) / framing::VIEWPORT;
        let opening = Layout {
            tiles: cells
                .map(|cell| Some(glass_in_cell(cell, field, field, padding)))
                .into(),
            gutters: cells.map(Some).into(),
        };
        let packing = Packing {
            gutter_scale: 4.0,
            growth: 1.0,
            ..Packing::default()
        };
        let files = vec![1.0, 4.0];
        let ticks = held::reflow_ticks();
        let without = opening::plan_shift(&packing, &opening, &files, ticks).unwrap();
        let plan = Program::compile(
            &packing,
            opening,
            &[],
            &vec![files.clone(); ticks + 1],
            CompilePolicy::Serial,
        )
        .unwrap();
        assert!(plan.exchanges(1, 0) && plan.exchanges(1, 1));
        let (_, _, pair, gap, uncovered) = plan.swap_gaps().next().unwrap();
        assert!(pair.contains(&0) && pair.contains(&1));
        assert!((0.49..=0.51).contains(&gap) && uncovered < ticks);
        let settled = plan.sample(ticks, &[]);
        assert!(super::super::retirement::fills_field(&settled, field));
        assert!(
            opening::density_cost(&settled, &files)
                < opening::density_cost(&without, &files)
        );
    }

    #[test]
    fn joint_birth_opens_both_tiles_by_the_first_service() {
        let space = Space::new(4.0, 1.0);
        let field = Tile {
            low: Vec2::new(-0.4, -0.3),
            high: Vec2::new(0.4, 0.3),
        };
        let opening = Layout {
            tiles: vec![Some(field), None, None],
            gutters: vec![Some(field), None, None],
        };
        let weights = [1.0; 3];
        let (batch, guests, end) = BirthBatch::plan_together(
            opening,
            space,
            &[(59, 149, 1), (67, 157, 2)],
            |_| Some(&weights),
        )
        .unwrap();
        assert_eq!((guests, end), (2, 149));
        assert_eq!(batch.sample(59, &[]).tiles, vec![Some(field), None, None]);
        assert!(batch.sample(end, &[]).tiles.iter().all(Option::is_some));
    }

    #[test]
    fn opposite_opening_cuts_accept_independent_presentation_progress() {
        use super::super::{exchange, retirement};

        let space = Space::new(4.0, 1.0);
        let field = space.field(framing::MARGIN);
        let opening = Layout {
            tiles: vec![Some(field), None, None],
            gutters: vec![Some(field), None, None],
        };
        let weights = vec![1.0; 3];
        let row = Sample {
            weights: weights.clone(),
            presence: vec![1.0; 3],
        };
        let packing = Packing {
            gutter_scale: space.gap,
            growth: space.growth,
            samples: vec![row; 158].into(),
            ..Packing::default()
        };
        let events = [(59, 149, 1, true), (67, 157, 2, true)];
        let plan = Program::compile(
            &packing,
            opening,
            &events,
            &vec![weights; 158],
            CompilePolicy::SharedLifecycle,
        )
        .unwrap();
        assert_eq!(plan.counts(), (2, 0));
        assert_eq!(
            plan.sample(149, &[]).tiles[1],
            plan.sample(157, &[]).tiles[1]
        );
        for presentation in [false, true] {
            let mut previous = plan.sample(59, &[]);
            for tick in 60_usize..=157 {
                let early = [(2, u64::try_from(tick.saturating_sub(63)).unwrap())];
                let overrides = if presentation { &early[..] } else { &[] };
                let current = plan.sample(tick, overrides);
                assert!(exchange::edge_travel(&previous, &current) <= 8.0, "{tick}");
                assert!(retirement::fills_field(&current, field), "{tick}");
                assert!(retirement::contained(&current), "{tick}");
                previous = current;
            }
        }
    }

    #[test]
    fn a_late_birth_compiles_after_a_large_opening_cohort() {
        const OPENING: usize = 193;
        const READY: usize = 130;
        let weights = vec![1.0; OPENING + 1];
        let mut births = vec![0; OPENING + 1];
        births[OPENING] = usize::MAX;
        let (opening, space) =
            opening::bands(Space::new(4.0, 1.0), &weights, &births).unwrap();
        let row = Sample {
            weights: weights.clone(),
            presence: vec![1.0; OPENING + 1],
        };
        let packing = Packing {
            gutter_scale: space.gap,
            growth: space.growth,
            labels: (0..=OPENING).map(|repo| repo.to_string()).collect(),
            samples: vec![row; READY + 1].into(),
            ..Packing::default()
        };
        let mut windows = vec![(0, 1); OPENING];
        windows.push((2, READY));
        let live_files = vec![weights; READY + 1];
        let plan = Program::plan(&packing, opening, &windows, &live_files).unwrap();
        assert_eq!(plan.counts(), (1, 0));
        assert!(plan.sample(0, &[]).tiles[OPENING].is_none());
        let settled = plan.sample(READY, &[]);
        assert!(settled.tiles.iter().all(Option::is_some));
        assert!(super::super::retirement::fills_field(
            &settled,
            plan.field()
        ));
    }

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
        let plan = Program::compile(
            &packing,
            opening,
            &events,
            &live_files,
            CompilePolicy::Serial,
        )
        .unwrap();
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
            Program::compile(
                &packing,
                opening.clone(),
                &events,
                &live_files,
                CompilePolicy::Serial
            )
            .is_none()
        );
        let plan = Program::compile(
            &packing,
            opening,
            &events,
            &live_files,
            CompilePolicy::SharedArchives,
        )
        .unwrap();
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
