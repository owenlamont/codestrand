use std::{
    ops::Deref,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use super::{Options, Simulation, Time};
use crate::timeline::Event;

// Consumer, worker, and the completed handoff while the consumer catches up.
pub(super) const PLAN_COPIES: u128 = 3;
// Four checkpoint owners plus mutable replay, seed, and handoff workspaces.
pub(super) const STATE_COPIES: u128 = 10;
pub(super) const GLOBAL_STATE_COPIES: u128 = 3;

#[cfg(test)]
pub(crate) type WindowHook = Arc<dyn Fn(usize, &mut Simulation) + Send + Sync>;

#[derive(Clone)]
pub(super) struct Events {
    rows: Arc<[Event]>,
    start: usize,
    end: usize,
}
impl From<Vec<Event>> for Events {
    fn from(rows: Vec<Event>) -> Self {
        Self {
            start: 0,
            end: rows.len(),
            rows: rows.into(),
        }
    }
}
impl Deref for Events {
    type Target = [Event];
    fn deref(&self) -> &[Event] {
        &self.rows[self.start..self.end]
    }
}
impl Events {
    pub(super) fn prefix(&self, end: usize) -> Self {
        Self {
            rows: self.rows.clone(),
            start: self.start,
            end: self.start + end,
        }
    }

    pub(super) fn range(&self, range: std::ops::Range<usize>) -> Self {
        Self {
            rows: self.rows.clone(),
            start: self.start + range.start,
            end: self.start + range.end,
        }
    }
}
impl<'a> IntoIterator for &'a Events {
    type Item = &'a Event;
    type IntoIter = std::slice::Iter<'a, Event>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Planning(#[from] anyhow::Error),
    #[error("window planning worker panicked: {0}")]
    Panic(String),
    #[error("window planning worker disconnected")]
    Disconnected,
}
struct Packet {
    generation: u64,
    simulation: Simulation,
}
/// How far a planned window lets playback run.
#[derive(Clone, Copy, Debug)]
pub(super) struct Ready {
    /// The last tick planned.
    pub(super) end: u64,
    /// Whether the window reaches the end of playback.
    pub(super) complete: bool,
    /// Playback length without walk waits.
    pub(super) unpadded: Time,
}
pub(super) struct Stream {
    receiver: mpsc::Receiver<Result<Packet, Error>>,
    reset: mpsc::SyncSender<u64>,
    generation: u64,
    resetting: bool,
}

fn work(
    seed: &Simulation,
    all: &[Event],
    options: &Options,
    labels: &[String],
    seconds: f32,
    sender: &mpsc::SyncSender<Result<Packet, Error>>,
    resets: &mpsc::Receiver<u64>,
) -> anyhow::Result<()> {
    let mut generation = 0;
    loop {
        let mut next = None;
        let mut connected = true;
        let mut publish = |simulation: Simulation| -> anyhow::Result<bool> {
            if let Ok(request) = resets.try_recv() {
                next = Some(request);
                return Ok(false);
            }
            connected = sender
                .send(Ok(Packet {
                    generation,
                    simulation,
                }))
                .is_ok();
            Ok(connected)
        };
        seed.clone().run_windows(
            all,
            options,
            labels,
            seconds,
            Vec::new(),
            Some(&mut publish),
        )?;
        if !connected {
            return Ok(());
        }
        generation = match next.or_else(|| resets.recv().ok()) {
            Some(generation) => generation,
            None => return Ok(()),
        };
    }
}

impl Stream {
    pub(super) fn start(
        mut seed: Simulation,
        all: Events,
        options: Options,
        labels: Vec<String>,
        seconds: f32,
    ) -> anyhow::Result<Simulation> {
        seed.events = Events {
            rows: all.rows.clone(),
            start: seed.offset,
            end: seed.offset + seed.events.len(),
        };
        // The worker plans one window ahead, then hands it directly to the consumer.
        let (sender, receiver) = mpsc::sync_channel(0);
        let (reset, resets) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("travel-planner".into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        work(&seed, &all, &options, &labels, seconds, &sender, &resets)
                    }));
                let failure = match result {
                    Ok(Ok(())) => return,
                    Ok(Err(error)) => Error::Planning(error),
                    Err(payload) => Error::Panic(
                        payload
                            .downcast_ref::<&str>()
                            .copied()
                            .or_else(|| {
                                payload.downcast_ref::<String>().map(String::as_str)
                            })
                            .unwrap_or("unknown panic")
                            .to_owned(),
                    ),
                };
                let _ = sender.send(Err(failure));
            })?;
        let mut stream = Self {
            receiver,
            reset,
            generation: 0,
            resetting: false,
        };
        let mut first = stream
            .receive(true)?
            .expect("blocking receive returns a window");
        first.stream = Some(Arc::new(Mutex::new(stream)));
        Ok(first)
    }
    fn rewind(&mut self) -> Result<(), Error> {
        if !self.resetting {
            self.generation += 1;
            self.reset
                .try_send(self.generation)
                .map_err(|_| Error::Disconnected)?;
            self.resetting = true;
        }
        Ok(())
    }
    fn receive(&mut self, wait: bool) -> Result<Option<Simulation>, Error> {
        loop {
            let packet = if wait {
                self.receiver.recv().map_err(|_| Error::Disconnected)?
            } else {
                match self.receiver.try_recv() {
                    Ok(packet) => packet,
                    Err(mpsc::TryRecvError::Empty) => return Ok(None),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(Error::Disconnected);
                    }
                }
            }?;
            if packet.generation == self.generation {
                self.resetting = false;
                return Ok(Some(packet.simulation));
            }
        }
    }
}

