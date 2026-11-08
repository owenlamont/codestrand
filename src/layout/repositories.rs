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
        let mut cursor = 0.0;
        for (repository, label) in names.iter().enumerate() {
            let prefix = crate::repositories::prefix(repository);
            let mut local = Vec::new();
            let mut times = Vec::new();
            for (event, &time) in events.iter().zip(starts) {
                if !event
                    .changes
                    .first()
                    .is_some_and(|change| change.path.starts_with(&prefix))
                {
                    continue;
                }
                local.push(crate::repositories::local_event(event, &prefix));
                times.push(time);
            }
            let mut layout = Self::scheduled(&local, &times);
            let (low, high) = layout.plan.envelope();
            let offset = Vec2::new(cursor - low.x, -(low.y + high.y) * 0.5);
            let region = RepositoryRegion {
                label: label.clone(),
                low: low + offset,
                high: high + offset,
            };
            cursor = region.high.x + 4.0;
            combined.repositories.push(region);
            layout.plan.translate(offset);
            let base = combined.dirs.len();
            for (index, mut dir) in layout.dirs.into_iter().enumerate() {
                dir.repository = repository;
                dir.path = if index == 0 {
                    repository.to_string()
                } else {
                    format!("{prefix}{}", dir.path)
                };
                dir.parent = dir.parent.map(|parent| base + parent);
                dir.position += offset;
                combined.dir_index.insert(dir.path.clone(), base + index);
                combined.dirs.push(dir);
            }
            combined.plan.add_part(base, layout.plan);
        }
        let centre = cursor * 0.5 - 2.0;
        for region in &mut combined.repositories {
            region.low.x -= centre;
            region.high.x -= centre;
        }
        for dir in &mut combined.dirs {
            dir.position.x -= centre;
        }
        for (_, plan) in &mut combined.plan.parts {
            plan.translate(Vec2::new(-centre, 0.0));
        }
        combined
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
        let events = crate::repositories::merge(vec![history(0), history(1)]);
        let config = tempfile::tempdir().unwrap();
        let path = config.path().join("contributors.toml");
        std::fs::write(&path, "[[contributor]]\nname = 'Shared contributor'\nemails = ['shared0@example.invalid', 'shared1@example.invalid']\n").unwrap();
        let mappings = crate::contributors::load(Some(&path)).unwrap();
        let mut simulation = Simulation::with_repositories(
            events,
            &Options::default(),
            &Theme::neon(),
            &mappings,
            &["First".to_owned(), "Second".to_owned()],
        );
        let regions = simulation.layout().repositories.clone();
        assert!(regions[0].high.x < regions[1].low.x);
        let mut avatar_positions = Vec::new();
        for frame in 0..360 {
            simulation.advance_to(frame, 30);
            let layout = simulation.layout();
            for (_, dir) in layout.dirs() {
                let region = &regions[dir.repository];
                assert!(
                    (dir.position - Vec2::splat(dir.radius()))
                        .cmpge(region.low)
                        .all()
                );
                assert!(
                    (dir.position + Vec2::splat(dir.radius()))
                        .cmple(region.high)
                        .all()
                );
            }
            for (_, file) in layout.files() {
                let region = &regions[layout.dir(file.dir).repository];
                assert!(file.position.cmpge(region.low).all());
                assert!(file.position.cmple(region.high).all());
            }
            assert!(simulation.avatars().len() <= 1);
            if let Some(avatar) = simulation.avatars().first() {
                avatar_positions.push(avatar.position);
            }
            assert_eq!(
                simulation
                    .labels()
                    .iter()
                    .filter(|label| label.kind == crate::model::LabelKind::Repository)
                    .count(),
                2
            );
        }
        assert!(
            avatar_positions
                .windows(2)
                .any(|pair| pair[0].distance(pair[1]) > 0.01)
        );
        let snapshot = |simulation: &Simulation| {
            simulation
                .layout()
                .files()
                .map(|(id, file)| (id, file.path.clone(), file.position))
                .collect::<Vec<_>>()
        };
        simulation.advance_to(100, 30);
        simulation.repository_label_plane = crate::repositories::LabelPlane::Raised;
        let lowest = simulation
            .avatars()
            .iter()
            .filter(|avatar| avatar.presence > 0.05)
            .map(|avatar| avatar.position.y)
            .reduce(f32::min)
            .unwrap();
        assert!(
            simulation
                .labels()
                .iter()
                .filter(|label| label.kind == crate::model::LabelKind::Repository)
                .all(|label| (label.position.y - lowest * 0.5).abs() < f32::EPSILON)
        );
        let before = snapshot(&simulation);
        simulation.advance_to(150, 30);
        simulation.advance_to(100, 30);
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
