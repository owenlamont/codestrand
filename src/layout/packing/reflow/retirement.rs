use super::{Layout, Space, Tile, compatible, connected, exchange, held};
use crate::model::{count, framing};
use bevy::math::Vec2;

const EPSILON: f32 = 1e-6;
const MAX_ASPECT_CORRECTIONS: usize = 6;
const ASPECT_SEARCH_BUDGET: usize = 256;

#[derive(Clone, Debug)]
pub(in crate::layout::packing) struct Retirement {
    from: Layout,
    to: Layout,
    departing: Box<[usize]>,
}

impl Retirement {
    pub(in crate::layout::packing) fn plan(
        from: Layout,
        departing: usize,
        space: Space,
        ticks: usize,
        glyph_size: impl Fn(usize, Tile) -> f32,
    ) -> Option<Self> {
        let field = space.field(framing::MARGIN);
        let gutters = gutter_bounds(&from)?;
        let retiring = from.gutters.get(departing).copied().flatten()?;
        if !fills_field(&from, gutters) || !contained(&from) {
            return None;
        }
        let mut closures = Vec::new();
        let mut blocked_shapes = Vec::new();
        let sizes: Vec<_> = from
            .tiles
            .iter()
            .enumerate()
            .map(|(repo, &tile)| {
                tile.filter(|_| repo != departing)
                    .map(|tile| glyph_size(repo, tile))
            })
            .collect();
        let max_travel =
            exchange::MAX_EDGE_STEP_PX * held::REFLOW_LINEAR_FRACTION * count(ticks);
        for axis in 0..2 {
            let low = Group::find(&from, departing, axis, true);
            let high = Group::find(&from, departing, axis, false);
            for part in 0..=8_u8 {
                let share = f32::from(part) / 8.0;
                let Some((mut to, donors)) = Self::close(
                    &from,
                    departing,
                    field,
                    retiring,
                    axis,
                    [&low, &high],
                    share,
                ) else {
                    continue;
                };
                expand_last_tile(&mut to, &from, departing);
                if !settled_changes(&from, &to, departing) {
                    blocked_shapes.push((to, donors, 1 - axis));
                } else if let Some(travel) =
                    certified_closure(&from, &to, departing, gutters, max_travel)
                {
                    closures.push((to, donors, travel));
                }
            }
        }
        if closures.is_empty() {
            let mut budget = ASPECT_SEARCH_BUDGET;
            closures.extend(blocked_shapes.into_iter().filter_map(
                |(to, donors, axis)| {
                    let repair = AspectBalance {
                        from: &from,
                        departing,
                        axis,
                        field,
                        gutters,
                        max_travel,
                    };
                    let (to, travel) = repair.plan(to, &mut budget)?;
                    Some((to, donors, travel))
                },
            ));
        }
        let (to, ..) = closures
            .into_iter()
            .map(|(to, donors, travel)| {
                let gain = from
                    .tiles
                    .iter()
                    .zip(&to.tiles)
                    .enumerate()
                    .filter_map(|(repo, (before, after))| {
                        let size = sizes[repo]?;
                        let after = *after.as_ref()?;
                        (Some(after) != *before)
                            .then(|| size.recip() - glyph_size(repo, after).recip())
                    })
                    .sum::<f32>();
                (to, gain, donors, travel)
            })
            .min_by(|a, b| {
                b.1.total_cmp(&a.1)
                    .then_with(|| a.2.cmp(&b.2))
                    .then_with(|| a.3.total_cmp(&b.3))
            })?;
        Some(Self {
            from,
            to,
            departing: [departing].into(),
        })
    }

    pub(super) fn together(
        from: &Layout,
        departing: &[usize],
        space: Space,
        ticks: usize,
        glyph_size: impl Fn(usize, Tile) -> f32,
    ) -> Option<Self> {
        let gutters = gutter_bounds(from)?;
        let max_travel =
            exchange::MAX_EDGE_STEP_PX * held::REFLOW_LINEAR_FRACTION * count(ticks);
        let assemble = |order: &mut dyn Iterator<Item = &usize>| {
            let mut remaining = from.clone();
            let mut ghosts = Vec::with_capacity(departing.len());
            for &repository in order {
                let next =
                    Self::plan(remaining, repository, space, ticks, &glyph_size)?;
                ghosts.push((
                    repository,
                    next.to.tiles[repository],
                    next.to.gutters[repository],
                ));
                remaining = next.sample(0.0);
            }
            for (repo, tile, cell) in ghosts {
                remaining.tiles[repo] = tile;
                remaining.gutters[repo] = cell;
            }
            (exchange::edge_travel(from, &remaining) <= max_travel
                && fills_sweep(from, &remaining, gutters)
                && contained(&remaining)
                && compatible(&from.tiles, &remaining.tiles)
                && compatible(&from.gutters, &remaining.gutters)
                && archive_gutters_connected(from, &remaining, departing))
            .then_some(Self {
                from: from.clone(),
                to: remaining,
                departing: departing.into(),
            })
        };
        assemble(&mut departing.iter())
            .or_else(|| assemble(&mut departing.iter().rev()))
    }

    #[cfg(test)]
    pub(super) fn count(&self) -> usize {
        self.departing.len()
    }

