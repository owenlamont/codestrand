use std::sync::Arc;

use bevy::math::{FloatExt, Vec2};

use super::RepositoryRegion;
use crate::model::framing;

const EASE_SECONDS: f32 = 0.06;
mod fit;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Tile {
    pub low: Vec2,
    pub high: Vec2,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(index: u16, live: bool) -> RepositoryRegion {
        RepositoryRegion {
            label: index.to_string(),
            low: Vec2::new(-15.0, -4.0),
            high: Vec2::new(15.0, 4.0),
            live,
            files: f32::from(index + 1),
            tile: None,
            outline: super::super::outline::rectangle(
                Vec2::new(-15.0, -4.0),
                Vec2::new(15.0, 4.0),
            ),
            height: 0.105,
            rotated: false,
        }
    }

    #[test]
    fn content_fit_sets_tile_areas_and_uniform_scales_preserve_local_shapes() {
        let packing = Packing::build(&[vec![region(0, true), region(2, true)]]);
        let (regions, _, scales) = packing.sample(0).unwrap();
        let tiles: Vec<_> = regions.iter().map(|r| r.tile.unwrap()).collect();
        assert!(tiles.iter().all(|t| t.area() > 0.0));
        assert!(
            (tiles.iter().map(|t| t.area()).sum::<f32>() - Tile::viewport().area())
                .abs()
                < 1e-6
        );
        for (r, scale) in regions.iter().zip(scales) {
            assert!(
                ((r.high - r.low) / scale
                    - (if r.rotated {
                        Vec2::new(8.0, 30.0)
                    } else {
                        Vec2::new(30.0, 8.0)
                    }))
                .length()
                    < 1e-4
            );
            let tile = r.tile.unwrap();
            for point in r.corners() {
                let screen =
                    framing::project_ground(super::super::region_coordinates(point));
                assert!(screen.cmpge(tile.low - Vec2::splat(1e-6)).all());
                assert!(screen.cmple(tile.high + Vec2::splat(1e-6)).all());
            }
        }
    }

    #[test]
    fn content_shape_changes_the_split_instead_of_always_preferring_square_tiles() {
        let axis = |size| {
            let mut regions = vec![region(0, true), region(1, true)];
            for r in &mut regions {
                r.low = Vec2::ZERO;
                r.high = size;
                r.files = 1.0;
            }
            let packing = Packing::build(&[regions]);
            let Some(Node::Split { axis, .. }) = packing.root else {
                panic!("two repositories split");
            };
            (axis, packing.rotated)
        };
        assert_ne!(axis(Vec2::new(100.0, 4.0)), axis(Vec2::new(4.0, 100.0)));
    }

    #[test]
    fn births_and_archive_fades_reclaim_the_entire_view_without_changing_slots() {
        let rows: Vec<_> = (0..240)
            .map(|tick| vec![region(0, true), region(2, tick >= 60)])
            .collect();
        let packing = Packing::build(&rows);
        assert_eq!(
            packing.sample(59).unwrap().0[0].tile,
            Some(Tile::viewport())
        );
        assert_eq!(
            packing.sample(60).unwrap().0[0].tile,
            Some(Tile::viewport())
        );
        let mut previous = packing.sample(60).unwrap().0[0].tile.unwrap();
        for tick in 61..240 {
            let (regions, _, _) = packing.sample(tick).unwrap();
            let now = regions[0].tile.unwrap();
            assert!(
                now.low.distance(previous.low) + now.high.distance(previous.high)
                    < 0.25
            );
            assert!(
                crate::model::treemap::largest_empty(
                    &regions.iter().filter_map(|r| r.tile).collect::<Vec<_>>()
                ) < 0.034
            );
            previous = now;
        }
        let (regions, _, _) = packing
            .sample_with_presence(239, Some(&[1.0, 0.0]))
            .unwrap();
        assert_eq!(regions[0].tile, Some(Tile::viewport()));
        assert!(!regions[1].live);
        assert!(Packing::default().sample(0).is_none());
    }
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