impl Simulation {
    #[cfg(test)]
    pub(crate) fn force_unrouteable_test(&mut self) {
        self.travel.force_unrouteable();
    }
    pub(crate) fn planning_complete(&self) -> bool {
        self.ready_end.as_ref().is_none_or(|ready| ready.complete)
    }
    pub(crate) fn frame_ready(&self, frame: u32, fps: u16) -> bool {
        let tick = super::Schedule::frame_tick(frame, fps);
        self.ready_end.as_ref().is_none_or(|ready| {
            tick >= self.initial.tick && (tick <= ready.end || ready.complete)
        })
    }
    pub(crate) fn prepare_frame(
        &mut self,
        frame: u32,
        fps: u16,
        wait: bool,
    ) -> anyhow::Result<bool> {
        let Some(shared) = self.stream.clone() else {
            return Ok(true);
        };
        let mut stream = shared.lock().map_err(|_| Error::Disconnected)?;
        if super::Schedule::frame_tick(frame, fps) < self.initial.tick {
            stream.rewind()?;
        }
        while stream.resetting || !self.frame_ready(frame, fps) {
            let Some(mut next) = stream.receive(wait)? else {
                return Ok(false);
            };
            next.set_theme(self.theme.clone());
            next.playback_multiplier = self.playback_multiplier;
            next.caption_style = self.caption_style;
            next.repository_boundary = self.repository_boundary;
            next.repository_label_plane = self.repository_label_plane;
            next.collapse_directories = self.collapse_directories;
            next.diff_text_distance = self.diff_text_distance;
            next.stream = Some(shared.clone());
            *self = next;
        }
        Ok(true)
    }
    pub(crate) fn history_progress(&self) -> (Time, Time) {
        (
            super::tick_time(self.clock.rows(self.state.tick).1),
            self.clock.history.duration(),
        )
    }

    /// History time planned so far; playback past it waits for the next window.
    pub(crate) fn planned_history(&self) -> Time {
        self.ready_end
            .as_ref()
            .map_or(self.clock.history.duration(), |ready| {
                super::tick_time(self.clock.rows(ready.end).1)
            })
    }

    /// Whole playback length, exact once planning completes. Until then, the
    /// unpadded playback plus the walk padding planned so far, scaled from the
    /// commits planned to every commit.
    pub(crate) fn projected_duration(&self) -> Time {
        let Some(ready) = self.ready_end.filter(|ready| !ready.complete) else {
            return self.duration();
        };
        let end = ready.end;
        let padding = end.saturating_sub(self.clock.rows(end).1);
        let planned = self.schedule.applied(end) as u64;
        let commits = self.clock.history.starts.len() as u64;
        let projected = padding
            .saturating_mul(commits)
            .checked_div(planned)
            .unwrap_or(padding);
        let unpadded = ready.unpadded.ticks(true);
        super::tick_time(unpadded.saturating_add(projected).max(end))
    }
}

