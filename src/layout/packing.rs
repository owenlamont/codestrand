use std::sync::Arc;

use bevy::math::{FloatExt, Vec2};

use super::{RepositoryRegion, outline};
use crate::model::{caption, framing};

mod fit;
mod held;
mod zoom;

const BOUND: f32 = 2.5;
pub(crate) const MINIMUM: Vec2 = Vec2::new(144.0, 64.0);
pub(crate) const BIRTH_TICKS: u32 = 45;
/// Ticks whose zoom is fitted together; fixed so counts do not depend on the thread count.
const ZOOM_CHUNK: usize = 256;
const ALLOCATION_TOLERANCE_PX: f32 = 0.25;

/// Detail shrink per failed packing attempt, and the attempts allowed before giving up.
const DETAIL_SHRINK: f32 = 1.125;
const DETAIL_ATTEMPTS: u32 = 64;
/// Bisection steps towards full presence for a sample whose easing tiles conflict.
const FIRMING_STEPS: u32 = 8;
/// Insertion candidates each scoring thread takes at least, so small trees stay serial.
const PARALLEL_SPLITS: usize = 32;

#[derive(Debug, thiserror::Error)]
#[error(
    "repository tile packing cannot place {repositories} repositories at {detail} times the reference tile size"
)]
pub(crate) struct CapacityError {
    repositories: usize,
    detail: f32,
}

/// The smallest tile and the gutters between tiles, in reference pixels.
#[derive(Clone, Copy, Debug)]
struct Space {
    minimum: Vec2,
    gap: Vec2,
}

impl Space {
    fn new(detail: f32, gutter: f32) -> Self {
        Self {
            minimum: MINIMUM * detail,
            gap: Vec2::splat(gutter * detail),
        }
    }
}

/// The split tree inserting repositories in birth `order`, at the first detail that fits
/// when shrinking from full size, or from the area bound when that is smaller.
fn plan_tree(
    regions: &[Vec<Input>],
    items: &[Vec<f32>],
    births: &[usize],
    order: &[usize],
    gutter: f32,
    threads: usize,
) -> Result<(Vec<Node>, f32), CapacityError> {
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
            if !insert(&mut nodes, i, &weights, &aspects, space, threads) {
                return None;
            }
        }
        Some(nodes)
    };
    let mut detail = area_detail(order.len(), gutter).min(1.0);
    for _ in 0..DETAIL_ATTEMPTS {
        if let Some(nodes) = place(Space::new(detail, gutter)) {
            return Ok((nodes, detail));
        }
        detail /= DETAIL_SHRINK;
    }
    Err(CapacityError {
        repositories: order.len(),
        detail,
    })
}