    fn split(self, axis: usize, ratio: f32) -> (Self, Self) {
        let boundary = self.low[axis] + (self.high[axis] - self.low[axis]) * ratio;
        let mut first = self;
        let mut second = self;
        first.high[axis] = boundary;
        second.low[axis] = boundary;
        (first, second)
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

    fn fit(self, size: Vec2) -> (Vec2, f32) {
        let points = super::outline::rectangle(-size * 0.5, size * 0.5);
        let fitted = fit::best(self, &points, 0.105);
        (fitted.origin, fitted.scale)
    }
}
#[derive(Clone, Debug)]
enum Node {
    Repository(usize),
    Split {
        axis: usize,
        index: usize,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl Node {
    fn number(&mut self, next: &mut usize) {
        if let Self::Split {
            index,
            first,
            second,
            ..
        } = self
        {
            *index = *next;
            *next += 1;
            first.number(next);
            second.number(next);
        }
    }

    fn presence(&self, weights: &[f32]) -> f32 {
        match self {
            Self::Repository(i) => weights[*i],
            Self::Split { first, second, .. } => {
                first.presence(weights).max(second.presence(weights))
            }
        }
    }

    fn allocate(
        &self,
        tile: Tile,
        presence: &[f32],
        ratios: &[f32],
        output: &mut [Option<Tile>],
    ) {
        match self {
            Self::Repository(i) => {
                output[*i] = Some(tile);
            }
            Self::Split {
                axis,
                index,
                first,
                second,
            } => {
                let a = ratios[*index] * first.presence(presence);
                let b = (1.0 - ratios[*index]) * second.presence(presence);
                let (a_tile, b_tile) =
                    tile.split(*axis, if a + b > 0.0 { a / (a + b) } else { 0.5 });
                first.allocate(a_tile, presence, ratios, output);
                second.allocate(b_tile, presence, ratios, output);
            }
        }
    }

    fn cost(
        &self,
        tile: Tile,
        shapes: &[Vec<Vec2>],
        rows: &[RepositoryRegion],
        presence: &[f32],
        weights: &[f32],
        ratios: &[f32],
    ) -> f32 {
        let mut tiles = vec![None; shapes.len()];
        self.allocate(tile, presence, ratios, &mut tiles);
        tiles
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                t.filter(|t| presence[i] > 0.001 && t.area() > 1e-9)
                    .map(|t| {
                        -fit::best(t, &shapes[i], rows[i].height).fill.max(1e-5).ln()
                            * weights[i].sqrt()
                            * presence[i]
                    })
            })
            .sum()
    }

    fn optimise(
        &self,
        tile: Tile,
        shapes: &[Vec<Vec2>],
        rows: &[RepositoryRegion],
        presence: &[f32],
        weights: &[f32],
        ratios: &mut [f32],
    ) {
        if let Self::Split {
            axis,
            index,
            first,
            second,
        } = self
        {
            let previous = ratios[*index];
            let total = first.weight(weights) + second.weight(weights);
            let weighted = first.weight(weights) / total.max(1e-9);
            let mut best = (
                self.cost(tile, shapes, rows, presence, weights, ratios),
                previous,
            );
            for candidate in [0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, weighted] {
                ratios[*index] = candidate;
                let cost = self.cost(tile, shapes, rows, presence, weights, ratios);
                if cost < best.0 - 0.005 {
                    best = (cost, candidate);
                }
            }
            ratios[*index] = best.1;
            let a = best.1 * first.presence(presence);
            let b = (1.0 - best.1) * second.presence(presence);
            let (near, far) =
                tile.split(*axis, if a + b > 0.0 { a / (a + b) } else { 0.5 });
            first.optimise(near, shapes, rows, presence, weights, ratios);
            second.optimise(far, shapes, rows, presence, weights, ratios);
        }
    }
    fn weight(&self, weights: &[f32]) -> f32 {
        match self {
            Self::Repository(index) => weights[*index],
            Self::Split { first, second, .. } => {
                first.weight(weights) + second.weight(weights)
            }
        }
    }

