use super::*;
use crate::timeline::{FileChange, Identity};

mod directory_edges;
mod generated;
mod replay;

fn write(
    repository: usize,
    day: i64,
    paths: impl IntoIterator<Item = String>,
) -> Event {
    let author = Identity::new(
        ["Ada", "Grace"][repository],
        ["ada@example.test", "grace@example.test"][repository],
    );
    namespaced_event(
        repository,
        day,
        author,
        paths
            .into_iter()
            .map(|path| FileChange {
                path,
                insertions: 1,
                ..Default::default()
            })
            .collect(),
    )
}

fn namespaced_event(
    repository: usize,
    day: i64,
    author: Identity,
    changes: Vec<FileChange>,
) -> Event {
    let mut event = Event::at(day * 86_400, changes);
    event.author = author;
    event.commit = format!("day-{day}-repo-{repository}");
    crate::repositories::namespace(std::slice::from_mut(&mut event), repository);
    event
}

fn busy_history() -> Vec<Event> {
    let mut events = vec![
        write(0, 0, (0..8).map(|i| format!("small/{i}.rs"))),
        write(1, 16, ["large/root.rs".into()]),
    ];
    events.extend((17_i64..80).map(|day| {
        let repository = usize::from(day % 2 == 0);
        let path = ["small/0.rs", "large/root.rs"][repository];
        let mut event = write(repository, day, [path.into()]);
        event.changes[0].kind = crate::timeline::Kind::Modified;
        event
    }));
    events
}

fn playback(events: Vec<Event>, archived: [bool; 2]) -> Simulation {
    Simulation::with_repositories(
        events,
        &Options {
            seconds_per_day: 0.25,
            max_idle_seconds: 0.25,
            repository_archived: archived.into(),
            repository_ends: vec![None; 2],
            gutter: 0.0,
            ..Default::default()
        },
        &Theme::neon(),
        &Mappings::default(),
        &["Small".into(), "Large".into()],
    )
}

#[test]
fn a_large_tile_catches_up_during_continuous_writes() {
    let mut events = busy_history();
    events.insert(2, write(1, 17, (0..24).map(|i| format!("large/{i}.rs"))));
    let sim = playback(events, [false; 2]);
    let first_full_write = sim.schedule.commit_ticks().nth(2).unwrap();
    let bound = u64::from(2 * TICK_RATE);
    for tick in first_full_write + bound..=sim.end_tick() {
        let (regions, _, _) = sim.packing_sample(tick, None).unwrap();
        let small = regions[0].gutter.unwrap().area();
        let large = regions[1].gutter.unwrap().area();
        let actual = large / (small + large);
        assert!(
            (actual - 27.0 / 37.0).abs() <= 0.05,
            "large tile at tick {tick}: {actual:.3}"
        );
    }
}

#[test]
fn archive_closes_its_gap_during_continuous_writes() {
    let events = busy_history()
        .into_iter()
        .filter(|event| event.repository == 0 || event.seconds <= 24 * 86_400)
        .collect();
    let sim = playback(events, [false, true]);
    let mut previous = 0.0;
    let mut closing = 0;
    for tick in 0..=sim.end_tick() {
        let (regions, _, _) = sim.packing_sample(tick, None).unwrap();
        let area = regions[0].gutter.unwrap().area();
        let (tile_tick, _) = sim.clock.rows(tick);
        let fade_end = sim.repository_ends[1] + lifecycle::REPOSITORY_EXIT;
        if tile_tick >= fade_end.ticks(true) && regions[1].gutter.is_some() {
            assert_eq!(
                sim.schedule.applied(tick),
                sim.schedule.applied(tick.saturating_sub(1)),
                "write during archive close at tick {tick}",
            );
            if tile_tick > fade_end.ticks(true) {
                assert!(area > previous, "archive gap stalled at tick {tick}");
                closing += 1;
            }
        }
        previous = area;
    }
    assert!(closing > 0 && closing <= 4 * TICK_RATE);
}

#[test]
fn alternating_leads_keep_both_same_repository_docks() {
    let ada = Identity::new("Ada", "ada@example.test");
    let grace = Identity::new("Grace", "grace@example.test");
    let mut events = Vec::new();
    for day in 0..8 {
        let mut event = write(0, day, ["src/lib.rs".into()]);
        let (lead, coauthor) = if day % 2 == 0 {
            (&ada, &grace)
        } else {
            (&grace, &ada)
        };
        event.author = lead.clone();
        event.co_authors.push(coauthor.clone());
        events.push(event);
    }
    let mut sim = playback(events, [false; 2]);
    let mut initial = None;
    for tick in 0..=u32::try_from(sim.end_tick()).unwrap() {
        sim.advance_to(tick, 60);
        if sim.state.next_event == 0 {
            continue;
        }
        let docks = [0, 1].map(|index| {
            let slot = sim.state.avatars[index].slot.as_ref().unwrap();
            assert!(
                sim.travel.sample(index, tick.into()).is_none(),
                "tick {tick}"
            );
            (slot.dock, slot.offset)
        });
        assert_eq!(*initial.get_or_insert(docks), docks, "tick {tick}");
    }
}