    pub(super) fn finish_in_field(mut self, space: Space, ticks: usize) -> Self {
        if let Some(target) = self.full_field_target(space, ticks) {
            self.to = target;
        }
        self
    }

    fn full_field_target(&self, space: Space, ticks: usize) -> Option<Layout> {
        let &[departing] = self.departing.as_ref() else {
            return None;
        };
        let mut survivors =
            self.to.tiles.iter().enumerate().filter_map(|(repo, tile)| {
                (repo != departing && tile.is_some()).then_some(repo)
            });
        let survivor = survivors.next()?;
        if survivors.next().is_some() {
            return None;
        }
        let old_field = gutter_bounds(&self.from)?;
        let ghost = self.to.gutters[departing]?;
        let axis = (0..2)
            .find(|&axis| (ghost.low[axis] - ghost.high[axis]).abs() <= EPSILON)?;
        let high = (ghost.low[axis] - old_field.high[axis]).abs() <= EPSILON;
        if !high && (ghost.low[axis] - old_field.low[axis]).abs() > EPSILON {
            return None;
        }
        let gutter_field = space.field(super::super::GUTTER_MARGIN);
        let glass_field = space.field(framing::MARGIN);
        let cut = edge(gutter_field, axis, high);
        let mut ghost = gutter_field;
        ghost.low[axis] = cut;
        ghost.high[axis] = cut;
        let mut target = self.to.clone();
        target.gutters[departing] = Some(ghost);
        target.tiles[departing] = Some(ghost);
        target.gutters[survivor] = Some(gutter_field);
        target.tiles[survivor] = Some(glass_field);
        let max_travel =
            exchange::MAX_EDGE_STEP_PX * held::REFLOW_LINEAR_FRACTION * count(ticks);
        certified_closure(&self.from, &target, departing, gutter_field, max_travel)?;
        Some(target)
    }

    fn close(
        from: &Layout,
        departing: usize,
        field: Tile,
        retiring: Tile,
        axis: usize,
        [low, high]: [&Option<Group>; 2],
        share: f32,
    ) -> Option<(Layout, usize)> {
        let cut = if share >= 1.0 {
            retiring.high[axis]
        } else {
            retiring.low[axis] + (retiring.high[axis] - retiring.low[axis]) * share
        };
        if !(field.low[axis]..=field.high[axis]).contains(&cut) {
            return None;
        }
        match (share > 0.0, share < 1.0) {
            (false, true) => {
                let group = high.as_ref()?;
                Some((
                    group.finish(from, Some(departing), cut, field),
                    group.donors,
                ))
            }
            (true, false) => {
                let group = low.as_ref()?;
                Some((
                    group.finish(from, Some(departing), cut, field),
                    group.donors,
                ))
            }
            _ => {
                let low = low.as_ref()?;
                let high = high.as_ref()?;
                let intermediate = low.finish(from, Some(departing), cut, field);
                Some((
                    high.finish(&intermediate, Some(departing), cut, field),
                    low.donors + high.donors,
                ))
            }
        }
    }

    pub(in crate::layout::packing) fn sample(&self, presence: f32) -> Layout {
        if presence >= 1.0 {
            return self.from.clone();
        }
        let mut layout = if presence <= 0.0 {
            self.to.clone()
        } else {
            self.from.blend_edges(&self.to, 1.0 - presence)
        };
        if presence <= 0.0 {
            for &repo in &self.departing {
                layout.tiles[repo] = None;
                layout.gutters[repo] = None;
            }
        }
        layout
    }
}

fn settled_changes(from: &Layout, to: &Layout, departing: usize) -> bool {
    to.tiles
        .iter()
        .zip(&from.tiles)
        .enumerate()
        .all(|(repo, (&after, &before))| {
            repo == departing || after == before || after.is_none_or(settled)
        })
}

fn certified_closure(
    from: &Layout,
    to: &Layout,
    departing: usize,
    gutters: Tile,
    max_travel: f32,
) -> Option<f32> {
    let travel = exchange::edge_travel(from, to);
    if travel > max_travel
        || !fills_sweep(from, to, gutters)
        || !contained(to)
        || !compatible(&from.tiles, &to.tiles)
        || !compatible(&from.gutters, &to.gutters)
    {
        return None;
    }
    archive_gutters_connected(from, to, &[departing]).then_some(travel)
}

fn fills_sweep(from: &Layout, to: &Layout, end: Tile) -> bool {
    let Some(start) = gutter_bounds(from) else {
        return false;
    };
    // Rectangular areas are quadratic in the shared interpolation fraction.
    let middle = Tile {
        low: start.low.midpoint(end.low),
        high: start.high.midpoint(end.high),
    };
    fills_field(to, end) && fills_field(&from.blend_edges(to, 0.5), middle)
}

fn archive_gutters_connected(from: &Layout, to: &Layout, departing: &[usize]) -> bool {
    if !connected(from, to, false) {
        return false;
    }
    let mut survivors = to.clone();
    for &repository in departing {
        survivors.gutters[repository] = None;
    }
    connected(&survivors, &survivors, false)
}

