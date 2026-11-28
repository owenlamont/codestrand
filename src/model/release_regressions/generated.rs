use super::{
    replay::{Fault, Replay, replay},
    *,
};
use proptest::{
    prelude::*,
    test_runner::{Config, RngSeed, TestError, TestRunner},
};
use std::{fmt::Write, ops::RangeInclusive, time::Instant};

const CI_TICK_LIMIT: u32 = 3_600;

#[test]
fn glyph_ratio_dense_tile_growth() {
    let shallow = |file| format!("group-{}/file-{file}.rs", file % 6);
    let deep =
        |file| format!("group-{}/node-{}/leaf/file-{file}.rs", file % 6, file / 32);
    let event = |repo, day, changes| {
        let name = ["sparse", "dense", "control"][repo];
        namespaced_event(
            repo,
            day,
            Identity::new(name, &format!("{name}@example.invalid")),
            changes,
        )
    };
    let events = vec![
        event(
            0,
            0,
            (0..6).map(|i| change(&shallow(i), Kind::Added)).collect(),
        ),
        event(
            1,
            5,
            (0..24).map(|i| change(&deep(i), Kind::Added)).collect(),
        ),
        event(
            2,
            10,
            (0..24).map(|i| change(&shallow(i), Kind::Added)).collect(),
        ),
        event(
            1,
            16,
            (24..216).map(|i| change(&deep(i), Kind::Added)).collect(),
        ),
        event(2, 33, vec![change(&shallow(0), Kind::Modified)]),
        event(1, 42, vec![change(&deep(0), Kind::Modified)]),
    ];
    let options = Options {
        seconds_per_day: 1.0,
        max_idle_seconds: 8.0,
        ..Options::default()
    };
    let report = replay(
        events,
        &["sparse".into(), "dense".into(), "control".into()],
        &options,
        CI_TICK_LIMIT,
        Fault::None,
    )
    .unwrap();
    assert!(report.comparable_glyph_frames > 0);
    println!("dense tile glyphs: {report:?}");
    report.check_glyph_ratio().unwrap();
}

#[test]
fn glyph_ratio_distant_directory_growth() {
    let events = vec![
        write(0, 0, (0..7).map(|dir| format!("early/{dir}/initial.rs"))),
        write(1, 1, (0..7).map(|dir| format!("stable/{dir}/initial.rs"))),
        write(0, 30, (0..350).map(|dir| format!("future/{dir}/new.rs"))),
    ];
    let options = Options {
        seconds_per_day: 0.6,
        max_idle_seconds: 20.0,
        travel_window_seconds: Some(8.0),
        ..Options::default()
    };
    let report = replay(
        events,
        &["Growth".into(), "Stable".into()],
        &options,
        CI_TICK_LIMIT,
        Fault::None,
    )
    .unwrap();
    assert!(report.comparable_glyph_frames > 0);
    println!("distant directory glyphs: {report:?}");
    report.check_glyph_ratio().unwrap();
}

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

#[derive(Clone, Copy, Debug)]
struct TreeShape {
    branches: u8,
    max_depth: u8,
    suffix_offset: u8,
}

impl TreeShape {
    fn path(self, file: u16) -> String {
        const SUFFIXES: &[&str] = &[
            "", ".rs", ".py", ".toml", ".json", ".yaml", ".md", ".sh", ".svg", ".png",
            ".bin", ".tar.gz", ".TS",
        ];
        let file = usize::from(file);
        let offset = usize::from(self.suffix_offset);
        let depth = (file + offset) % (usize::from(self.max_depth) + 1);
        let branch = file % usize::from(self.branches);
        let mut path = String::new();
        for level in 0..depth {
            write!(path, "level-{level}-{branch}/").unwrap();
        }
        let suffix = SUFFIXES[(file + offset) % SUFFIXES.len()];
        write!(path, "file-{file}{suffix}").unwrap();
        path
    }
}

#[derive(Clone, Copy, Debug)]
struct PlaybackVariation {
    contributors_per_repo: u8,
    seconds_per_day: f32,
    max_idle_seconds: f32,
    avatar_max_speed: f32,
    avatar_size: f32,
    gutter: f32,
    fragment_speed: f32,
    bot_travel: BotTravel,
    uncapped_walk: bool,
    shared_bot: bool,
}

