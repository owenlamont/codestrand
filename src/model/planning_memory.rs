use std::{collections::HashSet, mem::size_of};

use super::{Options, Schedule, frames_for, travel};
use crate::{
    layout::RepositoryRegion,
    timeline::{Event, FileChange, Kind},
};

const LIMIT: u128 = 2 * 1024 * 1024 * 1024;

pub(super) fn check(
    events: &[Event],
    options: &Options,
    repositories: &[String],
    schedule: &Schedule,
    duration: super::Time,
) -> anyhow::Result<()> {
    let frames = u128::from(frames_for(duration, 60)?) + 1;
    let selected = &events[options.selected_range(events.len())];
    let poses = pose_count(selected, schedule, frames)?;
    let pose_bytes = poses * size_of::<travel::Pose>() as u128 * 2;
    let snapshot_bytes = super::slots::Snapshots::estimated_bytes(selected);
    let regions = frames * repositories.len() as u128;
    let labels = repositories
        .iter()
        .map(|label| label.len() as u128)
        .sum::<u128>();
    let region_bytes = 2 * size_of::<RepositoryRegion>()
        + 2 * crate::layout::outline::NORMALS.len() * size_of::<bevy::math::Vec2>()
        + 2 * size_of::<Option<crate::layout::packing::Tile>>()
        + 3 * size_of::<f32>();
    let changes = events
        .iter()
        .map(|event| event.changes.len() as u128)
        .sum::<u128>();
    let paths = events
        .iter()
        .flat_map(|event| &event.changes)
        .map(|change| change.path.len() as u128)
        .sum::<u128>();
    let bytes = regions * region_bytes as u128
        + frames * labels * 2
        + pose_bytes.max(snapshot_bytes)
        + frames * (8 * size_of::<Vec<()>>()) as u128
        + changes * (3 * size_of::<FileChange>()) as u128
        + events.len() as u128 * (3 * size_of::<Event>()) as u128
        + paths * 3
        + layout_bytes(events, options, schedule)?;
    check_bytes(bytes)
}

fn check_bytes(bytes: u128) -> anyhow::Result<()> {
    anyhow::ensure!(
        bytes <= LIMIT,
        "estimated planning memory {bytes} bytes ({} MiB) exceeds the {} MiB planning memory limit; shorten the selected history or reduce playback times",
        bytes.div_ceil(1024 * 1024),
        LIMIT / (1024 * 1024)
    );
    Ok(())
}

fn pose_count(
    events: &[Event],
    schedule: &Schedule,
    frames: u128,
) -> anyhow::Result<u128> {
    // Faded-out docks can follow a moving gutter every tick, so change capture retains this upper bound.
    let mut seen = HashSet::new();
    let mut poses = 0;
    for (event, &start) in events.iter().zip(&schedule.starts) {
        let identity = event
            .contributor
            .as_ref()
            .map(|contributor| &contributor.key);
        if seen.insert((
            identity,
            identity
                .is_none()
                .then_some((&event.author.name, &event.author.email)),
        )) {
            poses += frames.saturating_sub(u128::from(frames_for(start, 60)?));
        }
    }
    Ok(poses)
}

fn layout_bytes(
    events: &[Event],
    options: &Options,
    selected: &Schedule,
) -> anyhow::Result<u128> {
    let mut directories = HashSet::new();
    for change in events.iter().flat_map(|event| &event.changes) {
        let source = match &change.kind {
            Kind::Moved { from } => Some(from.as_str()),
            _ => None,
        };
        for mut path in std::iter::once(change.path.as_str()).chain(source) {
            while let Some((parent, _)) = path.rsplit_once('/') {
                directories.insert(parent);
                path = parent;
            }
        }
    }
    let schedule = Schedule::with_padding(
        events,
        options.seconds_per_day,
        options.max_idle_seconds,
        |index| {
            index
                .checked_sub(options.start)
                .filter(|&index| index < selected.starts.len())
                .map_or(0, |index| selected.padding(index))
        },
    );
    let ticks = u128::from(frames_for(schedule.duration(), 60)?);
    let samples = ticks / (ticks / 8192 + 1).max(15) + 2;
    Ok((directories.len() as u128 + 1) * samples * 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_reports_estimated_memory_and_limit() {
        assert!(check_bytes(LIMIT).is_ok());
        assert_eq!(
            check_bytes(LIMIT + 1).unwrap_err().to_string(),
            "estimated planning memory 2147483649 bytes (2049 MiB) exceeds the 2048 MiB planning memory limit; shorten the selected history or reduce playback times"
        );
    }

    #[test]
    fn actual_padding_and_label_bytes_contribute_to_the_estimate() {
        let mut events = crate::timeline::synthetic();
        events.truncate(1);
        let options = Options::default();
        let labels = ["r".repeat(4096)];
        let short = Schedule::with_padding(&events, 1.0, 1.0, |_| 0);
        check(&events, &options, &labels, &short, short.duration()).unwrap();
        let padded = Schedule::with_padding(&events, 1.0, 1.0, |_| 360_000);
        let error =
            check(&events, &options, &labels, &padded, padded.duration()).unwrap_err();
        assert!(error.to_string().contains("estimated planning memory"));
        assert!(
            error
                .to_string()
                .contains("exceeds the 2048 MiB planning memory limit")
        );
    }

    #[test]
    fn late_contributors_only_count_poses_after_their_arrival() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        events[1].author.email = b"late@test.invalid".to_vec();
        let schedule = Schedule::with_padding(&events, 1.0, 1.0, |_| 0);
        let frames = u128::from(frames_for(schedule.duration(), 60).unwrap()) + 1;
        let expected = schedule
            .starts
            .iter()
            .map(|&start| frames - u128::from(frames_for(start, 60).unwrap()))
            .sum::<u128>();
        assert_eq!(pose_count(&events, &schedule, frames).unwrap(), expected);
        assert!(expected < frames * 2);
    }
}