struct AspectBalance<'a> {
    from: &'a Layout,
    departing: usize,
    axis: usize,
    field: Tile,
    gutters: Tile,
    max_travel: f32,
}

impl AspectBalance<'_> {
    fn plan(&self, mut to: Layout, budget: &mut usize) -> Option<(Layout, f32)> {
        let ghost = (
            to.tiles[self.departing].take()?,
            to.gutters[self.departing].take()?,
        );
        if !fills_field(&to, self.gutters) || !contained(&to) {
            return None;
        }
        self.search(&to, ghost, MAX_ASPECT_CORRECTIONS, budget)
    }

    fn search(
        &self,
        to: &Layout,
        ghost: (Tile, Tile),
        remaining: usize,
        budget: &mut usize,
    ) -> Option<(Layout, f32)> {
        let mut violations = to
            .tiles
            .iter()
            .enumerate()
            .filter_map(|(repo, &tile)| {
                let tile = tile?;
                (Some(tile) != self.from.tiles[repo] && !settled(tile))
                    .then_some((repo, tile))
            })
            .peekable();
        if violations.peek().is_none() {
            let mut to = to.clone();
            to.tiles[self.departing] = Some(ghost.0);
            to.gutters[self.departing] = Some(ghost.1);
            let travel = certified_closure(
                self.from,
                &to,
                self.departing,
                self.gutters,
                self.max_travel,
            )?;
            return Some((to, travel));
        }
        if remaining == 0 {
            return None;
        }
        *budget = budget.checked_sub(1)?;
        violations.find_map(|(repository, tile)| {
            let distance = self.correction(tile)?;
            [false, true].into_iter().find_map(|high| {
                let (moved, ghost) =
                    self.move_seam(to, ghost, repository, high, distance)?;
                self.search(&moved, ghost, remaining - 1, budget)
            })
        })
    }

    fn correction(&self, tile: Tile) -> Option<f32> {
        let extent = tile.pixels();
        let cross = extent[1 - self.axis];
        let min = (cross / super::super::BOUND).max(16.0) + 0.01;
        let max = cross * super::super::BOUND - 0.01;
        if min > max || cross < 16.0 {
            return None;
        }
        let span = extent[self.axis];
        Some((span.clamp(min, max) - span) / (framing::VIEWPORT[self.axis] * 0.5))
    }

    fn move_seam(
        &self,
        to: &Layout,
        ghost: (Tile, Tile),
        repository: usize,
        high: bool,
        distance: f32,
    ) -> Option<(Layout, (Tile, Tile))> {
        let group = Group::find(to, repository, self.axis, !high)?;
        let offset = if high { distance } else { -distance };
        let cut = group.cut + offset;
        let moved = group.finish(to, None, cut, self.field);
        (settled(moved.tiles[repository]?)
            && fills_field(&moved, self.gutters)
            && contained(&moved))
        .then(|| (moved, group.move_ghost(ghost, cut)))
    }
}

pub(super) fn best_slide(
    from: &Layout,
    space: Space,
    ticks: usize,
    weights: Option<&[f32]>,
    score: impl Fn(&Layout) -> f32,
) -> Option<Layout> {
    let mut best_score = score(from);
    let mut best = None;
    let field = space.field(framing::MARGIN);
    let gutters = gutter_bounds(from)?;
    let max_travel =
        exchange::MAX_EDGE_STEP_PX * held::REFLOW_LINEAR_FRACTION * count(ticks);
    let gap = Vec2::splat(space.gap) * 2.0 / framing::VIEWPORT;
    let density = weights.and_then(|weights| {
        let (area, total) = super::totals(&from.tiles, weights);
        (total > 0.0).then_some((weights, area, total))
    });
    for (repo, cell) in from.gutters.iter().enumerate() {
        if cell.is_none() {
            continue;
        }
        for (axis, high) in [(0, false), (0, true), (1, false), (1, true)] {
            let Some(group) = Group::find(from, repo, axis, high) else {
                continue;
            };
            if group.members.iter().any(|member| member.repo < repo) {
                continue;
            }
            let local =
                [-2.0, -1.0, 1.0, 2.0].map(|distance| group.cut + distance * gap[axis]);
            let extent = gutters.high[axis] - gutters.low[axis];
            let broad = [0.15, 0.25, 1.0 / 3.0, 0.5, 2.0 / 3.0, 0.75, 0.85]
                .map(|fraction| gutters.low[axis] + extent * fraction);
            let proportional = density.and_then(|(weights, area, total)| {
                group.density_target(from, weights, area, total)
            });
            let Some((low, high)) = group.slide_limits(from, max_travel) else {
                continue;
            };
            let limits = density.map(|_| [low, high]).into_iter().flatten();
            for cut in local
                .into_iter()
                .chain(broad)
                .chain(proportional)
                .chain(limits)
            {
                let cut = cut.clamp(low, high);
                let to = group.finish(from, None, cut, field);
                if !to.tiles.iter().zip(&from.tiles).all(|(&after, &before)| {
                    after == before || after.is_none_or(settled)
                }) || exchange::edge_travel(from, &to) > max_travel + EPSILON
                {
                    continue;
                }
                let score = score(&to);
                if score > best_score
                    && contained(&to)
                    && fills_field(&to, gutters)
                    && compatible(&from.tiles, &to.tiles)
                    && compatible(&from.gutters, &to.gutters)
                    && connected(from, &to, false)
                {
                    best_score = score;
                    best = Some(to);
                }
            }
        }
    }
    best
}

