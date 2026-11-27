use super::{Node, Space, Tile, gutter_cells, held, insert, tiles};

mod exchange;
pub(super) mod opening;
pub(super) mod retirement;

const MAX_LOCAL_SPLITS: usize = 8;
const MOVING_GUTTER_PX: f32 = 4.0;
const MAX_REBUILD_TILES: usize = 8;
const MAX_CHAIN_CANDIDATES: usize = 16;
pub(super) const MAX_MOTION_TICKS: usize = 4 * crate::model::TICK_RATE as usize;

#[derive(Clone, Copy)]
pub(super) struct Policy<'a> {
    pub stability: f32,
    pub available: usize,
    pub forecast: &'a [f32],
}

#[derive(Clone, Debug)]
enum Motion {
    Exchange(Box<exchange::Exchange>),
    Chain(Box<exchange::Chain>),
    ColumnTransfer(Box<exchange::ColumnTransfer>),
}

#[derive(Clone, Debug)]
pub(super) struct Change {
    pub tick: usize,
    pub nodes: Vec<Node>,
    transition: Option<(Layout, Layout)>,
    exchange: Option<Motion>,
}

#[derive(Clone, Debug)]
pub(super) struct Layout {
    pub tiles: Vec<Option<Tile>>,
    pub gutters: Vec<Option<Tile>>,
}

impl Layout {
    pub fn new(nodes: &[Node], tiles: Vec<Option<Tile>>, space: Space) -> Self {
        Self {
            gutters: gutter_cells(nodes, &tiles, space),
            tiles,
        }
    }
}

impl Change {
    pub fn retained_bytes() -> u128 {
        let route = std::mem::size_of::<exchange::Chain>()
            .max(std::mem::size_of::<exchange::ColumnTransfer>());
        (std::mem::size_of::<Self>() + route) as u128
    }

    pub fn duration(&self) -> usize {
        match &self.exchange {
            Some(Motion::Chain(chain)) => chain.duration(),
            Some(Motion::ColumnTransfer(route)) => route.duration(),
            _ => held::reflow_ticks(),
        }
    }

    pub fn weights(tick: usize, nodes: &[Node]) -> Self {
        Self {
            tick,
            nodes: nodes.to_vec(),
            transition: None,
            exchange: None,
        }
    }

    pub fn in_transition(&self, tick: usize) -> bool {
        self.transition.is_some() && tick - self.tick < self.duration()
    }

    pub fn matches_presence(&self, presence: &[f32]) -> bool {
        let Some((from, _)) = &self.transition else {
            return false;
        };
        from.tiles.iter().zip(presence).all(|(tile, &p)| {
            if tile.is_some() { p >= 1.0 } else { p <= 0.0 }
        })
    }

    pub fn sample(&self, tick: usize) -> Option<Layout> {
        let (from, to) = self.transition.as_ref()?;
        let elapsed = tick - self.tick;
        if elapsed >= self.duration() {
            return None;
        }
        Some(match &self.exchange {
            Some(Motion::Exchange(exchange)) => exchange.sample(from, to, elapsed),
            Some(Motion::Chain(chain)) => chain.sample(from, to, elapsed),
            Some(Motion::ColumnTransfer(route)) => route.sample(from, to, elapsed),
            None => from.blend(to, held::reflow_ease(elapsed)),
        })
    }
}

impl Layout {
    fn blend_edges(&self, to: &Self, progress: f32) -> Self {
        let mut layout = self.clone();
        let blend = |rects: &mut [Option<Tile>], target: &[Option<Tile>]| {
            for (rect, goal) in rects.iter_mut().zip(target) {
                if let (Some(rect), Some(goal)) = (rect, goal) {
                    rect.low += (goal.low - rect.low) * progress;
                    rect.high += (goal.high - rect.high) * progress;
                }
            }
        };
        blend(&mut layout.tiles, &to.tiles);
        blend(&mut layout.gutters, &to.gutters);
        layout
    }

