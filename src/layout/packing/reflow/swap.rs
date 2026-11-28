use super::{
    Layout, Space, Tile,
    exchange::{Exchange, MAX_EDGE_STEP_PX, edge_travel, side_by_side},
    held,
    lifecycle::glass_in_cell,
    opening::{aspect_excess, density_cost},
    retirement::{contained, fills_field, gutter_bounds, rectangle_bounds, settled},
};
use crate::model::framing;
use bevy::math::Vec2;

#[derive(Debug)]
pub(super) struct Swap {
    from: Layout,
    to: Layout,
    route: Exchange,
    pub(super) peak_gap: f32,
    pub(super) uncovered_ticks: usize,
}

impl Swap {
    pub(super) fn plan(
        from: &Layout,
        without: &Layout,
        space: Space,
        weights: &[f32],
    ) -> Option<Self> {
        if space.gap < super::MOVING_GUTTER_PX {
            return None;
        }
        let field = gutter_bounds(from)?;
        let glass = rectangle_bounds(&from.tiles)?;
        if !fills_field(from, field) || !contained(from) {
            return None;
        }
        let padding = Vec2::splat(space.gap) / framing::VIEWPORT;
        let mut best = None;
        let mut cost = density_cost(without, weights);
        let pairs = from
            .gutters
            .iter()
            .copied()
            .enumerate()
            .flat_map(|(a, cell)| {
                let right = from.gutters.iter().copied().enumerate().skip(a + 1);
                right.filter_map(move |(b, other)| neighbours((a, cell?), (b, other?)))
            });
        for (pair, union, along) in pairs {
            let contents = glass_in_cell(union, field, glass, padding);
            let candidates = [
                (along, [pair[1], pair[0]]),
                (1 - along, pair),
                (1 - along, [pair[1], pair[0]]),
            ];
            for (axis, order) in candidates {
                let Some(cells) = split(
                    contents,
                    union,
                    axis,
                    order.map(|repo| weights[repo]),
                    space.gap,
                ) else {
                    continue;
                };
                let mut to = from.clone();
                for (repo, cell) in order.into_iter().zip(cells) {
                    to.gutters[repo] = Some(cell);
                    to.tiles[repo] = Some(glass_in_cell(cell, field, glass, padding));
                }
                let score = density_cost(&to, weights);
                if score >= cost || aspect_excess(&to) > aspect_excess(without) + 1e-6 {
                    continue;
                }
                if let Some(swap) = Self::certify(from, to, pair, union, field, glass) {
                    cost = score;
                    best = Some(swap);
                }
            }
        }
        best
    }

    fn certify(
        from: &Layout,
        to: Layout,
        pair: [usize; 2],
        union: Tile,
        field: Tile,
        glass: Tile,
    ) -> Option<Self> {
        if !fills_field(&to, field) || !contained(&to) {
            return None;
        }
        let route = Exchange::plan(from, &to, pair, glass)?;
        let mut previous = from.clone();
        let mut peak_gap = 0.0_f32;
        let mut uncovered_ticks = 0;
        for tick in 0..=held::reflow_ticks() {
            let next = route.sample(from, &to, tick);
            if edge_travel(&previous, &next) > MAX_EDGE_STEP_PX + 0.001 {
                return None;
            }
            let covered = covered_area(&next, pair, union)?;
            let gap = (1.0 - covered / union.area()).max(0.0);
            if gap > 1e-6 {
                uncovered_ticks += 1;
                peak_gap = peak_gap.max(gap);
            }
            previous = next;
        }
        Some(Self {
            from: from.clone(),
            to,
            route,
            peak_gap,
            uncovered_ticks,
        })
    }

    pub(super) fn sample(&self, elapsed: usize) -> Layout {
        self.route.sample(&self.from, &self.to, elapsed)
    }

    pub(super) fn settled(&self) -> Layout {
        self.to.clone()
    }

    pub(super) fn repositories(&self) -> [usize; 2] {
        self.route.repositories
    }
}

fn neighbours(
    (mut a, mut first): (usize, Tile),
    (mut b, mut second): (usize, Tile),
) -> Option<([usize; 2], Tile, usize)> {
    let along = side_by_side(first, second).or_else(|| {
        std::mem::swap(&mut a, &mut b);
        std::mem::swap(&mut first, &mut second);
        side_by_side(first, second)
    })?;
    Some((
        [a, b],
        Tile {
            low: first.low,
            high: second.high,
        },
        along,
    ))
}

fn split(
    contents: Tile,
    union: Tile,
    axis: usize,
    [first, second]: [f32; 2],
    gap: f32,
) -> Option<[Tile; 2]> {
    let size = contents.pixels();
    let span = size[axis] - gap;
    let cross = size[1 - axis];
    let minimum = (cross / super::super::BOUND).max(16.0) + 0.001;
    let maximum = cross * super::super::BOUND - 0.001;
    let low = minimum.max(span - maximum);
    let high = maximum.min(span - minimum);
    if low > high || cross < 16.0 || first <= 0.0 || second <= 0.0 {
        return None;
    }
    let length = (span * first / (first + second)).clamp(low, high);
    let cut = contents.low[axis] + (length + gap * 0.5) * 2.0 / framing::VIEWPORT[axis];
    let mut cells = [union; 2];
    cells[0].high[axis] = cut;
    cells[1].low[axis] = cut;
    Some(cells)
}

fn covered_area(layout: &Layout, [a, b]: [usize; 2], union: Tile) -> Option<f32> {
    let (a_cell, b_cell) = (layout.gutters[a]?, layout.gutters[b]?);
    let disjoint = (a_cell.high.min(b_cell.high) - a_cell.low.max(b_cell.low))
        .min_element()
        <= 0.0;
    let within = |cell: Tile| {
        cell.low.cmpge(union.low).all() && cell.high.cmple(union.high).all()
    };
    let inside = |repo: usize, cell: Tile| {
        layout.tiles[repo].is_some_and(|tile| {
            settled(tile)
                && tile.low.cmpge(cell.low).all()
                && tile.high.cmple(cell.high).all()
        })
    };
    (disjoint
        && within(a_cell)
        && within(b_cell)
        && inside(a, a_cell)
        && inside(b, b_cell))
    .then(|| a_cell.area() + b_cell.area())
}