#[test]
fn alternating_contributors_keep_clear_docks() {
    let grace = Identity::new("Grace", "grace@example.test");
    let events = (0..8)
        .map(|day| {
            let mut event = write(0, day, ["src/lib.rs".into()]);
            if day % 2 == 1 {
                event.author = grace.clone();
            }
            event
        })
        .collect();
    let mut sim = playback(events, [false; 2]);
    let first = u32::try_from(sim.schedule.commit_ticks().next().unwrap()).unwrap();
    sim.advance_to(first + 1, 60);
    let incumbent = sim.state.avatars[0].slot.as_ref().unwrap().dock;
    sim.travel.docks.insert((1, 1), incumbent);
    sim.restart();

    let mut newcomer = None;
    let mut audit = occlusion::Audit::default();
    for tick in 0..=u32::try_from(sim.end_tick()).unwrap() {
        sim.advance_to(tick, 60);
        if sim.state.avatars.len() < 2 {
            continue;
        }
        let [ada, grace] = [&sim.state.avatars[0], &sim.state.avatars[1]];
        audit.observe(&sim);
        assert_eq!(audit.parked_avatar_observations, 0, "tick {tick}");
        assert_eq!(ada.slot.as_ref().unwrap().dock, incumbent, "tick {tick}");
        let dock = grace.slot.as_ref().unwrap().dock;
        assert_eq!(*newcomer.get_or_insert(dock), dock, "tick {tick}");
        assert!((0..2).all(|index| sim.travel.sample(index, tick.into()).is_none()));
    }
    assert!(newcomer.is_some());
}

#[test]
fn consecutive_writes_keep_the_same_dock() {
    let mut sim = playback(busy_history(), [false; 2]);
    let services: Vec<_> = sim.schedule.commit_ticks().collect();
    for (index, &tick) in services[..2].iter().enumerate() {
        sim.advance_to(u32::try_from(tick).unwrap(), 60);
        let dock = sim.state.avatars[index].slot.as_ref().unwrap().dock;
        let mut alternative = slots::Dock::new(2, 0.5);
        if alternative == dock {
            alternative = slots::Dock::new(0, 0.5);
        }
        for (commit, event) in sim.events.iter().enumerate().skip(2) {
            if event.repository == index {
                sim.travel.docks.insert((commit, index), alternative);
            }
        }
    }
    sim.restart();
    let mut docks = [None; 2];
    for tick in 0..=u32::try_from(sim.end_tick()).unwrap() {
        sim.advance_to(tick, 60);
        for (index, avatar) in sim.state.avatars.iter().enumerate() {
            if u64::from(tick) == services[avatar.commit] {
                let dock = avatar.slot.as_ref().unwrap().dock;
                assert!(
                    docks[index].is_none_or(|previous| dock == previous),
                    "dock changed at commit {}: {:?} to {dock:?}",
                    avatar.commit,
                    docks[index],
                );
                docks[index] = Some(dock);
            }
            assert!(sim.travel.sample(index, tick.into()).is_none());
        }
    }
    assert!(docks.iter().all(Option::is_some));
}
#[test]
fn visible_avatar_steps_stay_within_travel_speed() {
    let actions = Identity::new("Actions", "actions@example.test");
    let mut events = vec![
        write(0, 0, ["src/lib.rs".into()]),
        write(0, 1, ["src/lib.rs".into()]),
        write(0, 2, ["src/lib.rs".into()]),
    ];
    events[0].co_authors.push(actions.clone());
    events[2].author = actions;
    let mut sim = playback(events, [false; 2]);
    let mut previous = Vec::new();
    let max_step =
        sim.state.avatar_max_speed * framing::WORLD_UNITS_PER_REFERENCE_PIXEL / 60.0;
    let relocation =
        u32::try_from(sim.schedule.commit_ticks().nth(1).unwrap()).unwrap();
    for frame in 0..=u32::try_from(sim.end_tick()).unwrap() {
        if frame == relocation {
            let slot = sim.state.avatars[0].slot.as_mut().unwrap();
            slot.dock = slots::Dock::new(2, 0.5);
        }
        sim.advance_to(frame, 60);
        for ((before, presence), avatar) in previous.iter().zip(&sim.state.avatars) {
            if *presence > 0.001 && avatar.presence > 0.001 {
                let step = avatar.position.distance(*before);
                assert!(
                    step <= max_step + 0.001,
                    "frame {frame}: {} stepped {step} (limit {max_step})",
                    avatar.name,
                );
            }
        }
        previous = sim
            .state
            .avatars
            .iter()
            .map(|a| (a.position, a.presence))
            .collect();
    }
}

