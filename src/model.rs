use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    ops::Range,
    sync::Arc,
    time::Duration,
};

use bevy::math::{Vec2, Vec3};

use crate::{
    contributors::{Contributor, Key, Mappings},
    layout::{FileMotion, Layout, StepPart},
    progress::{Progress, Stage},
    theme::{BeamStyle, Geometry, Theme},
    timeline::{Event, Kind},
};

pub(crate) mod caption;
pub(crate) mod framing;
mod lifecycle;
pub(crate) use lifecycle::RepositoryExit;
#[cfg(test)]
pub(crate) use lifecycle::exits_from_starts;
pub(crate) mod checkpoint_memory;
pub(crate) use schedule::BIRTH_TICKS;
#[cfg(test)]
mod measurement;
mod names;
#[cfg(test)]
mod nitpicks;
pub(crate) mod occlusion;
#[cfg(test)]
mod pacing_tests;
mod planning_memory;
#[cfg(test)]
mod ray_tests;
#[cfg(test)]
mod release_regressions;

pub(crate) mod quality;
pub(crate) mod schedule;
mod slots;
#[cfg(test)]
pub(crate) use slots::diagnostics::MovingBeams;
pub(crate) mod streaming;
mod time;
mod trace;
mod travel;
#[cfg(test)]
pub(crate) use travel::assert_gutter_routes;
mod windows;
pub(super) use names::rect as label_rect;
#[cfg(not(test))]
pub(super) use planning_memory::available as available_memory;
use schedule::Schedule;
pub(crate) use time::Time;
pub(crate) use trace::Trace;
use trace::{Phase, Scope};
pub(crate) mod treemap;

pub(super) const TICK_RATE: u32 = 60;
pub(super) const PREVIEW_FPS: u16 = 30;
const LEAD_IN: f32 = 0.5;
const TAIL: f32 = 2.5;
// Sixty-second windows beat fifteen on whole mid30 (459 vs 514 s) and tie thirty within
// 2% over five paired runs, though thirty reaches the first frame about 2.5 s sooner.
const WINDOW_SECONDS: f32 = 60.0;
const PULSE_SECONDS: f32 = 1.3;
/// Beam crossings per second at the default `--fragment-speed` of 1.
const FRAGMENT_SPEED: f32 = 1.1;
/// Accepted `--fragment-speed` values. The slowest pulse still ends before its avatar
/// goes idle and gives up its slot.
pub(crate) const FRAGMENT_SPEEDS: std::ops::RangeInclusive<f32> = 0.25..=2.0;
/// Live preview speed multipliers, both bounds inclusive.
pub(crate) const PLAYBACK_MULTIPLIERS: std::ops::RangeInclusive<f32> = 0.125..=8.0;
const ADD_DELAY: f32 = 0.2;
const ARRIVAL_SECONDS: f32 = ADD_DELAY + crate::layout::FADE_SECONDS;
const DELETE_DELAY: f32 = 0.0;
const MIN_AVATAR_HEIGHT: f32 = 3.2;

const AVATAR_IDLE: f32 = 5.0;
const ARRIVAL_SCALE: f32 = 1.15;
/// Seconds a disbanding group's ring, and the co-authors leaving with it, take to fade.
const DISBAND: f32 = 0.4;
/// Seconds an avatar takes to walk a short step to a new seat.
const SETTLE_SECONDS: f32 = 0.4;
/// Reference pixels a seat may move in one tick before it counts as a new seat: seats
/// drift by far less as avatars rescale, and a reseat moves by about a member's width.
const SEAT_DRIFT: f32 = 1.0;
const BEAM_LIMIT: usize = 24;
const ARRIVAL_FLIGHT: f32 = 0.8;
const ARRIVAL_SPACING: f32 = 0.025;
const MAX_ARRIVAL_OFFSET: f32 = 0.25;
const FRAGMENT_TARGETS: usize = 24;
const SETTLE_TICKS: u32 = 1200;
/// Ticks of the global replay whose region samples are held before they become inputs.
const REGION_BATCH: usize = 512;
const DIR_LABELS: usize = 20;

fn check_travel_pass(pass: u32) -> anyhow::Result<()> {
    anyhow::ensure!(
        pass <= 32,
        "avatar travel planning did not converge within 32 passes; shorten the history or reduce the playback times"
    );
    Ok(())
}

/// How fast diff fragments cross their beams, which sets how long a pulse lasts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct FragmentPace {
    /// Beam crossings per second.
    speed: f32,
}

impl FragmentPace {
    /// The pace at `scale` times the default speed.
    pub(super) fn new(scale: f32) -> Self {
        Self {
            speed: FRAGMENT_SPEED * scale,
        }
    }

    /// Seconds one fragment takes to cross its beam.
    fn flight(self) -> f32 {
        1.0 / self.speed
    }

    /// When a pulse starting at `start` ends unless its author's next commit cuts it
    /// short.
    fn pulse_end(self, start: Time) -> Time {
        start + PULSE_SECONDS + self.flight()
    }
}

#[derive(Clone)]
pub(super) struct Options {
    pub available_memory: u64,
    pub contributor_inactivity_days: u32,
    pub repository_ends: Vec<Option<i64>>,
    pub repository_archived: Vec<bool>,
    pub heat_half_life: f32,
    pub seconds_per_day: f32,
    pub max_idle_seconds: f32,
    pub start: usize,
    pub commits: usize,
    pub unread: Unread,
    pub travel_cap: TravelCap,
    pub travel_window_seconds: Option<f32>,
    pub replay: Option<Arc<std::sync::Mutex<streaming::Replay>>>,
    #[cfg(test)]
    pub eager_windows: bool,
    #[cfg(test)]
    pub advance_all_files: bool,
    #[cfg(test)]
    pub window_hook: Option<streaming::WindowHook>,
    /// Runs every replay even where its result is already held; tests compare against it.
    pub replay_everything: bool,
    /// Threads that turn sampled ticks into regions; zero takes the machine's parallelism.
    pub region_threads: usize,
    pub bot_travel: BotTravel,
    pub avatar_max_speed: f32,
    pub avatar_size: f32,
    pub gutter: f32,
    /// Diff fragment speed relative to the default.
    pub fragment_speed: f32,
    pub progress: Progress,
    pub trace: Trace,
}

/// Commits a header scan counted that import never read, so commit numbers and
/// repository endings describe the whole history rather than the events.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Unread {
    /// Commits before the selection beyond the boundary events that settle them.
    pub before: usize,
    /// Commits after the selection, per repository.
    pub after: Vec<usize>,
}

impl Unread {
    fn after(&self, repository: usize) -> usize {
        self.after.get(repository).copied().unwrap_or(0)
    }

