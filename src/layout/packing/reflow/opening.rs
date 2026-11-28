use super::{
    Layout, compatible, connected, held,
    retirement::{best_slide, settled},
};
use crate::{
    layout::packing::{
        Feasible, Node, Packing, Space, Tile, feasibility, gutter_cells_in, place_in,
    },
    model::{count, framing},
};
use bevy::math::Vec2;

const MAX_REPOSITORIES: usize = 96;

const LOW_X: [f32; 3] = [0.16, 0.23, 0.30];
const HIGH_X: [f32; 3] = [0.72, 0.81, 0.87];
const LOW_Y: [f32; 3] = [0.11, 0.17, 0.23];
const HIGH_Y: [f32; 3] = [0.77, 0.85, 0.91];

fn fields(field: Tile, [x1, x2, y1, y2]: [f32; 4], mirrored: bool) -> [Tile; 5] {
    let [left, low] = field.low.to_array();
    let [right, high] = field.high.to_array();
    let x1 = left + x1 * (right - left);
    let x2 = left + x2 * (right - left);
    let y1 = low + y1 * (high - low);
    let y2 = low + y2 * (high - low);
    let tile = |l, b, r, t| {
        if mirrored {
            Tile {
                low: Vec2::new(left + right - r, b),
                high: Vec2::new(left + right - l, t),
            }
        } else {
            Tile {
                low: Vec2::new(l, b),
                high: Vec2::new(r, t),
            }
        }
    };
    [
        tile(left, low, x2, y1),
        tile(x2, low, right, y2),
        tile(x1, y2, right, high),
        tile(left, y1, x1, high),
        tile(x1, y1, x2, y2),
    ]
}

fn groups(
    active: &[usize],
    centers: &[Vec2],
    center: usize,
    fields: &[Tile; 5],
) -> [Vec<usize>; 5] {
    let mut groups: [Vec<usize>; 5] = std::array::from_fn(|_| Vec::new());
    let distance = |repo: usize, arm: usize| {
        let field = fields[arm];
        let point = centers[repo];
        let size = (field.high - field.low).max(Vec2::splat(0.01));
        (point - point.clamp(field.low, field.high)).length_squared()
            + 0.025 * ((point - field.low.midpoint(field.high)) / size).length_squared()
    };
    for &repo in active {
        if repo == center {
            groups[4].push(repo);
        } else {
            let arm = (0..4)
                .min_by(|&a, &b| distance(repo, a).total_cmp(&distance(repo, b)))
                .unwrap();
            groups[arm].push(repo);
        }
    }
    for arm in 0..4 {
        if groups[arm].is_empty() {
            let choice = (0..4)
                .filter(|&i| groups[i].len() > 1)
                .flat_map(|i| groups[i].iter().map(move |&repo| (i, repo)))
                .min_by(|&(_, a), &(_, b)| {
                    distance(a, arm).total_cmp(&distance(b, arm))
                });
            if let Some((from, repo)) = choice {
                groups[from].retain(|&i| i != repo);
                groups[arm].push(repo);
            }
        }
    }
    groups
}

fn split_group(
    nodes: &mut Vec<Node>,
    group: &mut [usize],
    centers: &[Vec2],
    weights: &[f32],
    field: Tile,
    style: usize,
) -> usize {
    let root = nodes.len();
    nodes.push(Node::Repository(group[0]));
    if group.len() == 1 {
        return root;
    }
    let [width, height] = field.pixels().to_array();
    let axis = match style {
        0 => usize::from(height > width),
        1 => usize::from(group.len() > 2 && height > width * 0.7),
        _ => usize::from(height > width * 1.4),
    };
    group.sort_unstable_by(|&a, &b| {
        centers[a][axis]
            .total_cmp(&centers[b][axis])
            .then(a.cmp(&b))
    });
    let total = group.iter().map(|&repo| weights[repo]).sum::<f32>();
    let mut sum = 0.0;
    let mut best = (1, weights[group[0]], f32::INFINITY);
    for (index, &repo) in group[..group.len() - 1].iter().enumerate() {
        sum += weights[repo];
        let cost = (sum - total * 0.5).abs() / total;
        if cost < best.2 {
            best = (index + 1, sum, cost);
        }
    }
    let (near, far) = group.split_at_mut(best.0);
    let cut = field.low[axis] + (field.high[axis] - field.low[axis]) * best.1 / total;
    let mut low = field;
    let mut high = field;
    low.high[axis] = cut;
    high.low[axis] = cut;
    let first = split_group(nodes, near, centers, weights, low, style);
    let second = split_group(nodes, far, centers, weights, high, style);
    nodes[root] = Node::Split {
        axis,
        first,
        second,
    };
    root
}

