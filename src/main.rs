#![forbid(unsafe_code)]
#![deny(
    clippy::all,
    clippy::pedantic,
    clippy::cargo,
    clippy::cognitive_complexity,
    clippy::disallowed_types,
    clippy::disallowed_methods,
    clippy::allow_attributes,
    clippy::allow_attributes_without_reason
)]
#![expect(
    clippy::multiple_crate_versions,
    reason = "Bevy's platform backends require incompatible transitive crate versions"
)]

#[cfg(test)]
mod agent_skill;
mod avatars;
mod cache;
mod camera;
mod camera_path;
mod config;
mod contributors;
mod diff_samples;
mod encoder;
mod export;
mod history;
mod layout;
mod model;
mod progress;
mod repositories;
mod resolution;
mod scene;
mod small_file;
mod theme;
mod timeline;

fn terminal_escape(value: &str) -> String {
    escape_except(value, &[])
}

fn terminal_escape_layout(value: &str) -> String {
    escape_except(value, &['\n', '\t'])
}

fn escape_except(value: &str, kept: &[char]) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if (character.is_control() && !kept.contains(&character))
            || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

#[cfg(test)]
mod terminal_tests {
    #[test]
    fn multiline_diagnostics_keep_layout_and_escape_controls() {
        assert_eq!(
            super::terminal_escape_layout("error:\n\ttip\r\u{1b}[31m\u{202e}"),
            "error:\n\ttip\\r\\u{1b}[31m\\u{202e}"
        );
    }

    #[test]
    fn error_chains_cannot_forge_lines() {
        let error = anyhow::anyhow!("bad path a\nError: forged\tx").context("import");
        assert_eq!(
            super::terminal_escape(&format!("{error:#}")),
            "import: bad path a\\nError: forged\\tx"
        );
    }

    #[test]
    fn escapes_paths_and_dependency_error_chains() {
        let error = anyhow::anyhow!("gix: bad path \u{1b}[31mfile\u{7}")
            .context("import repository");
        let diagnostic = super::terminal_escape(&format!("{error:#}"));
        assert!(!diagnostic.chars().any(char::is_control));
        assert!(diagnostic.contains("gix: bad path"));
        assert!(diagnostic.contains("\\u{1b}"));
    }
}

use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};
use progress::{Progress, Stage};

#[derive(Parser)]
#[command(version, about = "Git history inspection and playback")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[cfg(test)]
impl Cli {
    fn try_fixture_from<I, T>(arguments: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let mut arguments = arguments.into_iter().map(Into::into);
        let mut public = Vec::new();
        let mut start = 0;
        let mut commits = None;
        let mut delay = 0;
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--start" | "--commits" | "--frame-delay-ms") => {
                    let value = arguments.next().expect("fixture option value");
                    let value = value.to_str().unwrap().parse().unwrap();
                    match argument.to_str().unwrap() {
                        "--start" => start = value,
                        "--commits" => commits = Some(value),
                        _ => delay = u16::try_from(value).unwrap(),
                    }
                }
                _ => public.push(argument),
            }
        }
        let mut cli = Self::try_parse_from(public)?;
        if let Some(Command::Export { frame_delay_ms, .. }) = &mut cli.command {
            *frame_delay_ms = delay;
        }
        if let Some(
            Command::Preview { source, .. }
            | Command::Export { source, .. }
            | Command::LayoutQuality { source, .. },
        ) = &mut cli.command
        {
            source.start = start;
            source.commits = commits;
            if source.repository.is_empty() && source.repositories_file.is_none() {
                source.test_input = Some(TestInput::Synthetic);
            }
        }
        Ok(cli)
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum TestInput {
    Synthetic,
}

#[derive(Subcommand)]
enum Command {
    /// Measure tick-sampled layout quality without starting a renderer.
    #[command(hide = true)]
    LayoutQuality {
        #[command(flatten)]
        source: Source,
        /// Write per-tick measurements and a summary as JSON Lines.
        output: PathBuf,
        #[arg(long)]
        treemap: bool,
    },
    /// Inspect local history as JSON; merge changes compare against the first parent.
    Inspect {
        repository: PathBuf,
        #[arg(long, default_value = "HEAD")]
        revision: String,
        #[command(flatten)]
        history: history::Options,
    },
    /// Play history in a loop as planning progresses; [ halves and ] doubles live speed (0.125x–8x).
    #[command(
        after_help = "Camera: overhead by default; pitch clamps to ±90° and the eye stays above the file plane. Right drag orbits. With settings closed, C or a left viewport click captures the mouse to aim; while captured, hold left / right buttons to move up / down. C or Escape releases capture; Escape otherwise closes preview. WASD pans, scroll zooms, Tab cycles visible contributors, Space restores overhead. Orbit, captured aim and zoom retain follow; pan or button elevation releases it. F10 toggles display settings; F11 toggles borderless fullscreen."
    )]
    Preview {
        #[arg(long, value_enum, default_value = "neon")]
        theme: theme::ThemeName,
        #[command(flatten)]
        source: Source,
    },
    /// Write PNG frames or video as planning progresses, with a manifest in a new directory.
    Export {
        #[arg(long, value_enum, default_value = "neon")]
        theme: theme::ThemeName,
        directory: PathBuf,
        /// Native output size; each side 64–8192, at most 33,554,432 pixels.
        #[arg(long, default_value_t = resolution::Resolution::default())]
        resolution: resolution::Resolution,
        /// TOML camera keyframes, with semantic targets resolved at every frame.
        #[arg(long)]
        camera_path: Option<PathBuf>,
        #[command(flatten)]
        source: Source,
        /// Frames to write; defaults to the whole playback.
        #[arg(long, value_parser = clap::value_parser!(u32).range(2..))]
        frames: Option<u32>,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u16).range(1..=240))]
        fps: u16,
        #[arg(skip)]
        frame_delay_ms: u16,
        #[command(flatten)]
        encoding: encoder::Options,
    },
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum WindowOverride {
    Whole,
    Seconds(f32),
}

