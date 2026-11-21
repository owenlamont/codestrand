use std::{
    mem::size_of,
    ops::Deref,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use super::{Options, Simulation, Time};
use crate::contributors::Key;
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
    replay_bytes: u128,
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
    latest_ready: Option<Ready>,
    replay: Option<Arc<Mutex<Replay>>>,
}

const REPLAY_LIMIT: u128 = 1024 * 1024 * 1024;

pub(crate) struct Replay {
    windows: Vec<Simulation>,
    used: u128,
    limit: u128,
    packet_bytes: u128,
    exceeded: bool,
}

impl Replay {
    pub(super) fn new() -> Self {
        Self {
            windows: Vec::new(),
            used: 0,
            limit: REPLAY_LIMIT,
            packet_bytes: 0,
            exceeded: false,
        }
    }

    pub(super) fn set_budget(&mut self, limit: u128, packet_bytes: u128) {
        self.limit = self.limit.min(limit);
        self.packet_bytes = packet_bytes;
        self.check_limit(self.used);
    }

    fn check_limit(&mut self, used: u128) -> bool {
        if !self.exceeded && used > self.limit {
            self.windows = Vec::new();
            self.used = 0;
            self.exceeded = true;
        }
        !self.exceeded
    }

    fn retain(&mut self, packet: &Packet) {
        if self.exceeded {
            return;
        }
        let used = self
            .used
            .saturating_add(packet.replay_bytes)
            .saturating_add(packet.simulation.replay_identity_bytes());
        if self.check_limit(used) {
            self.windows.push(packet.simulation.clone());
            self.used = used;
        }
    }

    fn frame(&self, tick: u64) -> Option<Simulation> {
        let index = self.windows.partition_point(|window| {
            let ready = window.ready_end.expect("retained window is planned");
            !ready.complete && ready.end <= tick
        });
        self.windows.get(index).cloned()
    }
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
            let replay_bytes = options.replay.as_ref().map_or(0, |replay| {
                let base = replay
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .packet_bytes;
                base + simulation.replay_plan_bytes()
            });
            connected = sender
                .send(Ok(Packet {
                    generation,
                    simulation,
                    replay_bytes,
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
        let replay = options.replay.clone();
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
            latest_ready: None,
            replay,
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
                self.latest_ready = packet.simulation.ready_end;
                if let Some(replay) = &self.replay {
                    replay
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .retain(&packet);
                }
                return Ok(Some(packet.simulation));
            }
        }
    }
}

impl Simulation {
    fn replay_identity_bytes(&self) -> u128 {
        fn key_bytes(key: &Key) -> u128 {
            match key {
                Key::Email(bytes) | Key::Name(bytes) => bytes.capacity() as u128,
                Key::GitHubLogin(login) => login.capacity() as u128,
                Key::Config(_) | Key::GitHubId(_) => 0,
            }
        }

        // A window packet shares its initial checkpoint with its current state.
        let checkpoint = &self.initial;
        let avatars = checkpoint
            .avatars
            .iter()
            .map(|avatar| {
                let placement = avatar
                    .name_visibility
                    .placement
                    .as_ref()
                    .map_or(0, |placement| placement.text.capacity());
                let slot = avatar.slot.as_ref().map_or(0, |slot| slot.text.capacity());
                avatar.name.capacity() as u128
                    + key_bytes(&avatar.key)
                    + placement as u128
                    + slot as u128
            })
            .sum::<u128>();
        avatars + checkpoint.authors.keys().map(key_bytes).sum::<u128>()
    }

    fn replay_plan_bytes(&self) -> u128 {
        let titles = self.title_plan.capacity() * size_of::<Vec<u8>>()
            + self.title_plan.iter().map(Vec::capacity).sum::<usize>();
        let captions =
            self.caption_plan.capacity() * size_of::<super::caption::Transition>();
        let rays = self
            .envelopes
            .iter()
            .map(super::slots::Envelope::ray_bytes)
            .sum::<usize>();
        (titles + captions + rays) as u128 + self.travel.replay_bytes()
    }

