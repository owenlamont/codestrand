use super::{Layout, Space, Tile, compatible, connected, exchange, retirement};
use crate::model::{count, framing};
use bevy::math::Vec2;

#[derive(Debug)]
pub(super) struct Birth {
    start: Layout,
    end: Layout,
    pub(super) guest: usize,
}

impl Birth {
    pub(super) fn plan(
        layout: &Layout,
        space: Space,
        guest: usize,
        weights: &[f32],
        ticks: usize,
    ) -> Option<Self> {
        if layout.tiles.get(guest)?.is_some() {
            return None;
        }
        let field = space.field(framing::MARGIN);
        let gap = Vec2::splat(space.gap) / framing::VIEWPORT;
        let speed = exchange::MAX_EDGE_STEP_PX * 0.75;
        let max_travel = speed * count(ticks);
        let mut best = None;
        let mut cost = f32::INFINITY;
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
                    [0.15, 0.20, 0.25, 1.0 / 3.0, 0.5]
                        .map(move |fraction| (axis, high, fraction))
                });
            for (axis, high, fraction) in cuts {
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
                let candidate_cost = (ratio - share).abs() + fraction * 0.1;
                if candidate_cost >= cost {
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
                best = Some(Self { start, end, guest });
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
