use super::{Layout, Space, Tile, compatible, connected, exchange, held};
use crate::model::{count, framing};
use bevy::math::Vec2;

const EPSILON: f32 = 1e-6;

#[derive(Clone, Debug)]
pub(in crate::layout::packing) struct Retirement {
    from: Layout,
    to: Layout,
    departing: usize,
    field: Tile,
}

impl Retirement {
    pub(in crate::layout::packing) fn plan(
        from: Layout,
        departing: usize,
        space: Space,
        ticks: usize,
    ) -> Option<Self> {
        let field = space.field(framing::MARGIN);
        let gutters = space.field(super::super::GUTTER_MARGIN);
        let retiring = from.gutters.get(departing).copied().flatten()?;
        if !fills_field(&from, gutters) || !contained(&from) {
            return None;
        }
        let mut best = None;
        let mut least_donors = usize::MAX;
        let mut distance = f32::INFINITY;
        for axis in 0..2 {
            for grows_high in [false, true] {
                let Some(group) = Group::find(&from, departing, axis, grows_high)
                else {
                    continue;
                };
                let to = group.finish(
                    &from,
                    departing,
                    edge(retiring, axis, grows_high),
                    field,
                );
                let travel = exchange::edge_travel(&from, &to);
                if travel
                    > exchange::MAX_EDGE_STEP_PX
                        * held::REFLOW_LINEAR_FRACTION
                        * count(ticks)
                    || group.donors > least_donors
                    || (group.donors == least_donors && travel >= distance)
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
                least_donors = group.donors;
                distance = travel;
                best = Some(to);
            }
        }
        Some(Self {
            from,
            to: best?,
            departing,
            field,
        })
    }

    fn interpolate(&self, progress: f32) -> Layout {
        let mut layout = self.from.clone();
        let blend = |rects: &mut [Option<Tile>], target: &[Option<Tile>]| {
            for (rect, goal) in rects.iter_mut().zip(target) {
                if let (Some(rect), Some(goal)) = (rect, goal) {
                    rect.low += (goal.low - rect.low) * progress;
                    rect.high += (goal.high - rect.high) * progress;
                }
            }
        };
        blend(&mut layout.tiles, &self.to.tiles);
        blend(&mut layout.gutters, &self.to.gutters);
        layout
    }

    pub(in crate::layout::packing) fn sample(&self, presence: f32) -> Layout {
        if presence >= 1.0 {
            return self.from.clone();
        }
        let mut layout = if presence <= 0.0 {
            self.to.clone()
        } else {
            self.interpolate(1.0 - presence)
        };
        if presence <= 0.0 {
            layout.tiles[self.departing] = None;
            layout.gutters[self.departing] = None;
        } else if let Some(mut tile) = layout.tiles[self.departing] {
            tile.low = tile.low.max(self.field.low);
            tile.high = tile.high.min(self.field.high);
            layout.tiles[self.departing] =
                tile.low.cmplt(tile.high).all().then_some(tile);
        }
        layout
    }
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

    fn finish(&self, from: &Layout, retiring: usize, cut: f32, field: Tile) -> Layout {
        let mut result = from.clone();
        for member in &self.members {
            let cell = result.gutters[member.repo].as_mut().unwrap();
            let tile = result.tiles[member.repo].as_mut().unwrap();
            set_edge(cell, self.axis, member.high, cut);
            if member.repo == retiring {
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

fn settled(tile: Tile) -> bool {
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
        let change = Retirement::plan(opening, 4, space, 240).unwrap();
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
            .finish(&before, 4, cells[4].low.x, glass)
            .tiles
        ));
        let after = change.sample(0.0);
        assert!(after.tiles[4].is_none());
        let changed = before.tiles[..4]
            .iter()
            .zip(&after.tiles[..4])
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(changed, 2);
        let shrunk = before.tiles[..4]
            .iter()
            .zip(&after.tiles[..4])
            .filter(|(a, b)| a.unwrap().area() > b.unwrap().area())
            .count();
        assert_eq!(shrunk, 1);
        let fixed: Vec<_> = before.tiles[..4]
            .iter()
            .zip(&after.tiles[..4])
            .enumerate()
            .filter_map(|(repo, (a, b))| (a == b).then_some((repo, *a)))
            .collect();
        let mut previous = before;
        for point in (0..=100_u8).rev() {
            let current = change.sample(f32::from(point) / 100.0);
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
