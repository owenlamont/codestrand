use std::{borrow::Cow, sync::Arc};

use bevy::math::{FloatExt, Vec2};

use super::{RepositoryRegion, outline};
use crate::model::{BIRTH_TICKS, caption, framing};

mod fit;
mod held;
mod reflow;

const BOUND: f32 = 3.5;
pub(crate) const DENSITY_TOLERANCE: f32 = 0.25;
const ALLOCATION_TOLERANCE_PX: f32 = 0.25;

const FIELD_GROWTH_STEP: f32 = 1.125;
/// Bisection steps towards full presence for a sample whose easing tiles conflict.
const FIRMING_STEPS: u32 = 8;
/// Insertion candidates each scoring thread takes at least, so small trees stay serial.
const PARALLEL_SPLITS: usize = 32;

#[derive(Clone, Copy, Debug)]
struct Space {
    gap: f32,
    growth: f32,
}

impl Space {
    fn new(gap: f32, growth: f32) -> Self {
        Self { gap, growth }
    }

    fn field(self, margin: f32) -> Tile {
        let field = Tile::viewport();
        let center = field.low.midpoint(field.high);
        let field = Tile::inset(margin);
        Tile {
            low: field.low + (field.low - center) * (self.growth - 1.0),
            high: field.high + (field.high - center) * (self.growth - 1.0),
        }
    }
}

/// Inserts repositories in birth order, growing the field if the gutters need more room.
fn plan_tree(
    regions: &[Vec<Input>],
    items: &[Vec<f32>],
    births: &[usize],
    order: &[usize],
    mut space: Space,
    threads: usize,
    history: &[f32],
) -> anyhow::Result<(Vec<Node>, Space)> {
    let windows: Vec<_> = order
        .iter()
        .enumerate()
        .map(|(position, &i)| {
            let begin = births[i].min(regions.len() - 1);
            let end = order
                .get(position + 1)
                .map_or(regions.len(), |&j| births[j].min(regions.len()))
                .max(begin + 1);
            (begin, end)
        })
        .collect();
    let place = |space: Space| {
        let mut nodes = Vec::new();
        let mut born = vec![false; order.len()];
        for (&i, &(begin, end)) in order.iter().zip(&windows) {
            born[i] = true;
            let (weights, aspects) = birth_weights(regions, items, &born, begin, end);
            if !insert(
                &mut nodes,
                i,
                &weights,
                &aspects,
                Some(history),
                space,
                threads,
            ) {
                return None;
            }
        }
        Some(nodes)
    };
    loop {
        if let Some(nodes) = place(space) {
            return Ok((nodes, space));
        }
        space.growth *= FIELD_GROWTH_STEP;
        anyhow::ensure!(
            space
                .field(framing::MARGIN)
                .pixels()
                .length_squared()
                .is_finite(),
            "repository field dimensions exceed finite layout arithmetic"
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Tile {
    pub low: Vec2,
    pub high: Vec2,
}

impl Tile {
    pub(crate) fn viewport() -> Self {
        Self::inset(framing::MARGIN)
    }

    /// The field between the caption bars, `margin` reference pixels inside each edge.
    pub(crate) fn inset(margin: f32) -> Self {
        let limit = Vec2::ONE - Vec2::splat(2.0 * margin) / framing::VIEWPORT;
        Self {
            low: Vec2::new(
                -limit.x,
                (caption::BOTTOM + margin) * 2.0 / framing::VIEWPORT.y,
            ),
            high: Vec2::new(
                limit.x,
                (caption::TOP - margin) * 2.0 / framing::VIEWPORT.y,
            ),
        }
    }

    pub(crate) fn area(self) -> f32 {
        (self.high - self.low).element_product() * 0.25
    }

    pub(crate) fn usable(self) -> Self {
        let size = self.high - self.low;
        let padding = (Vec2::splat(8.0) * 2.0 / framing::VIEWPORT).min(size * 0.2);
        Self {
            low: self.low + padding,
            high: self.high - padding,
        }
    }

    pub(crate) fn pixels(self) -> Vec2 {
        (self.high - self.low) * framing::VIEWPORT * 0.5
    }

    pub(crate) fn ground_aspect(self) -> f32 {
        let size = self.pixels();
        size.x / size.y
    }

    pub(crate) fn corners(self) -> [Vec2; 4] {
        [
            self.low,
            Vec2::new(self.high.x, self.low.y),
            self.high,
            Vec2::new(self.low.x, self.high.y),
        ]
    }

    pub(crate) fn ground(self) -> [Vec2; 4] {
        self.corners().map(framing::field_to_world)
    }
}

#[cfg(test)]
/// Width of the gutter avatars travel along, in reference pixels.
pub(crate) const AVATAR_GUTTER: f32 = 16.0;

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Repository(usize),
    Split {
        axis: usize,
        first: usize,
        second: usize,
    },
}

#[derive(Clone, Default)]
struct Feasible {
    sizes: Vec<Vec2>,
    weight: f32,
    presence: f32,
    gap: f32,
}

fn interval(points: &[Vec2], axis: usize, cross: f32) -> Option<(f32, f32)> {
    let mut span = (f32::INFINITY, f32::NEG_INFINITY);
    let mut before = *points.last()?;
    for &point in points {
        widen(&mut span, before, point, axis, cross);
        before = point;
    }
    (span.0 <= span.1).then_some(span)
}

/// Widens `span` by where the edge from `before` to `point` meets `cross`.
fn widen(span: &mut (f32, f32), before: Vec2, point: Vec2, axis: usize, cross: f32) {
    let delta = point[1 - axis] - before[1 - axis];
    if delta.abs() > 1e-5 {
        let t = (cross - before[1 - axis]) / delta;
        if (-1e-5..=1.00001).contains(&t) {
            let value = before[axis].lerp(point[axis], t.clamp(0.0, 1.0));
            span.0 = span.0.min(value);
            span.1 = span.1.max(value);
        }
    } else if (cross - point[1 - axis]).abs() < 0.001 {
        span.0 = span.0.min(point[axis]).min(before[axis]);
        span.1 = span.1.max(point[axis]).max(before[axis]);
    }
}

/// [`interval`] at each ascending `crosses` value, sweeping only the edges whose padded
/// cross range can meet it.
fn intervals(points: &[Vec2], axis: usize, crosses: &[f32]) -> Vec<Option<(f32, f32)>> {
    let Some(&last) = points.last() else {
        return vec![None; crosses.len()];
    };
    let mut edges: Vec<_> = std::iter::once(last)
        .chain(points.iter().copied())
        .zip(points.iter().copied())
        .map(|(before, point)| {
            let (low, high) = (before[1 - axis], point[1 - axis]);
            let pad = 0.001 + 1e-4 * (high - low).abs();
            (low.min(high) - pad, low.max(high) + pad, before, point)
        })
        .collect();
    edges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut next = 0;
    let mut active = Vec::new();
    crosses
        .iter()
        .map(|&cross| {
            while next < edges.len() && edges[next].0 <= cross {
                active.push(edges[next]);
                next += 1;
            }
            active.retain(|edge| edge.1 >= cross);
            let mut span = (f32::INFINITY, f32::NEG_INFINITY);
            for &(_, _, before, point) in &active {
                widen(&mut span, before, point, axis, cross);
            }
            (span.0 <= span.1).then_some(span)
        })
        .collect()
}

fn combine(a: &[Vec2], b: &[Vec2], axis: usize, gap: f32) -> Vec<Vec2> {
    let mut coordinates: Vec<_> = a.iter().chain(b).map(|p| p[1 - axis]).collect();
    coordinates.sort_by(f32::total_cmp);
    coordinates.dedup_by(|a, b| (*a - *b).abs() < 0.001);
    let mut low = Vec::new();
    let mut high = Vec::new();
    let spans = intervals(a, axis, &coordinates).into_iter().zip(intervals(
        b,
        axis,
        &coordinates,
    ));
    for (cross, (a_span, b_span)) in coordinates.iter().copied().zip(spans) {
        if let Some(((a0, a1), (b0, b1))) = a_span.zip(b_span) {
            let mut p = Vec2::splat(cross);
            p[axis] = a0 + b0 + gap;
            low.push(p);
            p[axis] = a1 + b1 + gap;
            high.push(p);
        }
    }
    low.extend(high.into_iter().rev());
    low.dedup_by(|a, b| a.distance_squared(*b) < 1e-6);
    if low
        .first()
        .zip(low.last())
        .is_some_and(|(a, b)| a.distance_squared(*b) < 1e-6)
    {
        low.pop();
    }
    let length = low.len();
    (0..length)
        .filter_map(|i| {
            let before = low[(i + length - 1) % length];
            let point = low[i];
            let after = low[(i + 1) % length];
            let first = point - before;
            let second = after - point;
            (first.perp_dot(second).abs() > 1e-6 * first.length() * second.length())
                .then_some(point)
        })
        .collect()
}

fn feasibility(
    nodes: &[Node],
    index: usize,
    weights: &[f32],
    presence: &[f32],
    space: Space,
    result: &mut [Feasible],
) {
    if let Node::Split { first, second, .. } = nodes[index] {
        feasibility(nodes, first, weights, presence, space, result);
        feasibility(nodes, second, weights, presence, space, result);
    }
    update(nodes, index, weights, presence, space, result);
}

fn update(
    nodes: &[Node],
    index: usize,
    weights: &[f32],
    presence: &[f32],
    space: Space,
    result: &mut [Feasible],
) {
    let limit = space.field(framing::MARGIN).pixels();
    let state = match nodes[index] {
        Node::Repository(i) => {
            let p = presence[i];
            let mut sizes = outline::rectangle(Vec2::ZERO, limit);
            for normal in [Vec2::new(-1.0, 1.0 / BOUND), Vec2::new(1.0, -BOUND)] {
                sizes = outline::clip(&sizes, normal, 0.0);
            }
            Feasible {
                sizes,
                weight: weights[i] * p,
                presence: p,
                gap: 0.0,
            }
        }
        Node::Split {
            axis,
            first,
            second,
        } => {
            let a = &result[first];
            let b = &result[second];
            let presence = a.presence.max(b.presence);
            let gutter = space.gap * a.presence.min(b.presence) / presence.max(1e-9);
            let sizes = if a.presence == 0.0 {
                b.sizes.clone()
            } else if b.presence == 0.0 {
                a.sizes.clone()
            } else {
                let scaled = |child: &Feasible| {
                    child
                        .sizes
                        .iter()
                        .map(|&p| {
                            let mut p = p;
                            p[axis] *= child.presence / presence;
                            p
                        })
                        .collect::<Vec<_>>()
                };
                combine(&scaled(a), &scaled(b), axis, gutter)
            };
            Feasible {
                sizes,
                weight: a.weight + b.weight,
                presence: a.presence.max(b.presence),
                gap: gutter,
            }
        }
    };
    result[index] = state;
}

fn allocate(
    nodes: &[Node],
    index: usize,
    tile: Tile,
    states: &[Feasible],
    stretch: Vec2,
    tiles: &mut [Option<Tile>],
) -> Option<()> {
    if states[index].presence == 0.0 {
        return Some(());
    }
    match nodes[index] {
        Node::Repository(i) => tiles[i] = Some(tile),
        Node::Split {
            axis,
            first,
            second,
        } => {
            let a = &states[first];
            let b = &states[second];
            if a.presence == 0.0 {
                return allocate(nodes, second, tile, states, stretch, tiles);
            }
            if b.presence == 0.0 {
                return allocate(nodes, first, tile, states, stretch, tiles);
            }
            let size = tile.pixels() / stretch;
            let pa = a.presence / states[index].presence;
            let pb = b.presence / states[index].presence;
            let child_interval = |child: &Feasible| {
                if child.sizes.is_empty() {
                    return None;
                }
                let cross = size[1 - axis];
                let (low, high) = outline::bounds(&child.sizes);
                let clamped = cross.clamp(low[1 - axis], high[1 - axis]);
                // Inverting a subpixel birth sliver amplifies cancellation in its tile edges.
                if (cross - clamped).abs() * stretch[1 - axis]
                    >= ALLOCATION_TOLERANCE_PX
                {
                    return None;
                }
                interval(&child.sizes, axis, clamped)
            };
            let (a0, a1) = child_interval(a)?;
            let (b0, b1) = child_interval(b)?;
            let (a0, a1, b0, b1) = (a0 * pa, a1 * pa, b0 * pb, b1 * pb);
            let available = size[axis] - states[index].gap;
            if available < 0.0 {
                return None;
            }
            let low = a0.max(available - b1);
            let high = a1.min(available - b0);
            if (low - high) * stretch[axis] > ALLOCATION_TOLERANCE_PX {
                return None;
            }
            let share = available * a.weight / (a.weight + b.weight).max(1e-9);
            // Tile-edge cancellation during births is bounded in reference pixels, not unstretched coordinates.
            let extent = if low > high {
                low.midpoint(high)
            } else {
                share.clamp(low, high)
            };
            let extent = extent.clamp(0.0, available);
            let mut near = tile;
            let mut far = tile;
            near.high[axis] =
                tile.low[axis] + 2.0 * extent * stretch[axis] / framing::VIEWPORT[axis];
            far.low[axis] = near.high[axis]
                + 2.0 * states[index].gap * stretch[axis] / framing::VIEWPORT[axis];
            let mut sa = stretch;
            let mut sb = stretch;
            sa[axis] *= pa;
            sb[axis] *= pb;
            allocate(nodes, first, near, states, sa, tiles)?;
            allocate(nodes, second, far, states, sb, tiles)?;
        }
    }
    Some(())
}

/// Recomputes only the weights of feasibility states, as [`update`] would.
fn reweigh(
    nodes: &[Node],
    index: usize,
    weights: &[f32],
    presence: &[f32],
    result: &mut [Feasible],
) {
    result[index].weight = match nodes[index] {
        Node::Repository(i) => weights[i] * presence[i],
        Node::Split { first, second, .. } => {
            reweigh(nodes, first, weights, presence, result);
            reweigh(nodes, second, weights, presence, result);
            result[first].weight + result[second].weight
        }
    };
}

/// Feasibility states for the last tree and presence vector, with reuse counts.
#[derive(Default)]
struct Tiling {
    tree_tick: usize,
    presence: Vec<u32>,
    states: Vec<Feasible>,
    samples: usize,
    reuses: usize,
}

#[derive(Default)]
struct TilingCache(std::sync::Mutex<Tiling>);

impl TilingCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Tiling> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Clone for TilingCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for TilingCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TilingCache")
    }
}