#[derive(Clone, Copy, clap::ValueEnum, serde::Serialize)]
enum ContributorTravel {
    Auto,
    Walk,
}

impl ContributorTravel {
    fn cap(self) -> model::TravelCap {
        match self {
            Self::Auto => model::TravelCap::Seconds(0.4),
            Self::Walk => model::TravelCap::None,
        }
    }
}

#[derive(clap::Args, serde::Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent clap switch"
)]
struct Source {
    #[command(flatten)]
    #[serde(flatten)]
    history: history::Options,
    /// Assume departure after this many calendar days; default 60 marks two quiet months.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=36500))]
    contributor_inactivity_days: u32,
    /// Repository caption colour treatment.
    #[arg(long, value_enum, default_value = "bar")]
    caption_style: model::caption::Style,
    /// Local repository to play; repeat for a shared timeline; default discovers the current repository.
    #[arg(long)]
    repository: Vec<PathBuf>,
    /// TOML repository list; relative paths resolve beside the TOML file.
    #[arg(long)]
    repositories_file: Option<PathBuf>,
    /// TOML options keyed by flag name and an optional [[repositories]] list; paths resolve beside it; flags override it.
    #[arg(long, value_name = "FILE")]
    #[serde(skip)]
    config: Option<PathBuf>,
    #[arg(long, hide = true, value_enum, conflicts_with_all = ["repository", "repositories_file"])]
    #[serde(skip)]
    test_input: Option<TestInput>,
    /// Repository tile boundary treatment.
    #[arg(long, value_enum, default_value = "glass")]
    repository_boundary: repositories::Boundary,
    /// Permanent repository labels at file level or between files and avatars.
    #[arg(long, value_enum, default_value = "raised")]
    repository_label_plane: repositories::LabelPlane,
    /// Revision resolved independently in every repository.
    #[arg(long, default_value = "HEAD")]
    revision: String,
    /// Skip history and avatar cache reads and writes.
    #[arg(long)]
    no_cache: bool,
    /// Hide changed source lines in preview and exported frames.
    #[arg(long)]
    no_diff_samples: bool,
    /// Collapse small directory subtrees into file-sized folder icons (on|off).
    #[arg(long, action = clap::ArgAction::Set, default_value = "on", value_parser = directory_collapsing)]
    collapse_directories: bool,
    /// Contributor TOML file; defaults to the Codestrand user configuration directory.
    #[arg(long)]
    contributors: Option<PathBuf>,
    /// Fetch contributor avatars before playback; sends account requests and email hashes.
    #[arg(long)]
    fetch_avatars: bool,
    /// Select opted-in GitLab cache entries offline; allow email lookup with --fetch-avatars.
    #[arg(long)]
    gitlab_email_avatars: bool,
    #[arg(skip)]
    #[serde(skip)]
    start: usize,
    /// Begin at the first commit on or after this UTC date, inclusive; settle earlier commits.
    #[arg(long, value_name = "YYYY-MM-DD", value_parser = start_date)]
    start_date: Option<i64>,
    /// End at the last commit on or before this UTC date, inclusive of the whole day.
    #[arg(long, value_name = "YYYY-MM-DD", value_parser = start_date)]
    end_date: Option<i64>,
    #[arg(skip)]
    #[serde(skip)]
    commits: Option<usize>,
    /// Canonical timeline seconds per calendar day; live speed keys leave the timeline fixed.
    #[arg(long, default_value_t = 0.5, value_parser = positive_seconds)]
    seconds_per_day: f32,
    /// Maximum canonical timeline seconds between commits, compressing idle gaps.
    #[arg(long, default_value_t = 1.0, value_parser = idle_seconds)]
    max_idle_seconds: f32,
    /// Contributors: auto walks short trips and fades longer ones; walk always walks.
    #[arg(long, value_enum, default_value = "auto")]
    contributor_travel: ContributorTravel,
    #[cfg(test)]
    #[arg(skip)]
    #[serde(skip)]
    window_override: Option<WindowOverride>,
    #[arg(skip = model::Trace::from_env())]
    #[serde(skip)]
    trace: model::Trace,
    /// Bot travel policy: fade across or walk the shortest gutter route.
    #[arg(long, value_enum, default_value = "fade")]
    bot_travel: model::BotTravel,
    /// Maximum avatar travel speed in pixels/second at the 1280x720 reference size.
    #[arg(long, default_value_t = 480.0, value_parser = avatar_speed)]
    avatar_max_speed: f32,
    /// Avatar disc diameter relative to the original size, in both playback modes (0.5–2).
    #[arg(long, default_value_t = 2.0, value_parser = avatar_size)]
    avatar_scale: f32,
    /// Visible spacing between repository tiles in reference pixels at full detail (0–20).
    #[arg(long, default_value_t = 4.0, value_parser = gutter_pixels)]
    gutter: f32,
    /// File heat half-life in canonical playback seconds.
    #[arg(long, default_value_t = 3.0, value_parser = positive_seconds)]
    heat_half_life: f32,
}

