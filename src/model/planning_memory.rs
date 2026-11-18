use std::{collections::HashSet, mem::size_of};

use super::{Options, Schedule, frames_for, travel};
use crate::{
    layout::RepositoryRegion,
    timeline::{Event, FileChange, Kind},
};

/// Available physical RAM in bytes at run startup, including memory the OS can
/// reclaim without swapping. Swap is excluded. The snapshot is reused for every
/// padded schedule and window; it is not a reservation against other processes.
#[cfg(not(test))]
pub(crate) fn available(trace: &super::Trace) -> anyhow::Result<u64> {
    use anyhow::Context;
    let mut system = sysinfo::System::new()
        .context("cannot read available physical memory; planning stopped")?;
    system.refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
    let available =
        validate_available(system.total_memory(), system.available_memory())?;
    if trace.on() {
        eprintln!("planning_available_memory_bytes={available}");
    }
    Ok(available)
}

fn validate_available(total: u64, available: u64) -> anyhow::Result<u64> {
    anyhow::ensure!(
        total > 0 && available <= total,
        "cannot read available physical memory; planning stopped; check operating-system memory reporting and retry"
    );
    Ok(available)
}

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
    let snapshot_bytes =
        super::slots::Snapshots::estimated_bytes(selected, options.fragment_pace());
    let history = Schedule::with_padding(
        selected,
        options.seconds_per_day,
        options.max_idle_seconds,
        |_| 0,
    );
    let (_, endings) = super::lifecycle::plan(
        events,
        &history,
        options,
        options.selected_range(events.len()),
    );
    let history_duration =
        super::lifecycle::duration(&history, &endings, options.fragment_pace());
    let history_frames = u128::from(frames_for(history_duration, 60)?) + 1;
    let regions = history_frames * repositories.len() as u128;
    let labels = repositories
        .iter()
        .map(|label| label.len() as u128)
        .sum::<u128>();
    let packing_bytes = regions
        * crate::layout::packing::Packing::input_bytes() as u128
        + history_frames * crate::layout::packing::Packing::row_bytes() as u128;
    let conversion =
        history_frames * crate::layout::packing::Packing::conversion_bytes() as u128;
    let row_bytes = repositories.len() as u128
        * (size_of::<RepositoryRegion>()
            + crate::layout::outline::NORMALS.len() * size_of::<bevy::math::Vec2>())
            as u128
        + labels;
    let changes = events
        .iter()
        .map(|event| event.changes.len() as u128)
        .sum::<u128>();
    let paths = events
        .iter()
        .flat_map(|event| &event.changes)
        .map(|change| change.path.len() as u128)
        .sum::<u128>();
    let input = changes * size_of::<FileChange>() as u128
        + events.len() as u128 * size_of::<Event>() as u128
        + paths;
    let (layout, layout_peak, directories) =
        layout_bytes(events, options, &history, repositories.len());
    let files = events
        .iter()
        .flat_map(|event| &event.changes)
        .map(|change| change.path.as_str())
        .collect::<HashSet<_>>()
        .len();
    // The global replay holds a batch of region samples and the rows computed from them.
    let batch = super::REGION_BATCH as u128
        * (crate::layout::RegionSample::bytes(directories, files) as u128 + row_bytes);
    // Resident code and allocator bookkeeping remain outside the retained payloads.
    let overhead = 64 * 1024 * 1024 + regions * 16 + history_frames * 3 * 16;
    let retained = packing_bytes + labels + layout + input + overhead;
    let global =
        (packing_bytes + conversion + row_bytes + batch + labels + layout + input * 2)
            .max(layout_peak + input * 3)
            + overhead;
    if options.trace.on() {
        eprintln!(
            "planning_memory_parts packing={packing_bytes} layout_retained={layout} layout_peak={layout_peak} input={input} overhead={overhead} global={global}"
        );
    }
    if options.travel_window_seconds.is_some() {
        return check_streaming(
            events,
            selected,
            options,
            repositories.len(),
            (&history, history_duration),
            schedule,
            (retained, global),
        );
    }
    let bytes = global.max(
        retained
            + pose_bytes.max(snapshot_bytes)
            + frames * (8 * size_of::<Vec<()>>()) as u128,
    );
    check_bytes(bytes, options.available_memory)
}

