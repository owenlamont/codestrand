use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use bevy::{
    app::PluginsState,
    prelude::{App, Handle, Image, InheritedVisibility, On, Text},
    render::{
        RenderApp,
        render_resource::{PipelineCache, PollType},
        renderer::RenderDevice,
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    text::{ComputedTextBlock, TextLayoutInfo},
};

use crate::{encoder::Encoder, scene};

const WAIT_LIMIT: Duration = Duration::from_secs(30);
const MAX_PENDING_CAPTURES: usize = 4;
const PLAN_POLL: Duration = Duration::from_millis(50);
pub(crate) const MANIFEST_HEADER: &str =
    "frame,seconds,commit,files,avatars,beams,glyphs,missing_glyphs";

mod png;
pub(super) mod report;

pub(super) fn prepare(directory: &Path) -> anyhow::Result<()> {
    fs::create_dir(directory)
        .with_context(|| format!("create new export directory {}", directory.display()))
}

pub(super) fn render(
    directory: &Path,
    (mut app, target): (App, Handle<Image>),
    frames: Option<u32>,
    fps: u16,
    delay_ms: u16,
    mut encoder: Option<crate::encoder::Encoder>,
    camera_path: Option<&crate::camera_path::CameraPath>,
) -> anyhow::Result<()> {
    let mut manifest = BufWriter::new(File::create(directory.join("frames.csv"))?);
    writeln!(manifest, "{MANIFEST_HEADER}")?;
    initialize(&mut app, &target)?;
    let mut png = encoder.is_none().then(png::Writer::new).transpose()?;
    let started = Instant::now();
    let mut report = report::Report::new(started, fps);
    let (mut previous_camera, mut captures) = (None, Captures::default());
    let mut index = 0;
    let rendered = (|| -> anyhow::Result<()> {
        let mut write = |index, image| {
            write_frame(directory, &mut encoder, png.as_mut(), index, image)
        };
        loop {
            if frames.is_some_and(|count| index >= count) {
                break;
            }
            let waiting = !app
                .world()
                .resource::<scene::Playback>()
                .0
                .frame_ready(index, fps);
            prepare_after_captures(
                &mut app,
                &mut captures,
                index,
                waiting,
                update,
                &mut write,
                |app| wait_for_plan(app, &mut report, index, fps),
            )?;
            let playback = &app.world().resource::<scene::Playback>().0;
            let planned = crate::model::frames_for(playback.duration(), fps)?;
            if frames.is_none() && playback.planning_complete() && index >= planned {
                break;
            }
            if let Err(error) = place_export_camera(
                &mut app,
                camera_path,
                &mut previous_camera,
                (index, fps),
            ) {
                captures.finish(index, || update(&mut app), &mut write)?;
                return Err(error);
            }
            app.world_mut().resource_mut::<scene::Frame>().index = index;
            let sender = captures.request();
            app.world_mut()
                .spawn(Screenshot::image(target.clone()))
                .observe(move |event: On<ScreenshotCaptured>| {
                    let _ = sender.send(event.image.clone());
                });
            update(&mut app)?;
            captures.write_ready(&mut write)?;
            record_frame(&mut app, &mut manifest, index)?;
            while captures.pending.len() >= MAX_PENDING_CAPTURES {
                update(&mut app)?;
                captures.write_ready(&mut write)?;
            }
            thread::sleep(Duration::from_millis(u64::from(delay_ms)));
            report_progress(
                &app,
                &mut report,
                captures.written,
                frames,
                (planned, fps),
            );
            index += 1;
        }
        captures.finish(index, || update(&mut app), &mut write)?;
        Ok(())
    })();
    let png_result = png.map_or(Ok(()), png::Writer::finish);
    if let Err(error) = rendered {
        manifest.flush()?;
        encoder.as_mut().map_or(Ok(()), Encoder::finish)?;
        return Err(error);
    }
    png_result?;
    eprintln!(
        "Exported {index}/{index} frames ({:.1}s elapsed)",
        started.elapsed().as_secs_f32()
    );
    manifest.flush()?;
    encoder.as_mut().map_or(Ok(()), Encoder::finish)?;
    eprintln!(
        "Export complete: {}",
        crate::terminal_escape(&directory.to_string_lossy())
    );
    Ok(())
}

fn report_progress(
    app: &App,
    report: &mut report::Report,
    written: u32,
    frames: Option<u32>,
    (planned, fps): (u32, u16),
) {
    let playback = &app.world().resource::<scene::Playback>().0;
    let now = Instant::now();
    report.planned(now, playback.planned_history(), written);
    let length = match frames {
        Some(frames) => report::Length::Requested(frames),
        None if playback.planning_complete() => report::Length::Planned(planned),
        // A projection past the export range only says the video is too long.
        None => report::Length::Projected(
            crate::model::frames_for(playback.projected_duration(), fps)
                .unwrap_or(u32::MAX),
        ),
    };
    let history = playback.history_progress();
    if let Some(line) = report.progress(now, written, length, history) {
        eprintln!("{line}");
    }
}

/// Prepares frame `index`, polling the planner so a long wait is reported as one.
fn wait_for_plan(
    app: &mut App,
    report: &mut report::Report,
    index: u32,
    fps: u16,
) -> anyhow::Result<()> {
    loop {
        let mut playback = app.world_mut().resource_mut::<scene::Playback>();
        if playback.0.prepare_frame(index, fps, false)? {
            return Ok(());
        }
        if let Some(line) = report.waiting(Instant::now(), playback.0.planned_history())
        {
            eprintln!("{line}");
        }
        thread::sleep(PLAN_POLL);
    }
}

fn record_frame(
    app: &mut App,
    manifest: &mut impl Write,
    index: u32,
) -> anyhow::Result<()> {
    let (glyphs, missing_glyphs) = glyph_coverage(app);
    let playback = &app.world().resource::<scene::Playback>().0;
    writeln!(
        manifest,
        "{index},{:.9},{},{},{},{},{glyphs},{missing_glyphs}",
        playback.seconds(),
        playback.commit(),
        playback.layout().present_files(),
        playback
            .avatars()
            .iter()
            .filter(|avatar| avatar.presence > 0.0)
            .count(),
        playback.beams().len()
    )?;
    Ok(())
}

fn prepare_after_captures<T>(
    context: &mut T,
    captures: &mut Captures,
    index: u32,
    waiting: bool,
    mut update: impl FnMut(&mut T) -> anyhow::Result<()>,
    write: impl FnMut(u32, Image) -> anyhow::Result<()>,
    prepare: impl FnOnce(&mut T) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if waiting {
        captures.finish(index, || update(context), write)?;
    }
    prepare(context)
}

fn place_export_camera(
    app: &mut App,
    path: Option<&crate::camera_path::CameraPath>,
    previous: &mut Option<bevy::prelude::Transform>,
    (index, fps): (u32, u16),
) -> anyhow::Result<()> {
    let Some(path) = path else { return Ok(()) };
    let mut playback = app.world_mut().resource_mut::<scene::Playback>();
    playback.0.advance_to(index, fps);
    let pose = path.pose(&playback.0, index, fps)?;
    if let Some(previous) = previous {
        path.validate_step(previous, &pose, index)?;
    }
    *previous = Some(pose);
    app.world_mut().insert_resource(scene::ScriptedCamera(pose));
    Ok(())
}

fn initialize(app: &mut App, target: &Handle<Image>) -> anyhow::Result<()> {
    let started = Instant::now();
    while app.plugins_state() != PluginsState::Ready {
        if started.elapsed() > WAIT_LIMIT {
            bail!("renderer plugins were not ready within 30 seconds");
        }
        bevy::tasks::tick_global_task_pools_on_main_thread();
        thread::sleep(Duration::from_millis(1));
    }
    app.finish();
    app.cleanup();
    validate_target(app, target)?;

    loop {
        update(app)?;
        if started.elapsed() > WAIT_LIMIT {
            bail!("text and rendering pipelines were not ready within 30 seconds");
        }
        if ready(app) {
            break;
        }
    }
    Ok(())
}

fn write_frame(
    directory: &Path,
    encoder: &mut Option<crate::encoder::Encoder>,
    png: Option<&mut png::Writer>,
    index: u32,
    image: Image,
) -> anyhow::Result<()> {
    if let Some(png) = png {
        return png.write(directory.join(format!("frame-{index:05}.png")), image);
    }
    let pixels = image
        .try_into_dynamic()
        .context("convert captured frame")?
        .to_rgb8();
    encoder
        .as_mut()
        .expect("video encoder exists when the PNG worker is absent")
        .write(pixels.into_raw())?;
    Ok(())
}

fn validate_target(app: &App, target: &Handle<Image>) -> anyhow::Result<()> {
    let limit = app
        .world()
        .resource::<RenderDevice>()
        .limits()
        .max_texture_dimension_2d;
    let size = app
        .world()
        .resource::<bevy::prelude::Assets<Image>>()
        .get(target)
        .expect("export render target")
        .size();
    if size.x > limit || size.y > limit {
        bail!(
            "export resolution {}x{} exceeds the graphics adapter's {limit}-pixel texture limit",
            size.x,
            size.y
        );
    }

    Ok(())
}

fn glyph_coverage(app: &mut App) -> (usize, usize) {
    let world = app.world_mut();
    let mut query = world.query::<(&ComputedTextBlock, &InheritedVisibility)>();
    let (mut total, mut missing) = (0, 0);
    for (text, _) in query.iter(world).filter(|(_, visibility)| visibility.get()) {
        for run in text.buffer().lines().flat_map(|line| line.runs()) {
            for glyph in run.clusters().flat_map(|cluster| cluster.glyphs()) {
                total += 1;
                missing += usize::from(glyph.id == 0);
            }
        }
    }
    (total, missing)
}

fn ready(app: &mut App) -> bool {
    let world = app.world_mut();
    let mut query = world.query::<(&Text, &TextLayoutInfo)>();
    let mut layouts = query
        .iter(world)
        .filter(|(text, _)| !text.0.trim().is_empty())
        .peekable();
    let labels_ready = layouts.peek().is_some()
        && layouts.all(|(_, layout)| !layout.glyphs.is_empty());
    labels_ready
        && app
            .sub_app(RenderApp)
            .world()
            .resource::<PipelineCache>()
            .waiting_pipelines()
            .next()
            .is_none()
}

#[derive(Default)]
struct Captures {
    pending: VecDeque<(mpsc::Receiver<Image>, Instant)>,
    written: u32,
}

impl Captures {
    fn request(&mut self) -> mpsc::Sender<Image> {
        let (sender, receiver) = mpsc::channel();
        self.pending.push_back((receiver, Instant::now()));
        sender
    }

    fn write_ready(
        &mut self,
        mut write: impl FnMut(u32, Image) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        while let Some((receiver, started)) = self.pending.front() {
            match receiver.try_recv() {
                Ok(image) => {
                    write(self.written, image)?;
                    self.written += 1;
                    self.pending.pop_front();
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    bail!("capture channel disconnected at frame {}", self.written)
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if started.elapsed() > WAIT_LIMIT {
                        bail!("capture timed out at frame {}", self.written);
                    }
                    break;
                }
            }
        }
        Ok(())
    }

    fn finish(
        &mut self,
        frames: u32,
        mut update: impl FnMut() -> anyhow::Result<()>,
        mut write: impl FnMut(u32, Image) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        while !self.pending.is_empty() {
            update()?;
            self.write_ready(&mut write)?;
        }
        if self.written != frames {
            bail!("captured {} frames but expected {frames}", self.written);
        }
        Ok(())
    }
}

fn update(app: &mut App) -> anyhow::Result<()> {
    app.update();
    app.world()
        .resource::<RenderDevice>()
        .wgpu_device()
        .poll(PollType::Wait {
            submission_index: None,
            timeout: Some(WAIT_LIMIT),
        })
        .context("wait for GPU frame completion")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contributors, model,
        theme::{Theme, ThemeName},
        timeline,
    };
    use bevy::{color::ColorToPacked, prelude::IntoScheduleConfigs};

    #[test]
    fn captures_write_in_request_order_once() {
        let mut captures = Captures::default();
        let first = captures.request();
        let second = captures.request();
        second.send(Image::default()).unwrap();
        let mut written = Vec::new();
        captures
            .write_ready(|index, _| {
                written.push(index);
                Ok(())
            })
            .unwrap();
        assert_eq!(written, [] as [u32; 0]);
        first.send(Image::default()).unwrap();
        for _ in 0..2 {
            captures
                .write_ready(|index, _| {
                    written.push(index);
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(written, [0, 1]);
    }

    #[test]
    fn captures_drain_delayed_tail() {
        let mut captures = Captures::default();
        let sender = captures.request();
        let mut updates = 0;
        let mut written = Vec::new();
        captures
            .finish(
                1,
                || {
                    updates += 1;
                    if updates == 2 {
                        sender.send(Image::default()).unwrap();
                    }
                    Ok(())
                },
                |index, _| {
                    written.push(index);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(updates, 2);
        assert_eq!(written, [0]);
    }

    #[test]
    fn pending_capture_drains_before_planning() {
        let mut captures = Captures::default();
        let sender = captures.request();
        let mut updates = 0;
        let written = std::cell::RefCell::new(Vec::new());
        prepare_after_captures(
            &mut updates,
            &mut captures,
            1,
            true,
            |updates| {
                *updates += 1;
                if *updates == 2 {
                    sender.send(Image::default()).unwrap();
                }
                Ok(())
            },
            |frame, _| {
                written.borrow_mut().push(frame);
                Ok(())
            },
            |updates| {
                assert_eq!(*updates, 2);
                assert_eq!(*written.borrow(), [0]);
                Ok(())
            },
        )
        .unwrap();
        assert!(captures.pending.is_empty());
        let next = captures.request();
        next.send(Image::default()).unwrap();
        captures
            .finish(
                2,
                || Ok(()),
                |frame, _| {
                    written.borrow_mut().push(frame);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(*written.borrow(), [0, 1]);
    }

    fn sample_source() -> (crate::Source, std::path::PathBuf) {
        let manifest_path = std::env::var("CODESTRAND_SAMPLE_MANIFEST").unwrap();
        let output = std::path::PathBuf::from(
            std::env::var_os("CODESTRAND_SAMPLE_OUTPUT").unwrap(),
        );
        let mut args = vec![
            "codestrand".to_owned(),
            "layout-quality".into(),
            "unused.jsonl".into(),
            "--repositories-file".into(),
            manifest_path,
        ];
        if let Ok(commits) = std::env::var("CODESTRAND_SAMPLE_COMMITS") {
            args.extend(["--commits".into(), commits]);
        }
        if let Ok(extra) = std::env::var("CODESTRAND_SAMPLE_EXTRA_ARGS") {
            args.extend(serde_json::from_str::<Vec<String>>(&extra).unwrap());
        }
        let cli = crate::Cli::try_fixture_from(args).unwrap();
        let Some(crate::Command::LayoutQuality { mut source, .. }) = cli.command else {
            panic!("fixture source");
        };
        if let Ok(seconds) = std::env::var("CODESTRAND_SAMPLE_WINDOW") {
            source.window_override = Some(if seconds == "whole" {
                crate::WindowOverride::Whole
            } else {
                crate::WindowOverride::Seconds(seconds.parse().unwrap())
            });
        }
        (source, output)
    }

    fn first_sample(app: &mut App, target: &Handle<Image>, output: &Path) {
        let mut captures = Captures::default();
        let sender = captures.request();
        app.world_mut()
            .spawn(Screenshot::image(target.clone()))
            .observe(move |event: On<ScreenshotCaptured>| {
                let _ = sender.send(event.image.clone());
            });
        captures
            .finish(
                1,
                || update(app),
                |_, image| {
                    image
                        .try_into_dynamic()
                        .unwrap()
                        .save(output.join("first.png"))
                        .map_err(Into::into)
                },
            )
            .unwrap();
    }

    #[test]
    #[ignore = "requires a graphics adapter; verifies streamed export bytes and failures"]
    fn streaming_export_timing_and_failure_paths() {
        use std::sync::Arc;
        let scratch = tempfile::tempdir().unwrap();
        let mut expected = Vec::new();
        for arm in 0..5 {
            let hook: model::streaming::WindowHook = Arc::new(move |index, sim| {
                if index != 1 {
                    return;
                }
                match arm {
                    2 => thread::sleep(WAIT_LIMIT + Duration::from_secs(1)),
                    3 => sim.force_unrouteable_test(),
                    4 => panic!("forced export worker panic"),
                    _ => (),
                }
            });
            let cap = if arm >= 3 {
                model::TravelCap::None
            } else {
                model::TravelCap::Seconds(0.4)
            };
            let simulation =
                model::streaming::tests::fixture_with(false, Some(hook), cap);
            let output = scratch.path().join(arm.to_string());
            prepare(&output).unwrap();
            let app = scene::headless(
                simulation,
                "Streaming fixture".into(),
                10,
                "640x360".parse().unwrap(),
            );
            let result = render(&output, app, None, 10, 0, None, None);
            if arm >= 3 {
                let failure = result.unwrap_err();
                let error = failure.to_string();
                assert!(
                    error.contains(if arm == 3 {
                        "no route for commit"
                    } else {
                        "forced export worker panic"
                    }),
                    "{error}"
                );
                assert!(output.join("frame-00000.png").exists());
                assert_eq!(
                    crate::termination(Err(failure)),
                    std::process::ExitCode::FAILURE
                );
                continue;
            }
            result.unwrap();
            let mut paths: Vec<_> = fs::read_dir(&output)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            paths.sort();
            let bytes: Vec<_> =
                paths.iter().map(|path| fs::read(path).unwrap()).collect();
            assert!(bytes.len() > 100);
            if arm == 0 {
                expected = bytes;
            } else {
                assert_eq!(bytes, expected, "export arm {arm}");
            }
        }
    }

    fn sample_memory() -> u64 {
        let available_memory = std::env::var("CODESTRAND_SAMPLE_MEMORY_BYTES")
            .map_or_else(
                |_| model::Options::default().available_memory,
                |value| value.parse().expect("injected available memory in bytes"),
            );
        eprintln!("sample_available_memory_bytes={available_memory}");
        available_memory
    }

    #[test]
    #[ignore = "requires an explicit video command, graphics adapter and run bound"]
    fn diagnosed_video_export() {
        use bevy::prelude::{PostUpdate, Res};
        use std::sync::{Arc, Mutex};

        let args: Vec<String> =
            serde_json::from_str(&std::env::var("CODESTRAND_PROOF_ARGS").unwrap())
                .unwrap();
        let Some(crate::Command::Export {
            source,
            directory,
            theme,
            fps,
            resolution,
            encoding,
            frames,
            frame_delay_ms,
            camera_path,
        }) = crate::Cli::try_fixture_from(args).unwrap().command
        else {
            panic!("explicit export command required");
        };
        assert!(camera_path.is_none());
        let available = sample_memory();
        let (mut simulation, title) = crate::Source::report_loading(
            Duration::from_secs(1),
            |update| {
                if let Some(update) = update {
                    eprintln!("{update}");
                }
            },
            |progress| source.load_with_memory(progress, available),
        )
        .unwrap();
        simulation.set_theme(theme.build());
        eprintln!(
            "diagnostic_capture_hz={fps} history_seconds={}",
            simulation.history_progress().1
        );
        prepare(&directory).unwrap();
        let encoder = encoding.start(fps, resolution).unwrap();
        assert!(encoder.is_some());
        let (mut app, target) = scene::headless(simulation, title, fps, resolution);
        let audit = Arc::new(Mutex::new((None, model::MovingBeams::default())));
        let observed = Arc::clone(&audit);
        app.add_systems(
            PostUpdate,
            move |frame: Res<scene::Frame>, playback: Res<scene::Playback>| {
                let mut audit = observed.lock().unwrap();
                if audit.0 != Some(frame.index) {
                    audit.0 = Some(frame.index);
                    audit.1.observe(&playback.0);
                }
            },
        );
        let result = render(
            &directory,
            (app, target),
            frames,
            fps,
            frame_delay_ms,
            encoder,
            None,
        );
        let audit = audit.lock().unwrap();
        fs::write(
            directory.join("diagnostics.json"),
            serde_json::to_vec_pretty(&audit.1).unwrap(),
        )
        .unwrap();
        result.unwrap();
        audit.1.assert_clear(source.avatar_max_speed);
    }

    #[test]
    #[ignore = "requires a graphics adapter and an explicit history fixture"]
    fn streaming_history_sample_export() {
        let (source, output) = sample_source();
        let started = Instant::now();
        let (simulation, title) = source
            .load_with_memory(&crate::progress::Progress::default(), sample_memory())
            .unwrap();
        eprintln!(
            "sample_first_window_seconds={}",
            started.elapsed().as_secs_f64()
        );
        prepare(&output).unwrap();
        let (mut app, target) =
            scene::headless(simulation, title, 60, "1280x720".parse().unwrap());
        initialize(&mut app, &target).unwrap();
        first_sample(&mut app, &target, &output);
        eprintln!(
            "sample_first_frame_seconds={}",
            started.elapsed().as_secs_f64()
        );
        let mut captures = Captures::default();
        let mut png = png::Writer::new().unwrap();
        let mut samples =
            BufWriter::new(File::create(output.join("samples.csv")).unwrap());
        writeln!(samples, "sample,frame,history_seconds").unwrap();
        let mut frame = 0;
        let mut sample = 1;
        loop {
            let waiting = !app
                .world()
                .resource::<scene::Playback>()
                .0
                .frame_ready(frame, 60);
            prepare_after_captures(
                &mut app,
                &mut captures,
                sample - 1,
                waiting,
                update,
                |index, image| {
                    png.write(output.join(format!("frame-{index:05}.png")), image)
                },
                |app| {
                    app.world_mut()
                        .resource_mut::<scene::Playback>()
                        .0
                        .prepare_frame(frame, 60, true)
                        .map(|_| ())
                },
            )
            .unwrap();
            let mut playback = app.world_mut().resource_mut::<scene::Playback>();
            playback.0.advance_to(frame, 60);
            let (history, duration) = playback.0.history_progress();
            let last = playback.0.planning_complete()
                && frame >= model::frames_for(playback.0.duration(), 60).unwrap();
            if (sample < 4
                && history.lossy_secs_f32()
                    >= duration.lossy_secs_f32() * model::count_u32(sample) / 4.0)
                || last
            {
                writeln!(samples, "{sample},{frame},{history}").unwrap();
                app.world_mut().resource_mut::<scene::Frame>().index = frame;
                let sender = captures.request();
                app.world_mut()
                    .spawn(Screenshot::image(target.clone()))
                    .observe(move |event: On<ScreenshotCaptured>| {
                        let _ = sender.send(event.image.clone());
                    });
                update(&mut app).unwrap();
                captures
                    .write_ready(|index, image| {
                        png.write(output.join(format!("frame-{index:05}.png")), image)
                    })
                    .unwrap();
                sample += 1;
            }
            if last {
                break;
            }
            frame += 1;
        }
        captures
            .finish(
                sample - 1,
                || update(&mut app),
                |index, image| {
                    png.write(output.join(format!("frame-{index:05}.png")), image)
                },
            )
            .unwrap();
        png.finish().unwrap();
        assert_eq!(sample, 5);
        eprintln!(
            "sample_complete_frames={frame} seconds={}",
            started.elapsed().as_secs_f64()
        );
    }

    #[test]
    fn captures_reject_manifest_count_mismatch() {
        let error = Captures::default()
            .finish(1, || Ok(()), |_, _| Ok(()))
            .unwrap_err();
        assert_eq!(error.to_string(), "captured 0 frames but expected 1");
    }

    #[test]
    fn captures_reject_expired_pending_frame() {
        let mut captures = Captures::default();
        let _sender = captures.request();
        captures.pending.front_mut().unwrap().1 =
            Instant::now().checked_sub(WAIT_LIMIT).unwrap();
        let error = captures.write_ready(|_, _| Ok(())).unwrap_err();
        assert_eq!(error.to_string(), "capture timed out at frame 0");
    }

    #[test]
    #[ignore = "requires a graphics adapter; writes a live theme-switch sequence"]
    fn live_theme_switch_export() {
        let scratch = tempfile::tempdir().unwrap();
        let output = std::env::var_os("CODESTRAND_THEME_PROOF").map_or_else(
            || scratch.path().join("live-switch"),
            std::path::PathBuf::from,
        );
        prepare(&output).unwrap();
        let mut events = timeline::synthetic();
        contributors::Mappings::default().apply(&mut events);
        let mut pixels = vec![0; 128 * 128 * 4];
        for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let color = if (index % 128 / 16 + index / 128 / 16) % 2 == 0 {
                [230, 160, 65, 255]
            } else {
                [65, 105, 190, 255]
            };
            pixel.copy_from_slice(&color);
        }
        let portrait = std::sync::Arc::new(crate::avatars::Portrait { pixels });
        let key = events[0].contributor.as_ref().unwrap().key.clone();
        for event in &mut events {
            let contributor = event.contributor.as_mut().unwrap();
            if contributor.key == key {
                contributor.portrait = Some(portrait.clone());
            }
        }
        let simulation = crate::model::Simulation::new(
            events,
            &model::Options {
                start: 4,
                seconds_per_day: 2.0,
                max_idle_seconds: 2.0,
                ..Default::default()
            },
            &Theme::neon(),
            &contributors::Mappings::default(),
        );
        let (mut app, target) = scene::headless(
            simulation,
            "Live theme switch / portrait fixture".to_owned(),
            5,
            crate::resolution::Resolution::default(),
        );
        app.add_systems(
            bevy::prelude::Update,
            (|world: &mut bevy::prelude::World| {
                let index = world.resource::<scene::Frame>().index;
                let wanted = ThemeName::ALL[usize::try_from(index / 10)
                    .unwrap()
                    .min(ThemeName::ALL.len() - 1)];
                if world.resource::<scene::Playback>().0.theme().name != wanted {
                    scene::switch_theme(world, wanted);
                }
            })
            .before(scene::animate),
        );
        render(&output, (app, target), Some(70), 5, 0, None, None).unwrap();
        for index in 0..70 {
            let name = ThemeName::ALL[index / 10];
            let image = image::open(output.join(format!("frame-{index:05}.png")))
                .unwrap()
                .to_rgb8();
            let corner = image.get_pixel(0, 0);
            let expected = name.build().clear.to_srgba().to_u8_array();
            assert!(
                corner
                    .0
                    .iter()
                    .zip(expected)
                    .all(|(&actual, expected)| actual.abs_diff(expected) <= 3),
                "{} must replace the previous camera target",
                name.title()
            );
        }
    }
}

#[cfg(test)]
mod pacing_tests {
    use std::{fs, path::Path};
    #[test]
    #[ignore = "requires a graphics adapter; repeats sibling-directory junction exports"]
    fn sibling_directory_exports_are_identical() {
        let scratch = tempfile::tempdir().unwrap();
        let repository = scratch.path().join("repository");
        let repo = gix::init_bare(&repository).unwrap();
        let blob = repo.write_blob(b"inline fixture\n").unwrap().detach();
        let tree = junction_tree(&repo, blob, 3);
        let author = gix::actor::Signature {
            name: "Fixture".into(),
            email: "fixture@example.invalid".into(),
            time: gix::date::Time {
                seconds: 1_700_000_000,
                offset: 0,
            },
        };
        let commit = repo
            .write_object(&gix::objs::Commit {
                tree,
                parents: std::iter::empty().collect(),
                author: author.clone(),
                committer: author,
                encoding: None,
                message: "sibling directories".into(),
                extra_headers: Vec::new(),
            })
            .unwrap()
            .detach();
        fs::write(repository.join("HEAD"), format!("{commit}\n")).unwrap();
        let camera = scratch.path().join("camera.toml");
        fs::write(&camera, "fps=30\n[[key]]\nframe=0\neye={point=[0.0,12.0,0.0]}\nfocus={point=[0.0,0.0,0.0]}\n").unwrap();
        let first = scratch.path().join("first");
        let second = scratch.path().join("second");
        for directory in [&first, &second] {
            let args = vec![
                "codestrand",
                "export",
                directory.to_str().unwrap(),
                "--repository",
                repository.to_str().unwrap(),
                "--camera-path",
                camera.to_str().unwrap(),
                "--start",
                "1",
                "--frames",
                "24",
                "--fps",
                "30",
                "--no-cache",
            ];
            crate::dispatch(crate::Cli::try_fixture_from(args).unwrap()).unwrap();
        }
        for frame in 0..24 {
            let filename = format!("frame-{frame:05}.png");
            let first = image::open(first.join(&filename)).unwrap().to_rgb8();
            let second = image::open(second.join(&filename)).unwrap().to_rgb8();
            assert_eq!(first.dimensions(), second.dimensions());
            let mut count = 0;
            let mut bounds = (u32::MAX, u32::MAX, 0, 0);
            for (x, y, pixel) in first.enumerate_pixels() {
                if pixel != second.get_pixel(x, y) {
                    count += 1;
                    bounds = (
                        bounds.0.min(x),
                        bounds.1.min(y),
                        bounds.2.max(x),
                        bounds.3.max(y),
                    );
                }
            }
            assert_eq!(
                count, 0,
                "{filename}: {count} unequal pixels, inclusive bounds {bounds:?}"
            );
        }
    }

    fn junction_tree(
        repo: &gix::Repository,
        blob: gix::ObjectId,
        depth: u8,
    ) -> gix::ObjectId {
        let entries = if depth == 0 {
            vec![gix::objs::tree::Entry {
                mode: gix::objs::tree::EntryKind::Blob.into(),
                filename: "file.rs".into(),
                oid: blob,
            }]
        } else {
            let child = junction_tree(repo, blob, depth - 1);
            ["a", "b", "c", "d"]
                .into_iter()
                .map(|name| gix::objs::tree::Entry {
                    mode: gix::objs::tree::EntryKind::Tree.into(),
                    filename: name.into(),
                    oid: child,
                })
                .collect()
        };
        repo.write_object(&gix::objs::Tree { entries })
            .unwrap()
            .detach()
    }

    #[test]
    #[ignore = "requires a graphics adapter; renders two short PNG sequences"]
    fn export_is_complete_and_independent_of_render_pacing() {
        let scratch = tempfile::tempdir().expect("temporary directory is available");
        let fast = scratch.path().join("fast");
        let slow = scratch.path().join("slow");
        export(&fast, "0");
        export(&slow, "50");
        let expected: Vec<_> = (0..8)
            .map(|index| format!("frame-{index:05}.png"))
            .collect();
        for directory in [&fast, &slow] {
            let mut actual: Vec<_> = fs::read_dir(directory)
                .expect("export directory exists")
                .map(|entry| entry.expect("directory entry is readable").file_name())
                .filter(|name| {
                    Path::new(name).extension().is_some_and(|ext| ext == "png")
                })
                .collect();
            actual.sort();
            let expected_names: Vec<_> =
                expected.iter().map(std::ffi::OsString::from).collect();
            assert_eq!(actual, expected_names);
        }
        for filename in &expected {
            let fast_image = image::open(fast.join(filename))
                .expect("PNG is decodable")
                .to_rgb8();
            let slow_image = image::open(slow.join(filename))
                .expect("PNG is decodable")
                .to_rgb8();
            assert_eq!(fast_image.dimensions(), (1280, 720));
            assert!(fast_image == slow_image, "render pacing changed {filename}");
        }
        let first = fs::read(fast.join(&expected[0])).expect("first frame exists");
        let last = fs::read(fast.join(&expected[7])).expect("last frame exists");
        assert!(first != last, "scene did not advance");
        let fast_manifest =
            fs::read_to_string(fast.join("frames.csv")).expect("manifest exists");
        let slow_manifest =
            fs::read_to_string(slow.join("frames.csv")).expect("manifest exists");
        assert_eq!(fast_manifest.lines().count(), 9);
        assert_eq!(fast_manifest, slow_manifest);
    }

    fn export(directory: &Path, delay: &str) {
        let camera = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/camera-targets.toml");
        let args = vec![
            "codestrand",
            "export",
            directory.to_str().unwrap(),
            "--camera-path",
            camera.to_str().unwrap(),
            "--test-input",
            "synthetic",
            "--frames",
            "8",
            "--fps",
            "2",
            "--frame-delay-ms",
            delay,
        ];
        crate::dispatch(crate::Cli::try_fixture_from(args).unwrap()).unwrap();
    }
}