#[test]
fn visible_cross_repository_handoffs_stay_out_of_foreign_tiles() {
    let mut sim = handoff_playback();
    let mut previous: Vec<(usize, bool)> = Vec::new();
    let mut handoffs = 0;
    for frame in 0..=u32::try_from(sim.end_tick()).unwrap() {
        sim.advance_to(frame, 60);
        let state = &sim.state;
        for (index, avatar) in state.avatars.iter().enumerate() {
            if avatar.presence <= 0.001 {
                continue;
            }
            if let Some(&(repository, visible)) = previous.get(index)
                && repository != avatar.repository
            {
                handoffs += 1;
                assert!(
                    !visible || sim.travel.walking(index, u64::from(frame - 1)),
                    "frame {frame}: {} changed repository without walking or fading",
                    avatar.name,
                );
            }
            let point = slots::field_point(avatar.position);
            for (repository, layout) in state.layout.repositories.iter().enumerate() {
                if repository != avatar.repository
                    && let Some(tile) = layout.tile
                {
                    assert!(
                        !travel::in_guard(tile, state.avatar_radius(), point),
                        "frame {frame}: {} entered foreign tile {repository}",
                        avatar.name,
                    );
                }
            }
        }
        previous = state
            .avatars
            .iter()
            .map(|avatar| (avatar.repository, avatar.presence > 0.001))
            .collect();
    }
    assert_eq!(handoffs, 1);
}

fn handoff_playback() -> Simulation {
    let mut events = vec![
        write(0, 0, ["src/lib.rs".into()]),
        write(1, 1, ["main.rs".into()]),
        write(1, 10, ["main.rs".into()]),
    ];
    events[2].author = events[0].author.clone();
    Simulation::with_repositories(
        events,
        &Options {
            seconds_per_day: 1.0,
            max_idle_seconds: 15.0,
            contributor_inactivity_days: 30,
            ..Default::default()
        },
        &Theme::neon(),
        &Mappings::default(),
        &["Source".into(), "Destination".into()],
    )
}

#[test]
fn visible_travellers_do_not_hold_away_from_their_seat() {
    let mut sim = handoff_playback();
    let mut origins = [Vec3::ZERO; 2];
    let mut stopped = [(Vec3::ZERO, 0); 2];
    let mut walked = false;
    let stationary_radius = 0.1 * framing::WORLD_UNITS_PER_REFERENCE_PIXEL;
    let settling_frames = 6;
    for frame in 0..=u32::try_from(sim.end_tick()).unwrap() {
        sim.advance_to(frame, 60);
        for (index, avatar) in sim.state.avatars.iter().enumerate() {
            let point = avatar.position;
            let travelling = sim.travel.travelling(index, u64::from(frame));
            walked |=
                avatar.presence > 0.001 && sim.travel.walking(index, u64::from(frame));
            if !travelling {
                origins[index] = point;
            }
            let (anchor, since) = &mut stopped[index];
            if !travelling
                || avatar.presence <= 0.001
                || point.distance(origins[index]) <= stationary_radius
                || point.distance(*anchor) > stationary_radius
            {
                *anchor = point;
                *since = frame;
                continue;
            }
            assert!(
                frame - *since <= settling_frames,
                "frame {frame}: {} stopped away from the origin seat at {point:?}",
                avatar.name,
            );
        }
    }
    assert!(walked);
}
#[test]
fn crowded_tree_moves_and_reparents_a_file_while_tiles_resize() {
    use crate::timeline::Kind;

    let paths = (0..8).flat_map(|branch| {
        [
            format!("branch-{branch}/main.rs"),
            format!("branch-{branch}/parse/format/check.rs"),
            format!("branch-{branch}/tests/check.rs"),
        ]
    });
    let rename = namespaced_event(
        0,
        15,
        Identity::new("Ada", "ada@example.test"),
        vec![FileChange {
            path: "branch-1/parse/moved/check.rs".into(),
            kind: Kind::Moved {
                from: "branch-0/parse/format/check.rs".into(),
            },
            ..Default::default()
        }],
    );
    let events = vec![
        write(0, 0, paths),
        write(1, 1, ["neighbor/main.rs".into()]),
        write(
            1,
            14,
            (0..55).map(|file| format!("neighbor/growth/{file}.rs")),
        ),
        rename,
        write(
            0,
            16,
            (0..120).map(|file| format!("branch-2/parse/format/{file}.rs")),
        ),
        write(0, 29, ["after.rs".into()]),
    ];
    let labels = ["Crowded".into(), "Neighbor".into()];
    let report = replay::replay(
        events,
        &labels,
        &Options {
            seconds_per_day: 1.0,
            max_idle_seconds: 15.0,
            ..Options::default()
        },
        3_600,
        replay::Fault::None,
    )
    .unwrap();
    assert!(report.peak_live_directories > 30, "{report:?}");
    assert_eq!(report.reparented_files, 1, "{report:?}");
    assert!(report.directory_intervals > 0, "{report:?}");
    assert!(report.resizing_directory_intervals > 0, "{report:?}");
}
