use std::sync::Arc;

use bevy::math::{FloatExt, Vec2};

use super::RepositoryRegion;
use crate::model::framing;

const EASE_SECONDS: f32 = 0.75;
const CONTENT_HEIGHT: f32 = 2.0;

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
        }
    }

    #[test]
    fn file_counts_set_tile_areas_and_uniform_scales_preserve_local_shapes() {
        let packing = Packing::build(&[vec![region(0, true), region(2, true)]]);
        let (regions, _, scales) = packing.sample(0).unwrap();
        let tiles: Vec<_> = regions.iter().map(|r| r.tile.unwrap()).collect();
        assert!((tiles[1].area() / tiles[0].area() - 3.0).abs() < 1e-5);
        assert!(
            (tiles.iter().map(|t| t.area()).sum::<f32>() - Tile::viewport().area())
                .abs()
                < 1e-6
        );
        for (r, scale) in regions.iter().zip(scales) {
            assert!(((r.high - r.low) / scale - Vec2::new(30.0, 8.0)).length() < 1e-4);
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
            axis
        };
        assert_eq!(axis(Vec2::new(100.0, 4.0)), 1);
        assert_eq!(axis(Vec2::new(4.0, 100.0)), 0);
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
                    < 0.08
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
        let centre = framing::unproject((self.low + self.high) * 0.5);
        let direction = crate::model::view_direction();
        let vertical = direction.y;
        let horizontal = direction.x.hypot(direction.z);
        let tangent = (framing::FOV * 0.5).tan();
        let slopes =
            Vec2::new(tangent * framing::VIEWPORT.x / framing::VIEWPORT.y, tangent);
        let depth = framing::DISTANCE - horizontal * centre.y;
        let projected = Vec2::new(centre.x, -vertical * centre.y);
        let mut scale = f32::INFINITY;
        let corners = crate::model::box_corners(
            bevy::math::Vec3::new(-size.x * 0.5, 0.0, -size.y * 0.5),
            bevy::math::Vec3::new(size.x * 0.5, CONTENT_HEIGHT, size.y * 0.5),
        );
        for point in corners {
            let depth_slope = -horizontal * point.z - vertical * point.y;
            let point_slope =
                Vec2::new(point.x, horizontal * point.y - vertical * point.z);
            for (limit, sign) in [(self.low, -1.0), (self.high, 1.0)] {
                let coefficient = sign * (point_slope - limit * slopes * depth_slope);
                let remaining = sign * (limit * slopes * depth - projected);
                let bound = Vec2::select(
                    coefficient.cmpgt(Vec2::ZERO),
                    remaining.max(Vec2::ZERO) / coefficient,
                    Vec2::splat(f32::INFINITY),
                );
                scale = scale.min(bound.min_element());
            }
        }
        (centre, scale.max(0.0) * 0.995)
    }
}

