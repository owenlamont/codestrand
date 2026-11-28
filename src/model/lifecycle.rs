use std::sync::Arc;

use super::{FragmentPace, Schedule, TICK_RATE, Time, tick_seconds};
use crate::timeline::Event;

pub(super) const EXIT: f32 = 1.2;
pub(super) const REPOSITORY_EXIT: f32 = 2.5;
const REFLOW: f32 = 4.0;
const FADE_TICKS: u64 = 5 * TICK_RATE as u64 / 2;
const REFLOW_TICKS: u64 = 4 * TICK_RATE as u64;
const STAGGERED_REFLOW_TICKS: u64 = 5 * TICK_RATE as u64;
pub(super) const MAX_BIRTH_ARCHIVE_WAIT: u64 =
    STAGGERED_REFLOW_TICKS + super::BIRTH_TICKS;

/// The first and last sample of a tile's exit on the unpadded history clock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RepositoryExit {
    pub(crate) start: u64,
    pub(crate) end: u64,
}

pub(super) fn repository_exits(endings: &[Time]) -> Arc<[Option<RepositoryExit>]> {
    let starts: Vec<_> = endings
        .iter()
        .map(|&end| {
            (end.is_finite() && end >= Time::ZERO)
                .then(|| super::schedule::first_tick(end).saturating_add(FADE_TICKS))
        })
        .collect();
    let mut exits = exits_from_starts(&starts);
    for (exit, &ending) in exits.iter_mut().zip(endings) {
        if ending < Time::ZERO {
            *exit = Some(RepositoryExit { start: 0, end: 0 });
        }
    }
    exits.into()
}

pub(crate) fn exits_from_starts(starts: &[Option<u64>]) -> Vec<Option<RepositoryExit>> {
    let mut distinct: Vec<_> = starts.iter().flatten().copied().collect();
    distinct.sort_unstable();
    distinct.dedup();
    starts
        .iter()
        .map(|start| {
            let start = (*start)?;
            let i = distinct.binary_search(&start).expect("recorded exit start");
            let overlaps = (i > 0 && start - distinct[i - 1] < REFLOW_TICKS)
                || distinct
                    .get(i + 1)
                    .is_some_and(|&next| next - start < REFLOW_TICKS);
            let duration = if overlaps {
                STAGGERED_REFLOW_TICKS
            } else {
                REFLOW_TICKS
            };
            Some(RepositoryExit {
                start,
                end: start.saturating_add(duration),
            })
        })
        .collect()
}

pub(super) fn exit_end(exits: &[Option<RepositoryExit>]) -> u64 {
    exits
        .iter()
        .flatten()
        .map(|exit| exit.end)
        .max()
        .unwrap_or(0)
}

pub(super) fn duration(
    schedule: &Schedule,
    endings: &[Time],
    pace: FragmentPace,
) -> Time {
    activity_duration(schedule, pace)
        .max(Time::from_tick(exit_end(&repository_exits(endings))))
}

pub(super) fn activity_duration(schedule: &Schedule, pace: FragmentPace) -> Time {
    let last_pulse = schedule
        .starts
        .last()
        .map_or(Time::ZERO, |&start| pace.pulse_end(start));
    let last_ring = schedule
        .last_ring_service()
        .map_or(Time::ZERO, |start| pace.pulse_end(start) + super::DISBAND);
    schedule.duration().max(last_pulse).max(last_ring)
}

/// For each played event, bit `rank` is set when its participant at that rank (the author
/// first, then co-authors) takes part in no later played event before its departure, so
/// a co-author left behind fades with its group's ring rather than waiting alone.
pub(super) fn finals(
    events: &[Event],
    starts: &[Time],
    departures: &[Time],
) -> Vec<u16> {
    let mut next = std::collections::HashMap::new();
    let mut finals = vec![0; events.len()];
    for (index, event) in events.iter().enumerate().rev() {
        for (rank, contributor) in event.participants().enumerate() {
            let later = next.insert(contributor.key.clone(), index);
            if later.is_none_or(|later| starts[later] >= departures[index]) {
                finals[index] |= 1 << rank;
            }
        }
    }
    finals
}

