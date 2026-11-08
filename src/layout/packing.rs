use std::sync::Arc;

use bevy::math::Vec2;

use super::RepositoryRegion;
use crate::model::framing;

const GAP: f32 = 1.0;
const GROW_SECONDS: f32 = 2.0;

#[derive(Clone, Debug)]
struct Sample {
    low: Vec<Vec2>,
    high: Vec<Vec2>,
    live: Vec<bool>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Packing {
    rows: Vec<Vec<usize>>,
    samples: Arc<[Sample]>,
    labels: Vec<String>,
    births: Vec<usize>,
}

impl Packing {
    pub(crate) fn build(regions: &[Vec<RepositoryRegion>], sizes: &[Vec2]) -> Self {
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
        let mut order: Vec<_> = (0..first.len()).collect();
        order.sort_by(|&a, &b| {
            sizes[b]
                .element_product()
                .total_cmp(&sizes[a].element_product())
                .then(a.cmp(&b))
        });
        let reservations = samples.last().expect("nonempty region history");
        let sizes: Vec<_> = reservations
            .high
            .iter()
            .zip(&reservations.low)
            .map(|(high, low)| high - low)
            .collect();
        let minimum = sizes.iter().map(|size| size.x).fold(0.0_f32, f32::max);
        let maximum = sizes.iter().map(|size| size.x + GAP).sum::<f32>();
        let mut best = Vec::new();
        let mut score = f32::INFINITY;
        let mut placed = first.clone();
        for region in &mut placed {
            region.live = true;
        }
        for trial in 0..=32_u16 {
            let limit = minimum + (maximum - minimum) * f32::from(trial) / 32.0;
            let mut rows = vec![Vec::new()];
            let (mut height, mut x, mut row_height) = (0.0_f32, 0.0_f32, 0.0_f32);
            for &index in &order {
                if x > 0.0 && x + sizes[index].x > limit {
                    height += row_height + GAP;
                    x = 0.0;
                    row_height = 0.0;
                    rows.push(Vec::new());
                }
                rows.last_mut().expect("front row exists").push(index);
                placed[index].low = Vec2::new(x, -height - sizes[index].y);
                placed[index].high = Vec2::new(x + sizes[index].x, -height);
                x += sizes[index].x + GAP;
                row_height = row_height.max(sizes[index].y);
            }
            let candidate = framing::distance(
                &placed,
                framing::centre(&placed),
                framing::FOV,
                framing::VIEWPORT,
            );
            if candidate < score {
                score = candidate;
                best = rows;
            }
        }
        Self {
            rows: best,
            samples: samples.into(),
            labels: first.iter().map(|r| r.label.clone()).collect(),
            births,
        }
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
        let mut y = 0.0;
        for row in &self.rows {
            let (mut x, mut height) = (0.0_f32, 0.0_f32);
            for &index in row {
                let region = &mut regions[index];
                let size = region.high - region.low;
                offsets[index] =
                    Vec2::new(x, y) - Vec2::new(region.low.x, region.high.y);
                region.low += offsets[index];
                region.high += offsets[index];
                if region.live {
                    x += size.x + GAP * scales[index];
                    height = height.max(size.y + GAP * scales[index]);
                }
            }
            y -= height;
        }
        Some((regions, offsets, scales))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perspective_fit_prefers_two_rows_for_a_dominant_tall_repository() {
        let sizes = [
            Vec2::new(27.0, 22.0),
            Vec2::new(61.0, 86.0),
            Vec2::new(19.0, 18.0),
            Vec2::new(37.0, 26.0),
            Vec2::new(30.0, 26.0),
        ];
        let regions = sizes
            .iter()
            .enumerate()
            .map(|(index, &size)| RepositoryRegion {
                label: index.to_string(),
                low: Vec2::ZERO,
                high: size,
                live: true,
            })
            .collect();
        let history = [regions];
        let packing = Packing::build(&history, &sizes);
        assert_eq!(packing.rows, [vec![1, 3], vec![4, 0, 2]]);
        assert_eq!(packing.rows, Packing::build(&history, &sizes).rows);
        let fit = |packing: &Packing| {
            let regions = packing.sample(0).unwrap().0;
            framing::distance(
                &regions,
                framing::centre(&regions),
                framing::FOV,
                framing::VIEWPORT,
            )
        };
        for rows in [
            vec![vec![1, 3, 4, 0, 2]],
            vec![vec![1], vec![3, 4], vec![0, 2]],
        ] {
            let mut alternative = packing.clone();
            alternative.rows = rows;
            assert!(fit(&packing) < fit(&alternative));
        }
    }

    #[test]
    fn growth_and_births_never_overlap_reorder_or_move_inward() {
        let samples: Vec<_> = (0..600_u16)
            .map(|tick| {
                (0..12_u16)
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
        let packing = Packing::build(
            &samples,
            &samples
                .last()
                .unwrap()
                .iter()
                .map(|r| r.high - r.low)
                .collect::<Vec<_>>(),
        );
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
        let largest = &regions[11];
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
        let packing = Packing::build(
            &[
                vec![absent.clone(), region.clone()],
                vec![region.clone(), region.clone()],
                vec![absent, region],
            ],
            &[Vec2::splat(5.0); 2],
        );
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
        let packing = Packing::build(&[regions], &[Vec2::splat(5.0); 2]);
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
