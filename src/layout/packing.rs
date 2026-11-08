use std::sync::Arc;

use bevy::math::Vec2;

use super::RepositoryRegion;

const GAP: f32 = 3.0;
const ASPECT: f32 = 16.0 / 9.0;
const PAUSE: u64 = 360;
const SEARCH_LIMIT: usize = 4096;

#[derive(Clone, Debug)]
struct Placement {
    regions: Vec<RepositoryRegion>,
    offsets: Vec<Vec2>,
}

impl Placement {
    fn empty(regions: Vec<RepositoryRegion>) -> Self {
        let offsets = vec![Vec2::ZERO; regions.len()];
        Self { regions, offsets }
    }

    fn world_regions(&self) -> Vec<RepositoryRegion> {
        self.regions
            .iter()
            .zip(&self.offsets)
            .map(|(region, offset)| {
                let mut region = region.clone();
                region.low += offset;
                region.high += offset;
                region
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
struct Move {
    start: u64,
    duration: u64,
    local_tick: u64,
    before: Placement,
    after: Placement,
    old_lanes: Vec<f32>,
    new_lanes: Vec<f32>,
    rows: Vec<f32>,
}

impl Move {
    fn sample(&self, tick: u64) -> Placement {
        let fraction = crate::model::tick_time(tick - self.start)
            / crate::model::tick_time(self.duration)
            * 6.0;
        let ease = |x: f32| {
            let x = x.clamp(0.0, 1.0);
            x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
        };
        let mut result = self.before.clone();
        for (index, region) in result.regions.iter_mut().enumerate() {
            let old = &self.before.regions[index];
            let new = &self.after.regions[index];
            region.live = old.live && new.live;
            let old_lane = self.old_lanes[index] - old.low.x;
            let resized_lane = self.old_lanes[index] - new.low.x;
            let new_lane = self.new_lanes[index] - new.low.x;
            let row = self.rows[index] - new.low.y;
            let offset = &mut result.offsets[index];
            if fraction < 1.0 {
                offset.x = offset.x + (old_lane - offset.x) * ease(fraction);
            } else if fraction < 2.0 {
                let u = ease(fraction - 1.0);
                region.low = old.low.lerp(new.low, u);
                region.high = old.high.lerp(new.high, u);
                offset.x = old_lane + (resized_lane - old_lane) * u;
            } else {
                region.low = new.low;
                region.high = new.high;
                *offset = if fraction < 3.0 {
                    Vec2::new(
                        resized_lane,
                        offset.y + (row - offset.y) * ease(fraction - 2.0),
                    )
                } else if fraction < 4.0 {
                    Vec2::new(
                        resized_lane + (new_lane - resized_lane) * ease(fraction - 3.0),
                        row,
                    )
                } else if fraction < 5.0 {
                    Vec2::new(
                        new_lane,
                        row + (self.after.offsets[index].y - row)
                            * ease(fraction - 4.0),
                    )
                } else {
                    Vec2::new(new_lane, self.after.offsets[index].y)
                        .lerp(self.after.offsets[index], ease(fraction - 5.0))
                };
            }
        }
        result
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Packing {
    initial: Option<Placement>,
    moves: Arc<[Move]>,
}

impl Packing {
    pub(crate) fn build(samples: &[Vec<RepositoryRegion>]) -> Self {
        let Some(first) = samples.first() else {
            return Self::default();
        };
        let mut reserved = first.clone();
        for region in reserved.iter_mut().filter(|region| region.live) {
            let slack = (region.high - region.low) * 0.125 + Vec2::ONE;
            region.low -= slack;
            region.high += slack;
        }
        let initial = pack(reserved);
        let mut current = initial.clone();
        let mut moves = Vec::new();
        let mut pauses = 0;
        for (tick, regions) in samples.iter().enumerate().skip(1) {
            let changed =
                regions
                    .iter()
                    .zip(&current.regions)
                    .any(|(actual, reserved)| {
                        actual.live != reserved.live
                            || (actual.live
                                && (!actual.low.cmpge(reserved.low).all()
                                    || !actual.high.cmple(reserved.high).all()
                                    || (actual.high - actual.low).element_product()
                                        < (reserved.high - reserved.low)
                                            .element_product()
                                            * 0.25))
                    });
            if !changed {
                continue;
            }
            let mut reserved = regions.clone();
            for (index, region) in reserved.iter_mut().enumerate() {
                let previous = &samples[tick - 1][index];
                region.low = region.low.min(previous.low);
                region.high = region.high.max(previous.high);
                let slack = (region.high - region.low) * 0.125 + Vec2::ONE;
                region.low -= slack;
                region.high += slack;
            }
            let after = pack(reserved);
            let widths: Vec<_> = current
                .regions
                .iter()
                .zip(&after.regions)
                .map(|(old, new)| (old.high.x - old.low.x).max(new.high.x - new.low.x))
                .collect();
            let old_lanes = lanes(&current, &widths);
            let new_lanes = lanes(&after, &widths);
            let mut y = 0.0;
            let rows = after
                .regions
                .iter()
                .map(|region| {
                    let row = y;
                    y += region.high.y - region.low.y + GAP;
                    row
                })
                .collect();
            let local_tick = u64::try_from(tick).expect("playback tick fits u64");
            let duration = if current.regions.iter().any(|region| region.live) {
                PAUSE
            } else {
                0
            };
            moves.push(Move {
                start: local_tick - 1 + pauses,
                duration,
                local_tick,
                before: current,
                after: after.clone(),
                old_lanes,
                new_lanes,
                rows,
            });
            pauses += duration;
            current = after;
        }
        Self {
            initial: Some(initial),
            moves: moves.into(),
        }
    }

    pub(crate) fn extra_ticks(&self) -> u64 {
        self.moves.last().map_or(0, |movement| {
            movement.start + movement.duration - (movement.local_tick - 1)
        })
    }

    pub(crate) fn sample(
        &self,
        tick: u64,
    ) -> Option<(u64, Vec<RepositoryRegion>, Vec<Vec2>)> {
        let initial = self.initial.as_ref()?;
        let next = self.moves.partition_point(|movement| movement.start < tick);
        let (local, placement) = if next == 0 {
            (tick, initial.clone())
        } else {
            let movement = &self.moves[next - 1];
            if tick < movement.start + movement.duration {
                (movement.local_tick - 1, movement.sample(tick))
            } else {
                (
                    tick - (movement.start + movement.duration
                        - (movement.local_tick - 1)),
                    movement.after.clone(),
                )
            }
        };
        Some((local, placement.world_regions(), placement.offsets))
    }
}

fn lanes(placement: &Placement, widths: &[f32]) -> Vec<f32> {
    let mut order: Vec<_> = (0..placement.regions.len()).collect();
    order.sort_by(|&a, &b| {
        (placement.regions[a].low.x + placement.offsets[a].x)
            .total_cmp(&(placement.regions[b].low.x + placement.offsets[b].x))
            .then(a.cmp(&b))
    });
    let mut result = vec![0.0; order.len()];
    let mut x = 0.0;
    for index in order {
        result[index] = x;
        x += widths[index] + GAP;
    }
    result
}

#[derive(Clone)]
struct Candidate {
    size: Vec2,
    positions: Vec<(usize, Vec2)>,
}

fn combine(
    left: &[Candidate],
    right: &[Candidate],
    candidates: &mut Vec<Candidate>,
    nodes: &mut usize,
) -> Option<()> {
    for a in left {
        for b in right {
            for axis in 0..2 {
                *nodes += 1;
                if *nodes > SEARCH_LIMIT {
                    return None;
                }
                let mut size = a.size.max(b.size);
                size[axis] = a.size[axis] + b.size[axis] + GAP;
                if candidates.iter().any(|old| old.size.cmple(size).all()) {
                    continue;
                }
                candidates.retain(|old| !size.cmple(old.size).all());
                let mut positions = a.positions.clone();
                let mut offset = Vec2::ZERO;
                offset[axis] = a.size[axis] + GAP;
                positions.extend(
                    b.positions
                        .iter()
                        .map(|(index, point)| (*index, *point + offset)),
                );
                candidates.push(Candidate { size, positions });
            }
        }
    }
    Some(())
}

fn guillotine(regions: &[RepositoryRegion], live: &[usize]) -> Option<Candidate> {
    if live.len() > 8 {
        return None;
    }
    let mut frontier: Vec<Vec<Candidate>> = vec![Vec::new(); 1 << live.len()];
    let mut nodes = 0;
    for mask in 1_usize..frontier.len() {
        if mask.is_power_of_two() {
            let index = live
                [usize::try_from(mask.trailing_zeros()).expect("bit index fits usize")];
            frontier[mask].push(Candidate {
                size: regions[index].high - regions[index].low,
                positions: vec![(index, Vec2::ZERO)],
            });
            continue;
        }
        let mut left = (mask - 1) & mask;
        let mut candidates: Vec<Candidate> = Vec::new();
        while left > 0 {
            let right = mask ^ left;
            if left & mask.wrapping_neg() != 0 {
                combine(
                    &frontier[left],
                    &frontier[right],
                    &mut candidates,
                    &mut nodes,
                )?;
            }
            left = (left - 1) & mask;
        }
        frontier[mask] = candidates;
    }
    frontier
        .pop()?
        .into_iter()
        .min_by(|a, b| score(a.size).total_cmp(&score(b.size)))
}

fn score(size: Vec2) -> f32 {
    (size.x / ASPECT).max(size.y * crate::model::view_direction().y)
}

fn pack(regions: Vec<RepositoryRegion>) -> Placement {
    let live: Vec<_> = regions
        .iter()
        .enumerate()
        .filter(|(_, region)| region.live)
        .map(|(index, _)| index)
        .collect();
    if let Some(candidate) = guillotine(&regions, &live) {
        let mut offsets = vec![Vec2::ZERO; regions.len()];
        for (index, position) in candidate.positions {
            offsets[index] = position - regions[index].low - candidate.size * 0.5;
        }
        return Placement { regions, offsets };
    }
    shelves(regions)
}

fn shelves(regions: Vec<RepositoryRegion>) -> Placement {
    let mut live: Vec<_> = regions
        .iter()
        .enumerate()
        .filter(|(_, region)| region.live)
        .map(|(index, _)| index)
        .collect();
    live.sort_by(|&a, &b| {
        (regions[b].high.y - regions[b].low.y)
            .total_cmp(&(regions[a].high.y - regions[a].low.y))
            .then(a.cmp(&b))
    });
    if live.is_empty() {
        return Placement::empty(regions);
    }
    let minimum = live
        .iter()
        .map(|&index| regions[index].high.x - regions[index].low.x)
        .fold(0.0_f32, f32::max);
    let maximum = live
        .iter()
        .map(|&index| regions[index].high.x - regions[index].low.x + GAP)
        .sum::<f32>();
    let mut best = Placement::empty(regions.clone());
    let mut best_score = f32::INFINITY;
    for trial in 0..=32_u16 {
        let limit = minimum + (maximum - minimum) * f32::from(trial) / 32.0;
        let mut offsets = vec![Vec2::ZERO; regions.len()];
        let (mut x, mut y, mut row_height, mut width) =
            (0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32);
        for &index in &live {
            let region = &regions[index];
            let size = region.high - region.low;
            if x > 0.0 && x + size.x > limit {
                width = width.max(x - GAP);
                y += row_height + GAP;
                x = 0.0;
                row_height = 0.0;
            }
            offsets[index] = Vec2::new(x, y) - region.low;
            x += size.x + GAP;
            row_height = row_height.max(size.y);
        }
        width = width.max(x - GAP);
        let height = y + row_height;
        let candidate = score(Vec2::new(width, height));
        if candidate < best_score {
            best_score = candidate;
            for offset in &mut offsets {
                *offset -= Vec2::new(width, height) * 0.5;
            }
            best.offsets = offsets;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_disjoint(regions: &[RepositoryRegion]) {
        for (index, a) in regions.iter().enumerate().filter(|(_, region)| region.live) {
            for b in regions.iter().skip(index + 1).filter(|region| region.live) {
                assert!(
                    a.high.x <= b.low.x + 0.0001
                        || b.high.x <= a.low.x + 0.0001
                        || a.high.y <= b.low.y + 0.0001
                        || b.high.y <= a.low.y + 0.0001,
                    "{} overlaps {}: {regions:?}",
                    a.label,
                    b.label
                );
            }
        }
    }

    #[test]
    fn transitions_separate_all_live_envelopes_at_every_tick() {
        let samples: Vec<_> = (0..100)
            .map(|tick| {
                (0..5)
                    .map(|index| {
                        let size =
                            f32::from(u16::try_from(index + 1 + tick / 10).unwrap());
                        RepositoryRegion {
                            label: index.to_string(),
                            low: Vec2::new(-size, -2.0),
                            high: Vec2::new(size, size),
                            live: tick >= index * 10 && tick < 70 + index * 3,
                        }
                    })
                    .collect()
            })
            .collect();
        let plan = Packing::build(&samples);
        for tick in 0..100 + plan.extra_ticks() {
            let (_, regions, _) = plan.sample(tick).unwrap();
            assert_disjoint(&regions);
        }
    }

    #[test]
    fn bounded_exact_search_and_large_set_fallback_keep_envelopes_disjoint() {
        for count in 1..=12 {
            let regions: Vec<_> = (0..count)
                .map(|index| RepositoryRegion {
                    label: index.to_string(),
                    low: Vec2::ZERO,
                    high: Vec2::new(
                        f32::from(u16::try_from(3 + index % 4).unwrap()),
                        f32::from(u16::try_from(4 + index % 3).unwrap()),
                    ),
                    live: true,
                })
                .collect();
            let live: Vec<_> = (0..count).collect();
            let exact = guillotine(&regions, &live);
            eprintln!("repositories={count} exact_completed={}", exact.is_some());
            let placement = pack(regions);
            assert_disjoint(&placement.world_regions());
        }
    }

    #[test]
    fn settled_initial_regions_have_slack_before_the_first_played_commit() {
        let region = RepositoryRegion {
            label: "Existing".to_owned(),
            low: Vec2::splat(-4.0),
            high: Vec2::splat(4.0),
            live: true,
        };
        let mut grown = region.clone();
        grown.high += Vec2::splat(0.1);
        let packing = Packing::build(&[vec![region], vec![grown]]);
        assert_eq!(packing.extra_ticks(), 0);
        assert_eq!(packing.sample(1).unwrap().0, 1);
    }
}