impl Source {
    fn events(
        &self,
        cache_root: Option<&std::path::Path>,
        repositories: &[repositories::Repository],
        progress: &Progress,
    ) -> anyhow::Result<Vec<timeline::Event>> {
        let mut histories = Vec::new();
        for (index, repository) in repositories.iter().enumerate() {
            progress.report(Stage::History, index as u64, repositories.len() as u64);
            let started = std::time::Instant::now();
            let mut events = cache::load_with_options(
                &repository.path,
                &self.revision,
                cache_root.filter(|_| !self.no_cache),
                self.history,
            )?;
            eprintln!(
                "repository={} import_seconds={:.6} events={}",
                terminal_escape(&repository.name()),
                started.elapsed().as_secs_f64(),
                events.len()
            );
            if repositories.len() > 1 {
                repositories::namespace(&mut events, index);
            }
            if self.no_diff_samples {
                for change in events.iter_mut().flat_map(|event| &mut event.changes) {
                    change.samples.clear();
                }
            }
            histories.push(events);
        }
        Ok(if repositories.is_empty() {
            timeline::synthetic()
        } else if repositories.len() == 1 {
            histories.pop().expect("one repository")
        } else {
            repositories::merge(histories)
        })
    }

    fn resolve_avatars(
        &self,
        events: &mut [timeline::Event],
        repositories: &[repositories::Repository],
        range: std::ops::Range<usize>,
    ) {
        let fetch = self.fetch_avatars && !repositories.is_empty();
        let mut resolver = avatars::Resolver::new(
            fetch.then(avatars::Http::new),
            cache::root().filter(|_| !self.no_cache),
            fetch,
            avatars::now(),
        );
        resolver.gitlab_email = self.gitlab_email_avatars;
        let start = range.start.min(events.len());
        let end = range.end.min(events.len());
        if repositories.is_empty() {
            resolver.resolve(&mut events[start..end], None);
        }
        for (index, repository) in repositories.iter().enumerate() {
            let remote = avatars::repository(&repository.path);
            if repositories.len() == 1 {
                resolver.resolve(&mut events[start..end], remote.as_deref());
                return;
            }
            let indices: Vec<_> = (start..end)
                .filter(|&at| events[at].repository == index)
                .collect();
            let mut selected: Vec<_> =
                indices.iter().map(|&at| events[at].clone()).collect();
            resolver.resolve(&mut selected, remote.as_deref());
            for (at, event) in indices.into_iter().zip(selected) {
                events[at] = event;
            }
        }
    }

    /// The `--repository` paths, or the repository holding the current directory when
    /// no repository, manifest or test input is named.
    fn repositories(&self) -> anyhow::Result<Vec<PathBuf>> {
        Ok(
            if self.repository.is_empty()
                && self.repositories_file.is_none()
                && self.test_input.is_none()
            {
                vec![repositories::discover(&std::env::current_dir()?)?]
            } else {
                self.repository.clone()
            },
        )
    }

    fn load(&self, progress: &Progress) -> anyhow::Result<(model::Simulation, String)> {
        #[cfg(not(test))]
        let available_memory = model::available_memory(&self.trace)?;
        #[cfg(test)]
        let available_memory = model::Options::default().available_memory;
        self.load_with_memory(progress, available_memory)
    }

    fn load_with_memory(
        &self,
        progress: &Progress,
        available_memory: u64,
    ) -> anyhow::Result<(model::Simulation, String)> {
        let paths = self.repositories()?;
        let file = self.repositories_file.as_deref();
        let repositories = if self.config.is_some()
            && self.config == self.repositories_file
        {
            repositories::inputs_with(&paths, file, repositories::Keys::AlsoOptions)?
        } else {
            repositories::inputs(&paths, file)?
        };
        let mappings = contributors::load(self.contributors.as_deref())?;
        let mut events =
            self.events(cache::root().as_deref(), &repositories, progress)?;
        let range = self.resolve_range(&events)?;
        mappings.apply(&mut events);
        self.resolve_avatars(&mut events, &repositories, range.clone());
        let names: Vec<_> = repositories
            .iter()
            .map(repositories::Repository::name)
            .collect();
        let title = if names.is_empty() {
            "Synthetic history".to_owned()
        } else {
            names.join(" | ")
        };
        let mut options = model::Options {
            available_memory,
            contributor_inactivity_days: self.contributor_inactivity_days,
            repository_ends: repositories
                .iter()
                .map(|repository| {
                    repository
                        .ended
                        .as_deref()
                        .map(repositories::parse_ended)
                        .transpose()
                })
                .collect::<anyhow::Result<_>>()?,
            repository_archived: repositories.iter().map(|r| r.archived).collect(),
            heat_half_life: self.heat_half_life,
            seconds_per_day: self.seconds_per_day,
            max_idle_seconds: self.max_idle_seconds,
            start: range.start,
            commits: Some(range.len()),
            travel_cap: self.contributor_travel.cap(),
            travel_window_seconds: None,
            #[cfg(test)]
            eager_windows: false,
            #[cfg(test)]
            window_hook: None,
            replay_everything: false,
            region_threads: 0,
            bot_travel: self.bot_travel,
            avatar_max_speed: self.avatar_max_speed,
            avatar_size: self.avatar_scale,
            gutter: self.gutter,
            progress: progress.clone(),
            trace: self.trace.clone(),
        };
        let history_duration = options.choose_window(&events);
        #[cfg(test)]
        if let Some(window) = self.window_override {
            options.travel_window_seconds = match window {
                WindowOverride::Whole => None,
                WindowOverride::Seconds(seconds) => Some(seconds),
            };
        }
        if cfg!(test) {
            eprintln!(
                "planning_history_seconds={} travel_window_seconds={}",
                history_duration.lossy_secs_f32(),
                options
                    .travel_window_seconds
                    .map_or_else(|| "single".to_owned(), |seconds| seconds.to_string())
            );
        }
        let mut simulation = Self::playback(events, &options, &mappings, &names)?;
        simulation.repository_boundary = self.repository_boundary;
        simulation.collapse_directories = self.collapse_directories;
        simulation.caption_style = self.caption_style;
        simulation.repository_label_plane = self.repository_label_plane;
        Ok((simulation, title))
    }

