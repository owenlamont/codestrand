use bevy::math::Vec2;

use super::{Layout, RepositoryRegion};
use crate::timeline::Event;

/// Maps fixed-size chunks of `items` on up to `threads` scoped threads at a time and
/// returns the results in item order; zero threads takes the machine's parallelism.
pub(crate) fn in_order<T: Sync, R: Send>(
    items: &[T],
    chunk: usize,
    threads: usize,
    map: impl Fn(&[T]) -> Vec<R> + Sync,
) -> Vec<R> {
    let threads = match threads {
        0 => std::thread::available_parallelism().map_or(1, usize::from),
        forced => forced,
    };
    let chunks: Vec<_> = items.chunks(chunk).collect();
    if threads == 1 || chunks.len() <= 1 {
        return chunks.into_iter().flat_map(&map).collect();
    }
    let map = &map;
    let mut results = Vec::with_capacity(items.len());
    for batch in chunks.chunks(threads) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|&part| scope.spawn(move || map(part)))
                .collect();
            for handle in handles {
                results.extend(
                    handle
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                );
            }
        });
    }
    results
}

struct DirPoint {
    repository: usize,
    position: Vec2,
    radius: f32,
    scale: f32,
    live: bool,
}

struct FilePoint {
    dir: usize,
    position: Vec2,
    presence: f32,
    counted: bool,
}

/// What one tick's repository regions are computed from, copied out of the layout.
pub(crate) struct RegionSample {
    live: Vec<bool>,
    dirs: Vec<DirPoint>,
    files: Vec<FilePoint>,
}

impl RegionSample {
    /// Bytes one sample holds for a layout of this many directories and files.
    pub(crate) fn bytes(directories: usize, files: usize) -> usize {
        directories * std::mem::size_of::<DirPoint>()
            + files * std::mem::size_of::<FilePoint>()
    }

    pub(crate) fn regions(
        &self,
        labels: &[String],
        geometry: crate::theme::Geometry,
    ) -> Vec<RepositoryRegion> {
        let mut supports = vec![[f32::NEG_INFINITY; 8]; labels.len()];
        let mut counts = vec![0_u32; labels.len()];
        for dir in self.dirs.iter().filter(|dir| dir.live) {
            counts[dir.repository] = counts[dir.repository].saturating_add(1);
            super::outline::include(
                &mut supports[dir.repository],
                dir.position,
                dir.radius,
            );
        }
        let corners = super::outline::rectangle(
            Vec2::new(geometry.file_min.x, geometry.file_min.z),
            Vec2::new(geometry.file_max.x, geometry.file_max.z),
        );
        for file in &self.files {
            let dir = &self.dirs[file.dir];
            if file.counted {
                counts[dir.repository] = counts[dir.repository].saturating_add(1);
            }
            let presence = super::visible_presence(
                file.presence,
                file.position,
                dir.radius,
                dir.position,
                dir.scale,
            );
            if presence > 0.0 {
                for &point in &corners {
                    super::outline::include(
                        &mut supports[dir.repository],
                        file.position + point * presence,
                        0.0,
                    );
                }
            }
        }
        labels
            .iter()
            .enumerate()
            .map(|(repository, label)| {
                let outline = super::outline::from_supports(supports[repository]);
                let (low, high) = super::outline::bounds(&outline);
                RepositoryRegion {
                    label: label.clone(),
                    low,
                    high,
                    live: self.live[repository],
                    items: crate::model::count_u32(counts[repository]),
                    tile: None,
                    gutter: None,
                    outline,
                    height: geometry.file_max.y,
                    rotated: false,
                    stretch: Vec2::ONE,
                }
            })
            .collect()
    }
}

impl Layout {
    pub(crate) fn scheduled_repositories(
        events: &[Event],
        starts: &[crate::model::Time],
        names: &[String],
        progress: &crate::progress::Progress,
    ) -> Self {
        let mut combined = Self::empty();
        combined.dirs.clear();
        combined.dir_index.clear();
        let workers = std::thread::available_parallelism().map_or(1, usize::from);
        for first in (0..names.len()).step_by(workers) {
            progress.report(
                crate::progress::Stage::Directories,
                first as u64,
                names.len() as u64,
            );
            let end = first.saturating_add(workers).min(names.len());
            let quiet = crate::progress::Progress::default();
            let detailed = if names.len() == 1 { progress } else { &quiet };
            let layouts = Self::scheduled_batch(events, starts, first..end, detailed);
            for (repository, layout) in (first..end).zip(layouts) {
                combined.append_repository(repository, &names[repository], layout);
            }
        }
        combined
    }