#[cfg(test)]
impl std::ops::DerefMut for Events {
    fn deref_mut(&mut self) -> &mut [Event] {
        &mut Arc::make_mut(&mut self.rows)[self.start..self.end]
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{contributors::Mappings, theme::Theme};
    use std::time::Duration;

    fn diagnostic_fixture() -> Simulation {
        use clap::Parser as _;
        let capped =
            std::env::var("CODESTRAND_AUDIT_CAP").unwrap_or_default() != "none";
        let window = std::env::var("CODESTRAND_AUDIT_WINDOW")
            .ok()
            .filter(|s| s != "whole")
            .map(|s| s.parse().unwrap());
        if std::env::var("CODESTRAND_AUDIT_FIXTURE").unwrap() == "full30" {
            let histories = (0..30)
                .map(|repository| {
                    let mut events = crate::timeline::synthetic();
                    for event in &mut events {
                        event.seconds +=
                            i64::try_from(repository).unwrap() * 86_400 * 3;
                    }
                    crate::repositories::namespace(&mut events, repository);
                    events
                })
                .collect();
            let events = crate::repositories::merge(histories);
            let mut options = Options {
                travel_window_seconds: window,
                seconds_per_day: 0.02,
                max_idle_seconds: 0.05,
                travel_cap: if capped {
                    super::super::TravelCap::Seconds(0.4)
                } else {
                    super::super::TravelCap::None
                },
                bot_travel: super::super::BotTravel::Walk,
                ..Options::default()
            };
            if std::env::var("CODESTRAND_AUDIT_WINDOW").is_err() {
                options.choose_window(&events);
            }
            return Simulation::with_repositories(
                events,
                &options,
                &Theme::neon(),
                &Mappings::default(),
                &(0..30)
                    .map(|i| format!("Repository {i}"))
                    .collect::<Vec<_>>(),
            );
        }
        if std::env::var("CODESTRAND_AUDIT_FIXTURE").unwrap() == "short" {
            let (events, labels) = super::super::tests::short_repository_history(30);
            let mut options = Options {
                travel_window_seconds: window,
                seconds_per_day: 0.2,
                max_idle_seconds: 0.2,
                travel_cap: if capped {
                    super::super::TravelCap::Seconds(0.4)
                } else {
                    super::super::TravelCap::None
                },
                bot_travel: super::super::BotTravel::Walk,
                ..Options::default()
            };
            if std::env::var("CODESTRAND_AUDIT_WINDOW").is_err() {
                options.choose_window(&events);
            }
            return Simulation::with_repositories(
                events,
                &options,
                &Theme::neon(),
                &Mappings::default(),
                &labels,
            );
        }
        let args: Vec<String> =
            serde_json::from_str(&std::env::var("CODESTRAND_AUDIT_ARGS").unwrap())
                .unwrap();
        let cli = crate::Cli::try_parse_from(args).unwrap();
        let (mut source, theme) = match cli.command {
            Some(crate::Command::LayoutQuality { source, .. }) => {
                (source, Theme::neon())
            }
            Some(crate::Command::Export { source, theme, .. }) => {
                (source, theme.build())
            }
            _ => panic!("diagnostic source"),
        };
        if std::env::var("CODESTRAND_AUDIT_WINDOW").is_ok() {
            source.window_override =
                Some(window.map_or(
                    crate::WindowOverride::Whole,
                    crate::WindowOverride::Seconds,
                ));
        }
        let mut simulation = source
            .load(&crate::progress::Progress::default())
            .unwrap()
            .0;
        simulation.set_theme(theme);
        simulation
    }

    #[test]
    #[ignore = "requires an explicit history fixture; audits every streamed tick"]
    fn streaming_trajectory_fixture() {
        use std::hash::{Hash, Hasher};
        let mut sim = diagnostic_fixture();
        let mut diagnostic = super::super::slots::diagnostics::MovingBeams::default();
        let mut hash = std::hash::DefaultHasher::new();
        let mut boundaries = std::collections::BTreeSet::new();
        let mut step = BoundarySteps::default();
        let mut frame = 0;
        let mut maximum_faded = 0;
        let uncapped = std::env::var("CODESTRAND_AUDIT_CAP").as_deref() == Ok("none");
        loop {
            sim.prepare_frame(frame, 60, true).unwrap();
            maximum_faded = maximum_faded.max(sim.travel.stats.faded);
            if uncapped {
                assert_eq!(sim.travel.stats.faded, 0, "uncapped travel must not fade");
            }
            if let Some(ready) = &sim.ready_end {
                boundaries.insert(ready.end);
            }
            sim.advance_to(frame, 60);
            diagnostic.observe(&sim);
            visible(&sim).hash(&mut hash);
            step.observe(
                &sim,
                boundaries.contains(&u64::from(frame))
                    || boundaries.contains(&u64::from(frame).saturating_sub(1)),
            );
            if sim.planning_complete()
                && frame >= super::super::frames_for(sim.duration(), 60).unwrap()
            {
                break;
            }
            frame += 1;
        }
        let report = serde_json::json!({"frames":frame+1, "trajectory_hash":hash.finish(), "moving_beams":diagnostic, "steps":step, "boundaries":boundaries.len(), "maximum_faded":maximum_faded, "slots":super::super::slots::report(&sim.state)});
        std::fs::write(
            std::env::var_os("CODESTRAND_AUDIT_OUTPUT").unwrap(),
            report.to_string(),
        )
        .unwrap();
        diagnostic.assert_clear(sim.state.avatar_max_speed);
    }

    #[derive(Default, serde::Serialize)]
    struct BoundarySteps {
        boundary: [f32; 4],
        elsewhere: [f32; 4],
        #[serde(skip)]
        avatars: Vec<(bevy::math::Vec3, Option<bevy::math::Vec2>)>,
        #[serde(skip)]
        titles: std::collections::BTreeMap<usize, bevy::math::Vec2>,
        #[serde(skip)]
        caption: Option<bevy::color::LinearRgba>,
    }
    impl BoundarySteps {
        fn observe(&mut self, sim: &Simulation, boundary: bool) {
            let mut steps = [0.0_f32; 4];
            for (avatar, (position, dock)) in
                sim.state.avatars.iter().zip(&self.avatars)
            {
                steps[0] = steps[0].max(avatar.position.distance(*position));
                if let (Some(slot), Some(old)) = (&avatar.slot, dock) {
                    steps[1] = steps[1].max(slot.point.distance(*old));
                }
            }
            for (id, title) in &sim.state.titles {
                if let Some(old) = self.titles.get(id) {
                    steps[2] = steps[2].max(title.point.distance(*old));
                }
            }
            let color = sim.caption_background().to_linear();
            if let Some(old) = self.caption {
                steps[3] = (color.red - old.red)
                    .abs()
                    .max((color.green - old.green).abs())
                    .max((color.blue - old.blue).abs());
            }
            let maxima = if boundary {
                &mut self.boundary
            } else {
                &mut self.elsewhere
            };
            for (max, value) in maxima.iter_mut().zip(steps) {
                *max = max.max(value);
            }
            self.avatars = sim
                .state
                .avatars
                .iter()
                .map(|avatar| {
                    (avatar.position, avatar.slot.as_ref().map(|slot| slot.point))
                })
                .collect();
            self.titles = sim
                .state
                .titles
                .iter()
                .map(|(&id, title)| (id, title.point))
                .collect();
            self.caption = Some(color);
        }
    }

    fn fixture(eager: bool) -> Simulation {
        fixture_with(eager, None, super::super::TravelCap::Seconds(0.4))
    }
    pub(crate) fn fixture_with(
        eager: bool,
        hook: Option<WindowHook>,
        cap: super::super::TravelCap,
    ) -> Simulation {
        let events = (0..8)
            .map(|index| {
                let mut row = vec![crate::timeline::synthetic().remove(0)];
                row[0].seconds = i64::try_from(index).unwrap() * 86_400;
                crate::repositories::namespace(&mut row, index % 2);
                row.remove(0)
            })
            .collect();
        Simulation::with_repositories(
            events,
            &Options {
                seconds_per_day: 1.0,
                max_idle_seconds: 1.0,
                travel_window_seconds: Some(2.0),
                eager_windows: eager,
                window_hook: hook,
                travel_cap: cap,
                bot_travel: super::super::BotTravel::Walk,
                ..Options::default()
            },
            &Theme::neon(),
            &Mappings::default(),
            &["one".into(), "two".into()],
        )
    }

    pub(crate) fn visible(sim: &Simulation) -> String {
        format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            sim.avatars(),
            sim.beams(),
            sim.fragments(),
            sim.labels(),
            sim.caption(),
            sim.view()
        )
    }
    #[test]
    fn worker_failure_is_delivered_after_a_consumed_window() {
        for panic in [false, true] {
            let hook: WindowHook = Arc::new(move |index, sim| {
                if index == 1 {
                    assert!(!panic, "forced worker panic");
                    sim.travel.force_unrouteable();
                }
            });
            let mut sim =
                fixture_with(false, Some(hook), super::super::TravelCap::None);
            sim.advance_to(30, 60);
            let end = u32::try_from(sim.ready_end.as_ref().unwrap().end + 1).unwrap();
            let error = sim.prepare_frame(end, 60, true).unwrap_err().to_string();
            assert!(
                error.contains(if panic {
                    "forced worker panic"
                } else {
                    "no route for commit"
                }),
                "{error}"
            );
        }
    }