/// For each selected event, bit `rank` is set when its participant at that rank (the
/// author first, then co-authors) appears for the first time in the whole history.
pub(super) fn arrivals(events: &[Event], selected: std::ops::Range<usize>) -> Vec<u16> {
    let mut seen = std::collections::HashSet::new();
    events
        .iter()
        .map(|event| {
            assert!(event.contributor.is_some(), "resolved contributor");
            event
                .participants()
                .enumerate()
                .filter(|(_, contributor)| seen.insert(contributor.key.clone()))
                .fold(0, |bits, (rank, _)| bits | 1 << rank)
        })
        .skip(selected.start)
        .take(selected.len())
        .collect()
}

pub(super) fn plan(
    events: &[Event],
    schedule: &Schedule,
    options: &super::Options,
    selected: std::ops::Range<usize>,
) -> (Vec<Time>, Vec<Time>) {
    let endings = planned_repository_ends(events, schedule, options, selected.clone());
    (
        departures(
            &events[selected],
            schedule,
            options.contributor_inactivity_days,
            options.fragment_pace(),
        ),
        endings,
    )
}

pub(super) fn planned_repository_ends(
    events: &[Event],
    schedule: &Schedule,
    options: &super::Options,
    selected: std::ops::Range<usize>,
) -> Vec<Time> {
    let archived: Vec<_> = options
        .repository_archived
        .iter()
        .enumerate()
        .map(|(repository, &archived)| {
            archived
                && options.unread.after(repository) == 0
                && events
                    .iter()
                    .rposition(|event| event.repository == repository)
                    .is_some_and(|last| last < selected.end)
        })
        .collect();
    let complete =
        selected.end == events.len() && options.unread.after.iter().all(|&n| n == 0);
    repository_ends(&events[selected], schedule, options, &archived, complete)
}

pub(super) fn repository_ends(
    events: &[Event],
    schedule: &Schedule,
    options: &super::Options,
    archived: &[bool],
    complete: bool,
) -> Vec<Time> {
    options
        .repository_ends
        .iter()
        .enumerate()
        .map(|(repository, date)| {
            date.map_or_else(
                || {
                    if archived.get(repository) != Some(&true) {
                        return Time::INFINITY;
                    }
                    events
                        .iter()
                        .zip(&schedule.starts)
                        .filter(|(event, _)| event.repository == repository)
                        .map(|(_, &start)| options.fragment_pace().pulse_end(start))
                        .next_back()
                        .unwrap_or(Time::seconds(-REPOSITORY_EXIT - REFLOW))
                },
                |date| ending_time(events, schedule, date, options, complete),
            )
        })
        .collect()
}

fn ending_time(
    events: &[Event],
    schedule: &Schedule,
    date: i64,
    options: &super::Options,
    complete: bool,
) -> Time {
    if events.first().is_some_and(|event| date < event.seconds) {
        return Time::seconds(-REPOSITORY_EXIT - REFLOW);
    }
    let time = mapped_time(&calendar_dates(events), schedule, date);
    if time.is_finite() || !complete {
        return time;
    }
    let Some(last) = events.iter().map(|event| event.seconds).max() else {
        return Time::INFINITY;
    };
    let days = std::time::Duration::from_secs(date.saturating_sub(last).unsigned_abs())
        .as_secs_f32()
        / 86_400.0;
    schedule
        .starts
        .last()
        .copied()
        .unwrap_or(Time::seconds(super::LEAD_IN))
        + (days * options.seconds_per_day)
            .min(options.max_idle_seconds)
            .max(super::PULSE_SECONDS + options.fragment_pace().flight())
}

fn calendar_dates(events: &[Event]) -> Vec<i64> {
    let mut latest = i64::MIN;
    events
        .iter()
        .map(|event| {
            latest = latest.max(event.seconds);
            latest
        })
        .collect()
}

fn mapped_time(dates: &[i64], schedule: &Schedule, date: i64) -> Time {
    let next = dates.partition_point(|&seconds| seconds < date);
    let Some(&start) = schedule.starts.get(next) else {
        return Time::INFINITY;
    };
    next.checked_sub(1).map_or(start, |previous| {
        let seconds = dates[previous];
        let time = schedule.starts[previous];
        let span = dates[next].saturating_sub(seconds);
        let elapsed = date.saturating_sub(seconds);
        let fraction = std::time::Duration::from_secs(elapsed.unsigned_abs())
            .as_secs_f64()
            / std::time::Duration::from_secs(span.unsigned_abs())
                .as_secs_f64()
                .max(1.0);
        time + (start - time) * fraction32(fraction)
    })
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "interpolation fraction is in zero to one"
)]
fn fraction32(value: f64) -> f32 {
    value as f32
}