    fn scheduled_batch(
        events: &[Event],
        starts: &[crate::model::Time],
        repositories: std::ops::Range<usize>,
        progress: &crate::progress::Progress,
    ) -> Vec<Self> {
        std::thread::scope(|scope| {
            let handles: Vec<_> = repositories
                .map(|repository| {
                    scope.spawn(move || {
                        Self::scheduled_repository(events, starts, repository, progress)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect::<Vec<_>>()
        })
    }

    fn append_repository(&mut self, repository: usize, label: &str, layout: Self) {
        let prefix = crate::repositories::prefix(repository);
        let region = RepositoryRegion {
            label: label.to_owned(),
            low: Vec2::splat(-2.0),
            high: Vec2::splat(2.0),
            live: false,
            items: 0.0,
            tile: None,
            gutter: None,
            outline: Vec::new(),
            height: 0.0,
            rotated: false,
            stretch: Vec2::ONE,
        };
        self.repositories.push(region);
        let base = self.dirs.len();
        for (index, mut dir) in layout.dirs.into_iter().enumerate() {
            dir.repository = repository;
            dir.path = if index == 0 {
                repository.to_string()
            } else {
                format!("{prefix}{}", dir.path)
            };
            dir.parent = dir.parent.map(|parent| base + parent);
            self.dir_index.insert(dir.path.clone(), base + index);
            self.dirs.push(dir);
        }
        self.plan.add_part(base, layout.plan);
    }

    fn scheduled_repository(
        events: &[Event],
        starts: &[crate::model::Time],
        repository: usize,
        progress: &crate::progress::Progress,
    ) -> Self {
        let mut local = Vec::new();
        let mut times = Vec::new();
        for (event, &time) in events.iter().zip(starts) {
            if event.repository == repository {
                local.push(event);
                times.push(time);
            }
        }
        Self::scheduled(
            &local,
            &times,
            crate::repositories::prefix(repository).len(),
            progress,
        )
    }

    pub(crate) fn region_labels(&self) -> Vec<String> {
        self.repositories.iter().map(|r| r.label.clone()).collect()
    }

    /// Copies out what this tick's regions are computed from.
    pub(crate) fn region_sample(&self, active: &[bool]) -> RegionSample {
        RegionSample {
            live: active.to_vec(),
            dirs: self
                .dirs
                .iter()
                .map(|dir| DirPoint {
                    repository: dir.repository,
                    position: dir.position,
                    radius: dir.radius(),
                    scale: dir.scale,
                    live: dir.live,
                })
                .collect(),
            files: self
                .files()
                .map(|(_, file)| FilePoint {
                    dir: file.dir,
                    position: file.position,
                    presence: file.presence,
                    counted: file.state != super::Presence::Leaving,
                })
                .collect(),
        }
    }

    pub(crate) fn remove_offsets(&mut self) {
        if !self.placed {
            return;
        }
        for dir in &mut self.dirs {
            dir.position = dir.local_position;
            dir.scale = 1.0;
            dir.stretch = Vec2::ONE;
        }
        for file in self.files.values_mut() {
            file.position = file.local_position;
        }
        self.placed = false;
    }
    pub(crate) fn constrain_directory_fit(
        &self,
        regions: &mut [RepositoryRegion],
        origins: &mut [Vec2],
        scales: &mut [f32],
    ) {
        if self.placed {
            return;
        }
        let mut limits: Vec<_> = regions
            .iter()
            .map(|region| {
                region.tile.map(|tile| {
                    let (low, high) = super::outline::bounds(&tile.usable().ground());
                    (low.midpoint(high), (high - low) * 0.5, 1.0_f32)
                })
            })
            .collect();
        for (_, dir) in self.dirs() {
            let repo = dir.repository;
            let Some((center, half, fraction)) = &mut limits[repo] else {
                continue;
            };
            let local = regions[repo].local_axes(dir.position);
            let scale = scales[repo];
            let position = local * scale + origins[repo];
            let extent = (position - *center).abs()
                + regions[repo].stretch * (dir.radius() * scale);
            if extent.cmpgt(*half).any() {
                *fraction = fraction.min((*half / extent).min_element() * 0.999);
            }
        }
        for (repo, limit) in limits.into_iter().enumerate() {
            let Some((center, _, fraction)) =
                limit.filter(|(_, _, fraction)| *fraction < 1.0)
            else {
                continue;
            };
            let scale = |point| center + (point - center) * fraction;
            let region = &mut regions[repo];
            origins[repo] = scale(origins[repo]);
            scales[repo] *= fraction;
            region.low = scale(region.low);
            region.high = scale(region.high);
            region.height *= fraction;
            for point in &mut region.outline {
                *point = scale(*point);
            }
        }
    }

    pub(crate) fn place_regions(
        &mut self,
        regions: Vec<RepositoryRegion>,
        offsets: &[Vec2],
        scales: &[f32],
        files: super::FileMotion,
    ) {
        self.remove_offsets();
        for dir in &mut self.dirs {
            dir.local_position = dir.position;
            dir.scale = scales[dir.repository];
            dir.stretch = regions[dir.repository].stretch;
            let position = regions[dir.repository].local_axes(dir.position);
            dir.position = position * dir.scale + offsets[dir.repository];
        }
        if files != super::FileMotion::Hold {
            for file in self.files.values_mut() {
                file.local_position = file.position;
                let dir = &self.dirs[file.dir];
                let position = regions[dir.repository].local_axes(file.position);
                file.position = position * dir.scale + offsets[dir.repository];
            }
        }
        self.repositories = regions;
        self.placed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{Options, Simulation},
        theme::Theme,
    };

    fn history(index: usize) -> Vec<Event> {
        let mut events = crate::timeline::synthetic();
        events.truncate(10);
        for event in &mut events {
            event.author.name = b"Shared contributor".to_vec();
            event.author.email = format!("shared{index}@example.invalid").into_bytes();
            event.seconds += i64::try_from(index).unwrap() * 100;
        }
        crate::repositories::namespace(&mut events, index);
        events
    }

    fn snapshot(simulation: &Simulation) -> Vec<(u64, String, Vec2)> {
        simulation
            .layout()
            .files()
            .map(|(id, file)| (id, file.path.clone(), file.position))
            .collect()
    }

    #[test]
    fn chunks_finishing_in_reverse_come_back_in_item_order() {
        let finished = std::sync::Mutex::new(Vec::new());
        let turn = std::sync::Condvar::new();
        let items: Vec<usize> = (0..4).collect();
        let doubled = in_order(&items, 1, 4, |chunk| {
            let mut done = finished.lock().unwrap();
            while done.len() != items.len() - 1 - chunk[0] {
                let (guard, wait) = turn
                    .wait_timeout(done, std::time::Duration::from_secs(10))
                    .unwrap();
                assert!(!wait.timed_out(), "chunks did not run together");
                done = guard;
            }
            done.push(chunk[0]);
            turn.notify_all();
            vec![chunk[0] * 2]
        });
        assert_eq!(*finished.lock().unwrap(), [3, 2, 1, 0]);
        assert_eq!(doubled, [0, 2, 4, 6]);
        for (chunk, threads) in [(1, 2), (3, 2), (2, 0), (9, 4), (1, 1)] {
            assert_eq!(in_order(&items, chunk, threads, <[usize]>::to_vec), items);
        }
    }

    #[test]
    fn starting_a_tick_restores_local_positions_as_remove_offsets_does() {
        let mut simulation = Simulation::with_repositories(
            crate::repositories::merge((0..2).map(history).collect()),
            &Options::default(),
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
            &["First".to_owned(), "Second".to_owned()],
        );
        simulation.advance_to(90, 60);
        let placed = simulation.layout().clone();
        assert!(placed.placed);
        let mut fused = placed.clone();
        fused.begin_tick(
            crate::model::tick_seconds(),
            3.0,
            crate::layout::FileMotion::Advance,
        );
        let mut separate = placed.clone();
        separate.remove_offsets();
        assert_ne!(separate.exact_state(), placed.exact_state());
        separate.begin_tick(
            crate::model::tick_seconds(),
            3.0,
            crate::layout::FileMotion::Advance,
        );
        assert_eq!(fused.exact_state(), separate.exact_state());
        assert!(!fused.placed);
    }

    #[test]
    fn regions_remain_separate_and_one_contributor_moves_between_them() {
        let events = crate::repositories::merge((0..5).map(history).collect());
        let config = tempfile::tempdir().unwrap();
        let path = config.path().join("contributors.toml");
        std::fs::write(&path, "[[contributor]]\nname = 'Shared contributor'\nemails = ['shared0@example.invalid', 'shared1@example.invalid', 'shared2@example.invalid', 'shared3@example.invalid', 'shared4@example.invalid']\n").unwrap();
        let mappings = crate::contributors::load(Some(&path)).unwrap();
        let mut simulation = Simulation::with_repositories(
            events,
            &Options {
                seconds_per_day: 0.01,
                max_idle_seconds: crate::model::tick_seconds(),
                ..Options::default()
            },
            &Theme::neon(),
            &mappings,
            &[
                "First".to_owned(),
                "Second".to_owned(),
                "Third".to_owned(),
                "Fourth".to_owned(),
                "Fifth".to_owned(),
            ],
        );

        assert_eq!(
            simulation.duration(),
            simulation.scheduled_starts().last().copied().unwrap() + 2.5
        );
        let frames = crate::model::frames_for(simulation.duration(), 60).unwrap();
        for frame in 0..frames {
            simulation.tick_layout();
            let layout = simulation.layout();
            for (index, a) in layout
                .repositories
                .iter()
                .enumerate()
                .filter(|(_, r)| r.live)
            {
                for b in layout
                    .repositories
                    .iter()
                    .skip(index + 1)
                    .filter(|r| r.live)
                {
                    assert!(
                        a.tile.unwrap().high.x <= b.tile.unwrap().low.x + 0.0001
                            || b.tile.unwrap().high.x <= a.tile.unwrap().low.x + 0.0001
                            || a.tile.unwrap().high.y <= b.tile.unwrap().low.y + 0.0001
                            || b.tile.unwrap().high.y <= a.tile.unwrap().low.y + 0.0001,
                        "frame {frame}: {} overlaps {}",
                        a.label,
                        b.label
                    );
                }
            }
            for (_, file) in layout
                .files()
                .filter(|(_, f)| f.visible_presence(layout.dir(f.dir)) > 0.0)
            {
                let region = &layout.repositories[layout.dir(file.dir).repository];
                assert!(
                    file.position.cmpge(region.low - Vec2::splat(0.0001)).all(),
                    "frame {frame}: {} point={:?} low={:?} high={:?}",
                    file.path,
                    file.position,
                    region.low,
                    region.high
                );
                assert!(
                    file.position.cmple(region.high + Vec2::splat(0.0001)).all(),
                    "frame {frame}: {} point={:?} low={:?} high={:?}",
                    file.path,
                    file.position,
                    region.low,
                    region.high
                );
            }
            assert!(simulation.avatars().len() <= 1);
        }
        let before = snapshot(&simulation);
        simulation.restart();
        for _ in 0..frames {
            simulation.tick_layout();
        }

        assert_eq!(before, snapshot(&simulation));
    }

    #[test]
    fn same_paths_and_rename_identity_are_independent_per_repository() {
        let events = crate::repositories::merge(vec![history(0), history(1)]);
        let starts = crate::model::schedule::Schedule::new(&events, 0.5, 1.0).starts;
        let mut layout = Layout::scheduled_repositories(
            &events,
            &starts,
            &["Same".to_owned(), "Same".to_owned()],
            &crate::progress::Progress::default(),
        );
        let first = layout.add("0/src/layout.rs", 0.0, 0);
        let second = layout.add("1/src/layout.rs", 0.0, 0);
        assert_ne!(first, second);
        layout.settle(120, crate::model::tick_seconds());
        let mut previous = layout.file(first).unwrap().position;
        assert_eq!(
            layout.rename("0/src/layout.rs", "0/src/scene/layout.rs", 0),
            first
        );
        for _ in 0..120 {
            layout.step(crate::model::tick_seconds());
            let position = layout.file(first).unwrap().position;
            assert!(position.distance(previous) <= 0.051);
            previous = position;
        }
        assert_eq!(layout.file(second).unwrap().path, "1/src/layout.rs");
        assert_ne!(
            layout.file(first).unwrap().dir,
            layout.file(second).unwrap().dir
        );
    }
}
