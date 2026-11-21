use super::{Node, Space, Tile, gutter_cells, held, insert, tiles};

const MAX_LOCAL_SPLITS: usize = 8;
const MAX_REBUILD_TILES: usize = 8;

#[derive(Clone, Debug)]
pub(super) struct Change {
    pub tick: usize,
    pub nodes: Vec<Node>,
    from: Layout,
    to: Layout,
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
    pub fn matches_presence(&self, presence: &[f32]) -> bool {
        self.from.tiles.iter().zip(presence).all(|(tile, &p)| {
            if tile.is_some() { p >= 1.0 } else { p <= 0.0 }
        })
    }

    pub fn sample(&self, tick: usize) -> Option<Layout> {
        let elapsed = tick - self.tick;
        if elapsed >= held::reflow_ticks() {
            return None;
        }
        let fraction = held::reflow_ease(elapsed);
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
        Some(Layout {
            tiles: blend(&self.from.tiles, &self.to.tiles),
            gutters: blend(&self.from.gutters, &self.to.gutters),
        })
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
    if presence.iter().filter(|&&p| p > 0.0).count() < 3 {
        return None;
    }
    let from = tiles(nodes, weights, presence, space)?;
    let from = Layout::new(nodes, from, space);
    let unchanged = tiles(nodes, target, presence, space)?;
    let mut best = mismatch(&unchanged, target);
    if best <= 0.005 {
        return None;
    }
    let mut chosen = None;
    let mut consider = |trial: Vec<Node>| {
        if let Some(to) = tiles(&trial, target, presence, space) {
            let cost = mismatch(&to, target);
            if cost + 0.005 < best {
                let to = Layout::new(&trial, to, space);
                if compatible(&from.gutters, &to.gutters) && connected(&from, &to) {
                    best = cost;
                    chosen = Some((trial, to));
                }
            }
        }
    };
    for (index, node) in nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| matches!(node, Node::Split { .. }))
        .take(MAX_LOCAL_SPLITS)
    {
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
        consider(trial);
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
            consider(trial);
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
            consider(trial);
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
            insert(&mut trial, i, &inserted, &aspects, space, 1)
        })
    {
        consider(trial);
    }
    let (nodes, to) = chosen?;
    Some(Change {
        tick,
        nodes,
        from,
        to,
    })
}

fn connected(from: &Layout, to: &Layout) -> bool {
    let cells: Vec<_> = from
        .gutters
        .iter()
        .zip(&to.gutters)
        .filter_map(|(&a, &b)| a.zip(b))
        .collect();
    let mut reached = vec![false; cells.len()];
    reached[0] = true;
    let mut pending = vec![0];
    let shares_edge = |a: Tile, b: Tile, axis: usize| {
        let cross = 1 - axis;
        (a.high[axis] - b.low[axis]).abs() <= f32::EPSILON
            && a.high[cross].min(b.high[cross]) > a.low[cross].max(b.low[cross])
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

fn mismatch(tiles: &[Option<Tile>], weights: &[f32]) -> f32 {
    let total_area: f32 = tiles.iter().flatten().map(|tile| tile.area()).sum();
    let total_weight: f32 = tiles
        .iter()
        .zip(weights)
        .filter_map(|(tile, &weight)| tile.map(|_| weight))
        .sum();
    tiles
        .iter()
        .zip(weights)
        .filter_map(|(tile, &weight)| {
            tile.map(|tile| (tile.area() / total_area - weight / total_weight).abs())
        })
        .sum::<f32>()
        * 0.5
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
        let space = Space::new(1.0, 4.0);
        let change = plan(12, &nodes, &weights, &target, &presence, space).unwrap();
        assert!(matches!(change.nodes[0], Node::Split { axis: 1, .. }));
        let unchanged = tiles(&nodes, &target, &presence, space).unwrap();
        assert!(mismatch(&change.to.tiles, &target) < mismatch(&unchanged, &target));
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
            detail: 1.0,
            nodes: nodes.to_vec(),
            reflows: vec![change.clone()].into(),
            labels: ["a", "b", "c", "d"].map(str::to_owned).to_vec().into(),
            inputs: vec![vec![content; 4]; end + 1].into(),
            samples: samples.into(),
            rotated: vec![false; 4],
            ..Packing::default()
        };
        let sample = |tick| {
            let (regions, _, _) = packing.sample(u64::try_from(tick).unwrap()).unwrap();
            Layout {
                tiles: regions.iter().map(|r| r.tile).collect(),
                gutters: regions.iter().map(|r| r.gutter).collect(),
            }
        };
        let middle = sample(mid);
        let mut previous = sample(11).tiles;
        assert_eq!(previous, change.from.tiles);
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
        assert_eq!(previous, change.to.tiles);
        let replay = sample(mid);
        assert_eq!(middle.tiles, replay.tiles);
        assert_eq!(middle.gutters, replay.gutters);
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
        assert!(connected(&from, &to));
        assert!(compatible(&from.gutters, &to.gutters));
        let change = Change {
            tick: 12,
            nodes: Vec::new(),
            from,
            to,
        };
        let midpoint = change.sample(12 + held::reflow_ticks() / 2).unwrap();
        assert_eq!(midpoint.gutters[0].unwrap().high.x, 0.5);
        assert_eq!(midpoint.gutters[1].unwrap().low.x, 0.5);
        assert!(change.sample(12 + held::reflow_ticks()).is_none());
    }
}