    fn total(&self) -> usize {
        self.before + self.after.iter().sum::<usize>()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub(super) enum TravelCap {
    Seconds(f32),
    None,
}

impl TravelCap {
    fn ticks(self) -> u32 {
        match self {
            Self::Seconds(seconds) => travel::seconds_ticks(seconds, false),
            Self::None => 216_000,
        }
    }

    fn blocked_ticks(self) -> u32 {
        match self {
            Self::None => 60,
            Self::Seconds(_) => self.ticks(),
        }
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum, serde::Serialize,
)]
pub(super) enum BotTravel {
    Walk,
    #[default]
    Fade,
}

impl Options {
    pub(super) fn retain_preview_windows(&mut self) {
        self.replay = Some(Arc::new(std::sync::Mutex::new(streaming::Replay::new())));
    }

    pub(super) fn choose_window(&mut self, events: &[Event]) {
        let history = Schedule::for_replay(events, self);
        let (_, endings) =
            lifecycle::plan(events, &history, self, self.selected_range(events.len()));
        let duration = lifecycle::duration(&history, &endings, self.fragment_pace());
        self.travel_window_seconds = (duration
            > Time::seconds(WINDOW_SECONDS + windows::MARGIN))
        .then_some(WINDOW_SECONDS);
    }

    fn fragment_pace(&self) -> FragmentPace {
        FragmentPace::new(self.fragment_speed)
    }

    fn selected_range(&self, events: usize) -> Range<usize> {
        let start = self.start.min(events);
        start..start.saturating_add(self.commits).min(events)
    }
}

#[cfg(test)]
impl Default for Options {
    fn default() -> Self {
        Self {
            available_memory: 2 * 1024 * 1024 * 1024,
            contributor_inactivity_days: 60,
            repository_ends: Vec::new(),
            repository_archived: Vec::new(),
            heat_half_life: 3.0,
            seconds_per_day: 0.5,
            max_idle_seconds: 1.0,
            start: 0,
            commits: usize::MAX,
            unread: Unread::default(),
            travel_cap: TravelCap::Seconds(0.4),
            travel_window_seconds: None,
            replay: None,
            #[cfg(test)]
            eager_windows: false,
            #[cfg(test)]
            advance_all_files: false,
            #[cfg(test)]
            window_hook: None,
            replay_everything: false,
            region_threads: 0,
            bot_travel: BotTravel::Fade,
            avatar_max_speed: 480.0,
            avatar_size: 2.0,
            gutter: 4.0,
            fragment_speed: 1.0,
            progress: Progress::default(),
            trace: Trace::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Target {
    File(u64),
    Dir(usize),
}

#[derive(Clone, Debug)]
struct Pulse {
    commit: usize,
    repository: usize,
    touched: Vec<u64>,
    start: Time,
    end: Time,
    author: usize,
    group: Option<usize>,
    source: Option<RaySource>,
    arrival_revisions: std::ops::RangeInclusive<u64>,
    arrivals_land: Option<Time>,
    targets: Vec<(Target, f32, u32, u32)>,
    aggregated: bool,
    samples: Vec<(u64, crate::diff_samples::Sample)>,
}

impl Pulse {
    fn awaits_arrival_source(&self, now: Time) -> bool {
        self.source.is_none() && self.arrivals_land.is_some_and(|land| now <= land)
    }

    fn fragments(
        &self,
        age: f32,
        envelope: f32,
        beam: (BeamStyle, Vec3, Vec3),
        target: Target,
        change: (u32, bool),
        pace: FragmentPace,
    ) -> impl Iterator<Item = Fragment> {
        let (lines, rising) = change;
        diff_fragments(age, envelope, beam, (lines, rising), pace)
            .enumerate()
            .filter(move |(piece, _)| self.fragment_arrives(age, *piece, lines, pace))
            .map(move |(_, fragment)| fragment.with_beam(self.author, target))
    }
    fn fragment_arrives(
        &self,
        age: f32,
        piece: usize,
        lines: u32,
        pace: FragmentPace,
    ) -> bool {
        let pieces = lines.bit_width().min(3);
        let offset = count_u32(u32::try_from(piece).expect("three fragments"))
            / count_u32(pieces);
        let launch = ((age * pace.speed + offset).floor() - offset) / pace.speed;
        launch + pace.flight() <= self.end - self.start - 0.3 + 0.00001
    }
    fn samples_for(
        &self,
        target: Target,
        rising: bool,
    ) -> impl Iterator<Item = &crate::diff_samples::Sample> {
        self.samples
            .iter()
            .filter(move |(id, sample)| {
                target == Target::File(*id) && sample.added != rising
            })
            .map(|(_, sample)| sample)
    }
}

fn attach_samples<'a>(
    fragments: impl Iterator<Item = Fragment>,
    mut samples: impl Iterator<Item = &'a crate::diff_samples::Sample>,
    remaining: &mut usize,
    detail: f32,
) -> Vec<Fragment> {
    fragments
        .map(|mut fragment| {
            if *remaining > 0
                && let Some(sample) = samples.next()
            {
                fragment.sample = Some(sample.clone());
                fragment.sample_detail = detail;
                *remaining -= 1;
            }
            fragment
        })
        .collect()
}

#[cfg_attr(test, derive(Debug))]
pub(super) struct DiffLine {
    pub fragment: usize,
    pub detail: f32,
    pub position: Vec3,
    pub scale: f32,
    pub sample: crate::diff_samples::Sample,
    pub alpha: f32,
}

#[derive(Clone, Copy, Debug)]
enum RaySource {
    Avatar,
    Ring(usize),
}

#[derive(Clone, Debug)]
struct ArrivalsOnRing {
    revisions: std::ops::RangeInclusive<u64>,
    land: Time,
    lead: usize,
}

#[derive(Clone, Copy, Debug)]
struct CastingRing {
    repository: usize,
    dock: slots::Dock,
    offset: Vec3,
    radius: f32,
}

#[derive(Clone, Debug)]
pub(super) struct Avatar {
    expires: Time,
    arrived: Time,
    pub lifecycle_scale: f32,
    pub grey: f32,
    pub key: Key,
    pub name: String,
    idle_after: f32,
    idle_opacity: f32,
    pub portrait: Option<std::sync::Arc<crate::avatars::Portrait>>,
    pub position: Vec3,
    target: Vec3,
    repository: usize,
    last_transfer_revision: u64,
    pending_arrivals: Vec<(std::ops::RangeInclusive<u64>, Time)>,
    arrivals_land: Time,
    body_arrivals_land: Time,
    touched: Vec<u64>,
    last_active: Time,
    pub presence: f32,
    // Travel fades do not release a reserved parking slot.
    slot_presence: f32,
    pub projection_scale: f32,
    activity: f32,
    name_visibility: names::Visibility,
    commit: usize,
    slot: Option<slots::Placement>,
    dock_since: u64,
    dock_walk: Option<travel::DockWalk>,
    gutter_entry: Option<Vec2>,
    slot_origin: Option<(usize, slots::Dock, Vec2)>,
    group: Option<Group>,
    /// Left behind by a disbanding group with no later commit, so fading with its ring.
    dismissed: bool,
    /// How visible this author's group ring is, fading in and out over [`DISBAND`].
    ring_presence: f32,
    /// Seats the ring held when last shown, so a disbanding ring keeps its size.
    ring_capacity: u8,
    ring_arrivals: Vec<ArrivalsOnRing>,
    casting_ring: Option<CastingRing>,
    /// A short walk from a position to the avatar's place, and when it began.
    settle: Option<(Vec3, Time)>,
    /// How far through its settle the avatar is, from 0 to 1.
    settled: f32,
    /// Where the name stood when a walk to a seat began; it fades out there over the
    /// walk's first half while the name at the seat fades in over the second.
    leaving_name: Option<Vec2>,
    /// The seat offset last parked at, whose change starts a settle.
    seat: Vec2,
    travelling: bool,
    transmitting: bool,
}

impl Avatar {
    fn record_arrivals(
        &mut self,
        start: Time,
        pending: Option<std::ops::RangeInclusive<u64>>,
    ) -> Time {
        if let Some(revisions) = pending {
            self.pending_arrivals.push((revisions, start));
        }
        let land = start + ARRIVAL_SECONDS + tick_seconds();
        self.arrivals_land = self.arrivals_land.max(land);
        land
    }

    fn beside_gutter(
        &self,
        gutter: Option<crate::layout::packing::Tile>,
        radius: f32,
    ) -> bool {
        self.presence > 0.0
            && gutter.is_some_and(|tile| {
                travel::field_rect(tile)
                    .inflate(radius)
                    .contains(slots::field_point(self.position))
            })
    }

    fn hold_for_rays(&mut self) {
        self.dock_walk = None;
        self.gutter_entry = None;
        self.slot_origin = None;
        self.settle = None;
        self.settled = 1.0;
        self.travelling = false;
    }

    fn ring_shift(&self) -> Vec3 {
        self.slot
            .as_ref()
            .filter(|_| self.transmitting)
            .map_or(Vec3::ZERO, |slot| self.position - slot.world())
    }

    fn seat_target(&self, approaching: impl FnOnce() -> bool) -> (Vec3, Vec2) {
        self.slot
            .as_ref()
            .map_or((self.position, Vec2::ZERO), |slot| {
                if slot.offset != Vec2::ZERO && approaching() {
                    (slots::world(slot.centre()), Vec2::ZERO)
                } else {
                    (slot.world(), slot.offset)
                }
            })
    }

    fn travel_point(&mut self, planned: Option<Vec3>, tick: u64) -> Option<Vec3> {
        let local = self
            .dock_walk
            .as_ref()
            .and_then(|walk| walk.sample(tick))
            .map(slots::world);
        if planned.is_some() || local.is_none() {
            self.dock_walk = None;
        }
        planned.or(local)
    }

    /// Moves to `target`, or along a travel route; a short step to a new seat or off the
    /// end of a route is walked over [`SETTLE_SECONDS`] rather than jumped.
    fn walk(
        &mut self,
        target: Vec3,
        offset: Vec2,
        route: Option<Vec3>,
        now: Time,
        reach: f32,
        max_step: f32,
    ) {
        if let Some(point) = route {
            self.position = if self.presence > 0.0 {
                self.position.move_towards(point, max_step)
            } else {
                point
            };
            self.settle = None;
            self.settled = 1.0;
            self.leaving_name = None;
            self.travelling = true;
        } else {
            let distance = self.position.distance(target);
            let seated = self.travelling && offset != Vec2::ZERO
                || offset.distance(self.seat) > SEAT_DRIFT;
            // A new seat chooses its name afresh, at the ring point nearest it, rather than
            // holding the place it had before.
            let before = seated
                .then(|| self.name_visibility.placement.take())
                .flatten();
            if seated && self.presence > 0.0 && distance > 1e-4 && distance < reach {
                self.settle = Some((self.position, now));
                self.leaving_name = before.map(|name| name.point);
            }
            let seat = match self.settle {
                Some((from, begun)) if now - begun < SETTLE_SECONDS => {
                    let t = (now - begun) / SETTLE_SECONDS;
                    self.settled = t;
                    from.lerp(target, t * t * (3.0 - 2.0 * t))
                }
                _ => {
                    self.settle = None;
                    self.settled = 1.0;
                    self.leaving_name = None;
                    target
                }
            };
            self.position = if self.presence > 0.0 {
                self.position.move_towards(seat, max_step)
            } else {
                seat
            };
            self.travelling = false;
        }
        self.seat = offset;
    }

    fn update_ring(&mut self, dt: f32, ring_capacity: Option<u8>) {
        if self.casting_ring.is_some() {
            return;
        }
        let ring = if let Some(capacity) = ring_capacity {
            self.ring_capacity = self.ring_capacity.max(capacity);
            if capacity > 1 {
                self.ring_capacity = capacity;
                dt / DISBAND
            } else {
                -dt / DISBAND
            }
        } else {
            -dt / DISBAND
        };
        self.ring_presence = (self.ring_presence + ring).clamp(0.0, 1.0);
    }

    pub(super) fn display_presence(&self) -> f32 {
        self.presence * self.idle_opacity
    }

    fn update_presence(&mut self, now: Time, dt: f32, opacity: f32) {
        self.projection_scale = slots::projection_scale(self.position);
        let idle = !self.transmitting
            && !self.travelling
            && now - self.last_active > self.idle_after;
        let idle_fade = if idle { -dt / DISBAND } else { dt / DISBAND };
        self.idle_opacity = (self.idle_opacity + idle_fade).clamp(0.0, 1.0);
        let service_visible = now < self.expires || self.transmitting;
        let service_fade = if service_visible { dt / 0.4 } else { -dt / 1.2 };
        self.slot_presence = (self.slot_presence + service_fade).clamp(0.0, 1.0);
        let fade = if service_visible {
            dt / 0.4
        } else if self.dismissed {
            -dt / DISBAND
        } else {
            -dt / lifecycle::EXIT
        };
        self.presence = (self.presence + fade).clamp(0.0, opacity);
        self.grey = lifecycle::progress(now, self.expires, 0.4);
        self.lifecycle_scale = 1.0
            + (ARRIVAL_SCALE - 1.0)
                * (1.0 - lifecycle::progress(now, self.arrived + 0.4, 0.8));
    }
}

/// The author whose group an avatar parks in, and its seat there; an author sits in
/// seat 0 of its own group.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Group {
    pub lead: usize,
    pub seat: u8,
}

impl Avatar {
    pub(super) fn label_rect(
        &self,
        body: Vec2,
        size: Vec2,
        radius: f32,
    ) -> bevy::math::Rect {
        let angle = self
            .name_visibility
            .placement
            .as_ref()
            .map_or(0.0, |p| -p.angle);
        slots::labels::tangent_rect(body, size, radius, angle)
    }
}

#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug))]
pub(super) struct Beam {
    pub target: Target,
    pub author: usize,
    pub from: Vec3,
    pub to: Vec3,
    pub width: f32,
}

#[cfg_attr(test, derive(Debug))]
pub(super) struct Fragment {
    #[cfg(test)]
    beam: Option<(usize, Target)>,
    pub position: Vec3,
    pub rising: bool,
    pub sample_detail: f32,
    pub alpha: f32,
    pub scale: f32,
    pub sample: Option<crate::diff_samples::Sample>,
}

impl Fragment {
    fn with_beam(self, author: usize, target: Target) -> Self {
        #[cfg(not(test))]
        let _ = (author, target);
        #[cfg(test)]
        let result = Self {
            beam: Some((author, target)),
            ..self
        };
        #[cfg(not(test))]
        let result = self;
        result
    }
}

/// Placement priority, highest first, when labels would overlap on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum LabelKind {
    Avatar,
    Detail,
    Landmark,
    Activity,
}

impl LabelKind {
    pub(super) const fn font_scale(self) -> f32 {
        match self {
            Self::Avatar => 12.0 / 14.0,
            Self::Detail | Self::Landmark | Self::Activity => 10.0 / 14.0,
        }
    }
}

#[derive(Clone)]
#[cfg_attr(test, derive(Debug))]
pub(super) struct Label {
    pub repository: Option<usize>,
    pub position: Vec3,
    pub text: String,
    pub kind: LabelKind,
    pub scale: f32,
    pub opacity: f32,
    pub priority: f32,
}

#[derive(Clone, Debug)]
struct State {
    field_growth: f32,
    field: crate::layout::packing::Tile,
    region_depth: f32,
    tick: u64,
    layout: Layout,
    avatars: Vec<Avatar>,
    authors: HashMap<Key, usize>,
    next_event: usize,
    pulses: VecDeque<Pulse>,
    focus: Vec3,
    distance: f32,
    geometry: Geometry,
    avatar_scale: f32,
    avatar_size: f32,
    avatar_max_speed: f32,
    gutter: f32,
    fragment_pace: FragmentPace,
    slot_records: BTreeMap<usize, slots::Record>,
    /// Seats in each avatar's group as last placed, 1 for an avatar alone.
    capacities: Vec<u8>,
}

#[derive(Clone)]
struct SharedState(std::sync::Arc<State>);

impl std::ops::Deref for SharedState {
    type Target = State;
    fn deref(&self) -> &State {
        &self.0
    }
}

impl std::ops::DerefMut for SharedState {
    fn deref_mut(&mut self) -> &mut State {
        std::sync::Arc::make_mut(&mut self.0)
    }
}

impl std::fmt::Debug for SharedState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Deterministic playback stepped at a fixed tick rate, independent of output frame rate.
#[derive(Clone)]
pub(super) struct Simulation {
    file_ids: HashMap<String, FileIdentity>,
    first_arrivals: Vec<u16>,
    /// Per selected event, the participants taking part in no later event, as bits by
    /// rank.
    finals: Vec<u16>,
    departures: Vec<Time>,
    repository_ends: Vec<Time>,
    repository_exits: Arc<[Option<RepositoryExit>]>,
    pub caption_style: caption::Style,
    caption_plan: Vec<caption::Transition>,
    packing: crate::layout::packing::Packing,
    envelopes: Vec<slots::Envelope>,
    pub repository_boundary: crate::repositories::Boundary,
    pub(super) collapse_directories: bool,
    pub(super) diff_text_distance: f32,
    events: windows::Events,
    offset: usize,
    /// Commits before `offset` that import never read, counted only in commit numbers.
    unread_before: usize,
    total: usize,
    schedule: Schedule,
    clock: schedule::Clock,
    births: Vec<(usize, u64, u64)>,
    playback_multiplier: f32,
    heat_half_life: f32,
    initial: std::sync::Arc<State>,
    state: SharedState,
    theme: Theme,
    ray_view: Option<RayView>,
    travel: travel::Plan,
    visuals: Visuals,
    progress: Progress,
    trace: Trace,
    tick_times: trace::TickTimes,
    window: Option<windows::Planning>,
    boundaries: Vec<std::sync::Arc<State>>,
    windowed: bool,
    stream: Option<std::sync::Arc<std::sync::Mutex<windows::Stream>>>,
    ready_end: Option<streaming::Ready>,
    /// Lets a test advance every file in every replay, to compare against holding them.
    #[cfg(test)]
    advance_all_files: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RayView {
    eye: Vec3,
    normal: Vec3,
}

impl RayView {
    pub(super) fn new(eye: Vec3, forward: Vec3) -> Self {
        Self {
            eye,
            normal: forward,
        }
    }

    fn towards(self, centre: Vec3, target: Vec3) -> Vec3 {
        let from_eye = target - self.eye;
        let depth = from_eye.dot(self.normal);
        let across = if depth > 0.0001 {
            self.eye + from_eye * ((centre - self.eye).dot(self.normal) / depth)
                - centre
        } else {
            target - centre
        };
        (across - self.normal * across.dot(self.normal))
            .try_normalize()
            .unwrap_or_else(|| self.normal.any_orthonormal_vector())
    }
}

/// What a tick places beyond layout, slots and avatars.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Visuals {
    None,
    Seating,
    All,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum FileIdentity {
    Unique(u64),
    Ambiguous,
}

impl Simulation {
    pub(super) fn check_repository_plan(
        events: &[Event],
        options: &Options,
        repositories: &[String],
    ) -> anyhow::Result<()> {
        Self::check_repository_endings(events, options, repositories)?;
        let schedule = Schedule::for_replay(events, options);
        Self::check_schedule(events, options, repositories, &schedule, None)
    }

    fn check_repository_endings(
        events: &[Event],
        options: &Options,
        repositories: &[String],
    ) -> anyhow::Result<()> {
        let mut latest = i64::MIN;
        let mut required = vec![i64::MIN; repositories.len()];
        let mut first = None;
        for (index, event) in events.iter().enumerate() {
            latest = latest.max(event.seconds);
            required[event.repository] = latest;
            if let Some(ending) = options
                .repository_ends
                .get(event.repository)
                .copied()
                .flatten()
                .filter(|&ending| latest >= ending)
            {
                first.get_or_insert((index, event, ending));
            }
        }
        if let Some((index, event, ending)) = first {
            anyhow::bail!(
                "repository '{}' has ended = '{}' UTC (inclusive), but commit {} at imported position {} falls after that ending in history order; set ended to '{}' or later, inclusive, or remove ended",
                crate::terminal_escape(&repositories[event.repository]),
                date(ending.saturating_sub(1)),
                event.commit,
                index + 1,
                date(required[event.repository]),
            );
        }
        Ok(())
    }

    fn check_schedule(
        events: &[Event],
        options: &Options,
        repositories: &[String],
        schedule: &Schedule,
        exit_tail: Option<u64>,
    ) -> anyhow::Result<()> {
        let pace = options.fragment_pace();
        let duration = if let Some(exit_tail) = exit_tail {
            lifecycle::activity_duration(schedule, pace).max(tick_time(exit_tail))
        } else {
            let (_, endings) = lifecycle::plan(
                events,
                schedule,
                options,
                options.selected_range(events.len()),
            );
            lifecycle::duration(schedule, &endings, pace)
        };
        planning_memory::check(events, options, repositories, schedule, duration)?;
        Ok(())
    }

    /// The synthetic history twice, a day apart, as repositories `first` and `second`.
    #[cfg(test)]
    pub(super) fn two_synthetic_repositories() -> Self {
        let histories = (0..2_u8)
            .map(|index| {
                let mut events = crate::timeline::synthetic();
                for event in &mut events {
                    event.seconds += i64::from(index) * 86_400;
                }
                crate::repositories::namespace(&mut events, usize::from(index));
                events
            })
            .collect();
        Self::with_repositories(
            crate::repositories::merge(histories),
            &Options::default(),
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
            &["first".into(), "second".into()],
        )
    }

    #[cfg(test)]
    pub(super) fn scheduled_starts(&self) -> &[Time] {
        &self.schedule.starts
    }

    #[cfg(test)]
    pub(super) fn new(
        mut events: Vec<Event>,
        options: &Options,
        theme: &Theme,
        mappings: &Mappings,
    ) -> Self {
        crate::repositories::namespace(&mut events, 0);
        Self::with_repositories(
            events,
            options,
            theme,
            mappings,
            &["Synthetic history".into()],
        )
    }

    #[cfg(test)]
    pub(super) fn with_repositories(
        events: Vec<Event>,
        options: &Options,
        theme: &Theme,
        mappings: &Mappings,
        repositories: &[String],
    ) -> Self {
        Self::try_with_repositories(events, options, theme, mappings, repositories)
            .expect("validated repository playback")
    }

    pub(super) fn try_with_repositories(
        mut events: Vec<Event>,
        options: &Options,
        theme: &Theme,
        mappings: &Mappings,
        repositories: &[String],
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !repositories.is_empty(),
            "simulation requires a repository name"
        );
        if events.iter().any(|event| event.contributor.is_none()) {
            mappings.apply(&mut events);
        }
        Self::check_repository_plan(&events, options, repositories)?;
        let events = windows::Events::from(events);
        let global_started = std::time::Instant::now();
        let trace = &options.trace;
        let mut layout_cache = None;
        let mut simulation = Self::build_shared(
            &events,
            options,
            theme,
            &[],
            repositories,
            &mut layout_cache,
        )?;
        simulation.packing.tile_fresh(options.replay_everything);
        if let Some(seconds) = options.travel_window_seconds {
            let elapsed = global_started.elapsed();
            if trace.on() {
                eprintln!("global_plan_seconds={}", elapsed.as_secs_f64());
            }
            simulation.report_global(0, elapsed);
            return simulation.plan_windows(events, options, repositories, seconds);
        }
        let mut padding = simulation.travel_padding(options);
        let mut history = simulation.schedule.clone();
        let departures = simulation.departures.clone();
        let mut pass = 0;
        let mut compactions = 0;
        let walk = options.travel_cap == TravelCap::None;
        loop {
            pass += 1;
            check_travel_pass(pass)?;
            options.progress.pass(pass);
            let _pass = trace.phase(Phase::Pass);
            if pass > 1 || history.needs_presentation(&padding) {
                simulation.apply_padding(
                    &events,
                    options,
                    repositories,
                    &padding,
                    (&history, &departures),
                )?;
            }
            simulation.plan_travel(options, &padding);
            if trace.on() {
                eprintln!(
                    "travel_replan pass={} fades={} padding_seconds={}",
                    pass,
                    simulation.travel.stats.faded,
                    simulation.travel.stats.padding_seconds
                );
            }
            if walk && simulation.travel.protect_birth_walks(&mut history) {
                compactions = 0;
                continue;
            }
            let adjusted = simulation.adjust_travel_padding(options, &padding);
            let settled = adjusted == padding
                || (!walk && !simulation.travel.unsettled_relocation() && pass >= 4);
            if !settled {
                padding = adjusted;
                continue;
            }
            if compactions > 0 {
                let refined = simulation.refine_service_slots(options, &padding);
                if refined != padding {
                    padding = refined;
                    continue;
                }
                if compactions == 1 && walk {
                    compactions = 2;
                    padding = simulation.compact_travel_padding(&padding);
                    continue;
                }
                break;
            }
            compactions = 1;
            padding = simulation.compact_travel_padding(&padding);
        }
        if walk {
            simulation.travel.require_birth_routes(options.bot_travel)?;
        }
        simulation.align_travel_arrivals(options);
        simulation.repair_walk_departures(options, &padding);
        {
            let _phase = trace.phase(Phase::Caption);
            simulation.caption_plan =
                caption::plan(&simulation.schedule, &simulation.events);
        }
        options.progress.pass(1);
        simulation.progress = Progress::default();
        simulation.report_global(pass, global_started.elapsed());
        Ok(simulation)
    }

    fn report_global(&self, passes: u32, elapsed: Duration) {
        self.count_tilings();
        self.trace
            .report(Scope::Global, self.events.len(), passes, elapsed);
    }

    fn apply_padding(
        &mut self,
        events: &[Event],
        options: &Options,
        repositories: &[String],
        padding: &[(usize, u32)],
        (history, departures): (&Schedule, &[Time]),
    ) -> anyhow::Result<()> {
        let trace = &options.trace;
        let schedule = {
            let _phase = trace.phase(Phase::ScheduleRebuild);
            trace.count_rebuild(history.starts.len(), padding.len());
            history.with_travel(history.starts.len(), |index| {
                schedule::padding_at(padding, options.start + index)
            })
        };
        {
            let _phase = trace.phase(Phase::CheckSchedule);
            let exit_tail = schedule.present_tail_tick(
                history,
                lifecycle::exit_end(&self.repository_exits),
            );
            Self::check_schedule(
                events,
                options,
                repositories,
                &schedule,
                Some(exit_tail),
            )?;
        }
        let preferences = std::mem::take(&mut self.travel).preferences();
        {
            let _phase = trace.phase(Phase::WindowClock);
            let clock = schedule::Clock::new(history.clone(), schedule.clone());
            self.births = self.births_for(&schedule);
            self.departures =
                departures.iter().map(|&time| clock.present(time)).collect();
            self.clock = clock;
        }
        self.schedule = schedule;
        self.state = SharedState(self.initial.clone());
        self.plan_avatar_envelopes(None);
        self.travel = preferences;
        Ok(())
    }

    fn births_for(&self, schedule: &Schedule) -> Vec<(usize, u64, u64)> {
        self.clock
            .history
            .births()
            .zip(schedule.births())
            .map(|((index, history), (_, arrival))| {
                let travel =
                    schedule.padding(index) - self.clock.history.padding(index);
                (
                    self.events[index].repository,
                    arrival
                        - u64::from(travel)
                        - schedule.motion_wait(&self.clock.history, index),
                    history,
                )
            })
            .collect()
    }

    fn scheduled_layout(
        events: &[Event],
        layout_starts: &[Time],
        repositories: &[String],
        layout_cache: &mut Option<(Vec<Time>, Layout)>,
        progress: &Progress,
    ) -> Layout {
        if let Some((starts, layout)) = layout_cache
            && *starts == layout_starts
        {
            layout.clone()
        } else {
            #[cfg(test)]
            tests::LAYOUT_COUNTS
                .with(|counts| counts.set((counts.get().0 + 1, counts.get().1)));
            let layout = Layout::scheduled_repositories(
                events,
                layout_starts,
                repositories,
                progress,
            );
            *layout_cache = Some((layout_starts.to_vec(), layout.clone()));
            layout
        }
    }

    fn build_schedules(
        events: &[Event],
        options: &Options,
        padding: &[(usize, u32)],
    ) -> (Schedule, Schedule) {
        let start = options.start;
        let history = Schedule::for_replay(events, options);
        let schedule = history.with_travel(history.starts.len(), |index| {
            schedule::padding_at(padding, start + index)
        });
        let layout_starts = Schedule::with_padding(
            events,
            options.seconds_per_day,
            options.max_idle_seconds,
            |index| {
                index
                    .checked_sub(start)
                    .filter(|&i| i < schedule.starts.len())
                    .map_or(0, |i| schedule.padding(i))
            },
        );
        (schedule, layout_starts)
    }

    #[cfg(test)]
    fn build(
        mut events: Vec<Event>,
        options: &Options,
        theme: &Theme,
        mappings: &Mappings,
        padding: &[(usize, u32)],
        repositories: &[String],
        layout_cache: &mut Option<(Vec<Time>, Layout)>,
    ) -> anyhow::Result<Self> {
        if events.iter().any(|event| event.contributor.is_none()) {
            mappings.apply(&mut events);
        }
        Self::build_shared(
            &events.into(),
            options,
            theme,
            padding,
            repositories,
            layout_cache,
        )
    }

    /// Plays `events`, whose contributors are already mapped, sharing their storage.
    fn build_shared(
        events: &windows::Events,
        options: &Options,
        theme: &Theme,
        padding: &[(usize, u32)],
        repositories: &[String],
        layout_cache: &mut Option<(Vec<Time>, Layout)>,
    ) -> anyhow::Result<Self> {
        let _phase = options.trace.phase(Phase::Build);
        let Range { start, end } = options.selected_range(events.len());
        let (schedule, layout_starts) = Self::build_schedules(events, options, padding);
        let layout = {
            let _phase = options.trace.phase(Phase::ScheduledLayout);
            Self::scheduled_layout(
                events,
                &layout_starts.starts,
                repositories,
                layout_cache,
                &options.progress,
            )
        };
        let mut state = State::new(layout, theme.geometry(), options);
        state.apply_history(&events[..start]);
        if start > 0 {
            state.layout.plan_time(
                layout_starts.starts[start.min(layout_starts.starts.len() - 1)].since(
                    schedule
                        .starts
                        .first()
                        .copied()
                        .unwrap_or(Time::seconds(LEAD_IN)),
                ),
            );
            state.layout.settle(SETTLE_TICKS, tick_seconds());
            state.avatars.clear();
            state.authors.clear();
        }
        let total = events.len() + options.unread.total();
        let prior_active = prior_activity(&events[..start], repositories.len());
        let (departures, repository_ends) =
            lifecycle::plan(events, &schedule, options, start..end);
        let repository_exits = lifecycle::repository_exits(&repository_ends);
        let first_arrivals = lifecycle::arrivals(events, start..end);
        let finals =
            lifecycle::finals(&events[start..end], &schedule.starts, &departures);
        let mut simulation = Self {
            file_ids: file_identities(&state),
            first_arrivals,
            finals,
            departures,
            repository_ends,
            repository_exits,
            caption_style: caption::Style::default(),
            caption_plan: Vec::new(),
            packing: crate::layout::packing::Packing::default(),
            envelopes: Vec::new(),
            repository_boundary: crate::repositories::Boundary::default(),
            collapse_directories: true,
            diff_text_distance: 1.0,
            events: events.range(start..end),
            offset: start,
            unread_before: options.unread.before,
            total,
            clock: schedule::Clock::new(schedule.clone(), schedule.clone()),
            births: Vec::new(),
            schedule,
            playback_multiplier: 1.0,
            heat_half_life: options.heat_half_life,
            initial: std::sync::Arc::new(state.clone()),
            state: SharedState(std::sync::Arc::new(state)),
            theme: theme.clone(),
            ray_view: None,
            travel: travel::Plan::default(),
            visuals: Visuals::All,
            progress: options.progress.clone(),
            trace: options.trace.clone(),
            tick_times: trace::TickTimes::default(),
            window: None,
            boundaries: Vec::new(),
            windowed: options.travel_window_seconds.is_some(),
            stream: None,
            ready_end: None,
            #[cfg(test)]
            advance_all_files: options.advance_all_files,
        };
        simulation.births = simulation.births_for(&simulation.schedule);
        let snapshots = simulation.plan_repositories(prior_active, options)?;
        simulation.schedule.birth_protections = simulation.birth_protections();
        if let Some(end) = simulation.packing.opening_end() {
            simulation
                .schedule
                .protect_opening_rays(&simulation.events, end);
        }
        simulation
            .schedule
            .protect_tile_motion(simulation.packing.motion_intervals());
        if options.travel_window_seconds.is_none() {
            simulation.plan_avatar_envelopes(snapshots);
        }
        Ok(simulation)
    }

    fn birth_protections(&self) -> Vec<(usize, usize)> {
        self.schedule
            .births()
            .filter_map(|(birth, write)| {
                let begin = write - BIRTH_TICKS - 1;
                let first = self.schedule.applied(begin);
                if first >= birth {
                    return None;
                }
                let repository = self.events[birth].repository;
                let tick = usize::try_from(write - 1).expect("bounded birth tick");
                let growing = self.packing.expanding_neighbours(repository, tick);
                (first..birth)
                    .rev()
                    .find(|&i| growing[self.events[i].repository])
                    .map(|earlier| (birth, earlier))
            })
            .collect()
    }

    /// Turns a batch of sampled ticks into packing inputs, in tick order, on scoped threads.
    fn push_regions(
        &self,
        samples: &mut crate::layout::packing::Inputs,
        pending: &mut Vec<crate::layout::RegionSample>,
        labels: &[String],
        threads: usize,
    ) {
        let _phase = self.trace.phase(Phase::Regions);
        let geometry = self.state.geometry;
        let regions = crate::layout::in_order(pending, 32, threads, |chunk| {
            chunk
                .iter()
                .map(|sample| sample.regions(labels, geometry))
                .collect()
        });
        for regions in regions {
            samples.push(regions);
        }
        pending.clear();
    }

    fn plan_repositories(
        &mut self,
        mut active: Vec<bool>,
        options: &Options,
    ) -> anyhow::Result<Option<slots::Snapshots>> {
        let threads = options.region_threads;
        #[cfg(test)]
        tests::LAYOUT_COUNTS
            .with(|counts| counts.set((counts.get().0, counts.get().1 + 1)));
        let _phase = self.trace.phase(Phase::PlanRepositories);
        let mut snapshots = (!self.windowed)
            .then(|| slots::Snapshots::new(&self.events, self.state.fragment_pace));
        for (_, file) in self.layout().files() {
            active[self.layout().dir(file.dir).repository] = true;
        }
        if !active.iter().any(|&live| live)
            && let Some(event) = self.events.first()
        {
            active[event.repository] = true;
        }
        let mut sampled_events = 0;
        let ticks = frames_for_ticks(self.end_tick(), 60)
            .expect("multi-repository playback fits the supported frame range");
        self.trace.count_replay(ticks.into());
        let mut samples = crate::layout::packing::Inputs::new(
            usize::try_from(ticks).expect("bounded packing ticks") + 1,
        );
        let labels = self.layout().region_labels();
        let mut pending = vec![self.layout().region_sample(&active)];
        let files = self.file_motion(FileMotion::Glide);
        for tick in 0..ticks {
            self.progress
                .report(Stage::Layout, tick.into(), ticks.into());
            self.tick_layout_with_files(files);
            for event in &self.events[sampled_events..self.state.next_event] {
                active[event.repository] = true;
            }
            sampled_events = self.state.next_event;
            pending.push(self.layout().region_sample(&active));
            if pending.len() >= REGION_BATCH {
                self.push_regions(&mut samples, &mut pending, &labels, threads);
            }
            if let Some(snapshots) = &mut snapshots {
                snapshots.capture(&self.state);
            }
        }
        self.push_regions(&mut samples, &mut pending, &labels, threads);
        self.trace.add_ticks(&mut self.tick_times);
        self.packing = crate::layout::packing::Packing::from_inputs(
            samples,
            self.repository_exits.clone(),
            self.state.gutter,
            threads,
        )?;
        self.state = SharedState(self.initial.clone());
        self.state.field_growth = self.packing.overview_growth();
        self.state.field = self.packing.field();
        self.state.region_depth = framing::depth_bound() * self.state.field_growth;
        if let Some((regions, offsets, scales)) = self.packing.sample(0) {
            self.state.layout.place_regions(
                regions,
                &offsets,
                &scales,
                FileMotion::Advance,
            );
        }
        self.initial = self.state.0.clone();
        self.count_tilings();
        Ok(snapshots)
    }
    pub(super) fn duration(&self) -> Time {
        if self.ready_end.is_some() {
            return tick_time(self.end_tick());
        }
        if let Some(window) = &self.window {
            return self.clock.present(window.end);
        }
        self.activity_duration()
            .max(tick_time(self.exit_tail_tick()))
    }

    fn activity_duration(&self) -> Time {
        lifecycle::activity_duration(&self.schedule, self.state.fragment_pace)
    }

    fn exit_tail_tick(&self) -> u64 {
        self.clock
            .present_tail_tick(lifecycle::exit_end(&self.repository_exits))
    }

    /// The tick playback ends on, from the same padded schedule eager and streamed
    /// playback both finish with; a window still being planned ends where it is planned.
    pub(crate) fn end_tick(&self) -> u64 {
        match self.ready_end {
            Some(ready) if !ready.complete => ready.end,
            _ => self
                .activity_duration()
                .ticks(true)
                .max(self.exit_tail_tick()),
        }
    }

    /// Frames of whole playback at `fps`, counted from `end_tick` without an `f32` hop.
    pub(crate) fn frames(&self, fps: u16) -> anyhow::Result<u32> {
        frames_for_ticks(self.end_tick(), fps)
    }

    pub(super) fn repository_effect(&self, repository: usize) -> (f32, f32, f32) {
        let end = self
            .repository_ends
            .get(repository)
            .copied()
            .unwrap_or(Time::INFINITY);
        let tiles = tick_time(self.clock.rows(self.state.tick).0);
        let u = lifecycle::progress(tiles, end, lifecycle::REPOSITORY_EXIT);
        (u.min(0.2) * 5.0, 1.0 - u, u * self.scale() * 1.5)
    }

    pub(super) fn file_effect(
        &self,
        id: u64,
        file: &crate::layout::File,
    ) -> (Vec3, f32, f32) {
        let dir = self.layout().dir(file.dir);
        let (grey, alpha, rise) = self.repository_effect(dir.repository);
        let position = ground(file.position) + Vec3::Y * rise;
        let (position, grey, alpha) = match file.state {
            crate::layout::Presence::Arriving => {
                let (beam, progress) = self.arrival_beam(
                    Target::File(id),
                    file.arrival_author,
                    position,
                    file.presence,
                    file.flight,
                    file.arrival_revision,
                );
                (
                    self.arrival_position(
                        beam,
                        progress,
                        position,
                        file.flight.is_some(),
                    ),
                    grey,
                    if progress > 0.0 { alpha } else { 0.0 },
                )
            }
            crate::layout::Presence::Leaving => (
                position + Vec3::Y * (1.0 - file.presence) * self.scale(),
                1.0,
                if file.arrival_cancelled {
                    0.0
                } else {
                    alpha * file.presence
                },
            ),
            crate::layout::Presence::Recovering => (
                position + Vec3::Y * (1.0 - file.presence) * self.scale(),
                grey,
                alpha * file.presence,
            ),
            crate::layout::Presence::Present => (position, grey, alpha),
        };
        (position, grey, alpha)
    }

    fn arrival_beam(
        &self,
        mut target: Target,
        author: usize,
        mut to: Vec3,
        mut progress: f32,
        flight: Option<crate::layout::DirectoryFlight>,
        revision: u64,
    ) -> (Option<Beam>, f32) {
        if let Some(flight) = flight {
            target = Target::Dir(flight.directory);
            let dir = self.layout().dir(flight.directory);
            to = ground(dir.position)
                + Vec3::Y * self.repository_effect(dir.repository).2;
            progress = ((progress - flight.offset) / (1.0 - MAX_ARRIVAL_OFFSET))
                .clamp(0.0, 1.0);
        }
        let repository = match target {
            Target::File(id) => self
                .layout()
                .dir(self.layout().file(id).expect("arrival file exists").dir),
            Target::Dir(index) => self.layout().dir(index),
        }
        .repository;
        let avatar = &self.state.avatars[author];
        let beam = if revision > avatar.last_transfer_revision
            && !avatar
                .pending_arrivals
                .iter()
                .any(|(range, _)| range.contains(&revision))
            && self.can_emit_ray(author, repository)
        {
            let ring = self.state.avatars[author]
                .ring_arrivals
                .iter()
                .find(|flight| flight.revisions.contains(&revision))
                .and_then(|flight| self.state.visible_ring(flight.lead));
            self.clear_arrival_start(author, repository, to, ring)
                .map(|from| Beam {
                    target,
                    author,
                    from,
                    to,
                    width: 1.0,
                })
        } else {
            None
        };
        (beam, progress)
    }

    fn arrival_position(
        &self,
        beam: Option<Beam>,
        progress: f32,
        to: Vec3,
        aggregate: bool,
    ) -> Vec3 {
        let Some(beam) = beam else { return to };
        if !aggregate {
            return self.theme.beam_style.point(beam.from, beam.to, progress);
        }
        let flight = (progress / ARRIVAL_FLIGHT).min(1.0);
        let settle = ((progress - ARRIVAL_FLIGHT) / (1.0 - ARRIVAL_FLIGHT)).max(0.0);
        let eased = settle * settle * (3.0 - 2.0 * settle);
        self.theme
            .beam_style
            .point(beam.from, beam.to, flight)
            .lerp(to, eased)
    }

    pub(super) fn directory_arrival(
        &self,
        index: usize,
        to: Vec3,
    ) -> Option<(Option<Beam>, Vec3, f32)> {
        let arrival = self.layout().dir(index).arrival?;
        let (beam, progress) = self.arrival_beam(
            Target::Dir(index),
            arrival.author,
            to,
            arrival.progress,
            arrival.flight,
            arrival.revision,
        );
        let position =
            self.arrival_position(beam, progress, to, arrival.flight.is_some());
        Some((beam, position, progress))
    }

    pub(super) fn file_arrival_beams(
        &self,
    ) -> impl Iterator<Item = (usize, Beam)> + '_ {
        self.layout().arriving_files().filter_map(|(id, file)| {
            let dir = self.layout().dir(file.dir);
            let rise = self.repository_effect(dir.repository).2;
            let to = ground(file.position) + Vec3::Y * rise;
            self.arrival_beam(
                Target::File(id),
                file.arrival_author,
                to,
                file.presence,
                file.flight,
                file.arrival_revision,
            )
            .0
            .map(|beam| (file.dir, beam))
        })
    }

    pub(super) fn restart(&mut self) {
        self.state = SharedState(
            self.window
                .as_ref()
                .map_or(&self.initial, |w| &w.state)
                .clone(),
        );
    }

    pub(super) fn change_rate(&mut self, factor: f32) {
        self.set_playback_multiplier(self.playback_multiplier * factor);
    }

    pub(super) fn set_playback_multiplier(&mut self, multiplier: f32) {
        self.playback_multiplier = multiplier
            .clamp(*PLAYBACK_MULTIPLIERS.start(), *PLAYBACK_MULTIPLIERS.end());
    }

    pub(super) fn playback_multiplier(&self) -> f32 {
        self.playback_multiplier
    }

    pub(super) fn seconds(&self) -> Time {
        tick_time(self.state.tick)
    }

    pub(super) fn advance_to(&mut self, frame: u32, fps: u16) {
        if self.stream.is_some() {
            self.prepare_frame(frame, fps, true)
                .expect("streaming frame ready");
        }
        let target = Schedule::frame_tick(frame, fps);
        if target < self.state.tick {
            self.state = SharedState(self.initial.clone());
        }
        while self.state.tick < target {
            self.tick();
        }
    }

    fn packing_sample(
        &self,
        tick: u64,
        repositories: Option<&[usize]>,
    ) -> Option<(Vec<crate::layout::RepositoryRegion>, Vec<Vec2>, Vec<f32>)> {
        let (tiles, content) = self.clock.rows(tick);
        let span = BIRTH_TICKS + 1;
        let births: Vec<_> = self
            .births
            .iter()
            .filter_map(|&(repository, ready, history)| {
                let elapsed = |tick: u64, end: u64| {
                    tick.saturating_sub(end - span).min(BIRTH_TICKS)
                };
                let shown = elapsed(tick, ready);
                (shown != elapsed(tiles, history)).then_some((repository, shown))
            })
            .collect();
        self.packing
            .sample_split(tiles, content, &births, repositories)
    }

    pub(super) fn visible_repository_tile(
        &self,
        repository: usize,
    ) -> Option<crate::layout::packing::Tile> {
        self.layout().repositories[repository].tile
    }

    #[cfg_attr(
        not(test),
        expect(clippy::unused_self, reason = "only tests override the needed motion")
    )]
    fn file_motion(&self, needed: FileMotion) -> FileMotion {
        #[cfg(test)]
        if self.advance_all_files {
            return FileMotion::Advance;
        }
        needed
    }

    pub(crate) fn tick_layout(&mut self) {
        let files = self.file_motion(match self.visuals {
            Visuals::None | Visuals::Seating => FileMotion::Hold,
            Visuals::All => FileMotion::Advance,
        });
        self.tick_layout_with_files(files);
    }

    fn tick_layout_with_files(&mut self, files: FileMotion) {
        self.tick_times.laps.begin(&self.trace);
        let placement = self.packing_sample(self.state.tick + 1, None);
        self.tick_times.laps.end(Phase::TickPacking);
        self.tick_local_layout(files);
        if let Some((regions, offsets, scales)) = placement {
            // Carry occupied berths first so seat selection sees the new gutter positions.
            self.state.carry_docked_avatars(&regions, &self.travel);
            self.state
                .layout
                .place_regions(regions, &offsets, &scales, files);
        }
        self.tick_times.laps.end(Phase::TickPlace);
    }

    fn tick_local_layout(&mut self, files: FileMotion) {
        self.tick_times.laps.begin(&self.trace);
        let canonical = self.state.tick + 1;
        let plan_time = self
            .clock
            .history
            .layout_time(tick_time(self.clock.rows(canonical).1));
        let state = &mut *self.state;
        state.tick = canonical;

        state
            .layout
            .begin_tick(tick_seconds(), self.heat_half_life, files);
        self.tick_times.laps.end(Phase::TickHeat);
        while let Some(event) = self.events.get(state.next_event) {
            let Some(start) = self.schedule.start(state.next_event, canonical) else {
                break;
            };
            let behind = state.left_behind(event);
            state.begin(event, start, &mut self.file_ids);
            for member in behind {
                let commit = state.avatars[member].commit;
                let key = &state.avatars[member].key;
                let rank = self.events[commit]
                    .participants()
                    .position(|c| &c.key == key);
                if rank.is_some_and(|rank| self.finals[commit] & (1 << rank) != 0) {
                    state.avatars[member].expires = start;
                    state.avatars[member].dismissed = true;
                }
            }
            for (rank, contributor) in event.participants().enumerate() {
                let Some(&author) = state.authors.get(&contributor.key) else {
                    continue;
                };
                state.avatars[author].expires = self.departures[state.next_event];
                if self.first_arrivals[state.next_event] & (1 << rank) == 0
                    && state.avatars[author].arrived >= start
                {
                    state.avatars[author].arrived = Time::NEG_INFINITY;
                    state.avatars[author].lifecycle_scale = 1.0;
                }
            }
            state.next_event += 1;
        }
        self.tick_times.laps.end(Phase::TickEvents);
        let dt = tick_seconds();
        state.layout.sample_plan(plan_time, dt);
        let laps = &mut self.tick_times.laps;
        state.layout.step_observed(
            dt,
            files,
            self.trace.on().then_some(&mut self.tick_times.counts),
            |part| {
                laps.end(match part {
                    StepPart::Fade => Phase::TickFade,
                    StepPart::Compact => Phase::TickCompact,
                    StepPart::Prune => Phase::TickPrune,
                    StepPart::Relax => Phase::TickRelax,
                    StepPart::Files => Phase::TickFiles,
                });
            },
        );
    }

    fn tick(&mut self) {
        self.tick_layout();
        let state = &mut *self.state;
        let now = tick_time(state.tick);
        if self.windowed {
            for avatar in state.avatars.iter_mut().filter(|a| !a.dismissed) {
                let expires = self.departures[avatar.commit];
                avatar.expires = if expires - now > windows::MARGIN {
                    Time::INFINITY
                } else {
                    expires
                };
            }
        }
        let dt = tick_seconds();
        state.pulses.retain(|pulse| {
            (now < pulse.end || pulse.awaits_arrival_source(now))
                && (self.visuals == Visuals::None
                    || !self.travel.undocking(pulse.author, state.tick))
        });
        if self.visuals == Visuals::None {
            for avatar in &mut state.avatars {
                avatar.transmitting = false;
            }
        }
        let active = state
            .avatars
            .iter()
            .filter(|avatar| now < avatar.expires)
            .count();
        let scale = names::scale(active);
        state.avatar_scale += (scale - state.avatar_scale) * (1.0 - (-dt / 0.6).exp());
        state.place_slots_planned(&self.envelopes, now, &self.travel);
        self.tick_times.laps.end(Phase::TickSlots);
        if self.visuals != Visuals::None {
            state.hold_transmitters(now, &self.travel);
        }
        state.advance_avatars(now, dt, &self.travel, self.visuals != Visuals::None);
        self.tick_times.laps.end(Phase::TickAvatars);
        if self.visuals == Visuals::All {
            let texts: Vec<_> = self
                .labels_for_detail(|_| true)
                .iter()
                .filter(|l| l.kind != LabelKind::Avatar)
                .filter_map(|label| self.state.overlay_label_bounds(label))
                .chain(caption::frame_text_rects())
                .collect();
            self.tick_times.laps.end(Phase::TickLabels);
            self.state.place_names(dt, &self.travel, &texts);
            self.tick_times.laps.end(Phase::TickNames);
        }
    }

    pub(super) fn theme(&self) -> &Theme {
        &self.theme
    }

    pub(super) fn set_theme(&mut self, theme: Theme) {
        assert_eq!(
            self.state.geometry,
            theme.geometry(),
            "themes share geometry"
        );
        debug_assert_eq!(
            self.theme.file_tile.is_some(),
            theme.file_tile.is_some(),
            "live themes share tile presence"
        );
        self.theme = theme;
    }

    pub(super) fn layout(&self) -> &Layout {
        &self.state.layout
    }

    pub(super) fn avatars(&self) -> &[Avatar] {
        &self.state.avatars
    }

    pub(super) fn camera_events(&self) -> &[Event] {
        &self.events
    }

    pub(super) fn camera_file_identity(&self, path: &str) -> Option<FileIdentity> {
        self.file_ids.get(path).copied()
    }

    /// Default field camera focus and distance along [`view_direction`].
    pub(super) fn view(&self) -> (Vec3, f32) {
        self.state.overview()
    }

    pub(super) fn view_for_projection(&self, fov: f32, viewport: Vec2) -> (Vec3, f32) {
        let (focus, distance) = self.overview_for_projection(fov, viewport);
        if fov == framing::FOV && viewport == framing::VIEWPORT {
            return (focus, distance);
        }
        (
            focus,
            distance.max(framing::distance(
                &self.layout().repositories,
                focus,
                fov,
                viewport,
            )),
        )
    }

    pub(super) fn overview_for_projection(
        &self,
        fov: f32,
        viewport: Vec2,
    ) -> (Vec3, f32) {
        let (focus, distance) = self.view();
        let ratio = ((framing::FOV * 0.5).tan() / (fov * 0.5).tan())
            .max(
                (framing::FOV * 0.5).tan()
                    * (framing::VIEWPORT.x / framing::VIEWPORT.y)
                    / ((fov * 0.5).tan() * viewport.x / viewport.y),
            )
            .max(1.0);
        (
            focus,
            distance * ratio + self.state.region_depth * (ratio - 1.0),
        )
    }

    pub(super) fn scale(&self) -> f32 {
        self.state.scale()
    }

    #[cfg(test)]
    pub(super) fn avatar_scale(&self) -> f32 {
        self.state.avatar_scale
    }

    pub(super) fn member_scales(&self) -> Vec<f32> {
        self.state.member_scales()
    }

    pub(super) fn avatar_disc_scale(&self) -> f32 {
        self.state.avatar_scale * self.state.avatar_size
    }

    pub(super) fn commit(&self) -> usize {
        self.unread_before + self.offset + self.state.next_event
    }

    pub(super) fn total(&self) -> usize {
        self.total
    }

    #[cfg(test)]
    pub(super) fn caption(&self) -> String {
        let Some(event) = self
            .schedule
            .applied(self.state.tick)
            .checked_sub(1)
            .and_then(|index| self.events.get(index))
        else {
            return format!(
                "{} / {} commits | {}x",
                self.commit(),
                self.total(),
                self.playback_multiplier
            );
        };
        format!(
            "{}   |   {}{}   |   commit {} / {}   |   {}x",
            date(event.seconds),
            self.caption_repository()
                .map_or_else(String::new, |(_, name)| format!("{name}   |   ")),
            event
                .contributor
                .as_ref()
                .expect("contributors are resolved")
                .name,
            self.commit(),
            self.total,
            self.playback_multiplier
        )
    }

    pub(super) fn beams(&self) -> Vec<Beam> {
        let now = self.seconds();
        let mut beams = Vec::new();
        for pulse in &self.state.pulses {
            if !self.pulse_beams_visible(pulse) {
                continue;
            }
            let envelope = envelope(now - pulse.start, pulse.end - pulse.start);
            if envelope <= 0.0 {
                continue;
            }
            for &(target, weight, ..) in &pulse.targets {
                if let Some((from, to)) = self.visible_ray(pulse, target) {
                    beams.push(Beam {
                        target,
                        author: pulse.author,
                        from,
                        to,
                        width: envelope * (1.0 + weight.log2() * 0.35).min(3.0),
                    });
                }
            }
        }
        beams
    }

    /// Each visible group ring's author, centre, world radius and opacity.
    pub(super) fn groups(&self) -> Vec<(usize, Vec3, f32, f32)> {
        let now = self.seconds();
        let mut solo = vec![false; self.state.avatars.len()];
        for pulse in &self.state.pulses {
            if matches!(pulse.source, Some(RaySource::Avatar))
                && self.state.avatars[pulse.author].transmitting
            {
                let avatar = &self.state.avatars[pulse.author];
                solo[avatar.group.map_or(pulse.author, |group| group.lead)] = true;
                solo[pulse.author] = true;
            }
        }
        self.state
            .avatars
            .iter()
            .enumerate()
            .filter_map(|(lead, avatar)| {
                let locked = avatar.casting_ring.is_some();
                let body_arrival =
                    avatar.transmitting && now <= avatar.body_arrivals_land;
                if !locked && (solo[lead] || body_arrival) {
                    return None;
                }
                let (centre, rim) = self.state.visible_ring(lead)?;
                if !locked && avatar.position.distance(centre) > rim {
                    return None;
                }
                let opacity = if locked {
                    1.0
                } else {
                    avatar.ring_presence * avatar.display_presence()
                };
                Some((lead, centre, rim, opacity))
            })
            .collect()
    }

    pub(super) fn avatar_group_lead(&self, index: usize) -> usize {
        self.state.avatars[index]
            .group
            .filter(|group| self.state.in_party(index, group.lead))
            .map_or(index, |group| group.lead)
    }

    fn pulse_beams_visible(&self, pulse: &Pulse) -> bool {
        self.can_emit_ray(pulse.author, pulse.repository)
    }

    fn can_emit_ray(&self, author: usize, repository: usize) -> bool {
        let avatar = &self.state.avatars[author];
        avatar.repository == repository
            && avatar.transmitting
            && avatar.beside_gutter(
                self.layout().repositories[repository].gutter,
                self.state.avatar_radius(),
            )
    }

    fn surface_target(&self, target: Target) -> Option<Vec3> {
        let point = self.state.point(target)?;
        Some(ground(Vec2::new(point.x, point.z)))
    }

    fn visible_ray(&self, pulse: &Pulse, target: Target) -> Option<(Vec3, Vec3)> {
        let to = self.surface_target(target)?;
        let ring = self.state.ray_source_ring(pulse.source?);
        let from = self.clear_ray_start(pulse.author, pulse.repository, to, ring)?;
        Some((from, to))
    }

    fn clear_ray_start(
        &self,
        author: usize,
        repository: usize,
        to: Vec3,
        ring: Option<(Vec3, f32)>,
    ) -> Option<Vec3> {
        if let Some(view) = self.ray_view
            && let Some((centre, radius)) = ring
        {
            let inward = view.towards(centre, to);
            let across = view.normal.cross(inward);
            let start = [0.0_f32, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0]
                .into_iter()
                .map(|turn| {
                    let (sin, cos) = (turn * std::f32::consts::FRAC_PI_4).sin_cos();
                    centre + (inward * cos + across * sin) * radius
                })
                .find(|&from| self.ray_path_clear(repository, from, to));
            if start.is_some() {
                return start;
            }
        }
        let from = self.state.beam_start_from(author, to, ring);
        self.ray_path_clear(repository, from, to).then_some(from)
    }

    pub(super) fn set_ray_view(&mut self, view: RayView) {
        self.ray_view = Some(view);
    }

    pub(super) fn ray_view(&self) -> Option<RayView> {
        self.ray_view
    }

    fn clear_arrival_start(
        &self,
        author: usize,
        repository: usize,
        to: Vec3,
        ring: Option<(Vec3, f32)>,
    ) -> Option<Vec3> {
        if let Some(from) = self.clear_ray_start(author, repository, to, ring) {
            return Some(from);
        }
        let (centre, radius) = ring.unwrap_or_else(|| {
            let avatar = &self.state.avatars[author];
            let radius = self.state.geometry.avatar_radius
                * self.scale()
                * self.avatar_disc_scale()
                * avatar.projection_scale
                * avatar.lifecycle_scale
                * avatar.presence;
            (avatar.position, radius)
        });
        let inward = Vec2::new(to.x - centre.x, to.z - centre.z).normalize_or_zero();
        [0.0_f32, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0]
            .into_iter()
            .map(|turn| {
                let direction = Vec2::from_angle(turn * std::f32::consts::FRAC_PI_4);
                centre + ground(direction.rotate(inward) * radius)
            })
            .find(|&from| self.ray_path_clear(repository, from, to))
    }

    fn ray_path_clear(&self, repository: usize, from: Vec3, to: Vec3) -> bool {
        let low = Vec2::new(from.x, from.z);
        let high = Vec2::new(to.x, to.z);
        let tick = self
            .theme
            .beam_style
            .endpoint_tick(from, to)
            .map(|tick| Vec2::new(tick.x, tick.z));
        let crosses = self
            .layout()
            .repositories
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != repository)
            .filter_map(|(_, region)| region.tile)
            .any(|tile| {
                let corners = tile.ground();
                let rect = bevy::math::Rect::from_corners(corners[0], corners[2]);
                slots::segment(low, high, rect)
                    || tick.is_some_and(|tick| {
                        slots::segment(low - tick, low + tick, rect)
                            || slots::segment(high - tick, high + tick, rect)
                    })
            });
        !crosses
    }

    #[cfg(test)]
    pub(super) fn fragments(&self) -> Vec<Fragment> {
        self.fragments_for_detail(|_| 1.0)
    }

    pub(super) fn fragments_for_detail(
        &self,
        detail: impl Fn(usize) -> f32,
    ) -> Vec<Fragment> {
        let now = self.seconds();
        let mut fragments = Vec::new();
        let mut newest = BTreeMap::new();
        let mut remaining = crate::diff_samples::PER_COMMIT;
        for (pulse_index, pulse) in self.state.pulses.iter().enumerate().rev() {
            if !self.pulse_beams_visible(pulse) {
                continue;
            }
            let age = now - pulse.start;
            let envelope = envelope(age, pulse.end - pulse.start);
            if envelope <= 0.0 {
                continue;
            }
            for &(target, _, insertions, removals) in
                pulse.targets.iter().take(FRAGMENT_TARGETS)
            {
                let Some((from, to)) = self.visible_ray(pulse, target) else {
                    continue;
                };
                let sample_detail = match target {
                    Target::File(id) => {
                        self.layout().file(id).map_or(0.0, |file| detail(file.dir))
                    }
                    Target::Dir(_) => 0.0,
                };
                let current = match target {
                    Target::File(id) => {
                        *newest.entry(id).or_insert(pulse_index) == pulse_index
                            && sample_detail > 0.0
                    }
                    Target::Dir(_) => false,
                };
                for (count, rising) in [(insertions, false), (removals, true)] {
                    fragments.extend(attach_samples(
                        pulse.fragments(
                            age,
                            envelope,
                            (self.theme.beam_style, from, to),
                            target,
                            (count, rising),
                            self.state.fragment_pace,
                        ),
                        pulse.samples_for(target, rising).filter(|_| current),
                        &mut remaining,
                        sample_detail,
                    ));
                }
            }
        }
        fragments
    }

    #[cfg(test)]
    pub(super) fn diff_lines(&self) -> Vec<DiffLine> {
        self.diff_lines_for_detail(|_| 1.0)
    }

    pub(super) fn diff_lines_for_detail(
        &self,
        detail: impl Fn(usize) -> f32,
    ) -> Vec<DiffLine> {
        self.fragments_for_detail(detail)
            .into_iter()
            .enumerate()
            .filter_map(|(index, fragment)| {
                Some(DiffLine {
                    fragment: index,
                    detail: fragment.sample_detail,
                    position: fragment.position,
                    scale: fragment.scale * self.scale().sqrt(),
                    sample: fragment.sample?,
                    alpha: fragment.alpha,
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) fn labels(&self) -> Vec<Label> {
        self.labels_for_detail(|_| true)
    }

    /// Elides a detail label to its tile's overview width at the reference resolution.
    fn tile_label(&self, dir: usize, text: String) -> (String, f32) {
        let repository = self.state.layout.dir(dir).repository;
        let scale = LabelKind::Landmark.font_scale();
        let Some(tile) = self.state.layout.repositories[repository].tile else {
            return (text, scale);
        };
        let width = tile.pixels().x / self.state.field_growth;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a whole character count bounded by the tile width"
        )]
        let fits = ((width - 8.0) / (8.6 * scale)).floor().max(1.0) as usize;
        (names::elide(&text, fits), scale)
    }

    pub(super) fn labels_for_detail(
        &self,
        visible: impl Fn(usize) -> bool,
    ) -> Vec<Label> {
        self.labels_with_names(visible, &std::collections::BTreeSet::new())
    }

    pub(super) fn labels_with_names(
        &self,
        visible: impl Fn(usize) -> bool,
        permanent: &std::collections::BTreeSet<Target>,
    ) -> Vec<Label> {
        let layout = &self.state.layout;
        let mut subtree = vec![0_usize; layout.dir_count()];
        let mut centroids = vec![Vec2::ZERO; layout.dir_count()];
        for (index, dir) in layout.dirs() {
            let mut current = Some(index);
            while let Some(ancestor) = current {
                subtree[ancestor] += dir.files;
                centroids[ancestor] += dir.position * count(dir.files);
                current = layout.dir(ancestor).parent;
            }
        }
        let mut dirs: Vec<_> = layout
            .dirs()
            .filter(|(index, dir)| {
                dir.depth > 0
                    && visible(*index)
                    && !permanent.contains(&Target::Dir(*index))
            })
            .collect();
        dirs.sort_by_key(|&(index, dir)| {
            (
                std::cmp::Reverse(subtree[index] >> dir.depth.min(16)),
                dir.depth,
                index,
            )
        });
        let mut extra = 0;
        let mut dirs: Vec<_> = dirs
            .into_iter()
            .enumerate()
            .filter(|(rank, (_, dir))| {
                if *rank < DIR_LABELS {
                    true
                } else if dir.depth == 1 && extra < 4 {
                    extra += 1;
                    true
                } else {
                    false
                }
            })
            .map(|(_, dir)| dir)
            .collect();
        dirs.sort_by_key(|&(index, _)| std::cmp::Reverse(subtree[index]));
        let mut labels: Vec<Label> = dirs
            .into_iter()
            .map(|(index, dir)| {
                let (text, scale) = self.tile_label(index, format!("{}/", dir.name()));
                Label {
                    repository: Some(dir.repository),
                    position: ground(centroids[index] / count(subtree[index].max(1))),
                    text,
                    kind: LabelKind::Landmark,
                    scale,
                    opacity: self.repository_effect(dir.repository).1,
                    priority: 0.0,
                }
            })
            .collect();
        for pulse in &self.state.pulses {
            if pulse.targets.len() > 8 && !pulse.aggregated {
                continue;
            }
            for &(target, ..) in &pulse.targets {
                if permanent.contains(&target) {
                    continue;
                }
                labels.extend(self.activity_label(target));
            }
        }
        for avatar in &self.state.avatars {
            if avatar.presence > 0.0 {
                labels.push(self.state.avatar_label(avatar));
            }
        }
        labels
    }

    fn activity_label(&self, target: Target) -> Option<Label> {
        let layout = self.layout();
        let (index, text) = match target {
            Target::File(id) => {
                let file = layout.file(id)?;
                (file.dir, file.name().to_owned())
            }
            Target::Dir(index) => {
                (index, format!("{}/", layout.directory_label(index)))
            }
        };
        let (text, scale) = self.tile_label(index, text);
        let dir = layout.dir(index);
        let position = self.surface_target(target)?;
        Some(Label {
            repository: Some(dir.repository),
            position: position + Vec3::new(-0.2, 0.0, 0.45) * dir.scale,
            text,
            kind: LabelKind::Activity,
            scale,
            opacity: self.repository_effect(dir.repository).1,
            priority: 0.0,
        })
    }
}

fn file_identities(state: &State) -> HashMap<String, FileIdentity> {
    state
        .layout
        .files()
        .map(|(id, file)| (file.path.clone(), FileIdentity::Unique(id)))
        .collect()
}

impl State {
    fn new(layout: Layout, geometry: Geometry, options: &Options) -> Self {
        Self {
            field_growth: 1.0,
            field: crate::layout::packing::Tile::inset(0.0),
            region_depth: 0.0,
            tick: 0,
            layout,
            avatars: Vec::new(),
            authors: HashMap::new(),
            next_event: 0,
            pulses: VecDeque::new(),
            focus: Vec3::ZERO,
            distance: framing::DISTANCE,
            geometry,
            avatar_scale: 1.0,
            avatar_size: options.avatar_size,
            avatar_max_speed: options.avatar_max_speed,
            gutter: options.gutter,
            fragment_pace: options.fragment_pace(),
            slot_records: BTreeMap::new(),
            capacities: Vec::new(),
        }
    }

    fn apply_history(&mut self, events: &[Event]) {
        for event in events {
            let author = self.participants(event, Time::ZERO)[0];
            for change in &event.changes {
                apply(&mut self.layout, change, author);
            }
        }
    }

    fn carry_docked_avatars(
        &mut self,
        regions: &[crate::layout::RepositoryRegion],
        travel: &travel::Plan,
    ) {
        for (index, avatar) in self.avatars.iter_mut().enumerate() {
            let before = self.layout.repositories[avatar.repository].gutter;
            let after = regions[avatar.repository].gutter;
            let Some((before, after)) = before.zip(after) else {
                continue;
            };
            if before.low == after.low && before.high == after.high
                || avatar.presence <= 0.0
                || avatar.travelling
                || avatar.dock_walk.is_some()
                || avatar.settle.is_some()
                || travel.travelling(index, self.tick)
            {
                continue;
            }
            let Some(slot) = avatar.slot.as_mut() else {
                continue;
            };
            if avatar.transmitting
                || avatar.position.distance_squared(slot.world()) < 1e-8
            {
                let from = slot.dock.on_network(before);
                let to = slot.dock.on_network(after);
                let delta = to - from;
                avatar.position += slots::world(to) - slots::world(from);
                slot.point += delta;
            }
        }
    }

    fn hold_transmitters(&mut self, now: Time, travel: &travel::Plan) {
        for avatar in &mut self.avatars {
            avatar.transmitting = now <= avatar.arrivals_land;
        }
        for pulse in &self.pulses {
            self.avatars[pulse.author].transmitting = true;
        }
        let radius = self.avatar_radius();
        for (author, avatar) in self.avatars.iter_mut().enumerate() {
            avatar.transmitting = avatar.transmitting
                && avatar.beside_gutter(
                    self.layout.repositories[avatar.repository].gutter,
                    radius,
                )
                && !travel.travelling(author, self.tick);
        }
    }

    fn ring_shifts(&self) -> Vec<Vec3> {
        self.avatars
            .iter()
            .enumerate()
            .map(|(index, avatar)| {
                avatar
                    .group
                    .filter(|group| self.in_party(index, group.lead))
                    .map_or(Vec3::ZERO, |group| self.avatars[group.lead].ring_shift())
            })
            .collect()
    }

    fn visible_avatar_positions(&self) -> Vec<Option<Vec3>> {
        self.avatars
            .iter()
            .map(|avatar| (avatar.presence > 0.001).then_some(avatar.position))
            .collect()
    }

    fn advance_avatars(
        &mut self,
        now: Time,
        dt: f32,
        plan: &travel::Plan,
        visuals: bool,
    ) {
        let ring_shifts = self.ring_shifts();
        let reach = self.settle_reach();
        let radius = self.avatar_radius();
        let max_step =
            self.avatar_max_speed * framing::WORLD_UNITS_PER_REFERENCE_PIXEL * dt;
        let previous = (visuals && self.avatars.len() > 1)
            .then(|| self.visible_avatar_positions());
        let mut network = None;
        for (index, avatar) in self.avatars.iter_mut().enumerate() {
            avatar.activity *= (-dt / 3.0).exp();
            if avatar.transmitting {
                avatar.hold_for_rays();
                avatar.update_presence(now, dt, 1.0);
                continue;
            }
            let mut gutter_entry = None;
            let repository = avatar
                .slot_origin
                .map_or(avatar.repository, |(repository, ..)| repository);
            let mut join = |from, to| {
                let cells = network.get_or_insert_with(|| {
                    travel::network(
                        self.layout.repositories.iter().filter_map(|r| r.gutter),
                    )
                });
                let (point, entry) = travel::join_gutter(
                    slots::field_point(from),
                    to,
                    self.layout.repositories[repository].gutter,
                    cells,
                    self.avatar_max_speed * dt,
                );
                gutter_entry = entry;
                slots::world(point)
            };
            let target = avatar
                .slot
                .as_ref()
                .map_or(avatar.position, slots::Placement::world);
            let travel = plan.sample(index, self.tick);
            let planned = travel.map(|(point, _)| {
                if !plan.walking(index, self.tick) {
                    return avatar.position;
                }
                let target = slots::world(point);
                if avatar.presence <= 0.0
                    || avatar.position.distance(target) <= max_step
                {
                    return target;
                }
                join(avatar.position, point)
            });
            let route = avatar.travel_point(planned, self.tick).or_else(|| {
                let slot = avatar.slot.as_ref()?;
                (repository != avatar.repository
                    && avatar.presence > 0.0
                    && avatar.position.distance(target) > reach)
                    .then(|| join(avatar.position, slot.centre()))
            });
            avatar.gutter_entry = gutter_entry;
            let (target, offset) =
                avatar.seat_target(|| plan.approaching_departure(index, self.tick));
            let from = avatar.position;
            let seat = target + ring_shifts[index];
            avatar.walk(seat, offset, route, now, reach, max_step);
            if avatar.position.distance(seat) <= reach {
                avatar.slot_origin = None;
            }
            if (route.is_none() || gutter_entry.is_some()) && avatar.presence > 0.0 {
                let nominal = slots::field_point(avatar.position);
                let clear = travel::seat_clearance(
                    slots::field_point(from),
                    nominal,
                    radius,
                    self.layout
                        .repositories
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| *index != avatar.repository)
                        .filter_map(|(_, repository)| repository.tile),
                );
                if clear != nominal {
                    avatar.position = slots::world(clear);
                }
            }
            avatar.update_presence(now, dt, travel.map_or(1.0, |(_, alpha)| alpha));
        }
        if let Some(previous) = previous {
            occlusion::clear_seat_transitions(self, plan, &previous, max_step);
        }
        self.update_rings(dt);
        if visuals {
            self.update_ray_sources(now);
        }
    }

    fn update_rings(&mut self, dt: f32) {
        let mut capacities: Vec<_> = self
            .avatars
            .iter()
            .enumerate()
            .map(|(index, avatar)| {
                let capacity = avatar
                    .group
                    .filter(|group| group.lead == index)
                    .filter(|_| !avatar.travelling || avatar.ring_presence > 0.0)
                    .map(|_| self.capacity(index));
                (capacity, false)
            })
            .collect();
        let member_radius = self.avatar_radius()
            * slots::MEMBER_SCALE
            * framing::WORLD_UNITS_PER_REFERENCE_PIXEL;
        for (index, avatar) in self.avatars.iter().enumerate() {
            let Some(lead) = avatar.group.map(|group| group.lead) else {
                continue;
            };
            let walking_together =
                self.avatars[lead].travelling && self.avatars[lead].ring_presence > 0.0;
            if avatar.presence <= 0.0
                || avatar.travelling && !walking_together
                || !self.in_party(index, lead)
            {
                continue;
            }
            if index != lead {
                capacities[lead].1 = true;
            }
            let ring = if walking_together {
                self.ring_geometry(lead, self.capacity(lead))
            } else {
                self.ring_of(index)
            };
            if ring.is_some_and(|(centre, rim)| {
                avatar.position.distance(centre) + member_radius > rim
            }) {
                capacities[lead].0 = None;
            }
        }
        for (avatar, (capacity, has_partner)) in self.avatars.iter_mut().zip(capacities)
        {
            avatar.update_ring(dt, capacity.filter(|_| has_partner));
        }
    }

    fn update_ray_sources(&mut self, now: Time) {
        let mut used = vec![false; self.avatars.len()];
        for pulse in &self.pulses {
            if now < pulse.end
                && let Some(RaySource::Ring(lead)) = pulse.source
            {
                used[lead] = true;
            }
        }
        for avatar in &mut self.avatars {
            avatar.pending_arrivals.retain(|(_, start)| now < *start);
            avatar.ring_arrivals.retain(|flight| now <= flight.land);
            for flight in &avatar.ring_arrivals {
                used[flight.lead] = true;
            }
        }
        for (avatar, used) in self.avatars.iter_mut().zip(used) {
            if !used {
                avatar.casting_ring = None;
            }
        }
        for index in 0..self.pulses.len() {
            let pulse = &self.pulses[index];
            let author = pulse.author;
            if pulse.source.is_some()
                || now < pulse.start
                || !self.avatars[author].transmitting
            {
                continue;
            }
            let group = pulse.group.or_else(|| {
                let owner = self.avatars[author]
                    .group
                    .map_or(author, |group| group.lead);
                self.avatars[owner].casting_ring.is_some().then_some(owner)
            });
            let source = group
                .and_then(|lead| self.lock_source_ring(author, lead, now))
                .map_or(RaySource::Avatar, RaySource::Ring);
            let pulse = &mut self.pulses[index];
            pulse.source = Some(source);
            if let Some(land) = pulse.arrivals_land {
                match source {
                    RaySource::Ring(lead) => {
                        self.avatars[author].ring_arrivals.push(ArrivalsOnRing {
                            revisions: pulse.arrival_revisions.clone(),
                            land,
                            lead,
                        });
                    }
                    RaySource::Avatar => {
                        let avatar = &mut self.avatars[author];
                        avatar.body_arrivals_land = avatar.body_arrivals_land.max(land);
                    }
                }
            }
        }
    }

    fn lock_source_ring(
        &mut self,
        author: usize,
        lead: usize,
        now: Time,
    ) -> Option<usize> {
        let repository = self.avatars[author].repository;
        if !self.in_party(author, lead) {
            return None;
        }
        let avatar = &self.avatars[lead];
        let dock = avatar.slot.as_ref()?.dock;
        if let Some(ring) = avatar.casting_ring {
            return (ring.repository == repository && ring.dock == dock)
                .then_some(lead);
        }
        if avatar.ring_presence < 1.0
            || avatar.travelling
            || avatar.repository != repository
        {
            return None;
        }
        if self.body_rays_overlap_group(lead, now) {
            return None;
        }
        let (centre, radius) = self.visible_ring(lead)?;
        let gutter = self.layout.repositories[repository].gutter?;
        let offset = centre - slots::world(dock.on_network(gutter));
        self.avatars[lead].casting_ring = Some(CastingRing {
            repository,
            dock,
            offset,
            radius,
        });
        Some(lead)
    }

    fn body_rays_overlap_group(&self, lead: usize, now: Time) -> bool {
        self.pulses.iter().any(|pulse| {
            matches!(pulse.source, Some(RaySource::Avatar))
                && now < pulse.end
                && self.avatars[pulse.author].transmitting
                && self.in_party(pulse.author, lead)
        }) || self.avatars.iter().enumerate().any(|(index, avatar)| {
            avatar.transmitting
                && now <= avatar.body_arrivals_land
                && self.in_party(index, lead)
        })
    }

    fn avatar_radius(&self) -> f32 {
        6.5 * self.avatar_scale * self.avatar_size * ARRIVAL_SCALE + 1.0
    }

    fn author(&mut self, contributor: &Contributor, now: Time) -> usize {
        if let Some(&index) = self.authors.get(&contributor.key) {
            return index;
        }
        let index = self.avatars.len();
        self.avatars.push(Avatar {
            expires: now + AVATAR_IDLE,
            arrived: now,
            lifecycle_scale: ARRIVAL_SCALE,
            grey: 0.0,
            key: contributor.key.clone(),
            name: contributor.name.clone(),
            idle_after: if contributor.is_bot() {
                self.fragment_pace.pulse_end(Time::ZERO) - Time::ZERO
                    + DISBAND
                    + tick_seconds()
            } else {
                f32::INFINITY
            },
            idle_opacity: 1.0,
            portrait: contributor.portrait.clone(),
            position: self.focus + Vec3::Y * MIN_AVATAR_HEIGHT,
            target: self.focus,
            repository: 0,
            last_transfer_revision: 0,
            pending_arrivals: Vec::new(),
            arrivals_land: Time::NEG_INFINITY,
            body_arrivals_land: Time::NEG_INFINITY,
            touched: Vec::new(),
            last_active: now,
            presence: 0.0,
            slot_presence: 0.0,
            projection_scale: 1.0,
            activity: 0.0,
            name_visibility: names::Visibility::default(),
            commit: 0,
            slot: None,
            dock_since: 0,
            dock_walk: None,
            gutter_entry: None,
            slot_origin: None,
            group: None,
            dismissed: false,
            ring_presence: 0.0,
            ring_capacity: 1,
            ring_arrivals: Vec::new(),
            casting_ring: None,
            settle: None,
            settled: 1.0,
            leaving_name: None,
            seat: Vec2::ZERO,
            travelling: false,
            transmitting: false,
        });
        self.authors.insert(contributor.key.clone(), index);
        index
    }

    /// Avatar indices of the event's author, then its co-authors.
    fn participants(&mut self, event: &Event, now: Time) -> Vec<usize> {
        assert!(event.contributor.is_some(), "contributors are resolved");
        event
            .participants()
            .map(|contributor| self.author(contributor, now))
            .collect()
    }

    /// Applies the event's changes for its author and gathers every participant into
    /// one group, whose single pulse belongs to the author.
    fn begin(
        &mut self,
        event: &Event,
        start: Time,
        file_ids: &mut HashMap<String, FileIdentity>,
    ) {
        let participants = self.participants(event, start);
        let author = participants[0];
        let groups = self.seat_groups(&participants, event.repository);
        let lead = groups[0].lead;
        let group = groups[1..]
            .iter()
            .any(|group| group.lead == lead)
            .then_some(lead);
        let prelude = if group.is_some() && self.avatars[lead].ring_presence < 1.0 {
            DISBAND + tick_seconds()
        } else {
            0.0
        };
        let arrival_revision = self.layout.revision();
        let mut touched = BTreeMap::new();
        let mut samples = Vec::new();
        for change in &event.changes {
            let id = apply_with_delay(&mut self.layout, change, 0, author, prelude);
            let identity = FileIdentity::Unique(id);
            file_ids
                .entry(change.path.clone())
                .and_modify(|seen| {
                    if *seen != identity {
                        *seen = FileIdentity::Ambiguous;
                    }
                })
                .or_insert(identity);
            touched.insert(id, (change.insertions, change.removals));
            for sample in change.samples.iter().take(crate::diff_samples::PER_FILE) {
                if samples.len() < crate::diff_samples::PER_COMMIT {
                    samples.push((id, sample.clone()));
                }
            }
        }
        let mut targets: Vec<_> = touched
            .iter()
            .map(|(&id, &(insertions, removals))| {
                (Target::File(id), 1.0, insertions, removals)
            })
            .collect();
        let arriving_directories =
            self.arriving_directories(&touched, arrival_revision);
        let arrivals = !arriving_directories.is_empty()
            || touched
                .keys()
                .any(|&id| self.layout.new_file_arrival(id, arrival_revision));
        if arrivals && prelude > 0.0 {
            self.layout.delay_new_arrivals(
                touched.keys().copied(),
                arriving_directories.iter().copied(),
                arrival_revision,
                prelude,
            );
        }
        let aggregated = targets.len() + arriving_directories.len() > BEAM_LIMIT;
        if aggregated {
            targets = self.aggregate(&touched);
            self.route_arrivals(
                arrival_revision,
                &touched,
                &targets,
                &arriving_directories,
            );
        }
        let centroid = targets
            .iter()
            .filter_map(|&(target, ..)| self.destination(target))
            .fold((Vec2::ZERO, 0.0), |(sum, n), point| (sum + point, n + 1.0));
        let target = (centroid.1 > 0.0).then(|| ground(centroid.0 / centroid.1));
        let touched: Vec<u64> = touched.into_keys().collect();
        self.assign_participants(
            participants.into_iter().zip(groups),
            event.repository,
            start,
            arrival_revision,
            &touched,
            target,
        );
        let start = start + prelude;
        let revisions = (arrival_revision + 1)..=self.layout.revision();
        let arrives_by = arrivals.then(|| {
            let pending = (prelude > 0.0).then(|| revisions.clone());
            self.avatars[author].record_arrivals(start, pending)
        });
        self.pulses.push_back(Pulse {
            commit: self.next_event,
            repository: event.repository,
            touched,
            start,
            end: self.fragment_pace.pulse_end(start),
            author,
            group,
            source: None,
            arrival_revisions: revisions,
            arrivals_land: arrives_by,
            targets,
            aggregated,
            samples,
        });
        self.capacities = self.compute_capacities();
    }

    fn assign_participants(
        &mut self,
        participants: impl Iterator<Item = (usize, Group)>,
        repository: usize,
        start: Time,
        arrival_revision: u64,
        touched: &[u64],
        target: Option<Vec3>,
    ) {
        let flight = self.fragment_pace.flight();
        for (member, group) in participants {
            let avatar = &mut self.avatars[member];
            if avatar.repository != repository {
                avatar.last_transfer_revision = arrival_revision;
                avatar.pending_arrivals.clear();
                avatar.arrivals_land = Time::NEG_INFINITY;
                avatar.body_arrivals_land = Time::NEG_INFINITY;
                avatar.transmitting = false;
                avatar.ring_arrivals.clear();
                avatar.dock_walk = None;
                avatar.settle = None;
                avatar.slot_origin = avatar
                    .slot
                    .take()
                    .map(|s| (avatar.repository, s.dock, s.point));
            }
            avatar.repository = repository;
            avatar.commit = self.next_event;
            avatar.touched.clear();
            avatar.touched.extend_from_slice(touched);
            if let Some(target) = target {
                avatar.target = target;
            }
            if avatar.presence <= 0.0 {
                avatar.position = avatar.target + Vec3::Y * MIN_AVATAR_HEIGHT;
            }
            avatar.last_active = start;
            avatar.activity = (avatar.activity + 1.0).min(1000.0);
            avatar.group = Some(group);
            avatar.dismissed = false;
            self.retire_previous_pulses(member, repository, start + flight + 0.3);
        }
    }

    fn retire_previous_pulses(&mut self, author: usize, repository: usize, end: Time) {
        let previous = self.pulses.iter().rposition(|pulse| {
            pulse.author == author && pulse.repository == repository
        });
        let mut index = 0;
        let now = tick_time(self.tick);
        self.pulses.retain_mut(|pulse| {
            let keep = pulse.author != author
                || Some(index) == previous
                || pulse.repository == repository && pulse.awaits_arrival_source(now);
            if pulse.author == author {
                pulse.end = pulse.end.min(end);
            }
            index += 1;
            keep
        });
    }

    /// Keep parked participants at their docks, and seat newcomers with the author
    /// in the lowest free seats.
    fn seat_groups(&mut self, participants: &[usize], repository: usize) -> Vec<Group> {
        let author = participants[0];
        let parked = |index: usize| {
            let avatar = &self.avatars[index];
            avatar.slot.is_some() && avatar.repository == repository
        };
        if let Some(group) = self.avatars[author].group
            && parked(group.lead)
            && participants
                .iter()
                .all(|&member| parked(member) && self.in_party(member, group.lead))
        {
            return participants
                .iter()
                .map(|&member| self.avatars[member].group.unwrap())
                .collect();
        }
        let retained: Vec<_> = participants[1..]
            .iter()
            .map(|&member| {
                (parked(member) && !self.in_party(member, author)).then(|| {
                    self.avatars[member]
                        .group
                        .filter(|group| {
                            !participants.contains(&group.lead)
                                && self.in_party(member, group.lead)
                        })
                        .unwrap_or(Group {
                            lead: member,
                            seat: 0,
                        })
                })
            })
            .collect();
        let mut stay = Vec::new();
        for index in 0..self.avatars.len() {
            let Some(group) = self.avatars[index].group else {
                continue;
            };
            if group.lead == index
                || participants.contains(&index)
                || !participants.contains(&group.lead)
            {
                continue;
            }
            if group.lead == author
                && self.in_party(index, author)
                && self.avatars[index].repository == repository
            {
                stay.push(index);
            } else {
                self.avatars[index].group = None;
            }
        }
        let joining = retained.iter().filter(|group| group.is_none()).count();
        if stay.len() + joining > crate::timeline::MAX_CO_AUTHORS {
            for index in stay.drain(..) {
                self.avatars[index].group = None;
            }
        }
        let kept = |state: &Self, member: usize| {
            state
                .in_party(member, author)
                .then(|| state.avatars[member].group.map(|g| g.seat))
                .flatten()
        };
        let mut taken: Vec<u8> = stay
            .iter()
            .chain(&participants[1..])
            .filter_map(|&member| kept(self, member))
            .chain(std::iter::once(0))
            .collect();
        std::iter::once(Group {
            lead: author,
            seat: 0,
        })
        .chain(
            participants[1..]
                .iter()
                .zip(retained)
                .map(|(&member, retained)| {
                    retained.unwrap_or_else(|| {
                        let seat = kept(self, member).unwrap_or_else(|| {
                            let seat = (1..=u8::MAX)
                                .find(|seat| !taken.contains(seat))
                                .expect("groups hold at most 9 members");
                            taken.push(seat);
                            seat
                        });
                        Group { lead: author, seat }
                    })
                }),
        )
        .collect()
    }

    fn aggregate(
        &self,
        touched: &BTreeMap<u64, (u32, u32)>,
    ) -> Vec<(Target, f32, u32, u32)> {
        let mut groups: BTreeMap<usize, (f32, u32, u32)> = BTreeMap::new();
        for (&id, &(insertions, removals)) in touched {
            if let Some(file) = self.layout.file(id) {
                let entry = groups.entry(file.dir).or_default();
                *entry = (entry.0 + 1.0, entry.1 + insertions, entry.2 + removals);
            }
        }
        while groups.len() > BEAM_LIMIT {
            let deepest = groups
                .keys()
                .map(|&dir| self.layout.dir(dir).depth)
                .max()
                .unwrap_or(0);
            if deepest == 0 {
                break;
            }
            let mut lifted = BTreeMap::new();
            for (dir, (weight, insertions, removals)) in groups {
                let entry = self.layout.dir(dir);
                let key = if entry.depth == deepest {
                    entry.parent.unwrap_or(dir)
                } else {
                    dir
                };
                let total: &mut (f32, u32, u32) = lifted.entry(key).or_default();
                *total = (total.0 + weight, total.1 + insertions, total.2 + removals);
            }
            groups = lifted;
        }
        groups
            .into_iter()
            .map(|(dir, (weight, insertions, removals))| {
                (Target::Dir(dir), weight, insertions, removals)
            })
            .collect()
    }

    fn route_arrivals(
        &mut self,
        since: u64,
        touched: &BTreeMap<u64, (u32, u32)>,
        targets: &[(Target, f32, u32, u32)],
        arriving_directories: &BTreeSet<usize>,
    ) {
        let mut routes: BTreeMap<usize, Vec<(u64, usize, bool)>> = BTreeMap::new();
        let keep_directory_rays = arriving_directories.len() > BEAM_LIMIT;
        for &id in touched.keys() {
            let Some(file) = self.layout.file(id) else {
                continue;
            };
            let arriving = self.layout.new_file_arrival(id, since);
            if !arriving && !arriving_directories.contains(&file.dir) {
                continue;
            }
            let mut directory = file.dir;
            while !(keep_directory_rays && arriving_directories.contains(&directory))
                && targets
                    .binary_search_by_key(&Target::Dir(directory), |entry| entry.0)
                    .is_err()
            {
                let Some(parent) = self.layout.dir(directory).parent else {
                    break;
                };
                directory = parent;
            }
            routes
                .entry(directory)
                .or_default()
                .push((id, file.dir, arriving));
        }
        for (directory, files) in routes {
            let count = files.iter().filter(|(_, _, arriving)| *arriving).count();
            #[expect(
                clippy::cast_precision_loss,
                reason = "the offset is an animation fraction"
            )]
            let gaps = count.saturating_sub(1).max(1) as f32;
            let spacing = ARRIVAL_SPACING.min(MAX_ARRIVAL_OFFSET / gaps);
            let mut offset = 0.0_f32;
            for (id, parent, arriving) in files {
                let flight = crate::layout::DirectoryFlight { directory, offset };
                self.layout.route_arrival(
                    id,
                    since,
                    parent,
                    flight,
                    keep_directory_rays,
                );
                if arriving {
                    offset = (offset + spacing).min(MAX_ARRIVAL_OFFSET);
                }
            }
        }
    }

    fn arriving_directories(
        &self,
        touched: &BTreeMap<u64, (u32, u32)>,
        since: u64,
    ) -> BTreeSet<usize> {
        let mut directories = BTreeSet::new();
        for file in touched.keys().filter_map(|&id| self.layout.file(id)) {
            let mut directory = file.dir;
            while self.layout.new_directory_arrival(directory, since)
                && directories.insert(directory)
            {
                let Some(parent) = self.layout.dir(directory).parent else {
                    break;
                };
                directory = parent;
            }
        }
        directories
    }

    fn destination(&self, target: Target) -> Option<Vec2> {
        match target {
            Target::File(id) => self.layout.target(id),
            Target::Dir(index) => Some(self.layout.dir(index).position),
        }
    }

    /// The ring an avatar sits in now, as its world centre and radius, while its group
    /// has two or more seats.
    fn ring_of(&self, index: usize) -> Option<(Vec3, f32)> {
        let lead = self.avatars[index]
            .group
            .map(|g| g.lead)
            .filter(|&lead| self.in_party(index, lead))
            .unwrap_or(index);
        let capacity = self.capacity(lead);
        let author = &self.avatars[lead];
        let slot = author.slot.as_ref().filter(|_| capacity > 1)?;
        let radius = self.avatar_radius();
        let centre = slot.ring_centre(capacity, radius);
        let edge = centre + Vec2::X * slots::group_radius(capacity, radius);
        Some((
            slots::world(centre) + author.ring_shift(),
            slots::world(edge).distance(slots::world(centre)),
        ))
    }

    /// The enclosing world centre and radius of a ring, independent of its opacity.
    fn visible_ring(&self, lead: usize) -> Option<(Vec3, f32)> {
        let author = &self.avatars[lead];
        if let Some(ring) = author.casting_ring {
            let gutter = self.layout.repositories[ring.repository].gutter?;
            return Some((
                slots::world(ring.dock.on_network(gutter)) + ring.offset,
                ring.radius,
            ));
        }
        (author.ring_presence * author.presence > 0.0)
            .then(|| self.ring_geometry(lead, author.ring_capacity))
            .flatten()
    }

    fn ring_geometry(&self, lead: usize, capacity: u8) -> Option<(Vec3, f32)> {
        let author = &self.avatars[lead];
        let slot = author.slot.as_ref()?;
        let radius = self.avatar_radius();
        let centre = slot.ring_centre(capacity, radius);
        let edge = centre + Vec2::X * slots::group_radius(capacity, radius);
        let rim = slots::world(edge).distance(slots::world(centre));
        let shift = if author.travelling {
            author.position - slot.world()
        } else {
            author.ring_shift()
        };
        Some((slots::world(centre) + shift, rim))
    }

    /// The visible ring an avatar's beams leave from: its group's, or its own.
    fn beam_ring(&self, index: usize) -> Option<(Vec3, f32)> {
        let lead = self.avatars[index]
            .group
            .map(|g| g.lead)
            .filter(|&lead| self.in_party(index, lead))
            .unwrap_or(index);
        self.visible_ring(lead)
    }

    fn ray_source_ring(&self, source: RaySource) -> Option<(Vec3, f32)> {
        match source {
            RaySource::Avatar => None,
            RaySource::Ring(lead) => self.visible_ring(lead),
        }
    }

    /// Where an avatar's beam towards `to` starts: the point of its visible ring nearest
    /// `to` as seen from the camera, or the avatar itself outside a ring.
    fn beam_start(&self, author: usize, to: Vec3) -> Vec3 {
        self.beam_start_from(author, to, self.beam_ring(author))
    }

    fn beam_start_from(
        &self,
        author: usize,
        to: Vec3,
        ring: Option<(Vec3, f32)>,
    ) -> Vec3 {
        let Some((centre, rim)) = ring else {
            return self.avatars[author].position;
        };
        let view = view_direction();
        let across = to - centre;
        centre + (across - view * across.dot(view)).normalize_or_zero() * rim
    }

    /// Seats in an author's group, 1 for an author alone.
    fn capacity(&self, lead: usize) -> u8 {
        self.capacities.get(lead).copied().unwrap_or(1)
    }

    /// Members of the event author's group who take no part in the event.
    fn left_behind(&self, event: &Event) -> Vec<usize> {
        let Some(&author) = event
            .contributor
            .as_ref()
            .and_then(|contributor| self.authors.get(&contributor.key))
        else {
            return Vec::new();
        };
        (0..self.avatars.len())
            .filter(|&i| {
                i != author
                    && self.in_party(i, author)
                    && event.participants().all(|c| c.key != self.avatars[i].key)
            })
            .collect()
    }

    /// World distance within which an avatar walks to a new seat rather than jumping.
    fn settle_reach(&self) -> f32 {
        let span = Vec2::X * 4.0 * self.avatar_radius();
        slots::world(span).distance(slots::world(Vec2::ZERO))
    }

    /// Display scale of each avatar: smaller while packed into a group or fading with one.
    pub(super) fn member_scales(&self) -> Vec<f32> {
        (0..self.avatars.len())
            .map(|index| {
                let lead = self.avatars[index].group.map_or(index, |g| g.lead);
                if self.avatars[index].dismissed
                    || self.capacity(lead) > 1
                        && self.in_party(index, lead)
                        && self.ring_of(index).is_some_and(|(centre, rim)| {
                            self.avatars[index].position.distance(centre) < rim
                        })
                {
                    slots::MEMBER_SCALE
                } else {
                    1.0
                }
            })
            .collect()
    }

    fn point(&self, target: Target) -> Option<Vec3> {
        match target {
            Target::File(id) => self.layout.file(id).map(|file| ground(file.position)),
            Target::Dir(index) => Some(ground(self.layout.dir(index).position)),
        }
    }

    fn overview(&self) -> (Vec3, f32) {
        let tile = crate::layout::packing::Tile::viewport();
        let centre = framing::field_to_world(tile.low.midpoint(tile.high));
        let focus = self.focus + ground(centre * (1.0 - self.field_growth));
        let distance = (self.distance - framing::FIELD_HEIGHT) * self.field_growth
            + framing::FIELD_HEIGHT;
        (focus, distance)
    }

    fn scale(&self) -> f32 {
        1.0 + self.distance / 60.0
    }
}