fn checkpoint_bytes(events: &[Event], repositories: usize) -> u128 {
    let paths: HashSet<_> = events
        .iter()
        .flat_map(|e| &e.changes)
        .map(|c| c.path.as_str())
        .collect();
    let files = paths
        .iter()
        .map(|p| {
            (std::mem::size_of::<crate::layout::File>() + 256 + p.len() * 3) as u128
        })
        .sum::<u128>();
    let mut directories = HashSet::new();
    for mut path in paths {
        while let Some((parent, _)) = path.rsplit_once('/') {
            directories.insert(parent);
            path = parent;
        }
    }
    let directory_bytes = directories
        .iter()
        .map(|path| (size_of::<crate::layout::Dir>() + 128 + path.len() * 2) as u128)
        .sum::<u128>();
    let pulse_data = events
        .iter()
        .flat_map(|event| &event.changes)
        .map(|change| {
            64 + change
                .samples
                .iter()
                .map(|sample| {
                    (size_of::<crate::diff_samples::Sample>() + sample.text.len())
                        as u128
                })
                .sum::<u128>()
        })
        .sum::<u128>();
    let services = author_count(events) * (size_of::<super::Avatar>() + 512) as u128
        + events.len() as u128 * size_of::<super::Pulse>() as u128;
    files
        + directory_bytes
        + pulse_data
        + services
        + repositories as u128 * 1024
        + std::mem::size_of::<super::State>() as u128
}

fn author_count(events: &[Event]) -> u128 {
    events
        .iter()
        .map(|event| {
            let identity = event.contributor.as_ref().map(|c| &c.key);
            (
                identity,
                identity
                    .is_none()
                    .then_some((&event.author.name, &event.author.email)),
            )
        })
        .collect::<HashSet<_>>()
        .len() as u128
}