    #[cfg(test)]
    pub(crate) fn force_unrouteable_test(&mut self) {
        self.travel.force_unrouteable();
    }
    pub(crate) fn planning_complete(&self) -> bool {
        self.ready_end.as_ref().is_none_or(|ready| ready.complete)
    }
    /// Whether this packet can draw `frame`. An incomplete packet's last tick waits
    /// for the next one, which may end playback there.
    pub(crate) fn frame_ready(&self, frame: u32, fps: u16) -> bool {
        let tick = super::Schedule::frame_tick(frame, fps);
        self.ready_end.as_ref().is_none_or(|ready| {
            tick >= self.initial.tick && (tick < ready.end || ready.complete)
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
        if !stream.resetting && self.frame_ready(frame, fps) {
            return Ok(true);
        }
        let tick = super::Schedule::frame_tick(frame, fps);
        let retained = stream.replay.as_ref().and_then(|replay| {
            replay
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .frame(tick)
        });
        if let Some(next) = retained {
            self.install_window(next, &shared);
            return Ok(true);
        }
        if tick < self.initial.tick
            || stream
                .latest_ready
                .is_some_and(|ready| tick < ready.end || ready.complete)
        {
            stream.rewind()?;
        }
        while stream.resetting || !self.frame_ready(frame, fps) {
            let Some(next) = stream.receive(wait)? else {
                return Ok(false);
            };
            self.install_window(next, &shared);
        }
        Ok(true)
    }

    fn install_window(&mut self, mut next: Self, stream: &Arc<Mutex<Stream>>) {
        next.set_theme(self.theme.clone());
        next.playback_multiplier = self.playback_multiplier;
        next.caption_style = self.caption_style;
        next.repository_boundary = self.repository_boundary;
        next.repository_label_plane = self.repository_label_plane;
        next.collapse_directories = self.collapse_directories;
        next.diff_text_distance = self.diff_text_distance;
        next.stream = Some(stream.clone());
        *self = next;
    }

    pub(crate) fn replanning_after_replay_limit(&self) -> bool {
        self.stream.as_ref().is_some_and(|stream| {
            let stream = stream
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            stream.generation > 0
                && stream.replay.as_ref().is_some_and(|replay| {
                    replay
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .exceeded
                })
        })
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

    /// The tick whole playback ends on, exact once planning completes. Until then,
    /// the unpadded playback plus the walk padding planned so far, scaled from the
    /// commits planned to every commit.
    pub(crate) fn projected_end_tick(&self) -> u64 {
        let Some(ready) = self.ready_end.filter(|ready| !ready.complete) else {
            return self.end_tick();
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
        unpadded.saturating_add(projected).max(end)
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
        fixture_with_replay(eager, hook, cap, None)
    }

    fn fixture_with_replay(
        eager: bool,
        hook: Option<WindowHook>,
        cap: super::super::TravelCap,
        replay: Option<Arc<Mutex<Replay>>>,
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
                replay,
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
    fn a_window_ending_on_the_last_tick_leaves_that_frame_to_the_last_packet() {
        let mut events = vec![crate::timeline::synthetic().remove(0)];
        crate::repositories::namespace(&mut events, 0);
        let mut sim = Simulation::with_repositories(
            events,
            &Options {
                travel_window_seconds: Some(2.99),
                ..Options::default()
            },
            &Theme::neon(),
            &Mappings::default(),
            &["one".into()],
        );
        sim.prepare_frame(0, 60, true).unwrap();
        let ready = sim.ready_end.unwrap();
        let last = u32::try_from(ready.end).unwrap();
        assert!(!ready.complete);
        assert!(!sim.frame_ready(last, 60));
        sim.prepare_frame(last, 60, true).unwrap();
        assert!(sim.planning_complete());
        assert_eq!(sim.frames(60).unwrap(), last);
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
                let ready = stream.ready_end.unwrap();
                let remaining =
                    ready.unpadded.ticks(true) - stream.clock.rows(ready.end).1;
                let unscaled = ready.end + remaining;
                windows.push((
                    stream.planned_history(),
                    stream.projected_end_tick() > unscaled,
                ));
                assert!(stream.projected_end_tick() >= unscaled, "frame {frame}");
            }
        }
        assert!(windows.len() > 1, "fixture plans several windows");
        assert!(windows.iter().any(|&(_, scaled)| scaled), "{windows:?}");
        assert_eq!(stream.projected_end_tick(), whole.end_tick());
        // The eager f32 end, 12.583334 s, sits just above tick 755.
        assert_eq!(stream.frames(240).unwrap(), whole.frames(240).unwrap());
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

    #[test]
    fn retained_windows_replay_every_frame_without_planning() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let hook: WindowHook = {
            let calls = calls.clone();
            Arc::new(move |_, _| {
                calls.fetch_add(1, Ordering::Relaxed);
            })
        };
        let mut sim = fixture_with_replay(
            false,
            Some(hook),
            super::super::TravelCap::Seconds(0.4),
            Some(Arc::new(Mutex::new(Replay::new()))),
        );
        let frames = fixture(true).frames(60).unwrap();
        let first: Vec<_> = (0..frames)
            .map(|frame| {
                sim.advance_to(frame, 60);
                rendered_frame(&sim)
            })
            .collect();
        assert!(sim.planning_complete());
        let planned = calls.load(Ordering::Relaxed);
        assert!(planned > 1);
        for (frame, expected) in (0..frames).zip(first) {
            assert!(sim.prepare_frame(frame, 60, false).unwrap());
            sim.advance_to(frame, 60);
            assert_eq!(rendered_frame(&sim), expected, "loop frame {frame}");
        }
        assert_eq!(calls.load(Ordering::Relaxed), planned);
    }

    fn rendered_frame(sim: &Simulation) -> String {
        let layout = sim.layout();
        let files: Vec<_> = layout
            .files()
            .map(|(id, file)| sim.file_effect(id, file))
            .collect();
        let repositories: Vec<_> = (0..layout.repositories.len())
            .map(|repository| sim.repository_effect(repository))
            .collect();
        let settings = (
            sim.theme().name,
            sim.caption_style,
            sim.repository_boundary,
            sim.repository_label_plane,
            sim.collapse_directories,
            sim.diff_text_distance,
            sim.playback_multiplier(),
        );
        let placement = (
            sim.seconds(),
            layout.revision(),
            sim.state.region_depth,
            sim.groups(),
            sim.scale(),
            sim.avatar_disc_scale(),
            sim.member_scales(),
            &sim.state.pulses,
            &sim.state.titles,
        );
        let caption = (sim.caption_fields(), sim.caption_background());
        format!(
            "{}|{}|{files:?}|{repositories:?}|{settings:?}|{placement:?}|{caption:?}|{:?}",
            visible(sim),
            layout.exact_state(),
            sim.diff_lines()
        )
    }

    #[test]
    fn replay_comparison_detects_file_layout_and_presentation_changes() {
        let mut sim = fixture(true);
        sim.advance_to(60, 60);
        let expected = rendered_frame(&sim);
        let mut heated = sim.clone();
        let file = heated.layout().files().next().unwrap().0;
        heated.state.layout.heat(file, 1.0);
        assert_ne!(rendered_frame(&heated), expected, "file heat");
        sim.collapse_directories = !sim.collapse_directories;
        assert_ne!(rendered_frame(&sim), expected, "directory presentation");
    }

    #[test]
    fn long_contributor_identities_exhaust_the_replay_budget() {
        let mut sim = fixture(true);
        sim.advance_to(60, 60);
        let avatar = sim.avatars()[0].clone();
        let contributors = 128;
        let string_bytes = 4096;
        let checkpoint = Arc::make_mut(&mut sim.initial);
        checkpoint.avatars.clear();
        checkpoint.authors.clear();
        for author in 0..contributors {
            let mut avatar = avatar.clone();
            avatar.name = format!("{author:0>string_bytes$}");
            avatar.key = Key::Name(avatar.name.as_bytes().to_vec());
            avatar.name_visibility.placement.as_mut().unwrap().text =
                avatar.name.clone();
            avatar.slot.as_mut().unwrap().text = avatar.name.clone();
            checkpoint.authors.insert(avatar.key.clone(), author);
            checkpoint.avatars.push(avatar);
        }
        sim.state = super::super::SharedState(sim.initial.clone());
        let packet = Packet {
            generation: 0,
            simulation: sim,
            replay_bytes: 0,
        };
        let checkpoint_bytes = (contributors * 5 * string_bytes) as u128;
        let mut replay = Replay::new();
        replay.set_budget(checkpoint_bytes, 0);
        replay.retain(&packet);
        assert_eq!(replay.windows.len(), 1);
        assert_eq!(replay.used, checkpoint_bytes);
        let mut next = Packet {
            simulation: packet.simulation.clone(),
            ..packet
        };
        Arc::make_mut(&mut next.simulation.initial).tick += 1;
        next.simulation.state =
            super::super::SharedState(next.simulation.initial.clone());
        replay.retain(&next);
        assert!(replay.exceeded);
        assert!(replay.windows.is_empty());
    }

    #[test]
    fn seeking_to_an_evicted_window_resets_blocking_prepare() {
        seek_after_eviction(true);
    }

    #[test]
    fn seeking_to_an_evicted_window_resets_nonblocking_prepare() {
        seek_after_eviction(false);
    }

    #[test]
    fn seeking_past_retained_windows_resumes_the_worker() {
        let replay = Arc::new(Mutex::new(Replay::new()));
        let mut sim = fixture_with_replay(
            false,
            None,
            super::super::TravelCap::Seconds(0.4),
            Some(replay.clone()),
        );
        let second_start = u32::try_from(sim.ready_end.unwrap().end).unwrap();
        assert!(sim.prepare_frame(second_start, 60, true).unwrap());
        let third_start = u32::try_from(sim.ready_end.unwrap().end).unwrap();
        assert!(sim.prepare_frame(0, 60, false).unwrap());
        assert!(sim.prepare_frame(third_start, 60, true).unwrap());
        assert_eq!(sim.initial.tick, u64::from(third_start));
        let stream = sim.stream.as_ref().unwrap().lock().unwrap();
        assert_eq!(stream.generation, 0);
        assert_eq!(replay.lock().unwrap().windows.len(), 3);
    }

    fn seek_after_eviction(wait: bool) {
        let replay = Arc::new(Mutex::new(Replay::new()));
        let mut sim = fixture_with_replay(
            false,
            None,
            super::super::TravelCap::Seconds(0.4),
            Some(replay.clone()),
        );
        let second_start = u32::try_from(sim.ready_end.unwrap().end).unwrap();
        assert!(sim.prepare_frame(second_start, 60, true).unwrap());
        assert_eq!(sim.initial.tick, u64::from(second_start));
        assert!(sim.prepare_frame(0, 60, false).unwrap());
        assert_eq!(sim.initial.tick, 0);
        replay.lock().unwrap().set_budget(0, 0);

        let (done, result) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                let ready = sim.prepare_frame(second_start, 60, wait).unwrap();
                assert!(sim.initial.tick <= u64::from(second_start));
                if ready {
                    assert_eq!(sim.initial.tick, u64::from(second_start));
                    assert!(sim.replanning_after_replay_limit());
                    done.send(()).unwrap();
                    break;
                }
                assert!(std::time::Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
        });
        result
            .recv_timeout(Duration::from_secs(3))
            .expect("prepare must finish after resetting the worker");
        worker.join().unwrap();
    }

    #[test]
    fn exceeding_the_replay_limit_releases_windows_and_replans() {
        let replay = Arc::new(Mutex::new(Replay::new()));
        let mut sim = fixture_with_replay(
            false,
            None,
            super::super::TravelCap::Seconds(0.4),
            Some(replay.clone()),
        );
        let first = visible(&sim);
        assert_eq!(replay.lock().unwrap().windows.len(), 1);
        replay.lock().unwrap().set_budget(0, 0);
        assert!(replay.lock().unwrap().windows.is_empty());
        assert!(!sim.replanning_after_replay_limit());
        sim.advance_to(fixture(true).frames(60).unwrap() - 1, 60);
        assert!(sim.prepare_frame(0, 60, true).unwrap());
        assert!(sim.replanning_after_replay_limit());
        assert_eq!(visible(&sim), first);
    }
}
