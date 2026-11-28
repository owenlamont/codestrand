use super::{Layout, Space, Tile, compatible, connected, exchange, retirement};
use crate::model::{count, framing};
use bevy::math::{FloatExt, Vec2};

#[derive(Debug)]
pub(super) struct Birth {
    start: Layout,
    end: Layout,
    pub(super) guest: usize,
    cut: Cut,
}

#[derive(Clone, Copy, Debug)]
struct Cut {
    host: usize,
    guest: usize,
    axis: usize,
    high: bool,
    start: f32,
    end: f32,
    outer_inset: f32,
}

#[derive(Debug)]
pub(super) struct BirthBatch {
    opening: Layout,
    settled: Layout,
    cuts: Vec<(usize, usize, Cut)>,
    gap: f32,
    joint: Option<JointBirth>,
}

#[derive(Debug)]
struct JointBirth {
    start: usize,
    end: usize,
    collapsed: Layout,
}

impl Birth {
    pub(super) fn plan(
        layout: &Layout,
        space: Space,
        guest: usize,
        weights: &[f32],
        ticks: usize,
    ) -> Option<Self> {
        Self::with_edges(layout, space, guest, weights, ticks, |_| true)
    }

    fn with_edges(
        layout: &Layout,
        space: Space,
        guest: usize,
        weights: &[f32],
        ticks: usize,
        available: impl Fn(Cut) -> bool,
    ) -> Option<Self> {
        if layout.tiles.get(guest)?.is_some() {
            return None;
        }
        let field = space.field(framing::MARGIN);
        let gap = Vec2::splat(space.gap) / framing::VIEWPORT;
        let speed = exchange::MAX_EDGE_STEP_PX * (1.0 - super::held::birth_ramp(ticks));
        let max_travel = speed * count(ticks);
        let mut best = None;
        let mut cost = (true, f32::INFINITY);
        for (host, (&tile, &cell)) in
            layout.tiles.iter().zip(&layout.gutters).enumerate()
        {
            let Some((tile, cell)) = tile.zip(cell) else {
                continue;
            };
            let share = weights[guest].max(1.0)
                / (weights[host].max(1.0) + weights[guest].max(1.0));
            let cuts = [(0, false), (0, true), (1, false), (1, true)]
                .into_iter()
                .flat_map(|(axis, high)| {
                    let min_px = (tile.pixels()[1 - axis] / super::super::BOUND)
                        .max(16.0)
                        + 0.001;
                    let inset = if high {
                        cell.high[axis] - tile.high[axis]
                    } else {
                        tile.low[axis] - cell.low[axis]
                    };
                    let minimum =
                        (min_px * 2.0 / framing::VIEWPORT[axis] + inset + gap[axis])
                            / (cell.high[axis] - cell.low[axis]);
                    [0.15, 0.20, 0.25, 1.0 / 3.0, 0.5, minimum]
                        .into_iter()
                        .enumerate()
                        .map(move |(index, fraction)| {
                            (axis, high, fraction, index == 5)
                        })
                });
            for (axis, high, fraction, at_minimum) in cuts {
                let edge = if high {
                    cell.high[axis]
                } else {
                    cell.low[axis]
                };
                if !(field.low[axis]..=field.high[axis]).contains(&edge) {
                    continue;
                }
                let width = (cell.high[axis] - cell.low[axis]) * fraction;
                let cut = if high { edge - width } else { edge + width };
                let tiles = halves(tile, axis, high, cut - gap[axis], cut + gap[axis]);
                let gutter_cells = halves(cell, axis, high, cut, cut);
                let [host_tile, guest_tile] = tiles;
                if !retirement::settled(host_tile) || !retirement::settled(guest_tile) {
                    continue;
                }
                let ratio = gutter_cells[1].area() / cell.area();
                let cut_recipe = Cut {
                    host,
                    guest,
                    axis,
                    high,
                    start: edge,
                    end: cut,
                    outer_inset: if high {
                        edge - tile.high[axis]
                    } else {
                        tile.low[axis] - edge
                    },
                };
                let candidate_cost =
                    (at_minimum, (ratio - share).abs() + fraction * 0.1);
                if candidate_cost >= cost || !available(cut_recipe) {
                    continue;
                }
                let mut start = layout.clone();
                start.tiles[guest] = Some(collapsed(guest_tile, axis, edge));
                start.gutters[guest] = Some(collapsed(gutter_cells[1], axis, edge));
                let mut end = layout.clone();
                [end.tiles[host], end.tiles[guest]] = tiles.map(Some);
                [end.gutters[host], end.gutters[guest]] = gutter_cells.map(Some);
                if exchange::edge_travel(&start, &end) > max_travel
                    || !retirement::contained(&end)
                    || !compatible(&start.tiles, &end.tiles)
                    || !compatible(&start.gutters, &end.gutters)
                    || !connected(&end, &end, false)
                {
                    continue;
                }
                cost = candidate_cost;
                best = Some(Self {
                    start,
                    end,
                    guest,
                    cut: cut_recipe,
                });
            }
        }
        best
    }

