use std::sync::{Arc, OnceLock};

use bevy::math::{FloatExt, Vec2};

use super::{RepositoryRegion, outline};
use crate::model::framing;

mod fit;
mod zoom;

const BOUND: f32 = 2.5;
pub(crate) const MINIMUM: Vec2 = Vec2::new(144.0, 64.0);
const BIRTH_SECONDS: f32 = 0.75;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Tile {
    pub low: Vec2,
    pub high: Vec2,
}

impl Tile {
    pub(crate) fn viewport() -> Self {
        let limit = Vec2::ONE - Vec2::splat(2.0 * framing::MARGIN) / framing::VIEWPORT;
        Self {
            low: -limit,
            high: limit,
        }
    }

    pub(crate) fn area(self) -> f32 {
        (self.high - self.low).element_product() * 0.25
    }

    pub(crate) fn pixels(self) -> Vec2 {
        (self.high - self.low) * framing::VIEWPORT * 0.5
    }

    pub(crate) fn ground_aspect(self) -> f32 {
        let size = self.pixels();
        size.x / size.y * crate::model::view_direction().y
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
        self.corners().map(framing::unproject)
    }
}

pub(crate) fn gutters(count: f32) -> Vec2 {
    let shrink = ((count - 12.0) / 18.0).clamp(0.0, 1.0);
    Vec2::new(16.0, 32.0).lerp(Vec2::new(12.0, 16.0), shrink)
}

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
    let mut low = f32::INFINITY;
    let mut high = f32::NEG_INFINITY;
    let mut before = *points.last()?;
    for &point in points {
        let delta = point[1 - axis] - before[1 - axis];
        if delta.abs() > 1e-5 {
            let t = (cross - before[1 - axis]) / delta;
            if (-1e-5..=1.00001).contains(&t) {
                let value = before[axis].lerp(point[axis], t.clamp(0.0, 1.0));
                low = low.min(value);
                high = high.max(value);
            }
        } else if (cross - point[1 - axis]).abs() < 0.001 {
            low = low.min(point[axis]).min(before[axis]);
            high = high.max(point[axis]).max(before[axis]);
        }
        before = point;
    }
    (low <= high).then_some((low, high))
}

// Feasible size polygons retain S1's aspect intervals while accounting for fixed pixel gutters and floors.
fn combine(a: &[Vec2], b: &[Vec2], axis: usize, gap: f32) -> Vec<Vec2> {
    let mut coordinates: Vec<_> = a.iter().chain(b).map(|p| p[1 - axis]).collect();
    coordinates.sort_by(f32::total_cmp);
    coordinates.dedup_by(|a, b| (*a - *b).abs() < 0.001);
    let mut low = Vec::new();
    let mut high = Vec::new();
    for cross in coordinates {
        if let Some(((a0, a1), (b0, b1))) =
            interval(a, axis, cross).zip(interval(b, axis, cross))
        {
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
            ((point - before).perp_dot(after - point).abs() > 0.01).then_some(point)
        })
        .collect()
}

fn feasibility(
    nodes: &[Node],
    index: usize,
    weights: &[f32],
    presence: &[f32],
    gap: Vec2,
    result: &mut [Feasible],
) {
    if let Node::Split { first, second, .. } = nodes[index] {
        feasibility(nodes, first, weights, presence, gap, result);
        feasibility(nodes, second, weights, presence, gap, result);
    }
    update(nodes, index, weights, presence, gap, result);
}