pub(crate) fn apply(
    layout: &mut Layout,
    change: &crate::timeline::FileChange,
    author: usize,
) -> u64 {
    apply_within(layout, change, 0, author)
}

/// Applies `change` with the first `skip` bytes of its paths, a repository prefix,
/// removed.
pub(crate) fn apply_within(
    layout: &mut Layout,
    change: &crate::timeline::FileChange,
    skip: usize,
    author: usize,
) -> u64 {
    apply_with_delay(layout, change, skip, author, 0.0)
}

fn apply_with_delay(
    layout: &mut Layout,
    change: &crate::timeline::FileChange,
    skip: usize,
    author: usize,
    removal_delay: f32,
) -> u64 {
    let path = &change.path[skip..];
    let id = match &change.kind {
        Kind::Added => layout.add(path, ADD_DELAY, author),
        Kind::Deleted => layout
            .remove(path, DELETE_DELAY + removal_delay, author)
            .unwrap_or_else(|| layout.touch(path, author)),
        Kind::Modified => layout.touch(path, author),
        Kind::Moved { from } => layout.rename(&from[skip..], path, author),
    };
    let lines = f64::from(change.insertions) + f64::from(change.removals);
    #[expect(clippy::cast_possible_truncation, reason = "heat is bounded to 0.75")]
    let heat = (0.12 + 0.08 * lines.ln_1p()).min(0.75) as f32;
    if !matches!(change.kind, Kind::Deleted) {
        layout.heat(id, heat);
    }
    id
}