fn tiles(
    nodes: &[Node],
    weights: &[f32],
    presence: &[f32],
    space: Space,
) -> Option<Vec<Option<Tile>>> {
    let mut states = vec![Feasible::default(); nodes.len()];
    feasibility(nodes, 0, weights, presence, space, &mut states);
    allocated(nodes, weights.len(), &states, space)
}

fn allocated(
    nodes: &[Node],
    repositories: usize,
    states: &[Feasible],
    space: Space,
) -> Option<Vec<Option<Tile>>> {
    let field = space.field(framing::MARGIN);
    if states[0].presence == 0.0 {
        return Some(vec![None; repositories]);
    }
    let size = field.pixels();
    let (low, high) = interval(&states[0].sizes, 0, size.y)?;
    if size.x < low - ALLOCATION_TOLERANCE_PX || size.x > high + ALLOCATION_TOLERANCE_PX
    {
        return None;
    }
    place_in(nodes, repositories, states, field)
}

fn place_in(
    nodes: &[Node],
    repositories: usize,
    states: &[Feasible],
    field: Tile,
) -> Option<Vec<Option<Tile>>> {
    let mut output = vec![None; repositories];
    allocate(nodes, 0, field, states, Vec2::ONE, &mut output)?;
    for tile in output.iter_mut().flatten() {
        *tile = scaled_tile(*tile, field.low.midpoint(field.high), states[0].presence);
    }
    Some(output)
}

fn scaled_tile(tile: Tile, center: Vec2, presence: f32) -> Tile {
    Tile {
        low: center.lerp(tile.low, presence),
        high: center.lerp(tile.high, presence),
    }
}

fn birth_presence(elapsed: u64) -> f32 {
    held::birth_ease(
        usize::try_from(elapsed).expect("bounded birth"),
        usize::try_from(BIRTH_TICKS).expect("bounded birth"),
    )
}

fn exit_presence(tick: u64, exit: crate::model::RepositoryExit) -> f32 {
    if tick >= exit.end {
        return 0.0;
    }
    let elapsed =
        usize::try_from(tick.saturating_sub(exit.start)).expect("bounded exit");
    let duration = usize::try_from(exit.end - exit.start).expect("bounded exit");
    1.0 - held::progress(elapsed, duration)
}

fn shrink_tiles(tiles: &mut [Option<Tile>], presence: &[f32]) {
    for (tile, &p) in tiles.iter_mut().zip(presence) {
        *tile = tile
            .filter(|_| p > 0.0)
            .map(|tile| scaled_tile(tile, tile.low.midpoint(tile.high), p));
    }
}

fn nearest_tiles(
    nodes: &[Node],
    weights: &[f32],
    presence: &[f32],
    space: Space,
) -> Option<Vec<Option<Tile>>> {
    let mut states = vec![Feasible::default(); nodes.len()];
    feasibility(nodes, 0, weights, presence, space, &mut states);
    let field = space.field(framing::MARGIN);
    let target = field.pixels();
    let sizes = outline::clip(&states[0].sizes, Vec2::X, target.x);
    let sizes = outline::clip(&sizes, Vec2::Y, target.y);
    let mut before = *sizes.last()?;
    let mut closest = before;
    for point in sizes {
        let edge = point - before;
        let length = edge.length_squared();
        if length > 0.0 {
            let fraction = ((target - before).dot(edge) / length).clamp(0.0, 1.0);
            let candidate = before.lerp(point, fraction);
            if candidate.distance_squared(target) < closest.distance_squared(target) {
                closest = candidate;
            }
        }
        before = point;
    }
    let center = field.low.midpoint(field.high);
    let radius = closest / framing::VIEWPORT;
    place_in(
        nodes,
        weights.len(),
        &states,
        Tile {
            low: center - radius,
            high: center + radius,
        },
    )
}

fn repository_ranks(order: &[usize]) -> Vec<usize> {
    let mut rank = vec![0; order.len()];
    for (position, &repository) in order.iter().enumerate() {
        rank[repository] = position;
    }
    rank
}

fn birth_order(nodes: &[Node], births: &[usize]) -> Vec<usize> {
    fn visit(
        nodes: &[Node],
        index: usize,
        births: &[usize],
        order: &mut [usize],
    ) -> usize {
        let born = match nodes[index] {
            Node::Repository(repository) => births[repository],
            Node::Split { first, second, .. } => visit(nodes, first, births, order)
                .min(visit(nodes, second, births, order)),
        };
        order[index] = born;
        born
    }
    let mut order = vec![usize::MAX; nodes.len()];
    visit(nodes, 0, births, &mut order);
    order
}

#[derive(Clone, Debug)]
struct Opening {
    end: usize,
    cuts: Vec<(f32, f32)>,
    order: Vec<usize>,
}

impl Opening {
    fn new(
        nodes: &[Node],
        samples: &[Sample],
        births: &[usize],
        order: &[usize],
        windows: &[(usize, usize)],
        exits: &[Option<crate::model::RepositoryExit>],
        space: Space,
    ) -> Option<Self> {
        let end = births[*order.last()?];
        if order.len() < 3
            || births[order[0]] != 0
            || births[order[1]] == 0
            || end >= samples.len()
            || exits.iter().flatten().any(|exit| exit.start <= end as u64)
            || order[1..]
                .windows(2)
                .any(|pair| windows[pair[1]].0 > births[pair[0]])
        {
            return None;
        }
        let weights = &samples[end].weights;
        let field = tiles(nodes, weights, &vec![1.0; births.len()], space)?;
        let mut cuts = vec![(0.0, 0.0); nodes.len()];
        Self::cuts(nodes, 0, &field, &mut cuts)?;
        Some(Self {
            end,
            cuts,
            order: birth_order(nodes, &repository_ranks(order)),
        })
    }

    fn cuts(
        nodes: &[Node],
        index: usize,
        tiles: &[Option<Tile>],
        cuts: &mut [(f32, f32)],
    ) -> Option<Tile> {
        match nodes[index] {
            Node::Repository(repository) => tiles[repository],
            Node::Split {
                axis,
                first,
                second,
            } => {
                let a = Self::cuts(nodes, first, tiles, cuts)?;
                let b = Self::cuts(nodes, second, tiles, cuts)?;
                let gap = b.low[axis] - a.high[axis];
                let share =
                    (a.high[axis] - a.low[axis]) / (b.high[axis] - a.low[axis] - gap);
                cuts[index] = (share, gap);
                Some(Tile {
                    low: a.low.min(b.low),
                    high: a.high.max(b.high),
                })
            }
        }
    }

    fn tiles(
        &self,
        nodes: &[Node],
        presence: &[f32],
        space: Space,
    ) -> Vec<Option<Tile>> {
        let prefix = presence.iter().filter(|&&p| p > 0.0).count();
        let mut tiles = vec![None; presence.len()];
        self.place(nodes, 0, space.field(framing::MARGIN), prefix, &mut tiles);
        tiles
    }