fn allocate(
    fields: [Tile; 5],
    groups: [Vec<usize>; 5],
    centers: &[Vec2],
    presence: &[f32],
    weights: &[f32],
    space: Space,
) -> Option<Layout> {
    let field = space.field(framing::MARGIN);
    let gap = Vec2::splat(space.gap) / framing::VIEWPORT;
    let mut layout = Layout {
        tiles: vec![None; weights.len()],
        gutters: vec![None; weights.len()],
    };
    for (cell, mut group) in fields.into_iter().zip(groups) {
        let inner = Tile {
            low: (cell.low + gap).max(field.low),
            high: (cell.high - gap).min(field.high),
        };
        let (tiles, gutters) = (0..3).find_map(|style| {
            let mut nodes = Vec::new();
            split_group(&mut nodes, &mut group, centers, weights, inner, style);
            let mut states = vec![Feasible::default(); nodes.len()];
            feasibility(&nodes, 0, weights, presence, space, &mut states);
            let tiles = place_in(&nodes, weights.len(), &states, inner)?;
            tiles.iter().flatten().copied().all(settled).then(|| {
                let gutters = gutter_cells_in(&nodes, &tiles, cell);
                (tiles, gutters)
            })
        })?;
        for repo in group {
            layout.tiles[repo] = tiles[repo];
            layout.gutters[repo] = gutters[repo];
        }
    }
    (compatible(&layout.tiles, &layout.tiles)
        && compatible(&layout.gutters, &layout.gutters)
        && connected(&layout, &layout, false))
    .then_some(layout)
}

fn shared_gutters(layout: &Layout) -> f32 {
    let mut cost = 0.0;
    for axis in 0..2 {
        let cross = 1 - axis;
        let mut sides: Vec<_> = layout
            .gutters
            .iter()
            .flatten()
            .flat_map(|cell| {
                let low = cell.low[cross];
                let high = cell.high[cross];
                [
                    (cell.low[axis], low, high, false),
                    (cell.high[axis], low, high, true),
                ]
            })
            .collect();
        sides.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let mut sides = sides.as_slice();
        while let Some(&(cut, start, mut end, high)) = sides.first() {
            let mut seen = [!high, high];
            let mut used = 1_usize;
            for &(position, low, far, high) in &sides[1..] {
                if position.to_bits() != cut.to_bits() || low > end {
                    break;
                }
                end = end.max(far);
                seen[usize::from(high)] = true;
                used += 1;
            }
            if seen == [true, true] {
                cost += (end - start) * count(used.saturating_sub(2));
            }
            sides = &sides[used..];
        }
    }
    cost
}

pub(in crate::layout::packing) fn glyph_scales(
    packing: &Packing,
    tick: usize,
) -> impl Fn(usize, Tile) -> f32 + '_ {
    let shapes: Vec<_> = packing.inputs[tick]
        .iter()
        .zip(&packing.rotated)
        .map(|(input, &rotated)| {
            let shape = input.shape(rotated);
            let center = packing.content_center(tick, &shape);
            (shape, center, input.height)
        })
        .collect();
    move |repo, tile| {
        let (shape, center, height) = &shapes[repo];
        packing.fit_scale(tile, shape, *height, *center).max(1e-6)
    }
}

fn glyph_fit<'a>(
    packing: &'a Packing,
    tick: usize,
    active: &'a [usize],
) -> impl Fn(&Layout) -> (f32, f32) + 'a {
    let size = glyph_scales(packing, tick);
    move |layout: &Layout| {
        let mut sizes: Vec<_> = active
            .iter()
            .map(|&repo| size(repo, layout.tiles[repo].unwrap()))
            .collect();
        sizes.sort_unstable_by(f32::total_cmp);
        let end = sizes.len() - 1;
        let p10 = sizes[end / 10].ln();
        let p90 = sizes[9 * end / 10].ln();
        let mean = sizes.iter().map(|s| s.ln()).sum::<f32>() / count(sizes.len());
        (0.50 * p10 + 0.45 * mean - 0.05 * p90, p90 - p10)
    }
}

pub(super) fn plan_shift(
    packing: &Packing,
    current: &Layout,
    weights: &[f32],
    ticks: usize,
) -> Option<Layout> {
    if current.tiles.iter().flatten().take(2).count() < 2 {
        return None;
    }
    best_slide(current, packing.space(), ticks, Some(weights), |layout| {
        let (area, total) = super::totals(&layout.tiles, weights);
        let density = total / area;
        -layout
            .tiles
            .iter()
            .zip(weights)
            .filter(|&(_, &files)| files > 0.0)
            .filter_map(|(&tile, &files)| {
                tile.map(|tile| {
                    files * super::super::log_density_error(tile, files, density)
                })
            })
            .sum::<f32>()
    })
}

