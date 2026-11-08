use std::sync::Arc;

use bevy::math::{Vec2, Vec3};

use super::RepositoryRegion;
use crate::model::framing;

const GAP: f32 = 1.0;
const GROW_SECONDS: f32 = 2.0;
const SEARCH_NODES: usize = 300_000;

fn permutations(order: &mut [usize], start: usize, result: &mut Vec<Vec<usize>>) {
    if start == order.len() {
        if !result.iter().any(|entry| entry == order) {
            result.push(order.to_vec());
        }
        return;
    }
    for index in start..order.len() {
        order.swap(start, index);
        permutations(order, start + 1, result);
        order.swap(start, index);
    }
}

fn orders(sizes: &[Vec2]) -> Vec<Vec<usize>> {
    let mut result = Vec::new();
    for axis in 0..4 {
        let mut order: Vec<_> = (0..sizes.len()).collect();
        let key = |index: usize| match axis {
            0 => sizes[index].element_product(),
            1 => sizes[index].x,
            2 => sizes[index].y,
            _ => sizes[index].max_element(),
        };
        order.sort_by(|&a, &b| key(b).total_cmp(&key(a)).then(a.cmp(&b)));
        if !result.contains(&order) {
            result.push(order);
        }
    }
    if sizes.len() <= 5 {
        permutations(&mut (0..sizes.len()).collect::<Vec<_>>(), 0, &mut result);
    }
    result
}

fn next_position(
    index: usize,
    sizes: &[Vec2],
    positions: &[Vec2],
    done: &[usize],
    limit: f32,
    nodes: &mut usize,
) -> Option<Vec2> {
    let mut choice = Vec2::splat(f32::INFINITY);
    for edge in 0..=done.len() {
        if *nodes == SEARCH_NODES {
            return None;
        }
        *nodes += 1;
        let x = if edge == 0 {
            0.0
        } else {
            let before = done[edge - 1];
            positions[before].x + sizes[before].x + GAP
        };
        if x + sizes[index].x > limit + 0.0001 {
            continue;
        }
        let mut y = 0.0_f32;
        loop {
            let next = done
                .iter()
                .filter(|&&before| {
                    x < positions[before].x + sizes[before].x + GAP - 0.0001
                        && x + sizes[index].x + GAP > positions[before].x + 0.0001
                        && y < positions[before].y + sizes[before].y + GAP - 0.0001
                        && y + sizes[index].y + GAP > positions[before].y + 0.0001
                })
                .map(|&before| positions[before].y + sizes[before].y + GAP)
                .fold(y, f32::max);
            if next <= y {
                break;
            }
            y = next;
        }
        if y.total_cmp(&choice.y).then(x.total_cmp(&choice.x)).is_lt() {
            choice = Vec2::new(x, y);
        }
    }
    Some(choice)
}

fn bottom_left(
    sizes: &[Vec2],
    order: &[usize],
    limit: f32,
    nodes: &mut usize,
) -> Option<Vec<Vec2>> {
    let mut positions = vec![Vec2::ZERO; sizes.len()];
    let mut done = Vec::with_capacity(sizes.len());
    for &index in order {
        positions[index] =
            next_position(index, sizes, &positions, &done, limit, nodes)?;
        done.push(index);
    }
    Some(positions)
}

#[cfg(test)]
fn coverage(regions: &[RepositoryRegion]) -> f32 {
    let focus = framing::centre(regions);
    let distance = framing::distance(regions, focus, framing::FOV, framing::VIEWPORT);
    coverage_at(regions, focus, distance)
}