#[derive(Clone, Copy)]
struct Member {
    repo: usize,
    high: bool,
    low: f32,
    end: f32,
}

struct Group {
    axis: usize,
    cut: f32,
    members: Vec<Member>,
    donors: usize,
}

impl Group {
    fn move_ghost(
        &self,
        (mut glass, mut cell): (Tile, Tile),
        cut: f32,
    ) -> (Tile, Tile) {
        let cross = 1 - self.axis;
        if self.members.iter().any(|member| {
            member.low - EPSILON <= cell.low[cross]
                && cell.high[cross] <= member.end + EPSILON
        }) {
            for high in [false, true] {
                if (edge(cell, self.axis, high) - self.cut).abs() <= EPSILON {
                    set_edge(&mut cell, self.axis, high, cut);
                    let glass_edge = edge(glass, self.axis, high) + cut - self.cut;
                    set_edge(&mut glass, self.axis, high, glass_edge);
                }
            }
        }
        (glass, cell)
    }

    fn find(
        layout: &Layout,
        retiring: usize,
        axis: usize,
        grows_high: bool,
    ) -> Option<Self> {
        let cross = 1 - axis;
        let cut = edge(layout.gutters[retiring]?, axis, !grows_high);
        let edges: Vec<_> = layout
            .gutters
            .iter()
            .enumerate()
            .filter_map(|(repo, &cell)| {
                let cell = cell?;
                [false, true]
                    .into_iter()
                    .find(|&high| edge(cell, axis, high).to_bits() == cut.to_bits())
                    .map(|high| Member {
                        repo,
                        high,
                        low: cell.low[cross],
                        end: cell.high[cross],
                    })
            })
            .collect();
        let initial = edges.iter().position(|member| member.repo == retiring)?;
        let mut reached = vec![false; edges.len()];
        reached[initial] = true;
        let mut members = vec![edges[initial]];
        let mut next = 0;
        while let Some(&member) = members.get(next) {
            next += 1;
            for (&other, reached) in edges.iter().zip(&mut reached) {
                if !*reached
                    && member.high != other.high
                    && member.low.max(other.low) + EPSILON < member.end.min(other.end)
                {
                    *reached = true;
                    members.push(other);
                }
            }
        }
        if members.len() < 2 {
            return None;
        }
        let span = |high| -> Option<[f32; 2]> {
            let mut spans: Vec<_> =
                members.iter().filter(|s| s.high == high).copied().collect();
            spans.sort_unstable_by(|a, b| a.low.total_cmp(&b.low));
            let first = spans.first()?;
            let mut end = first.end;
            for side in &spans[1..] {
                if (side.low - end).abs() > EPSILON {
                    return None;
                }
                end = side.end;
            }
            Some([first.low, end])
        };
        let [low, high] = span(true)?;
        let [other_low, other_high] = span(false)?;
        let covers =
            (low - other_low).abs() <= EPSILON && (high - other_high).abs() <= EPSILON;
        let donors = members
            .iter()
            .filter(|m| m.repo != retiring && m.high != grows_high)
            .count();
        covers.then_some(Self {
            axis,
            cut,
            members,
            donors,
        })
    }

    fn slide_limits(&self, layout: &Layout, max_travel: f32) -> Option<(f32, f32)> {
        let pixels_per_unit = framing::VIEWPORT[self.axis] * 0.5;
        let max_offset = max_travel / pixels_per_unit;
        let mut low = -max_offset;
        let mut high = max_offset;
        for member in &self.members {
            let size = layout.tiles[member.repo]?.pixels();
            let cross = size[1 - self.axis];
            let length = size[self.axis];
            let minimum = (cross / super::super::BOUND).max(16.0) + 0.001;
            let maximum = cross * super::super::BOUND - 0.001;
            let bounds = if member.high {
                (minimum - length, maximum - length)
            } else {
                (length - maximum, length - minimum)
            };
            low = low.max(bounds.0 / pixels_per_unit);
            high = high.min(bounds.1 / pixels_per_unit);
        }
        (low <= high).then_some((self.cut + low, self.cut + high))
    }

    fn density_target(
        &self,
        layout: &Layout,
        weights: &[f32],
        area: f32,
        files: f32,
    ) -> Option<f32> {
        let cross = 1 - self.axis;
        let terms = |member: &Member| {
            let tile = layout.tiles[member.repo].unwrap();
            let share = weights[member.repo] / files;
            let slope = (tile.high[cross] - tile.low[cross])
                * if member.high { 0.25 } else { -0.25 };
            (tile.area(), share, slope)
        };
        let total_slope: f32 = self.members.iter().map(|member| terms(member).2).sum();
        let (tile_area, share, slope) = self
            .members
            .iter()
            .map(terms)
            .filter(|&(_, share, _)| share > 0.0)
            .max_by(|a, b| {
                let error = |(tile_area, share, _): (f32, f32, f32)| {
                    (tile_area / (share * area) - 1.0).abs()
                };
                error(*a).total_cmp(&error(*b))
            })?;
        let offset = (share * area - tile_area) / (slope - share * total_slope);
        offset.is_finite().then_some(self.cut + offset)
    }