pub(super) fn view_direction() -> Vec3 {
    Vec3::Y
}

#[cfg(test)]
pub(crate) fn box_corners(low: Vec3, high: Vec3) -> [Vec3; 8] {
    [
        low,
        Vec3::new(low.x, low.y, high.z),
        Vec3::new(low.x, high.y, low.z),
        Vec3::new(low.x, high.y, high.z),
        Vec3::new(high.x, low.y, low.z),
        Vec3::new(high.x, low.y, high.z),
        Vec3::new(high.x, high.y, low.z),
        high,
    ]
}

/// Projects points to normalised screen coordinates, or `None` for one within 0.5 of
/// the eye plane or behind it.
fn project(
    eye: Vec3,
    focus: Vec3,
    points: &[Vec3],
) -> impl Iterator<Item = Option<Vec2>> {
    let forward = (focus - eye).normalize();
    let right = Vec3::X;
    let up = right.cross(forward);
    let half_height = (std::f32::consts::FRAC_PI_8).tan();
    points.iter().map(move |&point| {
        let offset = point - eye;
        let depth = offset.dot(forward);
        (depth > 0.5).then(|| {
            Vec2::new(
                offset.dot(right) / (depth * half_height * 16.0 / 9.0),
                offset.dot(up) / (depth * half_height),
            )
        })
    })
}