fn coverage_at(regions: &[RepositoryRegion], focus: Vec3, distance: f32) -> f32 {
    let forward = -crate::model::view_direction();
    let right = forward.cross(Vec3::Y).normalize();
    let up = right.cross(forward);
    let tangent = (framing::FOV * 0.5).tan();
    regions
        .iter()
        .filter(|region| region.live)
        .map(|region| {
            let corners = [
                region.low,
                Vec2::new(region.high.x, region.low.y),
                region.high,
                Vec2::new(region.low.x, region.high.y),
            ];
            let points = corners.map(|corner| {
                let world = super::world_coordinates(corner);
                let offset = Vec3::new(world.x, 0.0, world.y) - focus;
                Vec2::new(
                    offset.dot(right) / (framing::VIEWPORT.x / framing::VIEWPORT.y),
                    offset.dot(up),
                ) / (tangent * (distance + offset.dot(forward)))
            });
            (0..4)
                .map(|i| points[i].perp_dot(points[(i + 1) % 4]))
                .sum::<f32>()
                .abs()
                * 0.125
        })
        .sum()
}

#[derive(Clone, Debug, Default)]
struct Constraints {
    axes: [Vec<usize>; 2],
    predecessors: [Vec<Vec<usize>>; 2],
}

impl Constraints {
    fn new(sizes: &[Vec2], positions: &[Vec2]) -> Self {
        let axes = std::array::from_fn(|axis| {
            let mut order: Vec<_> = (0..sizes.len()).collect();
            order.sort_by(|&a, &b| {
                positions[a][axis]
                    .total_cmp(&positions[b][axis])
                    .then(a.cmp(&b))
            });
            order
        });
        let mut predecessors =
            [vec![Vec::new(); sizes.len()], vec![Vec::new(); sizes.len()]];
        for a in 0..sizes.len() {
            for b in a + 1..sizes.len() {
                let (dimension, before, after) = if positions[a].x + sizes[a].x + GAP
                    <= positions[b].x + 0.0001
                {
                    (0, a, b)
                } else if positions[b].x + sizes[b].x + GAP <= positions[a].x + 0.0001 {
                    (0, b, a)
                } else if positions[a].y < positions[b].y {
                    (1, a, b)
                } else {
                    (1, b, a)
                };
                predecessors[dimension][after].push(before);
            }
        }
        Self { axes, predecessors }
    }

    fn positions(&self, extents: &[Vec2]) -> Vec<Vec2> {
        let mut positions = vec![Vec2::ZERO; extents.len()];
        for (dimension, order) in self.axes.iter().enumerate() {
            for &index in order {
                positions[index][dimension] = self.predecessors[dimension][index]
                    .iter()
                    .map(|&before| {
                        positions[before][dimension] + extents[before][dimension]
                    })
                    .fold(0.0_f32, f32::max);
            }
        }
        positions
    }
}

fn search(
    sizes: &[Vec2],
    labels: &[RepositoryRegion],
    mut evaluate: impl FnMut(&Constraints, &[RepositoryRegion]) -> f32,
) -> (Vec<Vec2>, usize) {
    let mut best = vec![Vec2::ZERO; sizes.len()];
    let mut x = 0.0;
    for (position, size) in best.iter_mut().zip(sizes) {
        position.x = x;
        x += size.x + GAP;
    }
    let mut score = f32::NEG_INFINITY;
    let mut priority = f32::INFINITY;
    let mut nodes = 0;
    let mut placed = labels.to_vec();
    for order in orders(sizes) {
        for transpose in [false, true] {
            let shapes: Vec<_> = sizes
                .iter()
                .map(|s| if transpose { Vec2::new(s.y, s.x) } else { *s })
                .collect();
            let minimum = shapes.iter().map(|s| s.x).fold(0.0_f32, f32::max);
            let maximum = shapes.iter().map(|s| s.x + GAP).sum::<f32>();
            for trial in 0..=32_u16 {
                let limit = minimum + (maximum - minimum) * f32::from(trial) / 32.0;
                let Some(mut positions) =
                    bottom_left(&shapes, &order, limit, &mut nodes)
                else {
                    return (best, nodes);
                };
                if transpose {
                    positions =
                        positions.into_iter().map(|p| Vec2::new(p.y, p.x)).collect();
                }
                let constraints = Constraints::new(sizes, &positions);
                positions = constraints.positions(
                    &sizes
                        .iter()
                        .map(|s| *s + Vec2::splat(GAP))
                        .collect::<Vec<_>>(),
                );
                for (index, region) in placed.iter_mut().enumerate() {
                    region.high = Vec2::new(
                        positions[index].x + sizes[index].x,
                        -positions[index].y,
                    );
                    region.low = region.high - sizes[index];
                    region.live = true;
                }
                let candidate = evaluate(&constraints, &placed);
                let front = positions
                    .iter()
                    .zip(sizes)
                    .map(|(p, s)| p.y * s.element_product())
                    .sum::<f32>();
                if candidate > score + 0.000_001
                    || ((candidate - score).abs() <= 0.000_001 && front < priority)
                {
                    score = candidate;
                    priority = front;
                    best = positions;
                }
            }
        }
    }
    (best, nodes)
}

