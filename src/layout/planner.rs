use std::{collections::VecDeque, sync::Arc, time::Duration};

use bevy::math::Vec2;

use super::{Layout, MAX_ACCELERATION, MAX_SPEED, Presence, parent_path};
use crate::{
    model,
    timeline::{Event, Kind},
};

const SAMPLE_TICKS: usize = 15;
const GAP: f32 = 0.08;
pub(super) const RELEASE_RADIUS: f32 = 0.05;

#[derive(Clone, Debug, Default)]
pub(super) struct Plan {
    pub(super) parts: Vec<(usize, Plan)>,
    samples: Arc<[Vec<Vec2>]>,
    pub(super) time: f32,
    origin: f32,
    step: f32,
    seconds: f64,
}

#[derive(Clone, Copy, Debug)]
enum Region {
    Directory(usize),
    Split {
        left: usize,
        right: usize,
        axis: usize,
    },
}

impl Plan {
    pub(super) fn add_part(&mut self, base: usize, plan: Self) {
        self.seconds += plan.seconds;
        self.parts.push((base, plan));
    }
    fn new(layout: &mut Layout, events: &[Event], starts: &[f32]) -> Self {
        for event in events {
            for change in &event.changes {
                layout.ensure_dir(parent_path(&change.path));
            }
        }
        let (required, step) = occupancy(layout, events, starts);
        let peak: Vec<_> = required
            .iter()
            .map(|row| row.iter().copied().fold(0.0, f32::max))
            .collect();
        let mut regions = Vec::new();
        let mut sizes = Vec::new();
        let mut children = vec![Vec::new(); layout.dir_count()];
        for (index, dir) in layout.dirs.iter().enumerate() {
            if let Some(parent) = dir.parent {
                children[parent].push(index);
            }
        }
        let root = subtree(&children, 0, &peak, &mut regions, &mut sizes);
        let mut windows = vec![8_usize; required[0].len()];
        let mut samples = loop {
            // Reverse the two filter supports, including the spline, into reservations.
            let reserved: Vec<_> = required
                .iter()
                .map(|row| spread(&spread(&maximum(row, 2), &windows), &windows))
                .collect();
            let mut samples = pack(&regions, root, &reserved);
            anchor_motion(&mut samples, &required);
            for row in &mut samples {
                *row = smooth(&smooth(row, &windows), &windows);
            }
            let failed = motion_failures(&samples, &required, step);
            if !failed.iter().any(|&value| value) {
                break samples;
            }
            let updated = widen_windows(&windows, &failed);
            windows = updated;
        };
        for position in samples.iter_mut().flatten() {
            *position = Vec2::new(position.y, -position.x);
        }
        Self {
            parts: Vec::new(),
            samples: samples.into(),
            time: 0.0,
            origin: 0.0,
            step,
            seconds: 0.0,
        }
    }

    pub(super) fn position(&self, dir: usize) -> Option<Vec2> {
        if !self.parts.is_empty() {
            let (offset, plan) =
                self.parts.iter().rev().find(|(offset, _)| *offset <= dir)?;
            return plan.at(dir - offset, self.time);
        }
        self.at(dir, self.time)
    }

    fn at(&self, dir: usize, time: f32) -> Option<Vec2> {
        let row = self.samples.get(dir)?;
        Some(interpolate(row, time, self.step))
    }

    fn place(&self, time: f32, dirs: &mut [super::Dir]) {
        if self.parts.is_empty() {
            assert_eq!(dirs.len(), self.samples.len(), "planned directories");
            let sample = interpolator(time, self.step, self.samples[0].len());
            for (dir, row) in dirs.iter_mut().zip(self.samples.iter()) {
                dir.position = sample(row);
            }
        } else {
            for (index, (base, plan)) in self.parts.iter().enumerate() {
                let end = self.parts.get(index + 1).map_or(dirs.len(), |p| p.0);
                plan.place(time, &mut dirs[*base..end]);
            }
        }
    }
}

fn interpolate(row: &[Vec2], time: f32, step: f32) -> Vec2 {
    interpolator(time, step, row.len())(row)
}

fn interpolator(time: f32, step: f32, length: usize) -> impl Fn(&[Vec2]) -> Vec2 {
    let at = (time.max(0.0) / step).floor();
    let index = usize::try_from(std::time::Duration::from_secs_f32(at).as_secs())
        .unwrap_or(usize::MAX)
        .min(length - 1);
    let fraction = ((time
        - model::count_u32(u32::try_from(index).expect("sample index fits u32"))
            * step)
        / step)
        .clamp(0.0, 1.0);
    let u = fraction;
    let weights = [
        (1.0 - u).powi(3),
        3.0 * u.powi(3) - 6.0 * u * u + 4.0,
        -3.0 * u.powi(3) + 3.0 * u * u + 3.0 * u + 1.0,
        u.powi(3),
    ];
    let controls = [
        index.saturating_sub(1),
        index,
        (index + 1).min(length - 1),
        (index + 2).min(length - 1),
    ];
    move |row| {
        controls
            .into_iter()
            .zip(weights)
            .map(|(index, weight)| row[index] * (weight / 6.0))
            .sum()
    }
}

