use std::{
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
    prelude::{App, Handle, Image, On, Text},
    render::{
        RenderApp,
        render_resource::{PipelineCache, PollType},
        renderer::RenderDevice,
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    text::TextLayoutInfo,
};

use crate::{model::Simulation, scene};

const WAIT_LIMIT: Duration = Duration::from_secs(30);

pub(super) fn prepare(directory: &Path) -> anyhow::Result<()> {
    fs::create_dir(directory)
        .with_context(|| format!("create new export directory {}", directory.display()))
}

pub(super) fn run(
    directory: &Path,
    simulation: Simulation,
    title: String,
    frames: u32,
    fps: u16,
    delay_ms: u16,
    encoder: Option<crate::encoder::Encoder>,
) -> anyhow::Result<()> {
    render(
        directory,
        scene::headless(simulation, title, fps),
        frames,
        fps,
        delay_ms,
        encoder,
    )
}

fn render(
    directory: &Path,
    (mut app, target): (App, Handle<Image>),
    frames: u32,
    fps: u16,
    delay_ms: u16,
    mut encoder: Option<crate::encoder::Encoder>,
) -> anyhow::Result<()> {
    let mut manifest = BufWriter::new(File::create(directory.join("frames.csv"))?);
    writeln!(manifest, "frame,seconds,commit,files,avatars,beams")?;
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
    let readiness_started = Instant::now();
    loop {
        update(&mut app)?;
        if ready(&mut app) {
            break;
        }
        if readiness_started.elapsed() > WAIT_LIMIT {
            bail!("text and rendering pipelines were not ready within 30 seconds");
        }
    }
    let started = Instant::now();
    for index in 0..frames {
        app.world_mut().resource_mut::<scene::Frame>().index = index;
        let image = capture(&mut app, &target, index)?;
        let pixels = image
            .try_into_dynamic()
            .context("convert captured frame")?
            .to_rgb8();
        if let Some(encoder) = &mut encoder {
            encoder.write(pixels.into_raw())?;
        } else {
            let path = directory.join(format!("frame-{index:05}.png"));
            pixels
                .save(&path)
                .with_context(|| format!("write frame {}", path.display()))?;
        }
        let playback = &app.world().resource::<scene::Playback>().0;
        writeln!(
            manifest,
            "{index},{:.9},{},{},{},{}",
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
        thread::sleep(Duration::from_millis(u64::from(delay_ms)));
        if index % u32::from(fps) == 0 || index + 1 == frames {
            eprintln!(
                "Exported {}/{} frames ({:.1}s elapsed)",
                index + 1,
                frames,
                started.elapsed().as_secs_f32()
            );
        }
    }
    manifest.flush()?;
    if let Some(encoder) = &mut encoder {
        encoder.finish()?;
    }
    eprintln!("Export complete: {}", directory.display());
    Ok(())
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

fn capture(app: &mut App, target: &Handle<Image>, frame: u32) -> anyhow::Result<Image> {
    let (sender, receiver) = mpsc::channel();
    app.world_mut()
        .spawn(Screenshot::image(target.clone()))
        .observe(move |event: On<ScreenshotCaptured>| {
            let _ = sender.send(event.image.clone());
        });
    let started = Instant::now();
    loop {
        update(app)?;
        match receiver.try_recv() {
            Ok(image) => return Ok(image),
            Err(mpsc::TryRecvError::Disconnected) => {
                bail!("capture channel disconnected at frame {frame}")
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if started.elapsed() > WAIT_LIMIT {
            bail!("capture timed out at frame {frame}");
        }
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
    use bevy::prelude::IntoScheduleConfigs;

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
            &model::Options::default(),
            &Theme::neon(),
            &contributors::Mappings::default(),
        );
        let (mut app, target) = scene::headless(
            simulation,
            "Live theme switch / portrait fixture".to_owned(),
            15,
        );
        app.add_systems(
            bevy::prelude::Update,
            (|world: &mut bevy::prelude::World| {
                let index = world.resource::<scene::Frame>().index;
                let wanted = if (45..90).contains(&index) {
                    ThemeName::Phosphor
                } else {
                    ThemeName::Neon
                };
                if world.resource::<scene::Playback>().0.theme().name != wanted {
                    scene::switch_theme(world, wanted);
                }
            })
            .before(scene::animate),
        );
        render(&output, (app, target), 120, 15, 0, None).unwrap();
    }
}