pub(super) fn departures(
    events: &[Event],
    schedule: &Schedule,
    days: u32,
    pace: FragmentPace,
) -> Vec<Time> {
    // Slower fragments keep their contributor until the last one lands; the default
    // pace keeps its deadlines, which a compressed idle gap can place mid-flight.
    let slower = pace.flight() > FragmentPace::new(1.0).flight();
    let dates = calendar_dates(events);
    dates
        .iter()
        .zip(&schedule.starts)
        .map(|(&date, &start)| {
            let deadline = mapped_time(
                &dates,
                schedule,
                date.saturating_add(i64::from(days) * 86_400),
            )
            .max(start + tick_seconds());
            if slower {
                deadline.max(pace.pulse_end(start))
            } else {
                deadline
            }
        })
        .collect()
}

pub(super) fn progress(now: Time, start: Time, duration: f32) -> f32 {
    ((now - start) / duration).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_lasts_until_the_final_fragment_lands() {
        let schedule = Schedule::new(&crate::timeline::synthetic(), 0.5, 1.0);
        let last = *schedule.starts.last().unwrap();
        let default = FragmentPace::new(1.0);
        assert_eq!(duration(&schedule, &[], default), schedule.duration());
        let slowest = FragmentPace::new(0.25);
        assert_eq!(duration(&schedule, &[], slowest), slowest.pulse_end(last));
    }

    #[test]
    fn slower_fragments_hold_a_compressed_departure_until_they_land() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        events[0].seconds = 0;
        events[1].seconds = 100 * 86_400;
        let schedule = Schedule::new(&events, 0.5, 1.0);
        let start = schedule.starts[0];
        let default = FragmentPace::new(1.0);
        assert!(
            departures(&events, &schedule, 60, default)[0] < default.pulse_end(start)
        );
        let slow = FragmentPace::new(0.5);
        assert_eq!(
            departures(&events, &schedule, 60, slow)[0],
            slow.pulse_end(start)
        );
    }

    #[test]
    fn reversed_author_dates_do_not_trigger_immediate_departures() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        events[0].seconds = 100 * 86_400;
        events[1].seconds = 0;
        let schedule = Schedule::new(&events, 0.5, 1.0);
        assert_eq!(
            departures(&events, &schedule, 60, FragmentPace::new(1.0)),
            [Time::INFINITY; 2]
        );
        let mut later = events[1].clone();
        later.seconds = 200 * 86_400;
        events.push(later);
        let schedule = Schedule::new(&events, 0.5, 1.0);
        let expected =
            schedule.starts[1] + (schedule.starts[2] - schedule.starts[1]) * 0.6;
        assert_eq!(
            departures(&events, &schedule, 60, FragmentPace::new(1.0)),
            [expected, expected, Time::INFINITY]
        );
    }

    #[test]
    fn explicit_endings_before_selected_history_are_already_complete() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        events[0].seconds = 0;
        events[1].seconds = 100 * 86_400;
        let options = super::super::Options {
            start: 1,
            repository_ends: vec![Some(50 * 86_400)],
            ..Default::default()
        };
        let selected = &events[1..];
        let schedule =
            Schedule::new(selected, options.seconds_per_day, options.max_idle_seconds);
        let (_, endings) = plan(&events, &schedule, &options, 1..2);
        assert!(endings[0] + REPOSITORY_EXIT < Time::ZERO);
        let boundary = super::super::Options {
            repository_ends: vec![Some(100 * 86_400)],
            ..options
        };
        assert_eq!(
            ending_time(selected, &schedule, 100 * 86_400, &boundary, true),
            schedule.starts[0]
        );
    }

    #[test]
    fn boundary_repository_exits_finish_on_the_playback_end_tick() {
        use super::super::{
            Options, Simulation, schedule::Clock, tests::short_repository_history,
        };

        let (mut events, labels) = short_repository_history(2);
        events.truncate(2);
        let options = Options::default();
        let mut sim = Simulation::build(
            events.clone(),
            &options,
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &[],
            &labels,
            &mut None,
        )
        .unwrap();
        sim.repository_ends = [1.500_050_1, 5.500_05].map(Time::seconds).to_vec();
        sim.repository_exits = repository_exits(&sim.repository_ends);
        sim.plan_repositories(vec![true; 2], &options).unwrap();
        let history = sim.schedule.clone();
        for padding in [0, 37] {
            sim.schedule =
                history.with_travel(
                    events.len(),
                    |index| if index == 1 { padding } else { 0 },
                );
            sim.clock = Clock::new(history.clone(), sim.schedule.clone());
            let end = sim.end_tick();
            let before = sim.packing_sample(end - 1, None).unwrap().0;
            let after = sim.packing_sample(end, None).unwrap().0;
            assert!(before[1].tile.is_some());
            assert!(after.iter().all(|region| region.tile.is_none()));
        }
    }

    #[test]
    fn only_overlapping_four_second_intervals_stretch() {
        let exits = exits_from_starts(&[Some(0), Some(210), Some(480), Some(480)]);
        let durations: Vec<_> = exits
            .iter()
            .flatten()
            .map(|exit| exit.end - exit.start)
            .collect();
        assert_eq!(durations, [300, 300, 240, 240]);
    }

    #[test]
    fn explicit_endings_cannot_bypass_the_repository_planning_budget() {
        let mut events = crate::timeline::synthetic();
        events.truncate(1);
        events[0].seconds = 0;
        let options = super::super::Options {
            repository_ends: vec![Some(100 * 86_400)],
            seconds_per_day: 86400.0,
            max_idle_seconds: 86400.0,
            ..Default::default()
        };
        assert!(
            super::super::Simulation::check_repository_plan(
                &events,
                &options,
                &[String::new(), String::new()]
            )
            .is_err()
        );
    }

    #[test]
    fn arrivals_fit_the_existing_outer_gutter_clearance() {
        let sim = playback(
            crate::timeline::synthetic(),
            &super::super::Options::default(),
        );
        let clearance =
            super::super::framing::MARGIN - crate::layout::packing::AVATAR_GUTTER * 0.5;
        assert!(sim.state.avatar_radius() <= clearance);
    }

    #[test]
    fn explicit_ending_overrides_archival_and_selection_keeps_prior_identity() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        events[0].seconds = 0;
        events[1] = events[0].clone();
        events[1].seconds = 100 * 86_400;
        crate::contributors::Mappings::default().apply(&mut events);
        let schedule = Schedule::new(&events, 0.5, 1.0);
        let options = super::super::Options {
            repository_ends: vec![Some(50 * 86_400)],
            repository_archived: vec![true],
            ..Default::default()
        };
        let (_, ends) = plan(&events, &schedule, &options, 0..2);
        assert_eq!(
            ends[0],
            schedule.starts[0] + (schedule.starts[1] - schedule.starts[0]) * 0.5
        );
        assert_eq!(arrivals(&events, 0..2), [1, 0]);
        assert_eq!(arrivals(&events, 1..2), [0]);
        let archived = super::super::Options {
            repository_ends: vec![None],
            repository_archived: vec![true],
            ..Default::default()
        };
        let selected = Schedule::new(&events[..1], 0.5, 1.0);
        assert_eq!(
            plan(&events, &selected, &archived, 0..1).1,
            [Time::INFINITY]
        );
        let later_unread = super::super::Options {
            unread: super::super::Unread {
                before: 0,
                after: vec![1],
            },
            ..archived
        };
        assert_eq!(
            plan(&events[..1], &selected, &later_unread, 0..1).1,
            [Time::INFINITY]
        );
        let ended_after_unread = super::super::Options {
            repository_ends: vec![Some(200 * 86_400)],
            ..later_unread
        };
        assert_eq!(
            plan(&events[..1], &selected, &ended_after_unread, 0..1).1,
            [Time::INFINITY]
        );
    }

    #[test]
    #[ignore = "requires CODESTRAND_LIFECYCLE_OUTPUT and local five-repository manifest"]
    fn movie_event_audit() {
        use clap::Parser as _;
        let output = std::env::var("CODESTRAND_LIFECYCLE_OUTPUT").unwrap();
        let root = std::path::Path::new(&output)
            .parent()
            .expect("audit output directory")
            .to_string_lossy();
        let mut arguments = vec![
            "codestrand".to_owned(),
            "preview".to_owned(),
            "--repositories-file".to_owned(),
            format!("{root}/repositories.toml"),
            "--contributors".to_owned(),
            format!("{root}/contributors.toml"),
        ];
        if std::env::var_os("CODESTRAND_LIFECYCLE_WALKING").is_some() {
            arguments.extend(
                ["--contributor-travel", "walk", "--bot-travel", "walk"]
                    .map(str::to_owned),
            );
        }
        let crate::Command::Preview { source, .. } =
            crate::Cli::parse_from(arguments).command.unwrap()
        else {
            panic!("preview arguments")
        };
        let (simulation, _) =
            source.load(&crate::progress::Progress::default()).unwrap();
        let mut seen = std::collections::HashMap::new();
        let mut records = Vec::new();
        for (index, (event, &time)) in simulation
            .events
            .iter()
            .zip(&simulation.schedule.starts)
            .enumerate()
        {
            let contributor = event.contributor.as_ref().unwrap();
            let previous = seen.insert(contributor.key.clone(), index);
            let kind = previous.map_or("arrival", |last| {
                if simulation.departures[last] < time {
                    "revival"
                } else {
                    "commit"
                }
            });
            if kind != "commit" {
                records.push(serde_json::json!({"kind":kind,"time":time.lossy_secs_f32(),"contributor":contributor.name,"repository":event.repository}));
            }
            if let Some(last) = previous
                && simulation.departures[last] < time
            {
                records.push(serde_json::json!({"kind":"departure","time":simulation.departures[last].lossy_secs_f32(),"contributor":contributor.name}));
            }
            for change in &event.changes {
                let kind = match change.kind {
                    crate::timeline::Kind::Added => "file_added",
                    crate::timeline::Kind::Deleted => "file_deleted",
                    _ => continue,
                };
                records.push(serde_json::json!({"kind":kind,"time":time.lossy_secs_f32(),"path":change.path,"repository":event.repository}));
            }
        }
        for &last in seen.values() {
            if simulation.departures[last].is_finite() {
                records.push(serde_json::json!({"kind":"departure","time":simulation.departures[last].lossy_secs_f32(),"contributor":simulation.events[last].contributor.as_ref().unwrap().name}));
            }
        }
        for (repository, &time) in simulation.repository_ends.iter().enumerate() {
            if time.is_finite() {
                records.push(serde_json::json!({"kind":"repository_ended","time":time.lossy_secs_f32(),"repository":repository}));
            }
        }
        records.sort_by(|a, b| {
            a["time"]
                .as_f64()
                .unwrap()
                .total_cmp(&b["time"].as_f64().unwrap())
                .then(a.to_string().cmp(&b.to_string()))
        });
        std::fs::write(
            &output,
            serde_json::to_vec_pretty(
                &serde_json::json!({"duration":simulation.duration().lossy_secs_f32(),"events":records,"travel":simulation.travel.report(simulation.field_space())}),
            )
            .unwrap(),
        )
        .unwrap();
    }

    fn gutter_trace(sim: &mut super::super::Simulation) -> u64 {
        use std::hash::Hasher as _;
        let mut trace = std::collections::hash_map::DefaultHasher::new();
        for frame in 0..=super::super::frames_for(sim.duration(), 30).unwrap() {
            sim.advance_to(frame, 30);
            trace.write_u32(frame);
            for avatar in sim.avatars().iter().filter(|a| a.presence > 0.0) {
                trace.write(avatar.name.as_bytes());
                for coordinate in avatar.position.to_array() {
                    trace.write_u32(coordinate.to_bits());
                }
            }
        }
        trace.finish()
    }

    #[test]
    #[ignore = "requires local lifecycle repositories and CODESTRAND_GUTTER_OUTPUT"]
    fn capped_gutter_measurements() {
        use bevy::math::{Vec2, Vec3};
        use clap::Parser as _;
        let root = std::env::var("CODESTRAND_LIFECYCLE_ROOT").unwrap();
        let crate::Command::Preview { source, .. } = crate::Cli::parse_from([
            "codestrand",
            "preview",
            "--repositories-file",
            &format!("{root}/repositories.toml"),
            "--contributors",
            &format!("{root}/contributors.toml"),
        ])
        .command
        .unwrap() else {
            panic!("preview fixture")
        };
        let (mut sim, _) = source.load(&crate::progress::Progress::default()).unwrap();
        let output = std::env::var("CODESTRAND_GUTTER_OUTPUT").unwrap();
        std::fs::write(
            format!("{output}.trace"),
            gutter_trace(&mut sim).to_string(),
        )
        .unwrap();
        let mut rows = Vec::new();
        for (time, frame) in [(107.77_f32, 3233), (125.0, 3750), (126.98, 3809)] {
            sim.advance_to(frame, 30);
            let segments: Vec<_> = sim
                .state
                .layout
                .repositories
                .iter()
                .filter_map(|r| r.gutter)
                .flat_map(|tile| {
                    let p = tile.ground();
                    [(p[0], p[1]), (p[1], p[2]), (p[2], p[3]), (p[3], p[0])]
                })
                .collect();
            let nearest = |point: Vec2| {
                segments
                    .iter()
                    .map(|&(a, b)| {
                        a + (b - a)
                            * ((point - a).dot(b - a) / (b - a).length_squared())
                                .clamp(0.0, 1.0)
                    })
                    .min_by(|a, b| {
                        point
                            .distance_squared(*a)
                            .total_cmp(&point.distance_squared(*b))
                    })
                    .unwrap()
            };
            let screen = |point| {
                super::super::project(
                    sim.state.focus + Vec3::Y * sim.state.distance,
                    sim.state.focus,
                    &[point],
                )
                .next()
                .flatten()
                .unwrap()
                    * Vec2::new(640.0, 360.0)
            };
            for (index, avatar) in sim
                .avatars()
                .iter()
                .enumerate()
                .filter(|(_, a)| a.presence > 0.0)
            {
                let xz = Vec2::new(avatar.position.x, avatar.position.z);
                let centre = nearest(xz);
                let plane = super::super::ground(xz);
                let projected = screen(avatar.position);
                let screen_distance = segments
                    .iter()
                    .map(|&(a, b)| {
                        let a = screen(super::super::ground(a));
                        let b = screen(super::super::ground(b));
                        let nearest = a
                            + (b - a)
                                * ((projected - a).dot(b - a)
                                    / (b - a).length_squared())
                                .clamp(0.0, 1.0);
                        projected.distance(nearest)
                    })
                    .fold(f32::INFINITY, f32::min);
                rows.push(serde_json::json!({"time":time,"frame":frame,"name":avatar.name,
                    "mode":if sim.travel.walking(index,sim.state.tick){"walking"}else if sim.travel.travelling(index,sim.state.tick){"fading"}else{"parked"},
                    "world_xz":xz.to_array(),"world_distance":xz.distance(centre),
                    "screen_centreline_distance":screen_distance,
                    "screen_offset":projected.distance(screen(super::super::ground(centre))),
                    "height_parallax":projected.distance(screen(plane))}));
                assert!(
                    xz.distance(centre) < 0.0001,
                    "{} at {time}: {xz:?} vs {centre:?}",
                    avatar.name
                );
            }
        }
        std::fs::write(output, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
    }

    #[test]
    fn calendar_deadlines_follow_padded_playback_and_do_not_invent_future_history() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        events[0].seconds = 0;
        events[1].seconds = 100 * 86_400;
        let schedule = Schedule::with_padding(&events, 0.5, 1.0, |_| 60);
        let deadlines = departures(&events, &schedule, 50, FragmentPace::new(1.0));
        assert_eq!(
            deadlines[0],
            schedule.starts[0] + (schedule.starts[1] - schedule.starts[0]) * 0.5
        );
        assert_eq!(deadlines[1], Time::INFINITY);
        assert_eq!(progress(deadlines[0], deadlines[0], EXIT), 0.0);
        assert_eq!(progress(deadlines[0] + EXIT, deadlines[0], EXIT), 1.0);
    }

    fn playback(
        events: Vec<Event>,
        options: &super::super::Options,
    ) -> super::super::Simulation {
        super::super::Simulation::new(
            events,
            options,
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        )
    }

    fn at(sim: &mut super::super::Simulation, time: f32) {
        let frames = super::super::frames_for(Time::seconds(time), 60).unwrap();
        sim.advance_to(frames, 60);
    }

    #[test]
    fn avatar_scale_option_multiplies_the_drawn_disc() {
        let disc = |avatar_size| {
            let options = super::super::Options {
                avatar_size,
                ..Default::default()
            };
            playback(crate::timeline::synthetic(), &options).avatar_disc_scale()
        };
        assert_eq!(disc(2.0).to_bits(), (disc(1.0) * 2.0).to_bits());
        assert_eq!(disc(0.5).to_bits(), (disc(1.0) * 0.5).to_bits());
    }

    #[test]
    fn contributor_arrival_departure_and_return_have_fixed_timing() {
        let mut events = crate::timeline::synthetic();
        events.truncate(3);
        events[0].seconds = 0;
        events[1].seconds = 200 * 86_400;
        events[2] = events[0].clone();
        events[2].seconds = 400 * 86_400;
        let options = super::super::Options {
            seconds_per_day: 0.1,
            max_idle_seconds: 30.0,
            contributor_inactivity_days: 180,
            ..Default::default()
        };
        let mut sim = playback(events, &options);
        at(&mut sim, 0.5);
        assert_eq!(
            sim.state.avatars[0].lifecycle_scale,
            super::super::ARRIVAL_SCALE
        );
        at(&mut sim, 1.7);
        assert_eq!(sim.state.avatars[0].lifecycle_scale, 1.0);
        let deadline = sim.departures[0];
        at(&mut sim, (deadline + 0.4).lossy_secs_f32());
        assert!(sim.state.avatars[0].grey > 0.999);
        assert!(sim.state.avatars[0].presence > 0.0);
        at(&mut sim, (deadline + EXIT + 0.1).lossy_secs_f32());
        assert_eq!(sim.state.avatars[0].presence, 0.0);
        at(&mut sim, 41.0);
        assert!(sim.state.avatars[0].presence > 0.9);
        assert_eq!(sim.state.avatars[0].grey, 0.0);
        assert_eq!(sim.state.avatars[0].lifecycle_scale, 1.0);
        at(&mut sim, 1.7);
        assert_eq!(sim.state.avatars[0].lifecycle_scale, 1.0);
    }

    #[test]
    fn whole_file_creation_descends_and_deletion_rises_without_replacing_identity() {
        let mut event = crate::timeline::synthetic().remove(0);
        event.seconds = 0;
        event.changes.truncate(1);
        event.changes[0].kind = crate::timeline::Kind::Added;
        let mut deletion = event.clone();
        deletion.seconds = 20 * 86_400;
        deletion.changes[0].kind = crate::timeline::Kind::Deleted;
        let options = super::super::Options {
            max_idle_seconds: 30.0,
            ..Default::default()
        };
        let mut sim = playback(vec![event, deletion], &options);
        at(&mut sim, 0.75);
        let (id, file) = sim.layout().files().next().unwrap();
        assert!(sim.file_effect(id, file).0.y > 0.0);
        at(&mut sim, 1.5);
        assert_eq!(sim.file_effect(id, sim.layout().file(id).unwrap()).0.y, 0.0);
        at(&mut sim, 11.0);
        let (position, grey, alpha) =
            sim.file_effect(id, sim.layout().file(id).unwrap());
        assert_eq!(grey, 1.0);
        assert!(position.y > 0.0 && alpha > 0.0 && alpha < 1.0);
        at(&mut sim, 11.8);
        assert!(sim.layout().file(id).is_none());
    }

    #[test]
    fn archived_single_repository_waits_for_activity_and_extends_the_tail() {
        for scale in [1.0, 0.5] {
            let mut events = crate::timeline::synthetic();
            events.truncate(1);
            let options = super::super::Options {
                repository_ends: vec![None],
                repository_archived: vec![true],
                fragment_speed: scale,
                ..Default::default()
            };
            let mut sim = playback(events, &options);
            let end = sim.repository_ends[0];
            assert_eq!(
                end,
                sim.schedule.starts[0]
                    + super::super::PULSE_SECONDS
                    + 1.0 / (super::super::FRAGMENT_SPEED * scale)
            );
            at(&mut sim, (end - 0.1).lossy_secs_f32());
            assert_eq!(sim.repository_effect(0).1, 1.0);
            at(&mut sim, (end + REPOSITORY_EXIT).lossy_secs_f32());
            assert_eq!(sim.repository_effect(0).1, 0.0);
            assert!(sim.duration() >= end + REPOSITORY_EXIT + REFLOW);
        }
    }
}