fn occupancy(
    layout: &Layout,
    events: &[Event],
    starts: &[f32],
) -> (Vec<Vec<f32>>, f32) {
    let mut shadow = layout.clone();
    let duration = starts.last().copied().unwrap_or(0.5) + 2.5;
    let ticks = tick_at(duration);
    let sample_ticks = (usize::try_from(ticks).expect("tick count fits usize") / 8_192
        + 1)
    .max(SAMPLE_TICKS);
    let step = model::tick_time(
        u64::try_from(sample_ticks).expect("sample interval fits u64"),
    );
    let samples =
        usize::try_from(ticks).expect("tick count fits usize") / sample_ticks + 2;
    let mut required = vec![vec![0.0_f32; samples]; layout.dir_count()];
    let mut event_index = 0;
    let mut releases = std::collections::BinaryHeap::new();
    let mut deadlines = std::collections::HashMap::new();
    let mut tick = 0;
    while tick <= ticks {
        let time = model::tick_time(tick);
        while starts.get(event_index).is_some_and(|&start| start <= time) {
            for change in &events[event_index].changes {
                let id = model::apply(&mut shadow, change, 0);
                if matches!(change.kind, Kind::Deleted)
                    && shadow.files[&id].state == Presence::Leaving
                {
                    let end = tick_at(
                        time + shadow.files[&id].delay
                            + super::FADE_SECONDS
                            + 2.0 / 60.0,
                    );
                    deadlines.insert(id, end);
                    releases.push(std::cmp::Reverse((end, id)));
                } else if shadow.files[&id].state != Presence::Leaving {
                    deadlines.remove(&id);
                }
            }
            event_index += 1;
        }
        while releases
            .peek()
            .is_some_and(|&std::cmp::Reverse((end, _))| end <= tick)
        {
            let std::cmp::Reverse((end, id)) = releases.pop().expect("release is due");
            if deadlines.get(&id) != Some(&end) {
                continue;
            }
            deadlines.remove(&id);
            let Some(file) = shadow.files.remove(&id) else {
                continue;
            };
            if shadow.file_index.get(&file.path) == Some(&id) {
                shadow.file_index.remove(&file.path);
            }
            shadow.free_slot(file.dir, file.slot);
        }
        shadow.compact_slots();
        let mut active: Vec<_> = shadow.dirs.iter().map(|dir| dir.files > 0).collect();
        for index in (1..active.len()).rev() {
            if active[index] {
                active[shadow.dirs[index]
                    .parent
                    .expect("non-root directories have parents")] = true;
            }
        }
        active[0] = true;
        let sample =
            usize::try_from(tick).expect("tick count fits usize") / sample_ticks;
        for (index, dir) in shadow.dirs.iter().enumerate() {
            let radius = if active[index] { dir.radius() } else { 0.0 };
            required[index][sample] = required[index][sample].max(radius);
            required[index][sample + 1] = required[index][sample + 1].max(radius);
        }
        let next_sample =
            u64::try_from((sample + 1) * sample_ticks).expect("sample tick fits u64");
        let next_event = starts
            .get(event_index)
            .map_or(ticks.saturating_add(1), |&start| tick_at(start));
        let next_release = releases
            .peek()
            .map_or(ticks.saturating_add(1), |&std::cmp::Reverse((end, _))| end);
        tick = next_sample.min(next_event).min(next_release).max(tick + 1);
    }
    (required, step)
}

fn tick_at(time: f32) -> u64 {
    u64::try_from(
        (Duration::from_secs_f32(time).as_nanos() * u128::from(model::TICK_RATE))
            .div_ceil(1_000_000_000),
    )
    .expect("playback tick fits u64")
}

fn maximum(values: &[f32], window: usize) -> Vec<f32> {
    let mut deque: VecDeque<usize> = VecDeque::new();
    let mut end = 0;
    values
        .iter()
        .enumerate()
        .map(|(index, _)| {
            while end < values.len().min(index + window + 1) {
                while deque
                    .back()
                    .is_some_and(|&last| values[last] <= values[end])
                {
                    deque.pop_back();
                }
                deque.push_back(end);
                end += 1;
            }
            while deque
                .front()
                .is_some_and(|&first| first < index.saturating_sub(window))
            {
                deque.pop_front();
            }
            values[*deque.front().expect("window contains its centre")]
        })
        .collect()
}

#[cfg(test)]
fn average(values: &[Vec2], window: usize) -> Vec<Vec2> {
    let width = u16::try_from(window * 2 + 1).expect("smoothing window fits u16");
    let mut sum =
        values[0] * f32::from(u16::try_from(window + 1).expect("window fits u16"));
    for index in 1..=window {
        sum += values[index.min(values.len() - 1)];
    }
    let mut result = Vec::with_capacity(values.len());
    for index in 0..values.len() {
        result.push(sum / f32::from(width));
        sum -= values[index.saturating_sub(window)];
        sum += values[(index + window + 1).min(values.len() - 1)];
    }
    result
}

