use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    ops::Range,
    time::Duration,
};

use bevy::math::{Vec2, Vec3};

use crate::{
    contributors::{Contributor, Key, Mappings},
    layout::Layout,
    theme::{BeamStyle, Geometry, Theme},
    timeline::{Event, Kind},
};

pub(crate) mod caption;
pub(crate) mod framing;
mod lifecycle;
#[cfg(test)]
mod measurement;
mod names;
#[cfg(test)]
mod nitpicks;
pub(crate) mod occlusion;
#[cfg(test)]
mod pacing_tests;
pub(crate) mod quality;
pub(crate) mod schedule;
mod slots;
mod titles;
mod travel;
pub(super) use names::rect as label_rect;
use schedule::Schedule;
pub(crate) mod treemap;

pub(super) const TICK_RATE: u32 = 60;
pub(super) const PREVIEW_FPS: u16 = 30;
const LEAD_IN: f32 = 0.5;
const TAIL: f32 = 2.5;
const PULSE_SECONDS: f32 = 1.3;
const FRAGMENT_SPEED: f32 = 1.1;
const FRAGMENT_FLIGHT: f32 = 1.0 / FRAGMENT_SPEED;
const ADD_DELAY: f32 = 0.2;
const AGGREGATED_ARRIVAL_FLIGHT: f32 = 0.8;
const DELETE_DELAY: f32 = 0.0;
const MIN_AVATAR_HEIGHT: f32 = 3.2;

const AVATAR_IDLE: f32 = 5.0;
const ARRIVAL_SCALE: f32 = 1.15;
const BEAM_LIMIT: usize = 24;
const FRAGMENT_TARGETS: usize = 24;
const SETTLE_TICKS: u32 = 1200;
const DIR_LABELS: usize = 20;

fn check_travel_pass(pass: u32) -> anyhow::Result<()> {
    anyhow::ensure!(
        pass <= 32,
        "avatar travel planning did not converge within 32 passes; shorten the history or reduce the playback times"
    );
    Ok(())
}

pub(super) struct Options {
    pub contributor_inactivity_days: u32,
    pub repository_ends: Vec<Option<i64>>,
    pub repository_archived: Vec<bool>,
    pub heat_half_life: f32,
    pub seconds_per_day: f32,
    pub max_idle_seconds: f32,
    pub start: usize,
    pub commits: Option<usize>,
    pub travel_cap: TravelCap,
    pub bot_travel: BotTravel,
    pub avatar_max_speed: f32,
    pub avatar_size: f32,
    pub gutter: f32,
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
    fn selected_range(&self, events: usize) -> Range<usize> {
        let start = self.start.min(events);
        let end = self
            .commits
            .map_or(events, |count| start.saturating_add(count).min(events));
        start..end
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            contributor_inactivity_days: 60,
            repository_ends: Vec::new(),
            repository_archived: Vec::new(),
            heat_half_life: 3.0,
            seconds_per_day: 0.5,
            max_idle_seconds: 1.0,
            start: 0,
            commits: None,
            travel_cap: TravelCap::Seconds(0.4),
            bot_travel: BotTravel::Fade,
            avatar_max_speed: 480.0,
            avatar_size: 2.0,
            gutter: 4.0,
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
    start: f32,
    end: f32,
    author: usize,
    targets: Vec<(Target, f32, u32, u32)>,
    aggregated: bool,
    samples: Vec<(u64, crate::diff_samples::Sample)>,
}

impl Pulse {
    fn directory_target(&self, mut dir: usize, layout: &Layout) -> Option<Target> {
        loop {
            let target = Target::Dir(dir);
            if self.targets.iter().any(|&(drawn, ..)| drawn == target) {
                return Some(target);
            }
            dir = layout.dir(dir).parent?;
        }
    }