    fn blend(&self, to: &Self, fraction: f32) -> Self {
        let blend = |from: &[Option<Tile>], to: &[Option<Tile>]| {
            from.iter()
                .zip(to)
                .map(|(&a, &b)| {
                    a.zip(b).map(|(a, b)| Tile {
                        low: a.low.lerp(b.low, fraction),
                        high: a.high.lerp(b.high, fraction),
                    })
                })
                .collect()
        };
        Self {
            tiles: blend(&self.tiles, &to.tiles),
            gutters: blend(&self.gutters, &to.gutters),
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "candidate moves share the allocation and scoring state"
)]
pub(super) fn plan(
    tick: usize,
    nodes: &[Node],
    weights: &[f32],
    target: &[f32],
    presence: &[f32],
    space: Space,
    policy: Policy<'_>,
) -> Option<Change> {
    let from = tiles(nodes, weights, presence, space)?;
    let from = Layout::new(nodes, from, space);
    let unchanged = Layout::new(nodes, tiles(nodes, target, presence, space)?, space);
    let baseline = Density::new(&unchanged.tiles, target);
    let mut best = baseline;
    let mut best_motion = 0.0;
    if best.worst <= super::DENSITY_TOLERANCE {
        return Some(Change {
            tick,
            nodes: nodes.to_vec(),
            transition: Some((from, unchanged)),
            exchange: None,
        });
    }
    let splits = affected_splits(nodes, &unchanged.tiles, target);
    let mut chosen = (nodes.to_vec(), unchanged, None);
    let mut consider = |trial: Vec<Node>, pair: Option<[usize; 2]>| {
        if let Some(to) = tiles(&trial, target, presence, space) {
            let cost = Density::new(&to, target);
            let to = Layout::new(&trial, to, space);
            let motion = movement_cost(&from, &to, policy.stability, 1.0);
            if cost.improves_with_motion(best, baseline, [motion, best_motion]) {
                let direct = compatible(&from.gutters, &to.gutters)
                    && connected(&from, &to, false);
                let exchange = pair.filter(|_| !direct).and_then(|pair| {
                    exchange::Exchange::plan(
                        &from,
                        &to,
                        pair,
                        space.field(super::framing::MARGIN),
                    )
                });
                if direct || exchange.is_some() {
                    best = cost;
                    best_motion = motion;
                    chosen = (
                        trial,
                        to,
                        exchange.map(|path| Motion::Exchange(Box::new(path))),
                    );
                }
            }
        }
    };
    for &index in &splits {
        let node = &nodes[index];
        let Node::Split {
            axis,
            first,
            second,
        } = *node
        else {
            continue;
        };
        let mut trial = nodes.to_vec();
        trial[index] = Node::Split {
            axis: 1 - axis,
            first,
            second,
        };
        let pair = match (&nodes[first], &nodes[second]) {
            (Node::Repository(a), Node::Repository(b))
                if space.gap >= MOVING_GUTTER_PX =>
            {
                Some([*a, *b])
            }
            _ => None,
        };
        consider(trial, pair);
        if let (
            Node::Split {
                axis: a,
                first: top_left,
                second: bottom_left,
            },
            Node::Split {
                axis: b,
                first: top_right,
                second: bottom_right,
            },
        ) = (&nodes[first], &nodes[second])
            && *a == 1 - axis
            && *b == 1 - axis
        {
            let mut trial = nodes.to_vec();
            trial[index] = Node::Split {
                axis: 1 - axis,
                first,
                second,
            };
            trial[first] = Node::Split {
                axis,
                first: *top_left,
                second: *top_right,
            };
            trial[second] = Node::Split {
                axis,
                first: *bottom_left,
                second: *bottom_right,
            };
            consider(trial, None);
        }
        for (child, other, first_child) in
            [(first, second, true), (second, first, false)]
        {
            let Node::Split {
                axis: child_axis,
                first: a,
                second: b,
            } = nodes[child]
            else {
                continue;
            };
            if child_axis != axis {
                continue;
            }
            let (first, second, inner_first, inner_second) = if first_child {
                (a, child, b, other)
            } else {
                (child, b, other, a)
            };
            let mut trial = nodes.to_vec();
            trial[index] = Node::Split {
                axis,
                first,
                second,
            };
            trial[child] = Node::Split {
                axis,
                first: inner_first,
                second: inner_second,
            };
            consider(trial, None);
        }
    }
    let mut order: Vec<_> = (0..target.len()).collect();
    order.sort_by(|&a, &b| target[b].total_cmp(&target[a]));
    let mut trial = Vec::new();
    let mut inserted = vec![0.0; target.len()];
    let aspects = vec![1.0; target.len()];
    if target.len() <= MAX_REBUILD_TILES
        && presence.iter().all(|&p| p > 0.0)
        && order.into_iter().all(|i| {
            inserted[i] = target[i];
            insert(&mut trial, i, &inserted, &aspects, None, space, 1)
        })
    {
        consider(trial, None);
    }
    if space.gap >= MOVING_GUTTER_PX {
        for (a, b, repositories) in
            exchange_candidates(nodes, &splits, &from.gutters, target)
        {
            let mut trial = nodes.to_vec();
            trial.swap(a, b);
            consider(trial, Some(repositories));
        }
    }
    if best.worst > super::DENSITY_TOLERANCE
        && space.gap >= MOVING_GUTTER_PX
        && policy.available >= held::reflow_ticks()
    {
        let original = &chosen.1;
        let forecast_baseline = Density::new(&original.tiles, policy.forecast);
        let mut best_forecast = forecast_baseline;
        let mut best_chain = None;
        let errors = density_errors(&chosen.1.tiles, target);
        for (leaves, repositories) in
            chain_candidates(nodes, &from, weights, target, &errors)
        {
            let mut trial = nodes.to_vec();
            trial.swap(leaves[0], leaves[1]);
            trial.swap(leaves[1], leaves[2]);
            let Some(allocation) = tiles(&trial, target, presence, space) else {
                continue;
            };
            let cost = Density::new(&allocation, target);
            let forecast = Density::new(&allocation, policy.forecast);
            let to = Layout::new(&trial, allocation, space);
            let motion = movement_cost(&from, &to, policy.stability, 2.0);
            if !cost.improves_with_motion(best, baseline, [motion, best_motion])
                || !forecast.improves(best_forecast, forecast_baseline)
            {
                continue;
            }
            if let Some(chain) = exchange::Chain::plan(
                &from,
                &to,
                repositories,
                space.field(super::framing::MARGIN),
                policy.available.min(MAX_MOTION_TICKS),
            ) {
                best = cost;
                best_motion = motion;
                best_forecast = forecast;
                best_chain = Some((trial, to, Some(Motion::Chain(Box::new(chain)))));
            }
        }
        if let Some(chain) = best_chain {
            chosen = chain;
        }
    }
    if best.worst > super::DENSITY_TOLERANCE
        && space.gap >= MOVING_GUTTER_PX
        && policy.available >= held::reflow_ticks()
    {
        let forecast_baseline = Density::new(&chosen.1.tiles, policy.forecast);
        let mut best_forecast = forecast_baseline;
        for candidate in restack_candidates(nodes, &splits) {
            let mut trial = nodes.to_vec();
            for (index, node) in candidate.splits {
                trial[index] = node;
            }
            let Some(allocation) = tiles(&trial, target, presence, space) else {
                continue;
            };
            let cost = Density::new(&allocation, target);
            let forecast = Density::new(&allocation, policy.forecast);
            let to = Layout::new(&trial, allocation, space);
            let motion = movement_cost(&from, &to, policy.stability, 2.0);
            if !cost.improves_with_motion(best, baseline, [motion, best_motion])
                || !forecast.improves(best_forecast, forecast_baseline)
            {
                continue;
            }
            if let Some(route) = exchange::ColumnTransfer::plan(
                &from,
                &to,
                candidate.route,
                space.field(super::framing::MARGIN),
                policy.available.min(MAX_MOTION_TICKS),
            ) {
                best = cost;
                best_motion = motion;
                best_forecast = forecast;
                chosen = (trial, to, Some(Motion::ColumnTransfer(Box::new(route))));
            }
        }
    }
    let (nodes, to, exchange) = chosen;
    Some(Change {
        tick,
        nodes,
        transition: Some((from, to)),
        exchange,
    })
}

fn movement_cost(from: &Layout, to: &Layout, stability: f32, hops: f32) -> f32 {
    let relative = exchange::edge_travel(from, to) / super::framing::VIEWPORT.x;
    stability * (0.02 * hops + 0.06 * relative)
}

struct RestackTrial {
    splits: [(usize, Node); 3],
    route: exchange::Restack,
}

fn restack_candidates(nodes: &[Node], splits: &[usize]) -> Vec<RestackTrial> {
    let mut candidates = Vec::new();
    for &index in splits {
        let Node::Split {
            axis,
            first,
            second,
        } = nodes[index]
        else {
            continue;
        };
        let across = 1 - axis;
        let leaves = |row: usize| {
            let Node::Split {
                axis,
                first,
                second,
            } = nodes[row]
            else {
                return None;
            };
            let (Node::Repository(a), Node::Repository(b)) =
                (&nodes[first], &nodes[second])
            else {
                return None;
            };
            (axis == across).then_some([(first, *a), (second, *b)])
        };
        let (Some(first_row), Some(second_row)) = (leaves(first), leaves(second))
        else {
            continue;
        };
        let rows = [first_row, second_row];
        for (moving_row, near) in rows.iter().enumerate() {
            let far = rows[1 - moving_row];
            for (staying_side, &(staying, stays)) in far.iter().enumerate() {
                let [
                    (moving, moves),
                    (opposing_far, far_repository),
                    (opposing_near, near_repository),
                ] = [
                    near[staying_side],
                    far[1 - staying_side],
                    near[1 - staying_side],
                ];
                let repositories = [stays, moves, far_repository, near_repository];
                let split = |axis, first, second| Node::Split {
                    axis,
                    first,
                    second,
                };
                let root = if staying_side == 0 {
                    split(across, staying, first)
                } else {
                    split(across, first, staying)
                };
                let [outer, inner] = if moving_row == 0 {
                    [
                        split(axis, moving, second),
                        split(axis, opposing_near, opposing_far),
                    ]
                } else {
                    [
                        split(axis, opposing_far, second),
                        split(axis, opposing_near, moving),
                    ]
                };
                candidates.push(RestackTrial {
                    splits: [(index, root), (first, outer), (second, inner)],
                    route: exchange::Restack {
                        repositories,
                        across_axis: across,
                        starts_on_low_side: staying_side == 0,
                        enters_at_low_end: moving_row == 0,
                    },
                });
            }
        }
    }
    candidates
}

fn chain_candidates(
    nodes: &[Node],
    from: &Layout,
    weights: &[f32],
    target: &[f32],
    errors: &[f32],
) -> Vec<([usize; 3], [usize; 3])> {
    let mut leaves: Vec<_> = nodes
        .iter()
        .enumerate()
        .filter_map(|(leaf, node)| {
            if let Node::Repository(repo) = *node {
                from.gutters[repo].map(|tile| (leaf, repo, tile))
            } else {
                None
            }
        })
        .collect();
    let drift = |repo: usize| (target[repo] / weights[repo] - 1.0).abs();
    leaves.sort_by(|a, b| {
        drift(b.1)
            .total_cmp(&drift(a.1))
            .then(errors[b.1].total_cmp(&errors[a.1]))
            .then(a.1.cmp(&b.1))
    });
    let neighbour = |tile, axis, forward| {
        leaves.iter().copied().find(|&(_, _, other)| {
            let aligned = if forward {
                exchange::side_by_side(tile, other)
            } else {
                exchange::side_by_side(other, tile)
            };
            aligned == Some(axis)
        })
    };
    let directions = [(0, true), (0, false), (1, true), (1, false)];
    leaves
        .iter()
        .take(MAX_LOCAL_SPLITS)
        .flat_map(|&(first, a, tile)| {
            directions.into_iter().filter_map(move |(axis, forward)| {
                let (second, b, middle) = neighbour(tile, axis, forward)?;
                let (third, c, _) = neighbour(middle, axis, forward)?;
                Some(([first, second, third], [a, b, c]))
            })
        })
        .take(MAX_CHAIN_CANDIDATES)
        .collect()
}

fn exchange_candidates(
    nodes: &[Node],
    splits: &[usize],
    gutters: &[Option<Tile>],
    target: &[f32],
) -> Vec<(usize, usize, [usize; 2])> {
    let leaves: Vec<_> = nodes
        .iter()
        .enumerate()
        .filter_map(|(node, leaf)| {
            if let Node::Repository(repo) = *leaf {
                gutters[repo].map(|cell| (node, repo, cell))
            } else {
                None
            }
        })
        .collect();
    let mut candidates = Vec::new();
    let affected = splits.iter().flat_map(|&index| match nodes[index] {
        Node::Split { first, second, .. } => [first, second],
        Node::Repository(_) => unreachable!("ranked split"),
    });
    for a in affected {
        let Node::Repository(repo) = nodes[a] else {
            continue;
        };
        let Some(cell) = gutters[repo] else { continue };
        for &(b, other, neighbour) in &leaves {
            if target[repo].max(target[other]) > 1.25 * target[repo].min(target[other])
                && (exchange::side_by_side(cell, neighbour).is_some()
                    || exchange::side_by_side(neighbour, cell).is_some())
                && !candidates
                    .iter()
                    .any(|&(first, second, _)| first == b && second == a)
            {
                candidates.push((a, b, [repo, other]));
                if candidates.len() == MAX_LOCAL_SPLITS {
                    return candidates;
                }
            }
        }
    }
    candidates
}

fn connected(from: &Layout, to: &Layout, allow_corners: bool) -> bool {
    let cells: Vec<_> = from
        .gutters
        .iter()
        .zip(&to.gutters)
        .filter_map(|(&a, &b)| a.zip(b))
        .collect();
    if cells.is_empty() {
        return false;
    }
    let mut reached = vec![false; cells.len()];
    reached[0] = true;
    let mut pending = vec![0];
    let shares_edge = |a: Tile, b: Tile, axis: usize| {
        let cross = 1 - axis;
        let overlap = a.high[cross].min(b.high[cross]) - a.low[cross].max(b.low[cross]);
        (a.high[axis] - b.low[axis]).abs() <= f32::EPSILON
            && (overlap > 0.0 || (allow_corners && overlap >= 0.0))
    };
    while let Some(i) = pending.pop() {
        let (a, end_a) = cells[i];
        for (j, &(b, end_b)) in cells.iter().enumerate() {
            if !reached[j]
                && (0..2).any(|axis| {
                    (shares_edge(a, b, axis) && shares_edge(end_a, end_b, axis))
                        || (shares_edge(b, a, axis) && shares_edge(end_b, end_a, axis))
                })
            {
                reached[j] = true;
                pending.push(j);
            }
        }
    }
    reached.into_iter().all(|seen| seen)
}

#[derive(Clone, Copy)]
struct Density {
    worst: f32,
    variation: f32,
    narrow: f32,
}

impl Density {
    fn new(tiles: &[Option<Tile>], weights: &[f32]) -> Self {
        let (area, weight) = totals(tiles, weights);
        let (mut worst, mut variation, mut narrow) = (0.0_f32, 0.0, 0.0);
        for (&tile, &items) in tiles.iter().zip(weights) {
            if let Some(tile) = tile {
                let (share, due) = (tile.area() / area, items / weight);
                if items > 0.0 {
                    worst = worst.max(density_error(share, due));
                }
                variation += (share - due).abs() * 0.5;
                narrow += share * super::narrow_tile_cost(tile);
            }
        }
        Self {
            worst,
            variation,
            narrow,
        }
    }