fn motion_failures(
    samples: &[Vec<Vec2>],
    required: &[Vec<f32>],
    step: f32,
) -> Vec<bool> {
    let mut failed = vec![false; required[0].len()];
    for (row, radii) in samples.iter().zip(required) {
        for (index, (points, radii)) in row.windows(3).zip(radii.windows(3)).enumerate()
        {
            if radii.iter().all(|&radius| radius > 0.0)
                && (points[0].distance(points[1]) / step > MAX_SPEED
                    || points[1].distance(points[2]) / step > MAX_SPEED
                    || (points[0] - 2.0 * points[1] + points[2]).length()
                        / (step * step)
                        > MAX_ACCELERATION)
            {
                failed[index..index + 3].fill(true);
            }
        }
    }
    failed
}

fn widen_windows(windows: &[usize], failed: &[bool]) -> Vec<usize> {
    let mut updated = windows.to_vec();
    for (index, _) in failed.iter().enumerate().filter(|&(_, failed)| *failed) {
        let window = windows[index];
        assert!(
            window < 16_384,
            "layout smoothing did not meet motion bounds"
        );
        for value in &mut updated
            [index.saturating_sub(window)..windows.len().min(index + window + 1)]
        {
            *value = (*value).max(window * 2);
        }
    }
    updated
}
fn spread(values: &[f32], windows: &[usize]) -> Vec<f32> {
    let mut intervals: Vec<_> = values
        .iter()
        .zip(windows)
        .enumerate()
        .map(|(index, (&radius, &window))| {
            (index.saturating_sub(window), index + window, radius)
        })
        .collect();
    intervals.sort_by_key(|&(start, ..)| start);
    let mut pending = 0;
    let mut heap = std::collections::BinaryHeap::new();
    (0..values.len())
        .map(|index| {
            while pending < intervals.len() && intervals[pending].0 <= index {
                let (_, end, radius) = intervals[pending];
                heap.push((radius.to_bits(), end));
                pending += 1;
            }
            while heap.peek().is_some_and(|&(_, end)| end < index) {
                heap.pop();
            }
            f32::from_bits(heap.peek().expect("support includes its centre").0)
        })
        .collect()
}

fn smooth(values: &[Vec2], windows: &[usize]) -> Vec<Vec2> {
    let mut prefix = vec![bevy::math::DVec2::ZERO; values.len() + 1];
    for (index, value) in values.iter().enumerate() {
        prefix[index + 1] = prefix[index] + value.as_dvec2();
    }
    windows
        .iter()
        .enumerate()
        .map(|(index, &window)| {
            let low = index.saturating_sub(window);
            let high = values.len().min(index + window + 1);
            let left = window.saturating_sub(index);
            let right = (index + window + 1).saturating_sub(values.len());
            let count = |n| f64::from(u32::try_from(n).expect("filter width fits u32"));
            ((prefix[high] - prefix[low]
                + values[0].as_dvec2() * count(left)
                + values[values.len() - 1].as_dvec2() * count(right))
                / count(window * 2 + 1))
            .as_vec2()
        })
        .collect()
}
fn anchor_motion(samples: &mut [Vec<Vec2>], required: &[Vec<f32>]) {
    let mut previous: Vec<_> = samples.iter().map(|row| row[0]).collect();
    let mut shift = Vec2::ZERO;
    for sample in 1..samples[0].len() {
        let deltas: Vec<_> = samples
            .iter()
            .zip(&previous)
            .zip(required)
            .map(|((row, &previous), radii)| {
                let radius = radii[sample].min(radii[sample - 1]);
                let weight = if radius > 0.0 {
                    1.0 + ((radius - 0.4) / super::SPACING).powi(2)
                } else {
                    0.0
                };
                (row[sample] - previous, weight)
            })
            .collect();
        let total: f32 = deltas.iter().map(|&(_, weight)| weight).sum();
        let mut translation = if total > 0.0 {
            deltas
                .iter()
                .map(|&(delta, weight)| delta * weight)
                .sum::<Vec2>()
                / total
        } else {
            Vec2::ZERO
        };
        for _ in 0..16 {
            let mut sum = Vec2::ZERO;
            let mut total = 0.0;
            for &(delta, weight) in &deltas {
                let influence = weight / delta.distance(translation).max(0.0001);
                sum += delta * influence;
                total += influence;
            }
            if total > 0.0 {
                translation = sum / total;
            }
        }
        shift += translation;
        for (row, previous) in samples.iter_mut().zip(&mut previous) {
            *previous = row[sample];
            row[sample] -= shift;
        }
    }
}