    fn place(
        &self,
        nodes: &[Node],
        index: usize,
        tile: Tile,
        prefix: usize,
        tiles: &mut [Option<Tile>],
    ) {
        match nodes[index] {
            Node::Repository(repository) => tiles[repository] = Some(tile),
            Node::Split {
                axis,
                first,
                second,
            } => {
                if self.order[first] >= prefix {
                    self.place(nodes, second, tile, prefix, tiles);
                } else if self.order[second] >= prefix {
                    self.place(nodes, first, tile, prefix, tiles);
                } else {
                    let (share, gap) = self.cuts[index];
                    let mut near = tile;
                    let mut far = tile;
                    near.high[axis] = tile.low[axis]
                        + (tile.high[axis] - tile.low[axis] - gap) * share;
                    far.low[axis] = near.high[axis] + gap;
                    self.place(nodes, first, near, prefix, tiles);
                    self.place(nodes, second, far, prefix, tiles);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Transition<'a> {
    Birth(Option<&'a Opening>),
    Exit,
}

fn collapse_missing(
    nodes: &[Node],
    index: usize,
    tiles: &mut [Option<Tile>],
    order: Option<&[usize]>,
) -> Option<Tile> {
    fn fill(
        nodes: &[Node],
        index: usize,
        tile: Tile,
        tiles: &mut [Option<Tile>],
        order: Option<&[usize]>,
    ) {
        match nodes[index] {
            Node::Repository(i) => tiles[i] = Some(tile),
            Node::Split {
                axis,
                first,
                second,
            } => {
                let cut = order.map_or_else(
                    || tile.low[axis].midpoint(tile.high[axis]),
                    |order| {
                        if order[first] <= order[second] {
                            tile.high[axis]
                        } else {
                            tile.low[axis]
                        }
                    },
                );
                let mut near = tile;
                let mut far = tile;
                near.high[axis] = cut;
                far.low[axis] = cut;
                fill(nodes, first, near, tiles, order);
                fill(nodes, second, far, tiles, order);
            }
        }
    }
    let edge = |tile: Tile, axis: usize, position: f32| {
        let center = tile.low.midpoint(tile.high);
        let mut zero = if order.is_some() {
            tile
        } else {
            Tile {
                low: center,
                high: center,
            }
        };
        zero.low[axis] = position;
        zero.high[axis] = position;
        zero
    };
    match nodes[index] {
        Node::Repository(i) => tiles[i],
        Node::Split {
            axis,
            first,
            second,
        } => {
            match (
                collapse_missing(nodes, first, tiles, order),
                collapse_missing(nodes, second, tiles, order),
            ) {
                (Some(a), Some(b)) => Some(Tile {
                    low: a.low.min(b.low),
                    high: a.high.max(b.high),
                }),
                (None, Some(tile)) => {
                    fill(nodes, first, edge(tile, axis, tile.low[axis]), tiles, order);
                    Some(tile)
                }
                (Some(tile), None) => {
                    fill(
                        nodes,
                        second,
                        edge(tile, axis, tile.high[axis]),
                        tiles,
                        order,
                    );
                    Some(tile)
                }
                (None, None) => None,
            }
        }
    }
}

fn reflow_window(
    tick: usize,
    presence: f32,
    birth: usize,
    opening: usize,
    exit: Option<crate::model::RepositoryExit>,
) -> usize {
    let now = u64::try_from(tick).expect("bounded packing tick");
    if presence >= 1.0 && tick > birth {
        exit.map_or(usize::MAX, |exit| {
            usize::try_from(exit.start.saturating_sub(now + 1)).unwrap_or(usize::MAX)
        })
    } else if presence != 0.0 {
        0
    } else if exit.is_some_and(|exit| now >= exit.end) {
        usize::MAX
    } else {
        opening.saturating_sub(tick)
    }
}

fn narrow_tile_cost(tile: Tile) -> f32 {
    (1.0 - tile.ground_aspect()).max(0.0)
}

fn insertion_cost(
    placement: &[Option<Tile>],
    weights: &[f32],
    aspects: &[f32],
    density: f32,
    history: Option<&[f32]>,
    history_total: f32,
) -> (f32, f32) {
    let current = placement
        .iter()
        .enumerate()
        .filter_map(|(i, tile)| {
            tile.map(|tile| {
                weights[i]
                    * ((tile.area() * density / weights[i]).ln().abs()
                        + (tile.ground_aspect() / aspects[i]).ln().abs())
            })
        })
        .sum();
    let lifetime = history
        .filter(|_| history_total > 0.0)
        .map_or(0.0, |history| {
            let used = placement
                .iter()
                .flatten()
                .map(|tile| tile.area())
                .sum::<f32>();
            placement
                .iter()
                .zip(history)
                .filter_map(|(&tile, &weight)| {
                    tile.map(|tile| {
                        let share = tile.area() / used;
                        (share - weight / history_total).abs()
                            + 0.025 * share * narrow_tile_cost(tile)
                    })
                })
                .sum()
        });
    (current, lifetime)
}

fn insert(
    nodes: &mut Vec<Node>,
    repository: usize,
    weights: &[f32],
    aspects: &[f32],
    history: Option<&[f32]>,
    space: Space,
    threads: usize,
) -> bool {
    if nodes.is_empty() {
        nodes.push(Node::Repository(repository));
        return true;
    }
    let presence: Vec<_> = weights.iter().map(|&w| f32::from(w > 0.0)).collect();
    let mut cached = vec![Feasible::default(); nodes.len()];
    feasibility(nodes, 0, weights, &presence, space, &mut cached);
    let mut leaf = vec![Feasible::default()];
    update(
        &[Node::Repository(repository)],
        0,
        weights,
        &presence,
        space,
        &mut leaf,
    );
    let mut parents = vec![None; nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        if let Node::Split { first, second, .. } = node {
            parents[*first] = Some(index);
            parents[*second] = Some(index);
        }
    }
    let split = |host: usize, axis: usize| {
        let mut candidate = nodes.clone();
        let first = candidate.len();
        candidate.push(candidate[host].clone());
        candidate.push(Node::Repository(repository));
        candidate[host] = Node::Split {
            axis,
            first,
            second: first + 1,
        };
        candidate
    };
    let total: f32 = weights.iter().sum();
    let field_area = space.field(framing::MARGIN).area();
    let history_total: f32 = history.map_or(0.0, |history| {
        history
            .iter()
            .zip(weights)
            .filter(|(_, w)| **w > 0.0)
            .map(|(&h, _)| h)
            .sum()
    });
    let score = |&(host, axis): &(usize, usize)| {
        let candidate = split(host, axis);
        let mut states = cached.clone();
        states.push(cached[host].clone());
        states.push(leaf[0].clone());
        let mut ancestor = Some(host);
        while let Some(index) = ancestor {
            update(&candidate, index, weights, &presence, space, &mut states);
            ancestor = parents[index];
        }
        let placement = allocated(&candidate, weights.len(), &states, space)?;
        Some(insertion_cost(
            &placement,
            weights,
            aspects,
            total / field_area,
            history,
            history_total,
        ))
    };
    let splits: Vec<_> = (0..nodes.len())
        .flat_map(|host| [(host, 0), (host, 1)])
        .collect();
    let workers = match threads {
        0 => std::thread::available_parallelism().map_or(1, usize::from),
        forced => forced,
    };
    let chunk = splits.len().div_ceil(workers).max(PARALLEL_SPLITS);
    let scores = super::in_order(&splits, chunk, workers, |chunk| {
        chunk.iter().map(score).collect()
    });
    let min_current = scores
        .iter()
        .flatten()
        .map(|&(current, _)| current)
        .fold(f32::INFINITY, f32::min);
    let comparable = if history.is_some() {
        total * 0.005
    } else {
        1e-5
    };
    let best = splits
        .iter()
        .zip(scores)
        .filter_map(|(&split, score)| {
            score
                .filter(|&(current, _)| current <= min_current + comparable)
                .map(|(current, lifetime)| (lifetime, current, split))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let Some((_, _, (host, axis))) = best else {
        return false;
    };
    *nodes = split(host, axis);
    true
}

#[derive(Clone, Debug)]
struct Sample {
    weights: Vec<f32>,
    presence: Vec<f32>,
}

#[derive(Clone, Copy, Debug)]
struct Placement {
    tile: Option<Tile>,
    gutter: Option<Tile>,
    origin: Vec2,
    scale: f32,
}

fn gutter_cells(
    nodes: &[Node],
    placement: &[Option<Tile>],
    space: Space,
) -> Vec<Option<Tile>> {
    fn bounds(
        nodes: &[Node],
        index: usize,
        placement: &[Option<Tile>],
    ) -> Option<Tile> {
        match nodes[index] {
            Node::Repository(i) => placement[i],
            Node::Split { first, second, .. } => {
                match (
                    bounds(nodes, first, placement),
                    bounds(nodes, second, placement),
                ) {
                    (Some(a), Some(b)) => Some(Tile {
                        low: a.low.min(b.low),
                        high: a.high.max(b.high),
                    }),
                    (a, b) => a.or(b),
                }
            }
        }
    }
    fn divide(
        nodes: &[Node],
        index: usize,
        cell: Tile,
        placement: &[Option<Tile>],
        result: &mut [Option<Tile>],
    ) {
        match nodes[index] {
            Node::Repository(i) => result[i] = placement[i].map(|_| cell),
            Node::Split {
                axis,
                first,
                second,
            } => {
                let a = bounds(nodes, first, placement);
                let b = bounds(nodes, second, placement);
                match (a, b) {
                    (Some(a), Some(b)) => {
                        let cut = a.high[axis].midpoint(b.low[axis]);
                        let mut near = cell;
                        let mut far = cell;
                        near.high[axis] = cut;
                        far.low[axis] = cut;
                        divide(nodes, first, near, placement, result);
                        divide(nodes, second, far, placement, result);
                    }
                    (Some(_), None) => divide(nodes, first, cell, placement, result),
                    (None, Some(_)) => divide(nodes, second, cell, placement, result),
                    (None, None) => {}
                }
            }
        }
    }
    let mut result = vec![None; placement.len()];
    divide(nodes, 0, space.field(GUTTER_MARGIN), placement, &mut result);
    result
}

const GUTTER_MARGIN: f32 = 16.0;

#[derive(Clone, Debug, Default)]
pub(crate) struct Packing {
    gutter_scale: f32,
    growth: f32,
    nodes: Vec<Node>,
    birth_order: Arc<[usize]>,
    opening: Option<Opening>,
    reflows: Arc<[reflow::Change]>,
    labels: Arc<[String]>,
    inputs: Arc<[Vec<Input>]>,
    samples: Arc<[Sample]>,
    rotated: Vec<bool>,
    content_scales: Arc<[f32]>,
    exits: Arc<[Option<crate::model::RepositoryExit>]>,
    tiling: TilingCache,
    /// Tiles every sample from scratch; tests compare against it.
    fresh_tiling: bool,
}

#[derive(Clone, Debug)]
struct Input {
    low: Vec2,
    high: Vec2,
    outline: Box<[Vec2]>,
    height: f32,
    live: bool,
}

impl From<RepositoryRegion> for Input {
    fn from(region: RepositoryRegion) -> Self {
        Self {
            low: region.low,
            high: region.high,
            outline: region.outline.into_boxed_slice(),
            height: region.height,
            live: region.live,
        }
    }
}

pub(crate) struct Inputs {
    labels: Vec<String>,
    rows: Vec<Vec<Input>>,
    items: Vec<Vec<f32>>,
}

impl Inputs {
    pub(crate) fn new(ticks: usize) -> Self {
        Self {
            labels: Vec::new(),
            rows: Vec::with_capacity(ticks),
            items: Vec::with_capacity(ticks),
        }
    }

    pub(crate) fn push(&mut self, regions: Vec<RepositoryRegion>) {
        if self.rows.is_empty() {
            self.labels = regions.iter().map(|region| region.label.clone()).collect();
        }
        let (rows, items) = regions
            .into_iter()
            .map(|region| {
                let items = region.items;
                (Input::from(region), items)
            })
            .unzip();
        self.rows.push(rows);
        self.items.push(items);
    }
}
impl Input {
    fn shape(&self, rotated: bool) -> Cow<'_, [Vec2]> {
        if rotated {
            Cow::Owned(self.outline.iter().map(|point| point.perp()).collect())
        } else {
            Cow::Borrowed(&self.outline)
        }
    }

    fn region(&self, label: &str) -> RepositoryRegion {
        RepositoryRegion {
            label: label.to_owned(),
            low: self.low,
            high: self.high,
            outline: self.outline.to_vec(),
            height: self.height,
            live: self.live,
            items: 0.0,
            tile: None,
            gutter: None,
            rotated: false,
        }
    }
}

fn rotations(
    nodes: &[Node],
    regions: &[Vec<Input>],
    samples: &[Sample],
    space: Space,
) -> Vec<bool> {
    let mut scores = vec![[0.0; 2]; regions[0].len()];
    for tick in (0..regions.len()).step_by((regions.len() / 16).max(1)) {
        let sample = &samples[tick];
        let Some(placement) = tiles(nodes, &sample.weights, &sample.presence, space)
            .or_else(|| nearest_tiles(nodes, &sample.weights, &sample.presence, space))
        else {
            continue;
        };
        for (i, tile) in placement
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.map(|t| (i, t)))
        {
            let r = &regions[tick][i];
            let turned: Vec<_> = r.outline.iter().map(|p| p.perp()).collect();
            scores[i][0] += tile.area() * fit::best(tile, &r.outline, r.height).fill;
            scores[i][1] += tile.area() * fit::best(tile, &turned, r.height).fill;
        }
    }
    scores.into_iter().map(|s| s[1] > s[0] * 1.02).collect()
}

impl Packing {
    pub(crate) fn replay_bytes(&self) -> u128 {
        use crate::model::checkpoint_memory::{buffer_bytes, retained_fields};

        let (nodes, rotated, birth_order, opening) = retained_fields!(self, Packing {
            nodes, rotated, birth_order, opening;
            gutter_scale, growth, reflows, labels, inputs, samples, exits, content_scales,
            tiling, fresh_tiling,
        });
        let opening = opening.as_ref().map_or(0, |opening| {
            let (cuts, order) = retained_fields!(opening, Opening { cuts, order; end });
            buffer_bytes::<(f32, f32)>(cuts.capacity())
                + buffer_bytes::<usize>(order.capacity())
        });
        buffer_bytes::<Node>(nodes.capacity())
            + buffer_bytes::<bool>(rotated.capacity())
            + buffer_bytes::<usize>(birth_order.len() + 2)
            + opening
    }

    pub(crate) fn input_bytes() -> usize {
        std::mem::size_of::<Input>()
            + outline::NORMALS.len() * std::mem::size_of::<Vec2>()
            + 6 * std::mem::size_of::<f32>()
    }

    pub(crate) fn row_bytes() -> usize {
        std::mem::size_of::<Vec<Input>>()
            + std::mem::size_of::<Sample>()
            + 2 * std::mem::size_of::<Vec<f32>>()
    }

    pub(crate) fn conversion_bytes() -> usize {
        std::mem::size_of::<Sample>()
    }

    pub(crate) fn reflow_bytes(ticks: u128, repositories: u128) -> u128 {
        (1 + ticks / held::reflow_spacing() as u128)
            * (reflow::Change::retained_bytes()
                + repositories
                    * (2 * std::mem::size_of::<Node>()
                        + 4 * std::mem::size_of::<Option<Tile>>())
                        as u128)
    }

    #[cfg(test)]
    pub(crate) fn before_service(regions: &[Vec<RepositoryRegion>]) -> Self {
        Self::with_gutter(regions, 4.0).unwrap()
    }

    #[cfg(test)]
    pub(crate) fn with_gutter(
        regions: &[Vec<RepositoryRegion>],
        gutter_scale: f32,
    ) -> anyhow::Result<Self> {
        let mut inputs = Inputs::new(regions.len());
        for row in regions {
            inputs.push(row.clone());
        }
        Self::from_inputs(inputs, Arc::default(), gutter_scale, 1.0, 0)
    }

    /// Inputs, samples and tree with floats printed exactly.
    #[cfg(test)]
    pub(crate) fn exact_state(&self) -> String {
        format!(
            "{:?}{:?}{:?}{:?}{:?}",
            self.nodes, self.inputs, self.samples, self.rotated, self.reflows
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "birth, exit and weight timing share one pass"
    )]
    pub(crate) fn from_inputs(
        inputs: Inputs,
        exits: Arc<[Option<crate::model::RepositoryExit>]>,
        gutter_scale: f32,
        stability: f32,
        threads: usize,
    ) -> anyhow::Result<Self> {
        let Inputs {
            labels,
            rows: regions,
            items,
        } = inputs;
        let Some(first) = regions.first().filter(|r| !r.is_empty()) else {
            return Ok(Self::default());
        };
        let births: Vec<_> = (0..first.len())
            .map(|i| regions.iter().position(|r| r[i].live).unwrap_or(usize::MAX))
            .collect();
        let mut order: Vec<_> = (0..first.len()).collect();
        order.sort_by_key(|&i| (births[i], i));
        let peak = items
            .iter()
            .map(|row| row.iter().sum::<f32>())
            .fold(1.0, f32::max);
        let area = peak * 100.0 + crate::model::count(first.len()) * 96.0_f32.powi(2);
        let growth = (area / Tile::viewport().pixels().element_product())
            .sqrt()
            .max(1.0);
        let history: Vec<f32> = births
            .iter()
            .enumerate()
            .map(|(repo, &birth)| {
                let end = exits.get(repo).and_then(|&exit| exit).map_or(
                    items.len(),
                    |exit| {
                        usize::try_from(exit.start)
                            .unwrap_or(usize::MAX)
                            .min(items.len())
                    },
                );
                items
                    .get(birth..end)
                    .unwrap_or_default()
                    .iter()
                    .map(|row| row[repo].max(1.0))
                    .sum()
            })
            .collect();
        let (nodes, space) = plan_tree(
            &regions,
            &items,
            &births,
            &order,
            Space::new(gutter_scale, growth),
            threads,
            &history,
        )?;
        let span = usize::try_from(BIRTH_TICKS).expect("bounded birth");
        let windows: Vec<_> = births
            .iter()
            .map(|&birth| {
                if birth == usize::MAX {
                    (birth, birth)
                } else {
                    let end = birth.saturating_sub(1);
                    (end.saturating_sub(span), end)
                }
            })
            .collect();
        let mut held = held::Weights::new(first.len());
        let mut reflows = Vec::<reflow::Change>::new();
        let mut was_stable = false;
        let mut retry_allocation = true;
        let mut next_density_probe = 0;
        let mut samples: Vec<_> = regions
            .iter()
            .enumerate()
            .map(|(tick, row)| {
                let mut presence: Vec<_> = row
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        let (start, end) = windows[i];
                        if tick >= end {
                            f32::from(births[i] != usize::MAX)
                        } else if tick <= start {
                            0.0
                        } else {
                            held::birth_ease(tick - start, end - start)
                        }
                    })
                    .collect();
                if presence.iter().all(|&p| p == 0.0) {
                    presence[order[0]] = 1.0;
                }
                let now = u64::try_from(tick).expect("bounded packing tick");
                let shown: Vec<_> = presence
                    .iter()
                    .enumerate()
                    .map(|(i, &p)| {
                        if exits
                            .get(i)
                            .and_then(|exit| *exit)
                            .is_some_and(|exit| now >= exit.start)
                        {
                            0.0
                        } else {
                            p
                        }
                    })
                    .collect();
                let finish = tick + held::reflow_ticks();
                let end = u64::try_from(finish).expect("bounded reflow");
                let lifecycle_window = shown
                    .iter()
                    .enumerate()
                    .map(|(i, &p)| {
                        reflow_window(
                            tick,
                            p,
                            births[i],
                            windows[i].0,
                            exits.get(i).copied().flatten(),
                        )
                    })
                    .min()
                    .unwrap_or(0);
                let stable = lifecycle_window >= held::reflow_ticks();
                let available = lifecycle_window.min(regions.len() - 1 - tick);
                let policy = reflow::Policy {
                    stability,
                    available,
                    forecast: &items[tick + available.min(reflow::MAX_MOTION_TICKS)],
                };
                let future = held::target_tick(tick).min(items.len() - 1);
                let targets: Vec<_> = births
                    .iter()
                    .enumerate()
                    .map(|(i, &birth)| items[tick.max(birth.min(items.len() - 1))][i])
                    .collect();
                let clear_of_archives = exits
                    .iter()
                    .flatten()
                    .all(|exit| now >= exit.end || end < exit.start);
                let weights = held.step_with_births(
                    tick,
                    &targets,
                    &items[future],
                    &shown,
                    stable,
                    clear_of_archives,
                );
                retry_allocation |= stable && !was_stable;
                was_stable = stable;
                let tree = reflows
                    .last()
                    .map_or(nodes.as_slice(), |event| &event.nodes);
                if stable
                    && finish < regions.len()
                    && held.ready(tick)
                    && tick >= next_density_probe
                {
                    next_density_probe = finish;
                    if reflow::can_reweight(
                        tree,
                        &weights,
                        &targets,
                        &items[future],
                        &shown,
                        space,
                    ) {
                        held.retarget(tick, &items[future]);
                    }
                }
                if let Some(target) = held.target_at_start(tick) {
                    let change = (stable && finish < regions.len())
                        .then(|| {
                            reflow::plan(
                                tick, tree, &weights, target, &shown, space, policy,
                            )
                        })
                        .flatten()
                        .unwrap_or_else(|| reflow::Change::weights(tick, tree));
                    held.reserve_reflow(tick, change.duration());
                    reflows.push(change);
                    retry_allocation = true;
                } else if stable
                    && finish < regions.len()
                    && retry_allocation
                    && held.ready(tick)
                {
                    let change = reflow::plan(
                        tick, tree, &weights, &weights, &shown, space, policy,
                    )
                    .filter(|change| change.nodes != tree);
                    retry_allocation = change.is_some();
                    if let Some(change) = change {
                        held.reserve_reflow(tick, change.duration());
                        reflows.push(change);
                    }
                }
                for (presence, exit) in presence.iter_mut().zip(exits.iter()) {
                    if let Some(exit) = exit {
                        *presence *= exit_presence(now, *exit);
                    }
                }
                Sample { weights, presence }
            })
            .collect();
        for (repository, &(start, _)) in windows.iter().enumerate() {
            if let Some(sample) = samples.get(start.saturating_add(1)) {
                let weight = sample.weights[repository];
                for sample in &mut samples[..=start] {
                    sample.weights[repository] = weight;
                }
            }
        }
        let opening =
            Opening::new(&nodes, &samples, &births, &order, &windows, &exits, space)
                .filter(|opening| {
                    reflows
                        .first()
                        .is_none_or(|change| change.tick > opening.end)
                });
        let rotated = rotations(&nodes, &regions, &samples, space);
        drop(items);
        let mut packing = Self {
            gutter_scale,
            growth: space.growth,
            nodes,
            birth_order: repository_ranks(&order).into(),
            opening,
            reflows: reflows.into(),
            labels: labels.into(),
            inputs: regions.into(),
            samples: samples.into(),
            rotated,
            content_scales: Arc::default(),
            exits,
            tiling: TilingCache::default(),
            fresh_tiling: false,
        };
        packing.content_scales = packing.plan_content_scales();
        Ok(packing)
    }

    pub(crate) fn sample(
        &self,
        tick: u64,
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        self.sample_with_presence(tick, None)
    }

    pub(crate) fn sample_with_presence(
        &self,
        tick: u64,
        presence: Option<&[f32]>,
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        self.sample_filtered(tick, presence, |_| true)
    }

    pub(crate) fn sample_repositories(
        &self,
        tick: u64,
        repositories: &[usize],
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        self.sample_filtered(tick, None, |index| repositories.contains(&index))
    }

    fn sample_filtered(
        &self,
        tick: u64,
        presence: Option<&[f32]>,
        selected: impl Fn(usize) -> bool,
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        self.sample_rows(tick, tick, &[], presence, selected)
    }

    pub(crate) fn sample_split(
        &self,
        tiles: u64,
        content: u64,
        births: &[(usize, u64)],
        repositories: Option<&[usize]>,
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        self.sample_rows(tiles, content, births, None, |index| {
            repositories.is_none_or(|selected| selected.contains(&index))
        })
    }

    pub(crate) fn motion_intervals(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.exits
            .iter()
            .flatten()
            .map(|exit| (exit.start, exit.end))
            .chain(self.reflows.iter().map(|change| {
                let start = u64::try_from(change.tick).expect("bounded reflow");
                let duration =
                    u64::try_from(change.duration()).expect("bounded reflow");
                (start, start + duration)
            }))
    }

    pub(crate) fn reflowing(&self, tick: u64) -> bool {
        let tick = usize::try_from(tick).expect("bounded reflow tick");
        self.reflows[..self.reflows.partition_point(|change| change.tick <= tick)]
            .last()
            .is_some_and(|change| tick - change.tick < change.duration())
    }

    pub(crate) fn opening_end(&self) -> Option<u64> {
        self.opening
            .as_ref()
            .map(|opening| u64::try_from(opening.end).expect("bounded opening ticks"))
    }

    pub(crate) fn expanding_neighbours(
        &self,
        repository: usize,
        tick: usize,
    ) -> Vec<bool> {
        if self
            .opening
            .as_ref()
            .is_some_and(|opening| tick <= opening.end)
        {
            return vec![false; self.labels.len()];
        }
        let sample = &self.samples[tick];
        let tree = self.reflows
            [..self.reflows.partition_point(|reflow| reflow.tick <= tick)]
            .last()
            .map_or((0, self.nodes.as_slice()), |reflow| {
                (reflow.tick, reflow.nodes.as_slice())
            });
        let rank = self.birth_order[repository];
        let mut visible: Vec<_> = self
            .birth_order
            .iter()
            .zip(&sample.presence)
            .map(|(&order, &presence)| f32::from(order < rank && presence > 0.0))
            .collect();
        let before = self.allocated_tiles(tree, &sample.weights, &visible, None);
        visible[repository] = 1.0;
        let after = self.allocated_tiles(tree, &sample.weights, &visible, None);
        before
            .into_iter()
            .zip(after)
            .map(|(before, after)| {
                before.zip(after).is_some_and(|(before, after)| {
                    let start = before.high - before.low;
                    let end = after.high - after.low;
                    let change = end - start;
                    [start, end]
                        .iter()
                        .any(|size| change.x * size.y + change.y * size.x > 1e-6)
                })
            })
            .collect()
    }

    fn sample_rows(
        &self,
        tiles: u64,
        tick: u64,
        births: &[(usize, u64)],
        presence: Option<&[f32]>,
        selected: impl Fn(usize) -> bool,
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        let tick = usize::try_from(tick)
            .unwrap_or(usize::MAX)
            .min(self.samples.len().checked_sub(1)?);
        let tiles = usize::try_from(tiles)
            .unwrap_or(usize::MAX)
            .min(self.samples.len() - 1);
        let sample = &self.samples[tiles];
        let placement = self.fit_rows(tiles, tick, births, presence, None);
        let mut regions: Vec<_> = self.inputs[tick]
            .iter()
            .zip(self.labels.iter())
            .map(|(input, label)| input.region(label))
            .collect();
        let mut origins = Vec::with_capacity(regions.len());
        let mut scales = Vec::with_capacity(regions.len());
        for (i, r) in regions.iter_mut().enumerate() {
            let turn = |p: Vec2| if self.rotated[i] { p.perp() } else { p };
            let tile = placement[i].tile.filter(|_| selected(i));
            let origin = if tile.is_some() {
                placement[i].origin
            } else {
                Vec2::ZERO
            };
            let scale = if tile.is_some() {
                placement[i].scale
            } else {
                0.0
            };
            let (low, high) = (turn(r.low), turn(r.high));
            r.low = low.min(high) * scale + origin;
            r.high = low.max(high) * scale + origin;
            r.height *= scale;
            r.live &= tile.is_some();
            r.tile = tile;
            r.gutter = placement[i].gutter.filter(|_| selected(i));
            r.items = sample.weights[i]
                * (births
                    .iter()
                    .find(|&&(repository, _)| repository == i)
                    .map_or(sample.presence[i], |&(_, elapsed)| {
                        birth_presence(elapsed)
                    })
                    * presence
                        .and_then(|v| v.get(i))
                        .copied()
                        .unwrap_or(1.0)
                        .clamp(0.0, 1.0));
            r.rotated = self.rotated[i];
            for point in &mut r.outline {
                *point = turn(*point) * scale + origin;
            }
            origins.push(origin);
            scales.push(scale);
        }
        Some((regions, origins, scales))
    }

    /// Reuse the polygons while the tree and presence repeat.
    fn tiles_in(
        &self,
        tiling: &mut Tiling,
        tree: (usize, &[Node]),
        weights: &[f32],
        presence: &[f32],
    ) -> Option<Vec<Option<Tile>>> {
        let (tree_tick, nodes) = tree;
        let space = self.space();
        tiling.samples += 1;
        if tiling.tree_tick == tree_tick
            && tiling.states.len() == nodes.len()
            && tiling
                .presence
                .iter()
                .copied()
                .eq(presence.iter().map(|p| p.to_bits()))
        {
            tiling.reuses += 1;
            reweigh(nodes, 0, weights, presence, &mut tiling.states);
        } else {
            tiling.tree_tick = tree_tick;
            tiling.presence = presence.iter().map(|p| p.to_bits()).collect();
            tiling.states = vec![Feasible::default(); nodes.len()];
            feasibility(nodes, 0, weights, presence, space, &mut tiling.states);
        }
        allocated(nodes, weights.len(), &tiling.states, space)
    }

    /// Tiles with every easing repository moved towards full presence by the least
    /// bisected fraction that fits, since fractional slivers can conflict where the
    /// planned full-presence arrangement does not.
    fn firmer_tiles(
        &self,
        nodes: &[Node],
        weights: &[f32],
        presence: &[f32],
    ) -> Option<Vec<Option<Tile>>> {
        let at = |firmness: f32| {
            let firm: Vec<_> = presence
                .iter()
                .map(|&p| if p > 0.0 { p.lerp(1.0, firmness) } else { 0.0 })
                .collect();
            tiles(nodes, weights, &firm, self.space())
        };
        let mut best = at(1.0)?;
        let (mut loose, mut firm) = (0.0_f32, 1.0_f32);
        for _ in 0..FIRMING_STEPS {
            let middle = loose.midpoint(firm);
            if let Some(placement) = at(middle) {
                best = placement;
                firm = middle;
            } else {
                loose = middle;
            }
        }
        Some(best)
    }

    fn space(&self) -> Space {
        Space::new(self.gutter_scale, self.overview_growth())
    }

    fn allocated_tiles(
        &self,
        tree: (usize, &[Node]),
        weights: &[f32],
        presence: &[f32],
        tiling: Option<&mut Tiling>,
    ) -> Vec<Option<Tile>> {
        let (.., nodes) = tree;
        let space = self.space();
        let result = if self.fresh_tiling {
            tiles(nodes, weights, presence, space)
        } else if let Some(tiling) = tiling {
            self.tiles_in(tiling, tree, weights, presence)
        } else {
            self.tiles_in(&mut self.tiling.lock(), tree, weights, presence)
        };
        result
            .or_else(|| nearest_tiles(nodes, weights, presence, space))
            .or_else(|| self.firmer_tiles(nodes, weights, presence))
            .unwrap_or_else(|| {
                let mut allocated = {
                    let full_presence = vec![1.0; presence.len()];
                    tiles(nodes, weights, &full_presence, space)
                        .or_else(|| tiles(&self.nodes, weights, &full_presence, space))
                }
                .unwrap_or_else(|| vec![None; presence.len()]);
                shrink_tiles(&mut allocated, presence);
                allocated
            })
    }

    fn transition_tiles(
        &self,
        tree: (usize, &[Node]),
        weights: &[f32],
        presence: &[f32],
        mut transitions: Vec<(f32, usize)>,
        tiling: Option<&mut Tiling>,
        transition: Transition<'_>,
    ) -> Vec<Option<Tile>> {
        let place = |presence: &[f32], tiling| {
            if let Transition::Birth(Some(opening)) = transition {
                opening.tiles(tree.1, presence, self.space())
            } else {
                self.allocated_tiles(tree, weights, presence, tiling)
            }
        };
        if transitions.is_empty() {
            return place(presence, tiling);
        }
        transitions.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let mut layer = presence.to_vec();
        for &(p, i) in &transitions {
            layer[i] /= p;
        }
        let field = self.space().field(framing::MARGIN);
        let center = field.low.midpoint(field.high);
        let mut result = vec![
            Tile {
                low: center,
                high: center
            };
            weights.len()
        ];
        let order = matches!(transition, Transition::Birth(_))
            .then(|| birth_order(tree.1, &self.birth_order));
        let mut previous = 0.0;
        // Blending nested layouts gives a split zero influence when a tile has zero presence.
        for (level, i) in transitions.into_iter().chain([(1.0, weights.len())]) {
            let fraction = level - previous;
            if fraction > 0.0 && layer.iter().any(|&p| p > 0.0) {
                let mut placed = place(&layer, None);
                collapse_missing(tree.1, 0, &mut placed, order.as_deref());
                for (sum, tile) in result
                    .iter_mut()
                    .zip(placed)
                    .filter_map(|(sum, tile)| Some((sum, tile?)))
                {
                    sum.low += (tile.low - center) * fraction;
                    sum.high += (tile.high - center) * fraction;
                }
            }
            previous = level;
            if let Some(p) = layer.get_mut(i) {
                *p = 0.0;
            }
        }
        result
            .into_iter()
            .zip(presence)
            .map(|(tile, &p)| {
                (p > 0.0).then(|| Tile {
                    low: tile.low.clamp(field.low, field.high),
                    high: tile.high.clamp(field.low, field.high),
                })
            })
            .collect()
    }

    pub(crate) fn overview_growth(&self) -> f32 {
        self.growth.max(1.0)
    }

    pub(crate) fn field(&self) -> Tile {
        self.space().field(0.0)
    }

    #[cfg(test)]
    pub(crate) fn gutter_field(&self) -> Tile {
        self.space().field(GUTTER_MARGIN)
    }

    /// Samples tiled and samples that reused the previous polygons, since the last call.
    pub(crate) fn take_tiling_counts(&self) -> (usize, usize) {
        let mut tiling = self.tiling.lock();
        (
            std::mem::take(&mut tiling.samples),
            std::mem::take(&mut tiling.reuses),
        )
    }

    pub(crate) fn tile_fresh(&mut self, fresh: bool) {
        self.fresh_tiling = fresh;
        if fresh {
            self.tiling = TilingCache::default();
        }
    }

    fn layout_at(
        &self,
        tile_tick: usize,
        births: &[(usize, u64)],
        presence: Option<&[f32]>,
        tiling: Option<&mut Tiling>,
    ) -> reflow::Layout {
        let opening = self.opening.as_ref().filter(|opening| {
            presence.is_none_or(|mask| mask.iter().all(|&p| p >= 1.0))
                && tile_tick <= opening.end
        });
        let sample = &self.samples[tile_tick];
        let weights = &sample.weights;
        let mut visibility = sample.presence.clone();
        for &(repository, elapsed) in births {
            visibility[repository] = birth_presence(elapsed);
        }
        let presence: Vec<_> = visibility
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                p * presence
                    .and_then(|v| v.get(i))
                    .copied()
                    .unwrap_or(1.0)
                    .clamp(0.0, 1.0)
            })
            .collect();
        let changes =
            &self.reflows[..self.reflows.partition_point(|r| r.tick <= tile_tick)];
        let reflow = changes.last();
        reflow
            .filter(|change| change.matches_presence(&presence))
            .and_then(|change| change.sample(tile_tick))
            .unwrap_or_else(|| {
                // A pending birth may override the history row while the tile clock waits.
                let current =
                    if reflow.is_some_and(|change| change.in_transition(tile_tick)) {
                        changes[..changes.len() - 1].last()
                    } else {
                        reflow
                    };
                let tree = current.map_or((0, self.nodes.as_slice()), |c| {
                    (c.tick, c.nodes.as_slice())
                });
                let now = u64::try_from(tile_tick).expect("bounded packing tick");
                let mut split_edges = true;
                let transitions = visibility
                    .iter()
                    .enumerate()
                    .filter_map(|(i, &p)| {
                        let exiting = self
                            .exits
                            .get(i)
                            .and_then(|exit| *exit)
                            .is_some_and(|exit| now >= exit.start);
                        let partial = p > 0.0 && p < 1.0 && presence[i] > 0.0;
                        split_edges &= !partial || !exiting;
                        partial.then_some((p, i))
                    })
                    .collect();
                let allocated = self.transition_tiles(
                    tree,
                    weights,
                    &presence,
                    transitions,
                    tiling,
                    if split_edges {
                        Transition::Birth(opening)
                    } else {
                        Transition::Exit
                    },
                );
                reflow::Layout::new(tree.1, allocated, self.space())
            })
    }

    fn fit_rows(
        &self,
        tile_tick: usize,
        tick: usize,
        births: &[(usize, u64)],
        presence: Option<&[f32]>,
        tiling: Option<&mut Tiling>,
    ) -> Vec<Placement> {
        let layout = self.layout_at(tile_tick, births, presence, tiling);
        let repositories = self.rotated.len();
        self.inputs[tick]
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let shape = r.shape(self.rotated[i]);
                let tile = layout.tiles[i];
                let (origin, scale) = tile.map_or((Vec2::ZERO, 0.0), |tile| {
                    let mut scale = self.content_scales[tile_tick * repositories + i];
                    if tile_tick != tick
                        || !births.is_empty()
                        || presence.is_some_and(|mask| mask.iter().any(|&p| p < 1.0))
                    {
                        scale = scale.min(self.fit_scale(tile, &shape, r.height));
                    }
                    let (low, high) = outline::bounds(&shape);
                    let center = (low + high) * 0.5;
                    let origin = framing::field_to_world(tile.low.midpoint(tile.high))
                        - center * scale;
                    (origin, scale)
                });
                Placement {
                    tile,
                    gutter: layout.gutters[i],
                    origin,
                    scale,
                }
            })
            .collect()
    }

    fn fit_scale(&self, tile: Tile, shape: &[Vec2], height: f32) -> f32 {
        fit::anchored_scale(
            tile.usable(),
            shape,
            height / self.overview_growth(),
            Vec2::splat(0.5),
        )
        .1
    }

    fn plan_content_scales(&self) -> Arc<[f32]> {
        let width = self.rotated.len();
        let count = width * self.inputs.len();
        let mut scales = Vec::with_capacity(count);
        let mut areas = Vec::with_capacity(count);
        let mut tiling = Tiling::default();
        for (tick, row) in self.inputs.iter().enumerate() {
            let layout = self.layout_at(tick, &[], None, Some(&mut tiling));
            for (i, (input, tile)) in row.iter().zip(layout.tiles).enumerate() {
                let live = tile.filter(|_| {
                    input.live
                        && self
                            .exits
                            .get(i)
                            .and_then(|exit| *exit)
                            .is_none_or(|exit| (tick as u64) < exit.start)
                });
                areas.push(live.map_or(0.0, Tile::area));
                scales.push(tile.map_or(1.0, |tile| {
                    self.fit_scale(tile, &input.shape(self.rotated[i]), input.height)
                        .min(32.0)
                }));
            }
        }
        // Preview a tighter fit early enough to remain below it without reversing tile motion.
        let permitted = |from, to, scale: f32| {
            let (a, b) = (areas[from], areas[to]);
            (a > 0.0 && b > 0.0).then(|| {
                if b > a + 1e-6 {
                    (scale * 0.005).clamp(0.002, 0.02)
                } else {
                    0.0
                }
            })
        };
        for index in (0..count - width).rev() {
            if let Some(step) = permitted(index + width, index, scales[index + width]) {
                scales[index] = scales[index].min(scales[index + width] + step);
            }
        }
        for index in width..count {
            if let Some(step) = permitted(index - width, index, scales[index - width]) {
                scales[index] = scales[index].min(scales[index - width] + step);
            }
        }
        scales.into()
    }
}

