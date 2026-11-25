use super::{
    replay::{Fault, Replay, replay},
    *,
};
use proptest::{
    prelude::*,
    test_runner::{Config, RngSeed, TestError, TestRunner},
};

const CI_TICK_LIMIT: u32 = 3_600;

fn seam_history(extra_files: u8, fault: Fault) -> Result<Replay, TestCaseError> {
    let events = vec![
        write(0, 0, ["first.rs".into()]),
        write(1, 1, ["src/second.rs".into()]),
        write(
            0,
            2,
            (0..=extra_files).map(|file| format!("more/{file}.rs")),
        ),
    ];
    replay(
        events,
        &["First".into(), "Second".into()],
        &Options {
            seconds_per_day: 0.75,
            max_idle_seconds: 0.75,
            travel_window_seconds: Some(0.5),
            ..Options::default()
        },
        CI_TICK_LIMIT,
        fault,
    )
}

#[test]
fn a_live_arrival_survives_the_planning_window_boundary() {
    let report = seam_history(0, Fault::None).unwrap();
    assert!(report.windows > 0);
    assert!(report.arrivals_at_seam > 0);
}

#[test]
fn a_foreign_tile_ray_at_the_window_boundary_fails_and_shrinks() {
    let mut runner = TestRunner::new(Config {
        cases: 1,
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(234),
        ..Config::default()
    });
    let first = std::cell::Cell::new(None);
    let failure = runner.run(&(0_u8..16), |extra| {
        if first.get().is_none() {
            first.set(Some(extra));
        }
        seam_history(extra, Fault::ForeignRayAtSeam).map(|_| ())
    });
    let first = first.get().unwrap();
    assert!(first > 0);
    match failure {
        Err(TestError::Fail(reason, 0)) => {
            assert!(
                reason.to_string().contains("crossed repository"),
                "{reason}"
            );
            println!(
                "Shrank {first} extra files to 0: repo 0 adds first.rs, repo 1 adds src/second.rs, repo 0 adds more/0.rs. {reason}"
            );
        }
        other => panic!("expected the three-commit seam history, got {other:?}"),
    }
}

#[derive(Clone, Copy, Debug)]
enum Skew {
    Balanced,
    Dominant,
    ChangingRank,
}

#[derive(Debug)]
struct History {
    repositories: u8,
    files_per_repo: u16,
    skew: Skew,
    archive: u8,
    shared_contributor: bool,
    writes: Vec<u8>,
}

impl History {
    fn replay(&self, tick_limit: u32) -> Result<Replay, TestCaseError> {
        let repositories = usize::from(self.repositories);
        let labels: Vec<_> = (0..repositories).map(|id| format!("Repo {id}")).collect();
        let active = self.repositories - self.archive.min(self.repositories - 2);
        let mut events = Vec::new();
        for repo in 0..self.repositories {
            let count = match (self.skew, repo) {
                (Skew::Dominant | Skew::ChangingRank, 0) => self.files_per_repo * 3,
                _ => self.files_per_repo,
            };
            append(
                &mut events,
                repo,
                false,
                std::iter::once(change("anchor.rs", Kind::Added))
                    .chain((0..count).map(|file| {
                        change(
                            &format!("wide/{}/deep/tree/{file}.rs", file % 4),
                            Kind::Added,
                        )
                    }))
                    .collect(),
            );
        }

        append(
            &mut events,
            1,
            self.shared_contributor,
            vec![change(
                "moved.rs",
                Kind::Moved {
                    from: "wide/0/deep/tree/0.rs".into(),
                },
            )],
        );
        let burst = match self.skew {
            Skew::ChangingRank => self.files_per_repo * 3,
            _ => 1,
        };
        append(
            &mut events,
            1,
            self.shared_contributor,
            (0..burst)
                .map(|file| change(&format!("burst/{file}.rs"), Kind::Added))
                .collect(),
        );
        for &repo in &self.writes {
            append(
                &mut events,
                repo % active,
                self.shared_contributor,
                vec![change("anchor.rs", Kind::Modified)],
            );
        }
        append(
            &mut events,
            1,
            false,
            vec![change("moved.rs", Kind::Deleted)],
        );
        replay(
            events,
            &labels,
            &Options {
                repository_ends: vec![None; repositories],
                repository_archived: (0..self.repositories)
                    .map(|repo| repo >= active)
                    .collect(),
                seconds_per_day: 0.25,
                max_idle_seconds: 0.25,
                travel_window_seconds: Some(3.5),
                ..Options::default()
            },
            tick_limit,
            Fault::None,
        )
    }
}

fn change(path: &str, kind: Kind) -> FileChange {
    FileChange {
        path: path.into(),
        kind,
        insertions: 1,
        ..FileChange::default()
    }
}