    fn place(&self, tile: Tile, weights: &[f32], result: &mut [Option<Tile>]) {
        match self {
            Self::Repository(index) => {
                result[*index] = Some(tile);
            }
            Self::Split {
                axis,
                first,
                second,
                ..
            } => {
                let a = first.weight(weights);
                let b = second.weight(weights);
                let ratio = if a + b > 0.0 { a / (a + b) } else { 0.5 };
                let (near, far) = tile.split(*axis, ratio);
                first.place(near, weights, result);
                second.place(far, weights, result);
            }
        }
    }
}

fn tree(order: &[usize], tile: Tile, reference: &[RepositoryRegion]) -> (Node, f32) {
    if order.len() == 1 {
        let index = order[0];
        let size = reference[index].high - reference[index].low;
        let (centre, scale) = tile.fit(size);
        let content = framing::project_rectangle(
            centre - size * (0.5 * scale),
            centre + size * (0.5 * scale),
        );
        let cost =
            reference[index].files.max(1.0) * (1.0 - content / tile.area().max(1e-12));
        return (Node::Repository(index), cost);
    }
    let split = order.len() / 2;
    let weight = |indices: &[usize]| {
        indices
            .iter()
            .map(|&i| reference[i].files.max(1.0))
            .sum::<f32>()
    };
    let near_weight = weight(&order[..split]);
    let ratio = near_weight / (near_weight + weight(&order[split..]));
    let candidate = |axis| {
        let (a, b) = tile.split(axis, ratio);
        let (first, first_cost) = tree(&order[..split], a, reference);
        let (second, second_cost) = tree(&order[split..], b, reference);
        (
            Node::Split {
                axis,
                index: 0,
                first: Box::new(first),
                second: Box::new(second),
            },
            first_cost + second_cost,
        )
    };
    let near_far = candidate(1);
    let side_by_side = candidate(0);
    if near_far.1 <= side_by_side.1 {
        near_far
    } else {
        side_by_side
    }
}
#[derive(Clone, Debug)]
struct Placement {
    low: Vec2,
    high: Vec2,
    origin: Vec2,
    scale: f32,
    tile: Option<Tile>,
    files: f32,
    shape: Box<[Vec2]>,
    height: f32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Packing {
    root: Option<Node>,
    samples: Arc<[Vec<Placement>]>,
    labels: Vec<String>,
    rotated: Vec<bool>,
}

impl Packing {
    pub(crate) fn build(regions: &[Vec<RepositoryRegion>]) -> Self {
        let started = std::time::Instant::now();
        let Some(first) = regions.first().filter(|row| !row.is_empty()) else {
            return Self::default();
        };
        let (root, rotated, nodes) = planned_tree(regions, first);
        let births: Vec<_> = (0..first.len())
            .map(|i| regions.iter().position(|r| r[i].live).unwrap_or(usize::MAX))
            .collect();
        let first_birth = births.iter().copied().min().unwrap_or(0);
        let alpha = 1.0 - (-crate::model::tick_seconds() / EASE_SECONDS).exp();
        let mut ratios = vec![0.5; nodes];
        let mut goals = ratios.clone();
        let mut anchors = vec![Vec2::splat(0.5); first.len()];
        let mut samples = Vec::with_capacity(regions.len());
        let mut previous_shapes = vec![Vec::new(); first.len()];
        for (tick, row) in regions.iter().enumerate() {
            let mut presence = vec![0.0; row.len()];
            let weights: Vec<_> = row.iter().map(|r| r.files.max(1.0)).collect();
            let shapes: Vec<_> = row
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    presence[i] = birth_presence(r.live, tick, births[i], first_birth);
                    anticipated(
                        regions,
                        tick,
                        i,
                        &previous_shapes[i],
                        alpha,
                        rotated[i],
                    )
                })
                .collect();
            previous_shapes.clone_from(&shapes);
            if tick.is_multiple_of(6) {
                root.optimise(
                    Tile::viewport(),
                    &shapes,
                    row,
                    &presence,
                    &weights,
                    &mut goals,
                );
            }
            for (now, &goal) in ratios.iter_mut().zip(&goals) {
                *now = now.lerp(goal, alpha);
            }
            let mut tiles = vec![None; row.len()];
            root.allocate(Tile::viewport(), &presence, &ratios, &mut tiles);
            let anchor_alpha = if tick == 0 { 1.0 } else { alpha };
            let placements = row
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let tile = tiles[i].filter(|_| presence[i] > 0.0);
                    let (origin, scale) = if let Some(tile) = tile {
                        let target = fit::best(tile, &shapes[i], r.height);
                        anchors[i] = anchors[i].lerp(target.anchor, anchor_alpha);
                        let fitted =
                            fit::anchored(tile, &shapes[i], r.height, anchors[i]);
                        (fitted.origin, fitted.scale)
                    } else {
                        (Vec2::ZERO, 0.0)
                    };
                    Placement {
                        low: if rotated[i] {
                            Vec2::new(-r.high.y, r.low.x)
                        } else {
                            r.low
                        } * scale
                            + origin,
                        high: if rotated[i] {
                            Vec2::new(-r.low.y, r.high.x)
                        } else {
                            r.high
                        } * scale
                            + origin,
                        origin,
                        scale,
                        tile,
                        files: weights[i] * presence[i],
                        shape: shapes[i].clone().into_boxed_slice(),
                        height: r.height,
                    }
                })
                .collect();
            samples.push(placements);
        }
        eprintln!(
            "treemap_plan_seconds={:.6} samples={} repositories={}",
            started.elapsed().as_secs_f64(),
            regions.len(),
            first.len()
        );
        Self {
            root: Some(root),
            samples: samples.into(),
            labels: first.iter().map(|r| r.label.clone()).collect(),
            rotated,
        }
    }

    fn placed(
        &self,
        tick: u64,
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        let frame = self.samples.get(
            usize::try_from(tick)
                .unwrap_or(usize::MAX)
                .min(self.samples.len().checked_sub(1)?),
        )?;
        let regions = frame
            .iter()
            .enumerate()
            .map(|(i, p)| RepositoryRegion {
                label: self.labels[i].clone(),
                low: p.low,
                high: p.high,
                live: p.tile.is_some(),
                files: p.files,
                tile: p.tile,
                outline: Vec::new(),
                height: 0.0,
                rotated: self.rotated[i],
            })
            .collect();
        Some((
            regions,
            frame.iter().map(|p| p.origin).collect(),
            frame.iter().map(|p| p.scale).collect(),
        ))
    }

    pub(crate) fn sample(
        &self,
        tick: u64,
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        self.sample_with_presence(tick, None)
    }

    /// Archive fades release tile area without changing the fixed split tree.
    pub(crate) fn sample_with_presence(
        &self,
        tick: u64,
        presence: Option<&[f32]>,
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        let (mut regions, mut origins, mut scales) = self.placed(tick)?;
        if let Some(presence) = presence {
            if presence.iter().all(|&p| p >= 1.0) {
                return Some((regions, origins, scales));
            }
            let frame = &self.samples[usize::try_from(tick)
                .unwrap_or(usize::MAX)
                .min(self.samples.len() - 1)];
            let weights: Vec<_> = regions
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    r.tile.map_or(0.0, Tile::area)
                        * presence.get(i).copied().unwrap_or(1.0).clamp(0.0, 1.0)
                })
                .collect();
            let mut tiles = vec![None; regions.len()];
            self.root
                .as_ref()?
                .place(Tile::viewport(), &weights, &mut tiles);
            for (i, r) in regions.iter_mut().enumerate() {
                let old_scale = scales[i].max(f32::EPSILON);
                let low = (r.low - origins[i]) / old_scale;
                let high = (r.high - origins[i]) / old_scale;
                let tile = tiles[i].filter(|_| weights[i] > 0.0);
                let fitted =
                    tile.map(|tile| fit::best(tile, &frame[i].shape, frame[i].height));
                scales[i] = fitted.map_or(0.0, |f| f.scale);
                origins[i] = fitted.map_or(Vec2::ZERO, |f| f.origin);
                r.low = low * scales[i] + origins[i];
                r.high = high * scales[i] + origins[i];
                r.tile = tile;
                r.live = tile.is_some();
            }
        }
        Some((regions, origins, scales))
    }
}