    fn resolve_range(
        &self,
        events: &[timeline::Event],
    ) -> anyhow::Result<std::ops::Range<usize>> {
        let start_label = self
            .start_date
            .map_or_else(|| "beginning of history".to_owned(), model::date);
        let end_label = self
            .end_date
            .map_or_else(|| "end of history".to_owned(), model::date);
        anyhow::ensure!(
            self.start_date
                .zip(self.end_date)
                .is_none_or(|(start, end)| end >= start),
            "inclusive date range {start_label} through {end_label}: end precedes start"
        );
        if (self.start_date.is_some() || self.end_date.is_some())
            && !events.iter().any(|event| {
                self.start_date.is_none_or(|start| event.seconds >= start)
                    && self.end_date.is_none_or(|end| event.seconds < end + 86_400)
            })
        {
            let last = events.last().map_or_else(
                || "history is empty".to_owned(),
                |event| {
                    format!("last commit date is {} UTC", model::date(event.seconds))
                },
            );
            anyhow::bail!(
                "inclusive date range {start_label} through {end_label} contains no commits; {last}"
            );
        }
        let start = self
            .start_date
            .map_or(self.start.min(events.len()), |date| {
                events
                    .iter()
                    .position(|event| event.seconds >= date)
                    .unwrap_or(events.len())
            });
        let end = self.end_date.map_or_else(
            || {
                self.commits.map_or(events.len(), |count| {
                    start.saturating_add(count).min(events.len())
                })
            },
            |end| {
                events
                    .iter()
                    .rposition(|event| event.seconds < end + 86_400)
                    .map_or(0, |last| last + 1)
            },
        );
        anyhow::ensure!(
            start <= end,
            "inclusive date range {start_label} through {end_label} contains no commits"
        );
        Ok(start..end)
    }

    fn playback(
        mut events: Vec<timeline::Event>,
        options: &model::Options,
        mappings: &contributors::Mappings,
        names: &[String],
    ) -> anyhow::Result<model::Simulation> {
        let names = if names.is_empty() {
            vec!["Synthetic history".to_owned()]
        } else {
            names.to_vec()
        };
        if names.len() == 1 {
            repositories::namespace(&mut events, 0);
        }
        model::Simulation::try_with_repositories(
            events,
            options,
            &theme::Theme::neon(),
            mappings,
            &names,
        )
    }

    /// Reports initial planning through `report`, then later planning to stderr,
    /// at most once per `interval`.
    fn load_reporting(
        &self,
        interval: std::time::Duration,
        report: impl FnMut(progress::Update),
    ) -> anyhow::Result<(model::Simulation, String)> {
        Self::report_loading(interval, report, |progress| self.load(progress))
    }

    fn report_loading(
        interval: std::time::Duration,
        mut report: impl FnMut(progress::Update),
        load: impl FnOnce(&Progress) -> anyhow::Result<(model::Simulation, String)> + Send,
    ) -> anyhow::Result<(model::Simulation, String)> {
        let (progress, updates) = Progress::channel(interval);
        std::thread::scope(|scope| {
            let worker_progress = progress.clone();
            let worker = scope.spawn(move || load(&worker_progress));
            while !worker.is_finished() {
                match updates.recv_timeout(std::time::Duration::from_millis(50)) {
                    Ok(update) => report(update),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => (),
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            progress.continue_on_stderr();
            for update in updates.try_iter() {
                report(update);
            }
            worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        })
    }
}

fn start_date(value: &str) -> Result<i64, String> {
    let invalid = || "expected a valid UTC date in YYYY-MM-DD format".to_owned();
    if value.len() != 10
        || !value.bytes().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
    {
        return Err(invalid());
    }
    gix::date::parse(value, None)
        .map(|date| date.seconds)
        .map_err(|_| invalid())
}

#[cfg(test)]
mod single_source_tests {
    #[test]
    fn cli_single_multi_and_synthetic_share_camera_and_caption_columns() {
        for (names, expected) in [
            (vec![], "Synthetic history"),
            (vec!["One".to_owned()], "One"),
            (vec!["One".to_owned(), "Two".to_owned()], "Two"),
        ] {
            let events = if names.len() > 1 {
                crate::repositories::merge(
                    (0..names.len())
                        .map(|index| {
                            let mut events = crate::timeline::synthetic();
                            crate::repositories::namespace(&mut events, index);
                            events
                        })
                        .collect(),
                )
            } else {
                crate::timeline::synthetic()
            };
            let mut sim = super::Source::playback(
                events,
                &crate::model::Options::default(),
                &crate::contributors::Mappings::default(),
                &names,
            )
            .unwrap();
            sim.advance_to(60, 60);
            assert_eq!(
                sim.view(),
                (bevy::math::Vec3::ZERO, crate::model::framing::DISTANCE)
            );
            assert_eq!(crate::model::caption::Columns::new(&sim).layout().len(), 6);
            let repository = sim.caption_repository().unwrap().1;
            assert_eq!(repository, expected);
        }
    }

    #[test]
    fn small_single_repository_keeps_frame_sized_discs_and_clear_text() {
        let mut events = crate::timeline::synthetic();
        events.truncate(5);
        for (i, event) in events.iter_mut().enumerate() {
            event.seconds = [0, 3, 10, 13, 16][i] * 86_400;
            event.author.name = if i % 2 == 0 {
                b"Alice".to_vec()
            } else {
                b"Bob".to_vec()
            };
            event.author.email = event.author.name.clone();
            event.contributor = None;
            event.changes.truncate(1);
            event.changes[0].path = format!("src/file-{i}.rs");
            event.changes[0].kind = crate::timeline::Kind::Added;
        }
        let options = crate::model::Options {
            seconds_per_day: 2.0,
            max_idle_seconds: 6.0,
            ..Default::default()
        };
        let mut sim = super::Source::playback(
            events,
            &options,
            &crate::contributors::Mappings::default(),
            &["Small".to_owned()],
        )
        .unwrap();
        let mut audit = crate::model::occlusion::Audit::default();
        for frame in 0..=crate::model::frames_for(sim.duration(), 60).unwrap() {
            sim.advance_to(frame, 60);
            audit.observe(&sim);
        }
        assert!(audit.maximum_avatar_radius <= 17.0, "{audit:?}");
        assert_eq!(audit.avatar_text_observations, 0, "{audit:?}");
        assert_eq!(audit.parked_avatar_observations, 0, "{audit:?}");
    }
}

fn directory_collapsing(value: &str) -> Result<bool, &'static str> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err("expected on or off"),
    }
}