#[derive(Clone, Debug)]
struct Sample {
    low: Vec<Vec2>,
    high: Vec<Vec2>,
    live: Vec<bool>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Packing {
    constraints: Constraints,
    samples: Arc<[Sample]>,
    labels: Vec<String>,
    births: Vec<usize>,
}

impl Packing {
    pub(crate) fn build(regions: &[Vec<RepositoryRegion>]) -> Self {
        let Some(first) = regions.first() else {
            return Self::default();
        };
        let mut samples: Vec<_> = regions
            .iter()
            .map(|row| Sample {
                low: row.iter().map(|r| r.low).collect(),
                high: row.iter().map(|r| r.high).collect(),
                live: row.iter().map(|r| r.live).collect(),
            })
            .collect();
        let births: Vec<_> = (0..first.len())
            .map(|index| {
                samples
                    .iter()
                    .position(|s| s.live[index])
                    .unwrap_or(usize::MAX)
            })
            .collect();
        let weight = (-crate::model::tick_seconds() / GROW_SECONDS).exp();
        for tick in (0..samples.len().saturating_sub(1)).rev() {
            let (before, after) = samples.split_at_mut(tick + 1);
            let now = &mut before[tick];
            let next = &after[0];
            for index in 0..first.len() {
                now.low[index] =
                    now.low[index].min(now.low[index].lerp(next.low[index], weight));
                now.high[index] =
                    now.high[index].max(now.high[index].lerp(next.high[index], weight));
            }
        }
        for tick in 1..samples.len() {
            let (before, after) = samples.split_at_mut(tick);
            let previous = &before[tick - 1];
            let now = &mut after[0];
            for index in 0..first.len() {
                now.low[index] = now.low[index].min(previous.low[index]);
                now.high[index] = now.high[index].max(previous.high[index]);
            }
        }
        let reservations = samples.last().expect("nonempty region history");
        let sizes: Vec<_> = reservations
            .high
            .iter()
            .zip(&reservations.low)
            .map(|(high, low)| high - low)
            .collect();
        let last = samples.len() - 1;
        let mut ticks: Vec<_> = (0..=32).map(|part| last * part / 32).collect();
        for &birth in &births {
            if birth <= last {
                ticks.push(birth.saturating_sub(1));
                ticks.push((birth + 120).min(last));
            }
        }
        ticks.sort_unstable();
        ticks.dedup();
        let mut packing = Self {
            constraints: Constraints::default(),
            samples: samples.into(),
            labels: first.iter().map(|r| r.label.clone()).collect(),
            births,
        };
        let positions = search(&sizes, first, |constraints, _| {
            packing.constraints = constraints.clone();
            let final_regions = packing
                .sample(u64::try_from(last).expect("history index fits tick range"))
                .expect("nonempty history")
                .0;
            let focus = framing::centre(&final_regions);
            let distance = ticks
                .iter()
                .map(|&tick| {
                    let regions = packing
                        .sample(
                            u64::try_from(tick).expect("history index fits tick range"),
                        )
                        .expect("nonempty history")
                        .0;
                    framing::distance(&regions, focus, framing::FOV, framing::VIEWPORT)
                })
                .fold(24.0_f32, f32::max);
            coverage_at(&final_regions, focus, distance)
        })
        .0;
        packing.constraints = Constraints::new(&sizes, &positions);
        packing
    }