    fn finish(
        &self,
        from: &Layout,
        retiring: Option<usize>,
        cut: f32,
        field: Tile,
    ) -> Layout {
        let mut result = from.clone();
        let exterior = retiring.and_then(|repository| {
            let bounds = gutter_bounds(from)?;
            let high = [false, true]
                .into_iter()
                .find(|&high| (edge(bounds, self.axis, high) - cut).abs() <= EPSILON)?;
            Some((high, edge(from.tiles[repository]?, self.axis, high)))
        });
        for member in &self.members {
            let cell = result.gutters[member.repo].as_mut().unwrap();
            let tile = result.tiles[member.repo].as_mut().unwrap();
            set_edge(cell, self.axis, member.high, cut);
            if Some(member.repo) == retiring {
                tile.low[self.axis] = cut;
                tile.high[self.axis] = cut;
            } else {
                let moved = exterior
                    .filter(|&(high, _)| high == member.high)
                    .map_or_else(
                        || edge(*tile, self.axis, member.high) + cut - self.cut,
                        |(_, boundary)| boundary,
                    )
                    .clamp(field.low[self.axis], field.high[self.axis]);
                set_edge(tile, self.axis, member.high, moved);
            }
        }
        result
    }
}

fn expand_last_tile(layout: &mut Layout, from: &Layout, departing: usize) {
    let mut live = layout
        .tiles
        .iter_mut()
        .zip(&layout.gutters)
        .enumerate()
        .filter_map(|(repo, (tile, &cell))| {
            (repo != departing)
                .then_some(tile.as_mut().zip(cell))
                .flatten()
        });
    if let Some((tile, cell)) = live.next()
        && live.next().is_none()
    {
        *tile = rectangle_bounds(&from.tiles).unwrap_or(cell);
    }
}

fn edge(tile: Tile, axis: usize, high: bool) -> f32 {
    if high {
        tile.high[axis]
    } else {
        tile.low[axis]
    }
}

fn set_edge(tile: &mut Tile, axis: usize, high: bool, value: f32) {
    if high {
        tile.high[axis] = value;
    } else {
        tile.low[axis] = value;
    }
}

pub(super) fn settled(tile: Tile) -> bool {
    let bounds =
        (1.0 / super::super::BOUND - EPSILON)..=(super::super::BOUND + EPSILON);
    tile.pixels().min_element() >= 16.0 && bounds.contains(&tile.ground_aspect())
}

pub(super) fn gutter_bounds(layout: &Layout) -> Option<Tile> {
    rectangle_bounds(&layout.gutters)
}

pub(super) fn rectangle_bounds(rectangles: &[Option<Tile>]) -> Option<Tile> {
    rectangles
        .iter()
        .flatten()
        .copied()
        .reduce(|bounds, cell| Tile {
            low: bounds.low.min(cell.low),
            high: bounds.high.max(cell.high),
        })
}

pub(super) fn fills_field(layout: &Layout, field: Tile) -> bool {
    let tolerance = Vec2::splat(EPSILON);
    let area = layout
        .gutters
        .iter()
        .flatten()
        .map(|tile| tile.area())
        .sum::<f32>();
    (field.area() - area).abs() <= EPSILON * field.area().max(1.0)
        && layout.gutters.iter().flatten().all(|tile| {
            tile.low.cmpge(field.low - tolerance).all()
                && tile.high.cmple(field.high + tolerance).all()
                && tile.low.cmple(tile.high).all()
        })
        && compatible(&layout.gutters, &layout.gutters)
}