fn positive_seconds(value: &str) -> Result<f32, String> {
    match value.parse::<f32>() {
        Ok(seconds) if (0.01..=86_400.0).contains(&seconds) => Ok(seconds),
        _ => Err("expected seconds between 0.01 and 86400".to_owned()),
    }
}

fn idle_seconds(value: &str) -> Result<f32, String> {
    let seconds = positive_seconds(value)?;
    if seconds < model::tick_seconds() {
        return Err(
            "idle cap must be at least one simulation tick (1/60 second)".to_owned(),
        );
    }
    Ok(seconds)
}

fn avatar_speed(value: &str) -> Result<f32, String> {
    let speed = positive_seconds(value)?;
    if speed >= 1.0 {
        Ok(speed)
    } else {
        Err("avatar speed must be at least one reference pixel/second".to_owned())
    }
}

fn avatar_size(value: &str) -> Result<f32, String> {
    between(value, 0.5, 2.0, "avatar scale")
}

fn between(value: &str, low: f32, high: f32, name: &str) -> Result<f32, String> {
    value
        .parse::<f32>()
        .ok()
        .filter(|parsed| (low..=high).contains(parsed))
        .ok_or_else(|| format!("{name} must be between {low} and {high}"))
}

fn main() -> std::process::ExitCode {
    termination(run())
}

fn termination(result: anyhow::Result<()>) -> std::process::ExitCode {
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            if let Some(usage) = error.downcast_ref::<clap::Error>() {
                eprint!("{}", terminal_escape_layout(&usage.to_string()));
                std::process::ExitCode::from(2)
            } else {
                eprintln!("Error: {}", terminal_escape(&format!("{error:#}")));
                std::process::ExitCode::FAILURE
            }
        }
    }
}

/// Opens the preview window while a worker thread loads and plans `source`.
fn preview(mut source: Source, theme: theme::ThemeName) -> anyhow::Result<()> {
    // Outside a repository, fail before a window opens.
    source.repository = source.repositories()?;
    let (progress, updates) = Progress::channel(std::time::Duration::from_millis(50));
    let theme = theme.build();
    let worker = std::thread::spawn(move || {
        let (mut simulation, title) = source.load(&progress)?;
        simulation.set_theme(theme);
        Ok((simulation, title))
    });
    scene::preview(scene::Loading::new(updates, worker))
}

fn run() -> anyhow::Result<()> {
    let cli =
        match Cli::try_parse_from(config::arguments(std::env::args_os().collect())?) {
            Ok(cli) => cli,
            Err(error) if error.use_stderr() => return Err(error.into()),
            Err(error) => {
                print!("{error}");
                return Ok(());
            }
        };
    dispatch(cli)
}

fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Some(Command::LayoutQuality {
            source,
            output,
            treemap,
        }) => {
            let started = std::time::Instant::now();
            let (mut simulation, _) = source.load(&Progress::default())?;
            eprintln!("load_plan_seconds={:.6}", started.elapsed().as_secs_f64());
            eprintln!(
                "region_plan_seconds={:.6}",
                simulation.layout().planning_seconds()
            );
            let started = std::time::Instant::now();
            let mut writer = std::io::BufWriter::new(std::fs::File::create(output)?);
            serde_json::to_writer(
                &mut writer,
                &serde_json::json!({
                    "schema": 1, "source": source,
                    "history_events": simulation.total(),
                    "duration_seconds": simulation.duration().lossy_secs_f32(), "tick_rate": model::TICK_RATE,
                }),
            )?;
            std::io::Write::write_all(&mut writer, b"\n")?;
            if treemap {
                model::treemap::run(&mut simulation, &mut writer)?;
            } else {
                model::quality::run(&mut simulation, &mut writer)?;
            }
            std::io::Write::flush(&mut writer)?;
            eprintln!(
                "replay_report_seconds={:.6}",
                started.elapsed().as_secs_f64()
            );
        }
        Some(Command::Inspect {
            repository,
            revision,
            history: options,
        }) => {
            let (repo, tip) = history::resolve(&repository, &revision)?;
            let history = history::import_with_options(&repo, tip, options)?;
            serde_json::to_writer_pretty(std::io::stdout().lock(), &history)?;
        }
        Some(Command::Preview { source, theme }) => preview(source, theme)?,
        Some(Command::Export {
            theme,
            directory,
            source,
            frames,
            fps,
            frame_delay_ms,
            encoding,
            resolution,
            camera_path,
        }) => {
            let (mut simulation, title) =
                source.load_reporting(std::time::Duration::from_secs(1), |update| {
                    eprintln!("{update}");
                })?;
            simulation.set_theme(theme.build());

            let camera_path = camera_path
                .as_deref()
                .map(|path| camera_path::CameraPath::load(path, &simulation))
                .transpose()?;
            if let Some(path) = &camera_path {
                path.pose(&simulation, 0, fps)?;
            }
            let encoder = encoding.start(fps, resolution)?;
            export::prepare(&directory)?;
            export::render(
                &directory,
                scene::headless(simulation, title, fps, resolution),
                frames,
                fps,
                frame_delay_ms,
                encoder,
                camera_path.as_ref(),
            )?;
        }
        None => Cli::command().print_help()?,
    }
    Ok(())
}

