use super::*;
use crate::timeline::{FileChange, Identity};

fn event(day: i64, changes: Vec<FileChange>) -> Event {
    Event {
        seconds: day * 86_400,
        author: Identity {
            name: b"Ada".to_vec(),
            email: Vec::new(),
        },
        repository: 0,
        contributor: None,
        commit: String::new(),
        changes,
    }
}

fn changes(dir: &str, count: usize, kind: &Kind) -> Vec<FileChange> {
    (0..count)
        .map(|index| FileChange {
            path: format!("{dir}/{index}"),
            kind: kind.clone(),
            insertions: 1,
            removals: 0,
            samples: Vec::new(),
        })
        .collect()
}

fn growth() -> Vec<Event> {
    let mut events: Vec<_> = (0..40)
        .map(|day| event(day, changes("a", 1, &Kind::Modified)))
        .collect();
    events[0].changes = changes("a", 1, &Kind::Added);
    events[0].changes.extend(changes("z", 1, &Kind::Added));
    events[20].changes = changes("a/deep", 80, &Kind::Added);
    events
}

fn simulation(events: Vec<Event>, options: &Options) -> Simulation {
    Simulation::new(events, options, &Theme::neon(), &Mappings::default())
}

fn snapshot(sim: &Simulation) -> String {
    format!(
        "{:?}",
        (
            sim.state.tick,
            sim.state.next_event,
            sim.layout().dirs().collect::<Vec<_>>(),
            sim.layout().files().collect::<Vec<_>>(),
            &sim.state.avatars,
            &sim.state.pulses,
            sim.view(),
        )
    )
}

#[test]
fn themes_preserve_simulation_during_playback_seek_and_restart() {
    let events = crate::timeline::synthetic()
        .into_iter()
        .take(2)
        .collect::<Vec<_>>();
    let mut switched = simulation(events.clone(), &Options::default());
    let mut reference = simulation(events, &Options::default());
    switched.change_rate(2.0);
    reference.change_rate(2.0);
    for frame in [30, 90, 45, 120] {
        switched.advance_to(frame, 30);
        reference.advance_to(frame, 30);
        for name in crate::theme::ThemeName::ALL {
            switched.set_theme(name.build());
            assert_eq!(snapshot(&switched), snapshot(&reference));
            assert_eq!(switched.schedule.starts, reference.schedule.starts);
            assert_eq!(switched.duration(), reference.duration());
            assert_eq!(
                switched.playback_multiplier(),
                reference.playback_multiplier()
            );
        }
    }
    switched.restart();
    reference.restart();
    assert_eq!(snapshot(&switched), snapshot(&reference));
}

#[test]
fn mid_gap_rate_changes_preserve_state_and_schedule() {
    let mut sim = simulation(growth(), &Options::default());
    sim.advance_to(585, 60);
    let before = snapshot(&sim);
    let starts = sim.schedule.starts.clone();
    let duration = sim.duration();
    for factor in [0.125, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 0.5] {
        sim.change_rate(factor);
        assert_eq!(snapshot(&sim), before);
        assert_eq!(sim.schedule.starts, starts);
        assert_eq!(sim.duration(), duration);
    }
}

#[test]
fn every_multiplier_samples_the_canonical_timeline() {
    for multiplier in [0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0] {
        let mut sim = simulation(growth(), &Options::default());
        let mut canonical = simulation(growth(), &Options::default());
        sim.change_rate(multiplier);
        let mut elapsed = Duration::ZERO;
        for _ in 0..frames_for(sim.duration() / multiplier, 8).unwrap() {
            elapsed += Duration::from_millis(125).mul_f32(sim.playback_multiplier());
            let frame = preview_frame(elapsed, 60);
            sim.advance_to(frame, 60);
            canonical.advance_to(frame, 60);
            assert_eq!(snapshot(&sim), snapshot(&canonical));
        }
        sim.advance_to(585, 60);
        canonical.advance_to(585, 60);
        assert_eq!(snapshot(&sim), snapshot(&canonical));
        sim.restart();
        assert_eq!(sim.seconds(), 0.0);
        assert_eq!(sim.playback_multiplier(), multiplier);
    }
}