    pub(super) fn sample(&self, presence: f32) -> Layout {
        if presence >= 1.0 {
            return self.end.clone();
        }
        let mut layout = self.start.blend_edges(&self.end, presence);
        if presence <= 0.0 {
            layout.tiles[self.guest] = None;
            layout.gutters[self.guest] = None;
        }
        layout
    }
}

impl BirthBatch {
    pub(super) fn plan<'a>(
        opening: Layout,
        space: Space,
        births: &[(usize, usize, usize)],
        weights: impl Fn(usize) -> Option<&'a [f32]>,
        allow_perpendicular: bool,
    ) -> Option<(Self, usize, usize)> {
        let mut current = opening.clone();
        let mut cuts: Vec<(usize, usize, Cut)> = Vec::with_capacity(births.len());
        let mut processed = 0;
        for &(start, end, guest) in births {
            if current.tiles[guest].is_some() {
                processed += 1;
                continue;
            }
            let Some(duration) = end.checked_sub(start).filter(|&ticks| ticks > 0)
            else {
                break;
            };
            let Some(birth) = Birth::with_edges(
                &current,
                space,
                guest,
                weights(end)?,
                duration,
                |cut| {
                    opening.tiles[cut.host].is_some()
                        && cuts.iter().all(|&(_, _, earlier)| {
                            cut.host != earlier.host
                                || (cut.axis == earlier.axis
                                    && cut.high != earlier.high)
                                || (cut.axis != earlier.axis && allow_perpendicular)
                        })
                },
            ) else {
                break;
            };
            current = birth.end;
            cuts.push((start, end, birth.cut));
            processed += 1;
        }
        let births = &births[..processed];
        let start = births.first()?.0;
        let end = births.iter().map(|&(_, end, _)| end).max()?;
        let settled = Self::certify(&opening, births, start, end, |tick| {
            Self::apply_cuts(&opening, &cuts, space.gap, tick, &[])
        })?;
        Some((
            Self {
                opening,
                settled,
                cuts,
                gap: space.gap,
                joint: None,
            },
            processed,
            end,
        ))
    }

    pub(super) fn plan_together<'a>(
        opening: Layout,
        space: Space,
        births: &[(usize, usize, usize)],
        weights: impl Fn(usize) -> Option<&'a [f32]>,
    ) -> Option<(Self, usize, usize)> {
        let start = births.first()?.0;
        let end = births.iter().map(|&(_, ready, _)| ready).min()?;
        let processed = births.partition_point(|&(begin, _, _)| begin < end);
        if processed < 2 || end <= start {
            return None;
        }
        let births = &births[..processed];
        if let Some(batch) =
            Self::joint_opening(&opening, space, births, start, end, &weights)
        {
            return Some((batch, processed, end));
        }
        let mut settled = opening.clone();
        let mut ghosts = opening.clone();
        let mut cuts = Vec::with_capacity(processed);
        for &(_, ready, guest) in births {
            if settled.tiles[guest].is_some() {
                continue;
            }
            let birth =
                Birth::plan(&settled, space, guest, weights(ready)?, end - start)?;
            let cut = birth.cut;
            let host_tile = ghosts.tiles[cut.host]?;
            let host_cell = ghosts.gutters[cut.host]?;
            let side = |rect: Tile| {
                if cut.high {
                    rect.high[cut.axis]
                } else {
                    rect.low[cut.axis]
                }
            };
            let edge = side(host_cell);
            ghosts.tiles[guest] = Some(collapsed(host_tile, cut.axis, edge));
            ghosts.gutters[guest] = Some(collapsed(host_cell, cut.axis, edge));
            settled = birth.end;
            cuts.push((start, end, cut));
        }
        Self::certify(&opening, births, start, end, |tick| {
            let progress = super::held::birth_ease(tick - start, end - start);
            if tick == end {
                settled.clone()
            } else {
                ghosts.blend_edges(&settled, progress)
            }
        })?;
        Some((
            Self {
                opening,
                settled,
                cuts,
                gap: space.gap,
                joint: Some(JointBirth {
                    start,
                    end,
                    collapsed: ghosts,
                }),
            },
            processed,
            end,
        ))
    }

    fn joint_opening<'a>(
        opening: &Layout,
        space: Space,
        births: &[(usize, usize, usize)],
        start: usize,
        end: usize,
        weights: impl Fn(usize) -> Option<&'a [f32]>,
    ) -> Option<Self> {
        if births.len() < 3 {
            return None;
        }
        let mut present = opening
            .gutters
            .iter()
            .enumerate()
            .filter_map(|(repo, &cell)| cell.map(|cell| (repo, cell)));
        let (host, _) = present.next()?;
        if present.next().is_some() {
            return None;
        }
        let mut sizes = vec![0.0; opening.tiles.len()];
        for &(_, ready, repo) in births {
            sizes[repo] = weights(ready)?[repo].max(1.0);
        }
        let mut members: Vec<_> = births.iter().map(|&(_, _, repo)| repo).collect();
        members.retain(|&repo| repo != host);
        members.insert(members.len() / 2, host);
        let basis = weights(end)?[host].max(1.0);
        let limit = members.len().min(12);
        let shapes =
            (0..2).flat_map(|axis| (1..=limit).map(move |bands| (axis, bands)));
        for scale in [1.0, 1.5, 2.0, 3.0, 4.0] {
            sizes[host] = basis * scale;
            let candidate = shapes.clone().find_map(|(axis, bands)| {
                let settled = super::opening::build_bands(
                    space, &sizes, &members, axis, bands, true,
                )?;
                let (settled, collapsed) = Self::certify_bands(
                    opening,
                    settled,
                    births,
                    host,
                    axis,
                    (start, end),
                )?;
                Some(Self {
                    opening: opening.clone(),
                    settled,
                    cuts: Vec::new(),
                    gap: space.gap,
                    joint: Some(JointBirth {
                        start,
                        end,
                        collapsed,
                    }),
                })
            });
            if candidate.is_some() {
                return candidate;
            }
        }
        None
    }

    fn certify_bands(
        opening: &Layout,
        mut settled: Layout,
        births: &[(usize, usize, usize)],
        host: usize,
        axis: usize,
        (start, end): (usize, usize),
    ) -> Option<(Layout, Layout)> {
        let cross = 1 - axis;
        let field = opening.gutters[host]?;
        let glass = opening.tiles[host]?;
        for cell in settled.gutters.iter_mut().flatten() {
            cell.low = cell.low.max(field.low);
            cell.high = cell.high.min(field.high);
        }
        let host_cell = settled.gutters[host]?;
        let mut ghosts = opening.clone();
        for &(_, _, repo) in births {
            if repo == host {
                continue;
            }
            let mut cell = settled.gutters[repo]?;
            let mut tile = settled.tiles[repo]?;
            if cell.high[cross] <= host_cell.low[cross] {
                cell = collapsed(cell, cross, field.low[cross]);
                tile = collapsed(tile, cross, field.low[cross]);
            } else if cell.low[cross] >= host_cell.high[cross] {
                cell = collapsed(cell, cross, field.high[cross]);
                tile = collapsed(tile, cross, field.high[cross]);
            } else {
                let edge = if cell.high[axis] <= host_cell.low[axis] {
                    field.low[axis]
                } else {
                    field.high[axis]
                };
                cell = collapsed(cell, axis, edge);
                tile = collapsed(tile, axis, edge);
                cell.low[cross] = field.low[cross];
                cell.high[cross] = field.high[cross];
                tile.low[cross] = glass.low[cross];
                tile.high[cross] = glass.high[cross];
            }
            ghosts.tiles[repo] = Some(tile);
            ghosts.gutters[repo] = Some(cell);
        }
        let settled = Self::certify(opening, births, start, end, |tick| {
            if tick == end {
                settled.clone()
            } else {
                ghosts.blend_edges(
                    &settled,
                    super::held::birth_ease(tick - start, end - start),
                )
            }
        })?;
        Some((settled, ghosts))
    }

    fn certify(
        opening: &Layout,
        births: &[(usize, usize, usize)],
        start: usize,
        end: usize,
        sample: impl Fn(usize) -> Layout,
    ) -> Option<Layout> {
        let field = retirement::gutter_bounds(opening)?;
        let mut previous = sample(start);
        for tick in start + 1..=end {
            let next = sample(tick);
            if exchange::edge_travel(&previous, &next) > exchange::MAX_EDGE_STEP_PX
                || !retirement::fills_field(&next, field)
                || !retirement::contained(&next)
                || !compatible(&previous.tiles, &next.tiles)
                || !compatible(&previous.gutters, &next.gutters)
                || !connected(&next, &next, false)
                || births.iter().any(|&(_, ready, repo)| {
                    ready.min(end) == tick && next.tiles[repo].is_none()
                })
            {
                return None;
            }
            previous = next;
        }
        Some(previous)
    }

    pub(super) fn sample(&self, tick: usize, overrides: &[(usize, u64)]) -> Layout {
        if let Some(joint) = &self.joint {
            if tick <= joint.start {
                return self.opening.clone();
            }
            if tick >= joint.end {
                return self.settled();
            }
            let progress =
                super::held::birth_ease(tick - joint.start, joint.end - joint.start);
            return joint.collapsed.blend_edges(&self.settled, progress);
        }
        Self::apply_cuts(&self.opening, &self.cuts, self.gap, tick, overrides)
    }

    pub(super) fn settled(&self) -> Layout {
        self.settled.clone()
    }

    fn apply_cuts(
        opening: &Layout,
        cuts: &[(usize, usize, Cut)],
        gap: f32,
        tick: usize,
        overrides: &[(usize, u64)],
    ) -> Layout {
        let mut layout = opening.clone();
        for &(start, end, cut) in cuts {
            let progress = overrides
                .iter()
                .find(|&&(repo, _)| repo == cut.guest)
                .map_or_else(
                    || super::held::birth_ease(tick.saturating_sub(start), end - start),
                    |&(_, elapsed)| crate::layout::packing::birth_presence(elapsed),
                );
            if progress > 0.0 {
                cut.apply(&mut layout, gap, progress);
            }
        }
        layout
    }

    pub(super) fn has_override(
        &self,
        overrides: &[(usize, u64)],
        accept: impl Fn(u64) -> bool,
    ) -> bool {
        self.joint.is_none()
            && self.cuts.iter().any(|&(_, _, cut)| {
                overrides
                    .iter()
                    .any(|&(repo, elapsed)| repo == cut.guest && accept(elapsed))
            })
    }

    #[cfg(test)]
    pub(super) fn count(&self) -> usize {
        if self.joint.is_some() {
            self.settled.tiles.iter().flatten().count()
                - self.opening.tiles.iter().flatten().count()
        } else {
            self.cuts.len()
        }
    }
}