#[derive(Clone, Debug)]
enum Node {
    Repository(usize),
    Split {
        axis: usize,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl Node {
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
struct Sample {
    low: Vec<Vec2>,
    high: Vec<Vec2>,
    weights: Vec<f32>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Packing {
    root: Option<Node>,
    samples: Arc<[Sample]>,
    labels: Vec<String>,
}

impl Packing {
    pub(crate) fn build(regions: &[Vec<RepositoryRegion>]) -> Self {
        let started = std::time::Instant::now();
        let Some(first) = regions.first().filter(|row| !row.is_empty()) else {
            return Self::default();
        };
        let mut samples: Vec<_> = regions
            .iter()
            .map(|row| Sample {
                low: row.iter().map(|r| r.low).collect(),
                high: row.iter().map(|r| r.high).collect(),
                weights: row
                    .iter()
                    .map(|r| if r.live { r.files.max(1.0) } else { 0.0 })
                    .collect(),
            })
            .collect();
        let decay = (-crate::model::tick_seconds() / EASE_SECONDS).exp();
        let first_birth = regions.iter().position(|row| row.iter().any(|r| r.live));
        for i in 0..first.len() {
            if let Some(birth) = regions.iter().position(|row| row[i].live)
                && Some(birth) != first_birth
            {
                for (tick, sample) in samples.iter_mut().enumerate().skip(birth) {
                    let elapsed =
                        u64::try_from(tick - birth).expect("bounded history tick");
                    let u = (crate::model::tick_time(elapsed) / EASE_SECONDS).min(1.0);
                    sample.weights[i] *= u * u * (3.0 - 2.0 * u);
                }
            }
        }
        for tick in (0..samples.len().saturating_sub(1)).rev() {
            let (before, after) = samples.split_at_mut(tick + 1);
            for i in 0..first.len() {
                before[tick].low[i] = before[tick].low[i]
                    .min(before[tick].low[i].lerp(after[0].low[i], decay));
                before[tick].high[i] = before[tick].high[i]
                    .max(before[tick].high[i].lerp(after[0].high[i], decay));
            }
        }
        for tick in 1..samples.len() {
            let (before, after) = samples.split_at_mut(tick);
            for i in 0..first.len() {
                after[0].low[i] = after[0].low[i]
                    .min(after[0].low[i].lerp(before[tick - 1].low[i], decay));
                after[0].high[i] = after[0].high[i]
                    .max(after[0].high[i].lerp(before[tick - 1].high[i], decay));
                after[0].weights[i] =
                    after[0].weights[i].lerp(before[tick - 1].weights[i], decay);
            }
        }
        let mut reference = first.clone();
        for (i, r) in reference.iter_mut().enumerate() {
            r.files = regions.iter().map(|row| row[i].files).fold(0.0, f32::max);
            let mut size = Vec2::ZERO;
            let mut weight = 0.0;
            for row in regions.iter().step_by((regions.len() / 32).max(1)) {
                if row[i].live {
                    size += (row[i].high - row[i].low) * row[i].files.max(1.0);
                    weight += row[i].files.max(1.0);
                }
            }
            r.low = Vec2::ZERO;
            r.high = if weight > 0.0 {
                size / weight
            } else {
                Vec2::ONE
            };
        }
        let mut order: Vec<_> = (0..first.len()).collect();
        order.sort_by(|&a, &b| {
            reference[b]
                .files
                .total_cmp(&reference[a].files)
                .then(a.cmp(&b))
        });
        let result = Self {
            root: Some(tree(&order, Tile::viewport(), &reference).0),
            samples: samples.into(),
            labels: first.iter().map(|r| r.label.clone()).collect(),
        };
        eprintln!(
            "treemap_plan_seconds={:.6} samples={} repositories={}",
            started.elapsed().as_secs_f64(),
            regions.len(),
            first.len()
        );
        result
    }

    pub(crate) fn sample(
        &self,
        tick: u64,
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        self.sample_with_presence(tick, None)
    }

    /// Archive fade weights shrink fixed slots while neighbours reclaim their screen area.
    pub(crate) fn sample_with_presence(
        &self,
        tick: u64,
        presence: Option<&[f32]>,
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        let sample = self.samples.get(
            usize::try_from(tick)
                .unwrap_or(usize::MAX)
                .min(self.samples.len().checked_sub(1)?),
        )?;
        let mut weights = sample.weights.clone();
        if let Some(presence) = presence {
            for (weight, fade) in weights.iter_mut().zip(presence) {
                *weight *= fade.clamp(0.0, 1.0);
            }
        }
        let mut tiles = vec![None; weights.len()];
        self.root
            .as_ref()?
            .place(Tile::viewport(), &weights, &mut tiles);
        let mut offsets = vec![Vec2::ZERO; weights.len()];
        let mut scales = vec![0.0; weights.len()];
        let regions = self
            .labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                if let Some(tile) = tiles[i] {
                    let (centre, scale) = tile.fit(sample.high[i] - sample.low[i]);
                    scales[i] = if weights[i] > 0.0 { scale } else { 0.0 };
                    offsets[i] =
                        centre - (sample.high[i] + sample.low[i]) * (0.5 * scales[i]);
                }
                RepositoryRegion {
                    label: label.clone(),
                    low: sample.low[i] * scales[i] + offsets[i],
                    high: sample.high[i] * scales[i] + offsets[i],
                    live: weights[i] > 0.0,
                    files: weights[i],
                    tile: tiles[i].filter(|_| weights[i] > 0.0),
                }
            })
            .collect();
        Some((regions, offsets, scales))
    }
}