fn birth_weights(
    regions: &[Vec<Input>],
    items: &[Vec<f32>],
    born: &[bool],
    begin: usize,
    end: usize,
) -> (Vec<f32>, Vec<f32>) {
    let mut weights = vec![0.0; born.len()];
    let mut aspects = vec![1.0; born.len()];
    for (j, _) in born.iter().enumerate().filter(|(_, live)| **live) {
        weights[j] = items[begin..end]
            .iter()
            .map(|row| row[j].max(1.0))
            .sum::<f32>()
            / crate::model::count_u32(
                u32::try_from(end - begin).expect("bounded timeline"),
            );
        let size = regions[end - 1][j].high - regions[end - 1][j].low;
        aspects[j] = (size.x / size.y.max(0.01)).max(0.01);
    }
    (weights, aspects)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capacity_fixture() -> Vec<RepositoryRegion> {
        (0..128)
            .map(|i| RepositoryRegion {
                label: format!("Repository {i}"),
                low: -Vec2::ONE,
                high: Vec2::ONE,
                live: true,
                items: 1.0,
                tile: None,
                gutter: None,
                outline: outline::rectangle(-Vec2::ONE, Vec2::ONE),
                height: 0.105,
                rotated: false,
            })
            .collect()
    }

    #[test]
    fn the_field_grows_to_pack_every_tile_without_overlap() {
        let packing = Packing::with_gutter(&[capacity_fixture()], 4.0).unwrap();
        let field = packing.space().field(framing::MARGIN);
        assert!(packing.overview_growth() > 1.0);
        let (regions, _, _) = packing.sample(0).unwrap();
        let tiles: Vec<_> = regions.iter().map(|r| r.tile.unwrap()).collect();
        for (index, tile) in tiles.iter().enumerate() {
            assert!(tile.low.cmpge(field.low - Vec2::splat(1e-5)).all());
            assert!(tile.high.cmple(field.high + Vec2::splat(1e-5)).all());
            for other in &tiles[index + 1..] {
                let overlap = tile.high.min(other.high) - tile.low.max(other.low);
                assert!(overlap.min_element() <= 1e-5, "{tile:?} {other:?}");
            }
        }
    }

    #[test]
    fn tile_areas_follow_item_counts() {
        let counts = [25.0, 75.0];
        let row: Vec<_> = counts
            .iter()
            .enumerate()
            .map(|(i, &items)| RepositoryRegion {
                label: format!("Repository {i}"),
                items,
                ..capacity_fixture()[0].clone()
            })
            .collect();
        let packing = Packing::before_service(&vec![row; 60]);
        let (regions, _, _) = packing.sample(59).unwrap();
        let areas: Vec<_> = regions.iter().map(|r| r.tile.unwrap().area()).collect();
        let total: f32 = areas.iter().sum();
        let total_items: f32 = counts.iter().sum();
        for (area, items) in areas.iter().zip(counts) {
            let share = area / total;
            assert!((share - items / total_items).abs() < 0.001, "{areas:?}");
        }
    }

    #[test]
    fn content_uses_the_full_fit_when_a_neighbour_exits() {
        let mut row = capacity_fixture();
        row.truncate(2);
        let extent = Vec2::new(110.0, 50.0);
        row[1].low = -extent;
        row[1].high = extent;
        row[1].outline = outline::rectangle(-extent, extent);
        let mut inputs = Inputs::new(301);
        for _ in 0..301 {
            inputs.push(row.clone());
        }
        let exits = crate::model::exits_from_starts(&[Some(60), None]);
        let packing = Packing::from_inputs(inputs, exits.into(), 4.0, 1.0, 1).unwrap();
        for tick in [60, 180, 300] {
            let (regions, _, scales) = packing.sample(tick).unwrap();
            let input = &packing.inputs[usize::try_from(tick).unwrap()][1];
            let turn = |point: Vec2| {
                if packing.rotated[1] {
                    point.perp()
                } else {
                    point
                }
            };
            let shape: Vec<_> = input.outline.iter().copied().map(turn).collect();
            let tile = regions[1].tile.unwrap();
            let fit = fit::anchored(
                tile.usable(),
                &shape,
                input.height / packing.overview_growth(),
                Vec2::splat(0.5),
            );
            assert!((scales[1] - fit.scale).abs() < 1e-6, "tick {tick}");
        }
    }

    #[test]
    fn kept_polygons_tile_like_fresh_ones_across_seeks_and_presence_changes() {
        let kept = Packing::before_service(&fixture());
        let mut fresh = kept.clone();
        fresh.tile_fresh(true);
        kept.take_tiling_counts();
        let hidden: [f32; 30] = std::array::from_fn(|i| [0.5, 1.0, 0.25][i % 3]);
        let samples = [
            (1300, None),
            (1301, None),
            (1420, None),
            (700, None),
            (701, None),
            (701, Some(&hidden[..])),
            (702, Some(&hidden[..])),
            (1598, None),
            (1599, None),
            (1300, None),
        ];
        for (tick, presence) in samples {
            assert_eq!(
                format!("{:?}", kept.sample_with_presence(tick, presence)),
                format!("{:?}", fresh.sample_with_presence(tick, presence)),
                "tick {tick}"
            );
        }
        let (tiled, reused) = kept.take_tiling_counts();
        assert!(
            tiled >= samples.len(),
            "{tiled} allocations for {} samples",
            samples.len()
        );
        assert!(0 < reused && reused < tiled, "{reused} of {tiled}");
        assert_eq!(fresh.take_tiling_counts(), (0, 0));

        let weights: Vec<f32> = (0..30_u8).map(|i| 1.0 + f32::from(i)).collect();
        let states = |weights: &[f32]| {
            let mut states = vec![Feasible::default(); kept.nodes.len()];
            feasibility(&kept.nodes, 0, weights, &hidden, kept.space(), &mut states);
            states
        };
        let mut reweighed = states(&[1.0; 30]);
        reweigh(&kept.nodes, 0, &weights, &hidden, &mut reweighed);
        assert!(
            reweighed
                .iter()
                .zip(states(&weights))
                .all(|(a, b)| a.weight.to_bits() == b.weight.to_bits())
        );
    }

    #[test]
    fn cached_fits_match_fresh_fits_after_seek_and_presence_changes() {
        let packing = Packing::before_service(&fixture());
        for tick in [0, 60, 700, 1599, 700, 60, u64::MAX] {
            let (cached, origins, scales) = packing.sample(tick).unwrap();
            let (fresh, fresh_origins, fresh_scales) =
                packing.sample_with_presence(tick, Some(&[])).unwrap();
            assert_eq!(origins, fresh_origins);
            assert_eq!(scales, fresh_scales);
            for (a, b) in cached.iter().zip(fresh) {
                assert_eq!(a.tile, b.tile);
                assert_eq!((a.low, a.high, a.items), (b.low, b.high, b.items));
            }
            let (hidden, _, _) = packing
                .sample_with_presence(tick, Some(&[0.0; 30]))
                .unwrap();
            assert!(hidden.iter().all(|r| r.tile.is_none()));
            let (selected, selected_origins, selected_scales) =
                packing.sample_repositories(tick, &[0]).unwrap();
            assert_eq!(selected[0].tile, cached[0].tile);
            assert_eq!(selected_origins[0], origins[0]);
            assert_eq!(selected_scales[0], scales[0]);
            assert!(selected[1..].iter().all(|r| r.tile.is_none()));
        }
    }

    #[test]
    fn births_finish_by_service_including_a_short_lead_in() {
        let regions = fixture();
        let packing = Packing::before_service(&regions);
        for repository in 0..30 {
            let birth = regions.iter().position(|r| r[repository].live).unwrap();
            assert!((packing.samples[birth].presence[repository] - 1.0).abs() < 1e-6);
            assert!(
                packing.sample(u64::try_from(birth).unwrap()).unwrap().0[repository]
                    .tile
                    .is_some()
            );
        }
        assert!(packing.samples[1].presence[1] > 0.0);
    }

    fn partition(
        nodes: &[Node],
        index: usize,
        tiles: &[Option<Tile>],
    ) -> (Option<Tile>, f32) {
        match nodes[index] {
            Node::Repository(i) => (tiles[i], 0.0),
            Node::Split {
                axis,
                first,
                second,
            } => {
                let (a, ga) = partition(nodes, first, tiles);
                let (b, gb) = partition(nodes, second, tiles);
                match (a, b) {
                    (Some(a), Some(b)) => {
                        assert!(a.high[axis] <= b.low[axis] + 1e-5);
                        assert!((a.low[1 - axis] - b.low[1 - axis]).abs() < 1e-5);
                        assert!((a.high[1 - axis] - b.high[1 - axis]).abs() < 1e-5);
                        let gap = (b.low[axis] - a.high[axis])
                            * (a.high[1 - axis] - a.low[1 - axis])
                            * 0.25;
                        (
                            Some(Tile {
                                low: a.low.min(b.low),
                                high: a.high.max(b.high),
                            }),
                            ga + gb + gap,
                        )
                    }
                    (a, b) => (a.or(b), ga + gb),
                }
            }
        }
    }

    fn fixture() -> Vec<Vec<RepositoryRegion>> {
        (0..1600_u16)
            .map(|tick| {
                (0..30_u16)
                    .map(|i| {
                        let size =
                            Vec2::new(4.0 + f32::from(i % 7), 5.0 + f32::from(i % 5));
                        RepositoryRegion {
                            label: format!("Repository {i}"),
                            low: -size,
                            high: size,
                            live: tick >= i * 40,
                            items: f32::from(1 + (i * 37 + tick / 30) % 200),
                            tile: None,
                            gutter: None,
                            outline: outline::rectangle(-size, size),
                            height: 0.105,
                            rotated: false,
                        }
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn configurable_gutters_partition_five_and_thirty_repository_views() {
        for count in [5, 30] {
            let rows: Vec<_> = fixture()
                .into_iter()
                .map(|mut row| {
                    row.truncate(count);
                    row
                })
                .collect();
            let shares: Vec<_> = [0.0, 4.0, 20.0]
                .into_iter()
                .map(|scale| {
                    let packing = Packing::with_gutter(&rows, scale).unwrap();
                    let row = packing
                        .sample(u64::try_from(rows.len() - 1).unwrap())
                        .unwrap()
                        .0;
                    let area = row
                        .iter()
                        .filter_map(|r| r.tile)
                        .map(Tile::area)
                        .sum::<f32>();
                    assert_eq!(row.iter().filter(|r| r.tile.is_some()).count(), count);
                    assert!(row.iter().filter_map(|r| r.gutter).all(|t| t.low.y
                        * framing::VIEWPORT.y
                        * 0.5
                        - 14.0
                        >= caption::BOTTOM));

                    Tile::viewport().area() - area
                })
                .collect();
            assert!(shares[0].abs() < 0.00001);
            assert!(shares[1] > shares[0]);
            assert!(shares[2] > shares[1]);
            eprintln!(
                "GUTTER_SHARE repositories={count} before={} after={} viewport={}",
                shares[0],
                shares[1],
                Tile::viewport().area()
            );
        }
    }

    #[test]
    fn birth_reaches_full_extent_before_content_arrives() {
        let packing = two_tiles_with_birth_at(180);
        let before = packing.sample(179).unwrap().0;
        let birth = packing.sample(180).unwrap().0;
        let after = packing.sample(181).unwrap().0;
        assert!(!before[1].live);
        assert!(birth[1].live);
        assert!(before[1].tile.is_some());
        assert_eq!(birth[1].tile, after[1].tile);
    }

    #[test]
    fn drift_catches_up_during_successive_births() {
        let mut initial = capacity_fixture();
        initial.truncate(4);
        let rows: Vec<_> = (0..460)
            .map(|tick| {
                let mut row = initial.clone();
                row[0].items = if tick < 150 { 20.0 } else { 200.0 };
                row[1].items = 20.0;
                row[2].live = tick >= 230;
                row[3].live = tick >= 310;
                row
            })
            .collect();
        let packing = Packing::before_service(&rows);
        let (start, _) = packing.motion_intervals().next().unwrap();
        assert!(start <= 270, "drift waited until tick {start}");
    }

    #[test]
    fn content_scale_follows_birth_and_archive_without_jumps() {
        let mut initial = capacity_fixture();
        initial.truncate(2);
        let rows: Vec<_> = (0..520)
            .map(|tick| {
                let mut row = initial.clone();
                row[0].items = 20.0;
                row[1].items = 20.0;
                row[1].live = tick >= 220;
                let size = if tick < 110 {
                    Vec2::new(2.0, 2.0)
                } else if tick < 205 {
                    Vec2::new(5.0, 3.0)
                } else {
                    Vec2::new(4.0, 15.0)
                };
                row[0].outline = outline::rectangle(-size, size);
                row[0].low = -size;
                row[0].high = size;
                row
            })
            .collect();
        let exits = [
            None,
            Some(crate::model::RepositoryExit {
                start: 280,
                end: 520,
            }),
        ];
        let mut inputs = Inputs::new(rows.len());
        for row in rows {
            inputs.push(row);
        }
        let packing = Packing::from_inputs(inputs, exits.into(), 0.0, 1.0, 1).unwrap();
        let mut previous: Option<(f32, f32)> = None;
        for tick in 0..520 {
            let (regions, _, scales) = packing.sample(tick).unwrap();
            let area = regions[0].tile.unwrap().area();
            let scale = scales[0];
            if let Some((prior_area, prior_scale)) = previous {
                let change: f32 = scale - prior_scale;
                assert!(change.abs() <= 0.025, "scale jump at {tick}: {change}");
                if (area - prior_area).abs() <= 1e-6 {
                    assert_eq!(
                        scale, prior_scale,
                        "scale changed on a still tile at {tick}"
                    );
                }
                assert!(
                    (area - prior_area) * change >= -1e-6,
                    "scale reversed at {tick}: {prior_area} to {area}, {prior_scale} to {scale}",
                );
            }
            previous = Some((area, scale));
        }
    }

    #[test]
    fn birth_shares_the_gutter_as_both_tiles_move() {
        let packing = two_tiles_with_birth_at(180);
        let initial = packing.sample(0).unwrap().0[0].tile.unwrap();
        for tick in 1..=180 {
            let (regions, _, _) = packing.sample(tick).unwrap();
            let tiles: Vec<_> = regions.iter().map(|region| region.tile).collect();
            let (field, gutters) = partition(&packing.nodes, 0, &tiles);
            let filled = tiles.iter().flatten().map(|tile| tile.area()).sum::<f32>();
            let tolerance = 2e-6;
            let field = field.unwrap();
            assert!(field.low.abs_diff_eq(initial.low, tolerance));
            assert!(field.high.abs_diff_eq(initial.high, tolerance));
            assert!((filled + gutters - initial.area()).abs() < tolerance);
        }
    }

    #[test]
    fn birth_edges_move_at_constant_speed() {
        let packing = two_tiles_with_birth_at(600);
        let edge = |tick| {
            let tile = packing.sample(tick).unwrap().0[0].tile.unwrap();
            [tile.low, tile.high]
        };
        let begin = 599 - BIRTH_TICKS;
        let ramp = BIRTH_TICKS.div_ceil(4);
        let first = begin + ramp;
        let last = begin + BIRTH_TICKS - ramp;
        let start = edge(first);
        let end = edge(last);
        assert_ne!(start, end);
        for tick in first + 1..last {
            let actual = edge(tick);
            let count = |span| crate::model::count(usize::try_from(span).unwrap());
            let fraction = count(tick - first) / count(last - first);
            for axis in 0..2 {
                assert!(
                    actual[axis]
                        .abs_diff_eq(start[axis].lerp(end[axis], fraction), 2e-6)
                );
            }
        }
    }

    #[test]
    fn overlapping_births_keep_shared_edge_acceleration_bounded() {
        let mut row = capacity_fixture();
        row.truncate(3);
        row[1].live = false;
        row[2].live = false;
        let mut rows = vec![row.clone(); 150];
        row[1].live = true;
        rows.extend(std::iter::repeat_n(row.clone(), 8));
        row[2].live = true;
        rows.extend(std::iter::repeat_n(row, 20));
        let packing = Packing::before_service(&rows);
        let samples: Vec<_> = (0..rows.len())
            .map(|tick| packing.layout_at(tick, &[], None, None).tiles)
            .collect();
        assert!(samples[140].iter().all(Option::is_some));
        for (tick, frame) in samples.windows(3).enumerate() {
            for (repository, ((a, b), c)) in
                frame[0].iter().zip(&frame[1]).zip(&frame[2]).enumerate()
            {
                let (Some(a), Some(b), Some(c)) = (a, b, c) else {
                    continue;
                };
                let low = c.low - b.low - (b.low - a.low);
                let high = c.high - b.high - (b.high - a.high);
                let peak =
                    (low.abs().max(high.abs()) * framing::VIEWPORT * 0.5).max_element();
                assert!(peak <= 1.0, "{tick}/{repository}: {peak}");
            }
        }
    }

    fn two_tiles_with_birth_at(birth: usize) -> Packing {
        let mut row = capacity_fixture();
        row.truncate(2);
        row[1].live = false;
        let mut rows = vec![row.clone(); birth];
        row[1].live = true;
        rows.extend(std::iter::repeat_n(row, 10));
        Packing::before_service(&rows)
    }

    fn assert_disjoint(placement: &[Option<Tile>]) {
        let visible: Vec<_> = placement.iter().flatten().collect();
        for (i, a) in visible.iter().enumerate() {
            for b in &visible[..i] {
                assert!((a.high.min(b.high) - a.low.max(b.low)).min_element() <= 1e-5);
            }
        }
    }

    #[test]
    fn thirty_tiles_partition_with_gutters_and_remain_readable_and_continuous() {
        let rows = fixture();
        let packing = Packing::before_service(&rows);
        let mut previous = vec![None; 30];
        for (tick, sample) in packing.samples.iter().enumerate() {
            let layout = packing.layout_at(tick, &[], None, None);
            let placement = layout.tiles;
            let moving = packing.reflowing(u64::try_from(tick).unwrap());
            if moving {
                assert_disjoint(&placement);
            } else {
                let tree = packing
                    .reflows
                    .iter()
                    .rfind(|change| change.tick <= tick)
                    .map_or(packing.nodes.as_slice(), |change| change.nodes.as_slice());
                let (root, gaps) = partition(tree, 0, &placement);
                let viewport = Tile::viewport();
                let root = root.unwrap();
                assert!(root.low.abs_diff_eq(viewport.low, 1e-6));
                assert!(root.high.abs_diff_eq(viewport.high, 1e-6));
                assert!(
                    (placement.iter().flatten().map(|t| t.area()).sum::<f32>() + gaps
                        - Tile::viewport().area())
                    .abs()
                        < 2e-6
                );
            }
            for (i, tile) in placement
                .iter()
                .enumerate()
                .filter_map(|(i, t)| t.map(|t| (i, t)))
            {
                if !moving && sample.presence.iter().all(|&p| p <= 0.0 || p >= 1.0) {
                    assert!(
                        tile.ground_aspect() >= 1.0 / BOUND - 1e-4,
                        "aspect at {tick}/{i}"
                    );
                    assert!(
                        tile.ground_aspect() <= BOUND + 1e-4,
                        "aspect at {tick}/{i}"
                    );
                }
                if let Some(before) = previous[i] {
                    let before: Tile = before;
                    assert!(
                        (tile.low - before.low).abs().max_element() < 0.15,
                        "low jump at {tick}/{i}"
                    );
                    assert!(
                        (tile.high - before.high).abs().max_element() < 0.15,
                        "high jump at {tick}/{i}: {before:?} -> {tile:?}"
                    );
                }
            }
            previous = placement;
        }
        let replay = Packing::before_service(&rows);
        for tick in [0, 60, 700, 1599] {
            let (_, origins, scales) = packing.sample(tick).unwrap();
            let (_, replay_origins, replay_scales) = replay.sample(tick).unwrap();
            assert_eq!((origins, scales), (replay_origins, replay_scales));
        }
    }

    #[test]
    fn archive_presence_scales_clamps_and_returns_the_whole_view_to_a_singleton() {
        let settled = fixture().pop().unwrap();
        let packing = Packing::before_service(&[settled]);
        for step in 0..=100_u16 {
            let p = f32::from(step) / 100.0;
            let presence: Vec<_> =
                (0..30).map(|i| if i == 0 { 1.0 } else { p }).collect();
            let (regions, _, _) =
                packing.sample_with_presence(1599, Some(&presence)).unwrap();
            let placement: Vec<_> = regions.iter().map(|r| r.tile).collect();
            let (_, gaps) = partition(&packing.nodes, 0, &placement);
            assert!(
                (placement.iter().flatten().map(|t| t.area()).sum::<f32>() + gaps
                    - Tile::viewport().area())
                .abs()
                    < 2e-6
            );
            if step == 0 {
                assert_eq!(regions[0].tile, Some(Tile::viewport()));
            }
        }
        let (regions, _, _) = packing
            .sample_with_presence(1599, Some(&[0.0; 30]))
            .unwrap();
        assert!(regions.iter().all(|r| !r.live));
        assert!(Packing::default().sample(0).is_none());
    }

    #[test]
    fn left_tile_exits_beside_two_stacked_tiles() {
        let packing = ending_fixture(false);
        assert!(matches!(packing.nodes[0], Node::Split { axis: 0, .. }));
        let at = |tick| packing.sample(tick).unwrap().0;
        let before = at(60);
        let middle = at(180);
        let after = at(300);
        assert!(middle[0].tile.unwrap().area() < before[0].tile.unwrap().area());
        assert!(after[0].tile.is_none());
        for pair in after[1..].windows(2) {
            let a = pair[0].tile.unwrap();
            let b = pair[1].tile.unwrap();
            assert!(a.high.y <= b.low.y || b.high.y <= a.low.y);
            assert!(a.ground_aspect() <= BOUND + 0.01);
            assert!(b.ground_aspect() <= BOUND + 0.01);
        }
    }

    #[test]
    fn repositories_with_a_shared_ending_all_shrink() {
        let packing = ending_fixture(true);
        let at = |tick| packing.sample(tick).unwrap().0;
        let mut previous = at(60);
        let mut last = [0.0; 3];
        for tick in 61..=300 {
            let current = at(tick);
            for (index, (before, after)) in previous.iter().zip(&current).enumerate() {
                let width = |region: &RepositoryRegion| {
                    region.tile.map_or(0.0, |tile| tile.pixels().x)
                };
                let loss = width(before) - width(after);
                assert!(
                    (-0.01..10.0).contains(&loss),
                    "repository {index} moved {loss} px at tick {tick}"
                );
                last[index] += loss;
            }
            if tick == 180 {
                assert!(last.iter().all(|&loss| loss > 20.0));
            }
            previous = current;
        }
        assert!(previous.iter().all(|region| region.tile.is_none()));
    }

    #[test]
    fn staggered_exits_keep_tile_edges_continuous() {
        let weights = [20.0, 39.0, 4.0, 37.0, 25.0, 29.0, 5.0, 21.0];
        let aspects = [1.9, 1.2, 0.7, 2.7, 1.1, 2.9, 2.4, 1.6];
        let phases = [79_u32, 8, 51, 153, 58, 21, 52, 146];
        let space = Space::new(4.0, 1.0);
        let mut nodes = Vec::new();
        for index in 0..weights.len() {
            let mut born = weights;
            born[index + 1..].fill(0.0);
            assert!(insert(&mut nodes, index, &born, &aspects, None, space, 1));
        }
        let mut largest = 0.0_f32;
        for stride in [1, 3, 5, 7] {
            for offset in 0..phases.len() {
                let phases = std::array::from_fn::<_, 8, _>(|i| {
                    phases[(i * stride + offset) % phases.len()]
                });
                for spacing in [1, 2, 3] {
                    let starts = phases.map(|tick| Some(u64::from(tick * spacing)));
                    largest =
                        largest.max(staggered_edge_step(&nodes, &weights, &starts));
                }
            }
        }
        println!("Largest per-tick tile edge step: {largest:.3} reference px");
        assert!(largest <= 4.5, "tile jumped {largest:.3} reference px");
    }

    fn staggered_edge_step(
        nodes: &[Node],
        weights: &[f32],
        starts: &[Option<u64>],
    ) -> f32 {
        let exits = crate::model::exits_from_starts(starts);
        let end = exits.iter().flatten().map(|exit| exit.end).max().unwrap();
        let samples: Vec<_> = (0..=end)
            .map(|tick| Sample {
                weights: weights.to_vec(),
                presence: exits
                    .iter()
                    .map(|exit| exit.map_or(1.0, |exit| exit_presence(tick, exit)))
                    .collect(),
            })
            .collect();
        let content = Input::from(capacity_fixture().swap_remove(0));
        let packing = Packing {
            gutter_scale: 4.0,
            nodes: nodes.to_vec(),
            rotated: vec![false; weights.len()],
            content_scales: vec![1.0; samples.len() * weights.len()].into(),
            inputs: vec![vec![content; weights.len()]; samples.len()].into(),
            samples: samples.into(),
            exits: exits.into(),
            ..Packing::default()
        };
        let at = |tick| {
            packing
                .fit_rows(tick, tick, &[], None, None)
                .into_iter()
                .map(|placement| placement.tile)
                .collect::<Vec<_>>()
        };
        for (index, exit) in packing.exits.iter().enumerate() {
            let end = usize::try_from(exit.unwrap().end).unwrap();
            assert!(packing.samples[end - 1].presence[index] > 0.0);
            assert_eq!(packing.samples[end].presence[index], 0.0);
        }
        let mut largest = 0.0_f32;
        let mut previous = at(0);
        for tick in 1..packing.samples.len() {
            let current = at(tick);
            for (index, (&before, &after)) in previous.iter().zip(&current).enumerate()
            {
                assert_eq!(
                    after.is_some(),
                    packing.samples[tick].presence[index] > 0.0
                );
                largest = largest.max(match (before, after) {
                    (Some(a), Some(b)) => {
                        let step = (a.low - b.low).abs().max((a.high - b.high).abs());
                        (step * framing::VIEWPORT * 0.5).max_element()
                    }
                    (Some(a), None) => a.pixels().max_element(),
                    (None, _) => 0.0,
                });
                let Some(a) = after else { continue };
                assert!(current[..index].iter().flatten().all(|b| {
                    (a.high.min(b.high) - a.low.max(b.low)).min_element() < 1e-5
                }));
            }
            previous = current;
        }
        assert!(previous.iter().all(Option::is_none));
        largest
    }

    fn ending_fixture(shared_ending: bool) -> Packing {
        let mut row = capacity_fixture();
        row.truncate(3);
        row[0].items = 2.0;
        for region in &mut row[1..] {
            let extent = Vec2::new(1.0, 0.5);
            region.low = -extent;
            region.high = extent;
            region.outline = outline::rectangle(-extent, extent);
        }
        let mut inputs = Inputs::new(301);
        for _ in 0..301 {
            inputs.push(row.clone());
        }
        let other = shared_ending.then_some(60);
        let exits = crate::model::exits_from_starts(&[Some(60), other, other]);
        Packing::from_inputs(inputs, exits.into(), 4.0, 1.0, 1).unwrap()
    }

    #[test]
    fn ended_repository_shrinks_while_its_neighbour_reclaims_the_field() {
        let mut inputs = Inputs::new(1600);
        for row in fixture().into_iter().take(1600) {
            inputs.push(row.into_iter().take(2).collect());
        }
        let exits = crate::model::exits_from_starts(&[Some(1350), None]);
        let packing = Packing::from_inputs(inputs, exits.into(), 4.0, 1.0, 1).unwrap();
        let tiles = |tick| {
            let regions = packing.sample(tick).unwrap().0;
            [regions[0].tile, regions[1].tile]
        };
        let mut previous = tiles(1349);
        assert!(previous[0].is_some());
        let mut largest = 0.0_f32;
        for tick in 1350..=1590 {
            let current = tiles(tick);
            for (before, after) in previous.into_iter().zip(current) {
                if let Some((before, after)) = before.zip(after) {
                    let delta = (before.low - after.low)
                        .abs()
                        .max((before.high - after.high).abs())
                        * framing::VIEWPORT
                        * 0.5;
                    largest = largest.max(delta.max_element());
                } else if let Some(before) = before {
                    largest = largest.max(before.pixels().min_element());
                }
            }
            previous = current;
        }
        assert!(largest < 10.0, "tile jumped {largest} reference pixels");
        assert!(tiles(1450)[0].is_some());
        assert_eq!(
            tiles(1590),
            [None, Some(packing.space().field(framing::MARGIN))]
        );
    }
}