impl Cut {
    fn apply(self, layout: &mut Layout, gap: f32, progress: f32) {
        let Self {
            host,
            guest,
            axis,
            high,
            start,
            end,
            outer_inset,
        } = self;
        let Some((tile, cell)) = layout.tiles[host].zip(layout.gutters[host]) else {
            return;
        };
        let margin = gap / framing::VIEWPORT[axis];
        let host_inset = outer_inset.lerp(margin, progress);
        let guest_inset = margin * progress;
        let guest_outer = outer_inset * progress;
        let cut = start.lerp(end, progress);
        let [before, after] = if high {
            [cut - host_inset, cut + guest_inset]
        } else {
            [cut - guest_inset, cut + host_inset]
        };
        let [host_tile, mut guest_tile] = halves(tile, axis, high, before, after);
        if high {
            guest_tile.high[axis] = start - guest_outer;
        } else {
            guest_tile.low[axis] = start + guest_outer;
        }
        let [host_cell, guest_cell] = halves(cell, axis, high, cut, cut);
        [layout.tiles[host], layout.tiles[guest]] = [Some(host_tile), Some(guest_tile)];
        [layout.gutters[host], layout.gutters[guest]] =
            [Some(host_cell), Some(guest_cell)];
    }
}

fn halves(
    rect: Tile,
    axis: usize,
    high: bool,
    low_cut: f32,
    high_cut: f32,
) -> [Tile; 2] {
    let [mut lower, mut upper] = [rect; 2];
    lower.high[axis] = low_cut;
    upper.low[axis] = high_cut;
    if high { [lower, upper] } else { [upper, lower] }
}

fn collapsed(mut rect: Tile, axis: usize, edge: f32) -> Tile {
    rect.low[axis] = edge;
    rect.high[axis] = edge;
    rect
}