pub(in crate::layout::packing) fn plan(
    packing: &Packing,
    original: &Layout,
    certified: impl Fn(&Layout) -> bool,
) -> Option<Layout> {
    if original.tiles.len() > MAX_REPOSITORIES {
        return None;
    }
    let active: Vec<_> = original
        .tiles
        .iter()
        .enumerate()
        .filter_map(|(repo, t)| t.map(|_| repo))
        .collect();
    if active.len() < 5 {
        return None;
    }
    let sample = &packing.samples[0];
    let centers: Vec<_> = original
        .gutters
        .iter()
        .map(|cell| cell.map_or(Vec2::ZERO, |cell| cell.low.midpoint(cell.high)))
        .collect();
    let fit = glyph_fit(packing, 0, &active);
    let (original_score, original_spread) = fit(original);
    let original_gutters = shared_gutters(original);
    let mut best_score = original_score + 0.005;
    let mut best = None;
    let weights = &sample.weights;
    let center = *active
        .iter()
        .max_by(|&&a, &&b| weights[a].total_cmp(&weights[b]))?;
    let space = packing.space();
    let field = space.field(crate::layout::packing::GUTTER_MARGIN);
    let ranges = if active.len() == 5 {
        [
            [0.27, 0.32, 0.37],
            [0.63, 0.68, 0.73],
            [0.32, 0.36, 0.40],
            [0.60, 0.64, 0.68],
        ]
    } else {
        [LOW_X, HIGH_X, LOW_Y, HIGH_Y]
    };
    let pairs = |near: [f32; 3], far: [f32; 3]| {
        near.into_iter()
            .flat_map(move |low| far.into_iter().map(move |high| [low, high]))
    };
    let [left, right, bottom, top] = ranges;
    for [x1, x2] in pairs(left, right) {
        for [y1, y2] in pairs(bottom, top) {
            for mirrored in [false, true] {
                let fields = fields(field, [x1, x2, y1, y2], mirrored);
                let groups = groups(&active, &centers, center, &fields);
                let Some(layout) = allocate(
                    fields,
                    groups,
                    &centers,
                    &sample.presence,
                    weights,
                    space,
                ) else {
                    continue;
                };
                let (score, spread) = fit(&layout);
                let gutter_cost = (shared_gutters(&layout) - original_gutters)
                    / original_gutters.max(1.0);
                let score = score - 0.05 * gutter_cost;
                if score > best_score && spread <= original_spread && certified(&layout)
                {
                    best_score = score;
                    best = Some(layout);
                }
            }
        }
    }
    best
}

#[derive(Debug)]
pub(in crate::layout::packing) struct FlatLayout {
    opening: Layout,
    changes: Vec<Shift>,
}

#[derive(Debug)]
struct Shift {
    start: usize,
    end: usize,
    layout: Layout,
}

impl FlatLayout {
    pub(in crate::layout::packing) fn retained_bytes(
        repositories: u128,
        shifts: u128,
    ) -> u128 {
        let rectangles =
            repositories * (2 * std::mem::size_of::<Option<Tile>>()) as u128;
        (std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()) as u128
            + rectangles
            + shifts * (rectangles + std::mem::size_of::<Shift>() as u128)
    }

    pub(in crate::layout::packing) fn plan(
        packing: &Packing,
        opening: Layout,
    ) -> Option<Self> {
        let active: Vec<_> = opening
            .tiles
            .iter()
            .enumerate()
            .filter_map(|(repo, tile)| tile.map(|_| repo))
            .collect();
        let mut changes = Vec::<Shift>::with_capacity(packing.reflows.len());
        let last_tick = packing.samples.len() - 1;
        for change in packing.reflows.iter() {
            let start = change.tick;
            let end = (start + change.duration()).min(last_tick);
            let previous = changes.last().map_or(&opening, |shift| &shift.layout);
            let fit = glyph_fit(packing, end, &active);
            let original_gutters = shared_gutters(previous).max(1.0);
            let score = |layout: &Layout| {
                fit(layout).0 - 0.05 * shared_gutters(layout) / original_gutters
            };
            let candidate =
                best_slide(previous, packing.space(), end - start, None, score);
            let after = candidate.as_ref().unwrap_or(previous);
            let baseline = packing.layout_at(end, &[], None, None);
            if score(after) + 0.005 < score(&baseline)
                || fit(after).1 > fit(&baseline).1
            {
                return None;
            }
            if let Some(layout) = candidate {
                changes.push(Shift { start, end, layout });
            }
        }
        Some(Self { opening, changes })
    }

    pub(in crate::layout::packing) fn sample(&self, tick: usize) -> Layout {
        let count = self.changes.partition_point(|shift| shift.start <= tick);
        let Some(current) = self.changes[..count].last() else {
            return self.opening.clone();
        };
        if tick >= current.end {
            return current.layout.clone();
        }
        let previous = self.changes[..count - 1]
            .last()
            .map_or(&self.opening, |shift| &shift.layout);
        previous.blend_edges(
            &current.layout,
            held::progress(tick - current.start, current.end - current.start),
        )
    }
}