fn anticipated(
    regions: &[Vec<RepositoryRegion>],
    tick: usize,
    index: usize,
    previous: &[Vec2],
    alpha: f32,
    rotated: bool,
) -> Vec<Vec2> {
    let normals = super::outline::NORMALS;
    let r = &regions[tick][index];
    let turn = |p: Vec2| if rotated { p.perp() } else { p };
    let base: [f32; 8] = normals.map(|n| {
        r.outline
            .iter()
            .map(|p| turn(*p).dot(n))
            .fold(f32::NEG_INFINITY, f32::max)
    });
    let mut supports = base;
    for delta in 1..=9_u16 {
        let future =
            &regions[(tick + usize::from(delta)).min(regions.len() - 1)][index];
        let u = 1.0 - f32::from(delta) / 9.0;
        let weight = u * u * (3.0 - 2.0 * u);
        for i in 0..8 {
            let next = future
                .outline
                .iter()
                .map(|p| turn(*p).dot(normals[i]))
                .fold(f32::NEG_INFINITY, f32::max);
            supports[i] = supports[i].max(base[i].lerp(next, weight));
        }
    }
    if !previous.is_empty() {
        for i in 0..8 {
            let before = previous
                .iter()
                .map(|p| p.dot(normals[i]))
                .fold(f32::NEG_INFINITY, f32::max);
            supports[i] = supports[i].max(before.lerp(supports[i], alpha));
        }
    }
    super::outline::from_supports(supports)
}

