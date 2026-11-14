#![forbid(unsafe_code)]
#![deny(
    clippy::all,
    clippy::pedantic,
    clippy::cargo,
    clippy::cognitive_complexity,
    clippy::allow_attributes,
    clippy::allow_attributes_without_reason
)]
#![expect(
    clippy::multiple_crate_versions,
    reason = "Bevy's platform backends require incompatible transitive crate versions"
)]

mod avatars;
mod cache;
mod camera;
mod camera_path;
mod contributors;
mod diff_samples;
mod encoder;
mod export;
mod history;
mod layout;
mod model;
mod repositories;
mod resolution;
mod scene;
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

#[derive(Parser)]
#[command(version, about = "Git history inspection and playback")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
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
    },
    /// Play history in a loop; [ halves and ] doubles live speed (0.125x–8x).
    #[command(
        after_help = "Camera: overhead by default; pitch clamps to ±90° and the eye stays above the file plane. Right drag orbits. With settings closed, C or a left viewport click captures the mouse to aim; while captured, hold left / right buttons to move up / down. C or Escape releases capture; Escape otherwise closes preview. WASD pans, scroll zooms, Tab cycles visible contributors, Space restores overhead. Orbit, captured aim and zoom retain follow; pan or button elevation releases it. F10 toggles display settings; F11 toggles borderless fullscreen."
    )]
    Preview {
        #[arg(long, value_enum, default_value = "neon")]
        theme: theme::ThemeName,
        #[command(flatten)]
        source: Source,
    },
    /// Write PNG frames or stream video, with a manifest in a new directory.
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
        #[arg(long, value_parser = clap::value_parser!(u32).range(2..=1_000_000))]
        frames: Option<u32>,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u16).range(1..=240))]
        fps: u16,
        #[arg(long, default_value_t = 0)]
        frame_delay_ms: u16,
        #[command(flatten)]
        encoding: encoder::Options,
    },
}

