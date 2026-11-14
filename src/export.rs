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

use crate::{encoder::Encoder, model::Simulation, scene};

const WAIT_LIMIT: Duration = Duration::from_secs(30);
const MAX_PENDING_CAPTURES: usize = 4;

mod png;

pub(super) fn prepare(directory: &Path) -> anyhow::Result<()> {
    fs::create_dir(directory)
        .with_context(|| format!("create new export directory {}", directory.display()))
}

pub(super) fn run(
    directory: &Path,
    (simulation, camera_path, resolution): (
        Simulation,
        Option<crate::camera_path::CameraPath>,
        crate::resolution::Resolution,
    ),
    title: String,
    frames: u32,
    fps: u16,
    delay_ms: u16,
    encoder: Option<crate::encoder::Encoder>,
) -> anyhow::Result<()> {
    render(
        directory,
        scene::headless(simulation, title, fps, resolution),
        frames,
        fps,
        delay_ms,
        encoder,
        camera_path.as_ref(),
    )
}

fn render(
    directory: &Path,
    (mut app, target): (App, Handle<Image>),
    frames: u32,
    fps: u16,
    delay_ms: u16,
    mut encoder: Option<crate::encoder::Encoder>,
    camera_path: Option<&crate::camera_path::CameraPath>,
) -> anyhow::Result<()> {
    let mut manifest = BufWriter::new(File::create(directory.join("frames.csv"))?);
    writeln!(
        manifest,
        "frame,seconds,commit,files,avatars,beams,glyphs,missing_glyphs"
    )?;
    initialize(&mut app, &target)?;
    let mut png = encoder.is_none().then(png::Writer::new).transpose()?;
    let started = Instant::now();
    let mut previous_camera = None;
    let mut captures = Captures::default();
    let rendered = (|| -> anyhow::Result<()> {
        for index in 0..frames {
            if let Err(error) = place_export_camera(
                &mut app,
                camera_path,
                &mut previous_camera,
                (index, fps),
            ) {
                captures.finish(
                    index,
                    || update(&mut app),
                    |index, image| {
                        write_frame(directory, &mut encoder, png.as_mut(), index, image)
                    },
                )?;
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
            captures.write_ready(|index, image| {
                write_frame(directory, &mut encoder, png.as_mut(), index, image)
            })?;
            let (glyphs, missing_glyphs) = glyph_coverage(&mut app);
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
            while captures.pending.len() >= MAX_PENDING_CAPTURES {
                update(&mut app)?;
                captures.write_ready(|index, image| {
                    write_frame(directory, &mut encoder, png.as_mut(), index, image)
                })?;
            }
            thread::sleep(Duration::from_millis(u64::from(delay_ms)));
            if index % u32::from(fps) == 0 && index + 1 < frames {
                eprintln!(
                    "Exported {}/{} frames ({:.1}s elapsed)",
                    index + 1,
                    frames,
                    started.elapsed().as_secs_f32()
                );
            }
        }
        captures.finish(
            frames,
            || update(&mut app),
            |index, image| {
                write_frame(directory, &mut encoder, png.as_mut(), index, image)
            },
        )?;
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
        "Exported {frames}/{frames} frames ({:.1}s elapsed)",
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
        let simulation = Simulation::new(
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
        render(&output, (app, target), 70, 5, 0, None, None).unwrap();
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