    pub(crate) fn sample(
        &self,
        tick: u64,
    ) -> Option<(Vec<RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        self.sample_with_presence(tick, None)
    }

    /// Fade weights release reserved space after a future archive fade without changing slots.
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
        let mut scales: Vec<_> = self
            .births
            .iter()
            .map(|&birth| {
                if birth == 0 {
                    return 1.0;
                }
                let elapsed =
                    tick.saturating_sub(u64::try_from(birth).unwrap_or(u64::MAX));
                let u =
                    (crate::model::tick_time(elapsed) / GROW_SECONDS).clamp(0.0, 1.0);
                u * u * (3.0 - 2.0 * u)
            })
            .collect();
        if let Some(presence) = presence {
            for (scale, weight) in scales.iter_mut().zip(presence) {
                *scale *= weight.clamp(0.0, 1.0);
            }
        }
        let mut regions: Vec<_> = self
            .labels
            .iter()
            .enumerate()
            .map(|(index, label)| RepositoryRegion {
                label: label.clone(),
                low: sample.low[index] * scales[index],
                high: sample.high[index] * scales[index],
                live: sample.live[index] && scales[index] > 0.0,
            })
            .collect();
        let mut offsets = vec![Vec2::ZERO; regions.len()];
        let extents: Vec<_> = regions
            .iter()
            .enumerate()
            .map(|(index, region)| {
                if region.live {
                    region.high - region.low + Vec2::splat(GAP * scales[index])
                } else {
                    Vec2::ZERO
                }
            })
            .collect();
        let positions = self.constraints.positions(&extents);
        for (index, region) in regions.iter_mut().enumerate() {
            offsets[index] = Vec2::new(positions[index].x, -positions[index].y)
                - Vec2::new(region.low.x, region.high.y);
            region.low += offsets[index];
            region.high += offsets[index];
        }
        Some((regions, offsets, scales))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacks_rectangles_beside_a_tall_repository() {
        let sizes = [
            Vec2::new(30.0, 60.0),
            Vec2::new(30.0, 29.0),
            Vec2::new(30.0, 30.0),
        ];
        let regions: Vec<_> = sizes
            .iter()
            .enumerate()
            .map(|(index, &size)| RepositoryRegion {
                label: index.to_string(),
                low: Vec2::ZERO,
                high: size,
                live: true,
            })
            .collect();
        let packing = Packing::build(&[regions]);
        let result = packing.sample(0).unwrap().0;
        let low = result.iter().map(|r| r.low).reduce(Vec2::min).unwrap();
        let high = result.iter().map(|r| r.high).reduce(Vec2::max).unwrap();
        assert!((high - low).element_product() < 4000.0);
        assert!(
            (result[1].low.x - result[2].low.x).abs() < 0.0001
                || (result[1].high.y - result[2].high.y).abs() < 0.0001
        );
    }

    #[test]
    fn thirty_repositories_are_deterministic_nonoverlapping_and_budgeted() {
        let sizes: Vec<_> = (0..30_u16)
            .map(|i| {
                Vec2::new(3.0 + f32::from(i * 17 % 47), 4.0 + f32::from(i * 23 % 61))
            })
            .collect();
        let regions: Vec<_> = sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| RepositoryRegion {
                label: i.to_string(),
                low: Vec2::ZERO,
                high: size,
                live: true,
            })
            .collect();
        let evaluate =
            |_: &Constraints, regions: &[RepositoryRegion]| coverage(regions);
        let (positions, nodes) = search(&sizes, &regions, evaluate);
        assert!(nodes <= SEARCH_NODES);
        assert_eq!(
            (positions.clone(), nodes),
            search(&sizes, &regions, evaluate)
        );
        let mut exhausted = SEARCH_NODES - 1;
        assert!(
            bottom_left(
                &sizes,
                &(0..sizes.len()).collect::<Vec<_>>(),
                200.0,
                &mut exhausted
            )
            .is_none()
        );
        assert_eq!(exhausted, SEARCH_NODES);
        for (a, position) in positions.iter().enumerate() {
            assert!(position.is_finite());
            for b in a + 1..sizes.len() {
                assert!(
                    position.x + sizes[a].x <= positions[b].x
                        || positions[b].x + sizes[b].x <= position.x
                        || position.y + sizes[a].y <= positions[b].y
                        || positions[b].y + sizes[b].y <= position.y
                );
            }
        }
    }

