use super::{Layout, Space, Tile, compatible, connected, exchange, held};
use crate::model::{count, framing};
use bevy::math::Vec2;

const EPSILON: f32 = 1e-6;

#[derive(Clone, Debug)]
pub(in crate::layout::packing) struct Retirement {
    from: Layout,
    to: Layout,
    departing: usize,
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
        let gutters = space.field(super::super::GUTTER_MARGIN);
        let retiring = from.gutters.get(departing).copied().flatten()?;
        if !fills_field(&from, gutters) || !contained(&from) {
            return None;
        }
        let mut best = None;
        let mut best_gain = f32::NEG_INFINITY;
        let sizes: Vec<_> = from
            .tiles
            .iter()
            .enumerate()
            .map(|(repo, &tile)| {
                tile.filter(|_| repo != departing)
                    .map(|tile| glyph_size(repo, tile))
            })
            .collect();
        let mut least_donors = usize::MAX;
        let mut distance = f32::INFINITY;
        for axis in 0..2 {
            let low = Group::find(&from, departing, axis, true);
            let high = Group::find(&from, departing, axis, false);
            for part in 0..=8_u8 {
                let share = f32::from(part) / 8.0;
                let Some((to, donors)) = Self::close(
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
                let travel = exchange::edge_travel(&from, &to);
                if travel
                    > exchange::MAX_EDGE_STEP_PX
                        * held::REFLOW_LINEAR_FRACTION
                        * count(ticks)
                    || !to.tiles.iter().enumerate().all(|(repo, &tile)| {
                        repo == departing || tile.is_none_or(settled)
                    })
                    || !fills_field(&to, gutters)
                    || !contained(&to)
                    || !compatible(&from.tiles, &to.tiles)
                    || !compatible(&from.gutters, &to.gutters)
                {
                    continue;
                }
                let mut survivors = to.clone();
                survivors.tiles[departing] = None;
                survivors.gutters[departing] = None;
                if !connected(&from, &survivors, false) {
                    continue;
                }
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
                if gain
                    .total_cmp(&best_gain)
                    .then_with(|| least_donors.cmp(&donors))
                    .then_with(|| distance.total_cmp(&travel))
                    .is_le()
                {
                    continue;
                }
                best_gain = gain;
                least_donors = donors;
                distance = travel;
                best = Some(to);
            }
        }
        Some(Self {
            from,
            to: best?,
            departing,
        })
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
            layout.tiles[self.departing] = None;
            layout.gutters[self.departing] = None;
        }
        layout
    }
}

pub(super) fn best_slide(
    from: &Layout,
    space: Space,
    ticks: usize,
    score: impl Fn(&Layout) -> f32,
) -> Option<Layout> {
    let mut best_score = score(from) + 0.005;
    let mut best = None;
    let field = space.field(framing::MARGIN);
    let gutters = space.field(super::super::GUTTER_MARGIN);
    let gap = Vec2::splat(space.gap) * 2.0 / framing::VIEWPORT;
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
            for distance in [-2.0, -1.0, 1.0, 2.0] {
                let cut = group.cut + distance * gap[axis];
                let to = group.finish(from, None, cut, field);
                if !to.tiles.iter().flatten().copied().all(settled)
                    || exchange::edge_travel(from, &to)
                        > exchange::MAX_EDGE_STEP_PX
                            * held::REFLOW_LINEAR_FRACTION
                            * count(ticks)
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

    fn finish(
        &self,
        from: &Layout,
        retiring: Option<usize>,
        cut: f32,
        field: Tile,
    ) -> Layout {
        let mut result = from.clone();
        for member in &self.members {
            let cell = result.gutters[member.repo].as_mut().unwrap();
            let tile = result.tiles[member.repo].as_mut().unwrap();
            set_edge(cell, self.axis, member.high, cut);
            if Some(member.repo) == retiring {
                tile.low[self.axis] = cut;
                tile.high[self.axis] = cut;
            } else {
                let moved = (edge(*tile, self.axis, member.high) + cut - self.cut)
                    .clamp(field.low[self.axis], field.high[self.axis]);
                set_edge(tile, self.axis, member.high, moved);
            }
        }
        result
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

fn fills_field(layout: &Layout, field: Tile) -> bool {
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

fn contained(layout: &Layout) -> bool {
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
        let target = best_slide(&opening, space, 100, |layout| {
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