/// Up to three pieces per direction, descending along the beam or rising from the file.
fn diff_fragments(
    age: f32,
    envelope: f32,
    beam: (BeamStyle, Vec3, Vec3),
    change: (u32, bool),
    pace: FragmentPace,
) -> impl Iterator<Item = Fragment> {
    let (lines, rising) = change;
    let pieces = lines.bit_width().min(3);
    (0..pieces).map(move |piece| {
        let phase = (age * pace.speed + count_u32(piece) / count_u32(pieces)).fract();
        let (style, from, to) = beam;
        let position = style.point(from, to, if rising { 1.0 - phase } else { phase });
        Fragment {
            #[cfg(test)]
            beam: None,
            position,
            rising,
            sample_detail: 1.0,
            alpha: if rising {
                let birth = (phase / 0.08).min(1.0);
                envelope * birth * birth * (3.0 - 2.0 * birth) * (1.0 - phase).powi(2)
            } else {
                envelope * (1.0 - phase.powi(6))
            },
            sample: None,
            scale: envelope * (1.0 - phase.powi(6)),
        }
    })
}

fn envelope(age: f32, length: f32) -> f32 {
    let rise = (age / 0.15).clamp(0.0, 1.0);
    let fall = ((length - age) / 0.3).clamp(0.0, 1.0);
    rise.min(fall)
}

fn ground(point: Vec2) -> Vec3 {
    Vec3::new(point.x, 0.0, point.y)
}

pub(super) fn tick_seconds() -> f32 {
    1.0 / count_u32(TICK_RATE)
}

/// The length of `ticks` simulation ticks in seconds.
pub(crate) fn ticks_duration(ticks: u64) -> f32 {
    let whole = u32::try_from(ticks / u64::from(TICK_RATE)).unwrap_or(u32::MAX);
    let part =
        u32::try_from(ticks % u64::from(TICK_RATE)).expect("remainder fits in u32");
    count_u32(whole) + count_u32(part) * tick_seconds()
}

pub(crate) fn tick_time(tick: u64) -> Time {
    Time::from_tick(tick)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "commit counts beyond 2^24 only lose sub-commit timing precision"
)]
pub(crate) fn count(value: usize) -> f32 {
    value as f32
}

#[expect(
    clippy::cast_precision_loss,
    reason = "tick and piece counts beyond 2^24 only lose sub-tick precision"
)]
pub(crate) fn count_u32(value: u32) -> f32 {
    value as f32
}

/// Formats Unix seconds as a UTC calendar date, using Howard Hinnant's civil algorithm.
pub(super) fn date(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524
        - day_of_era / 146_096)
        / 365;
    let day_of_year =
        day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "frame count is validated before conversion"
)]
pub(super) fn frames_for(duration: Time, fps: u16) -> anyhow::Result<u32> {
    let frames = (duration.as_secs_f64() * f64::from(fps)).ceil();
    if !(frames.is_finite() && frames >= 0.0 && frames <= f64::from(u32::MAX)) {
        return Err(beyond_export_range());
    }
    Ok((frames as u32).max(2))
}

/// Frames covering `ticks` simulation ticks at `fps`, rounded up.
pub(crate) fn frames_for_ticks(ticks: u64, fps: u16) -> anyhow::Result<u32> {
    let frames = (u128::from(ticks) * u128::from(fps)).div_ceil(u128::from(TICK_RATE));
    let frames = u32::try_from(frames).map_err(|_| beyond_export_range())?;
    Ok(frames.max(2))
}

fn beyond_export_range() -> anyhow::Error {
    anyhow::anyhow!(
        "whole playback exceeds the supported export range of {} frames; use --frames or reduce duration or fps",
        u32::MAX
    )
}

pub(super) fn preview_frame(elapsed: Duration, fps: u16) -> u32 {
    let frame = elapsed.as_nanos() * u128::from(fps) / 1_000_000_000;
    u32::try_from(frame).unwrap_or(u32::MAX)
}

fn prior_activity(events: &[Event], count: usize) -> Vec<bool> {
    (0..count)
        .map(|repository| events.iter().any(|e| e.repository == repository))
        .collect()
}

#[cfg(test)]
mod tests {
    thread_local! {
        pub(super) static LAYOUT_COUNTS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
    }

    #[test]
    fn a_shallow_view_aims_at_the_target_as_seen_from_the_ring() {
        let view = super::RayView::new(Vec3::ZERO, Vec3::Z);
        let centre = Vec3::new(2.0, 0.0, 4.0);
        let target = Vec3::new(3.0, 0.0, 12.0);

        assert_eq!(view.towards(centre, target), Vec3::NEG_X);
    }