fn gutter_pixels(value: &str) -> Result<f32, String> {
    let pixels: f32 = value.parse().map_err(|_| "gutter must be a number")?;
    if pixels.is_finite() && (0.0..=20.0).contains(&pixels) {
        Ok(pixels)
    } else {
        Err("gutter must be between 0 and 20 pixels".into())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn date_cli_rejects_positions_and_invalid_dates() {
        for command in [
            vec!["preview"],
            vec!["export", "frames"],
            vec!["layout-quality", "quality.jsonl"],
        ] {
            let args = [vec!["codestrand"], command].concat();
            for value in ["2026/05/01", "2026-5-01", "2026-02-30", "today"] {
                let error = Cli::try_fixture_from(
                    [args.clone(), vec!["--start-date", value]].concat(),
                )
                .err()
                .expect("invalid date");
                assert!(error.to_string().contains("YYYY-MM-DD"));
            }
            let error = Cli::try_parse_from(
                [
                    args.clone(),
                    vec!["--start-date", "2026-05-01", "--start", "0"],
                ]
                .concat(),
            )
            .err()
            .expect("removed positional option");
            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
            assert!(error.to_string().contains("--start"));
            assert!(
                Cli::try_fixture_from(
                    [args, vec!["--start-date", "2026-05-01"]].concat(),
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn start_date_resolves_boundaries_in_single_and_merged_history_order() {
        let cli = Cli::try_fixture_from([
            "codestrand",
            "preview",
            "--start-date",
            "2026-05-02",
        ])
        .unwrap();
        let Some(Command::Preview { mut source, .. }) = cli.command else {
            panic!("preview")
        };
        let midnight = start_date("2026-05-02").unwrap();
        let event = |seconds| {
            let mut event = timeline::synthetic().remove(0);
            event.seconds = seconds;
            event
        };
        for histories in [
            vec![vec![
                event(midnight - 1),
                event(midnight),
                event(midnight + 86_400),
            ]],
            vec![
                vec![event(midnight - 1), event(midnight + 86_400)],
                vec![event(midnight)],
            ],
        ] {
            let events = repositories::merge(histories);
            for (date, expected) in
                [("2026-05-01", 0), ("2026-05-02", 1), ("2026-05-03", 2)]
            {
                source.start_date = Some(start_date(date).unwrap());
                assert_eq!(source.resolve_range(&events).unwrap().start, expected);
            }
            source.start_date = Some(start_date("2026-05-04").unwrap());
            assert!(
                source
                    .resolve_range(&events)
                    .unwrap_err()
                    .to_string()
                    .contains("inclusive date range 2026-05-04 through end of history contains no commits; last commit date is 2026-05-03 UTC")
            );
        }
        source.start_date = Some(midnight);
        assert_eq!(
            source
                .resolve_range(&[event(midnight - 1), event(midnight + 1)])
                .unwrap()
                .start,
            1
        );
        let reversed = vec![event(midnight + 1), event(midnight - 1)];
        assert_eq!(source.resolve_range(&reversed).unwrap().start, 0);
        let merged = repositories::merge(vec![reversed, vec![event(midnight - 2)]]);
        assert_eq!(source.resolve_range(&merged).unwrap().start, 1);
    }

    #[test]
    fn diff_reveal_uses_the_same_fixed_distance_in_preview_and_export() {
        for command in [vec!["preview"], vec!["export", "frames"]] {
            let args = [vec!["codestrand"], command].concat();
            assert!(
                Cli::try_parse_from(
                    [args.clone(), vec!["--diff-text-distance", "4"]].concat()
                )
                .is_err()
            );
            let cli = Cli::try_fixture_from(args).unwrap();
            let (Command::Preview { source, .. } | Command::Export { source, .. }) =
                cli.command.unwrap()
            else {
                panic!("playback command")
            };
            let (simulation, _) = source.load(&Progress::default()).unwrap();
            assert_eq!(simulation.diff_text_distance, 1.0);
        }
    }

    #[test]
    fn pacing_flags_reach_the_loaded_schedule() {
        let cli = Cli::try_fixture_from([
            "codestrand",
            "preview",
            "--seconds-per-day",
            "2",
            "--max-idle-seconds",
            "5",
        ])
        .unwrap();
        let Some(Command::Preview { source, .. }) = cli.command else {
            panic!("preview")
        };
        let (simulation, _) =
            source.load(&crate::progress::Progress::default()).unwrap();
        let starts = |rate, idle| {
            model::schedule::Schedule::new(&timeline::synthetic(), rate, idle).starts
        };
        assert_eq!(simulation.scheduled_starts(), starts(2.0, 5.0));
        assert_ne!(starts(2.0, 5.0), starts(1.0, 5.0));
        assert_ne!(starts(2.0, 5.0), starts(2.0, 1.0));
    }

    #[test]
    fn export_planning_reports_every_stage_and_plans_the_same_playback() {
        let places = |simulation: &model::Simulation| -> Vec<_> {
            simulation
                .avatars()
                .iter()
                .map(|avatar| (avatar.position, avatar.presence))
                .collect()
        };
        let (finished, completion) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            for window in [None, Some(2.0)] {
                let cli =
                    Cli::try_fixture_from(["codestrand", "export", "unused"]).unwrap();
                let Some(Command::Export { mut source, .. }) = cli.command else {
                    panic!("export")
                };
                source.window_override = window.map(WindowOverride::Seconds);
                let mut lines = Vec::new();
                let (mut reported, title) = source
                    .load_reporting(std::time::Duration::ZERO, |update| {
                        lines.push(update.to_string());
                    })
                    .unwrap();
                for stage in [
                    "planning directories 0%",
                    "replaying layout 0%",
                    "planning travel 0%",
                    "planning travel 100%",
                ] {
                    let line = format!("Planning: {stage}");
                    assert!(lines.contains(&line), "missing {line:?}");
                }
                if window.is_none() {
                    assert_eq!(lines.last().unwrap(), "Planning: placing titles 100%");
                } else {
                    assert!(!reported.planning_complete());
                    assert!(
                        lines
                            .iter()
                            .any(|line| line.starts_with("Planning: placing titles "))
                    );
                }

                let (mut quiet, quiet_title) =
                    source.load(&crate::progress::Progress::default()).unwrap();
                assert_eq!(title, quiet_title);
                for frame in [0, 90, 300, 600] {
                    reported.prepare_frame(frame, 60, true).unwrap();
                    quiet.prepare_frame(frame, 60, true).unwrap();
                    reported.advance_to(frame, 60);
                    quiet.advance_to(frame, 60);
                    assert_eq!(reported.commit(), quiet.commit());
                    assert_eq!(reported.beams().len(), quiet.beams().len());
                    assert_eq!(places(&reported), places(&quiet));
                }
            }
            finished.send(()).unwrap();
        });
        completion
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect(
                "export progress must return while later windows are still planned",
            );
        worker.join().unwrap();
    }

    #[test]
    fn gutter_accepts_zero_and_rejects_nonfinite_or_out_of_range_values() {
        for value in ["0", "4", "20"] {
            assert!(super::gutter_pixels(value).is_ok());
        }
        for value in ["-1", "21", "NaN", "inf", "bad"] {
            assert!(super::gutter_pixels(value).is_err());
        }
    }

    use super::*;

    #[test]
    fn video_cli_requires_video_for_encoder_options() {
        for option in ["--ffmpeg=somewhere", "--ffmpeg-arg=-crf"] {
            assert!(
                Cli::try_fixture_from(["codestrand", "export", "frames", option])
                    .is_err()
            );
            assert!(
                Cli::try_fixture_from([
                    "codestrand",
                    "export",
                    "frames",
                    "--video=out.mp4",
                    option,
                ])
                .is_ok()
            );
        }
        assert!(
            Cli::try_fixture_from([
                "codestrand",
                "export",
                "frames",
                "--video=out.mp4",
                "--ffmpeg-arg=-c:v",
                "--ffmpeg-arg=ffv1",
            ])
            .is_ok()
        );
    }

    #[test]
    fn heat_half_life_is_shared_by_preview_and_export_and_rejects_invalid_values() {
        for command in [vec!["preview"], vec!["export", "frames"]] {
            for value in ["0", "-1", "NaN", "inf", "86401"] {
                let mut args = vec!["codestrand"];
                args.extend(command.clone());
                args.extend(["--heat-half-life", value]);
                assert!(Cli::try_fixture_from(args).is_err());
            }
            let mut args = vec!["codestrand"];
            args.extend(command);
            args.extend(["--heat-half-life", "2"]);
            let cli = Cli::try_fixture_from(args).unwrap();
            let (Command::Preview { source, .. } | Command::Export { source, .. }) =
                cli.command.unwrap()
            else {
                panic!("playback command")
            };
            assert_eq!(source.heat_half_life, 2.0);
        }
    }

    #[test]
    fn gitlab_email_permission_is_separate_from_fetching() {
        for (flags, fetch, email) in [
            (vec![], false, false),
            (vec!["--fetch-avatars"], true, false),
            (vec!["--gitlab-email-avatars"], false, true),
            (
                vec!["--fetch-avatars", "--gitlab-email-avatars"],
                true,
                true,
            ),
        ] {
            let args = [vec!["codestrand", "preview"], flags].concat();
            let cli = Cli::try_fixture_from(args).unwrap();
            let Some(Command::Preview { source, .. }) = cli.command else {
                panic!("preview command parsed")
            };
            assert_eq!(source.fetch_avatars, fetch);
            assert_eq!(source.gitlab_email_avatars, email);
        }
    }

    #[test]
    fn cli_combines_repeated_repositories_and_a_manifest() {
        let cli = Cli::try_fixture_from([
            "codestrand",
            "preview",
            "--repository",
            "first",
            "--repository",
            "second",
            "--repositories-file",
            "repos.toml",
            "--repository-boundary",
            "glass",
            "--repository-label-plane",
            "raised",
        ])
        .unwrap();
        let Some(Command::Preview { source, .. }) = cli.command else {
            panic!("preview")
        };
        assert_eq!(
            source.repository,
            [PathBuf::from("first"), PathBuf::from("second")]
        );
        assert_eq!(source.repositories_file, Some(PathBuf::from("repos.toml")));
        assert!(matches!(
            source.repository_boundary,
            repositories::Boundary::Glass
        ));
        assert!(matches!(
            source.repository_label_plane,
            repositories::LabelPlane::Raised
        ));
    }

    #[test]
    fn cli_selects_registered_themes_for_preview_and_export() {
        use theme::ThemeName;
        for (name, expected) in [
            ("neon", ThemeName::Neon),
            ("phosphor", ThemeName::Phosphor),
            ("outrun", ThemeName::Outrun),
            ("blueprint", ThemeName::Blueprint),
            ("retro-desktop", ThemeName::RetroDesktop),
            ("atlas", ThemeName::Atlas),
            ("forge", ThemeName::Forge),
        ] {
            let cli = Cli::try_fixture_from(["codestrand", "preview", "--theme", name])
                .unwrap();
            let Some(Command::Preview { theme, .. }) = cli.command else {
                panic!("preview")
            };
            assert_eq!(theme, expected);
            assert_eq!(theme.build().name, theme);
            let cli = Cli::try_fixture_from([
                "codestrand",
                "export",
                "frames",
                "--theme",
                name,
            ])
            .unwrap();
            let Some(Command::Export {
                theme: exported, ..
            }) = cli.command
            else {
                panic!("export")
            };
            assert_eq!(theme, exported);
        }
        for command in [
            vec!["codestrand", "preview", "--theme", "unknown"],
            vec!["codestrand", "export", "frames", "--theme", "unknown"],
        ] {
            assert!(Cli::try_fixture_from(command).is_err());
        }
        let cli = Cli::try_fixture_from(["codestrand", "preview"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Preview {
                theme: theme::ThemeName::Neon,
                ..
            })
        ));
    }

    #[test]
    fn minimum_idle_cap_is_accepted_by_cli() {
        let cap = model::tick_seconds().to_string();
        assert!(
            Cli::try_fixture_from([
                "codestrand",
                "preview",
                "--max-idle-seconds",
                &cap
            ])
            .is_ok()
        );
    }

    #[test]
    fn window_length_is_not_a_cli_option() {
        assert!(
            Cli::try_fixture_from([
                "codestrand",
                "preview",
                "--travel-window-seconds",
                "15"
            ])
            .is_err()
        );
    }

    #[test]
    fn frame_count_uses_the_supported_frame_index_range() {
        for value in ["2", "1000001", "4294967295"] {
            assert!(
                Cli::try_fixture_from([
                    "codestrand",
                    "export",
                    "unused",
                    "--frames",
                    value
                ])
                .is_ok()
            );
        }
        for value in ["0", "1", "4294967296"] {
            assert!(
                Cli::try_fixture_from([
                    "codestrand",
                    "export",
                    "unused",
                    "--frames",
                    value
                ])
                .is_err()
            );
        }
    }

    #[test]
    fn avatar_scale_defaults_to_two_and_accepts_the_original_size() {
        for (flags, expected) in [(vec![], 2.0), (vec!["--avatar-scale", "1"], 1.0)] {
            let cli =
                Cli::try_fixture_from([vec!["codestrand", "preview"], flags].concat())
                    .unwrap();
            let Some(Command::Preview { source, .. }) = cli.command else {
                panic!("preview")
            };
            assert_eq!(source.avatar_scale, expected);
        }
        for value in ["0", "-1", "NaN", "inf", "2.1"] {
            assert!(avatar_size(value).is_err());
        }
    }

    #[test]
    fn cli_accepts_walking_contributors_and_bots_without_numeric_budgets() {
        let cli = Cli::try_fixture_from([
            "codestrand",
            "preview",
            "--contributor-travel",
            "walk",
            "--bot-travel",
            "walk",
        ])
        .unwrap();
        let Some(Command::Preview { source, .. }) = cli.command else {
            panic!("preview")
        };
        assert_eq!(source.contributor_travel.cap(), model::TravelCap::None);
        assert_eq!(source.bot_travel, model::BotTravel::Walk);
        for value in ["0.4", "none", "NaN", "inf", "-1", "3601", "unlimited"] {
            assert!(
                Cli::try_parse_from(["codestrand", "preview", "--travel-cap", value])
                    .is_err()
            );
            assert!(
                Cli::try_parse_from([
                    "codestrand",
                    "preview",
                    "--contributor-travel",
                    value
                ])
                .is_err()
            );
        }
        assert_eq!(
            ContributorTravel::Auto.cap(),
            model::TravelCap::Seconds(0.4)
        );
    }

    #[test]
    fn inclusive_date_ranges_keep_both_days_and_reject_empty_or_reversed_ranges() {
        let day = start_date("2026-05-02").unwrap();
        let events: Vec<_> = [-1, 0, 86_399, 86_400, 172_800]
            .map(|offset| {
                let mut event = timeline::synthetic().remove(0);
                event.seconds = day + offset;
                event
            })
            .into();
        for (start, end, expected) in [
            ("2026-05-02", "2026-05-02", Some(1..3)),
            ("2026-05-02", "2026-05-03", Some(1..4)),
            ("2026-05-03", "2026-05-02", None),
            ("2026-05-05", "2026-05-06", None),
        ] {
            let cli = Cli::try_fixture_from([
                "codestrand",
                "preview",
                "--start-date",
                start,
                "--end-date",
                end,
            ])
            .unwrap();
            let Some(Command::Preview { source, .. }) = cli.command else {
                panic!("preview")
            };
            let actual = source.resolve_range(&events);
            if let Some(expected) = expected {
                assert_eq!(actual.unwrap(), expected);
            } else {
                let error = actual.unwrap_err().to_string();
                assert!(error.contains(start) && error.contains(end), "{error}");
            }
        }
    }

    #[test]
    fn internal_settled_fixture_clamps_positions_after_history() {
        let cli = Cli::try_fixture_from(["codestrand", "preview", "--start", "1000"])
            .unwrap();
        let Some(Command::Preview { source, .. }) = cli.command else {
            panic!("preview")
        };
        let events = timeline::synthetic();
        assert_eq!(
            source.resolve_range(&events).unwrap(),
            events.len()..events.len()
        );
    }

    #[test]
    #[ignore = "requires a graphics adapter and explicit fixture arguments"]
    fn export_cli_fixture() {
        let args: Vec<String> =
            serde_json::from_str(&std::env::var("CODESTRAND_FIXTURE_ARGS").unwrap())
                .unwrap();
        dispatch(Cli::try_fixture_from(args).unwrap()).unwrap();
    }
}