impl PlaybackVariation {
    fn apply(self, options: &mut Options) {
        options.seconds_per_day = self.seconds_per_day;
        options.max_idle_seconds = self.max_idle_seconds;
        options.avatar_max_speed = self.avatar_max_speed;
        options.avatar_size = self.avatar_size;
        options.gutter = self.gutter;
        options.fragment_speed = self.fragment_speed;
        options.bot_travel = self.bot_travel;
        if self.uncapped_walk {
            options.travel_cap = TravelCap::None;
        }
    }

    fn vary_contributors(self, events: &mut [Event], shared: bool) {
        for (index, event) in events.iter_mut().enumerate() {
            let sequence = i64::try_from(index).unwrap();
            event.seconds = (sequence + sequence / 7 * 3) * 86_400;
            let count = usize::from(self.contributors_per_repo);
            let local = |seat| contributor(event.repository * 16 + seat % count + 1);
            event.author = if shared && index % 7 == 0 {
                if self.shared_bot {
                    Identity::new("Shared contributor[bot]", "shared-bot@example.test")
                } else {
                    contributor(0)
                }
            } else {
                local(index)
            };
            event.co_authors.clear();
            if index % 3 == 1 {
                event.co_authors.push(local(index + 1));
            }
        }
    }
}

fn contributor(id: usize) -> Identity {
    let suffix = if id % 11 == 10 { "[bot]" } else { "" };
    Identity::new(
        &format!("Contributor {id}{suffix}"),
        &format!("contributor-{id}@example.test"),
    )
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
        self.replay_variant(tick_limit, None)
    }

    fn replay_variant(
        &self,
        tick_limit: u32,
        variation: Option<(TreeShape, PlaybackVariation)>,
    ) -> Result<Replay, TestCaseError> {
        let repositories = usize::from(self.repositories);
        let labels: Vec<_> = (0..repositories).map(|id| format!("Repo {id}")).collect();
        let active = self.repositories - self.archive.min(self.repositories - 2);
        let tree = variation.map(|(tree, _)| tree);
        let mut events = self.events(active, tree);
        let mut options = Options {
            repository_ends: vec![None; repositories],
            repository_archived: (0..self.repositories)
                .map(|repo| repo >= active)
                .collect(),
            seconds_per_day: 0.25,
            max_idle_seconds: 0.25,
            travel_window_seconds: Some(if tree.is_some() { 8.0 } else { 3.5 }),
            ..Options::default()
        };
        if let Some((_, playback)) = variation {
            playback.apply(&mut options);
            playback.vary_contributors(&mut events, self.shared_contributor);
        }
        replay(events, &labels, &options, tick_limit, Fault::None)
    }

    fn events(&self, active: u8, tree: Option<TreeShape>) -> Vec<Event> {
        let source_path = |file| {
            tree.map_or_else(
                || format!("wide/{}/deep/tree/{file}.rs", file % 4),
                |shape| shape.path(file),
            )
        };
        let moved_path = if tree.is_some() {
            "relocated/retained.txt"
        } else {
            "moved.rs"
        };
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
                    .chain(
                        (0..count).map(|file| change(&source_path(file), Kind::Added)),
                    )
                    .collect(),
            );
        }

        append(
            &mut events,
            1,
            self.shared_contributor,
            vec![change(
                moved_path,
                Kind::Moved {
                    from: source_path(0),
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
                .map(|file| {
                    let path = tree
                        .map_or_else(|| format!("{file}.rs"), |shape| shape.path(file));
                    change(&format!("burst/{path}"), Kind::Added)
                })
                .collect(),
        );
        let mut transient = vec![Vec::new(); usize::from(active)];
        for (index, &repo) in self.writes.iter().enumerate() {
            let repo = if tree.is_some() && index % 3 != 2 {
                u8::try_from((index / 12) % usize::from(active.min(3))).unwrap()
            } else {
                repo % active
            };
            let write = tree.map_or_else(
                || change("anchor.rs", Kind::Modified),
                |shape| {
                    self.tree_write(index, shape, &mut transient[usize::from(repo)])
                },
            );
            append(&mut events, repo, self.shared_contributor, vec![write]);
        }
        append(
            &mut events,
            1,
            false,
            vec![change(moved_path, Kind::Deleted)],
        );
        events
    }

    fn tree_write(
        &self,
        index: usize,
        tree: TreeShape,
        transient: &mut Vec<String>,
    ) -> FileChange {
        let file = u16::try_from(index).unwrap();
        match index % 4 {
            0 => {
                let path = format!("growth/{}", tree.path(file));
                transient.push(path.clone());
                return change(&path, Kind::Added);
            }
            2 => {
                if let Some(existing) = transient.last_mut() {
                    let path = format!("relocated/{}", tree.path(file));
                    let from = std::mem::replace(existing, path.clone());
                    return change(&path, Kind::Moved { from });
                }
            }
            3 => {
                if let Some(path) = transient.pop() {
                    return change(&path, Kind::Deleted);
                }
            }
            _ => {}
        }
        if self.files_per_repo > 1 {
            let retained = 1 + file % (self.files_per_repo - 1);
            return change(&tree.path(retained), Kind::Modified);
        }
        change("anchor.rs", Kind::Modified)
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

fn histories(
    repositories: RangeInclusive<u8>,
    files: RangeInclusive<u16>,
    archives: RangeInclusive<u8>,
    writes: RangeInclusive<usize>,
) -> impl Strategy<Value = History> {
    let last_repository = *repositories.end();
    (
        repositories,
        files,
        prop_oneof![
            Just(Skew::Balanced),
            Just(Skew::Dominant),
            Just(Skew::ChangingRank)
        ],
        archives,
        any::<bool>(),
        prop::collection::vec(0..last_repository, writes),
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

fn property_config(default_cases: u32) -> Config {
    let defaults = Config::default();
    let cases = if std::env::var_os("PROPTEST_CASES").is_some() {
        defaults.cases
    } else {
        default_cases
    };
    let seed = match defaults.rng_seed {
        RngSeed::Random => RngSeed::Fixed(234),
        seed @ RngSeed::Fixed(_) => seed,
    };
    Config {
        cases,
        max_shrink_iters: defaults.max_shrink_iters.min(64),
        failure_persistence: None,
        rng_seed: seed,
        ..defaults
    }
}

fn playback_variations() -> impl Strategy<Value = PlaybackVariation> {
    (
        2..=12_u8,
        prop::sample::select(vec![0.2, 0.25, 0.35]),
        prop::sample::select(vec![0.25, 0.5, 0.75]),
        prop::sample::select(vec![360.0, 480.0, 720.0]),
        prop::sample::select(vec![1.0, 2.0, 0.5]),
        prop::sample::select(vec![0.0, 4.0, 12.0, 20.0]),
        prop::sample::select(vec![0.75, 1.0, 1.5]),
        prop::sample::select(vec![BotTravel::Fade, BotTravel::Walk]),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(
                contributors_per_repo,
                seconds_per_day,
                max_idle_seconds,
                avatar_max_speed,
                avatar_size,
                gutter,
                fragment_speed,
                bot_travel,
                uncapped_walk,
                shared_bot,
            )| {
                PlaybackVariation {
                    contributors_per_repo,
                    seconds_per_day,
                    max_idle_seconds,
                    avatar_max_speed,
                    avatar_size,
                    gutter,
                    fragment_speed,
                    bot_travel,
                    uncapped_walk,
                    shared_bot,
                }
            },
        )
}

#[test]
fn short_histories_preserve_playback_invariants() {
    let config = property_config(8);
    let seed = config.rng_seed;
    println!(
        "Checking {} generated histories with seed {seed:?}",
        config.cases
    );
    TestRunner::new(config)
        .run(&histories(2..=5, 1..=8, 0..=2, 6..=12), |history| {
            let report = history.replay(CI_TICK_LIMIT)?;
            println!(
                "Worst nominal glyph width ratio {:.4} at frame {}",
                report.worst_glyph_ratio, report.worst_glyph_tick,
            );
            report.check_glyph_ratio()
        })
        .unwrap_or_else(|error| panic!("playback seed {seed:?}: {error}"));
}

#[test]
#[ignore = "opt-in generated playback with larger layouts"]
fn extended_histories_preserve_playback_invariants() {
    let mut config = property_config(8);
    if std::env::var_os("PROPTEST_MAX_SHRINK_TIME").is_none() {
        config.max_shrink_time = 90_000;
    }
    let seed = config.rng_seed;
    let trees = (1..=16_u8, 0..=6_u8, 0..13_u8).prop_map(
        |(branches, max_depth, suffix_offset)| TreeShape {
            branches,
            max_depth,
            suffix_offset,
        },
    );
    println!(
        "Checking {} extended histories with seed {seed:?}",
        config.cases
    );
    TestRunner::new(config)
        .run(&(histories(8..=40, 1..=40, 0..=8, 30..=160), trees, playback_variations()), |(history, tree, playback)| {
            let started = Instant::now();
            println!(
                "Starting {} repos, {} base files, {:?} skew, {} archives, {} commits, {} branches, depth {}",
                history.repositories, history.files_per_repo, history.skew,
                history.archive, usize::from(history.repositories) + history.writes.len() + 3,
                tree.branches, tree.max_depth,
            );
            println!("Playback settings {playback:?}, shared contributors {}", history.shared_contributor);
            let report = history.replay_variant(12_000, Some((tree, playback)))?;
            println!(
                "Completed with {} peak files, {} peak dirs, {} ticks, {:.2?}",
                report.peak_live_files, report.peak_live_directories, report.ticks, started.elapsed(),
            );
            println!("{} visible contributors at peak, {} assigned to one repository, {} passing contacts, {} ticks of parked overlap",
                report.peak_visible_contributors, report.peak_contributors_per_repository,
                report.passing_contacts, report.longest_parked_contact);
            println!(
                "Worst nominal glyph width ratio {:.4} at frame {}",
                report.worst_glyph_ratio, report.worst_glyph_tick,
            );
            report.check_glyph_ratio()
        })
        .unwrap_or_else(|error| panic!("extended playback seed {seed:?}: {error}"));
}

#[test]
fn parked_avatars_follow_moving_berths_during_archive_reflow() {
    let report = History {
        repositories: 4,
        files_per_repo: 8,
        skew: Skew::Balanced,
        archive: 1,
        shared_contributor: false,
        writes: vec![1, 1, 0, 1, 1, 1],
    }
    .replay(CI_TICK_LIMIT)
    .unwrap();
    assert!(report.max_carried_step > 0.0);
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
fn a_generated_late_birth_and_overlapping_exits_use_flat_cells() {
    TestRunner::new(property_config(8))
        .run(&(1..=4_u8, any::<bool>()), |(files, shared)| {
            let author = |repo: usize| {
                let id = if shared { 0 } else { repo };
                Identity::new(&format!("Author {id}"), &format!("a{id}@example.test"))
            };
            let mut events = Vec::new();
            for (repo, day) in [(0, 0), (1, 1), (2, 6), (3, 9)] {
                let changes = (0..files)
                    .map(|file| change(&format!("dir/{file}.rs"), Kind::Added))
                    .collect();
                events.push(namespaced_event(repo, day, author(repo), changes));
            }
            for day in 10..=30 {
                let repo = usize::try_from(day % 2).unwrap();
                events.push(namespaced_event(
                    repo,
                    day,
                    author(repo),
                    vec![change(&format!("live/{day}.rs"), Kind::Added)],
                ));
            }
            let labels = ["One", "Two", "Third", "Fourth"].map(str::to_owned);
            let report = replay(
                events,
                &labels,
                &Options {
                    start: 2,
                    seconds_per_day: 0.5,
                    max_idle_seconds: 1.5,
                    travel_window_seconds: Some(0.75),
                    repository_ends: vec![
                        None,
                        None,
                        Some(12 * 86_400),
                        Some(16 * 86_400),
                    ],
                    repository_archived: vec![false, false, true, true],
                    ..Options::default()
                },
                CI_TICK_LIMIT,
                Fault::None,
            )?;
            prop_assert_eq!(
                report.flat_lifecycles,
                (2, 2),
                "replay fell back to slicing: {:?}",
                report
            );
            prop_assert!(report.overlapping_exits && report.arrivals_at_seam > 0);
            prop_assert!(
                report.max_tile_step_px <= 8.001 && report.final_gap <= 1e-5,
                "{:?}",
                report
            );
            Ok(())
        })
        .unwrap();
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
