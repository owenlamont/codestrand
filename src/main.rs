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
mod contributors;
mod diff_samples;
mod encoder;
mod export;
mod history;
mod layout;
mod model;
mod repositories;
mod scene;
mod theme;
mod timeline;

fn terminal_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if (character.is_control() && !matches!(character, '\n' | '\t'))
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
            super::terminal_escape("error:\n\ttip\r\u{1b}[31m\u{202e}"),
            "error:\n\ttip\\r\\u{1b}[31m\\u{202e}"
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
#[command(version, about = "Git history inspection and layout playback study")]
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
    /// Play history in a loop; [ halves and ] doubles live speed (0.125x–8x). Escape closes it.
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
    /// Local repository to play; repeat for a shared timeline; synthetic when omitted.
    #[arg(long)]
    repository: Vec<PathBuf>,
    /// TOML repository list; relative paths resolve beside the TOML file.
    #[arg(long)]
    repositories_file: Option<PathBuf>,
    /// Repository boundary treatment for multiple repositories.
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
        let mappings = contributors::load(self.contributors.as_deref())?;
        let mut events = self.events(cache::root().as_deref(), &repositories)?;
        mappings.apply(&mut events);
        self.resolve_avatars(&mut events, &repositories);
        let title = if repositories.is_empty() {
            "Synthetic history".to_owned()
        } else {
            repositories
                .iter()
                .map(repositories::Repository::name)
                .collect::<Vec<_>>()
                .join(" | ")
        };
        let options = model::Options {
            heat_half_life: self.heat_half_life,
            seconds_per_day: self.seconds_per_day,
            max_idle_seconds: self.max_idle_seconds,
            start: self.start,
            commits: self.commits,
        };
        let names = if repositories.len() > 1 {
            repositories
                .iter()
                .map(repositories::Repository::name)
                .collect()
        } else {
            Vec::new()
        };
        let mut simulation = if names.is_empty() {
            model::Simulation::new(events, &options, &theme::Theme::neon(), &mappings)
        } else {
            model::Simulation::check_repository_plan(&events, &options, names.len())?;
            model::Simulation::with_repositories(
                events,
                &options,
                &theme::Theme::neon(),
                &mappings,
                &names,
            )
        };
        simulation.repository_boundary = self.repository_boundary;
        simulation.repository_label_plane = self.repository_label_plane;
        Ok((simulation, title))
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

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            if let Some(usage) = error.downcast_ref::<clap::Error>() {
                eprint!("{}", terminal_escape(&usage.to_string()));
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
        }) => {
            let encoder = encoding.start(fps)?;
            export::prepare(&directory)?;
            let (mut simulation, title) = source.load().inspect_err(|_| {
                let _ = std::fs::remove_dir(&directory);
            })?;
            simulation.set_theme(theme.build());
            let frames = frames
                .map_or_else(|| model::frames_for(simulation.duration(), fps), Ok)
                .inspect_err(|_| {
                    let _ = std::fs::remove_dir(&directory);
                })?;
            export::run(
                &directory,
                simulation,
                title,
                frames,
                fps,
                frame_delay_ms,
                encoder,
            )?;
        }
        None => Cli::command().print_help()?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        for name in ["neon", "phosphor"] {
            let cli = Cli::try_parse_from(["codestrand", "preview", "--theme", name])
                .unwrap();
            let Some(Command::Preview { theme, .. }) = cli.command else {
                panic!("preview")
            };
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
}