    #[test]
    fn a_visible_coauthor_circle_encloses_its_seated_members() {
        let mut event = crate::timeline::synthetic().remove(0);
        event.co_authors.push(crate::timeline::Identity {
            name: b"Pair".to_vec(),
            email: b"pair@example.invalid".to_vec(),
        });
        let mut sim = Simulation::new(
            vec![event],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let mut visible = false;
        for frame in 0..sim.frames(60).unwrap() {
            sim.advance_to(frame, 60);
            for (_, centre, radius, _) in sim.groups() {
                visible = true;
                assert!(
                    sim.avatars()
                        .iter()
                        .all(|avatar| avatar.position.distance(centre) <= radius),
                    "frame {frame}: ring does not enclose both contributors",
                );
            }
        }
        assert!(visible);
    }

    #[test]
    fn idle_bot_fades_during_an_unrelated_contributors_history() {
        let template = crate::timeline::synthetic().remove(0);
        let mut events = vec![template.clone()];
        events[0].author = crate::timeline::Identity {
            name: b"helper[bot]".to_vec(),
            email: b"helper@example.invalid".to_vec(),
        };
        for day in 1..=12 {
            let mut event = template.clone();
            event.seconds += day * 86_400;
            event.commit = format!("later-{day}");
            events.push(event);
        }
        let mut sim = Simulation::new(
            events,
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let mut cast = false;
        for frame in 0..sim.frames(60).unwrap() {
            sim.advance_to(frame, 60);
            for beam in sim.beams().into_iter().filter(|beam| beam.author == 0) {
                cast = true;
                assert!(sim.avatars()[beam.author].presence > 0.0);
            }
        }
        assert!(cast);
        assert_eq!(sim.avatars()[0].display_presence(), 0.0);
        assert!(sim.avatars()[0].presence > 0.0);
        assert!(sim.avatars()[1].display_presence() > 0.0);
    }

    #[test]
    fn a_visible_circle_never_draws_a_solo_ray_from_inside_its_rim() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        events[0].co_authors.push(crate::timeline::Identity {
            name: b"Pair".to_vec(),
            email: b"pair@example.invalid".to_vec(),
        });
        let mut sim = Simulation::new(
            events,
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let normal = -super::view_direction();
        let mut solo_rays = 0;
        let mut paired_rays = 0;
        for frame in 0..sim.frames(60).unwrap() {
            sim.advance_to(frame, 60);
            let (focus, distance) = sim.view();
            sim.set_ray_view(super::RayView::new(focus - normal * distance, normal));
            let circle = sim.groups().first().map(|ring| (ring.1, ring.2));
            for beam in sim
                .beams()
                .into_iter()
                .chain(sim.file_arrival_beams().map(|(_, beam)| beam))
            {
                let solo = sim.seconds() >= sim.schedule.starts[1];
                if solo {
                    solo_rays += 1;
                }
                if let Some((centre, radius)) = circle
                    && sim.avatars()[beam.author].position.distance(centre) < radius
                {
                    let spoke = beam.from - centre;
                    assert!(
                        spoke.dot(normal).abs() < 1e-4
                            && (spoke.length() - radius).abs() < 1e-4,
                        "frame {frame}"
                    );
                    paired_rays += usize::from(!solo);
                }
            }
        }
        assert!(solo_rays > 0 && paired_rays > 0);
    }

    #[test]
    fn a_superseded_commit_still_assigns_the_source_of_its_pending_arrivals() {
        let mut event = crate::timeline::synthetic().remove(0);
        event.co_authors.push(crate::timeline::Identity {
            name: b"Pair".to_vec(),
            email: b"pair@example.invalid".to_vec(),
        });
        let mut sim = Simulation::new(
            vec![event],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let first = super::frames_for(sim.schedule.starts[0], 60).unwrap();
        sim.advance_to(first, 60);
        let state = &mut *sim.state;
        let original = state.pulses.front().unwrap().clone();
        assert!(original.awaits_arrival_source(super::tick_time(state.tick)));
        let mut later = original.clone();
        later.commit += 1;
        later.arrivals_land = None;
        state.pulses.push_back(later);
        state.retire_previous_pulses(
            original.author,
            original.repository,
            original.end,
        );
        assert!(
            state
                .pulses
                .iter()
                .any(|pulse| pulse.commit == original.commit)
        );
    }

    #[test]
    fn a_new_arrival_waits_for_its_own_commit_when_a_ray_is_already_active() {
        let event = crate::timeline::synthetic().remove(0);
        let mut sim = Simulation::new(
            vec![event],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        for frame in 0..sim.frames(60).unwrap() {
            sim.advance_to(frame, 60);
            let arrival = sim.layout().arriving_files().find_map(|(id, file)| {
                let target = Target::File(id);
                let to = super::ground(file.position);
                sim.arrival_beam(target, 0, to, 0.25, None, file.arrival_revision)
                    .0
                    .map(|_| (target, to, file.arrival_revision))
            });
            if let Some((target, to, revision)) = arrival {
                let later = revision + 1;
                let start = sim.seconds() + super::DISBAND;
                sim.state.avatars[0].record_arrivals(start, Some(later..=later));
                assert!(
                    sim.arrival_beam(target, 0, to, 0.25, None, revision)
                        .0
                        .is_some()
                );
                assert!(
                    sim.arrival_beam(target, 0, to, 0.25, None, later)
                        .0
                        .is_none()
                );
                return;
            }
        }
        panic!("the first commit never had a file arrival ray");
    }

    #[test]
    fn a_new_circle_waits_for_existing_avatar_rays_to_finish() {
        let mut event = crate::timeline::synthetic().remove(0);
        event.co_authors.push(crate::timeline::Identity {
            name: b"Pair".to_vec(),
            email: b"pair@example.invalid".to_vec(),
        });
        let mut sim = Simulation::new(
            vec![event],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        for frame in 0..sim.frames(60).unwrap() {
            sim.advance_to(frame, 60);
            if sim
                .avatars()
                .first()
                .is_some_and(|author| author.casting_ring.is_some())
            {
                let now = sim.seconds();
                let state = &mut *sim.state;
                state.avatars[0].casting_ring = None;
                state.pulses[0].source = Some(super::RaySource::Avatar);
                assert!(state.lock_source_ring(0, 0, now).is_none());
                assert_eq!(sim.groups().as_slice(), &[]);
                return;
            }
        }
        panic!("the pair never formed a casting circle");
    }

    #[test]
    fn a_coauthor_circle_follows_members_who_walk_together() {
        let mut event = crate::timeline::synthetic().remove(0);
        event.co_authors.push(crate::timeline::Identity {
            name: b"Pair".to_vec(),
            email: b"pair@example.invalid".to_vec(),
        });
        let mut sim = Simulation::new(
            vec![event],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let mut established = false;
        for frame in 0..sim.frames(60).unwrap() {
            sim.advance_to(frame, 60);
            if sim
                .avatars()
                .first()
                .is_some_and(|avatar| avatar.ring_presence >= 1.0)
            {
                established = true;
                break;
            }
        }
        assert!(established);
        let state = &mut *sim.state;
        state.avatars[0].casting_ring = None;
        let (first, radius) = state.visible_ring(0).unwrap();
        for avatar in &mut state.avatars {
            avatar.transmitting = false;
            avatar.travelling = true;
            avatar.position += Vec3::X;
        }
        state.update_rings(tick_seconds());
        let (next, after) = state.visible_ring(0).unwrap();
        assert_eq!(state.avatars[0].ring_presence, 1.0);
        assert_eq!(after, radius);
        assert!(next.distance(first + Vec3::X) < 1e-4);
        state.avatars[1].position += Vec3::X * (radius * 3.0);
        state.update_rings(tick_seconds());
        assert!(state.avatars[0].ring_presence < 1.0);
    }

    #[test]
    fn a_casting_circle_keeps_its_radius_for_the_whole_flight() {
        let mut event = crate::timeline::synthetic().remove(0);
        event.co_authors.push(crate::timeline::Identity {
            name: b"Pair".to_vec(),
            email: b"pair@example.invalid".to_vec(),
        });
        let mut simulation = Simulation::new(
            vec![event],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let eye = Vec3::new(0.0, 12.0, 85.0);
        let normal = -eye.normalize();
        simulation.set_ray_view(super::RayView::new(eye, normal));
        let mut radius = None;
        let mut ring_arrivals = 0;
        for frame in 0..simulation.frames(60).unwrap() {
            simulation.advance_to(frame, 60);
            if let Some(pulse) = simulation.state.pulses.front()
                && let Some(super::RaySource::Ring(lead)) = pulse.source
            {
                let (centre, rim) = simulation.state.visible_ring(lead).unwrap();
                assert_eq!(simulation.state.avatars[lead].ring_presence, 1.0);
                assert_eq!(rim, *radius.get_or_insert(rim), "frame {frame}");
                for (_, beam) in simulation.file_arrival_beams() {
                    let spoke = beam.from - centre;
                    assert!(spoke.dot(normal).abs() < 1e-4);
                    assert!((spoke.length() - rim).abs() < 1e-4);
                    ring_arrivals += 1;
                }
            }
        }
        assert!(radius.is_some());
        assert!(ring_arrivals > 0);
        assert!(simulation.state.avatars[0].casting_ring.is_none());
    }

    #[test]
    fn parked_avatars_follow_their_berth_while_the_tile_resizes() {
        let mut simulation = Simulation::two_synthetic_repositories();
        simulation.advance_to(75, 60);
        for (translation, length_scale) in [(0.2, 1.0), (0.0, 0.5), (0.0, 1.5)] {
            let mut state = (*simulation.state).clone();
            let mut regions = state.layout.repositories.clone();
            let author = state.pulses.front().unwrap().author;
            let avatar = &mut state.avatars[author];
            let gutter = regions[avatar.repository].gutter.as_mut().unwrap();
            let slot = avatar.slot.as_mut().unwrap();
            slot.dock = slots::Dock::new(1, 0.25);
            slot.point = slot.dock.on_network(*gutter) + slot.offset;
            avatar.position = slot.world();
            let (dock, offset) = (slot.dock, slot.offset);
            avatar.transmitting = true;
            avatar.travelling = false;
            avatar.dock_walk = None;
            avatar.settle = None;
            let previous = avatar.position;
            let mut walking = avatar.clone();
            walking.transmitting = false;
            walking.travelling = true;
            let mut parked = avatar.clone();
            parked.transmitting = false;
            let original_length = gutter.high.y - gutter.low.y;
            gutter.low += Vec2::splat(translation);
            gutter.high += Vec2::new(
                translation,
                translation + original_length * (length_scale - 1.0),
            );
            let expected = slots::world(dock.on_network(*gutter) + offset);
            let parked_id = state.avatars.len();
            state.avatars.extend([parked, walking]);

            state.carry_docked_avatars(&regions, &travel::Plan::default());

            assert_ne!(expected, previous);
            assert!(state.avatars[author].position.distance(expected) < 1e-5);
            assert!(state.avatars[parked_id].position.distance(expected) < 1e-5);
            assert_eq!(state.avatars[parked_id + 1].position, previous);
        }
    }

    #[test]
    fn arrivals_stay_on_their_rays_during_a_many_tile_opening() {
        let mut events = Vec::new();
        let count = 12;
        let opening = (0..count).map(|repository| (false, repository));
        let revisits = (0..count)
            .step_by(2)
            .chain((1..count).rev().step_by(2))
            .map(|repository| (true, repository));
        let identity = |name: String| crate::timeline::Identity {
            email: format!("{}@example.invalid", name.replace(' ', "-")).into(),
            name: name.into(),
        };
        for (revisit, repository) in opening.chain(revisits) {
            let mut event = crate::timeline::synthetic().remove(0);
            event.seconds = i64::try_from(events.len()).unwrap() * 86_400;
            event.commit = format!("commit-{}", events.len());
            event.author = identity(if revisit {
                "Rapid".into()
            } else {
                format!("Seed {repository}")
            });
            if revisit && repository % 3 < 2 {
                let mut bot = identity("GitHub Actions[bot]".into());
                if repository % 3 == 0 {
                    std::mem::swap(&mut event.author, &mut bot);
                }
                event.co_authors = vec![bot];
            }
            event.changes[0].path = format!("round-{}/new.rs", u8::from(revisit));
            crate::repositories::namespace(
                std::slice::from_mut(&mut event),
                repository,
            );
            events.push(event);
        }
        let labels: Vec<_> = (0..count)
            .map(|repository| format!("repo-{repository}"))
            .collect();
        let mut simulation = Simulation::with_repositories(
            events,
            &Options {
                seconds_per_day: 0.02,
                max_idle_seconds: 0.05,
                travel_cap: TravelCap::Seconds(0.12),
                bot_travel: BotTravel::Fade,
                fragment_speed: 0.25,
                ..Options::default()
            },
            &Theme::neon(),
            &Mappings::default(),
            &labels,
        );
        let mut flights = 0;
        for frame in 0..simulation.frames(60).unwrap() {
            simulation.advance_to(frame, 60);
            for (id, file) in simulation.layout().arriving_files() {
                let dir = simulation.layout().dir(file.dir);
                let to = ground(file.position)
                    + Vec3::Y * simulation.repository_effect(dir.repository).2;
                let (_, _, alpha) = simulation.file_effect(id, file);
                let (beam, progress) = simulation.arrival_beam(
                    Target::File(id),
                    file.arrival_author,
                    to,
                    file.presence,
                    file.flight,
                    file.arrival_revision,
                );
                if alpha > 0.0 && 0.0 < progress && progress < 1.0 {
                    flights += 1;
                    assert!(
                        beam.is_some(),
                        "{} lost its ray at frame {frame}",
                        file.name()
                    );
                }
            }
        }
        assert!(flights > 0);
    }

    #[test]
    fn tile_content_stays_inside_visible_births_and_exits() {
        for day_rate in [0.02, 0.05, 0.18] {
            let day = 86_400;
            let histories = [0, 4 * day, 8 * day]
                .into_iter()
                .enumerate()
                .map(|(repository, start)| {
                    let mut events = crate::timeline::synthetic();
                    for (index, event) in events.iter_mut().enumerate() {
                        event.seconds = start + i64::try_from(index).unwrap();
                    }
                    crate::repositories::namespace(&mut events, repository);
                    events
                })
                .collect();
            let mut simulation = Simulation::with_repositories(
                crate::repositories::merge(histories),
                &Options {
                    seconds_per_day: day_rate,
                    max_idle_seconds: 0.1,
                    repository_archived: vec![false, true, false],
                    repository_ends: vec![None; 3],
                    ..Default::default()
                },
                &Theme::neon(),
                &Mappings::default(),
                &["One".into(), "Two".into(), "Three".into()],
            );
            simulation.repository_boundary = crate::repositories::Boundary::Glass;
            let mut saw_second = false;
            let mut saw_exit = false;
            for frame in 0..=simulation.frames(60).unwrap() {
                simulation.advance_to(frame, 60);
                assert_tile_content(&simulation, frame);
                let visible = simulation.visible_repository_tile(1).is_some();
                saw_exit |= saw_second && !visible;
                saw_second |= visible;
            }
            assert!(saw_second && saw_exit);
        }
    }

    #[test]
    fn reserved_ground_is_drawn_before_the_first_write() {
        let mut simulation = Simulation::two_synthetic_repositories();
        let mut saw_reservation = false;
        for frame in 0..simulation.frames(60).unwrap() {
            simulation.advance_to(frame, 60);
            let tile = &simulation.layout().repositories[1];
            if tile.live {
                assert!(saw_reservation);
                assert_eq!(simulation.visible_repository_tile(1), tile.tile);
                break;
            }
            if tile.tile.is_some() {
                saw_reservation = true;
                assert_eq!(simulation.visible_repository_tile(1), tile.tile);
            }
        }
        assert!(saw_reservation);
    }

    #[test]
    fn packed_directory_subtrees_stay_separate_during_tile_motion() {
        let mut simulation = Simulation::two_synthetic_repositories();
        let mut compared = 0;
        for frame in 0..simulation.frames(60).unwrap() {
            simulation.advance_to(frame, 60);
            compared += assert_subtrees_clear(simulation.layout(), frame);
        }
        assert!(compared > 0);
    }

    fn assert_subtrees_clear(layout: &Layout, frame: u32) -> usize {
        let dirs: Vec<_> = layout.dirs().map(|(_, dir)| dir).collect();
        let mut compared = 0;
        let ancestor = |path: &str, sub: &str| {
            sub.strip_prefix(path)
                .is_some_and(|suffix| suffix.starts_with('/'))
        };
        for (index, a) in dirs.iter().enumerate().filter(|(_, dir)| dir.depth > 0) {
            for b in dirs[index + 1..].iter().filter(|b| {
                a.repository == b.repository
                    && b.depth > 0
                    && !ancestor(&a.path, &b.path)
                    && !ancestor(&b.path, &a.path)
            }) {
                compared += 1;
                let clearance =
                    a.position.distance(b.position) - a.radius() - b.radius();
                assert!(
                    clearance >= -1e-4,
                    "frame {frame}: {} overlaps {}",
                    a.path,
                    b.path
                );
            }
        }
        compared
    }

    fn assert_tile_content(simulation: &Simulation, frame: u32) {
        let inside = |repository, point: Vec2| {
            let Some(tile) = simulation.visible_repository_tile(repository) else {
                return;
            };
            let point = framing::world_to_field(point);
            let tolerance = Vec2::splat(1e-5);
            assert!(
                point.cmpge(tile.low - tolerance).all()
                    && point.cmple(tile.high + tolerance).all(),
                "frame {frame}, repository {repository}: {point} outside {tile:?}",
            );
        };
        for (_, dir) in simulation.layout().dirs() {
            inside(dir.repository, dir.position);
            if let Some(parent) = dir.parent {
                inside(dir.repository, simulation.layout().dir(parent).position);
            }
        }
        for (id, file) in simulation.layout().files() {
            let repository = simulation.layout().dir(file.dir).repository;
            let (position, _, alpha) = simulation.file_effect(id, file);
            if alpha > 0.001 && file.state != crate::layout::Presence::Arriving {
                inside(repository, Vec2::new(position.x, position.z));
            }
            let anchor = simulation.surface_target(Target::File(id)).unwrap();
            inside(repository, Vec2::new(anchor.x, anchor.z));
        }
        for pulse in &simulation.state.pulses {
            for &(target, ..) in &pulse.targets {
                if let Some((_, anchor)) = simulation.visible_ray(pulse, target) {
                    inside(pulse.repository, Vec2::new(anchor.x, anchor.z));
                }
            }
        }
        for label in simulation.labels() {
            if let Some(repository) = label.repository {
                inside(repository, Vec2::new(label.position.x, label.position.z));
            }
        }
    }

    #[test]
    fn single_tile_fills_with_separate_directory_subtrees() {
        let mut event = timeline::synthetic().remove(0);
        event.changes = ["src/a.rs", "src/b.rs", "tests/a.rs", "tests/b.rs"]
            .map(|path| crate::timeline::FileChange {
                path: path.to_owned(),
                kind: Kind::Added,
                ..Default::default()
            })
            .into();
        let mut simulation = Simulation::new(
            vec![event],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        for frame in 0..simulation.frames(60).unwrap() {
            simulation.advance_to(frame, 60);
            let layout = simulation.layout();
            if layout.files().next().is_none() {
                continue;
            }
            let region = &layout.repositories[0];
            let tile = simulation.visible_repository_tile(0).unwrap();
            let dirs: Vec<_> = layout.dirs().map(|(_, dir)| dir).collect();
            let low = dirs.iter().fold(Vec2::splat(f32::INFINITY), |low, dir| {
                low.min(dir.position - Vec2::splat(dir.radius()))
            });
            let high = dirs
                .iter()
                .fold(Vec2::splat(f32::NEG_INFINITY), |high, dir| {
                    high.max(dir.position + Vec2::splat(dir.radius()))
                });
            let size =
                (framing::world_to_field(high) - framing::world_to_field(low)).abs();
            let fill = size.element_product() * 0.25 / tile.area();
            assert!(fill >= 0.25, "frame {frame}: fill {fill:.3}");
            for &point in &region.outline {
                let projected = framing::world_to_field(point);
                assert!(
                    projected.cmpge(tile.low).all() && projected.cmple(tile.high).all()
                );
            }
            for (index, dir) in dirs.iter().enumerate() {
                for other in &dirs[..index] {
                    assert!(
                        dir.position.distance(other.position) + 1e-5
                            >= dir.radius() + other.radius()
                    );
                }
            }
        }
    }

    #[test]
    fn padded_birth_opens_a_walkable_gutter_before_its_first_write() {
        let (events, labels) = short_repository_history(3);
        let options = Options {
            seconds_per_day: 0.1,
            max_idle_seconds: 0.1,
            ..Default::default()
        };
        let mut simulation = Simulation::build(
            events.clone(),
            &options,
            &Theme::neon(),
            &Mappings::default(),
            &[],
            &labels,
            &mut None,
        )
        .unwrap();
        let history = simulation.schedule.clone();
        let departures = simulation.departures.clone();
        simulation
            .apply_padding(
                &events,
                &options,
                &labels,
                &[(1, 120), (2, 10)],
                (&history, &departures),
            )
            .unwrap();
        assert_eq!(simulation.births.len(), 2);
        let opening = simulation.schedule.commit_ticks().next().unwrap();
        assert_eq!(
            simulation
                .packing_sample(opening, None)
                .unwrap()
                .0
                .iter()
                .filter(|region| region.tile.is_some())
                .count(),
            1
        );
        for (index, &(repository, ready, _)) in simulation.births.iter().enumerate() {
            let arrival = simulation.schedule.commit_ticks().nth(index + 1).unwrap();
            let begin = ready - BIRTH_TICKS - 1;
            let region = |tick| {
                simulation.packing_sample(tick, None).unwrap().0[repository].clone()
            };
            assert!(region(begin).tile.is_none());
            assert!(region(begin + 1).tile.is_some());
            assert!(
                region(ready - 2).tile.unwrap().area()
                    < region(ready - 1).tile.unwrap().area()
            );
            assert!(!region(ready).live);
            assert!(!region(arrival - 1).live);
            assert!(region(arrival).live);
            assert!(
                region(arrival - 1).tile.unwrap().area() + 1e-6
                    >= region(arrival).tile.unwrap().area()
            );
        }
    }

    #[test]
    fn a_shrinking_neighbour_can_show_a_commit_during_birth() {
        let (events, labels) = short_repository_history(2);
        let mut opening = vec![events[0].clone(), events[2].clone(), events[1].clone()];
        for (index, event) in opening.iter_mut().enumerate() {
            event.seconds = i64::try_from(index).unwrap() * 86_400;
        }
        let simulation = Simulation::build(
            opening,
            &Options {
                seconds_per_day: 0.2,
                ..Options::default()
            },
            &Theme::neon(),
            &Mappings::default(),
            &[],
            &labels,
            &mut None,
        )
        .unwrap();
        let write = simulation.schedule.commit_ticks().nth(1).unwrap();
        let regions = |tick| simulation.packing_sample(tick, None).unwrap().0;
        let before = regions(write - 1);
        let now = regions(write);
        let after = regions(write + 1);
        let area = |row: &[crate::layout::RepositoryRegion], repo: usize| {
            row[repo].tile.unwrap().area()
        };
        assert!(area(&before, 0) > area(&now, 0) && area(&now, 0) > area(&after, 0));
        assert!(area(&before, 1) < area(&now, 1) && area(&now, 1) < area(&after, 1));
        assert!(!now[1].live);
    }

    #[test]
    fn unchanged_rebuild_reuses_layout_but_changed_times_replan() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        crate::repositories::namespace(&mut events, 0);
        let options = Options::default();
        let theme = Theme::neon();
        let mappings = Mappings::default();
        let names = ["One".to_owned()];
        let build = |padding: &[(usize, u32)],
                     cache: &mut Option<(Vec<Time>, Layout)>| {
            Simulation::build(
                events.clone(),
                &options,
                &theme,
                &mappings,
                padding,
                &names,
                cache,
            )
            .unwrap()
        };
        let mut cache = None;
        let first = build(&[], &mut cache);
        let repeated = build(&[], &mut cache);
        assert!(
            first
                .initial
                .layout
                .shares_repository_plan(&repeated.initial.layout)
        );
        let changed = build(&[(1, 60)], &mut cache);
        assert!(
            !first
                .initial
                .layout
                .shares_repository_plan(&changed.initial.layout)
        );
    }

    #[test]
    fn a_pulse_is_live_for_at_most_the_snapshot_tick_capacity() {
        for scale in [*FRAGMENT_SPEEDS.start(), 1.0, *FRAGMENT_SPEEDS.end()] {
            assert_pulses_fit_snapshot_capacity(FragmentPace::new(scale));
        }
    }

    fn assert_pulses_fit_snapshot_capacity(pace: FragmentPace) {
        let capacity = u64::try_from(slots::Snapshots::pulse_ticks(pace)).unwrap();
        let lifetime = pace.pulse_end(Time::ZERO) - Time::ZERO;
        let assert_bounded = |near: Time, mut first: u64| {
            for start in [near.next_down(), near, near.next_up()] {
                while first > 0 && tick_time(first - 1) >= start {
                    first -= 1;
                }
                while tick_time(first) < start {
                    first += 1;
                }
                assert!(
                    tick_time(first + capacity) >= pace.pulse_end(start),
                    "a pulse starting at {start} outlives {capacity} ticks"
                );
            }
        };
        // The live tick count only changes where a start or an end crosses a tick.
        let span = 16_384 * u64::from(TICK_RATE);
        for tick in (0..span + 200).chain(11 * span - 200..11 * span + 20_000) {
            let aligned = tick_time(tick);
            let ending = aligned - lifetime;
            let begun = tick.saturating_sub(capacity);
            assert_bounded(aligned, tick);
            assert_bounded(ending.next_down().next_down(), begun);
            assert_bounded(ending, begun);
            assert_bounded(ending.next_up().next_up(), begun);
        }
    }

    #[test]
    fn the_slowest_fragment_pulse_ends_before_its_avatar_goes_idle() {
        let slowest = FragmentPace::new(*FRAGMENT_SPEEDS.start());
        assert!(slowest.pulse_end(Time::ZERO) - Time::ZERO < AVATAR_IDLE);
    }

    #[test]
    fn convergence_limit_does_not_recommend_an_already_capped_policy() {
        assert!(super::check_travel_pass(32).is_ok());
        let error = super::check_travel_pass(33).unwrap_err().to_string();
        assert!(error.contains("32 passes"));
        assert!(error.contains("shorten the history"));
        assert!(!error.contains("use capped travel"));
    }
    #[test]
    fn overlapping_commits_show_only_the_latest_samples_for_each_file() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        events[1].changes = events[0].changes.clone();
        events[1].seconds = events[0].seconds;
        for (index, event) in events.iter_mut().enumerate() {
            for change in &mut event.changes {
                change.samples = (0..3)
                    .map(|row| crate::diff_samples::Sample {
                        added: true,
                        text: format!("commit {index} row {row}"),
                    })
                    .collect();
            }
        }
        let mut sim = super::Simulation::new(
            events,
            &super::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        sim.advance_to(40, 60);
        let lines = sim.diff_lines();
        assert_eq!(lines.len(), 9);
        assert!(
            lines
                .iter()
                .all(|line| line.sample.text.starts_with("commit 1"))
        );
    }

    #[test]
    fn fragments_follow_beam_curve_for_every_style() {
        for ((name, frame), scale) in crate::theme::ThemeName::ALL
            .into_iter()
            .flat_map(|name| [35, 45, 55].map(move |frame| (name, frame)))
            .flat_map(|run| [1.0, 0.5].map(move |scale| (run, scale)))
        {
            let mut events = crate::timeline::synthetic();
            events.truncate(1);
            events[0].changes.truncate(1);
            events[0].changes[0].insertions = 1;
            events[0].changes[0].removals = 1;
            events[0].changes[0].samples = [true, false]
                .map(|added| crate::diff_samples::Sample {
                    added,
                    text: "sample".to_owned(),
                })
                .to_vec();
            let theme = name.build();
            let mut sim = super::Simulation::new(
                events,
                &super::Options {
                    fragment_speed: scale,
                    ..Default::default()
                },
                &theme,
                &crate::contributors::Mappings::default(),
            );
            sim.advance_to(frame, 60);
            let beam = &sim.beams()[0];
            let age = sim.seconds() - sim.state.pulses[0].start;
            let phase = (age * super::FRAGMENT_SPEED * scale).fract();
            let fragments = sim.fragments();
            assert_eq!(fragments.len(), 2);
            for fragment in &fragments {
                let t = if fragment.rising { 1.0 - phase } else { phase };
                let mut expected = beam.from.lerp(beam.to, t);
                if matches!(theme.beam_style, crate::theme::BeamStyle::Arch) {
                    expected.y += t * (1.0 - t) * beam.from.distance(beam.to);
                }
                assert!(
                    fragment.position.distance(expected) < 0.0001,
                    "{} at {frame}, rising={}: {:?} != {expected:?}",
                    name.title(),
                    fragment.rising,
                    fragment.position
                );
            }
            let lines = sim.diff_lines();
            assert_eq!(lines.len(), 2);
            for line in lines {
                assert_eq!(line.position, fragments[line.fragment].position);
            }
        }
    }

    #[test]
    fn file_deleted_before_launch_never_appears() {
        let event = timeline::synthetic().remove(0);
        let mut simulation = Simulation::new(
            vec![event],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        simulation.advance_to(60, 60);
        let path = format!("{}short-lived.rs", crate::repositories::prefix(0));
        let id = simulation.state.layout.add(&path, super::ADD_DELAY, 0);
        let file = simulation.layout().file(id).unwrap();
        let before_deletion = simulation.file_effect(id, file).2;
        simulation.state.layout.remove(&path, 0.0, 0).unwrap();
        let file = simulation.layout().file(id).unwrap();
        let after_deletion = simulation.file_effect(id, file).2;
        simulation.state.layout.step(1.0 / 60.0);
        assert!(simulation.layout().file(id).is_none());
        assert!(
            before_deletion <= 0.0 && after_deletion <= 0.0,
            "file alpha before deletion: {before_deletion}, after: {after_deletion}"
        );
    }

    #[test]
    fn grouped_file_deleted_while_waiting_stays_invisible_during_fade() {
        let event = timeline::synthetic().remove(0);
        let mut simulation = Simulation::new(
            vec![event],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        simulation.advance_to(60, 60);
        let path = format!("{}short-lived.rs", crate::repositories::prefix(0));
        let layout = &mut simulation.state.layout;
        let revision = layout.revision();
        let id = layout.add(&path, 0.0, 0);
        let directory = layout.file(id).unwrap().dir;
        let offset = super::MAX_ARRIVAL_OFFSET;
        layout.route_arrival(
            id,
            revision,
            directory,
            crate::layout::DirectoryFlight { directory, offset },
            false,
        );
        layout.step(1.0 / 60.0);
        layout.remove(&path, 0.0, 0).unwrap();
        let file = simulation.layout().file(id).unwrap();
        assert!(file.presence > 0.0 && file.presence < offset);
        assert!(simulation.file_effect(id, file).2 <= 0.0);
    }

    #[test]
    fn a_readded_visible_file_recovers_in_place() {
        let event = timeline::synthetic().remove(0);
        let mut simulation = Simulation::new(
            vec![event],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        simulation.advance_to(60, 60);
        let path = format!("{}recovered.rs", crate::repositories::prefix(0));
        let tick = super::tick_seconds();
        let id = simulation.state.layout.add(&path, 0.0, 0);
        for _ in 0..60 {
            simulation.state.layout.step(tick);
        }
        simulation.state.layout.remove(&path, 0.0, 0).unwrap();
        simulation.state.layout.step(tick);
        let before = simulation.file_effect(id, simulation.layout().file(id).unwrap());

        let revision = simulation.layout().revision();
        assert_eq!(simulation.state.layout.add(&path, tick, 0), id);
        assert!(!simulation.layout().new_file_arrival(id, revision));
        let restored =
            simulation.file_effect(id, simulation.layout().file(id).unwrap());
        assert_eq!(restored.0, before.0);
        assert_eq!(restored.2, before.2);
    }

    #[test]
    fn readded_grouped_file_waits_at_the_start_of_its_new_ray() {
        let event = timeline::synthetic().remove(0);
        let mut simulation = Simulation::new(
            vec![event],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        simulation.advance_to(60, 60);
        let path = format!("{}readded.rs", crate::repositories::prefix(0));
        let tick = 1.0 / 60.0;
        let layout = &mut simulation.state.layout;
        let revision = layout.revision();
        let id = layout.add(&path, 0.0, 0);
        let directory = layout.file(id).unwrap().dir;
        layout.route_arrival(
            id,
            revision,
            directory,
            crate::layout::DirectoryFlight {
                directory,
                offset: super::MAX_ARRIVAL_OFFSET,
            },
            false,
        );
        for _ in 0..8 {
            layout.step(tick);
        }
        layout.remove(&path, super::DELETE_DELAY, 0).unwrap();
        layout.step(tick);
        assert!(layout.file(id).unwrap().arrival_cancelled);

        let revision = layout.revision();
        assert_eq!(layout.add(&path, tick, 0), id);
        layout.route_arrival(
            id,
            revision,
            directory,
            crate::layout::DirectoryFlight {
                directory,
                offset: 0.0,
            },
            false,
        );
        let file = simulation.layout().file(id).unwrap();
        let (position, _, alpha) = simulation.file_effect(id, file);
        let (_, beam) = simulation
            .file_arrival_beams()
            .find(|(_, beam)| beam.target == super::Target::Dir(directory))
            .unwrap();
        assert!(alpha <= 0.0, "delayed arrival had alpha {alpha}");
        assert!(position.distance(beam.from) < 0.0001);

        simulation.state.layout.step(tick);
        let file = simulation.layout().file(id).unwrap();
        assert!(simulation.file_effect(id, file).2 <= 0.0);

        simulation.state.layout.step(tick);
        let file = simulation.layout().file(id).unwrap();
        assert!(simulation.file_effect(id, file).2 > 0.0);
    }

    #[test]
    fn arriving_files_follow_beam_curve_for_every_style() {
        for name in crate::theme::ThemeName::ALL {
            let mut events = crate::timeline::synthetic();
            events.truncate(1);
            events[0].changes.truncate(1);
            let theme = name.build();
            let mut sim = super::Simulation::new(
                events,
                &super::Options::default(),
                &theme,
                &crate::contributors::Mappings::default(),
            );
            for frame in [65, 70, 75] {
                sim.advance_to(frame, 60);
                let (id, file) = sim.layout().files().next().unwrap();
                assert_eq!(file.state, crate::layout::Presence::Arriving);
                assert!(file.presence > 0.0 && file.presence < 1.0);
                let from = sim.avatars()[file.author].position;
                let to = super::ground(file.position);
                let t = file.presence;
                let mut expected = from.lerp(to, t);
                if matches!(theme.beam_style, crate::theme::BeamStyle::Arch) {
                    expected.y += t * (1.0 - t) * from.distance(to);
                }
                assert!(
                    sim.file_effect(id, file).0.distance(expected) < 0.0001,
                    "{} at {frame}",
                    name.title()
                );
            }
        }
    }

    #[test]
    fn crowded_arrivals_stream_along_one_directory_ray_then_settle() {
        for count in [23, 25] {
            let mut events = crate::timeline::synthetic();
            events.truncate(1);
            events[0].changes = (0..count)
                .map(|i| crate::timeline::FileChange {
                    path: format!("src/file{i}.rs"),
                    insertions: 1,
                    removals: 1,
                    ..Default::default()
                })
                .collect();
            let theme = crate::theme::ThemeName::Outrun.build();
            let mut sim = super::Simulation::new(
                events,
                &super::Options::default(),
                &theme,
                &crate::contributors::Mappings::default(),
            );
            sim.advance_to(60, 60);
            let aggregated = count > super::BEAM_LIMIT;
            let beams = sim.beams();
            assert_eq!(beams.len(), if aggregated { 1 } else { count });
            let mut offsets = Vec::new();
            for (id, file) in sim.layout().files() {
                assert_eq!(file.state, crate::layout::Presence::Arriving);
                if aggregated {
                    offsets.push((id, file.flight.unwrap().offset));
                    let beam = beams[0];
                    let (position, _, _) = sim.file_effect(id, file);
                    let flight = file.flight.unwrap();
                    let progress = ((file.presence - flight.offset)
                        / (1.0 - super::MAX_ARRIVAL_OFFSET))
                        .clamp(0.0, 1.0);
                    let along = (progress / super::ARRIVAL_FLIGHT).min(1.0);
                    assert!(along < 1.0);
                    let expected = theme.beam_style.point(beam.from, beam.to, along);
                    assert!(position.distance(expected) < 0.0001);
                    continue;
                }
                let to = super::ground(file.position);
                let expected = theme.beam_style.point(
                    sim.state.beam_start(file.author, to),
                    to,
                    file.presence,
                );
                assert!(
                    sim.file_effect(id, file).0.distance(expected) < 0.0001,
                    "{count} files, file={id}"
                );
            }
            offsets.sort_by_key(|&(id, _)| id);
            assert!(offsets.windows(2).all(|pair| pair[0].1 < pair[1].1));
            let pulse = &sim.state.pulses[0];
            for fragment in sim.fragments() {
                let (_, target) = fragment.beam.unwrap();
                assert!(pulse.targets.iter().any(|&(drawn, ..)| drawn == target));
                assert!(
                    beams
                        .iter()
                        .any(|beam| beam.to == sim.state.point(target).unwrap())
                );
            }
            sim.advance_to(120, 60);
            for (id, file) in sim.layout().files() {
                assert_eq!(file.state, crate::layout::Presence::Present);
                assert_eq!(sim.file_effect(id, file).0, super::ground(file.position));
            }
        }
    }

    #[test]
    fn rising_fragments_fade_continuously_across_the_phase_wrap() {
        let samples: Vec<_> = (0_u16..=2000)
            .map(|step| {
                super::diff_fragments(
                    f32::from(step) / 1100.0,
                    1.0,
                    (
                        super::BeamStyle::Straight,
                        super::Vec3::Y * 4.0,
                        super::Vec3::ZERO,
                    ),
                    (1, true),
                    super::FragmentPace::new(1.0),
                )
                .next()
                .unwrap()
            })
            .collect();
        assert_eq!(samples[0].alpha, 0.0);
        assert!(samples[500].position.y > samples[100].position.y);
        assert!(samples[500].alpha < samples[100].alpha);
        assert!(samples[999].alpha < 0.00001);
        assert!(samples[1000].alpha < 0.00001);
        assert!(
            samples
                .windows(2)
                .all(|pair| { (pair[1].alpha - pair[0].alpha).abs() < 0.025 })
        );
    }

    #[test]
    fn samples_attach_to_matching_directions_and_leave_extra_pieces_plain() {
        let mut events = crate::timeline::synthetic();
        events.truncate(1);
        events[0].changes.truncate(1);
        let change = &mut events[0].changes[0];
        change.insertions = 7;
        change.removals = 7;
        change.samples = vec![
            crate::diff_samples::Sample {
                added: true,
                text: "let added = true;".to_owned(),
            },
            crate::diff_samples::Sample {
                added: false,
                text: "let removed = true;".to_owned(),
            },
        ];
        let mut sim = super::Simulation::new(
            events,
            &super::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        sim.advance_to(45, 60);
        let fragments = sim.fragments();
        assert_eq!(fragments.len(), 6);
        assert_eq!(
            fragments
                .iter()
                .filter(|piece| piece.sample.is_none())
                .count(),
            4
        );
        for fragment in fragments.iter().filter(|piece| piece.sample.is_some()) {
            assert_ne!(fragment.rising, fragment.sample.as_ref().unwrap().added);
        }
        let positions: Vec<_> =
            sim.diff_lines().iter().map(|line| line.position).collect();
        sim.advance_to(48, 60);
        assert!(
            sim.diff_lines()
                .iter()
                .zip(positions)
                .all(|(line, old)| line.position != old)
        );
    }

    #[test]
    fn a_co_authored_commit_shows_its_text_and_labels_once() {
        let mut events = crate::timeline::synthetic();
        events.truncate(1);
        events[0].changes[0].samples = vec![crate::diff_samples::Sample {
            added: true,
            text: "let sample = 1;".to_owned(),
        }];
        events[0].co_authors = vec![timeline::Identity {
            name: b"Pair".to_vec(),
            email: b"pair@example.invalid".to_vec(),
        }];
        let mut sim = super::Simulation::new(
            events,
            &super::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        let ray_start = sim.schedule.starts[0] + DISBAND + tick_seconds();
        sim.advance_to(frames_for(ray_start + 0.25, 60).unwrap(), 60);
        assert_eq!(sim.diff_lines().len(), 1);
        assert!(sim.fragments().iter().any(|fragment| {
            fragment.sample.is_some() && fragment.beam.is_some_and(|(a, _)| a == 0)
        }));
        let mut activity: Vec<_> = sim
            .labels()
            .into_iter()
            .filter(|label| label.kind == LabelKind::Activity)
            .map(|label| label.text)
            .collect();
        let labelled = activity.len();
        activity.dedup();
        assert_eq!((labelled, activity.len()), (3, 3));
    }

    #[test]
    fn diff_samples_follow_files_fade_and_rewind_at_explicit_times() {
        let mut events = crate::timeline::synthetic();
        events.truncate(1);
        events[0].changes[0].samples = vec![crate::diff_samples::Sample {
            added: true,
            text: "let sample = 1;".to_owned(),
        }];
        let mut sim = super::Simulation::new(
            events,
            &super::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        sim.advance_to(45, 60);
        let hidden_dir = sim.layout().files().next().unwrap().1.dir;
        assert!(
            sim.diff_lines_for_detail(|dir| if dir == hidden_dir { 0.0 } else { 1.0 })
                .is_empty()
        );
        assert_eq!(sim.diff_lines_for_detail(|_| 1.0).len(), 1);
        let plain = sim.fragments_for_detail(|_| 0.0);
        assert!(!plain.is_empty());
        assert!(plain.iter().all(|fragment| fragment.sample.is_none()));
        let line = sim.diff_lines().pop().unwrap();
        assert!(line.alpha > 0.0 && line.alpha <= 1.0);
        assert!(sim.fragments().iter().any(|fragment| {
            !fragment.rising
                && fragment.position == line.position
                && fragment.sample.is_some()
        }));
        sim.advance_to(102, 60);
        let faded = sim.diff_lines().pop().unwrap();
        assert!(faded.alpha > 0.0 && faded.alpha < line.alpha);
        sim.advance_to(180, 60);
        assert!(sim.diff_lines().is_empty());
        sim.advance_to(45, 60);
        let replayed = sim.diff_lines().pop().unwrap();
        assert_eq!(replayed.position, line.position);
        assert_eq!(replayed.sample, line.sample);
        assert_eq!(replayed.alpha, line.alpha);
        sim.advance_to(0, 60);
        assert!(sim.diff_lines().is_empty());
    }

    use super::*;

    fn ending_events(rows: &[(usize, i64)]) -> Vec<Event> {
        rows.iter()
            .enumerate()
            .map(|(index, &(repository, seconds))| {
                let mut event = crate::timeline::synthetic().remove(0);
                event.commit = format!("commit-{index}");
                event.seconds = seconds;
                crate::repositories::namespace(
                    std::slice::from_mut(&mut event),
                    repository,
                );
                event
            })
            .collect()
    }

    #[test]
    fn repository_endings_validate_the_whole_imported_order() {
        let day = 86_400;
        let ending = crate::repositories::parse_ended("1970-01-02").unwrap();
        let cases = [
            (vec![(0, 0), (0, 2 * day)], 0, 1, "1970-01-03"),
            (vec![(0, 0), (1, 4 * day), (0, 0)], 0, 2, "1970-01-05"),
            (vec![(0, 0), (0, 3 * day)], 1, 1, "1970-01-04"),
        ];
        for (rows, start, offending, remedy) in cases {
            let options = Options {
                repository_ends: vec![Some(ending), None],
                start,
                commits: 1,
                ..Options::default()
            };
            let result = Simulation::try_with_repositories(
                ending_events(&rows),
                &options,
                &Theme::neon(),
                &Mappings::default(),
                &["Ended".into(), "Other".into()],
            );
            let Err(error) = result else {
                panic!("ending must reject commit {offending}")
            };
            let message = error.to_string();
            assert!(message.contains("repository 'Ended'"), "{message}");
            assert!(message.contains("'1970-01-02' UTC (inclusive)"));
            assert!(message.contains(&format!("commit commit-{offending}")));
            assert!(message.contains(&format!("set ended to '{remedy}'")));
        }
    }

    #[test]
    fn ending_day_is_inclusive_and_an_ended_repository_can_precede_the_start() {
        let day = 86_400;
        let ending = crate::repositories::parse_ended("1970-01-02").unwrap();
        for (rows, start) in [
            (vec![(0, day), (0, 2 * day - 1)], 0),
            (vec![(0, day), (1, 4 * day)], 1),
        ] {
            let options = Options {
                repository_ends: vec![Some(ending), None],
                start,
                ..Options::default()
            };
            Simulation::try_with_repositories(
                ending_events(&rows),
                &options,
                &Theme::neon(),
                &Mappings::default(),
                &["Ended".into(), "Other".into()],
            )
            .unwrap();
        }
    }
    #[test]
    fn automatic_windows_keep_short_selected_histories_in_one_plan() {
        let mut event = crate::timeline::synthetic().remove(0);
        event.seconds = 0;
        let mut later = event.clone();
        later.seconds = 86_400;
        let events = vec![event, later];
        let mut options = Options {
            seconds_per_day: WINDOW_SECONDS + windows::MARGIN - LEAD_IN - TAIL,
            max_idle_seconds: WINDOW_SECONDS * 2.0,
            ..Options::default()
        };
        options.choose_window(&events);
        assert_eq!(options.travel_window_seconds, None);
        options.seconds_per_day += tick_seconds();
        options.choose_window(&events);
        assert_eq!(options.travel_window_seconds, Some(WINDOW_SECONDS));
        options.start = 1;
        options.choose_window(&events);
        assert_eq!(options.travel_window_seconds, None);
    }

    #[test]
    fn repository_plan_limits_reject_slow_or_oversized_inputs() {
        let events = crate::timeline::synthetic();
        assert!(
            Simulation::check_repository_plan(
                &events,
                &Options::default(),
                &vec![String::new(); 5]
            )
            .is_ok()
        );
        assert!(
            Simulation::check_repository_plan(
                &events,
                &Options::default(),
                &vec![String::new(); 30]
            )
            .is_ok()
        );
        assert!(
            Simulation::check_repository_plan(
                &events,
                &Options::default(),
                &vec![String::new(); 31]
            )
            .is_ok()
        );
        assert!(
            Simulation::check_repository_plan(
                &events,
                &Options {
                    seconds_per_day: 86_400.0,
                    max_idle_seconds: 86_400.0,
                    ..Options::default()
                },
                &vec![String::new(); 5]
            )
            .is_err()
        );
    }

    #[test]
    fn forty_repositories_born_together_ease_in_without_conflicting_tiles() {
        let count = 40;
        let mut events: Vec<_> = (0..count)
            .flat_map(|index| {
                let mut history = timeline::synthetic();
                crate::repositories::namespace(&mut history, index);
                history
            })
            .collect();
        events.sort_by_key(|event| event.seconds);
        let labels: Vec<_> = (0..count).map(|index| format!("repo{index}")).collect();
        let options = Options {
            seconds_per_day: 0.05,
            max_idle_seconds: 0.05,
            ..Options::default()
        };
        let mut simulation = Simulation::build(
            events,
            &options,
            &Theme::neon(),
            &Mappings::default(),
            &[],
            &labels,
            &mut None,
        )
        .unwrap();
        simulation.advance_to(frames_for(simulation.duration(), 60).unwrap(), 60);
    }

    #[test]
    fn crowded_fields_keep_avatar_size() {
        let (events, labels) = short_repository_history(56);
        let options = Options {
            seconds_per_day: 0.05,
            max_idle_seconds: 0.05,
            gutter: 20.0,
            ..Options::default()
        };
        let mut simulation = Simulation::build(
            events,
            &options,
            &Theme::neon(),
            &Mappings::default(),
            &[],
            &labels,
            &mut None,
        )
        .unwrap();
        for frame in 0..=frames_for(simulation.duration(), 60).unwrap() {
            simulation.advance_to(frame, 60);
            assert_eq!(
                simulation.avatar_disc_scale(),
                simulation.state.avatar_scale * options.avatar_size
            );
        }
        simulation.assert_stitched_travel_rules();
    }

    #[test]
    fn numeric_travel_cap_does_not_reserve_worst_case_waits() {
        let events = crate::timeline::synthetic();
        let options = Options {
            travel_cap: TravelCap::Seconds(3600.0),
            ..Options::default()
        };
        Simulation::check_repository_plan(&events, &options, &vec![String::new(); 48])
            .unwrap();
    }

    #[test]
    fn uncapped_actual_padding_still_obeys_capture_budgets() {
        let events = crate::timeline::synthetic();
        let options = Options {
            travel_cap: TravelCap::None,
            ..Options::default()
        };
        assert!(
            Simulation::check_repository_plan(
                &events,
                &options,
                &vec![String::new(); 5]
            )
            .is_ok()
        );
        let schedule = Schedule::with_padding(
            &events,
            options.seconds_per_day,
            options.max_idle_seconds,
            |_| 2_160_000,
        );
        assert!(
            Simulation::check_schedule(
                &events,
                &options,
                &vec![String::new(); 5],
                &schedule,
                None,
            )
            .is_err()
        );
    }

    pub(super) fn short_repository_history(count: usize) -> (Vec<Event>, Vec<String>) {
        let events = (0..count * 3)
            .map(|index| {
                let mut row = vec![timeline::synthetic().remove(0)];
                row[0].seconds = i64::try_from(index).unwrap() * 86_400;
                row[0].author.name = format!("person{}", index % 3).into_bytes();
                row[0].author.email =
                    format!("p{}@test.invalid", index % 3).into_bytes();
                crate::repositories::namespace(&mut row, index % count);
                row.remove(0)
            })
            .collect();
        let labels = (0..count).map(|index| format!("repo{index}")).collect();
        (events, labels)
    }

    #[test]
    fn short_repository_births_preserve_feasible_tiles() {
        for (count, gutter) in [(10, 0.0), (10, 4.0), (30, 0.0), (30, 4.0)] {
            let (events, labels) = short_repository_history(count);
            let options = Options {
                seconds_per_day: 0.05,
                max_idle_seconds: 0.05,
                gutter,
                ..Options::default()
            };
            let simulation = Simulation::build(
                events,
                &options,
                &Theme::neon(),
                &Mappings::default(),
                &[],
                &labels,
                &mut None,
            )
            .unwrap();
            for tick in 0..=frames_for(simulation.duration(), 60).unwrap() {
                let (regions, _, _) =
                    simulation.packing.sample(u64::from(tick)).unwrap();
                for tile in regions.iter().filter_map(|region| region.tile) {
                    assert!(tile.low.is_finite() && tile.high.is_finite());
                    assert!(tile.high.cmpge(tile.low).all());
                    assert!(
                        tile.low
                            .cmpge(
                                crate::layout::packing::Tile::viewport().low
                                    - Vec2::splat(1e-6)
                            )
                            .all()
                    );
                    assert!(
                        tile.high
                            .cmple(
                                crate::layout::packing::Tile::viewport().high
                                    + Vec2::splat(1e-6)
                            )
                            .all()
                    );
                }
            }
        }
    }

    #[test]
    fn uncapped_short_history_walks_to_new_repositories() {
        let (events, labels) = short_repository_history(10);
        let options = Options {
            seconds_per_day: 0.2,
            max_idle_seconds: 0.2,
            travel_cap: TravelCap::None,
            bot_travel: BotTravel::Walk,
            ..Options::default()
        };
        let started = std::time::Instant::now();
        let mut simulation = Simulation::try_with_repositories(
            events,
            &options,
            &Theme::neon(),
            &Mappings::default(),
            &labels,
        )
        .unwrap();
        eprintln!("DENSE_PLAN seconds={}", started.elapsed().as_secs_f64());
        assert_eq!(simulation.travel.stats.faded, 0);
        assert_eq!(simulation.travel.stats.birth_fades, 0);
        let mut audit = slots::diagnostics::MovingBeams::default();
        let mut checkpoints = Vec::new();
        let frames = frames_for(simulation.duration(), 60).unwrap();
        let mut previous_tiles = 0;
        let mut last_birth = 0;
        for tick in 0..=frames {
            simulation.advance_to(tick, 60);
            audit.observe(&simulation);
            if tick % 60 == 0 {
                checkpoints.push((tick / 2, format!("{:?}", simulation.state.avatars)));
            }
            let live_tiles = simulation
                .state
                .layout
                .repositories
                .iter()
                .filter(|r| r.tile.is_some())
                .count();
            let born = live_tiles > previous_tiles;
            if born {
                last_birth = tick;
            }
            previous_tiles = live_tiles;
            if born || tick == last_birth + 15 || tick == frames {
                travel::assert_edge_connectivity(&simulation);
            }
        }
        eprintln!("DENSE_AUDIT {}", serde_json::to_string(&audit).unwrap());
        audit.assert_clear(options.avatar_max_speed);
        simulation.advance_to(0, 30);
        for (frame, avatars) in checkpoints {
            simulation.advance_to(frame, 30);
            assert_eq!(format!("{:?}", simulation.state.avatars), avatars);
        }
    }

    #[test]
    fn uncapped_bots_fade_between_repositories_unless_told_to_walk() {
        let events: Vec<_> = (0..4)
            .map(|index| {
                let mut row = vec![timeline::synthetic().remove(0)];
                row[0].seconds = i64::try_from(index).unwrap() * 86_400;
                row[0].author.name = b"Automation[bot]".to_vec();
                row[0].author.email = b"automation@test.invalid".to_vec();
                crate::repositories::namespace(&mut row, index % 2);
                row.remove(0)
            })
            .collect();
        for bot_travel in [BotTravel::Fade, BotTravel::Walk] {
            let options = Options {
                seconds_per_day: 0.3,
                travel_cap: TravelCap::None,
                bot_travel,
                ..Options::default()
            };
            let simulation = Simulation::try_with_repositories(
                events.clone(),
                &options,
                &Theme::neon(),
                &Mappings::default(),
                &["a".into(), "b".into()],
            )
            .unwrap();
            let stats = simulation.travel.stats;
            assert_eq!(
                stats.bot_fades,
                if bot_travel == BotTravel::Fade { 3 } else { 0 }
            );
            assert_eq!(stats.faded, stats.bot_fades);
        }
    }

    #[test]
    fn repository_plan_limits_use_the_selected_playback() {
        let mut events: Vec<_> = (0..2)
            .map(|index| {
                let mut history = timeline::synthetic();
                history.truncate(1);
                history[0].seconds = i64::try_from(index).unwrap() * 86_400;
                crate::repositories::namespace(&mut history, index);
                history.remove(0)
            })
            .collect();
        let names = ["First".to_owned(), "Second".to_owned()];
        for (start, commits) in [(0, 1), (1, usize::MAX), (2, usize::MAX)] {
            let options = Options {
                start,
                commits,
                seconds_per_day: 86_400.0,
                max_idle_seconds: 86_400.0,
                ..Options::default()
            };
            Simulation::check_repository_plan(
                &events,
                &options,
                &vec![String::new(); 2],
            )
            .unwrap();
            let simulation = Simulation::with_repositories(
                events.clone(),
                &options,
                &Theme::neon(),
                &Mappings::default(),
                &names,
            );
            let first_write = if start == 1 {
                tick_time(BIRTH_TICKS + 1)
            } else {
                Time::seconds(LEAD_IN)
            };
            assert_eq!(simulation.duration(), first_write + TAIL);
        }
        events[1].seconds = events[0].seconds;
        assert!(
            Simulation::check_repository_plan(
                &events,
                &Options::default(),
                &vec![String::new(); 2]
            )
            .is_ok()
        );
    }
    use crate::timeline;

    fn simulation() -> Simulation {
        Simulation::new(
            timeline::synthetic(),
            &Options::default(),
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        )
    }

    #[test]
    fn heat_scales_with_both_diff_counts_and_accumulates_to_a_cap() {
        let mut layout = Layout::planned_paths(
            &["heat.rs", "insertions.rs", "huge.rs", "moved.rs"],
            0,
        );
        let mut change = timeline::FileChange {
            path: "heat.rs".to_owned(),
            kind: Kind::Added,
            insertions: 0,
            removals: 0,
            ..Default::default()
        };
        let id = apply(&mut layout, &change, 0);
        let small = layout.file(id).unwrap().heat;
        assert!(small > 0.0);
        let mut insertion = change.clone();
        insertion.path = "insertions.rs".to_owned();
        insertion.insertions = 100;
        let inserted = apply(&mut layout, &insertion, 0);
        assert!(layout.file(inserted).unwrap().heat > small);
        change.kind = Kind::Modified;
        change.removals = 100;
        apply(&mut layout, &change, 0);
        let accumulated = layout.file(id).unwrap().heat;
        assert!(accumulated > 2.0 * small);
        assert!(accumulated < 1.0);
        change.insertions = u32::MAX;
        change.removals = u32::MAX;
        apply(&mut layout, &change, 0);
        assert_eq!(layout.file(id).unwrap().heat, 1.0);
        let mut huge = change.clone();
        huge.path = "huge.rs".to_owned();
        huge.kind = Kind::Added;
        let huge_id = apply(&mut layout, &huge, 0);
        assert_eq!(layout.file(huge_id).unwrap().heat, 0.75);
        change.kind = Kind::Moved {
            from: change.path.clone(),
        };
        change.path = "moved.rs".to_owned();
        assert_eq!(apply(&mut layout, &change, 0), id);
        assert_eq!(layout.file(id).unwrap().heat, 1.0);
    }

    #[test]
    fn deletion_does_not_add_heat_and_halos_decay_independently() {
        let mut layout = Layout::planned_paths(&["a.rs"], 0);
        let mut change = timeline::FileChange {
            path: "a.rs".to_owned(),
            kind: Kind::Added,
            insertions: 0,
            removals: 0,
            ..Default::default()
        };
        let id = apply(&mut layout, &change, 0);
        let heat = layout.file(id).unwrap().heat;
        assert_eq!(layout.file(id).unwrap().activity, 1.0);
        layout.begin_tick(6.0, 86400.0, FileMotion::Advance);
        assert!(layout.file(id).unwrap().activity < 0.03);
        assert!(layout.file(id).unwrap().heat > heat * 0.99);
        change.kind = Kind::Deleted;
        let before = layout.file(id).unwrap().heat;
        apply(&mut layout, &change, 0);
        assert_eq!(layout.file(id).unwrap().heat, before);
        assert_eq!(layout.file(id).unwrap().activity, 1.0);
        layout.begin_tick(1.0, 0.01, FileMotion::Advance);
        assert_eq!(layout.file(id).unwrap().heat, 0.0);
        assert!(layout.file(id).unwrap().activity > 0.5);
        layout.settle(0, 0.0);
        assert_eq!(layout.file(id).unwrap().activity, 0.0);
    }

    #[test]
    fn camera_keeps_moving_repository_glass_inside_the_export_frame() {
        let histories = (0..5)
            .map(|index| {
                let mut history = crate::timeline::synthetic();
                history.truncate(3);
                crate::repositories::namespace(&mut history, index);
                history
            })
            .collect();
        let mut simulation = Simulation::with_repositories(
            crate::repositories::merge(histories),
            &Options {
                seconds_per_day: 0.01,
                max_idle_seconds: tick_seconds(),
                ..Options::default()
            },
            &Theme::neon(),
            &Mappings::default(),
            &[
                "A".to_owned(),
                "B".to_owned(),
                "C".to_owned(),
                "D".to_owned(),
                "E".to_owned(),
            ],
        );
        for frame in 1..frames_for(simulation.duration(), 60).unwrap() {
            simulation.advance_to(frame, 60);
            let (focus, distance) = simulation.view();
            let eye = focus + view_direction() * distance;
            let layout = simulation.layout();
            let geometry = simulation.theme().geometry();
            for (_, file) in layout.files() {
                let presence = file.visible_presence(layout.dir(file.dir));
                if presence <= 0.0 {
                    continue;
                }
                let tile = layout.repositories[layout.dir(file.dir).repository]
                    .tile
                    .unwrap();
                let corners = box_corners(geometry.file_min, geometry.file_max)
                    .map(|corner| ground(file.position) + corner * presence);
                for point in project(eye, focus, &corners) {
                    let point = point.expect("visible file in front of camera")
                        / framing::GROUND_FACTOR;
                    assert!(
                        point.cmpge(tile.low - Vec2::splat(0.0001)).all()
                            && point.cmple(tile.high + Vec2::splat(0.0001)).all(),
                        "frame {frame} {} at {point:?} outside {tile:?}",
                        file.path
                    );
                }
            }
        }
    }

    #[test]
    fn heat_cools_in_canonical_seconds_and_replays_at_any_frame_rate() {
        let mut events = timeline::synthetic();
        events.truncate(1);
        let mut sim = Simulation::new(
            events,
            &Options {
                heat_half_life: 2.0,
                ..Options::default()
            },
            &Theme::neon(),
            &Mappings::default(),
        );
        sim.advance_to(30, 60);
        let id = sim.layout().files().next().unwrap().0;
        let hot = sim.layout().file(id).unwrap().heat;
        sim.advance_to(150, 60);
        let cooled = sim.layout().file(id).unwrap().heat;
        assert!((cooled - hot * 0.5).abs() < 0.000_01);
        sim.restart();
        sim.advance_to(75, 30);
        assert_eq!(sim.layout().file(id).unwrap().heat, cooled);
        sim.advance_to(30, 60);
        assert_eq!(sim.layout().file(id).unwrap().heat, hot);
        sim.set_theme(crate::theme::ThemeName::Phosphor.build());
        assert_eq!(sim.layout().file(id).unwrap().heat, hot);
    }

    #[test]
    fn calendar_mapping_spreads_busy_days_and_caps_idle_gaps() {
        let mut events = timeline::synthetic();
        events.truncate(5);
        for (event, seconds) in
            events.iter_mut().zip([0, 21_600, 43_200, 864_000, 885_600])
        {
            event.seconds = seconds;
        }
        assert_eq!(
            Schedule::new(&events, 2.0, 1.0).starts,
            [0.5, 1.0, 1.5, 2.5, 3.0]
        );
        assert_eq!(
            Schedule::new(&events, 1.0, 1.0).starts,
            [0.5, 0.75, 1.0, 2.0, 2.25]
        );
    }

    #[test]
    fn coincident_and_reversed_timestamps_remain_strictly_monotonic() {
        let mut events = timeline::synthetic();
        for (event, seconds) in events.iter_mut().zip([100, 100, 90, 95, 101]) {
            event.seconds = seconds;
        }
        for rate in [0.01, 0.5, 100.0] {
            let starts = Schedule::new(&events, rate, 1.0).starts;
            assert!(starts.windows(2).all(|pair| pair[1] > pair[0]));
            assert!(
                starts[..5]
                    .windows(2)
                    .all(|pair| pair[1] - pair[0] >= tick_seconds() - 0.0001)
            );
        }
        assert_eq!(Schedule::new(&[], 1.0, 1.0).starts, [] as [f32; 0]);
    }

    #[test]
    fn minimum_idle_cap_starts_at_most_one_commit_per_tick() {
        for idle in [tick_seconds(), 0.01] {
            let events = vec![timeline::synthetic()[0].clone(); 300];
            let mut sim = Simulation::new(
                events,
                &Options {
                    max_idle_seconds: idle,
                    ..Options::default()
                },
                &Theme::neon(),
                &Mappings::default(),
            );
            for frame in 1..400 {
                let previous = sim.state.next_event;
                sim.advance_to(frame, 60);
                assert!(sim.state.next_event - previous <= 1, "frame {frame}");
            }
            assert_eq!(sim.state.next_event, 300);
        }
    }

    #[test]
    fn whole_playback_frame_count_rejects_overflow() {
        let mut events = vec![timeline::synthetic()[0].clone(); 300];
        for (index, event) in events.iter_mut().enumerate() {
            event.seconds = i64::try_from(index).unwrap() * 86_400;
        }
        let schedule = Schedule::new(&events, 86_400.0, 86_400.0);
        assert!(
            frames_for(schedule.duration(), 240)
                .unwrap_err()
                .to_string()
                .contains("supported export range")
        );
        assert!(frames_for(schedule.duration(), 30).is_ok());
        assert_eq!(frames_for(Time::seconds(1.0), 30).unwrap(), 30);
    }

    #[test]
    fn an_end_tick_counts_whole_frames_at_any_rate() {
        assert_eq!(frames_for_ticks(756, 240).unwrap(), 3024);
        assert_eq!(frames_for_ticks(757, 25).unwrap(), 316);
        assert!(frames_for_ticks(u64::MAX / 240, 240).is_err());
    }

    #[test]
    fn rate_changes_preserve_state_and_progress_through_the_current_gap() {
        let mut sim = simulation();
        sim.advance_to(45, 60);
        let tick = sim.state.tick;
        let position = sim.state.avatars[0].position;
        let pulse_end = sim.state.pulses[0].end;
        sim.change_rate(0.5);
        assert_eq!(sim.state.tick, tick);
        assert_eq!(sim.state.next_event, 1);
        assert_eq!(sim.state.avatars[0].position, position);
        assert_eq!(sim.state.pulses[0].end, pulse_end);
        assert_eq!(sim.schedule.starts[0], LEAD_IN);
        assert_eq!(sim.schedule.starts[1], 1.0);
        assert!(sim.schedule.starts.windows(2).all(|pair| pair[1] > pair[0]));
        sim.change_rate(2.0);
        assert!((sim.schedule.starts[1] - Time::seconds(1.0)).abs() < 0.0001);
        assert!(sim.caption().contains("1x"));
        sim.restart();
        assert_eq!(
            sim.schedule.starts,
            Schedule::new(&sim.events, 0.5, 1.0).starts
        );
        assert_eq!(sim.state.next_event, 0);
    }

    #[test]
    fn future_timestamp_does_not_pin_later_calendar_gaps() {
        let mut events = timeline::synthetic();
        events.truncate(5);
        for (event, seconds) in
            events
                .iter_mut()
                .zip([0, 536_544_000, 86_400, 129_600, 172_800])
        {
            event.seconds = seconds;
        }
        assert_eq!(
            Schedule::new(&events, 2.0, 1.0).starts,
            [0.5, 1.5, 1.7, 2.7, 3.7]
        );
    }

    #[test]
    fn rate_controls_stop_at_multiplier_bounds() {
        for (factor, bound) in [(0.5, 0.125), (2.0, 8.0)] {
            let mut sim = simulation();
            let starts = sim.schedule.starts.clone();
            for _ in 0..32 {
                sim.change_rate(factor);
            }
            assert_eq!(sim.playback_multiplier(), bound);
            assert_eq!(sim.schedule.starts, starts);
            sim.change_rate(1.0 / factor);
            assert_eq!(sim.playback_multiplier(), bound / factor);
        }
    }

    #[test]
    fn dense_same_author_burst_bounds_live_beams() {
        let mut event = timeline::synthetic().remove(7);
        event.seconds = 0;
        let mut sim = Simulation::new(
            vec![event; 100],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let mut peak = 0;
        for frame in 0..400 {
            sim.advance_to(frame, 60);
            let beams = sim.beams().len();
            peak = peak.max(beams);
            assert!(beams <= BEAM_LIMIT * 2, "{beams} beams at frame {frame}");
        }
        assert!(peak > 0);
        assert_eq!(sim.commit(), 100);
    }

    #[test]
    fn dense_same_author_commits_bound_pulse_overlap() {
        let mut events = timeline::synthetic();
        events.truncate(2);
        events[1].seconds = events[0].seconds;
        let mut sim = Simulation::new(
            events,
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        sim.advance_to(36, 60);
        assert_eq!(sim.commit(), 2);
        assert_eq!(sim.state.pulses.len(), 2);
        assert!(
            sim.state.pulses[0].end
                <= sim.state.pulses[1].start + sim.state.fragment_pace.flight() + 0.3
        );
    }

    #[test]
    fn construction_preserves_selected_event_content() {
        let events: Vec<_> = ["First", "Second"]
            .into_iter()
            .map(|name| Event {
                author: timeline::Identity {
                    name: name.as_bytes().to_vec(),
                    email: name.as_bytes().to_vec(),
                },
                repository: 0,
                contributor: None,
                co_authors: Vec::new(),
                co_contributors: Vec::new(),
                commit: String::new(),
                seconds: 0,
                changes: vec![timeline::FileChange {
                    path: name.to_owned(),
                    kind: Kind::Added,
                    insertions: 1,
                    removals: 0,
                    samples: Vec::new(),
                }],
            })
            .collect();
        let total = events.len();
        let path = format!("0/{}", events[1].changes[0].path);
        let author = events[1].author.name.clone();
        let sim = Simulation::new(
            events,
            &Options {
                start: 1,
                commits: 1,
                ..Options::default()
            },
            &Theme::neon(),
            &Mappings::default(),
        );
        assert_eq!(sim.total(), total);
        assert_eq!(sim.commit(), 1);
        assert_eq!(sim.events.len(), 1);
        assert_eq!(sim.events[0].changes[0].path, path);
        assert_eq!(sim.events[0].author.name, author);
    }

    #[test]
    fn unread_commits_keep_whole_history_numbers() {
        let sim = Simulation::new(
            timeline::synthetic(),
            &Options {
                start: 1,
                commits: 1,
                unread: Unread {
                    before: 5,
                    after: vec![3],
                },
                ..Options::default()
            },
            &Theme::neon(),
            &Mappings::default(),
        );
        assert_eq!(sim.commit(), 6);
        assert_eq!(sim.total(), timeline::synthetic().len() + 8);
    }

    #[test]
    fn unread_commits_leave_travel_padding_in_place() {
        let histories = (0..2)
            .map(|repository| {
                let mut events = timeline::synthetic();
                for event in &mut events {
                    event.seconds += i64::from(repository == 1) * 86_400;
                }
                crate::repositories::namespace(&mut events, repository);
                events
            })
            .collect();
        let events = crate::repositories::merge(histories);
        let unpadded = Schedule::new(&events[2..], 0.5, 1.0);
        let plan = |before| {
            Simulation::with_repositories(
                events.clone(),
                &Options {
                    start: 2,
                    unread: Unread {
                        before,
                        after: vec![0, 0],
                    },
                    ..Options::default()
                },
                &Theme::neon(),
                &Mappings::default(),
                &["first".into(), "second".into()],
            )
        };
        let (read, unread) = (plan(0), plan(7));
        assert_ne!(read.scheduled_starts(), unpadded.starts);
        assert_eq!(read.scheduled_starts(), unread.scheduled_starts());
        assert_eq!(unread.commit(), read.commit() + 7);
    }

    fn assert_clearance(sim: &Simulation) {
        nitpicks::assert_labels_and_beams(sim);
        assert_eq!(sim.view(), (Vec3::ZERO, framing::DISTANCE));
        assert!(
            sim.avatars()
                .iter()
                .all(|a| a.position.y == MIN_AVATAR_HEIGHT)
        );
    }

    #[test]
    fn overhead_avatar_bounds_remain_finite_during_growth_and_rewind() {
        for start in [0, 8] {
            let mut sim = Simulation::new(
                timeline::synthetic(),
                &Options {
                    start,
                    ..Options::default()
                },
                &Theme::neon(),
                &crate::contributors::Mappings::default(),
            );
            for _ in 0..2 {
                for frame in 0..frames_for(sim.duration(), 60).unwrap() {
                    sim.advance_to(frame, 60);
                    assert_clearance(&sim);
                }
            }
        }
    }

    #[test]
    fn overhead_fit_preserves_fixed_avatar_height() {
        let mut events = timeline::synthetic();
        events[7]
            .changes
            .extend((0..256).map(|index| timeline::FileChange {
                path: format!("tests/fixtures/clearance_{index}.yaml"),
                kind: Kind::Added,
                insertions: 1,
                removals: 0,
                samples: Vec::new(),
            }));
        let mut sim = Simulation::new(
            events,
            &Options::default(),
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        sim.advance_to(13, 2);
        assert_clearance(&sim);
        assert!(
            sim.avatars()
                .iter()
                .all(|a| a.position.y == MIN_AVATAR_HEIGHT)
        );
    }

    #[test]
    fn clearance_uses_themes_file_and_avatar_dimensions() {
        let mut theme = Theme::neon();
        theme.file_glyph_transform.translation.y = 0.4;
        theme.file_glyph_transform.scale *= 2.0;
        theme.avatar_radius = 0.5;
        theme.label_height = 28.0;
        let mut sim = Simulation::new(
            timeline::synthetic(),
            &Options::default(),
            &theme,
            &crate::contributors::Mappings::default(),
        );
        for frame in 0..frames_for(sim.duration(), 30).unwrap() {
            sim.advance_to(frame, 30);
            assert_clearance(&sim);
        }
    }

    #[test]
    fn playback_is_independent_of_frame_rate_and_replay() {
        for (start, seconds_per_day) in [(0, 0.01), (0, 0.5), (0, 20.0), (8, 0.5)] {
            let events = timeline::synthetic();
            let options = Options {
                seconds_per_day,
                start,
                ..Options::default()
            };
            let mut thirty = Simulation::new(
                events.clone(),
                &options,
                &Theme::neon(),
                &crate::contributors::Mappings::default(),
            );
            let mut sixty = Simulation::new(
                events.clone(),
                &options,
                &Theme::neon(),
                &crate::contributors::Mappings::default(),
            );
            thirty.advance_to(90, 30);
            sixty.advance_to(180, 60);
            let positions = |sim: &Simulation| {
                (
                    sim.layout()
                        .files()
                        .map(|(id, file)| (id, file.position))
                        .collect::<Vec<_>>(),
                    sim.layout()
                        .dirs()
                        .map(|(index, dir)| (index, dir.position))
                        .collect::<Vec<_>>(),
                )
            };
            assert_eq!(positions(&thirty), positions(&sixty));
            let reference = positions(&thirty);
            thirty.advance_to(200, 30);
            thirty.advance_to(90, 30);
            assert_eq!(positions(&thirty), reference);
        }
    }

    #[test]
    fn crowded_commit_aggregates_beams() {
        let mut sim = simulation();
        let crowded = sim.schedule.starts[7] + 0.5;
        sim.advance_to(frame_at(crowded.lossy_secs_f32()), 30);
        let beams = sim.beams();
        assert!(!beams.is_empty());
        assert!(beams.len() <= BEAM_LIMIT * 2, "{} beams", beams.len());
        assert!(
            sim.labels()
                .iter()
                .any(|label| label.text.ends_with("fixtures/"))
        );
    }

    fn paths_simulation(paths: &[String]) -> Simulation {
        let mut event = timeline::synthetic().remove(0);
        event.changes = paths
            .iter()
            .map(|path| timeline::FileChange {
                path: path.clone(),
                kind: Kind::Added,
                insertions: 1,
                removals: 0,
                samples: Vec::new(),
            })
            .collect();
        let mut sim = Simulation::new(
            vec![event],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        sim.advance_to(120, 60);
        sim
    }

    #[test]
    fn landmarks_rank_large_nested_subtrees_before_small_top_level_directories() {
        let paths: Vec<_> = (0..30)
            .flat_map(|dir| (0..8).map(move |file| format!("big/d{dir}/f{file}.rs")))
            .chain(std::iter::once("small/one.rs".to_owned()))
            .collect();
        let sim = paths_simulation(&paths);
        let labels: Vec<_> = sim
            .labels()
            .into_iter()
            .filter(|l| l.kind == LabelKind::Landmark)
            .collect();
        assert!(labels.iter().any(|label| label.text == "small/"));
        assert!(
            labels
                .iter()
                .filter(|label| label.text.starts_with('d'))
                .count()
                >= DIR_LABELS - 1
        );
        assert_eq!(labels[0].text, "big/");
        assert_eq!(labels.last().unwrap().text, "small/");
    }

    #[test]
    fn extra_top_level_landmarks_are_capped_and_hidden_directories_are_excluded() {
        let paths: Vec<_> = (0..50).map(|index| format!("d{index}/file.rs")).collect();
        let sim = paths_simulation(&paths);
        assert_eq!(
            sim.labels()
                .iter()
                .filter(|label| label.kind == LabelKind::Landmark)
                .count(),
            DIR_LABELS + 4
        );
        let visible = sim.labels_for_detail(|index| index == 1);
        let landmarks: Vec<_> = visible
            .iter()
            .filter(|label| label.kind == LabelKind::Landmark)
            .collect();
        assert_eq!(landmarks.len(), 1);
        assert_eq!(landmarks[0].text, "d0/");
    }

    #[test]
    fn whole_history_plays_out() {
        let mut sim = simulation();
        let end = frame_at(sim.duration().lossy_secs_f32());
        sim.advance_to(end, 30);
        assert_eq!(sim.commit(), sim.total());
        assert!(sim.beams().is_empty());
        let paths: Vec<_> = sim
            .layout()
            .files()
            .map(|(_, file)| file.path.as_str())
            .collect();
        assert!(paths.contains(&"0/src/scene/layout.rs"));
        assert!(!paths.contains(&"0/src/old.rs"));
        assert!(!paths.contains(&"0/docs/guide.md"));
    }

    #[test]
    fn starting_mid_history_shows_prior_files_settled() {
        let options = Options {
            start: 8,
            commits: 2,
            ..Options::default()
        };
        let sim = Simulation::new(
            timeline::synthetic(),
            &options,
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        assert_eq!(sim.commit(), 8);
        assert!(sim.layout().present_files() > 60);
        assert!(sim.layout().files().all(|(_, file)| file.presence >= 1.0));
        assert!(sim.layout().files().all(|(_, file)| file.heat == 0.0));
    }

    #[test]
    fn an_empty_boundary_settles_nothing() {
        let mut events = timeline::synthetic();
        events[0].changes.clear();
        let options = Options {
            start: 1,
            commits: 1,
            ..Options::default()
        };
        let sim = Simulation::new(
            events,
            &options,
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        assert_eq!(sim.commit(), 1);
        assert_eq!(sim.layout().present_files(), 0);
    }

    #[test]
    fn dates_are_utc_calendar_days() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(date(1_700_000_000), "2023-11-14");
    }

    #[test]
    fn preview_frame_counts_elapsed_frames() {
        assert_eq!(preview_frame(Duration::from_secs(1), 30), 30);
        assert_eq!(preview_frame(Duration::from_millis(1500), 60), 90);
    }

    fn frame_at(seconds: f32) -> u32 {
        let frames = (seconds * 30.0).ceil();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "test times are short and positive"
        )]
        let frames = frames as u32;
        frames
    }
}
