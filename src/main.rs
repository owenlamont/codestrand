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
mod scene;
mod theme;
mod timeline;

use std::path::{Path, PathBuf};

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
    /// Local repository to play; a built-in synthetic history plays when omitted.
    #[arg(long)]
    repository: Option<PathBuf>,
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
        cache_root: Option<&Path>,
    ) -> Result<Vec<timeline::Event>, history::Error> {
        let mut events = match &self.repository {
            Some(path) => {
                cache::load(path, &self.revision, cache_root.filter(|_| !self.no_cache))
            }
            None => Ok(timeline::synthetic()),
        }?;
        if self.no_diff_samples {
            for change in events.iter_mut().flat_map(|event| &mut event.changes) {
                change.samples.clear();
            }
        }
        Ok(events)
    }

    fn load(&self) -> anyhow::Result<(model::Simulation, String)> {
        let mappings = contributors::load(self.contributors.as_deref())?;
        let mut events = self.events(cache::root().as_deref())?;
        mappings.apply(&mut events);
        let remote = self.repository.as_deref().and_then(avatars::repository);
        let fetch = self.fetch_avatars && self.repository.is_some();
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
        resolver.resolve(&mut events[start..end], remote.as_deref());
        let (events, title) = match &self.repository {
            Some(path) => {
                let name = path
                    .canonicalize()
                    .ok()
                    .and_then(|path| {
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    })
                    .unwrap_or_else(|| path.display().to_string());
                (events, name)
            }
            None => (events, "Synthetic history".to_owned()),
        };
        let options = model::Options {
            heat_half_life: self.heat_half_life,
            seconds_per_day: self.seconds_per_day,
            max_idle_seconds: self.max_idle_seconds,
            start: self.start,
            commits: self.commits,
        };
        Ok((
            model::Simulation::new(events, &options, &theme::Theme::neon(), &mappings),
            title,
        ))
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

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Some(Command::LayoutQuality { source, output }) => {
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
            model::quality::run(&mut simulation, &mut writer)?;
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