    fn fragments(
        &self,
        age: f32,
        envelope: f32,
        beam: (BeamStyle, Vec3, Vec3),
        target: Target,
        change: (u32, bool),
    ) -> impl Iterator<Item = Fragment> {
        let (lines, rising) = change;
        diff_fragments(age, envelope, beam, lines, rising)
            .enumerate()
            .filter(move |(piece, _)| self.fragment_arrives(age, *piece, lines))
            .map(move |(_, fragment)| fragment.with_beam(self.author, target))
    }
    fn fragment_arrives(&self, age: f32, piece: usize, lines: u32) -> bool {
        let pieces = lines.bit_width().min(3);
        let offset = count_u32(u32::try_from(piece).expect("three fragments"))
            / count_u32(pieces);
        let launch =
            ((age * FRAGMENT_SPEED + offset).floor() - offset) / FRAGMENT_SPEED;
        launch + FRAGMENT_FLIGHT <= self.end - self.start - 0.3 + 0.00001
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

pub(super) struct DiffLine {
    pub fragment: usize,
    pub detail: f32,
    pub position: Vec3,
    pub scale: f32,
    pub sample: crate::diff_samples::Sample,
    pub alpha: f32,
}

#[derive(Clone, Debug)]
pub(super) struct Avatar {
    expires: f32,
    arrived: f32,
    pub lifecycle_scale: f32,
    pub grey: f32,
    pub key: Key,
    pub name: String,
    pub portrait: Option<std::sync::Arc<crate::avatars::Portrait>>,
    pub position: Vec3,
    target: Vec3,
    repository: usize,
    touched: Vec<u64>,
    last_active: f32,
    pub presence: f32,
    // Travel fades do not release a reserved parking slot.
    slot_presence: f32,
    pub projection_scale: f32,
    activity: f32,
    name_visibility: names::Visibility,
    commit: usize,
    slot: Option<slots::Placement>,
    slot_origin: Option<(usize, slots::Dock, Vec2)>,
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

pub(super) struct Beam {
    pub author: usize,
    pub from: Vec3,
    pub to: Vec3,
    pub width: f32,
}

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
    Repository,
    Avatar,
    Detail,
    Landmark,
    Activity,
}

#[derive(Clone)]
pub(super) struct Label {
    pub position: Vec3,
    pub text: String,
    pub kind: LabelKind,
    pub scale: f32,
    pub opacity: f32,
    pub priority: f32,
}

#[derive(Clone, Debug)]
struct State {
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
    slot_records: BTreeMap<usize, slots::Record>,
    titles: BTreeMap<usize, titles::Title>,
}

/// Deterministic playback stepped at a fixed tick rate, independent of output frame rate.
pub(super) struct Simulation {
    file_ids: HashMap<String, FileIdentity>,
    first_arrivals: Vec<bool>,
    departures: Vec<f32>,
    repository_ends: Vec<f32>,
    pub caption_style: caption::Style,
    caption_plan: Vec<caption::Transition>,
    packing: crate::layout::packing::Packing,
    envelopes: Vec<slots::Envelope>,
    pub repository_boundary: crate::repositories::Boundary,
    pub(super) collapse_directories: bool,
    pub(super) diff_text_distance: f32,
    pub repository_label_plane: crate::repositories::LabelPlane,
    events: Vec<Event>,
    offset: usize,
    total: usize,
    schedule: Schedule,
    playback_multiplier: f32,
    heat_half_life: f32,
    initial: State,
    state: State,
    theme: Theme,
    travel: travel::Plan,
    visual_updates: bool,
    title_plan: Vec<Vec<u8>>,
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
        repositories: usize,
    ) -> anyhow::Result<()> {
        let schedule = Schedule::with_padding(
            &events[options.selected_range(events.len())],
            options.seconds_per_day,
            options.max_idle_seconds,
            |_| match options.travel_cap {
                TravelCap::Seconds(_) => options.travel_cap.ticks(),
                TravelCap::None => 0,
            },
        );
        Self::check_schedule(events, options, repositories, &schedule)
    }

    fn check_schedule(
        events: &[Event],
        options: &Options,
        repositories: usize,
        schedule: &Schedule,
    ) -> anyhow::Result<()> {
        let (_, endings) = lifecycle::plan(
            events,
            schedule,
            options,
            options.selected_range(events.len()),
        );
        let ticks = frames_for(lifecycle::duration(schedule, &endings), 60)?;
        let authors = events[options.selected_range(events.len())]
            .iter()
            .map(|e| (e.author.name.as_slice(), e.author.email.as_slice()))
            .collect::<std::collections::HashSet<_>>()
            .len();
        anyhow::ensure!(
            repositories <= crate::repositories::MAX_REPOSITORIES
                && ticks <= 216_000
                && usize::try_from(ticks)
                    .ok()
                    .and_then(|ticks| ticks.checked_mul(repositories))
                    .is_some_and(|samples| samples <= 2_000_000)
                && usize::try_from(ticks)
                    .ok()
                    .and_then(|t| (t + 1).checked_mul(authors))
                    .is_some_and(|n| n <= 4_000_000),
            "multi-repository planning exceeds 30 repositories, 216000 ticks, 2000000 region samples or 4000000 avatar poses including worst-case travel padding; use fewer repositories, shorten the history or reduce the playback times"
        );
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn scheduled_starts(&self) -> &[f32] {
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
        Self::check_repository_plan(&events, options, repositories.len())?;
        let mut simulation =
            Self::build(events.clone(), options, theme, mappings, &[], repositories);
        let mut padding = simulation.travel_padding(options);
        let mut pass = 0;
        let mut compactions = 0;
        loop {
            pass += 1;
            check_travel_pass(pass)?;
            if pass > 1 || padding.iter().any(|&(_, ticks)| ticks > 0) {
                let schedule = Schedule::with_padding(
                    &events[options.selected_range(events.len())],
                    options.seconds_per_day,
                    options.max_idle_seconds,
                    |index| {
                        padding
                            .iter()
                            .find(|&&(event, _)| event == options.start + index)
                            .map_or(0, |&(_, ticks)| ticks)
                    },
                );
                Self::check_schedule(&events, options, repositories.len(), &schedule)?;
                let preferences = std::mem::take(&mut simulation.travel).preferences();
                simulation = Self::build(
                    events.clone(),
                    options,
                    theme,
                    mappings,
                    &padding,
                    repositories,
                );
                simulation.travel = preferences;
            }
            simulation.plan_travel(options, &padding);
            eprintln!(
                "travel_replan pass={} fades={} padding_seconds={}",
                pass,
                simulation.travel.stats.faded,
                simulation.travel.stats.padding_seconds
            );
            let adjusted = simulation.adjust_travel_padding(options, &padding);
            let settled = adjusted == padding
                || (options.travel_cap != TravelCap::None && pass >= 4);
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
                if compactions == 1 && options.travel_cap == TravelCap::None {
                    compactions = 2;
                    padding = simulation.compact_travel_padding(options, &padding);
                    continue;
                }
                break;
            }
            compactions = 1;
            padding = simulation.compact_travel_padding(options, &padding);
        }
        let Range { start, end } = options.selected_range(events.len());
        events.truncate(end);
        events.drain(..start);
        simulation.events = events;
        simulation.caption_plan =
            caption::plan(&simulation.schedule, &simulation.events);
        simulation.plan_titles();
        Ok(simulation)
    }

    fn build(
        mut events: Vec<Event>,
        options: &Options,
        theme: &Theme,
        mappings: &Mappings,
        padding: &[(usize, u32)],
        repositories: &[String],
    ) -> Self {
        if events.iter().any(|event| event.contributor.is_none()) {
            mappings.apply(&mut events);
        }
        let Range { start, end } = options.selected_range(events.len());
        let schedule = Schedule::with_padding(
            &events[start..end],
            options.seconds_per_day,
            options.max_idle_seconds,
            |index| schedule::padding_at(padding, start + index),
        );
        let layout_starts = Schedule::with_padding(
            &events,
            options.seconds_per_day,
            options.max_idle_seconds,
            |index| schedule::padding_at(padding, index) * u32::from(index >= start),
        );
        let mut state = State {
            region_depth: 0.0,
            tick: 0,
            layout: Layout::scheduled_repositories(
                &events,
                &layout_starts.starts,
                repositories,
            ),
            avatars: Vec::new(),
            authors: HashMap::new(),
            next_event: 0,
            pulses: VecDeque::new(),
            focus: Vec3::ZERO,
            distance: framing::DISTANCE,
            geometry: theme.geometry(),
            avatar_scale: 1.0,
            avatar_size: options.avatar_size
                * repository_avatar_factor(repositories.len()),
            avatar_max_speed: options.avatar_max_speed,
            gutter: options.gutter,
            slot_records: BTreeMap::new(),
            titles: BTreeMap::new(),
        };
        state.apply_history(&events[..start]);
        if start > 0 {
            state.layout.plan_time(
                layout_starts.starts[start.min(layout_starts.starts.len() - 1)]
                    - schedule.starts.first().copied().unwrap_or(LEAD_IN),
            );
            state.layout.settle(SETTLE_TICKS, tick_seconds());
            state.avatars.clear();
            state.authors.clear();
        }
        let total = events.len();
        let prior_active = prior_activity(&events[..start], repositories.len());
        let (departures, repository_ends) =
            lifecycle::plan(&events, &schedule, options, start..end);
        let first_arrivals = lifecycle::arrivals(&events, start..end);
        events.truncate(end);
        events.drain(..start);
        let mut simulation = Self {
            file_ids: state
                .layout
                .files()
                .map(|(id, file)| (file.path.clone(), FileIdentity::Unique(id)))
                .collect(),
            first_arrivals,
            departures,
            repository_ends,
            caption_style: caption::Style::default(),
            caption_plan: Vec::new(),
            packing: crate::layout::packing::Packing::default(),
            envelopes: Vec::new(),
            repository_boundary: crate::repositories::Boundary::default(),
            collapse_directories: true,
            diff_text_distance: 1.0,
            repository_label_plane: crate::repositories::LabelPlane::default(),
            events,
            offset: start,
            total,
            schedule,
            playback_multiplier: 1.0,
            heat_half_life: options.heat_half_life,
            initial: state.clone(),
            state,
            theme: theme.clone(),
            travel: travel::Plan::default(),
            visual_updates: true,
            title_plan: Vec::new(),
        };
        let snapshots = simulation
            .plan_repositories(prior_active, options.travel_cap == TravelCap::None);
        simulation.plan_avatar_envelopes(snapshots);
        simulation
    }

    fn plan_repositories(
        &mut self,
        mut active: Vec<bool>,
        uncapped: bool,
    ) -> Option<slots::Snapshots> {
        let mut activation = vec![u32::MAX; active.len()];
        for (repository, live) in active.iter().enumerate() {
            if *live {
                activation[repository] = 0;
            }
        }
        for (index, event) in self.events.iter().enumerate() {
            let arrival = travel::seconds_ticks(self.schedule.starts[index], false);
            let padding = self.schedule.padding(index);
            activation[event.repository] = activation[event.repository].min(
                arrival
                    .saturating_sub(padding)
                    .saturating_add(crate::layout::packing::BIRTH_TICKS)
                    .min(arrival),
            );
        }
        let mut snapshots = Some(slots::Snapshots::default());
        for (_, file) in self.layout().files() {
            active[self.layout().dir(file.dir).repository] = true;
        }
        if !active.iter().any(|&live| live)
            && let Some(event) = self.events.first()
        {
            active[event.repository] = true;
        }
        let geometry = self.state.geometry;
        let mut samples = vec![self.layout().local_regions(&active, geometry)];
        let mut sampled_events = 0;
        let ticks = frames_for(self.duration(), 60)
            .expect("multi-repository playback fits the supported frame range");
        for _ in 0..ticks {
            self.tick_layout();
            for event in &self.events[sampled_events..self.state.next_event] {
                active[event.repository] = true;
            }
            sampled_events = self.state.next_event;
            samples.push(self.layout().local_regions(&active, geometry));
            if snapshots
                .as_mut()
                .is_some_and(|cache| !cache.capture(&self.state))
            {
                snapshots = None;
            }
        }
        if uncapped {
            for (index, event) in self.events.iter().enumerate() {
                let arrival = usize::try_from(
                    frames_for(self.schedule.starts[index], 60)
                        .expect("bounded service tick"),
                )
                .expect("service tick fits usize");
                let begin = arrival.saturating_sub(
                    usize::try_from(self.schedule.padding(index))
                        .expect("bounded padding"),
                );
                let files = samples[arrival][event.repository].files;
                for row in &mut samples[begin..arrival] {
                    row[event.repository].files = files;
                }
            }
        }
        self.packing = crate::layout::packing::Packing::with_gutter(
            &samples,
            self.state.gutter,
            uncapped.then_some(activation.as_slice()),
        )
        .with_endings(
            &self.repository_ends,
            lifecycle::REPOSITORY_EXIT,
            lifecycle::REFLOW,
        );
        self.state = self.initial.clone();
        self.state.region_depth = framing::depth_bound();
        if let Some((regions, offsets, scales)) = self.packing.sample(0) {
            self.state.layout.place_regions(regions, offsets, &scales);
        }
        self.state.focus = Vec3::ZERO;
        self.state.distance = framing::DISTANCE;
        self.initial = self.state.clone();
        snapshots
    }
    pub(super) fn duration(&self) -> f32 {
        lifecycle::duration(&self.schedule, &self.repository_ends)
    }

    pub(super) fn repository_effect(&self, repository: usize) -> (f32, f32, f32) {
        let end = self
            .repository_ends
            .get(repository)
            .copied()
            .unwrap_or(f32::INFINITY);
        let u = lifecycle::progress(self.seconds(), end, lifecycle::REPOSITORY_EXIT);
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
        match file.state {
            crate::layout::Presence::Arriving => {
                (self.arrival_position(id, file, position), grey, alpha)
            }
            crate::layout::Presence::Leaving => (
                position + Vec3::Y * (1.0 - file.presence) * self.scale(),
                1.0,
                alpha * file.presence,
            ),
            crate::layout::Presence::Present => (position, grey, alpha),
        }
    }

    fn arrival_position(&self, id: u64, file: &crate::layout::File, to: Vec3) -> Vec3 {
        let Some(avatar) = self.state.avatars.get(file.author) else {
            return to;
        };
        let endpoint = self
            .state
            .pulses
            .iter()
            .rev()
            .find(|pulse| {
                pulse.author == file.author && pulse.touched.binary_search(&id).is_ok()
            })
            .filter(|pulse| pulse.aggregated)
            .and_then(|pulse| pulse.directory_target(file.dir, self.layout()))
            .and_then(|target| self.state.point(target));
        let Some(endpoint) = endpoint else {
            return self
                .theme
                .beam_style
                .point(avatar.position, to, file.presence);
        };
        let flight = (file.presence / AGGREGATED_ARRIVAL_FLIGHT).min(1.0);
        let settle = ((file.presence - AGGREGATED_ARRIVAL_FLIGHT)
            / (1.0 - AGGREGATED_ARRIVAL_FLIGHT))
            .max(0.0);
        self.theme
            .beam_style
            .point(avatar.position, endpoint, flight)
            .lerp(to, settle)
    }

    pub(super) fn restart(&mut self) {
        self.state = self.initial.clone();
    }

    pub(super) fn change_rate(&mut self, factor: f32) {
        self.playback_multiplier =
            (self.playback_multiplier * factor).clamp(0.125, 8.0);
    }

    pub(super) fn playback_multiplier(&self) -> f32 {
        self.playback_multiplier
    }

    pub(super) fn seconds(&self) -> f32 {
        tick_time(self.state.tick)
    }

    pub(super) fn advance_to(&mut self, frame: u32, fps: u16) {
        let target = Schedule::frame_tick(frame, fps);
        if target < self.state.tick {
            self.state = self.initial.clone();
        }
        while self.state.tick < target {
            self.tick();
        }
    }

    pub(crate) fn tick_layout(&mut self) {
        let placement = self.packing.sample(self.state.tick + 1);
        self.tick_local_layout();
        if let Some((regions, offsets, scales)) = placement {
            self.state.layout.place_regions(regions, offsets, &scales);
        }
    }

    fn tick_local_layout(&mut self) {
        let canonical = self.state.tick + 1;
        self.state.layout.remove_offsets();
        let plan_time = self.schedule.layout_time(tick_time(canonical));
        let state = &mut self.state;
        state.tick = canonical;

        state.layout.cool_heat(tick_seconds(), self.heat_half_life);
        while let Some(event) = self.events.get(state.next_event) {
            let Some(start) = self.schedule.start(state.next_event, canonical) else {
                break;
            };
            state.begin(event, start, &mut self.file_ids);
            if let Some(&author) = event
                .contributor
                .as_ref()
                .and_then(|c| state.authors.get(&c.key))
            {
                state.avatars[author].expires = self.departures[state.next_event];
                if !self.first_arrivals[state.next_event]
                    && state.avatars[author].arrived >= start
                {
                    state.avatars[author].arrived = f32::NEG_INFINITY;
                    state.avatars[author].lifecycle_scale = 1.0;
                }
            }
            state.next_event += 1;
        }
        let dt = tick_seconds();
        state.layout.sample_plan(plan_time, dt);
        state.layout.step(dt);
    }

    fn tick(&mut self) {
        self.tick_layout();
        let state = &mut self.state;
        let now = tick_time(state.tick);
        let dt = tick_seconds();
        state.pulses.retain(|pulse| now < pulse.end);
        let active = state
            .avatars
            .iter()
            .filter(|avatar| now < avatar.expires)
            .count();
        let scale = names::scale(active).min(
            crate::layout::packing::gutters(count(state.layout.repositories.len())).y
                / 32.0,
        );
        state.avatar_scale += (scale - state.avatar_scale) * (1.0 - (-dt / 0.6).exp());
        state.place_slots_planned(&self.envelopes, now, &self.travel);
        let targets: Vec<_> = state
            .avatars
            .iter()
            .map(|a| a.slot.as_ref().map_or(a.position, slots::Placement::world))
            .collect();
        for (index, (avatar, target)) in
            state.avatars.iter_mut().zip(targets).enumerate()
        {
            let travel = self.travel.sample(index, state.tick);
            avatar.activity *= (-dt / 3.0).exp();
            avatar.position = target;
            if let Some((point, _)) = travel {
                avatar.position = slots::world(point);
            }
            avatar.projection_scale = slots::projection_scale(avatar.position);
            let service_visible = now < avatar.expires;
            let service_fade = if service_visible { dt / 0.4 } else { -dt / 1.2 };
            avatar.slot_presence =
                (avatar.slot_presence + service_fade).clamp(0.0, 1.0);
            let visible = service_visible;
            let fade = if visible {
                dt / 0.4
            } else {
                -dt / lifecycle::EXIT
            };
            avatar.presence = (avatar.presence + fade).clamp(0.0, 1.0);
            avatar.grey = lifecycle::progress(now, avatar.expires, 0.4);
            avatar.lifecycle_scale = 1.0
                + (ARRIVAL_SCALE - 1.0)
                    * (1.0 - lifecycle::progress(now, avatar.arrived + 0.4, 0.8));
            if let Some((_, opacity)) = travel {
                avatar.presence = avatar.presence.min(opacity);
            }
        }
        state.place_view();
        if self.visual_updates {
            self.place_titles(dt);
            let texts: Vec<_> = self
                .labels_for_detail(|_| true)
                .iter()
                .filter(|l| l.kind != LabelKind::Avatar)
                .map(|l| titles::label_bounds(self, l))
                .chain(caption::frame_text_rects())
                .collect();
            self.state.place_names(dt, &self.travel, &texts);
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
        (self.state.focus, self.state.distance)
    }

    pub(super) fn view_for_projection(&self, fov: f32, viewport: Vec2) -> (Vec3, f32) {
        let (focus, distance) = self.view();
        if fov == framing::FOV && viewport == framing::VIEWPORT {
            return (focus, distance);
        }
        let ratio = ((framing::FOV * 0.5).tan() / (fov * 0.5).tan())
            .max(
                (framing::FOV * 0.5).tan()
                    * (framing::VIEWPORT.x / framing::VIEWPORT.y)
                    / ((fov * 0.5).tan() * viewport.x / viewport.y),
            )
            .max(1.0);
        let depth = self.state.region_depth;
        let distance = distance * ratio + depth * (ratio - 1.0);
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

    pub(super) fn scale(&self) -> f32 {
        self.state.scale()
    }

    #[cfg(test)]
    pub(super) fn avatar_scale(&self) -> f32 {
        self.state.avatar_scale
    }

    pub(super) fn avatar_disc_scale(&self) -> f32 {
        self.state.avatar_scale * self.state.avatar_size
    }

    pub(super) fn commit(&self) -> usize {
        self.offset + self.state.next_event
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
            let from = self.state.avatars[pulse.author].position;
            for &(target, weight, ..) in &pulse.targets {
                if let Some(to) = self.state.point(target) {
                    beams.push(Beam {
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

    fn pulse_beams_visible(&self, pulse: &Pulse) -> bool {
        self.state.avatars[pulse.author].repository == pulse.repository
            && !self.travel.travelling(pulse.author, self.state.tick)
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
            let from = self.state.avatars[pulse.author].position;
            for &(target, _, insertions, removals) in
                pulse.targets.iter().take(FRAGMENT_TARGETS)
            {
                let Some(to) = self.state.point(target) else {
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

    fn repository_labels(&self) -> impl Iterator<Item = Label> {
        self.state
            .layout
            .repositories
            .iter()
            .enumerate()
            .filter(|(_, r)| r.live)
            .flat_map(move |(index, region)| {
                let height = match self.repository_label_plane {
                    crate::repositories::LabelPlane::Files => 0.15,
                    crate::repositories::LabelPlane::Raised => framing::LABEL_Y,
                };
                let positions: Vec<_> = if region.tile.is_some() {
                    self.state
                        .titles
                        .get(&index)
                        .into_iter()
                        .flat_map(|t| t.positions(height).map(|(p, a)| (p, t.scale, a)))
                        .collect()
                } else {
                    let mut point = framing::label_position(region);
                    point.y = height;
                    vec![(point, 1.0, 1.0)]
                };
                positions
                    .into_iter()
                    .map(move |(position, scale, opacity)| Label {
                        position,
                        text: region.label.clone(),
                        kind: LabelKind::Repository,
                        scale,
                        opacity: opacity * self.repository_effect(index).1,
                        priority: 0.0,
                    })
            })
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
            .map(|(index, dir)| Label {
                position: ground(centroids[index] / count(subtree[index].max(1))),
                text: format!("{}/", dir.name()),
                kind: LabelKind::Landmark,
                scale: 1.0,
                opacity: self.repository_effect(dir.repository).1,
                priority: 0.0,
            })
            .collect();
        labels.extend(self.repository_labels());
        for pulse in &self.state.pulses {
            if pulse.targets.len() > 8 && !pulse.aggregated {
                continue;
            }
            for &(target, ..) in &pulse.targets {
                if permanent.contains(&target) {
                    continue;
                }
                let text = match target {
                    Target::File(id) => {
                        layout.file(id).map(|file| file.name().to_owned())
                    }
                    Target::Dir(index) => {
                        Some(format!("{}/", layout.directory_label(index)))
                    }
                };
                if let (Some(text), Some(position)) = (text, self.state.point(target)) {
                    labels.push(Label {
                        position: position + Vec3::new(-0.2, 0.0, 0.45),
                        text,
                        kind: LabelKind::Activity,
                        scale: 1.0,
                        opacity: 1.0,
                        priority: 0.0,
                    });
                }
            }
        }
        for avatar in &self.state.avatars {
            if avatar.presence > 0.0 {
                labels.push(self.state.avatar_label(avatar));
            }
        }
        labels
    }
}

impl State {
    fn apply_history(&mut self, events: &[Event]) {
        for event in events {
            let author = self.author(
                event
                    .contributor
                    .as_ref()
                    .expect("contributors are resolved"),
                0.0,
            );
            for change in &event.changes {
                apply(&mut self.layout, change, author);
            }
        }
    }

    fn avatar_radius(&self) -> f32 {
        6.5 * self.avatar_scale * self.avatar_size * ARRIVAL_SCALE + 1.0
    }

    fn author(&mut self, contributor: &Contributor, now: f32) -> usize {
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
            portrait: contributor.portrait.clone(),
            position: self.focus() + Vec3::Y * MIN_AVATAR_HEIGHT,
            target: self.focus(),
            repository: 0,
            touched: Vec::new(),
            last_active: now,
            presence: 0.0,
            slot_presence: 0.0,
            projection_scale: 1.0,
            activity: 0.0,
            name_visibility: names::Visibility::default(),
            commit: 0,
            slot: None,
            slot_origin: None,
        });
        self.authors.insert(contributor.key.clone(), index);
        index
    }

    fn begin(
        &mut self,
        event: &Event,
        start: f32,
        file_ids: &mut HashMap<String, FileIdentity>,
    ) {
        let author = self.author(
            event
                .contributor
                .as_ref()
                .expect("contributors are resolved"),
            start,
        );
        let mut touched = BTreeMap::new();
        let mut samples = Vec::new();
        for change in &event.changes {
            let id = apply(&mut self.layout, change, author);
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
        let aggregated = targets.len() > BEAM_LIMIT;
        if aggregated {
            targets = self.aggregate(&touched);
        }
        let centroid = targets
            .iter()
            .filter_map(|&(target, ..)| self.destination(target))
            .fold((Vec2::ZERO, 0.0), |(sum, n), point| (sum + point, n + 1.0));

        let avatar = &mut self.avatars[author];
        if avatar.repository != event.repository {
            avatar.slot_origin = avatar
                .slot
                .take()
                .map(|s| (avatar.repository, s.dock, s.point));
        }
        avatar.repository = event.repository;
        avatar.commit = self.next_event;
        avatar.touched = touched.into_keys().collect();
        if centroid.1 > 0.0 {
            avatar.target = ground(centroid.0 / centroid.1);
        }
        if avatar.presence <= 0.0 {
            avatar.position = avatar.target + Vec3::Y * MIN_AVATAR_HEIGHT;
        }
        avatar.last_active = start;
        avatar.activity = (avatar.activity + 1.0).min(1000.0);
        let previous = self.pulses.iter().rposition(|pulse| pulse.author == author);
        let mut index = 0;
        self.pulses.retain_mut(|pulse| {
            let keep = pulse.author != author || Some(index) == previous;
            if pulse.author == author {
                pulse.end = pulse.end.min(start + FRAGMENT_FLIGHT + 0.3);
            }
            index += 1;
            keep
        });
        self.pulses.push_back(Pulse {
            commit: self.next_event,
            repository: event.repository,
            touched: self.avatars[author].touched.clone(),
            start,
            end: start + PULSE_SECONDS + FRAGMENT_FLIGHT,
            author,
            targets,
            aggregated,
            samples,
        });
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

    fn destination(&self, target: Target) -> Option<Vec2> {
        match target {
            Target::File(id) => self.layout.target(id),
            Target::Dir(index) => Some(self.layout.dir(index).position),
        }
    }

    fn point(&self, target: Target) -> Option<Vec3> {
        match target {
            Target::File(id) => self.layout.file(id).map(|file| ground(file.position)),
            Target::Dir(index) => Some(ground(self.layout.dir(index).position)),
        }
    }

    fn focus(&self) -> Vec3 {
        self.focus
    }

    fn scale(&self) -> f32 {
        1.0 + self.distance / 60.0
    }

    fn place_view(&mut self) {
        self.focus = Vec3::ZERO;
        self.distance = framing::DISTANCE;
    }
}

pub(crate) fn apply(
    layout: &mut Layout,
    change: &crate::timeline::FileChange,
    author: usize,
) -> u64 {
    let id = match &change.kind {
        Kind::Added => layout.add(&change.path, ADD_DELAY, author),
        Kind::Deleted => layout
            .remove(&change.path, DELETE_DELAY, author)
            .unwrap_or_else(|| layout.touch(&change.path, author)),
        Kind::Modified => layout.touch(&change.path, author),
        Kind::Moved { from } => layout.rename(from, &change.path, author),
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

/// Projects points to normalised screen coordinates, or `None` for one behind the eye.
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
    lines: u32,
    rising: bool,
) -> impl Iterator<Item = Fragment> {
    let pieces = lines.bit_width().min(3);
    (0..pieces).map(move |piece| {
        let phase =
            (age * FRAGMENT_SPEED + count_u32(piece) / count_u32(pieces)).fract();
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

pub(crate) fn tick_time(tick: u64) -> f32 {
    let whole = u32::try_from(tick / u64::from(TICK_RATE)).unwrap_or(u32::MAX);
    let part =
        u32::try_from(tick % u64::from(TICK_RATE)).expect("remainder fits in u32");
    count_u32(whole) + count_u32(part) * tick_seconds()
}

#[expect(
    clippy::cast_precision_loss,
    reason = "commit counts beyond 2^24 only lose sub-commit timing precision"
)]
fn count(value: usize) -> f32 {
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
pub(super) fn frames_for(duration: f32, fps: u16) -> anyhow::Result<u32> {
    let frames = (f64::from(duration) * f64::from(fps)).ceil();
    anyhow::ensure!(
        frames.is_finite() && frames >= 0.0 && frames <= f64::from(u32::MAX),
        "whole playback exceeds the supported export range of {} frames; use --frames or reduce duration or fps",
        u32::MAX
    );
    Ok((frames as u32).max(2))
}

pub(super) fn preview_frame(elapsed: Duration, fps: u16) -> u32 {
    let frame = elapsed.as_nanos() * u128::from(fps) / 1_000_000_000;
    u32::try_from(frame).unwrap_or(u32::MAX)
}

fn repository_avatar_factor(repositories: usize) -> f32 {
    let t = ((count(repositories) - 5.0) / 25.0).clamp(0.0, 1.0);
    1.0 - 0.5 * t * t * (3.0 - 2.0 * t)
}

fn prior_activity(events: &[Event], count: usize) -> Vec<bool> {
    (0..count)
        .map(|repository| events.iter().any(|e| e.repository == repository))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn convergence_limit_does_not_recommend_an_already_capped_policy() {
        assert!(super::check_travel_pass(32).is_ok());
        let error = super::check_travel_pass(33).unwrap_err().to_string();
        assert!(error.contains("32 passes"));
        assert!(error.contains("shorten the history"));
        assert!(!error.contains("use capped travel"));
    }
    #[test]
    fn repository_size_taper_preserves_small_portraits_and_halves_at_thirty() {
        assert_eq!(super::repository_avatar_factor(1), 1.0);
        assert_eq!(super::repository_avatar_factor(5), 1.0);
        assert_eq!(super::repository_avatar_factor(30), 0.5);
        assert_eq!(super::repository_avatar_factor(300), 0.5);
        for count in 5..30 {
            assert!(
                super::repository_avatar_factor(count)
                    > super::repository_avatar_factor(count + 1)
            );
        }
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
        for (name, frame) in crate::theme::ThemeName::ALL
            .into_iter()
            .flat_map(|name| [35, 45, 55].map(move |frame| (name, frame)))
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
                &super::Options::default(),
                &theme,
                &crate::contributors::Mappings::default(),
            );
            sim.advance_to(frame, 60);
            let beam = &sim.beams()[0];
            let age = sim.seconds() - sim.state.pulses[0].start;
            let phase = (age * super::FRAGMENT_SPEED).fract();
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
    fn arriving_files_follow_aggregated_beams_before_settling() {
        for (count, nested, frame) in
            [(24, false), (25, false), (25, true)].into_iter().flat_map(
                |(count, nested)| [60, 70].map(move |frame| (count, nested, frame)),
            )
        {
            let mut events = crate::timeline::synthetic();
            events.truncate(1);
            events[0].changes = (0..count)
                .map(|i| crate::timeline::FileChange {
                    path: if nested {
                        format!("src/dir{i}/file.rs")
                    } else {
                        format!("src/file{i}.rs")
                    },
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
            sim.advance_to(frame, 60);
            let aggregated = count > super::BEAM_LIMIT;
            let beams = sim.beams();
            assert_eq!(beams.len(), if aggregated { 1 } else { count });
            for (id, file) in sim.layout().files() {
                assert_eq!(file.state, crate::layout::Presence::Arriving);
                let endpoint = if aggregated {
                    beams[0].to
                } else {
                    super::ground(file.position)
                };
                let flight = if aggregated { 0.8 } else { 1.0 };
                let expected = theme.beam_style.point(
                    sim.avatars()[file.author].position,
                    endpoint,
                    file.presence / flight,
                );
                assert!(
                    sim.file_effect(id, file).0.distance(expected) < 0.0001,
                    "{count} files, nested={nested}, frame={frame}, file={id}"
                );
                let mut settled = file.clone();
                settled.presence = 1.0;
                assert_eq!(
                    sim.file_effect(id, &settled).0,
                    super::ground(file.position)
                );
                if aggregated {
                    settled.presence = 0.8;
                    assert!(
                        sim.file_effect(id, &settled).0.distance(endpoint) < 0.0001
                    );
                    settled.presence = 0.9;
                    assert!(
                        sim.file_effect(id, &settled)
                            .0
                            .distance(endpoint.lerp(super::ground(file.position), 0.5))
                            < 0.0001
                    );
                }
            }
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
                    1,
                    true,
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

    #[test]
    fn repository_plan_limits_reject_slow_or_oversized_inputs() {
        let events = crate::timeline::synthetic();
        assert!(
            Simulation::check_repository_plan(&events, &Options::default(), 5).is_ok()
        );
        assert!(
            Simulation::check_repository_plan(&events, &Options::default(), 30).is_ok()
        );
        assert!(
            Simulation::check_repository_plan(&events, &Options::default(), 31)
                .is_err()
        );
        assert!(
            Simulation::check_repository_plan(&events, &Options::default(), usize::MAX)
                .is_err()
        );
        assert!(
            Simulation::check_repository_plan(
                &events,
                &Options {
                    seconds_per_day: 86_400.0,
                    max_idle_seconds: 86_400.0,
                    ..Options::default()
                },
                5
            )
            .is_err()
        );
    }

    #[test]
    fn uncapped_actual_padding_still_obeys_capture_budgets() {
        let events = crate::timeline::synthetic();
        let options = Options {
            travel_cap: TravelCap::None,
            ..Options::default()
        };
        assert!(Simulation::check_repository_plan(&events, &options, 5).is_ok());
        let schedule = Schedule::with_padding(
            &events,
            options.seconds_per_day,
            options.max_idle_seconds,
            |_| 216_000,
        );
        assert!(Simulation::check_schedule(&events, &options, 5, &schedule).is_err());
    }

    fn short_repository_history(count: usize) -> (Vec<Event>, Vec<String>) {
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
            );
            for tick in 0..=frames_for(simulation.duration(), 60).unwrap() {
                let (regions, _, _) =
                    simulation.packing.sample(u64::from(tick)).unwrap();
                for tile in regions.iter().filter_map(|region| region.tile) {
                    assert!(tile.low.is_finite() && tile.high.is_finite());
                    assert!(tile.high.cmpge(tile.low).all());
                    assert!(
                        tile.low
                            .cmpge(crate::layout::packing::Tile::viewport().low)
                            .all()
                    );
                    assert!(
                        tile.high
                            .cmple(crate::layout::packing::Tile::viewport().high)
                            .all()
                    );
                }
            }
        }
    }

    #[test]
    fn uncapped_short_history_has_no_silent_fades() {
        for count in [10, 30] {
            let (events, labels) = short_repository_history(count);
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
            eprintln!(
                "DENSE_PLAN repositories={count} seconds={}",
                started.elapsed().as_secs_f64()
            );
            assert_eq!(simulation.travel.stats.faded, 0);
            let mut audit = slots::diagnostics::MovingBeams::default();
            let mut checkpoints = Vec::new();
            let frames = frames_for(simulation.duration(), 60).unwrap();
            let mut previous_tiles = 0;
            let mut last_birth = 0;
            for tick in 0..=frames {
                simulation.advance_to(tick, 60);
                audit.observe(&simulation);
                if tick % 60 == 0 {
                    checkpoints
                        .push((tick / 2, format!("{:?}", simulation.state.avatars)));
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
    }
    #[test]
    fn uncapped_default_bot_policy_allows_intentional_fades() {
        let events = (0..4)
            .map(|index| {
                let mut row = vec![timeline::synthetic().remove(0)];
                row[0].seconds = i64::try_from(index).unwrap() * 86_400;
                row[0].author.name = b"Automation[bot]".to_vec();
                row[0].author.email = b"automation@test.invalid".to_vec();
                crate::repositories::namespace(&mut row, index % 2);
                row.remove(0)
            })
            .collect();
        let options = Options {
            seconds_per_day: 0.3,
            travel_cap: TravelCap::None,
            ..Options::default()
        };
        let simulation = Simulation::try_with_repositories(
            events,
            &options,
            &Theme::neon(),
            &Mappings::default(),
            &["a".into(), "b".into()],
        )
        .unwrap();
        assert_eq!(simulation.travel.stats.bot_fades, 3);
        assert_eq!(simulation.travel.stats.faded, 3);
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
        for (start, commits) in [(0, Some(1)), (1, None), (2, None)] {
            let options = Options {
                start,
                commits,
                seconds_per_day: 86_400.0,
                max_idle_seconds: 86_400.0,
                ..Options::default()
            };
            Simulation::check_repository_plan(&events, &options, 2).unwrap();
            let simulation = Simulation::with_repositories(
                events.clone(),
                &options,
                &Theme::neon(),
                &Mappings::default(),
                &names,
            );
            assert_eq!(simulation.duration(), 3.0);
        }
        events[1].seconds = events[0].seconds;
        assert!(
            Simulation::check_repository_plan(&events, &Options::default(), 2).is_ok()
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
        layout.cool_heat(6.0, 86400.0);
        assert!(layout.file(id).unwrap().activity < 0.03);
        assert!(layout.file(id).unwrap().heat > heat * 0.99);
        change.kind = Kind::Deleted;
        let before = layout.file(id).unwrap().heat;
        apply(&mut layout, &change, 0);
        assert_eq!(layout.file(id).unwrap().heat, before);
        assert_eq!(layout.file(id).unwrap().activity, 1.0);
        layout.cool_heat(1.0, 0.01);
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
        assert_eq!(frames_for(1.0, 30).unwrap(), 30);
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
        assert!((sim.schedule.starts[1] - 1.0).abs() < 0.0001);
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
                <= sim.state.pulses[1].start + FRAGMENT_FLIGHT + 0.3
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
                commits: Some(1),
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
        sim.advance_to(frame_at(crowded), 30);
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
        let end = frame_at(sim.duration());
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
            commits: Some(2),
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