#[test]
fn multipliers_preserve_fading_directory_space() {
    let mut first = changes("a", 100, &Kind::Added);
    first.extend(changes("b", 1, &Kind::Added));
    let events = vec![
        event(0, first),
        event(1, changes("a", 100, &Kind::Deleted)),
        event(2, changes("b", 100, &Kind::Added)),
        event(3, changes("b", 1, &Kind::Modified)),
    ];
    let options = Options {
        seconds_per_day: 100.0,
        max_idle_seconds: 200.0,
        ..Options::default()
    };
    for (factor, change_tick) in [(1.0, 0), (0.125, 0), (8.0, 6036)] {
        let mut sim = simulation(events.clone(), &options);
        for _ in 0..change_tick {
            sim.tick_layout();
        }
        if change_tick > 0 {
            assert!(sim.layout().files().any(|(_, file)| {
                file.state == crate::layout::Presence::Leaving && file.presence > 0.0
            }));
        }
        sim.change_rate(factor);
        let frames = frames_for(sim.duration(), 60).unwrap();
        let mut fading = false;
        for frame in change_tick..frames {
            sim.tick_layout();
            fading |= sim.layout().files().any(|(_, file)| {
                file.state == crate::layout::Presence::Leaving && file.presence > 0.0
            });
            let dirs: Vec<_> = sim.layout().dirs().collect();
            for (index, (_, a)) in dirs.iter().enumerate() {
                for (_, b) in &dirs[index + 1..] {
                    assert!(
                        a.position.distance(b.position) + 0.0001
                            >= a.radius() + b.radius(),
                        "factor {factor}: overlap {} / {} at frame {frame}",
                        a.path,
                        b.path
                    );
                }
            }
        }
        assert!(fading);
        assert_eq!(sim.commit(), sim.total());
        assert!(
            sim.layout()
                .files()
                .all(|(_, file)| !file.path.starts_with("a/"))
        );
    }
}

fn padded(
    mut events: Vec<Event>,
    options: &Options,
    padding: &[(usize, u32)],
) -> Simulation {
    crate::repositories::namespace(&mut events, 0);
    Simulation::build(
        events,
        options,
        &Theme::neon(),
        &Mappings::default(),
        padding,
        &["Synthetic history".into()],
    )
}

#[test]
fn padding_holds_date_and_commit_while_heat_fragments_and_fades_advance() {
    let events = vec![
        event(0, changes("a", 1, &Kind::Added)),
        event(1, changes("a", 1, &Kind::Modified)),
        event(2, changes("b", 1, &Kind::Added)),
    ];
    let mut sim = padded(events, &Options::default(), &[(1, 360)]);
    sim.advance_to(60, 60);
    let caption = sim.caption();
    let heat = sim.layout().files().next().unwrap().1.heat;
    assert!(!sim.fragments().is_empty());
    sim.advance_to(390, 60);
    assert_eq!(sim.commit(), 1);
    assert_eq!(sim.caption(), caption);
    assert!(sim.layout().files().next().unwrap().1.heat < heat);
    assert!(sim.fragments().is_empty());
    assert_eq!(sim.state.avatars[0].presence, 1.0);
    sim.advance_to(420, 60);
    assert_eq!(sim.commit(), 2);
    assert!(sim.caption().contains("1970-01-02"));
}

#[test]
fn padded_state_replays_identically_at_thirty_and_sixty_fps_and_after_rewind() {
    for options in [
        Options::default(),
        Options {
            start: 2,
            commits: Some(4),
            ..Options::default()
        },
    ] {
        let events = crate::timeline::synthetic();
        let make = || padded(events.clone(), &options, &[(0, 30), (2, 60), (4, 45)]);
        let mut thirty = make();
        let mut sixty = make();
        for frame in [0, 15, 30, 45, 90, 180, 300, 25, 120, 0, 90] {
            thirty.advance_to(frame, 30);
            sixty.advance_to(frame * 2, 60);
            let mut direct = make();
            direct.advance_to(frame, 30);
            assert_eq!(snapshot(&thirty), snapshot(&sixty));
            assert_eq!(snapshot(&thirty), snapshot(&direct));
            assert_eq!(thirty.caption(), direct.caption());
        }
    }
}

#[test]
fn first_selected_padding_aligns_playback_and_full_history_layout() {
    let events: Vec<_> = (0..5)
        .map(|day| event(day, changes("a", 1, &Kind::Added)))
        .collect();
    let options = Options {
        start: 2,
        ..Options::default()
    };
    let mut sim = padded(events, &options, &[(2, 60)]);
    assert_eq!(sim.schedule.starts[0], 1.5);
    sim.advance_to(90, 60);
    assert_eq!(sim.commit(), 3);
    assert_eq!(sim.layout().planned_time(), 2.5);
    sim.advance_to(120, 60);
    assert_eq!(sim.layout().planned_time(), 3.0);
    sim.advance_to(90, 60);
    assert_eq!(sim.layout().planned_time(), 2.5);
}