/// The detail at which every minimum tile and its gutters would exactly cover the field.
fn area_detail(repositories: usize, gutter: f32) -> f32 {
    let needed = MINIMUM + Vec2::splat(gutter);
    (Tile::viewport().pixels().element_product()
        / (needed.element_product() * crate::model::count(repositories)))
    .sqrt()
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
    fn inset(margin: f32) -> Self {
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

#[derive(Clone, Debug)]
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

// Feasible size polygons keep the children's aspect intervals while accounting for
// fixed pixel gutters and floors.
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
    let limit = Tile::viewport().pixels();
    let state = match nodes[index] {
        Node::Repository(i) => {
            let p = presence[i];
            let mut sizes = outline::rectangle(space.minimum, limit);
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
            let gutter =
                space.gap[axis] * a.presence.min(b.presence) / presence.max(1e-9);
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
            assert!(available >= 0.0, "nonnegative space after gutter");
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

/// The feasibility states of the last presence vector tiled, and how often they were reused.
#[derive(Default)]
struct Tiling {
    presence: Vec<u32>,
    states: Vec<Feasible>,
    samples: usize,
    reuses: usize,
}

/// Feasibility polygons depend on presence alone, so one presence vector's are kept.
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
    allocated(nodes, weights.len(), &states)
}

fn allocated(
    nodes: &[Node],
    repositories: usize,
    states: &[Feasible],
) -> Option<Vec<Option<Tile>>> {
    let size = Tile::viewport().pixels();
    let (low, high) = interval(&states[0].sizes, 0, size.y)?;
    if size.x < low - ALLOCATION_TOLERANCE_PX || size.x > high + ALLOCATION_TOLERANCE_PX
    {
        return None;
    }
    let mut output = vec![None; repositories];
    allocate(nodes, 0, Tile::viewport(), states, Vec2::ONE, &mut output)?;
    Some(output)
}

fn insert(
    nodes: &mut Vec<Node>,
    repository: usize,
    weights: &[f32],
    aspects: &[f32],
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
        let placement = allocated(&candidate, weights.len(), &states)?;
        let total: f32 = weights.iter().sum();
        Some(
            placement
                .iter()
                .enumerate()
                .filter_map(|(i, t)| {
                    t.map(|t| {
                        weights[i]
                            * ((t.area() / Tile::viewport().area() * total
                                / weights[i])
                                .ln()
                                .abs()
                                + (t.ground_aspect() / aspects[i]).ln().abs())
                    })
                })
                .sum::<f32>(),
        )
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
    let mut best: Option<(f32, (usize, usize))> = None;
    for (&split, score) in splits.iter().zip(scores) {
        if let Some(score) = score
            && best.is_none_or(|(cost, _)| score < cost - 1e-5)
        {
            best = Some((score, split));
        }
    }
    let Some((_, (host, axis))) = best else {
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
    origin: Vec2,
    scale: f32,
}

fn gutter_cells(nodes: &[Node], placement: &[Placement]) -> Vec<Option<Tile>> {
    fn bounds(nodes: &[Node], index: usize, placement: &[Placement]) -> Option<Tile> {
        match nodes[index] {
            Node::Repository(i) => placement[i].tile,
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
        placement: &[Placement],
        result: &mut [Option<Tile>],
    ) {
        match nodes[index] {
            Node::Repository(i) => result[i] = placement[i].tile.map(|_| cell),
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
    divide(nodes, 0, Tile::inset(16.0), placement, &mut result);
    result
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Packing {
    gutter_scale: f32,
    /// Scale of fixed sizes against the reference viewport; zero before planning.
    detail: f32,
    /// Repository title scale; zero before planning.
    lettering: f32,
    nodes: Vec<Node>,
    labels: Arc<[String]>,
    inputs: Arc<[Vec<Input>]>,
    samples: Arc<[Sample]>,
    rotated: Vec<bool>,
    zoom: Arc<[Vec<f32>]>,
    visible_until: Arc<[crate::model::Time]>,
    tiling: TilingCache,
    /// Tiles every sample from scratch; tests compare against it.
    fresh_tiling: bool,
    /// Threads for zoom planning; zero takes the machine's parallelism.
    threads: usize,
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
    /// When each repository starts fading out, after which its count no longer moves tiles.
    fading: Vec<crate::model::Time>,
}

impl Inputs {
    pub(crate) fn new(ticks: usize) -> Self {
        Self {
            labels: Vec::new(),
            rows: Vec::with_capacity(ticks),
            items: Vec::with_capacity(ticks),
            fading: Vec::new(),
        }
    }

    /// Stops counting each repository `exit` seconds after its end, when it starts fading.
    pub(crate) fn fade_from(&mut self, ends: &[crate::model::Time], exit: f32) {
        self.fading = ends.iter().map(|&end| end + exit).collect();
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

    pub(crate) fn anticipate_items(
        &mut self,
        repository: usize,
        begin: usize,
        arrival: usize,
    ) {
        let items = self.items[arrival][repository];
        for row in &mut self.items[begin..arrival] {
            row[repository] = items;
        }
    }
}
impl Input {
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
        let placement = tiles(nodes, &sample.weights, &sample.presence, space)
            .expect("planned tiles remain feasible");
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
    pub(crate) fn input_bytes() -> usize {
        std::mem::size_of::<Input>()
            + outline::NORMALS.len() * std::mem::size_of::<Vec2>()
            + 3 * std::mem::size_of::<f32>()
    }

    pub(crate) fn row_bytes() -> usize {
        std::mem::size_of::<Vec<Input>>()
            + std::mem::size_of::<Sample>()
            + std::mem::size_of::<Vec<f32>>()
    }

    pub(crate) fn conversion_bytes() -> usize {
        std::mem::size_of::<Sample>()
    }

    pub(crate) fn with_endings(
        mut self,
        endings: &[crate::model::Time],
        exit: f32,
        reflow: f32,
    ) -> Self {
        if !endings.iter().any(|end| end.is_finite()) {
            return self;
        }
        self.visible_until = endings.iter().map(|&end| end + exit).collect();
        for (tick, sample) in Arc::make_mut(&mut self.samples).iter_mut().enumerate() {
            let now = crate::model::tick_time(
                u64::try_from(tick).expect("bounded packing tick"),
            );
            for (presence, &end) in sample.presence.iter_mut().zip(endings) {
                let u = ((now - end - exit) / reflow).clamp(0.0, 1.0);
                *presence *= 1.0 - u * u * (3.0 - 2.0 * u);
                // Subpixel regions fall below the feasible-polygon clipping tolerance.
                if u > 0.0 && *presence < 0.001 {
                    *presence = 0.0;
                }
            }
        }
        self.plan_zoom()
    }
    #[cfg(test)]
    pub(crate) fn before_service(regions: &[Vec<RepositoryRegion>]) -> Self {
        Self::with_gutter(regions, 4.0, None).unwrap()
    }

    #[cfg(test)]
    pub(crate) fn with_gutter(
        regions: &[Vec<RepositoryRegion>],
        gutter_scale: f32,
        activation: Option<&[u32]>,
    ) -> Result<Self, CapacityError> {
        let mut inputs = Inputs::new(regions.len());
        for row in regions {
            inputs.push(row.clone());
        }
        Self::from_inputs(inputs, gutter_scale, activation, 0)
    }

    /// Inputs, samples, zoom and tree with floats printed exactly, without the thread count.
    #[cfg(test)]
    pub(crate) fn exact_state(&self) -> String {
        format!(
            "{:?}{:?}{:?}{:?}{:?}",
            self.nodes, self.inputs, self.samples, self.rotated, self.zoom
        )
    }

    /// The fitted scale of every repository visible on `tick`, zero for the others.
    fn visible_scales(&self, tick: usize, tiling: &mut Tiling) -> Vec<f32> {
        self.fit_rows(tick, tick, None, None, Some(tiling))
            .iter()
            .zip(&self.inputs[tick])
            .enumerate()
            .map(|(i, (p, r))| {
                if r.live
                    && crate::model::tick_time(
                        u64::try_from(tick).expect("bounded packing tick"),
                    ) < self
                        .visible_until
                        .get(i)
                        .copied()
                        .unwrap_or(crate::model::Time::INFINITY)
                {
                    p.scale
                } else {
                    0.0
                }
            })
            .collect()
    }

    pub(crate) fn from_inputs(
        inputs: Inputs,
        gutter_scale: f32,
        activation: Option<&[u32]>,
        threads: usize,
    ) -> Result<Self, CapacityError> {
        let Inputs {
            labels,
            rows: regions,
            items,
            fading,
        } = inputs;
        let Some(first) = regions.first().filter(|r| !r.is_empty()) else {
            return Ok(Self::default());
        };
        let births: Vec<_> = (0..first.len())
            .map(|i| regions.iter().position(|r| r[i].live).unwrap_or(usize::MAX))
            .collect();
        let mut order: Vec<_> = (0..first.len()).collect();
        order.sort_by_key(|&i| (births[i], i));
        let (nodes, detail) =
            plan_tree(&regions, &items, &births, &order, gutter_scale, threads)?;
        let lettering = detail.min(0.5 * area_detail(first.len(), gutter_scale));
        let space = Space::new(detail, gutter_scale);
        let mut held = held::Weights::new(first.len());
        let earliest = births.iter().copied().min().unwrap_or(0);
        let samples: Vec<_> = regions
            .iter()
            .enumerate()
            .map(|(tick, row)| {
                let mut presence: Vec<_> = row
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        let birth = births[i];
                        let ready = activation.map_or(birth, |a| {
                            birth
                                .min(usize::try_from(a[i]).expect("bounded activation"))
                        });
                        let begin = ready.saturating_sub(
                            usize::try_from(BIRTH_TICKS)
                                .expect("birth ticks fit usize"),
                        );
                        if tick < begin || birth == usize::MAX {
                            return 0.0;
                        }
                        if births[i] == earliest {
                            return 1.0;
                        }
                        let elapsed = crate::model::ticks_duration(
                            u64::try_from(tick.saturating_sub(begin))
                                .expect("bounded timeline"),
                        );
                        let transition = crate::model::ticks_duration(
                            u64::try_from(ready - begin).expect("bounded birth"),
                        )
                        .max(crate::model::tick_seconds());
                        let u = (elapsed / transition).min(1.0);
                        u * u * (3.0 - 2.0 * u)
                    })
                    .collect();
                if presence.iter().all(|&p| p == 0.0) {
                    presence[order[0]] = 1.0;
                }
                let now = crate::model::tick_time(
                    u64::try_from(tick).expect("bounded packing tick"),
                );
                let shown: Vec<_> = presence
                    .iter()
                    .enumerate()
                    .map(|(i, &p)| {
                        if fading.get(i).is_some_and(|&fade| now >= fade) {
                            0.0
                        } else {
                            p
                        }
                    })
                    .collect();
                Sample {
                    weights: held.step(tick, &items[tick], &shown),
                    presence,
                }
            })
            .collect();
        let rotated = rotations(&nodes, &regions, &samples, space);
        drop(items);
        Ok(Self {
            gutter_scale,
            detail,
            lettering,
            nodes,
            labels: labels.into(),
            inputs: regions.into(),
            samples: samples.into(),
            rotated,
            zoom: Arc::default(),
            visible_until: Arc::default(),
            tiling: TilingCache::default(),
            fresh_tiling: false,
            threads,
        }
        .plan_zoom())
    }

    fn plan_zoom(mut self) -> Self {
        self.zoom = Arc::default();
        let ticks: Vec<_> = (0..self.inputs.len()).collect();
        let mut scales = super::in_order(&ticks, ZOOM_CHUNK, self.threads, |chunk| {
            let mut tiling = Tiling::default();
            let rows = chunk
                .iter()
                .map(|&tick| self.visible_scales(tick, &mut tiling))
                .collect();
            let mut shared = self.tiling.lock();
            shared.samples += tiling.samples;
            shared.reuses += tiling.reuses;
            rows
        });
        zoom::plan(&mut scales);
        self.zoom = scales.into();
        self
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
        self.sample_rows(tick, tick, None, presence, selected)
    }

    pub(crate) fn sample_split(
        &self,
        tiles: u64,
        content: u64,
        transition: Option<(u64, f32)>,
        presence: Option<&[f32]>,
        repositories: Option<&[usize]>,
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        self.sample_rows(tiles, content, transition, presence, |index| {
            repositories.is_none_or(|selected| selected.contains(&index))
        })
    }

    fn sample_rows(
        &self,
        tiles: u64,
        tick: u64,
        transition: Option<(u64, f32)>,
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
        let placement = self.fit_rows(tiles, tick, transition, presence, None);
        let mut regions: Vec<_> = self.inputs[tick]
            .iter()
            .zip(self.labels.iter())
            .map(|(input, label)| input.region(label))
            .collect();
        let gutters = gutter_cells(&self.nodes, &placement);
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
            r.live = tile.is_some();
            r.tile = tile;
            r.gutter = gutters[i].filter(|_| selected(i));
            r.items = sample.weights[i]
                * (sample.presence[i]
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

    /// [`tiles`] for this packing, reusing the polygons while presence repeats bit for bit.
    fn tiles(&self, weights: &[f32], presence: &[f32]) -> Option<Vec<Option<Tile>>> {
        if self.fresh_tiling {
            return tiles(&self.nodes, weights, presence, self.space());
        }
        self.tiles_in(&mut self.tiling.lock(), weights, presence)
    }

    /// [`Self::tiles`] against a caller's own kept polygons, for use off the shared ones.
    fn tiles_in(
        &self,
        tiling: &mut Tiling,
        weights: &[f32],
        presence: &[f32],
    ) -> Option<Vec<Option<Tile>>> {
        let space = self.space();
        tiling.samples += 1;
        if tiling.states.len() == self.nodes.len()
            && tiling
                .presence
                .iter()
                .copied()
                .eq(presence.iter().map(|p| p.to_bits()))
        {
            tiling.reuses += 1;
            reweigh(&self.nodes, 0, weights, presence, &mut tiling.states);
        } else {
            tiling.presence = presence.iter().map(|p| p.to_bits()).collect();
            tiling.states = vec![Feasible::default(); self.nodes.len()];
            feasibility(&self.nodes, 0, weights, presence, space, &mut tiling.states);
        }
        allocated(&self.nodes, weights.len(), &tiling.states)
    }

    /// Tiles with every easing repository moved towards full presence by the least
    /// bisected fraction that fits, since fractional slivers can conflict where the
    /// planned full-presence arrangement does not.
    fn firmer_tiles(
        &self,
        weights: &[f32],
        presence: &[f32],
    ) -> Option<Vec<Option<Tile>>> {
        let at = |firmness: f32| {
            let firm: Vec<_> = presence
                .iter()
                .map(|&p| if p > 0.0 { p.lerp(1.0, firmness) } else { 0.0 })
                .collect();
            tiles(&self.nodes, weights, &firm, self.space())
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
        Space::new(self.detail(), self.gutter_scale)
    }

    /// Scale of tiles, gutters and avatars against their reference sizes: one while
    /// every repository fits at full size, smaller as more must share the field.
    pub(crate) fn detail(&self) -> f32 {
        if self.detail > 0.0 { self.detail } else { 1.0 }
    }

    /// Repository title scale: full size while each repository's share of the field
    /// holds four minimum tiles, then half that share's side, and never above [`Self::detail`].
    pub(crate) fn lettering(&self) -> f32 {
        if self.lettering > 0.0 {
            self.lettering
        } else {
            1.0
        }
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

    fn fit_rows(
        &self,
        tile_tick: usize,
        tick: usize,
        transition: Option<(u64, f32)>,
        presence: Option<&[f32]>,
        tiling: Option<&mut Tiling>,
    ) -> Vec<Placement> {
        let sample = &self.samples[tile_tick];
        let mut weights = sample.weights.clone();
        let mut visibility = sample.presence.clone();
        if let Some((from, fraction)) = transition {
            let from = usize::try_from(from)
                .unwrap_or(usize::MAX)
                .min(self.samples.len() - 1);
            for (weight, &before) in weights.iter_mut().zip(&self.samples[from].weights)
            {
                *weight = before.lerp(*weight, fraction);
            }
            for (visible, &before) in
                visibility.iter_mut().zip(&self.samples[from].presence)
            {
                *visible = before.lerp(*visible, fraction);
            }
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
        let placement = match tiling {
            Some(tiling) if !self.fresh_tiling => {
                self.tiles_in(tiling, &weights, &presence)
            }
            _ => self.tiles(&weights, &presence),
        }
        .or_else(|| self.firmer_tiles(&weights, &presence))
        .expect("tiles at full presence remain feasible");
        self.inputs[tick]
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let turn = |p: Vec2| if self.rotated[i] { p.perp() } else { p };
                let shape: Vec<_> = r.outline.iter().copied().map(turn).collect();
                let tile = placement[i];
                // A fixed screen anchor avoids switching between equal-area fit vertices.
                let fitted = tile.map(|tile| {
                    fit::anchored(tile.usable(), &shape, r.height, Vec2::splat(0.5))
                });
                let raw_scale = fitted.map_or(0.0, |f| f.scale);
                let scale = self
                    .zoom
                    .get(tile_tick)
                    .map_or(raw_scale, |row| row[i].min(raw_scale));
                let (low, high) = outline::bounds(&shape);
                let origin = fitted.map_or(Vec2::ZERO, |f| {
                    f.origin + (low + high) * 0.5 * (raw_scale - scale)
                });
                Placement {
                    tile,
                    origin,
                    scale,
                }
            })
            .collect()
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
    fn detail_shrinks_until_every_minimum_tile_fits() {
        let packing = Packing::with_gutter(&[capacity_fixture()], 4.0, None).unwrap();
        let field = Tile::viewport();
        let detail = packing.detail();
        assert!(detail < 1.0);
        let (regions, _, _) = packing.sample(0).unwrap();
        let tiles: Vec<_> = regions.iter().map(|r| r.tile.unwrap()).collect();
        for (index, tile) in tiles.iter().enumerate() {
            assert!(
                tile.pixels()
                    .cmpge(MINIMUM * detail - Vec2::splat(0.01))
                    .all()
            );
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
        let counts = [100.0, 300.0];
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
        for (area, items) in areas.iter().zip(counts) {
            let share = area / total / (items / 400.0);
            assert!((0.9..1.1).contains(&share), "{areas:?}");
        }
    }

    #[test]
    fn a_set_that_fits_at_full_size_keeps_full_detail() {
        assert_eq!(Packing::before_service(&fixture()).detail(), 1.0);
    }

    #[test]
    fn titles_shrink_while_seventy_two_tiles_still_fit_at_full_size() {
        let packing = Packing::with_gutter(&[capacity_fixture()], 4.0, None).unwrap();
        assert!(packing.lettering() < packing.detail());
        assert!(area_detail(72, 4.0) > 1.0);
        assert!(0.5 * area_detail(72, 4.0) < 0.6);
    }

    #[test]
    fn empty_birth_tiles_do_not_constrain_later_content_zoom() {
        let rows = fixture();
        let packing = Packing::before_service(&rows);
        for (tick, row) in rows.iter().enumerate() {
            for (i, region) in row.iter().enumerate().filter(|(_, r)| !r.live) {
                assert_eq!(
                    packing.zoom[tick][i], 0.0,
                    "unborn {} at {tick}",
                    region.label
                );
            }
        }
        assert!(packing.zoom[40][1] > 0.0);
    }

    #[test]
    fn a_padded_birth_can_finish_before_the_waiting_contributor_arrives() {
        let rows = fixture();
        let mut activation = vec![u32::MAX; 30];
        activation[3] = 30;
        let normal = Packing::with_gutter(&rows, 2.0, None).unwrap();
        let waiting = Packing::with_gutter(&rows, 2.0, Some(&activation)).unwrap();
        assert!(normal.sample(35).unwrap().0[3].tile.is_none());
        assert!(waiting.sample(35).unwrap().0[3].tile.is_some());
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
        assert_eq!(tiled, samples.len());
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
                    let packing = Packing::with_gutter(&rows, scale, None).unwrap();
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
                    assert!(
                        row.iter().filter_map(|r| r.tile).all(|t| t
                            .pixels()
                            .cmpge(MINIMUM - Vec2::splat(0.02))
                            .all())
                    );
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
    fn thirty_tiles_partition_with_gutters_and_remain_readable_and_continuous() {
        let rows = fixture();
        let packing = Packing::before_service(&rows);
        let mut previous = vec![None; 30];
        for (tick, sample) in packing.samples.iter().enumerate() {
            let placement = tiles(
                &packing.nodes,
                &sample.weights,
                &sample.presence,
                packing.space(),
            )
            .unwrap();
            let (root, gaps) = partition(&packing.nodes, 0, &placement);
            assert_eq!(root, Some(Tile::viewport()));
            assert!(
                (placement.iter().flatten().map(|t| t.area()).sum::<f32>() + gaps
                    - Tile::viewport().area())
                .abs()
                    < 2e-6
            );
            for (i, tile) in placement
                .iter()
                .enumerate()
                .filter_map(|(i, t)| t.map(|t| (i, t)))
            {
                let p = sample.presence[i];
                assert!(
                    tile.pixels().cmpge(MINIMUM * p - Vec2::splat(0.02)).all(),
                    "floor at {tick}/{i}"
                );
                assert!(
                    tile.ground_aspect() >= p / BOUND - 1e-4,
                    "aspect at {tick}/{i}"
                );
                assert!(
                    tile.ground_aspect() <= BOUND / p + 1e-4,
                    "aspect at {tick}/{i}"
                );
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
        let packing = Packing::before_service(&fixture());
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
    fn invisible_ending_slivers_do_not_zoom_out_visible_content() {
        let rows: Vec<_> = fixture()
            .into_iter()
            .take(1600)
            .map(|row| row.into_iter().take(2).collect())
            .collect();
        let original = Packing::before_service(&rows);
        let ending = original.clone().with_endings(
            &[
                crate::model::Time::seconds(20.0),
                crate::model::Time::INFINITY,
            ],
            2.5,
            4.0,
        );
        for tick in 600..1200 {
            let before = original.sample(tick).unwrap().2[0];
            let after = ending.sample(tick).unwrap().2[0];
            assert!(
                after >= before * 0.99,
                "premature collapse at {tick}: {before} -> {after}"
            );
        }
    }

    #[test]
    fn ending_holds_tiles_until_fade_finishes_then_reflows_for_four_seconds() {
        let original = Packing::before_service(&fixture());
        let endings: Vec<_> = (0..30)
            .map(|i| {
                if i == 0 {
                    crate::model::Time::INFINITY
                } else {
                    crate::model::Time::seconds(20.0)
                }
            })
            .collect();
        let ending = original.clone().with_endings(&endings, 2.5, 4.0);
        assert_eq!(
            original.sample(1350).unwrap().0[0].tile,
            ending.sample(1350).unwrap().0[0].tile
        );
        let middle = ending.sample(1470).unwrap().0;
        assert!(middle[1].tile.is_some());
        let final_regions = ending.sample(1599).unwrap().0;
        assert_eq!(final_regions[0].tile, Some(Tile::viewport()));
        assert!(
            final_regions[1..]
                .iter()
                .all(|region| region.tile.is_none())
        );
        assert_eq!(ending.sample(1470).unwrap().0[0].tile, middle[0].tile);
    }
}