    #[test]
    fn projected_length_scales_planned_waits_and_is_exact_once_planned() {
        let whole = fixture_with(true, None, super::super::TravelCap::None);
        let mut stream = fixture_with(false, None, super::super::TravelCap::None);
        let mut windows = Vec::new();
        for frame in 0.. {
            stream.prepare_frame(frame, 60, true).unwrap();
            if stream.planning_complete() {
                break;
            }
            if windows.last().map(|&(planned, _)| planned)
                != Some(stream.planned_history())
            {
                let unpadded = stream.ready_end.unwrap().unpadded;
                let remaining = unpadded.since(stream.planned_history());
                let unscaled = stream.duration().after(remaining);
                windows.push((
                    stream.planned_history(),
                    stream.projected_duration() > unscaled,
                ));
                assert!(stream.projected_duration() >= unscaled, "frame {frame}");
            }
        }
        assert!(windows.len() > 1, "fixture plans several windows");
        assert!(windows.iter().any(|&(_, scaled)| scaled), "{windows:?}");
        assert_eq!(stream.projected_duration(), whole.duration());
        let unpadded = stream.ready_end.unwrap().unpadded;
        assert!(whole.duration() > unpadded, "walk waits pad the fixture");
    }

    #[test]
    fn worker_stall_does_not_change_the_trajectory() {
        let hook: WindowHook = Arc::new(|index, _| {
            if index > 0 {
                thread::sleep(Duration::from_millis(10));
            }
        });
        let mut ordinary = fixture(false);
        let mut stalled =
            fixture_with(false, Some(hook), super::super::TravelCap::Seconds(0.4));
        for frame in 0..=700 {
            ordinary.prepare_frame(frame, 60, true).unwrap();
            stalled.prepare_frame(frame, 60, true).unwrap();
            ordinary.advance_to(frame, 60);
            stalled.advance_to(frame, 60);
            assert_eq!(visible(&ordinary), visible(&stalled), "frame {frame}");
        }
    }
    #[test]
    fn streaming_replays_each_tick_and_replans_a_dropped_loop() {
        let mut whole = fixture(true);
        let mut stream = fixture(false);
        let frames = super::super::frames_for(whole.duration(), 60).unwrap();
        let mut states = Vec::new();
        let mut diagnostic = super::super::slots::diagnostics::MovingBeams::default();
        for frame in 0..=frames {
            whole.advance_to(frame, 60);
            stream.prepare_frame(frame, 60, true).unwrap();
            stream.advance_to(frame, 60);
            assert_eq!(visible(&whole), visible(&stream), "frame {frame}");
            diagnostic.observe(&stream);
            if frame % 30 == 0 {
                states.push((frame, visible(&stream)));
            }
            assert!(stream.boundaries.is_empty());
        }
        assert!(stream.planning_complete());
        diagnostic.assert_clear(480.0);
        for (frame, expected) in states {
            stream.prepare_frame(frame, 60, true).unwrap();
            stream.advance_to(frame, 60);
            assert_eq!(visible(&stream), expected, "loop frame {frame}");
        }
    }
}