pub(super) fn contained(layout: &Layout) -> bool {
    let tolerance = Vec2::splat(EPSILON);
    layout
        .tiles
        .iter()
        .zip(&layout.gutters)
        .all(|(&tile, &cell)| {
            tile.is_none_or(|tile| {
                cell.is_some_and(|cell| {
                    tile.low.cmpge(cell.low - tolerance).all()
                        && tile.high.cmple(cell.high + tolerance).all()
                })
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exterior_archive_leaves_a_straight_visible_field_edge() {
        let space = Space::new(4.0, 1.0);
        let cells = [
            Tile {
                low: Vec2::new(-0.7, -0.6),
                high: Vec2::ZERO,
            },
            Tile {
                low: Vec2::new(-0.7, 0.0),
                high: Vec2::new(0.0, 0.6),
            },
            Tile {
                low: Vec2::new(0.0, -0.6),
                high: Vec2::new(0.7, 0.6),
            },
        ];
        let glass_bottom = -0.57;
        let from = Layout {
            tiles: vec![
                Some(Tile {
                    low: Vec2::new(-0.67, glass_bottom),
                    high: Vec2::splat(-0.005),
                }),
                Some(Tile {
                    low: Vec2::new(-0.67, 0.005),
                    high: Vec2::new(-0.005, 0.57),
                }),
                Some(Tile {
                    low: Vec2::new(0.005, glass_bottom),
                    high: Vec2::new(0.67, 0.57),
                }),
            ],
            gutters: cells.map(Some).into(),
        };
        let field = space.field(framing::MARGIN);
        let high = Group::find(&from, 0, 1, false);
        let (closed, _) =
            Retirement::close(&from, 0, field, cells[0], 1, [&None, &high], 0.0)
                .unwrap();
        let bounds = gutter_bounds(&from).unwrap();
        assert!(certified_closure(&from, &closed, 0, bounds, 400.0).is_some());
        assert_eq!(
            closed.tiles[1].unwrap().low.y.to_bits(),
            glass_bottom.to_bits()
        );
        assert_eq!(
            closed.tiles[1].unwrap().low.y.to_bits(),
            closed.tiles[2].unwrap().low.y.to_bits()
        );
    }

    #[test]
    fn archive_aspect_correction_moves_the_collapsing_gutters_cross_edge() {
        let space = Space::new(4.0, 1.0);
        let pixels = framing::VIEWPORT * 0.5;
        let gap = Vec2::splat(space.gap) / framing::VIEWPORT;
        let cells = [
            ([-608.3, -310.0], [448.3, 10.0]),
            ([448.3, -310.0], [608.3, 10.0]),
            ([-608.3, 10.0], [0.0, 310.0]),
            ([0.0, 10.0], [608.3, 310.0]),
        ]
        .map(|(low, high)| Tile {
            low: Vec2::from_array(low) / pixels,
            high: Vec2::from_array(high) / pixels,
        });
        let from = Layout {
            tiles: cells
                .map(|cell| {
                    Some(Tile {
                        low: cell.low + gap,
                        high: cell.high - gap,
                    })
                })
                .into(),
            gutters: cells.map(Some).into(),
        };
        let departing = 1;
        let field = space.field(framing::MARGIN);
        let gutters = gutter_bounds(&from).unwrap();
        let low = Group::find(&from, departing, 0, true);
        let (unbalanced, _) = Retirement::close(
            &from,
            departing,
            field,
            cells[departing],
            0,
            [&low, &None],
            1.0,
        )
        .unwrap();
        let repair = AspectBalance {
            from: &from,
            departing,
            axis: 1,
            field,
            gutters,
            max_travel: 400.0,
        };
        let mut budget = ASPECT_SEARCH_BUDGET;
        let (closed, _) = repair.plan(unbalanced, &mut budget).unwrap();
        assert!(closed.gutters[departing].unwrap().high.y > cells[departing].high.y);
        let mut stale_ghost = closed.clone();
        stale_ghost.gutters[departing].as_mut().unwrap().high.y =
            cells[departing].high.y;
        assert!(fills_field(&stale_ghost, gutters));
        assert!(!fills_sweep(&from, &stale_ghost, gutters));
        for tick in 0..=240 {
            let layout = from.blend_edges(&closed, held::progress(tick, 240));
            assert!(fills_field(&layout, gutters));
            assert!(contained(&layout));
        }
    }

    #[test]
    fn dividing_archive_connects_its_neighbours_as_the_gutter_closes() {
        let space = Space::new(4.0, 1.0);
        let edges = [-0.75, -0.25, 0.25, 0.75];
        let gap = Vec2::splat(space.gap) / framing::VIEWPORT;
        let cells: Vec<_> = edges
            .windows(2)
            .map(|pair| Tile {
                low: Vec2::new(pair[0], -0.5),
                high: Vec2::new(pair[1], 0.5),
            })
            .collect();
        let from = Layout {
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
        let mut survivors = from.clone();
        survivors.gutters[1] = None;
        assert!(!connected(&survivors, &survivors, false));
        let closure =
            Retirement::plan(from, 1, space, 240, |_, tile| tile.area().sqrt())
                .unwrap();
        for tick in 0..=240 {
            let layout = closure.sample(1.0 - held::progress(tick, 240));
            assert!(compatible(&layout.tiles, &layout.tiles));
            crate::model::assert_gutter_routes(
                layout.gutters.iter().flatten().copied(),
            );
            assert_eq!(layout.gutters[1].is_some(), tick < 240);
        }
    }

    #[test]
    fn archive_aspect_correction_can_pass_space_through_a_neighbour() {
        let space = Space::new(4.0, 1.3);
        let pixels = framing::VIEWPORT * 0.5;
        let gap = Vec2::splat(space.gap) / framing::VIEWPORT;
        let cells = [
            ([0.0, -100.0], [200.0, 100.0]),
            ([-200.0, -100.0], [0.0, -25.0]),
            ([-200.0, -25.0], [0.0, 100.0]),
            ([-200.0, -220.0], [200.0, -100.0]),
            ([-200.0, -350.0], [200.0, -220.0]),
            ([-200.0, 100.0], [200.0, 250.0]),
            ([-200.0, 250.0], [200.0, 375.0]),
        ]
        .map(|(low, high)| Tile {
            low: Vec2::from_array(low) / pixels,
            high: Vec2::from_array(high) / pixels,
        });
        let from = Layout {
            tiles: cells
                .map(|cell| {
                    Some(Tile {
                        low: cell.low + gap,
                        high: cell.high - gap,
                    })
                })
                .into(),
            gutters: cells.map(Some).into(),
        };
        let departing = 0;
        let field = space.field(framing::MARGIN);
        let gutters = gutter_bounds(&from).unwrap();
        let low = Group::find(&from, departing, 0, true);
        let (unbalanced, _) = Retirement::close(
            &from,
            departing,
            field,
            cells[departing],
            0,
            [&low, &None],
            1.0,
        )
        .unwrap();
        assert!(!settled(unbalanced.tiles[1].unwrap()));
        let repair = AspectBalance {
            from: &from,
            departing,
            axis: 1,
            field,
            gutters,
            max_travel: 400.0,
        };
        let mut budget = ASPECT_SEARCH_BUDGET;
        let (closed, travel) = repair.plan(unbalanced, &mut budget).unwrap();
        assert!(travel <= 400.0);
        assert!(settled_changes(&from, &closed, departing));
        assert!(
            closed.gutters[5] != from.gutters[5]
                || closed.gutters[3] != from.gutters[3]
        );
    }

    #[test]
    fn archive_closure_can_move_a_perpendicular_seam_to_keep_tiles_in_shape() {
        let space = Space::new(4.0, 1.0);
        let pixels = framing::VIEWPORT * 0.5;
        let cell = |low, high| Tile {
            low: Vec2::from_array(low) / pixels,
            high: Vec2::from_array(high) / pixels,
        };
        let cells = [
            cell([-200.0, -250.0], [200.0, -50.0]),
            cell([-200.0, -50.0], [-100.0, 250.0]),
            cell([-100.0, -50.0], [200.0, 250.0]),
        ];
        let [archived, left, right] = cells;
        let gap = Vec2::splat(space.gap) / framing::VIEWPORT;
        let layout = Layout {
            tiles: cells
                .map(|cell| {
                    Some(Tile {
                        low: cell.low + gap,
                        high: cell.high - gap,
                    })
                })
                .into(),
            gutters: cells.map(Some).into(),
        };
        assert!(layout.tiles.iter().flatten().copied().all(settled));
        let closure = Retirement::plan(layout.clone(), 0, space, 240, |_, tile| {
            tile.area().sqrt()
        })
        .unwrap();
        let field = gutter_bounds(&layout).unwrap();
        let mut previous = layout;
        for tick in 1..=240 {
            let current = closure.sample(1.0 - held::progress(tick, 240));
            assert!(fills_field(&current, field));
            assert!(contained(&current));
            assert!(compatible(&previous.tiles, &current.tiles));
            assert!(
                exchange::edge_travel(&previous, &current)
                    <= exchange::MAX_EDGE_STEP_PX
            );
            crate::model::assert_gutter_routes(
                current.gutters.iter().flatten().copied(),
            );
            previous = current;
        }
        assert!(previous.tiles[0].is_none());
        assert!(previous.tiles.iter().flatten().copied().all(settled));
        let [grown_left, grown_right] =
            [previous.gutters[1].unwrap(), previous.gutters[2].unwrap()];
        assert_eq!(grown_left.low.y, archived.low.y);
        assert!(grown_left.high.x > left.high.x);
        assert_eq!(grown_left.high.x, grown_right.low.x);
        assert_eq!(grown_right.high, right.high);
    }

    #[test]
    fn simultaneous_archives_can_give_space_to_the_same_neighbour() {
        let space = Space::new(4.0, 1.0);
        let field = space.field(framing::MARGIN);
        let width = (field.high.x - field.low.x) / 3.0;
        let edges = [
            field.low.x,
            field.low.x + width,
            field.high.x - width,
            field.high.x,
        ];
        let cells: Vec<_> = edges
            .windows(2)
            .map(|pair| {
                Some(Tile {
                    low: Vec2::new(pair[0], field.low.y),
                    high: Vec2::new(pair[1], field.high.y),
                })
            })
            .collect();
        let gap = Vec2::splat(space.gap) / framing::VIEWPORT;
        let layout = Layout {
            tiles: cells
                .iter()
                .map(|cell| {
                    cell.map(|cell| Tile {
                        low: cell.low + gap,
                        high: cell.high - gap,
                    })
                })
                .collect(),
            gutters: cells,
        };
        let archive = Retirement::together(&layout, &[0, 2], space, 240, |_, tile| {
            tile.area().sqrt()
        })
        .unwrap();
        let mut previous = layout.clone();
        for tick in 1..=240 {
            let current = archive.sample(1.0 - held::progress(tick, 240));
            assert!(fills_field(&current, field));
            assert!(contained(&current));
            assert!(compatible(&previous.tiles, &current.tiles));
            crate::model::assert_gutter_routes(
                current.gutters.iter().flatten().copied(),
            );
            assert_eq!(current.tiles[0].is_some(), tick < 240);
            assert_eq!(current.tiles[2].is_some(), tick < 240);
            previous = current;
        }
        assert_eq!(
            previous.tiles[1],
            Some(Tile {
                low: field.low + gap,
                high: field.high - gap
            })
        );
    }

    #[test]
    fn sliding_a_cut_can_change_neighbours_without_breaking_routes() {
        let space = Space::new(20.0, 1.0);
        let field = space.field(super::super::super::GUTTER_MARGIN);
        let glass = space.field(framing::MARGIN);
        let padding = Vec2::splat(space.gap) / framing::VIEWPORT;
        let point = |x, y| field.low + (field.high - field.low) * Vec2::new(x, y);
        let cells = [
            ([0.0, 0.0], [0.5, 0.4]),
            ([0.0, 0.4], [0.5, 1.0]),
            ([0.5, 0.0], [1.0, 0.45]),
            ([0.5, 0.45], [1.0, 1.0]),
        ]
        .map(|([x0, y0], [x1, y1])| Tile {
            low: point(x0, y0),
            high: point(x1, y1),
        });
        let opening = Layout {
            tiles: cells
                .map(|cell| {
                    Some(Tile {
                        low: (cell.low + padding).max(glass.low),
                        high: (cell.high - padding).min(glass.high),
                    })
                })
                .into(),
            gutters: cells.map(Some).into(),
        };
        assert!(cells[0].high.y < cells[3].low.y);
        let target = best_slide(&opening, space, 100, None, |layout| {
            layout.gutters[0].unwrap().high.y
        })
        .unwrap();
        assert!(target.gutters[0].unwrap().high.y > cells[3].low.y);
        assert_eq!(opening.tiles[2..], target.tiles[2..]);
        for tick in 0..=100 {
            let layout = opening.blend_edges(&target, held::progress(tick, 100));
            assert!(fills_field(&layout, field));
            assert!(contained(&layout));
            crate::model::assert_gutter_routes(layout.gutters.into_iter().flatten());
        }
    }

    #[test]
    fn a_growing_neighbour_can_border_only_part_of_the_retiring_edge() {
        let space = Space::new(4.0, 1.0);
        let field = space.field(super::super::super::GUTTER_MARGIN);
        let in_field = |x, y| field.low + (field.high - field.low) * Vec2::new(x, y);
        let dimensions = [
            ([0.5, 0.0], [1.0, 0.55]),
            ([0.25, 0.55], [1.0, 1.0]),
            ([0.0, 0.0], [0.5, 0.32]),
            ([0.0, 0.32], [0.25, 1.0]),
            ([0.25, 0.32], [0.5, 0.55]),
        ];
        let cells = dimensions.map(|([x0, y0], [x1, y1])| Tile {
            low: in_field(x0, y0),
            high: in_field(x1, y1),
        });
        let glass = space.field(framing::MARGIN);
        let padding = Vec2::splat(space.gap) / framing::VIEWPORT;
        let opening = Layout {
            tiles: cells
                .map(|cell| {
                    Some(Tile {
                        low: (cell.low + padding).max(glass.low),
                        high: (cell.high - padding).min(glass.high),
                    })
                })
                .into(),
            gutters: cells.map(Some).into(),
        };
        let before = opening.clone();
        for axis in 0..2 {
            for high in [false, true] {
                assert_eq!(Group::find(&before, 4, axis, high).unwrap().donors, 1);
            }
        }
        let low = Group::find(&before, 4, 0, true);
        let high = Group::find(&before, 4, 0, false);
        let (shared, _) =
            Retirement::close(&before, 4, glass, cells[4], 0, [&low, &high], 0.5)
                .unwrap();
        for repo in [0, 3] {
            assert!(
                shared.tiles[repo].unwrap().area() > before.tiles[repo].unwrap().area()
            );
        }
        assert!(fills_field(&shared, field));
        assert!(contained(&shared));
        assert!(compatible(&before.tiles, &shared.tiles));
        assert!(compatible(&before.gutters, &shared.gutters));

        let change = Retirement::plan(opening, 4, space, 240, |repo, tile| {
            tile.area().sqrt() * if repo == 0 { 0.25 } else { 1.0 }
        })
        .unwrap();
        assert!(!compatible(
            &before.tiles,
            &Group {
                axis: 0,
                cut: cells[4].high.x,
                members: vec![Member {
                    repo: 0,
                    high: false,
                    low: 0.0,
                    end: 1.0
                }],
                donors: 0,
            }
            .finish(&before, Some(4), cells[4].low.x, glass)
            .tiles
        ));
        let after = change.sample(0.0);
        assert!(after.tiles[4].is_none());
        assert!(after.tiles[0].unwrap().area() > before.tiles[0].unwrap().area());
        assert!(
            before.tiles[..4]
                .iter()
                .zip(&after.tiles[..4])
                .any(|(a, b)| { a.unwrap().area() > b.unwrap().area() })
        );
        let fixed: Vec<_> = before.tiles[..4]
            .iter()
            .zip(&after.tiles[..4])
            .enumerate()
            .filter_map(|(repo, (a, b))| (a == b).then_some((repo, *a)))
            .collect();
        let mut previous = before;
        for tick in 0..=240 {
            let current = change.sample(1.0 - held::progress(tick, 240));
            assert_eq!(current.tiles[4].is_some(), tick < 240);
            assert!(fills_field(&current, field));
            assert!(contained(&current));
            for &(repo, tile) in &fixed {
                assert_eq!(current.tiles[repo], tile);
            }
            assert!(compatible(&previous.tiles, &current.tiles));
            crate::model::assert_gutter_routes(
                current.gutters.iter().flatten().copied(),
            );
            previous = current;
        }
    }
}