fn update(
    nodes: &[Node],
    index: usize,
    weights: &[f32],
    presence: &[f32],
    gap: Vec2,
    result: &mut [Feasible],
) {
    let limit = Tile::viewport().pixels();
    let state = match nodes[index] {
        Node::Repository(i) => {
            let p = presence[i];
            let mut sizes = outline::rectangle(MINIMUM, limit);
            let vertical = crate::model::view_direction().y;
            for normal in [
                Vec2::new(-1.0, 1.0 / (BOUND * vertical)),
                Vec2::new(1.0, -BOUND / vertical),
            ] {
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
            let gutter = gap[axis] * a.presence.min(b.presence) / presence.max(1e-9);
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
) {
    if states[index].presence == 0.0 {
        return;
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
                allocate(nodes, second, tile, states, stretch, tiles);
                return;
            }
            if b.presence == 0.0 {
                allocate(nodes, first, tile, states, stretch, tiles);
                return;
            }
            let size = tile.pixels() / stretch;
            let pa = a.presence / states[index].presence;
            let pb = b.presence / states[index].presence;
            let child_interval = |child: &Feasible| {
                let cross = size[1 - axis];
                interval(&child.sizes, axis, cross)
                    .or_else(|| {
                        let low = child
                            .sizes
                            .iter()
                            .map(|p| p[1 - axis])
                            .fold(f32::INFINITY, f32::min);
                        let high = child
                            .sizes
                            .iter()
                            .map(|p| p[1 - axis])
                            .fold(f32::NEG_INFINITY, f32::max);
                        let clamped = cross.clamp(low, high);
                        // Inverting a subpixel birth sliver amplifies cancellation in its tile edges.
                        ((cross - clamped).abs() * stretch[1 - axis] < 0.001)
                            .then(|| interval(&child.sizes, axis, clamped))
                            .flatten()
                    })
                    .expect("feasible child cross-section")
            };
            let (a0, a1) = child_interval(a);
            let (b0, b1) = child_interval(b);
            let (a0, a1, b0, b1) = (a0 * pa, a1 * pa, b0 * pb, b1 * pb);
            let available = size[axis] - states[index].gap;
            let low = a0.max(available - b1);
            let high = a1.min(available - b0);
            assert!(low <= high + 0.02, "infeasible split: {low} > {high}");
            let share = available * a.weight / (a.weight + b.weight).max(1e-9);
            let extent = share.clamp(low.min(high), high);
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
            allocate(nodes, first, near, states, sa, tiles);
            allocate(nodes, second, far, states, sb, tiles);
        }
    }
}

fn tiles(
    nodes: &[Node],
    weights: &[f32],
    presence: &[f32],
    gap: Vec2,
) -> Option<Vec<Option<Tile>>> {
    let mut states: Vec<_> = nodes
        .iter()
        .map(|_| Feasible {
            sizes: Vec::new(),
            weight: 0.0,
            presence: 0.0,
            gap: 0.0,
        })
        .collect();
    feasibility(nodes, 0, weights, presence, gap, &mut states);
    allocated(nodes, weights.len(), &states)
}

fn allocated(
    nodes: &[Node],
    repositories: usize,
    states: &[Feasible],
) -> Option<Vec<Option<Tile>>> {
    let size = Tile::viewport().pixels();
    let (low, high) = interval(&states[0].sizes, 0, size.y)?;
    if size.x < low - 0.01 || size.x > high + 0.01 {
        return None;
    }
    let mut output = vec![None; repositories];
    allocate(nodes, 0, Tile::viewport(), states, Vec2::ONE, &mut output);
    Some(output)
}

fn insert(nodes: &mut Vec<Node>, repository: usize, weights: &[f32], aspects: &[f32]) {
    if nodes.is_empty() {
        nodes.push(Node::Repository(repository));
        return;
    }
    let mut best = None;
    let presence: Vec<_> = weights.iter().map(|&w| f32::from(w > 0.0)).collect();
    let gap = gutters(presence.iter().sum());
    let mut cached = vec![Feasible::default(); nodes.len()];
    feasibility(nodes, 0, weights, &presence, gap, &mut cached);
    let mut leaf = vec![Feasible::default()];
    update(
        &[Node::Repository(repository)],
        0,
        weights,
        &presence,
        gap,
        &mut leaf,
    );
    let mut parents = vec![None; nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        if let Node::Split { first, second, .. } = node {
            parents[*first] = Some(index);
            parents[*second] = Some(index);
        }
    }
    for host in 0..nodes.len() {
        for axis in 0..2 {
            let mut candidate = nodes.clone();
            let first = candidate.len();
            candidate.push(candidate[host].clone());
            candidate.push(Node::Repository(repository));
            candidate[host] = Node::Split {
                axis,
                first,
                second: first + 1,
            };
            let mut states = cached.clone();
            states.push(cached[host].clone());
            states.push(leaf[0].clone());
            let mut ancestor = Some(host);
            while let Some(index) = ancestor {
                update(&candidate, index, weights, &presence, gap, &mut states);
                ancestor = parents[index];
            }
            let Some(placement) = allocated(&candidate, weights.len(), &states) else {
                continue;
            };
            let total: f32 = weights.iter().sum();
            let score: f32 = placement
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
                .sum();
            if best.as_ref().is_none_or(|(cost, _)| score < *cost - 1e-5) {
                best = Some((score, candidate));
            }
        }
    }
    *nodes = best
        .expect("readable insertion fits the supported repository count")
        .1;
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

#[derive(Clone, Debug, Default)]
pub(crate) struct Packing {
    nodes: Vec<Node>,
    regions: Arc<[Vec<RepositoryRegion>]>,
    samples: Arc<[Sample]>,
    rotated: Vec<bool>,
    placements: Arc<[OnceLock<Vec<Placement>>]>,
    zoom: Arc<[Vec<f32>]>,
}

fn rotations(
    nodes: &[Node],
    regions: &[Vec<RepositoryRegion>],
    samples: &[Sample],
) -> Vec<bool> {
    let mut scores = vec![[0.0; 2]; regions[0].len()];
    for tick in (0..regions.len()).step_by((regions.len() / 16).max(1)) {
        let sample = &samples[tick];
        let placement = tiles(
            nodes,
            &sample.weights,
            &sample.presence,
            gutters(sample.presence.iter().sum()),
        )
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
    #[cfg(test)]
    pub(crate) fn build(regions: &[Vec<RepositoryRegion>]) -> Self {
        Self::build_births(regions, false)
    }

    pub(crate) fn before_service(regions: &[Vec<RepositoryRegion>]) -> Self {
        Self::build_births(regions, true)
    }

    fn build_births(regions: &[Vec<RepositoryRegion>], before_service: bool) -> Self {
        let started = std::time::Instant::now();
        let Some(first) = regions.first().filter(|r| !r.is_empty()) else {
            return Self::default();
        };
        let births: Vec<_> = (0..first.len())
            .map(|i| regions.iter().position(|r| r[i].live).unwrap_or(usize::MAX))
            .collect();
        let mut order: Vec<_> = (0..first.len()).collect();
        order.sort_by_key(|&i| (births[i], i));
        let mut nodes = Vec::new();
        let mut born = vec![false; first.len()];
        for (position, &i) in order.iter().enumerate() {
            born[i] = true;
            let begin = births[i].min(regions.len() - 1);
            let end = order
                .get(position + 1)
                .map_or(regions.len(), |&j| births[j].min(regions.len()))
                .max(begin + 1);
            let mut weights = vec![0.0; first.len()];
            let mut aspects = vec![1.0; first.len()];
            for (j, _) in born.iter().enumerate().filter(|(_, live)| **live) {
                weights[j] = regions[begin..end]
                    .iter()
                    .map(|r| r[j].files.max(1.0).sqrt())
                    .sum::<f32>()
                    / crate::model::count_u32(
                        u32::try_from(end - begin).expect("bounded timeline"),
                    );
                let size = regions[end - 1][j].high - regions[end - 1][j].low;
                aspects[j] = (size.x / size.y.max(0.01)).max(0.01);
            }
            insert(&mut nodes, i, &weights, &aspects);
        }
        let alpha = 1.0 - (-crate::model::tick_seconds() / 0.25).exp();
        let mut weights = vec![1.0; first.len()];
        let earliest = births.iter().copied().min().unwrap_or(0);
        let samples: Vec<_> = regions
            .iter()
            .enumerate()
            .map(|(tick, row)| {
                for (weight, r) in weights.iter_mut().zip(row) {
                    *weight = weight.lerp(r.files.max(1.0).sqrt(), alpha);
                }
                let mut presence: Vec<_> = row
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        let birth = births[i];
                        let begin = if before_service {
                            birth.saturating_sub(45)
                        } else {
                            birth
                        };
                        if tick < begin || birth == usize::MAX {
                            return 0.0;
                        }
                        if births[i] == earliest {
                            return 1.0;
                        }
                        let elapsed = crate::model::tick_time(
                            u64::try_from(tick.saturating_sub(begin))
                                .expect("bounded timeline"),
                        );
                        let transition = if before_service {
                            crate::model::tick_time(
                                u64::try_from(birth - begin).expect("bounded birth"),
                            )
                            .max(crate::model::tick_seconds())
                        } else {
                            BIRTH_SECONDS
                        };
                        let u = (elapsed / transition).min(1.0);
                        u * u * (3.0 - 2.0 * u)
                    })
                    .collect();
                if presence.iter().all(|&p| p == 0.0) {
                    presence[order[0]] = 1.0;
                }
                Sample {
                    weights: weights.clone(),
                    presence,
                }
            })
            .collect();
        let rotated = rotations(&nodes, regions, &samples);
        eprintln!(
            "treemap_plan_seconds={:.6} samples={} repositories={}",
            started.elapsed().as_secs_f64(),
            regions.len(),
            first.len()
        );
        Self {
            nodes,
            regions: regions.to_vec().into(),
            samples: samples.into(),
            rotated,
            placements: (0..regions.len()).map(|_| OnceLock::new()).collect(),
            zoom: Arc::default(),
        }
        .plan_zoom()
    }

    fn plan_zoom(mut self) -> Self {
        let mut scales: Vec<Vec<_>> = (0..self.regions.len())
            .map(|tick| {
                self.fit(tick, None)
                    .iter()
                    .zip(&self.regions[tick])
                    .map(|(p, r)| if r.live { p.scale } else { 0.0 })
                    .collect()
            })
            .collect();
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
        let tick = usize::try_from(tick)
            .unwrap_or(usize::MAX)
            .min(self.samples.len().checked_sub(1)?);
        let sample = &self.samples[tick];
        let computed;
        let placement = if presence.is_none() {
            self.placements[tick].get_or_init(|| self.fit(tick, None))
        } else {
            computed = self.fit(tick, presence);
            &computed
        };
        let mut regions = self.regions[tick].clone();
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
            r.live = tile.is_some();
            r.tile = tile;
            r.files = sample.weights[i]
                * (sample.presence[i]
                    * presence
                        .and_then(|v| v.get(i))
                        .copied()
                        .unwrap_or(1.0)
                        .clamp(0.0, 1.0));
            r.rotated = self.rotated[i];
            r.outline.clear();
            origins.push(origin);
            scales.push(scale);
        }
        Some((regions, origins, scales))
    }

    fn fit(&self, tick: usize, presence: Option<&[f32]>) -> Vec<Placement> {
        let sample = &self.samples[tick];
        let presence: Vec<_> = sample
            .presence
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
        let placement = tiles(
            &self.nodes,
            &sample.weights,
            &presence,
            gutters(presence.iter().sum()),
        )
        .expect("presence-scaled tiles remain feasible");
        self.regions[tick]
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let turn = |p: Vec2| if self.rotated[i] { p.perp() } else { p };
                let shape: Vec<_> = r.outline.iter().copied().map(turn).collect();
                let tile = placement[i];
                // A fixed screen anchor avoids switching between equal-area fit vertices.
                let fitted = tile.map(|tile| {
                    fit::anchored(tile, &shape, r.height, Vec2::splat(0.5))
                });
                let raw_scale = fitted.map_or(0.0, |f| f.scale);
                let scale = self
                    .zoom
                    .get(tick)
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

#[cfg(test)]
mod tests {
    use super::*;

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
                assert_eq!((a.low, a.high, a.files), (b.low, b.high, b.files));
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
                            files: f32::from(1 + (i * 37 + tick / 30) % 200),
                            tile: None,
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
    fn gutter_budget_shrinks_continuously_above_twelve_repositories() {
        assert_eq!(gutters(5.0), Vec2::new(16.0, 32.0));
        assert_eq!(gutters(12.0), Vec2::new(16.0, 32.0));
        assert_eq!(gutters(21.0), Vec2::new(14.0, 24.0));
        assert_eq!(gutters(30.0), Vec2::new(12.0, 16.0));
        assert_eq!(gutters(31.0), gutters(30.0));
    }

    #[test]
    fn thirty_tiles_partition_with_gutters_and_remain_readable_and_continuous() {
        let rows = fixture();
        let packing = Packing::build(&rows);
        let mut previous = vec![None; 30];
        for (tick, sample) in packing.samples.iter().enumerate() {
            let placement = tiles(
                &packing.nodes,
                &sample.weights,
                &sample.presence,
                gutters(sample.presence.iter().sum()),
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
        let replay = Packing::build(&rows);
        for tick in [0, 60, 700, 1599] {
            let (_, origins, scales) = packing.sample(tick).unwrap();
            let (_, replay_origins, replay_scales) = replay.sample(tick).unwrap();
            assert_eq!((origins, scales), (replay_origins, replay_scales));
        }
    }

    #[test]
    fn archive_presence_scales_clamps_and_returns_the_whole_view_to_a_singleton() {
        let packing = Packing::build(&fixture());
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
}