    #[test]
    fn growth_and_births_never_overlap_reorder_or_move_inward() {
        let samples: Vec<_> = (0..600_u16)
            .map(|tick| {
                (0..30_u16)
                    .map(|index| {
                        let size = 2.0 + f32::from(index) + f32::from(tick) / 100.0;
                        RepositoryRegion {
                            label: index.to_string(),
                            low: Vec2::splat(-size),
                            high: Vec2::splat(size),
                            live: tick >= index * 20,
                        }
                    })
                    .collect()
            })
            .collect();
        let packing = Packing::build(&samples);
        let mut previous: Option<Vec<RepositoryRegion>> = None;
        for tick in 0..600 {
            let (regions, _, _) = packing.sample(tick).unwrap();
            for (index, a) in regions.iter().enumerate().filter(|(_, r)| r.live) {
                if let Some(previous) = &previous
                    && previous[index].live
                {
                    assert!(a.low.x >= previous[index].low.x - 0.0001);
                    assert!(a.high.y <= previous[index].high.y + 0.0001);
                }
                for b in regions.iter().skip(index + 1).filter(|r| r.live) {
                    assert!(
                        a.high.x <= b.low.x + 0.0001
                            || b.high.x <= a.low.x + 0.0001
                            || a.high.y <= b.low.y + 0.0001
                            || b.high.y <= a.low.y + 0.0001
                    );
                }
            }
            previous = Some(regions);
        }
        let (regions, _, _) = packing.sample(599).unwrap();
        let largest = &regions[29];
        assert!(regions.iter().all(|r| r.high.y <= largest.high.y));
    }

    #[test]
    fn absent_slots_take_no_space_and_a_cleared_live_set_releases_space() {
        let region = RepositoryRegion {
            label: "repo".into(),
            low: Vec2::ZERO,
            high: Vec2::splat(5.0),
            live: true,
        };
        let mut absent = region.clone();
        absent.live = false;
        let packing = Packing::build(&[
            vec![absent.clone(), region.clone()],
            vec![region.clone(), region.clone()],
            vec![absent, region],
        ]);
        let (before, _, _) = packing.sample(0).unwrap();
        let (after, _, _) = packing.sample(2).unwrap();
        assert_eq!(before[1].low, after[1].low);
        assert!(!after[0].live);
    }

    #[test]
    fn fade_weight_hook_releases_space_continuously_without_changing_slots() {
        let regions: Vec<_> = (0..2)
            .map(|index| RepositoryRegion {
                label: index.to_string(),
                low: Vec2::ZERO,
                high: Vec2::splat(5.0),
                live: true,
            })
            .collect();
        let packing = Packing::build(&[regions]);
        let mut previous = packing
            .sample_with_presence(0, Some(&[1.0, 1.0]))
            .unwrap()
            .0;
        for tick in 1..=120_u16 {
            let u = f32::from(tick) / 120.0;
            let weight = 1.0 - u * u * (3.0 - 2.0 * u);
            let (regions, _, _) = packing
                .sample_with_presence(0, Some(&[weight, 1.0]))
                .unwrap();
            assert!(regions[1].low.distance(previous[1].low) < 0.08);
            assert!(
                regions[0].high.x <= regions[1].low.x + 0.0001
                    || regions[1].high.y <= regions[0].low.y + 0.0001
            );
            previous = regions;
        }
        assert!(!previous[0].live);
        assert_eq!(previous[1].high.y, 0.0);
        assert_eq!(previous[1].low.x, 0.0);
    }
}