    fn improves(self, best: Self, baseline: Self) -> bool {
        self.improves_with_motion(best, baseline, [0.0; 2])
    }

    fn score(self) -> f32 {
        self.worst + 4.0 * self.variation + 0.025 * self.narrow
    }

    fn improves_with_motion(
        self,
        best: Self,
        baseline: Self,
        [motion, best_motion]: [f32; 2],
    ) -> bool {
        self.worst <= best.worst
            && self.variation <= baseline.variation + 0.005
            && self.score() + motion + 0.02 < best.score() + best_motion
    }
}

pub(super) fn can_reweight(
    nodes: &[Node],
    weights: &[f32],
    live: &[f32],
    future: &[f32],
    presence: &[f32],
    space: Space,
) -> bool {
    if !live
        .iter()
        .zip(presence)
        .any(|(&items, &present)| items > 0.0 && present > 0.0)
    {
        return false;
    }
    let Some(current) = tiles(nodes, weights, presence, space) else {
        return false;
    };
    let baseline = Density::new(&current, live);
    if baseline.worst <= super::DENSITY_TOLERANCE {
        return false;
    }
    let future: Vec<_> = future.iter().map(|&items| items.max(1.0)).collect();
    tiles(nodes, &future, presence, space).is_some_and(|after| {
        let density = Density::new(&after, live);
        density.worst <= super::DENSITY_TOLERANCE
            || density.improves(baseline, baseline)
    })
}

fn totals(tiles: &[Option<Tile>], weights: &[f32]) -> (f32, f32) {
    tiles
        .iter()
        .zip(weights)
        .fold((0.0, 0.0), |sum, (&tile, &weight)| {
            tile.map_or(sum, |tile| (sum.0 + tile.area(), sum.1 + weight))
        })
}

fn density_error(share: f32, due: f32) -> f32 {
    (due / share.max(f32::MIN_POSITIVE) - 1.0).abs()
}

fn affected_splits(
    nodes: &[Node],
    tiles: &[Option<Tile>],
    weights: &[f32],
) -> Vec<usize> {
    fn rank(
        nodes: &[Node],
        errors: &[f32],
        index: usize,
        depth: usize,
        splits: &mut Vec<(usize, f32, usize)>,
    ) -> f32 {
        match nodes[index] {
            Node::Repository(repository) => errors[repository],
            Node::Split { first, second, .. } => {
                let a = rank(nodes, errors, first, depth + 1, splits);
                let b = rank(nodes, errors, second, depth + 1, splits);
                let worst = a.max(b);
                splits.push((index, worst, depth));
                worst
            }
        }
    }

    let errors = density_errors(tiles, weights);
    let mut splits = Vec::new();
    rank(nodes, &errors, 0, 0, &mut splits);
    splits.sort_unstable_by(|a, b| {
        b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0))
    });
    splits
        .into_iter()
        .take(MAX_LOCAL_SPLITS)
        .map(|(index, ..)| index)
        .collect()
}

