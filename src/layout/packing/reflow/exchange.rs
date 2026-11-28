use bevy::math::Vec2;

use super::{Layout, Tile, compatible, connected, held};
use crate::model::framing;

pub(super) const MAX_EDGE_STEP_PX: f32 = 8.0;
const MIN_SIDE_PX: f32 = 16.0;
const PHASES: usize = 5;

#[derive(Clone, Copy, Debug)]
struct Footprint {
    tile: Tile,
    gutter: Tile,
}

impl Footprint {
    fn at(layout: &Layout, repo: usize) -> Option<Self> {
        Some(Self {
            tile: layout.tiles[repo]?,
            gutter: layout.gutters[repo]?,
        })
    }

    fn inside(self, gutter: Tile) -> Self {
        let scale = (gutter.high - gutter.low) / (self.gutter.high - self.gutter.low);
        Self {
            gutter,
            tile: Tile {
                low: gutter.low + (self.tile.low - self.gutter.low) * scale,
                high: gutter.high - (self.gutter.high - self.tile.high) * scale,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Exchange {
    pub(super) repositories: [usize; 2],
    waypoints: [[Footprint; 2]; PHASES - 1],
    durations: [usize; PHASES],
}

impl Exchange {
    pub(super) fn plan(
        from: &Layout,
        to: &Layout,
        repositories: [usize; 2],
        field: Tile,
    ) -> Option<Self> {
        let (mut route, phases) = Self::geometry(from, to, repositories, field)?;
        route.allocate_time(from, to, phases)?;
        Some(route)
    }

    fn geometry(
        from: &Layout,
        to: &Layout,
        mut repositories: [usize; 2],
        field: Tile,
    ) -> Option<(Self, usize)> {
        let [a, b] = repositories;
        let (mut a, mut b) = (Footprint::at(from, a)?, Footprint::at(from, b)?);
        let axis = side_by_side(a.gutter, b.gutter).or_else(|| {
            repositories.swap(0, 1);
            std::mem::swap(&mut a, &mut b);
            side_by_side(a.gutter, b.gutter)
        })?;
        let lane = 1 - axis;
        let cut = a.gutter.low[lane].midpoint(a.gutter.high[lane]);
        let [left, right] = repositories;
        let (target_a, target_b) = (to.gutters[left]?, to.gutters[right]?);
        let turn = if side_by_side(target_a, target_b) == Some(lane) {
            Some(true)
        } else if side_by_side(target_b, target_a) == Some(lane) {
            Some(false)
        } else {
            None
        };
        let low_lane = |mut cell: Tile| {
            cell.high[lane] = cut;
            cell
        };
        let high_lane = |mut cell: Tile| {
            cell.low[lane] = cut;
            cell
        };
        let band = Tile {
            low: a.gutter.low,
            high: b.gutter.high,
        };
        let first = turn.unwrap_or(true);
        let lanes = |cell, low| if low { low_lane(cell) } else { high_lane(cell) };
        let shelves = [a.inside(lanes(band, first)), b.inside(lanes(band, !first))];
        let phases = if turn.is_some() { 3 } else { PHASES };
        let mut route = Self {
            repositories,
            waypoints: [
                [
                    a.inside(lanes(a.gutter, first)),
                    b.inside(lanes(b.gutter, !first)),
                ],
                shelves,
                [b.inside(low_lane(b.gutter)), a.inside(high_lane(a.gutter))],
                [b, a],
            ],
            durations: [0; PHASES],
        };
        for point in route.waypoints[..phases - 1].iter_mut().flatten() {
            point.tile.low = point.tile.low.max(field.low);
            point.tile.high = point.tile.high.min(field.high);
        }
        Some((route, phases))
    }

    fn allocate_time(
        &mut self,
        from: &Layout,
        to: &Layout,
        phases: usize,
    ) -> Option<()> {
        for points in &self.waypoints[..phases - 1] {
            for (&repo, point) in self.repositories.iter().zip(points) {
                let (start, end) = (from.tiles[repo]?, to.tiles[repo]?);
                if !has_room(point.tile, start, end) {
                    return None;
                }
            }
        }
        let budget = held::reflow_ticks();
        let mut before = from.clone();
        for phase in 1..=phases {
            let after = self.layout(from, to, phase, phases);
            self.durations[phase - 1] = step_ticks(&before, &after, budget)?;
            before = after;
        }
        let extra = budget.checked_sub(self.durations.iter().sum())?;
        for (phase, duration) in self.durations[..phases].iter_mut().enumerate() {
            *duration += extra / phases + usize::from(phase < extra % phases);
        }
        Some(())
    }

    fn layout(
        &self,
        from: &Layout,
        to: &Layout,
        index: usize,
        phases: usize,
    ) -> Layout {
        if index == phases {
            return to.clone();
        }
        let mut result = from.clone();
        if let Some(index) = index.checked_sub(1) {
            for (&repo, place) in self.repositories.iter().zip(&self.waypoints[index]) {
                result.tiles[repo] = Some(place.tile);
                result.gutters[repo] = Some(place.gutter);
            }
        }
        result
    }

    pub(super) fn sample(
        &self,
        from: &Layout,
        to: &Layout,
        mut elapsed: usize,
    ) -> Layout {
        let phases = self
            .durations
            .iter()
            .take_while(|&&duration| duration > 0)
            .count();
        for (phase, &duration) in self.durations[..phases].iter().enumerate() {
            if elapsed < duration {
                return self.layout(from, to, phase, phases).blend(
                    &self.layout(from, to, phase + 1, phases),
                    held::progress(elapsed, duration),
                );
            }
            elapsed -= duration;
        }
        to.clone()
    }
}

const CHAIN_STEPS: usize = 8;

#[derive(Clone, Debug)]
pub(super) struct Chain {
    pub(super) repositories: [usize; 3],
    waypoints: [[Footprint; 3]; CHAIN_STEPS - 1],
    durations: [usize; CHAIN_STEPS],
}

impl Chain {
    pub(super) fn plan(
        from: &Layout,
        to: &Layout,
        repositories: [usize; 3],
        field: Tile,
        budget: usize,
    ) -> Option<Self> {
        let [traveller, first, second] = repositories;
        let mut middle = from.clone();
        middle.tiles.swap(traveller, first);
        middle.gutters.swap(traveller, first);
        let (a, first_phases) =
            Exchange::geometry(from, &middle, [traveller, first], field)?;
        let (b, second_phases) =
            Exchange::geometry(&middle, to, [traveller, second], field)?;
        if first_phases != PHASES || second_phases != PHASES {
            return None;
        }
        let poses = [
            a.layout(from, &middle, 1, PHASES),
            a.layout(from, &middle, 2, PHASES),
            a.layout(from, &middle, 3, PHASES),
            b.layout(&middle, to, 1, PHASES),
            b.layout(&middle, to, 2, PHASES),
            b.layout(&middle, to, 3, PHASES),
            b.layout(&middle, to, 4, PHASES),
        ];
        if poses[2].tiles[traveller] != poses[3].tiles[traveller]
            || poses[2].gutters[traveller] != poses[3].gutters[traveller]
        {
            return None;
        }
        let capture = |pose: &Layout| {
            repositories
                .map(|repo| Footprint::at(pose, repo).expect("three visible tiles"))
        };
        let waypoints = poses.each_ref().map(capture);
        for points in &waypoints {
            for (&repo, place) in repositories.iter().zip(points) {
                let (start, end) = (from.tiles[repo]?, to.tiles[repo]?);
                if !has_room(place.tile, start, end) {
                    return None;
                }
            }
        }
        let durations = time_stages(from, &poses, to, budget)?;
        Some(Self {
            repositories,
            waypoints,
            durations,
        })
    }

    #[cfg(test)]
    pub(super) fn assert_compressed_handoff(
        &self,
        from: &Layout,
        to: &Layout,
        repositories: [usize; 3],
    ) {
        assert_eq!(self.repositories, repositories);
        let [traveller, _, _] = repositories;
        let before = self.layout(from, to, 3).tiles[traveller].unwrap();
        let after = self.layout(from, to, 4).tiles[traveller].unwrap();
        assert_eq!(before, after);
        assert!(before.area() < 0.6 * from.tiles[traveller].unwrap().area());
    }

    pub(super) fn duration(&self) -> usize {
        self.durations.iter().sum()
    }

    fn layout(&self, from: &Layout, to: &Layout, index: usize) -> Layout {
        if index == CHAIN_STEPS {
            return to.clone();
        }
        let mut layout = from.clone();
        if let Some(index) = index.checked_sub(1) {
            for (&repo, pose) in self.repositories.iter().zip(&self.waypoints[index]) {
                layout.tiles[repo] = Some(pose.tile);
                layout.gutters[repo] = Some(pose.gutter);
            }
        }
        layout
    }

    pub(super) fn sample(
        &self,
        from: &Layout,
        to: &Layout,
        mut elapsed: usize,
    ) -> Layout {
        for (phase, &duration) in self.durations.iter().enumerate() {
            if elapsed < duration {
                return self.layout(from, to, phase).blend(
                    &self.layout(from, to, phase + 1),
                    held::progress(elapsed, duration),
                );
            }
            elapsed -= duration;
        }
        to.clone()
    }
}

#[derive(Clone, Copy)]
pub(super) struct Restack {
    pub repositories: [usize; 4],
    pub across_axis: usize,
    pub starts_on_low_side: bool,
    pub enters_at_low_end: bool,
}

#[derive(Clone, Debug)]
pub(super) struct ColumnTransfer {
    pub(super) repositories: [usize; 4],
    waypoints: [[Footprint; 4]; 3],
    durations: [usize; 4],
}

impl Restack {
    fn waypoints(self, from: &Layout, to: &Layout) -> Option<[[Footprint; 4]; 3]> {
        let [staying, moving, _, _] = self.repositories;
        let across = self.across_axis;
        let along = 1 - across;
        let large = to.gutters[staying]?;
        let destination = to.gutters[moving]?;
        let width = (large.high[across] - large.low[across])
            .min(destination.high[across] - destination.low[across]);
        let mut lane = destination;
        let mut large_open = large;
        // The moving cell narrows beside the common cut before sliding down the lane.
        if self.starts_on_low_side {
            lane.high[across] = large.high[across];
            lane.low[across] = lane.high[across] - width;
        } else {
            lane.low[across] = large.low[across];
            lane.high[across] = lane.low[across] + width;
        }
        if self.enters_at_low_end {
            large_open.low[along] = destination.high[along];
        } else {
            large_open.high[along] = destination.low[along];
        }

        let mut aligned = [Footprint::at(to, staying)?; 4];
        let mut open = aligned;
        for (index, &repository) in self.repositories.iter().enumerate() {
            let end = Footprint::at(to, repository)?;
            let mut alignment = from.gutters[repository]?;
            alignment.low[across] = end.gutter.low[across];
            alignment.high[across] = end.gutter.high[across];
            let opening = match index {
                0 => large_open,
                1 => {
                    alignment.low[across] = lane.low[across];
                    alignment.high[across] = lane.high[across];
                    lane
                }
                _ => end.gutter,
            };
            aligned[index] = resized_inset(end, alignment);
            open[index] = resized_inset(end, opening);
        }
        let mut crossed = open;
        crossed[1] = Footprint::at(to, moving)?;
        Some([aligned, open, crossed])
    }
}

fn resized_inset(end: Footprint, gutter: Tile) -> Footprint {
    Footprint {
        gutter,
        tile: Tile {
            low: gutter.low + (end.tile.low - end.gutter.low),
            high: gutter.high + (end.tile.high - end.gutter.high),
        },
    }
}

impl ColumnTransfer {
    pub(super) fn plan(
        from: &Layout,
        to: &Layout,
        restack: Restack,
        field: Tile,
        budget: usize,
    ) -> Option<Self> {
        let mut route = Self {
            repositories: restack.repositories,
            waypoints: restack.waypoints(from, to)?,
            durations: [0; 4],
        };
        for points in &mut route.waypoints {
            for (&repository, point) in route.repositories.iter().zip(points) {
                point.tile.low = point.tile.low.max(field.low);
                point.tile.high = point.tile.high.min(field.high);
                if !has_room(point.tile, from.tiles[repository]?, to.tiles[repository]?)
                {
                    return None;
                }
            }
        }

        let poses = [1, 2, 3].map(|index| route.layout(from, to, index));
        route.durations = time_stages(from, &poses, to, budget)?;
        Some(route)
    }

    pub(super) fn duration(&self) -> usize {
        self.durations.iter().sum()
    }

    fn layout(&self, from: &Layout, to: &Layout, index: usize) -> Layout {
        if index > self.waypoints.len() {
            return to.clone();
        }
        let mut layout = from.clone();
        if let Some(index) = index.checked_sub(1) {
            for (&repository, point) in
                self.repositories.iter().zip(self.waypoints[index])
            {
                layout.tiles[repository] = Some(point.tile);
                layout.gutters[repository] = Some(point.gutter);
            }
        }
        layout
    }

    pub(super) fn sample(
        &self,
        from: &Layout,
        to: &Layout,
        mut elapsed: usize,
    ) -> Layout {
        for (phase, &ticks) in self.durations.iter().enumerate() {
            if elapsed < ticks {
                return self.layout(from, to, phase).blend(
                    &self.layout(from, to, phase + 1),
                    held::progress(elapsed, ticks),
                );
            }
            elapsed -= ticks;
        }
        to.clone()
    }
}

fn time_stages<'a, const COUNT: usize>(
    mut previous: &'a Layout,
    poses: &'a [Layout],
    destination: &'a Layout,
    budget: usize,
) -> Option<[usize; COUNT]> {
    let mut durations = [0; COUNT];
    let mut remaining = budget;
    for (next, duration) in poses
        .iter()
        .chain(std::iter::once(destination))
        .zip(&mut durations)
    {
        *duration = step_ticks(previous, next, remaining)?;
        remaining -= *duration;
        previous = next;
    }
    let extra = held::reflow_ticks().saturating_sub(budget - remaining);
    if extra > remaining {
        return None;
    }
    for (index, ticks) in durations.iter_mut().enumerate() {
        *ticks += extra / COUNT + usize::from(index < extra % COUNT);
    }
    Some(durations)
}

fn has_room(tile: Tile, start: Tile, end: Tile) -> bool {
    tile.pixels().min_element() >= MIN_SIDE_PX
        && tile.area() >= 0.49 * start.area().min(end.area())
}

fn step_ticks(from: &Layout, to: &Layout, budget: usize) -> Option<usize> {
    if !compatible(&from.gutters, &to.gutters) || !connected(from, to, true) {
        return None;
    }
    let travel = edge_travel(from, to);
    (1..=budget).find(|&ticks| {
        travel
            <= MAX_EDGE_STEP_PX
                * crate::model::count(ticks)
                * held::REFLOW_LINEAR_FRACTION
    })
}

pub(super) fn edge_travel(from: &Layout, to: &Layout) -> f32 {
    let pixels = framing::VIEWPORT * 0.5;
    from.tiles
        .iter()
        .zip(&to.tiles)
        .chain(from.gutters.iter().zip(&to.gutters))
        .filter_map(|(&a, &b)| a.zip(b))
        .map(|(a, b)| {
            ((a.low - b.low).abs() * pixels).max((a.high - b.high).abs() * pixels)
        })
        .fold(Vec2::ZERO, Vec2::max)
        .max_element()
}

pub(super) fn side_by_side(a: Tile, b: Tile) -> Option<usize> {
    (0..2).find(|&axis| {
        let cross = 1 - axis;
        a.high[axis].to_bits() == b.low[axis].to_bits()
            && a.low[cross].to_bits() == b.low[cross].to_bits()
            && a.high[cross].to_bits() == b.high[cross].to_bits()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIELD: Tile = Tile {
        low: Vec2::ZERO,
        high: Vec2::ONE,
    };

    fn row(height: f32) -> Layout {
        let gutters: Vec<_> = (0..3_u8)
            .map(|i| {
                let x = f32::from(i) * 0.125;
                Some(Tile {
                    low: Vec2::new(x, 0.0),
                    high: Vec2::new(x + 0.125, height),
                })
            })
            .collect();
        let tiles = gutters
            .iter()
            .map(|cell| {
                cell.map(|cell| Tile {
                    low: cell.low + Vec2::splat(1.0 / 128.0),
                    high: cell.high - Vec2::splat(1.0 / 128.0),
                })
            })
            .collect();
        Layout { tiles, gutters }
    }

    fn swapped(layout: &Layout, [a, b]: [usize; 2]) -> Layout {
        let mut result = layout.clone();
        result.tiles.swap(a, b);
        result.gutters.swap(a, b);
        result
    }

    #[test]
    fn an_exchange_can_pass_two_neighbours_with_connected_gutters() {
        let start = row(0.5);
        let field = Tile {
            low: Vec2::splat(1.0 / 128.0),
            ..FIELD
        };
        let first = swapped(&start, [0, 1]);
        let last = swapped(&first, [0, 2]);
        assert!(!compatible(&start.tiles, &first.tiles));
        for (from, to, pair) in [(&start, &first, [0, 1]), (&first, &last, [0, 2])] {
            let path = Exchange::plan(from, to, pair, field).unwrap();
            let mut before = from.clone();
            for tick in 0..=held::reflow_ticks() {
                let after = if tick == held::reflow_ticks() {
                    to.clone()
                } else {
                    path.sample(from, to, tick)
                };
                assert!(compatible(&before.tiles, &after.tiles), "tick {tick}");
                for tile in after.tiles.iter().flatten() {
                    assert!(tile.low.cmpge(field.low).all());
                    assert!(tile.high.cmple(field.high).all());
                }
                assert!(edge_travel(&before, &after) <= MAX_EDGE_STEP_PX + 1e-4);
                crate::model::assert_gutter_routes(
                    after.gutters.iter().flatten().copied(),
                );
                before = after;
            }
        }
        assert_eq!(last.tiles[0], start.tiles[2]);
    }

    #[test]
    fn an_exchange_rejects_a_corridor_with_no_room_to_pass() {
        let start = row(0.0625);
        let end = swapped(&start, [0, 1]);
        assert!(Exchange::plan(&start, &end, [0, 1], FIELD).is_none());
        let non_adjacent = swapped(&start, [0, 2]);
        assert!(Exchange::plan(&start, &non_adjacent, [0, 2], FIELD).is_none());
    }
}