fn subtree(
    children: &[Vec<usize>],
    dir: usize,
    peak: &[f32],
    regions: &mut Vec<Region>,
    sizes: &mut Vec<Vec2>,
) -> usize {
    let own = regions.len();
    regions.push(Region::Directory(dir));
    sizes.push(Vec2::splat(2.0 * peak[dir] + GAP));
    let mut entries = vec![own];
    for &child in &children[dir] {
        entries.push(subtree(children, child, peak, regions, sizes));
    }
    combine(&entries, regions, sizes)
}

fn combine(
    entries: &[usize],
    regions: &mut Vec<Region>,
    sizes: &mut Vec<Vec2>,
) -> usize {
    if entries.len() == 1 {
        return entries[0];
    }
    let total: f32 = entries
        .iter()
        .map(|&entry| sizes[entry].element_product())
        .sum();
    let mut accumulated = 0.0;
    let mut middle = 1;
    let mut imbalance = f32::INFINITY;
    for (offset, &entry) in entries[..entries.len() - 1].iter().enumerate() {
        accumulated += sizes[entry].element_product();
        let difference = (total * 0.5 - accumulated).abs();
        if difference < imbalance {
            imbalance = difference;
            middle = offset + 1;
        }
    }
    let left = combine(&entries[..middle], regions, sizes);
    let right = combine(&entries[middle..], regions, sizes);
    let a = sizes[left];
    let b = sizes[right];
    let horizontal = Vec2::new(a.x + b.x, a.y.max(b.y));
    let vertical = Vec2::new(a.x.max(b.x), a.y + b.y);
    let aspect = |size: Vec2| (size.x / size.y).ln().abs();
    let axis = usize::from(aspect(vertical) < aspect(horizontal));
    let index = regions.len();
    regions.push(Region::Split { left, right, axis });
    sizes.push(if axis == 0 { horizontal } else { vertical });
    index
}

fn pack(regions: &[Region], root: usize, reserved: &[Vec<f32>]) -> Vec<Vec<Vec2>> {
    let (samples, residual) = pack_with_residual(regions, root, reserved);
    assert!(residual < 0.001, "region separation residual: {residual}");
    samples
}

fn pack_with_residual(
    regions: &[Region],
    root: usize,
    reserved: &[Vec<f32>],
) -> (Vec<Vec<Vec2>>, f32) {
    let samples = reserved[0].len();
    let mut result = vec![vec![Vec2::ZERO; samples]; reserved.len()];
    let mut sizes = vec![Vec2::ZERO; regions.len()];
    let mut offsets = vec![Vec2::ZERO; regions.len()];
    let mut residual = 0.0_f32;
    for sample in 0..samples {
        for (index, region) in regions.iter().enumerate() {
            sizes[index] = match *region {
                Region::Directory(dir) => Vec2::splat(reserved[dir][sample] * 2.0),
                Region::Split { left, right, axis } => {
                    let mut size = sizes[left].max(sizes[right]);
                    size[axis] = sizes[left][axis] + sizes[right][axis] + GAP;
                    size
                }
            };
        }
        offsets[root] = Vec2::ZERO;
        for (index, region) in regions.iter().enumerate().rev() {
            match *region {
                Region::Directory(dir) => result[dir][sample] = offsets[index],
                Region::Split { left, right, axis } => {
                    offsets[left] = offsets[index];
                    offsets[right] = offsets[index];
                    offsets[left][axis] -= sizes[right][axis].midpoint(GAP);
                    offsets[right][axis] += sizes[left][axis].midpoint(GAP);
                    let gap = offsets[right][axis]
                        - offsets[left][axis]
                        - sizes[left][axis].midpoint(sizes[right][axis]);
                    residual = residual.max(GAP - gap);
                }
            }
        }
        let anchor = result[0][sample];
        for row in &mut result {
            row[sample] -= anchor;
        }
    }
    (result, residual)
}

impl Layout {
    #[cfg(test)]
    pub(crate) fn planned(events: &[Event]) -> Self {
        let starts = model::schedule::Schedule::new(events, 0.5, 1.0).starts;
        Self::scheduled(events, &starts)
    }

    pub(crate) fn scheduled(events: &[Event], starts: &[f32]) -> Self {
        let mut layout = Self::empty();
        let started = std::time::Instant::now();
        layout.plan = Plan::new(&mut layout, events, starts);
        layout.plan.seconds = started.elapsed().as_secs_f64();
        for (index, dir) in layout.dirs.iter_mut().enumerate() {
            dir.live = index == 0;
            dir.children = 0;
            if let Some(position) = layout.plan.position(index) {
                dir.position = position;
            }
        }
        layout
    }

    pub(crate) fn planning_seconds(&self) -> f64 {
        self.plan.seconds
    }

    pub(crate) fn plan_time(&mut self, time: f32) {
        self.plan.time = time;
        self.plan.origin = time;
    }

    pub(crate) fn sample_plan(&mut self, time: f32, dt: f32) {
        self.plan.time = self.plan.origin + time - dt;
    }