#[derive(clap::Args, serde::Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent clap switch"
)]
struct Source {
    /// Assume departure after this many calendar days; default 60 marks two quiet months.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=36500))]
    contributor_inactivity_days: u32,
    /// Repository caption colour treatment.
    #[arg(long, value_enum, default_value = "bar")]
    caption_style: model::caption::Style,
    /// Local repository to play; repeat for a shared timeline; synthetic when omitted.
    #[arg(long)]
    repository: Vec<PathBuf>,
    /// TOML repository list; relative paths resolve beside the TOML file.
    #[arg(long)]
    repositories_file: Option<PathBuf>,
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
    /// Diff text reveal distance multiplier (0.1–100); larger reveals farther away.
    #[arg(long, default_value_t = 1.0, value_parser = diff_text_distance)]
    diff_text_distance: f32,
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
    /// Commits laid out and settled before playback begins.
    #[arg(long, default_value_t = 0)]
    start: usize,
    /// Commits to play after the start; defaults to the rest of the history.
    #[arg(long)]
    commits: Option<usize>,
    /// Canonical timeline seconds per calendar day; live speed keys leave the timeline fixed.
    #[arg(long, default_value_t = 0.5, value_parser = positive_seconds)]
    seconds_per_day: f32,
    /// Maximum canonical timeline seconds between commits, compressing idle gaps.
    #[arg(long, default_value_t = 1.0, value_parser = idle_seconds)]
    max_idle_seconds: f32,
    /// Maximum extra seconds per walking trip; none waits for the route, zero never waits.
    #[arg(long, default_value = "0.4", value_parser = travel_seconds)]
    travel_cap: model::TravelCap,
    /// Bot travel policy: fade across or walk the shortest gutter route.
    #[arg(long, value_enum, default_value = "fade")]
    bot_travel: model::BotTravel,
    /// Maximum avatar travel speed in pixels/second at the 1280x720 reference size.
    #[arg(long, default_value_t = 480.0, value_parser = avatar_speed)]
    avatar_max_speed: f32,
    /// Avatar disc diameter relative to the original size, in both playback modes (0.5–2).
    #[arg(long, default_value_t = 2.0, value_parser = avatar_size)]
    avatar_scale: f32,
    /// Visible spacing between repository tiles in reference pixels (0–20).
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
    ) -> anyhow::Result<Vec<timeline::Event>> {
        let mut histories = Vec::new();
        for (index, repository) in repositories.iter().enumerate() {
            let started = std::time::Instant::now();
            let mut events = cache::load(
                &repository.path,
                &self.revision,
                cache_root.filter(|_| !self.no_cache),
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
    ) {
        let fetch = self.fetch_avatars && !repositories.is_empty();
        let mut resolver = avatars::Resolver::new(
            fetch.then(avatars::Http::new),
            cache::root().filter(|_| !self.no_cache),
            fetch,
            avatars::now(),
        );
        resolver.gitlab_email = self.gitlab_email_avatars;
        let start = self.start.min(events.len());
        let end = self.commits.map_or(events.len(), |count| {
            start.saturating_add(count).min(events.len())
        });
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

    fn load(&self) -> anyhow::Result<(model::Simulation, String)> {
        let repositories =
            repositories::inputs(&self.repository, self.repositories_file.as_deref())?;
        anyhow::ensure!(
            repositories.len() <= repositories::MAX_REPOSITORIES,
            "multi-repository playback supports at most {} repositories (received {})",
            repositories::MAX_REPOSITORIES,
            repositories.len()
        );
        let mappings = contributors::load(self.contributors.as_deref())?;
        let mut events = self.events(cache::root().as_deref(), &repositories)?;
        mappings.apply(&mut events);
        self.resolve_avatars(&mut events, &repositories);
        let names: Vec<_> = repositories
            .iter()
            .map(repositories::Repository::name)
            .collect();
        let title = if names.is_empty() {
            "Synthetic history".to_owned()
        } else {
            names.join(" | ")
        };
        let options = model::Options {
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
            start: self.start,
            commits: self.commits,
            travel_cap: self.travel_cap,
            bot_travel: self.bot_travel,
            avatar_max_speed: self.avatar_max_speed,
            avatar_size: self.avatar_scale,
            gutter: self.gutter,
        };
        let mut simulation = Self::playback(events, &options, &mappings, &names)?;
        simulation.repository_boundary = self.repository_boundary;
        simulation.collapse_directories = self.collapse_directories;
        simulation.diff_text_distance = self.diff_text_distance;
        simulation.caption_style = self.caption_style;
        simulation.repository_label_plane = self.repository_label_plane;
        Ok((simulation, title))
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

fn travel_seconds(value: &str) -> Result<model::TravelCap, String> {
    if value == "none" {
        return Ok(model::TravelCap::None);
    }
    let seconds = value.parse::<f32>().map_err(|e| e.to_string())?;
    if seconds.is_finite() && (0.0..=3600.0).contains(&seconds) {
        Ok(model::TravelCap::Seconds(seconds))
    } else {
        Err("travel cap must be none or finite seconds between 0 and 3600".to_owned())
    }
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
    value
        .parse::<f32>()
        .ok()
        .filter(|scale| (0.5..=2.0).contains(scale))
        .ok_or_else(|| "avatar scale must be between 0.5 and 2".to_owned())
}

fn diff_text_distance(value: &str) -> Result<f32, String> {
    value
        .parse::<f32>()
        .ok()
        .filter(|distance| (0.1..=100.0).contains(distance))
        .ok_or_else(|| "diff text distance must be between 0.1 and 100".to_owned())
}

fn main() -> std::process::ExitCode {
    match run() {
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

fn run() -> anyhow::Result<()> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) if error.use_stderr() => return Err(error.into()),
        Err(error) => {
            print!("{error}");
            return Ok(());
        }
    };
    match cli.command {
        Some(Command::LayoutQuality {
            source,
            output,
            treemap,
        }) => {
            let started = std::time::Instant::now();
            let (mut simulation, _) = source.load()?;
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
                    "duration_seconds": simulation.duration(), "tick_rate": model::TICK_RATE,
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
        }) => {
            let history = history::import(&repository, &revision)?;
            serde_json::to_writer_pretty(std::io::stdout().lock(), &history)?;
        }
        Some(Command::Preview { source, theme }) => {
            let (mut simulation, title) = source.load()?;
            simulation.set_theme(theme.build());
            scene::preview(simulation, title);
        }
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
            let (mut simulation, title) = source.load()?;
            simulation.set_theme(theme.build());
            let frames = frames
                .map_or_else(|| model::frames_for(simulation.duration(), fps), Ok)?;
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
    fn diff_text_distance_is_shared_by_preview_and_export() {
        for command in [vec!["preview"], vec!["export", "frames"]] {
            for value in ["0", "-1", "0.09", "101", "NaN", "inf", "bad"] {
                let args = [
                    vec!["codestrand"],
                    command.clone(),
                    vec!["--diff-text-distance", value],
                ]
                .concat();
                assert!(Cli::try_parse_from(args).is_err());
            }
            for (flags, expected) in [
                (vec![], 1.0),
                (vec!["--diff-text-distance", "4"], 4.0),
                (vec!["--diff-text-distance", "0.1"], 0.1),
                (vec!["--diff-text-distance", "100"], 100.0),
            ] {
                let args = [vec!["codestrand"], command.clone(), flags].concat();
                let cli = Cli::try_parse_from(args).unwrap();
                let (Command::Preview { source, .. } | Command::Export { source, .. }) =
                    cli.command.unwrap()
                else {
                    panic!("playback command")
                };
                let (simulation, _) = source.load().unwrap();
                assert_eq!(simulation.diff_text_distance, expected);
            }
        }
    }

    #[test]
    fn pacing_flags_reach_the_loaded_schedule() {
        let cli = Cli::try_parse_from([
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
        let (simulation, _) = source.load().unwrap();
        let starts = |rate, idle| {
            model::schedule::Schedule::new(&timeline::synthetic(), rate, idle).starts
        };
        assert_eq!(simulation.scheduled_starts(), starts(2.0, 5.0));
        assert_ne!(starts(2.0, 5.0), starts(1.0, 5.0));
        assert_ne!(starts(2.0, 5.0), starts(2.0, 1.0));
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
                Cli::try_parse_from(["codestrand", "export", "frames", option])
                    .is_err()
            );
            assert!(
                Cli::try_parse_from([
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
            Cli::try_parse_from([
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
                assert!(Cli::try_parse_from(args).is_err());
            }
            let mut args = vec!["codestrand"];
            args.extend(command);
            args.extend(["--heat-half-life", "2"]);
            let cli = Cli::try_parse_from(args).unwrap();
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
            let cli = Cli::try_parse_from(args).unwrap();
            let Some(Command::Preview { source, .. }) = cli.command else {
                panic!("preview command parsed")
            };
            assert_eq!(source.fetch_avatars, fetch);
            assert_eq!(source.gitlab_email_avatars, email);
        }
    }

    #[test]
    fn cli_combines_repeated_repositories_and_a_manifest() {
        let cli = Cli::try_parse_from([
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
            ("bevelled-desktop", ThemeName::BevelledDesktop),
            ("atlas", ThemeName::Atlas),
            ("forge", ThemeName::Forge),
        ] {
            let cli = Cli::try_parse_from(["codestrand", "preview", "--theme", name])
                .unwrap();
            let Some(Command::Preview { theme, .. }) = cli.command else {
                panic!("preview")
            };
            assert_eq!(theme, expected);
            assert_eq!(theme.build().name, theme);
            let cli = Cli::try_parse_from([
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
            assert!(Cli::try_parse_from(command).is_err());
        }
        let cli = Cli::try_parse_from(["codestrand", "preview"]).unwrap();
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
            Cli::try_parse_from(["codestrand", "preview", "--max-idle-seconds", &cap])
                .is_ok()
        );
    }

    #[test]
    fn avatar_scale_defaults_to_two_and_accepts_the_original_size() {
        for (flags, expected) in [(vec![], 2.0), (vec!["--avatar-scale", "1"], 1.0)] {
            let cli =
                Cli::try_parse_from([vec!["codestrand", "preview"], flags].concat())
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
    fn cli_accepts_uncapped_travel_and_walking_bots_but_rejects_invalid_limits() {
        let cli = Cli::try_parse_from([
            "codestrand",
            "preview",
            "--travel-cap",
            "none",
            "--bot-travel",
            "walk",
        ])
        .unwrap();
        let Some(Command::Preview { source, .. }) = cli.command else {
            panic!("preview")
        };
        assert_eq!(source.travel_cap, model::TravelCap::None);
        assert_eq!(source.bot_travel, model::BotTravel::Walk);
        for value in ["NaN", "inf", "-1", "3601", "unlimited"] {
            assert!(
                Cli::try_parse_from(["codestrand", "preview", "--travel-cap", value])
                    .is_err()
            );
        }
        assert_eq!(
            travel_seconds("3600").unwrap(),
            model::TravelCap::Seconds(3600.0)
        );
    }
}