fn check_bytes(bytes: u128, available: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        bytes <= u128::from(available),
        "estimated planning memory {bytes} bytes ({} MiB) exceeds available physical memory {available} bytes ({} MiB) at startup; use fewer repositories or a later --start-date",
        bytes.div_ceil(1024 * 1024),
        available / (1024 * 1024)
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
    repositories: usize,
) -> (u128, u128, usize) {
    let mut directories = vec![HashSet::from([""]); repositories];
    let prefixes: Vec<_> = (0..repositories).map(crate::repositories::prefix).collect();
    for event in events {
        for change in &event.changes {
            let source = match &change.kind {
                Kind::Moved { from } => Some(from.as_str()),
                _ => None,
            };
            for path in std::iter::once(change.path.as_str()).chain(source) {
                let mut path = path
                    .strip_prefix(&prefixes[event.repository])
                    .unwrap_or(path);
                while let Some((parent, _)) = path.rsplit_once('/') {
                    directories[event.repository].insert(parent);
                    path = parent;
                }
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
    let mut ends = vec![super::Time::seconds(0.5); repositories];
    for (event, &start) in events.iter().zip(&schedule.starts) {
        ends[event.repository] = start;
    }
    let count = directories.iter().map(HashSet::len).sum();
    let (retained, peak) =
        directories
            .iter()
            .zip(ends)
            .fold((0, 0), |(retained, peak), (dirs, end)| {
                let ticks = u128::from((end + 2.5).ticks(true));
                let samples = ticks / (ticks / 8192 + 1).max(15) + 2;
                let rows = dirs.len() as u128;
                (
                    retained + rows * (samples * 8 + 24),
                    peak + rows * (samples * 16 + 72) + samples * 128,
                )
            });
    (retained, peak, count)
}

fn check_streaming(
    events: &[Event],
    selected: &[Event],
    options: &Options,
    repositories: usize,
    (history, history_duration): (&Schedule, super::Time),
    schedule: &Schedule,
    (retained, global): (u128, u128),
) -> anyhow::Result<()> {
    let seconds = options.travel_window_seconds.expect("window option");
    anyhow::ensure!(
        seconds.is_finite() && seconds >= super::tick_seconds(),
        "travel window must be at least one tick of finite history time"
    );
    let clock = super::schedule::Clock::new(history.clone(), schedule.clone());
    let mut begin = super::Time::ZERO;
    let mut span = 0.0_f32;
    while begin < history_duration {
        let end = (begin + seconds + super::windows::MARGIN).min(history_duration);
        span = span.max(clock.present(end) - clock.present(begin));
        begin += seconds;
    }
    let frames = u128::from(travel::seconds_ticks(span, true)) + 145;
    let authors = author_count(selected);
    let window = frames
        * (authors
            * (2 * size_of::<travel::Pose>() + 3 * size_of::<bevy::math::Vec2>())
                as u128
            + (8 * size_of::<Vec<()>>()) as u128);
    let checkpoint = checkpoint_bytes(events, repositories);
    let states = super::streaming::STATE_COPIES * checkpoint;
    let windows = super::streaming::PLAN_COPIES * window;
    let global_states = super::streaming::GLOBAL_STATE_COPIES * checkpoint;
    let bytes = (global + global_states).max(retained + states + windows);
    if options.trace.on() {
        eprintln!(
            "streaming_memory global_peak={global} global_retained={retained} window={window} checkpoint={checkpoint} global_states={global_states} window_states={states} total={bytes}"
        );
    }
    check_bytes(bytes, options.available_memory).map_err(|error| error.context(format!("streaming memory: global peak {global} plus state copies {global_states}, global retained {retained}, three windows {windows}, ten window state copies {states}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_reports_estimated_memory_and_limit() {
        let available = 1024 * 1024;
        assert!(check_bytes(u128::from(available), available).is_ok());
        assert_eq!(
            check_bytes(u128::from(available) + 1, available)
                .unwrap_err()
                .to_string(),
            "estimated planning memory 1048577 bytes (2 MiB) exceeds available physical memory 1048576 bytes (1 MiB) at startup; use fewer repositories or a later --start-date"
        );
    }

    #[test]
    fn unavailable_reading_stops_planning_but_zero_available_is_valid() {
        assert!(validate_available(0, 0).is_err());
        assert!(validate_available(10, 11).is_err());
        assert_eq!(validate_available(10, 0).unwrap(), 0);
        assert!(check_bytes(1, 0).is_err());
        assert_eq!(validate_available(10, 4).unwrap(), 4);
    }

    #[test]
    fn a_batch_of_region_samples_counts_towards_the_estimate() {
        let mut events = crate::timeline::synthetic();
        events.truncate(1);
        let template = events[0].changes[0].clone();
        events[0].changes = (0..40_000)
            .map(|index| FileChange {
                path: format!("f{index}"),
                ..template.clone()
            })
            .collect();
        let schedule = Schedule::with_padding(&events, 1.0, 1.0, |_| 0);
        let batch = 512 * crate::layout::RegionSample::bytes(1, 40_000) as u64;
        let fits = |available_memory| {
            let options = Options {
                available_memory,
                ..Options::default()
            };
            check(
                &events,
                &options,
                &["r".into()],
                &schedule,
                schedule.duration(),
            )
        };
        assert!(fits(u64::MAX).is_ok());
        assert!(fits(batch).is_err());
    }

    #[test]
    fn actual_padding_contributes_to_the_pose_estimate() {
        let mut events = crate::timeline::synthetic();
        events.truncate(1);
        let options = Options::default();
        let labels = ["r".repeat(4096)];
        let short = Schedule::with_padding(&events, 1.0, 1.0, |_| 0);
        check(&events, &options, &labels, &short, short.duration()).unwrap();
        let padded = Schedule::with_padding(&events, 1.0, 1.0, |_| 60_000_000);
        let error =
            check(&events, &options, &labels, &padded, padded.duration()).unwrap_err();
        assert!(error.to_string().contains("estimated planning memory"));
        assert!(error.to_string().contains(
            "exceeds available physical memory 2147483648 bytes (2048 MiB) at startup"
        ));
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