fn append(events: &mut Vec<Event>, repo: u8, shared: bool, changes: Vec<FileChange>) {
    let contributor = if shared { 0 } else { u16::from(repo) + 1 };
    let mut event = namespaced_event(
        usize::from(repo),
        i64::try_from(events.len()).unwrap(),
        Identity::new(
            &format!("Author {contributor}"),
            &format!("a{contributor}@example.test"),
        ),
        changes,
    );
    if shared && repo == 1 {
        event
            .co_authors
            .push(Identity::new("Author 2", "a2@example.test"));
    }
    events.push(event);
}

fn histories() -> impl Strategy<Value = History> {
    (
        2..=5_u8,
        1..=8_u16,
        prop_oneof![
            Just(Skew::Balanced),
            Just(Skew::Dominant),
            Just(Skew::ChangingRank)
        ],
        0..=2_u8,
        any::<bool>(),
        prop::collection::vec(0..5_u8, 6..=12),
    )
        .prop_map(
            |(
                repositories,
                files_per_repo,
                skew,
                archive,
                shared_contributor,
                writes,
            )| {
                History {
                    repositories,
                    files_per_repo,
                    skew,
                    archive,
                    shared_contributor,
                    writes,
                }
            },
        )
}

#[test]
fn short_histories_preserve_playback_invariants() {
    let defaults = Config::default();
    let seed = match defaults.rng_seed {
        RngSeed::Random => RngSeed::Fixed(234),
        seed @ RngSeed::Fixed(_) => seed,
    };
    TestRunner::new(Config {
        cases: 8,
        max_shrink_iters: 64,
        source_file: Some(file!()),
        rng_seed: seed,
        ..defaults
    })
    .run(&histories(), |history| {
        history.replay(CI_TICK_LIMIT).map(|_| ())
    })
    .unwrap_or_else(|error| panic!("playback seed {seed:?}: {error}"));
}

#[test]
fn archive_and_rank_change_under_sustained_writes() {
    let history = History {
        repositories: 3,
        files_per_repo: 4,
        skew: Skew::ChangingRank,
        archive: 1,
        shared_contributor: true,
        writes: (0..2).cycle().take(32).collect(),
    };
    let report = history.replay(CI_TICK_LIMIT).unwrap();
    println!("{report:?}");
    assert!(report.windows > 0);
    assert!(report.cross_repo_services > 0);
    assert!(report.arrivals_at_seam > 0);
    assert!(report.max_carried_step > 0.0);
    assert!(report.final_gap <= 1e-5, "settled gutter gap: {report:?}");
}

#[test]
fn overlapping_archive_reflows_leave_later_services_a_live_tile() {
    let events = [0, 3, 6, 7]
        .into_iter()
        .enumerate()
        .map(|(repo, day)| {
            namespaced_event(
                repo,
                day,
                Identity::new(
                    &format!("Author {repo}"),
                    &format!("a{repo}@example.test"),
                ),
                vec![change("root.rs", Kind::Added)],
            )
        })
        .collect();
    let labels = ["First", "Second", "Third", "Survivor"].map(str::to_owned);
    let report = replay(
        events,
        &labels,
        &Options {
            seconds_per_day: 1.0,
            max_idle_seconds: 3.0,
            travel_window_seconds: Some(3.5),
            repository_ends: vec![None; 4],
            repository_archived: vec![true, true, true, false],
            ..Options::default()
        },
        CI_TICK_LIMIT,
        Fault::None,
    )
    .unwrap();
    assert!(report.windows > 0);
    assert!(
        report.last_archive_tick > 0
            && report.last_service_tile_tick >= report.last_archive_tick,
        "the surviving repository wrote during a close: {report:?}"
    );
}

#[test]
#[ignore = "opt-in CPU stress replay"]
fn beeware_scale_playback() {
    stress(&History {
        repositories: 80,
        files_per_repo: 4,
        skew: Skew::Dominant,
        archive: 37,
        shared_contributor: false,
        writes: (0..43).collect(),
    });
}

#[test]
#[ignore = "opt-in CPU stress replay"]
fn high_live_count_playback() {
    stress(&History {
        repositories: 3,
        files_per_repo: 256,
        skew: Skew::ChangingRank,
        archive: 1,
        shared_contributor: true,
        writes: (0..2).cycle().take(32).collect(),
    });
}

fn stress(history: &History) {
    let started = std::time::Instant::now();
    let report = history
        .replay(7_200)
        .unwrap_or_else(|error| panic!("{history:?}: {error}"));
    println!(
        "{} repositories, {} base files each: {report:?}, wall {:?}",
        history.repositories,
        history.files_per_repo,
        started.elapsed()
    );
}

#[test]
fn a_coauthor_circle_casts_across_windows() {
    let mut first = write(0, 0, ["src/nested/main.rs".into()]);
    first
        .co_authors
        .push(Identity::new("Pair", "pair@example.test"));
    let report = replay(
        vec![first, write(1, 1, ["other.rs".into()])],
        &["First".into(), "Second".into()],
        &Options {
            seconds_per_day: 3.0,
            travel_window_seconds: Some(0.5),
            ..Options::default()
        },
        CI_TICK_LIMIT,
        Fault::None,
    )
    .unwrap();
    assert!(report.windows > 0);
    assert!(report.ring_seams > 0);
}
