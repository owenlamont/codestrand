use bevy::math::Vec2;

use super::{Layout, RepositoryRegion};
use crate::timeline::Event;

impl Layout {
    pub(crate) fn scheduled_repositories(
        events: &[Event],
        starts: &[f32],
        names: &[String],
    ) -> Self {
        if names.is_empty() {
            return Self::scheduled(events, starts);
        }
        let mut combined = Self::empty();
        combined.dirs.clear();
        combined.dir_index.clear();
        for (repository, label) in names.iter().enumerate() {
            let prefix = crate::repositories::prefix(repository);
            let mut local = Vec::new();
            let mut times = Vec::new();
            for (event, &time) in events.iter().zip(starts) {
                if event.repository != repository {
                    continue;
                }
                local.push(crate::repositories::local_event(event, &prefix));
                times.push(time);
            }
            let layout = Self::scheduled(&local, &times);

            let region = RepositoryRegion {
                label: label.clone(),
                low: Vec2::splat(-2.0),
                high: Vec2::splat(2.0),
                live: false,
            };
            combined.repositories.push(region);
            let base = combined.dirs.len();
            for (index, mut dir) in layout.dirs.into_iter().enumerate() {
                dir.repository = repository;
                dir.path = if index == 0 {
                    repository.to_string()
                } else {
                    format!("{prefix}{}", dir.path)
                };
                dir.parent = dir.parent.map(|parent| base + parent);
                combined.dir_index.insert(dir.path.clone(), base + index);
                combined.dirs.push(dir);
            }
            combined.plan.add_part(base, layout.plan);
        }
        combined
    }

    pub(crate) fn local_regions(&self, active: &[bool]) -> Vec<RepositoryRegion> {
        self.repositories
            .iter()
            .enumerate()
            .map(|(repository, region)| {
                let mut low = Vec2::splat(f32::INFINITY);
                let mut high = Vec2::splat(f32::NEG_INFINITY);
                for (_, dir) in self.dirs().filter(|(_, dir)| {
                    dir.repository == repository && (dir.files > 0 || dir.children > 0)
                }) {
                    low = low.min(
                        crate::layout::region_coordinates(dir.position)
                            - Vec2::splat(dir.radius() + 1.0),
                    );
                    high = high.max(
                        crate::layout::region_coordinates(dir.position)
                            + Vec2::splat(dir.radius() + 1.0),
                    );
                }
                for (_, file) in self
                    .files()
                    .filter(|(_, file)| self.dirs[file.dir].repository == repository)
                {
                    low = low.min(
                        crate::layout::region_coordinates(file.position) - Vec2::ONE,
                    );
                    high = high.max(
                        crate::layout::region_coordinates(file.position) + Vec2::ONE,
                    );
                }
                if !low.is_finite() {
                    low = Vec2::splat(-2.0);
                    high = Vec2::splat(2.0);
                }
                RepositoryRegion {
                    label: region.label.clone(),
                    low,
                    high,
                    live: active[repository],
                }
            })
            .collect()
    }

    pub(crate) fn remove_offsets(&mut self) {
        if !self.placed {
            return;
        }
        for dir in &mut self.dirs {
            dir.position = dir.local_position;
            dir.scale = 1.0;
        }
        for file in self.files.values_mut() {
            file.position = file.local_position;
        }
        self.placed = false;
    }
    pub(crate) fn place_regions(
        &mut self,
        regions: Vec<RepositoryRegion>,
        offsets: Vec<Vec2>,
        scales: &[f32],
    ) {
        self.remove_offsets();
        let offsets: Vec<_> =
            offsets.into_iter().map(super::world_coordinates).collect();
        for dir in &mut self.dirs {
            dir.local_position = dir.position;
            dir.scale = scales[dir.repository];
            dir.position = dir.position * dir.scale + offsets[dir.repository];
        }
        for file in self.files.values_mut() {
            file.local_position = file.position;
            let dir = &self.dirs[file.dir];
            file.position = file.position * dir.scale + offsets[dir.repository];
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
                        a.high.x <= b.low.x + 0.0001
                            || b.high.x <= a.low.x + 0.0001
                            || a.high.y <= b.low.y + 0.0001
                            || b.high.y <= a.low.y + 0.0001,
                        "frame {frame}: {} overlaps {}",
                        a.label,
                        b.label
                    );
                }
            }
            for (_, file) in layout.files() {
                let region = &layout.repositories[layout.dir(file.dir).repository];
                assert!(
                    crate::layout::region_coordinates(file.position)
                        .cmpge(region.low - Vec2::splat(0.0001))
                        .all(),
                    "frame {frame}: {} point={:?} low={:?} high={:?}",
                    file.path,
                    crate::layout::region_coordinates(file.position),
                    region.low,
                    region.high
                );
                assert!(
                    crate::layout::region_coordinates(file.position)
                        .cmple(region.high + Vec2::splat(0.0001))
                        .all(),
                    "frame {frame}: {} point={:?} low={:?} high={:?}",
                    file.path,
                    crate::layout::region_coordinates(file.position),
                    region.low,
                    region.high
                );
            }
            assert!(simulation.avatars().len() <= 1);
        }
        let snapshot = |simulation: &Simulation| {
            simulation
                .layout()
                .files()
                .map(|(id, file)| (id, file.path.clone(), file.position))
                .collect::<Vec<_>>()
        };

        simulation.repository_label_plane = crate::repositories::LabelPlane::Raised;
        assert!(
            simulation
                .labels()
                .iter()
                .filter(|label| label.kind == crate::model::LabelKind::Repository)
                .all(|label| (label.position.y - 1.6).abs() < f32::EPSILON)
        );
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
        let starts = crate::model::event_starts(&events, 0.5, 1.0);
        let mut layout = Layout::scheduled_repositories(
            &events,
            &starts,
            &["Same".to_owned(), "Same".to_owned()],
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
