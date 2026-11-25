use super::{Node, Space, Tile, gutter_cells, held, insert, tiles};

mod exchange;

const MAX_LOCAL_SPLITS: usize = 8;
const MOVING_GUTTER_PX: f32 = 4.0;
const MAX_REBUILD_TILES: usize = 8;

#[derive(Clone, Debug)]
pub(super) struct Change {
    pub tick: usize,
    pub nodes: Vec<Node>,
    transition: Option<(Layout, Layout)>,
    exchange: Option<Box<exchange::Exchange>>,
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
        (std::mem::size_of::<Self>() + std::mem::size_of::<exchange::Exchange>())
            as u128
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
        self.transition.is_some() && tick - self.tick < held::reflow_ticks()
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
        if elapsed >= held::reflow_ticks() {
            return None;
        }
        Some(self.exchange.as_ref().map_or_else(
            || from.blend(to, held::reflow_ease(elapsed)),
            |exchange| exchange.sample(from, to, elapsed),
        ))
    }
}

impl Layout {
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
) -> Option<Change> {
    let from = tiles(nodes, weights, presence, space)?;
    let from = Layout::new(nodes, from, space);
    let unchanged = Layout::new(nodes, tiles(nodes, target, presence, space)?, space);
    let baseline = Density::new(&unchanged.tiles, target);
    let mut best = baseline;
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
            if cost.improves(best, baseline) {
                let to = Layout::new(&trial, to, space);
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
                    chosen = (trial, to, exchange.map(Box::new));
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
    let (nodes, to, exchange) = chosen;
    Some(Change {
        tick,
        nodes,
        transition: Some((from, to)),
        exchange,
    })
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
}

impl Density {
    fn new(tiles: &[Option<Tile>], weights: &[f32]) -> Self {
        let (area, weight) = totals(tiles, weights);
        let (mut worst, mut variation) = (0.0_f32, 0.0);
        for (&tile, &items) in tiles.iter().zip(weights) {
            if let Some(tile) = tile {
                let (share, due) = (tile.area() / area, items / weight);
                if items > 0.0 {
                    worst = worst.max(density_error(share, due));
                }
                variation += (share - due).abs() * 0.5;
            }
        }
        Self { worst, variation }
    }

    fn improves(self, best: Self, baseline: Self) -> bool {
        self.worst <= best.worst
            && self.variation <= baseline.variation + 0.005
            && self.worst + 4.0 * self.variation + 0.02
                < best.worst + 4.0 * best.variation
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

    let (area, weight) = totals(tiles, weights);
    let errors: Vec<_> = tiles
        .iter()
        .zip(weights)
        .map(|(&tile, &items)| {
            tile.map_or(0.0, |tile| {
                density_error(tile.area() / area, items / weight)
            })
        })
        .collect();
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
    use super::super::{Input, Packing, Sample, outline};
    use super::*;
    use bevy::math::Vec2;

    #[test]
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
        let change = plan(12, &nodes, &weights, &target, &presence, space).unwrap();
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
        let change = plan(12, &nodes, &counts, &counts, &[1.0; 4], space).unwrap();
        let (before, after) = change.transition.as_ref().unwrap();
        let error = |layout: &Layout| Density::new(&layout.tiles, &counts).worst;
        assert!(error(before) > super::super::DENSITY_TOLERANCE);
        assert!(error(after) < super::super::DENSITY_TOLERANCE);
        assert!(change.exchange.is_some());
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