    pub(super) fn relax(&mut self, dt: f32) {
        self.plan.time += dt;

        self.plan.place(self.plan.time, &mut self.dirs);
        for (index, dir) in self.dirs.iter_mut().enumerate() {
            let target = if index != 0 && dir.files == 0 && dir.children == 0 {
                0.0
            } else {
                dir.radius()
            };
            dir.reserve += (target - dir.reserve) * (1.0 - (-dt / 0.2).exp());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contributors::Mappings,
        model::{Options, Simulation},
        theme::Theme,
        timeline::{FileChange, Identity, Kind},
    };

    #[test]
    fn batched_splines_match_individual_directory_samples_exactly() {
        let histories = (0..3)
            .map(|repository| {
                let mut events = crate::timeline::synthetic();
                crate::repositories::namespace(&mut events, repository);
                events
            })
            .collect();
        let events = crate::repositories::merge(histories);
        let starts = model::schedule::Schedule::new(&events, 0.5, 1.0).starts;
        let combined = Layout::scheduled_repositories(
            &events,
            &starts,
            &["First".into(), "Second".into(), "Third".into()],
        );
        for mut layout in [Layout::planned(&crate::timeline::synthetic()), combined] {
            for tick in [0, 1, 13, 90, 1000, 10_000, 90, 1] {
                layout.plan.time = model::tick_time(tick);
                let expected: Vec<_> = (0..layout.dirs.len())
                    .map(|index| layout.plan.position(index).unwrap())
                    .collect();
                layout.plan.place(layout.plan.time, &mut layout.dirs);
                let actual: Vec<_> =
                    layout.dirs.iter().map(|dir| dir.position).collect();
                assert_eq!(actual, expected);
            }
        }
    }

    fn event(seconds: i64, changes: &[(&str, Kind)]) -> Event {
        Event {
            seconds,
            author: Identity {
                name: b"Ada".to_vec(),
                email: Vec::new(),
            },
            repository: 0,
            contributor: None,
            commit: String::new(),
            changes: changes
                .iter()
                .map(|(path, kind)| FileChange {
                    path: (*path).to_owned(),
                    kind: kind.clone(),
                    insertions: 1,
                    removals: 0,
                    samples: Vec::new(),
                })
                .collect(),
        }
    }

    fn burst() -> Vec<Event> {
        let mut events = vec![event(0, &[("z/keep", Kind::Added)])];
        for day in 1..20 {
            events.push(event(day * 86_400, &[("a/keep", Kind::Modified)]));
        }
        let mut growth = event(20 * 86_400, &[]);
        growth.changes = (0..80)
            .map(|index| FileChange {
                path: format!("a/deep/{index}"),
                kind: Kind::Added,
                insertions: 1,
                removals: 0,
                samples: Vec::new(),
            })
            .collect();
        events.push(growth);
        events.push(event(
            21 * 86_400,
            &[(
                "z/moved",
                Kind::Moved {
                    from: "a/keep".to_owned(),
                },
            )],
        ));
        let mut deletion = event(22 * 86_400, &[]);
        deletion.changes = (0..79)
            .map(|index| FileChange {
                path: format!("a/deep/{index}"),
                kind: Kind::Deleted,
                insertions: 0,
                removals: 1,
                samples: Vec::new(),
            })
            .collect();
        events.push(deletion);
        events
    }

    #[test]
    fn empty_history_has_a_root_trajectory() {
        let layout = Layout::planned(&[]);
        assert_eq!(layout.plan.samples.len(), 1);
        assert_eq!(layout.plan.position(0), Some(Vec2::ZERO));
    }

    #[test]
    fn local_windows_preserve_filter_support_and_leave_remote_spans_alone() {
        let values = [0.0_f32, 1.0, 0.0, 4.0, 2.0, 0.0];
        let windows = [1, 2, 1, 3, 1, 2];
        let actual = spread(&values, &windows);
        for (index, &value) in actual.iter().enumerate() {
            let expected = values
                .iter()
                .zip(windows)
                .enumerate()
                .filter(|&(source, (_, window))| source.abs_diff(index) <= window)
                .map(|(_, (&value, _))| value)
                .fold(0.0, f32::max);
            assert_eq!(value, expected);
        }
        let values: Vec<_> = values.into_iter().map(Vec2::splat).collect();
        let fixed = vec![2; values.len()];
        for (actual, expected) in
            smooth(&values, &fixed).iter().zip(average(&values, 2))
        {
            assert!(actual.distance(expected) < 0.00001);
        }
        let mut failed = vec![false; 400];
        failed[200] = true;
        let widened = widen_windows(&[8; 400], &failed);
        assert_eq!(widened[200], 16);
        assert_eq!(widened[0], 8);
        assert_eq!(widened[399], 8);
    }

    #[test]
    fn subtree_packing_reports_its_separation_residual() {
        let regions = [
            Region::Directory(0),
            Region::Directory(1),
            Region::Split {
                left: 0,
                right: 1,
                axis: 0,
            },
        ];
        let (_, residual) =
            pack_with_residual(&regions, 2, &[vec![100.0, 0.0], vec![0.0, 100.0]]);
        assert!(residual < 0.001);
    }
    #[test]
    fn global_sibling_order_never_changes_even_when_later_paths_sort_first() {
        let events = burst();
        let layout = Layout::planned(&events);
        let samples = &layout.plan.samples;
        for a in 0..samples.len() {
            for b in a + 1..samples.len() {
                let mut low = Vec2::splat(f32::INFINITY);
                let mut high = Vec2::splat(f32::NEG_INFINITY);
                for (&a, &b) in samples[a].iter().zip(&samples[b]) {
                    low = low.min(b - a);
                    high = high.max(b - a);
                }
                assert!(low.x > 0.0 || high.x < 0.0 || low.y > 0.0 || high.y < 0.0);
            }
        }
        let mut replay = layout.clone();
        replay.step(1.0 / 60.0);
        assert!(Arc::ptr_eq(&layout.plan.samples, &replay.plan.samples));
        assert_eq!(layout.plan.time, 0.0);
    }

    #[test]
    fn region_space_opens_before_a_subtree_burst_without_lifetime_reservation() {
        let events = burst();
        let layout = Layout::planned(&events);
        let a = layout.dir_index["a/deep"];
        let z = layout.dir_index["a"];
        let mut plan = layout.plan.clone();
        let separation = |plan: &Plan| {
            plan.position(a)
                .unwrap()
                .distance(plan.position(z).unwrap())
        };
        let early = separation(&plan);
        plan.time = 10.0;
        let before = separation(&plan);
        plan.time = 10.5;
        let at_growth = separation(&plan);
        assert!(before > early + 0.5, "early={early}, before={before}");
        assert!(at_growth >= before);
        let mut sim = Simulation::new(
            events,
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        for tick in 0..850 {
            sim.advance_to(tick, 60);
            let dirs: Vec<_> = sim.layout().dirs().collect();
            for (offset, (_, a)) in dirs.iter().enumerate() {
                for (_, b) in &dirs[offset + 1..] {
                    assert!(
                        a.position.distance(b.position) + 0.0001
                            >= a.radius() + b.radius(),
                        "{} overlaps {} at {tick}",
                        a.path,
                        b.path
                    );
                }
            }
        }
    }

    #[test]
    fn retained_files_and_directories_have_bounded_tick_velocity_and_acceleration() {
        use std::collections::BTreeMap;
        let mut sim = Simulation::new(
            burst(),
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let dt = 1.0 / 60.0;
        let mut previous = BTreeMap::new();
        let mut velocities = BTreeMap::new();
        for tick in 0..950 {
            sim.advance_to(tick, 60);
            let positions: BTreeMap<_, _> = sim
                .layout()
                .files()
                .map(|(id, file)| ((false, id), file.local_position))
                .chain(sim.layout().dirs().map(|(id, dir)| {
                    ((true, u64::try_from(id).unwrap()), dir.local_position)
                }))
                .collect();
            let mut next_velocities = BTreeMap::new();
            for (&id, &position) in &positions {
                let Some(&before) = previous.get(&id) else {
                    continue;
                };
                let velocity: Vec2 = (position - before) / dt;
                assert!(
                    velocity.length() <= MAX_SPEED + 0.001,
                    "speed {:?}: {} at {tick}",
                    id,
                    velocity.length()
                );
                if let Some(&before) = velocities.get(&id) {
                    let acceleration: Vec2 = (velocity - before) / dt;
                    assert!(
                        acceleration.length() <= MAX_ACCELERATION + 0.02,
                        "acceleration {:?}: {} at {tick}",
                        id,
                        acceleration.length()
                    );
                }
                next_velocities.insert(id, velocity);
            }
            previous = positions;
            velocities = next_velocities;
        }
    }

    #[test]
    fn maximum_matches_a_brute_force_window() {
        let values = [0.0_f32, 1.0, 0.0, 4.0, 2.0, 0.0];
        for window in [0, 1, 4, 20] {
            let expected: Vec<_> = (0..values.len())
                .map(|index| {
                    values[index.saturating_sub(window)
                        ..values.len().min(index + window + 1)]
                        .iter()
                        .copied()
                        .fold(0.0, f32::max)
                })
                .collect();
            assert_eq!(maximum(&values, window), expected);
        }
    }

    #[test]
    fn packing_releases_capacity_and_preserves_split_gaps() {
        let regions = [
            Region::Directory(0),
            Region::Directory(1),
            Region::Split {
                left: 0,
                right: 1,
                axis: 0,
            },
        ];
        let samples = pack(&regions, 2, &[vec![4.0, 1.0], vec![1.0, 1.0]]);
        assert!((samples[1][0].length() - 5.0 - GAP).abs() < 0.0001);
        assert!((samples[1][1].length() - 2.0 - GAP).abs() < 0.0001);
    }

    #[test]
    fn subtree_separation_survives_interpolation_during_opposing_growth() {
        let radii = [vec![4.0, 0.4], vec![0.4, 3.0], vec![2.0, 1.0]];
        let regions = [
            Region::Directory(0),
            Region::Directory(1),
            Region::Split {
                left: 0,
                right: 1,
                axis: 0,
            },
            Region::Directory(2),
            Region::Split {
                left: 2,
                right: 3,
                axis: 1,
            },
        ];
        let positions = pack(&regions, 4, &radii);
        for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let position =
                |index: usize| positions[index][0].lerp(positions[index][1], fraction);
            let radius = |index: usize| {
                radii[index][0] * (1.0 - fraction) + radii[index][1] * fraction
            };
            for a in 0..3 {
                for b in a + 1..3 {
                    assert!(position(a).distance(position(b)) >= radius(a) + radius(b));
                }
            }
        }
    }

    #[test]
    fn long_idle_histories_skip_settled_ticks_and_bound_schedule_storage() {
        let events: Vec<_> = (0..1_000)
            .map(|day| event(i64::from(day) * 86_400, &[("a/file", Kind::Modified)]))
            .collect();
        let starts = model::schedule::Schedule::new(&events, 86_400.0, 86_400.0).starts;
        assert!(tick_at(starts[999]) > u64::from(u32::MAX));
        let layout = Layout::scheduled(&events, &starts);
        assert!(layout.plan.samples[0].len() <= 8_194);
    }

    #[test]
    #[ignore = "developer planning cost measurement"]
    fn planning_cost_large_inline_history() {
        let events: Vec<_> = (0..1_000)
            .map(|commit| {
                let mut entry = event(i64::from(commit) * 3_600, &[]);
                entry.changes = (0..12)
                    .map(|file| {
                        let ordinal = commit * 12 + file;
                        FileChange {
                            path: format!(
                                "crate-{}/src/{}.rs",
                                ordinal / 4,
                                ordinal % 4
                            ),
                            kind: Kind::Added,
                            insertions: 1,
                            removals: 0,
                            samples: Vec::new(),
                        }
                    })
                    .collect();
                entry
            })
            .collect();

        let started = std::time::Instant::now();
        let layout = Layout::planned(&events);
        eprintln!(
            "large_inline_plan_seconds={:.6} events={} directories={} files=12000",
            started.elapsed().as_secs_f64(),
            events.len(),
            layout.dir_count()
        );
        assert_eq!(layout.dir_count(), 6_001);
        assert!(layout.plan.samples[0].len() <= 8_194);
    }
}

#[cfg(test)]
mod probes {
    use super::*;
    use crate::{
        contributors::Mappings,
        model::{Options, Simulation},
        theme::Theme,
        timeline::{FileChange, Identity, Kind},
    };

    fn ev(seconds: i64, changes: Vec<(String, Kind)>) -> Event {
        Event {
            seconds,
            author: Identity {
                name: b"Ada".to_vec(),
                email: Vec::new(),
            },
            repository: 0,
            contributor: None,
            commit: String::new(),
            changes: changes
                .into_iter()
                .map(|(path, kind)| FileChange {
                    path,
                    kind,
                    insertions: 1,
                    removals: 0,
                    samples: Vec::new(),
                })
                .collect(),
        }
    }

    pub(super) fn assert_separated(sim: &Simulation) {
        let dirs: Vec<_> = sim.layout().dirs().collect();
        for (index, (_, a)) in dirs.iter().enumerate() {
            for (_, b) in &dirs[index + 1..] {
                assert!(
                    a.position.distance(b.position) + 0.0001 >= a.radius() + b.radius(),
                    "{} overlaps {} at {}",
                    a.path,
                    b.path,
                    sim.seconds()
                );
            }
        }
    }

    fn audit(_name: &str, events: Vec<Event>, ticks: u32) {
        let mut sim = Simulation::new(
            events,
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let mut previous = std::collections::BTreeMap::new();
        for _ in 0..ticks {
            sim.tick_layout();
            assert_separated(&sim);
            for (id, file) in sim.layout().files() {
                if let Some(before) = previous.insert(id, file.local_position) {
                    assert!(
                        file.local_position.distance(before) * 60.0
                            <= MAX_SPEED + 0.001
                    );
                }
                let own = sim.layout().dir(file.dir);
                let presence = file.visible_presence(own);
                if presence > 0.0 {
                    assert!(
                        file.position.distance(own.position) + 0.27 * presence
                            <= own.radius() + 0.0001
                    );
                }
            }
        }
    }
    fn files(prefix: &str, n: usize, kind: &Kind) -> Vec<(String, Kind)> {
        (0..n)
            .map(|i| (format!("{prefix}/{i}"), kind.clone()))
            .collect()
    }

    #[test]
    fn probe_sibling_burst() {
        let mut events = vec![ev(
            0,
            (0..60)
                .map(|d| (format!("p/d{d}/seed"), Kind::Added))
                .collect(),
        )];
        for day in 1..5 {
            events.push(ev(day * 86_400, vec![("p/d0/seed".into(), Kind::Modified)]));
        }
        let mut big = Vec::new();
        for d in 0..60 {
            big.extend(files(&format!("p/d{d}"), 40, &Kind::Added));
        }
        events.push(ev(5 * 86_400, big));
        let mut del = Vec::new();
        for d in 0..60 {
            del.extend(files(&format!("p/d{d}"), 40, &Kind::Deleted));
        }
        events.push(ev(6 * 86_400, del));
        audit("sibling_burst", events, 60 * 10);
    }

    #[test]
    fn probe_far_rename() {
        let mut events = vec![ev(0, files("a/b/c/d", 30, &Kind::Added))];
        events.push(ev(86_400, files("z/y/x/w", 30, &Kind::Added)));
        for d in 0..20 {
            events.push(ev(
                2 * 86_400 + d,
                files(&format!("m{d}"), 30, &Kind::Added),
            ));
        }
        events.push(ev(
            3 * 86_400,
            (0..30)
                .map(|i| {
                    (
                        format!("z/y/x/w/r{i}"),
                        Kind::Moved {
                            from: format!("a/b/c/d/{i}"),
                        },
                    )
                })
                .collect(),
        ));
        events.push(ev(4 * 86_400, vec![("a/b/c/d/new".into(), Kind::Added)]));
        audit("far_rename", events, 60 * 8);
    }

    #[test]
    fn probe_root_burst_and_reactivation() {
        let mut events = vec![ev(0, files("x", 5, &Kind::Added))];
        events.push(ev(
            86_400,
            (0..400)
                .map(|i| (format!("r{i}.rs"), Kind::Added))
                .collect(),
        ));
        events.push(ev(2 * 86_400, files("x", 5, &Kind::Deleted)));
        events.push(ev(2 * 86_400 + 10, files("x", 50, &Kind::Added)));
        events.push(ev(
            3 * 86_400,
            (0..400)
                .map(|i| (format!("r{i}.rs"), Kind::Deleted))
                .collect(),
        ));
        audit("root_burst_reactivation", events, 60 * 7);
    }

    #[test]
    fn probe_scaling() {
        for dirs in [100usize, 200, 400] {
            let events: Vec<_> = (0..200)
                .map(|c| {
                    ev(
                        i64::try_from(c).unwrap() * 3_600,
                        (0..=dirs / 50)
                            .map(|k| {
                                (
                                    format!(
                                        "crate-{}/src/{c}.rs",
                                        (c * 7 + k * 13) % dirs
                                    ),
                                    Kind::Added,
                                )
                            })
                            .collect(),
                    )
                })
                .collect();
            let started = std::time::Instant::now();
            let layout = Layout::planned(&events);
            assert_eq!(layout.dir_count(), dirs * 2 + 1);
            assert!(layout.plan.samples[0].len() <= 53);
            eprintln!(
                "PROBE scaling dirs={} samples={} secs={:.3}",
                layout.dir_count(),
                layout.plan.samples[0].len(),
                started.elapsed().as_secs_f64()
            );
        }
    }
}

#[cfg(test)]
mod rate_probe {
    use super::*;
    use crate::{
        contributors::Mappings,
        model::{Options, Simulation},
        theme::Theme,
        timeline::{FileChange, Identity, Kind},
    };

    #[test]
    fn probe_rate_change() {
        for factor in [0.25_f32, 4.0] {
            let ev = |s: i64, paths: Vec<String>| Event {
                seconds: s,
                author: Identity {
                    name: b"Ada".to_vec(),
                    email: Vec::new(),
                },
                repository: 0,
                contributor: None,
                commit: String::new(),
                changes: paths
                    .into_iter()
                    .map(|path| FileChange {
                        path,
                        kind: Kind::Added,
                        insertions: 1,
                        removals: 0,
                        samples: Vec::new(),
                    })
                    .collect(),
            };
            let mut events =
                vec![ev(0, (0..30).map(|d| format!("p/d{d}/seed")).collect())];
            for day in 1..40 {
                events.push(ev(day * 86_400, vec![format!("p/d{}/f{day}", day % 30)]));
            }
            for (k, day) in (40..46).enumerate() {
                events.push(ev(
                    day * 86_400,
                    (0..60).map(|i| format!("p/d{}/burst{i}", k * 5)).collect(),
                ));
            }
            let mut sim = Simulation::new(
                events,
                &Options::default(),
                &Theme::neon(),
                &Mappings::default(),
            );
            sim.change_rate(factor);
            let ticks = u32::try_from(tick_at(sim.duration())).unwrap();
            for _ in 0..ticks {
                sim.tick_layout();
                super::probes::assert_separated(&sim);
            }
            sim.restart();
            let ticks = u32::try_from(tick_at(sim.duration())).unwrap();
            for _ in 0..ticks {
                sim.tick_layout();
                super::probes::assert_separated(&sim);
            }
        }
    }
}