fn reference_shapes(
    regions: &[Vec<RepositoryRegion>],
    first: &[RepositoryRegion],
) -> Vec<RepositoryRegion> {
    let mut reference = first.to_vec();
    for (i, r) in reference.iter_mut().enumerate() {
        let mut size = Vec2::ZERO;
        let mut weight = 0.0;
        for row in regions.iter().step_by((regions.len() / 32).max(1)) {
            if row[i].live {
                size += (row[i].high - row[i].low) * row[i].files.max(1.0);
                weight += row[i].files.max(1.0);
            }
        }
        r.files = regions.iter().map(|row| row[i].files).fold(0.0, f32::max);
        r.low = Vec2::ZERO;
        r.high = if weight > 0.0 {
            size / weight
        } else {
            Vec2::ONE
        };
    }
    reference
}

fn birth_presence(live: bool, tick: usize, birth: usize, first_birth: usize) -> f32 {
    if !live {
        return 0.0;
    }
    if birth == first_birth {
        return 1.0;
    }
    let elapsed = u64::try_from(tick.saturating_sub(birth)).expect("bounded timeline");
    let u = (crate::model::tick_time(elapsed) / 0.15).min(1.0);
    u * u * (3.0 - 2.0 * u)
}

fn rotations(root: &Node, regions: &[Vec<RepositoryRegion>]) -> Vec<bool> {
    let mut scores = vec![[0.0; 2]; regions[0].len()];
    for row in regions.iter().step_by((regions.len() / 32).max(1)) {
        let weights: Vec<_> = row
            .iter()
            .map(|r| if r.live { r.files.max(1.0) } else { 0.0 })
            .collect();
        let mut tiles = vec![None; row.len()];
        root.place(Tile::viewport(), &weights, &mut tiles);
        for (i, r) in row
            .iter()
            .enumerate()
            .filter(|(i, r)| r.live && weights[*i] > 0.0)
        {
            let tile = tiles[i].expect("repository slot");
            if tile.area() <= 0.0 {
                continue;
            }
            let turned: Vec<_> = r.outline.iter().map(|p| p.perp()).collect();
            scores[i][0] += tile.area() * fit::best(tile, &r.outline, r.height).fill;
            scores[i][1] += tile.area() * fit::best(tile, &turned, r.height).fill;
        }
    }
    scores.into_iter().map(|s| s[1] > s[0] * 1.02).collect()
}

fn planned_tree(
    regions: &[Vec<RepositoryRegion>],
    first: &[RepositoryRegion],
) -> (Node, Vec<bool>, usize) {
    let mut reference = reference_shapes(regions, first);
    let mut order: Vec<_> = (0..first.len()).collect();
    order.sort_by(|&a, &b| {
        reference[b]
            .files
            .total_cmp(&reference[a].files)
            .then(a.cmp(&b))
    });
    let initial = tree(&order, Tile::viewport(), &reference).0;
    let rotated = rotations(&initial, regions);
    for (r, &rotate) in reference.iter_mut().zip(&rotated) {
        if rotate {
            let size = r.high - r.low;
            r.low = Vec2::ZERO;
            r.high = Vec2::new(size.y, size.x);
        }
    }
    let mut root = tree(&order, Tile::viewport(), &reference).0;
    let mut nodes = 0;
    root.number(&mut nodes);
    (root, rotated, nodes)
}