fn density_errors(tiles: &[Option<Tile>], weights: &[f32]) -> Vec<f32> {
    let (area, weight) = totals(tiles, weights);
    tiles
        .iter()
        .zip(weights)
        .map(|(&tile, &items)| {
            tile.map_or(0.0, |tile| {
                density_error(tile.area() / area, items / weight)
            })
        })
        .collect()
}

fn compatible(from: &[Option<Tile>], to: &[Option<Tile>]) -> bool {
    let pairs: Vec<_> = from
        .iter()
        .zip(to)
        .filter_map(|(&a, &b)| a.zip(b))
        .collect();
    for (i, &(a, end_a)) in pairs.iter().enumerate() {
        for &(b, end_b) in &pairs[..i] {
            // A shared separating direction stays separated throughout the ease.
            let before = a.high.cmple(b.low);
            let after = end_a.high.cmple(end_b.low);
            let reverse_before = b.high.cmple(a.low);
            let reverse_after = end_b.high.cmple(end_a.low);
            if !((before & after) | (reverse_before & reverse_after)).any() {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::{Input, Inputs, Packing, Sample, outline};
    use super::*;
    use crate::layout::RepositoryRegion;
    use bevy::math::Vec2;

    fn repositories_after_deletion() -> Packing {
        let mut inputs = Inputs::new(600);
        for tick in 0..600 {
            let changed: u16 = if tick < 180 { 130 } else { 68 };
            let specs = [
                (124, 10.0, 3.0),
                (57, 1.0, 8.0),
                (13, 6.0, 1.0),
                (122, 9.0, 12.0),
                (47, 12.0, 12.0),
                (changed, 3.0, 6.0),
                (73, 5.0, 8.0),
                (136, 3.0, 9.0),
                (126, 7.0, 12.0),
                (122, 3.0, 2.0),
            ];
            inputs.push(
                specs
                    .into_iter()
                    .enumerate()
                    .map(|(repo, (items, x, y))| {
                        let items = f32::from(items);
                        let extent = Vec2::new(x, y) * items.sqrt();
                        RepositoryRegion {
                            label: format!("Repo {repo}"),
                            low: -extent,
                            high: extent,
                            outline: outline::rectangle(-extent, extent),
                            items,
                            live: true,
                            tile: None,
                            gutter: None,
                            height: 0.105,
                            rotated: false,
                            stretch: Vec2::ONE,
                        }
                    })
                    .collect(),
            );
        }
        Packing::from_inputs(inputs, std::sync::Arc::default(), 8.0, 1.0, 1).unwrap()
    }

    #[test]
    fn playback_exchanges_neighbours_after_a_sustained_deletion() {
        let packing = repositories_after_deletion();
        let change = packing
            .reflows
            .iter()
            .find(|change| {
                change.tick >= 180
                    && matches!(change.exchange, Some(Motion::Exchange(_)))
            })
            .expect("the sampled repository history should schedule an exchange");
        let tiles_at = |tick| {
            let (regions, _, _) = packing.sample(tick).unwrap();
            regions
                .into_iter()
                .map(|region| region.tile)
                .collect::<Vec<_>>()
        };
        let begin = u64::try_from(change.tick).unwrap();
        let end = begin + u64::try_from(change.duration()).unwrap();
        let before = tiles_at(begin);
        let after = tiles_at(end);
        let counts = [
            124.0, 57.0, 13.0, 122.0, 47.0, 68.0, 73.0, 136.0, 126.0, 122.0,
        ];
        assert!(
            Density::new(&after, &counts).worst < Density::new(&before, &counts).worst
        );
        assert!((0..before.len()).any(|a| (0..before.len()).any(|b| {
            let (before_a, before_b) = (before[a].unwrap(), before[b].unwrap());
            let (after_a, after_b) = (after[a].unwrap(), after[b].unwrap());
            (before_a.high.cmple(before_b.low) & after_b.high.cmple(after_a.low)).any()
        })));
        for tick in begin..=end {
            let tiles = tiles_at(tick);
            for (a, first) in tiles.iter().enumerate() {
                let first = first.unwrap();
                for second in &tiles[a + 1..] {
                    let second = second.unwrap();
                    let overlap =
                        first.high.min(second.high) - first.low.max(second.low);
                    assert!(overlap.min_element() <= 1e-5, "overlap at tick {tick}");
                }
            }
        }
    }

    #[test]
    fn a_shrinking_tile_joins_the_other_column_within_the_motion_budget() {
        use Node::Repository;

        let split = |axis, first, second| Node::Split {
            axis,
            first,
            second,
        };
        let stacked_pairs = [
            split(0, 1, 4),
            split(1, 2, 3),
            Repository(0),
            Repository(1),
            split(1, 5, 6),
            Repository(2),
            Repository(3),
        ];
        let three_on_right = [
            split(0, 1, 2),
            Repository(0),
            split(1, 3, 4),
            Repository(1),
            split(1, 5, 6),
            Repository(2),
            Repository(3),
        ];
        let initial = [300.0, 300.0, 30.0, 30.0];
        let target = [300.0, 30.0, 30.0, 30.0];
        let presence = [1.0; 4];
        let space = Space::new(8.0, 1.0);
        let density = |nodes: &[Node]| {
            let bounds = tiles(nodes, &target, &presence, space).unwrap();
            Density::new(&bounds, &target).worst
        };
        assert!(density(&stacked_pairs) > 0.8);
        assert!(density(&three_on_right) < 0.03);

        let first = plan(
            180,
            &stacked_pairs,
            &initial,
            &target,
            &presence,
            space,
            ordinary(&target),
        )
        .unwrap();
        assert!(density(&first.nodes) < density(&stacked_pairs));

        let retry = |available, forecast| {
            plan(
                first.tick + first.duration(),
                &first.nodes,
                &target,
                &target,
                &presence,
                space,
                Policy {
                    stability: 1.0,
                    available,
                    forecast,
                },
            )
            .unwrap()
        };
        let transfer = retry(MAX_MOTION_TICKS, &target);
        assert!(matches!(transfer.exchange, Some(Motion::ColumnTransfer(_))));
        assert!(density(&transfer.nodes) < 0.03);
        assert!(transfer.duration() <= MAX_MOTION_TICKS);

        let too_short = retry(transfer.duration() - 1, &target);
        assert!(too_short.duration() < transfer.duration());
        assert!(density(&too_short.nodes) > super::super::DENSITY_TOLERANCE);
        let reversed = retry(MAX_MOTION_TICKS, &initial);
        assert!(density(&reversed.nodes) > super::super::DENSITY_TOLERANCE);

        assert_safe_transfer(&transfer, space);
    }

    fn assert_safe_transfer(transfer: &Change, space: Space) {
        let (from, to) = transfer.transition.as_ref().unwrap();
        let field = space.field(super::super::framing::MARGIN);
        let gutter_field = space.field(super::super::GUTTER_MARGIN);
        let mut previous = from.clone();
        for elapsed in 1..=transfer.duration() {
            let next = transfer
                .sample(transfer.tick + elapsed)
                .unwrap_or_else(|| to.clone());
            assert!(compatible(&previous.gutters, &next.gutters));
            assert!(connected(&previous, &next, true));
            assert!(exchange::edge_travel(&previous, &next) <= 8.001);
            for (&tile, &gutter) in next.tiles.iter().zip(&next.gutters) {
                let (tile, gutter) = (tile.unwrap(), gutter.unwrap());
                assert!(tile.low.cmpge(gutter.low).all());
                assert!(tile.high.cmple(gutter.high).all());
                assert!(tile.low.cmpge(field.low - Vec2::splat(1e-5)).all());
                assert!(tile.high.cmple(field.high + Vec2::splat(1e-5)).all());
                assert!(gutter.low.cmpge(gutter_field.low - Vec2::splat(1e-5)).all());
                assert!(
                    gutter
                        .high
                        .cmple(gutter_field.high + Vec2::splat(1e-5))
                        .all()
                );
            }
            previous = next;
        }
    }

    fn ordinary(target: &[f32]) -> Policy<'_> {
        Policy {
            stability: 1.0,
            available: held::reflow_ticks(),
            forecast: target,
        }
    }

    fn grid(nodes: &mut Vec<Node>, start: usize, columns: usize, rows: usize) -> usize {
        let root = nodes.len();
        nodes.push(Node::Repository(start));
        if columns > 1 {
            let left = columns / 2;
            let first = grid(nodes, start, left, rows);
            let second = grid(nodes, start + left * rows, columns - left, rows);
            nodes[root] = Node::Split {
                axis: 0,
                first,
                second,
            };
        } else if rows > 1 {
            let top = rows / 2;
            let first = grid(nodes, start, 1, top);
            let second = grid(nodes, start + top, 1, rows - top);
            nodes[root] = Node::Split {
                axis: 1,
                first,
                second,
            };
        }
        root
    }

    #[test]
    fn a_shrinking_tile_can_pass_two_neighbours_without_settling_between_them() {
        let mut nodes = Vec::new();
        grid(&mut nodes, 0, 6, 4);
        let weights: Vec<_> = [3_u16, 3, 2, 4, 4, 3]
            .into_iter()
            .flat_map(|scale| [24, 20, 16, 18].map(|weight| f32::from(weight * scale)))
            .collect();
        let mut target = weights.clone();
        target[2] = 9.0;
        let presence = [1.0; 24];
        let space = Space::new(8.0, 1.0);
        let change = |available, forecast| {
            plan(
                1,
                &nodes,
                &weights,
                &target,
                &presence,
                space,
                Policy {
                    stability: 1.0,
                    available,
                    forecast,
                },
            )
            .unwrap()
        };
        let single = change(held::reflow_ticks(), &target);
        assert!(
            Density::new(&single.transition.unwrap().1.tiles, &target).worst > 0.38
        );
        let chosen = change(MAX_MOTION_TICKS, &target);
        let Some(Motion::Chain(chain)) = &chosen.exchange else {
            panic!("sustained deletion should trigger a two-hop move");
        };
        let (from, to) = chosen.transition.as_ref().unwrap();
        assert!(Density::new(&to.tiles, &target).worst < 0.04);
        assert!(chosen.duration() > held::reflow_ticks());
        assert!(chosen.duration() <= MAX_MOTION_TICKS);
        let packing = Packing {
            reflows: vec![chosen.clone()].into(),
            ..Packing::default()
        };
        let finish = u64::try_from(1 + chosen.duration()).unwrap();
        assert_eq!(packing.motion_intervals().next(), Some((1, finish)));
        assert!(packing.reflowing(finish - 1));
        assert!(!packing.reflowing(finish));
        assert!(chosen.sample(1 + held::reflow_ticks()).is_some());
        chain.assert_compressed_handoff(from, to, [2, 6, 10]);
        let mut previous = from.clone();
        for tick in 1..=1 + chosen.duration() {
            let sample = chosen.sample(tick).unwrap_or_else(|| to.clone());
            assert!(compatible(&previous.gutters, &sample.gutters));
            assert!(connected(&sample, &sample, true));
            assert!(exchange::edge_travel(&previous, &sample) <= 8.001);
            previous = sample;
        }
        assert!(!matches!(
            change(chosen.duration() - 1, &target).exchange,
            Some(Motion::Chain(_))
        ));
        assert!(!matches!(
            change(MAX_MOTION_TICKS, &weights).exchange,
            Some(Motion::Chain(_))
        ));

        target[2] = 1.0;
        let marginal = |stability| {
            plan(
                1,
                &nodes,
                &weights,
                &target,
                &presence,
                space,
                Policy {
                    stability,
                    available: MAX_MOTION_TICKS,
                    forecast: &target,
                },
            )
            .unwrap()
        };
        assert!(matches!(marginal(0.0).exchange, Some(Motion::Chain(_))));
        assert_eq!(marginal(2.0).nodes, nodes);
    }

    #[test]
    fn equivalent_density_prefers_the_wider_tile_in_view_pixels() {
        let origin = Vec2::ZERO;
        let tile = |width, height| {
            Some(Tile {
                low: origin,
                high: 2.0 * Vec2::new(width, height) / super::super::framing::VIEWPORT,
            })
        };
        let tall = Density::new(&[tile(60.0, 120.0)], &[1.0]);
        let wide = Density::new(&[tile(120.0, 60.0)], &[1.0]);
        assert_eq!(wide.worst, tall.worst);
        assert_eq!(wide.variation, tall.variation);
        assert!(wide.score() < tall.score());
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "checks smooth sampling and packing fallback"
    )]
    fn a_grid_can_change_its_outer_axis_to_follow_weights() {
        let split = |axis, first, second| Node::Split {
            axis,
            first,
            second,
        };
        let nodes = [
            split(0, 1, 2),
            split(1, 3, 4),
            split(1, 5, 6),
            Node::Repository(0),
            Node::Repository(1),
            Node::Repository(2),
            Node::Repository(3),
        ];
        let weights = [50.0; 4];
        let target = [80.0, 20.0, 20.0, 80.0];
        let presence = [1.0; 4];
        let space = Space::new(4.0, 1.0);
        let change = plan(
            12,
            &nodes,
            &weights,
            &target,
            &presence,
            space,
            ordinary(&target),
        )
        .unwrap();
        assert!(matches!(change.nodes[0], Node::Split { axis: 1, .. }));
        let unchanged = tiles(&nodes, &target, &presence, space).unwrap();
        let (from, to) = change.transition.as_ref().unwrap();
        assert!(
            Density::new(&to.tiles, &target).worst
                < Density::new(&unchanged, &target).worst
        );
        let end = 12 + held::reflow_ticks();
        let mid = 12 + held::reflow_ticks() / 2;
        let content = Input {
            low: Vec2::ZERO,
            high: Vec2::ONE,
            outline: outline::rectangle(Vec2::ZERO, Vec2::ONE).into(),
            height: 0.0,
            live: true,
        };
        let samples: Vec<_> = (0..=end)
            .map(|tick| Sample {
                weights: if tick < end { weights } else { target }.to_vec(),
                presence: presence.to_vec(),
            })
            .collect();
        let packing = Packing {
            gutter_scale: 4.0,
            nodes: nodes.to_vec(),
            reflows: vec![change.clone()].into(),
            labels: ["a", "b", "c", "d"].map(str::to_owned).to_vec().into(),
            inputs: vec![vec![content; 4]; end + 1].into(),
            samples: samples.into(),
            rotated: vec![false; 4],
            content_scales: vec![1.0; 4 * (end + 1)].into(),
            ..Packing::default()
        };
        let sample = |tick| {
            let (regions, _, _) = packing.sample(u64::try_from(tick).unwrap()).unwrap();
            Layout {
                tiles: regions.iter().map(|r| r.tile).collect(),
                gutters: regions.iter().map(|r| r.gutter).collect(),
            }
        };
        let birth = [1.0, 0.75, 1.0, 1.0];
        let masked_tiles = |packing: &Packing| {
            packing
                .sample_with_presence(12, Some(&birth))
                .unwrap()
                .0
                .into_iter()
                .map(|region| region.tile)
                .collect::<Vec<_>>()
        };
        let mut source_tree = packing.clone();
        source_tree.reflows = std::sync::Arc::default();
        assert_eq!(masked_tiles(&packing), masked_tiles(&source_tree));
        let middle = sample(mid);
        let mut previous = sample(11).tiles;
        assert_eq!(previous, from.tiles);
        for tick in 12..=end {
            let current = sample(tick);
            for (a, b) in previous
                .iter()
                .flatten()
                .zip(current.tiles.iter().flatten())
            {
                assert!(a.low.distance(b.low) < 0.015);
                assert!(a.high.distance(b.high) < 0.015);
            }
            if [12, mid, end].contains(&tick) {
                crate::model::assert_gutter_routes(
                    current.gutters.iter().flatten().copied(),
                );
            }
            previous = current.tiles;
        }
        assert_eq!(previous, to.tiles);
        let replay = sample(mid);
        assert_eq!(middle.tiles, replay.tiles);
        assert_eq!(middle.gutters, replay.gutters);
    }

    #[test]
    fn measured_area_can_require_reweighting_before_held_counts_drift() {
        let nodes = [
            Node::Split {
                axis: 0,
                first: 1,
                second: 2,
            },
            Node::Repository(0),
            Node::Split {
                axis: 0,
                first: 3,
                second: 4,
            },
            Node::Repository(1),
            Node::Repository(2),
        ];
        let held = [120.0, 730.0, 150.0];
        let live = [120.0, 694.0, 186.0];
        let space = Space::new(4.0, 1.0);
        assert!(can_reweight(&nodes, &held, &live, &live, &[1.0; 3], space));
        assert!(!can_reweight(&nodes, &live, &live, &live, &[1.0; 3], space));
        let held = [703.0, 165.0, 132.0];
        let live = [702.0, 184.0, 114.0];
        let space = Space::new(8.0, 1.0);
        let presence = [1.0; 3];
        let before =
            Density::new(&tiles(&nodes, &held, &presence, space).unwrap(), &live);
        let after =
            Density::new(&tiles(&nodes, &live, &presence, space).unwrap(), &live);
        assert!(before.worst > super::super::DENSITY_TOLERANCE);
        assert!(after.worst <= super::super::DENSITY_TOLERANCE);
        assert!(!after.improves(before, before));
        assert!(can_reweight(&nodes, &held, &live, &live, &presence, space));
    }

    #[test]
    fn a_settled_density_error_can_turn_an_adjacent_pair() {
        let split = |axis, first, second| Node::Split {
            axis,
            first,
            second,
        };
        let nodes = [
            split(0, 1, 2),
            Node::Repository(3),
            split(1, 3, 4),
            split(1, 5, 6),
            Node::Repository(2),
            Node::Repository(0),
            Node::Repository(1),
        ];
        let counts = [25.0, 75.0, 56.0, 180.0];
        let space = Space::new(4.0, 1.0);
        let change = plan(
            12,
            &nodes,
            &counts,
            &counts,
            &[1.0; 4],
            space,
            ordinary(&counts),
        )
        .unwrap();
        let (before, after) = change.transition.as_ref().unwrap();
        let error = |layout: &Layout| Density::new(&layout.tiles, &counts).worst;
        assert!(error(before) > super::super::DENSITY_TOLERANCE);
        assert!(error(after) < super::super::DENSITY_TOLERANCE);
        assert!(change.exchange.is_some());
    }

    #[test]
    fn a_surrounded_tile_can_shrink_to_its_neighbours_item_count_in_place() {
        use Node::{Repository, Split};

        let split = |axis, first, second| Split {
            axis,
            first,
            second,
        };
        // Four tiles on each side and two on each of the top and bottom.
        let nodes = [
            split(0, 1, 8),
            split(1, 2, 5),
            split(1, 3, 4),
            Repository(0),
            Repository(1),
            split(1, 6, 7),
            Repository(2),
            Repository(3),
            split(0, 9, 18),
            split(1, 10, 13),
            split(0, 11, 12),
            Repository(4),
            Repository(5),
            split(1, 14, 15),
            Repository(6),
            split(0, 16, 17),
            Repository(7),
            Repository(8),
            split(1, 19, 22),
            split(1, 20, 21),
            Repository(9),
            Repository(10),
            split(1, 23, 24),
            Repository(11),
            Repository(12),
        ];
        let target = [40.0; 13];
        let mut weights = target;
        weights[6] = 160.0;
        let space = Space::new(4.0, 1.0);
        let change = plan(
            1,
            &nodes,
            &weights,
            &target,
            &[1.0; 13],
            space,
            ordinary(&target),
        )
        .unwrap();
        assert_eq!(change.nodes, nodes);
        let (from, to) = change.transition.as_ref().unwrap();
        assert!(
            Density::new(&to.tiles, &target).worst < super::super::DENSITY_TOLERANCE
        );
        let centre_area = |layout: &Layout| layout.tiles[6].unwrap().area();
        let field_area = from
            .gutters
            .iter()
            .flatten()
            .map(|tile| tile.area())
            .sum::<f32>();
        let mut previous_area = centre_area(from);
        for elapsed in 0..=change.duration() {
            let sample = change.sample(1 + elapsed).unwrap_or_else(|| to.clone());
            let occupied = sample
                .gutters
                .iter()
                .flatten()
                .map(|tile| tile.area())
                .sum::<f32>();
            assert!((occupied - field_area).abs() < 1e-6);
            assert!(compatible(&sample.gutters, &sample.gutters));
            assert!(centre_area(&sample) <= previous_area + 1e-6);
            previous_area = centre_area(&sample);
        }
        assert!(centre_area(to) < 0.6 * centre_area(from));
    }

    #[test]
    fn a_large_tile_shrinks_against_three_neighbours_without_opening_a_gap() {
        let split = |axis, first, second| Node::Split {
            axis,
            first,
            second,
        };
        let nodes = [
            split(0, 1, 2),
            Node::Repository(0),
            split(1, 3, 4),
            Node::Repository(1),
            split(1, 5, 6),
            Node::Repository(2),
            Node::Repository(3),
        ];
        let weights = [255.0, 40.0, 40.0, 40.0];
        let target = [100.0, 40.0, 40.0, 40.0];
        let space = Space::new(4.0, 1.0);
        let change = plan(
            1,
            &nodes,
            &weights,
            &target,
            &[1.0; 4],
            space,
            ordinary(&target),
        )
        .unwrap();
        assert_eq!(change.nodes, nodes);
        let (from, to) = change.transition.as_ref().unwrap();
        let cut = |layout: &Layout| layout.gutters[0].unwrap().high.x;
        assert!(cut(to) < cut(from));
        let mut before = from.clone();
        let mut previous_step = 0.0;
        for elapsed in 0..=change.duration() {
            let sample = change.sample(1 + elapsed).unwrap_or_else(|| to.clone());
            let large = sample.tiles[0].unwrap();
            let step = cut(&before) - cut(&sample);
            assert!(step >= 0.0);
            if elapsed == change.duration() {
                assert!(step < previous_step);
            }
            for small in 1..=3 {
                assert_eq!(sample.gutters[small].unwrap().low.x, cut(&sample));
                let gap = sample.tiles[small].unwrap().low.x - large.high.x;
                let initial_gap =
                    from.tiles[small].unwrap().low.x - from.tiles[0].unwrap().high.x;
                assert!((gap - initial_gap).abs() < 1e-6);
            }
            previous_step = step;
            before = sample;
        }
    }

    #[test]
    fn a_reflow_preserves_shared_gutter_edges_as_the_cut_moves() {
        let layout = |cut| {
            let cells = vec![
                Some(Tile {
                    low: Vec2::ZERO,
                    high: Vec2::new(cut, 1.0),
                }),
                Some(Tile {
                    low: Vec2::new(cut, 0.0),
                    high: Vec2::ONE,
                }),
            ];
            Layout {
                tiles: cells.clone(),
                gutters: cells,
            }
        };
        let from = layout(0.25);
        let to = layout(0.75);
        assert!(connected(&from, &to, false));
        assert!(compatible(&from.gutters, &to.gutters));
        let change = Change {
            tick: 12,
            nodes: Vec::new(),
            transition: Some((from, to)),
            exchange: None,
        };
        let midpoint = change.sample(12 + held::reflow_ticks() / 2).unwrap();
        assert_eq!(midpoint.gutters[0].unwrap().high.x, 0.5);
        assert_eq!(midpoint.gutters[1].unwrap().low.x, 0.5);
        assert!(change.sample(12 + held::reflow_ticks()).is_none());
    }
}
