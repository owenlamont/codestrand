use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    time::Duration,
};

use bevy::math::{Vec2, Vec3};

use crate::{
    contributors::{Contributor, Key, Mappings},
    layout::Layout,
    theme::{Geometry, Theme},
    timeline::{Event, Kind},
};

#[cfg(test)]
mod measurement;
#[cfg(test)]
mod pacing_tests;
pub(crate) mod quality;

pub(super) const TICK_RATE: u32 = 60;
pub(super) const PREVIEW_FPS: u16 = 30;
const LEAD_IN: f32 = 0.5;
const TAIL: f32 = 2.5;
const PULSE_SECONDS: f32 = 1.3;
const ADD_DELAY: f32 = 0.2;
const DELETE_DELAY: f32 = 0.6;
const MIN_AVATAR_HEIGHT: f32 = 3.2;
const AVATAR_CLEARANCE: f32 = 0.04;
const AVATAR_IDLE: f32 = 5.0;
const BEAM_LIMIT: usize = 24;
const FRAGMENT_TARGETS: usize = 24;
const SETTLE_TICKS: u32 = 1200;
const DIR_LABELS: usize = 20;

pub(super) struct Options {
    pub heat_half_life: f32,
    pub seconds_per_day: f32,
    pub max_idle_seconds: f32,
    pub start: usize,
    pub commits: Option<usize>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            heat_half_life: 3.0,
            seconds_per_day: 0.5,
            max_idle_seconds: 1.0,
            start: 0,
            commits: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Target {
    File(u64),
    Dir(usize),
}

#[derive(Clone, Debug)]
struct Pulse {
    start: f32,
    end: f32,
    author: usize,
    targets: Vec<(Target, f32, u32, u32)>,
    aggregated: bool,
}

#[derive(Clone, Debug)]
pub(super) struct Avatar {
    pub key: Key,
    pub name: String,
    pub portrait: Option<std::sync::Arc<crate::avatars::Portrait>>,
    pub position: Vec3,
    velocity: Vec3,
    target: Vec3,
    last_active: f32,
    pub presence: f32,
}

pub(super) struct Beam {
    pub author: usize,
    pub from: Vec3,
    pub to: Vec3,
    pub width: f32,
}

pub(super) struct Fragment {
    pub position: Vec3,
    pub rising: bool,
    pub scale: f32,
}

/// Placement priority, highest first, when labels would overlap on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum LabelKind {
    Avatar,
    Landmark,
    Activity,
}

pub(super) struct Label {
    pub position: Vec3,
    pub text: String,
    pub kind: LabelKind,
}

#[derive(Clone, Debug)]
struct State {
    tick: u64,
    layout: Layout,
    avatars: Vec<Avatar>,
    authors: HashMap<Key, usize>,
    next_event: usize,
    pulses: VecDeque<Pulse>,
    focus: Vec3,
    distance: f32,
    geometry: Geometry,
}

/// Deterministic playback stepped at a fixed tick rate, independent of output frame rate.
pub(super) struct Simulation {
    events: Vec<Event>,
    offset: usize,
    total: usize,
    starts: Vec<f32>,
    playback_multiplier: f32,
    heat_half_life: f32,
    initial: State,
    state: State,
    theme: Theme,
}

pub(super) fn event_starts(events: &[Event], rate: f32, idle: f32) -> Vec<f32> {
    let mut previous = events.first().map_or(0, |event| event.seconds);
    let mut now = LEAD_IN;
    events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            if index > 0 {
                let seconds =
                    u64::try_from(event.seconds.saturating_sub(previous)).unwrap_or(0);
                let days = Duration::from_secs(seconds).as_secs_f32() / 86_400.0;
                now += (days.max(0.1) * rate).min(idle).max(tick_seconds());
            }
            previous = event.seconds;
            now
        })
        .collect()
}

fn layout_time(time: f32, starts: &[f32]) -> f32 {
    let next = starts.partition_point(|&start| start <= time);
    if next == 0 {
        return time;
    }
    let previous = starts[next - 1];
    // Preserve launch-timeline rounding so existing exports and quality reports stay identical.
    if next == starts.len() {
        return previous + time - previous;
    }
    let gap = starts[next] - previous;
    previous + (time - previous) / gap * gap
}

impl Simulation {
    #[cfg(test)]
    pub(super) fn scheduled_starts(&self) -> &[f32] {
        &self.starts
    }

    pub(super) fn new(
        mut events: Vec<Event>,
        options: &Options,
        theme: &Theme,
        mappings: &Mappings,
    ) -> Self {
        if events.iter().any(|event| event.contributor.is_none()) {
            mappings.apply(&mut events);
        }
        let start = options.start.min(events.len());
        let end = options
            .commits
            .map_or(events.len(), |count| (start + count).min(events.len()));
        let layout_starts =
            event_starts(&events, options.seconds_per_day, options.max_idle_seconds);
        let mut state = State {
            tick: 0,
            layout: Layout::scheduled(&events, &layout_starts),
            avatars: Vec::new(),
            authors: HashMap::new(),
            next_event: 0,
            pulses: VecDeque::new(),
            focus: Vec3::ZERO,
            distance: 12.0,
            geometry: theme.geometry(),
        };
        for event in &events[..start] {
            let author = state.author(
                event
                    .contributor
                    .as_ref()
                    .expect("contributors are resolved"),
                0.0,
            );
            for change in &event.changes {
                apply(&mut state.layout, change, author);
            }
        }
        if start > 0 {
            state
                .layout
                .plan_time(layout_starts[start.min(layout_starts.len() - 1)] - LEAD_IN);
            state.layout.settle(SETTLE_TICKS, tick_seconds());
            state.avatars.clear();
            state.authors.clear();
        }
        (state.focus, state.distance) = state.fit();
        let total = events.len();
        events.truncate(end);
        events.drain(..start);
        let starts =
            event_starts(&events, options.seconds_per_day, options.max_idle_seconds);
        Self {
            events,
            offset: start,
            total,
            starts,
            playback_multiplier: 1.0,
            heat_half_life: options.heat_half_life,
            initial: state.clone(),
            state,
            theme: theme.clone(),
        }
    }

    pub(super) fn duration(&self) -> f32 {
        self.starts.last().copied().unwrap_or(LEAD_IN) + TAIL
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
        let target = u64::from(frame) * u64::from(TICK_RATE) / u64::from(fps);
        if target < self.state.tick {
            self.state = self.initial.clone();
        }
        while self.state.tick < target {
            self.tick();
        }
    }

    pub(crate) fn tick_layout(&mut self) {
        let plan_time = layout_time(tick_time(self.state.tick + 1), &self.starts);
        let state = &mut self.state;
        state.tick += 1;
        let now = tick_time(state.tick);
        state.layout.cool_heat(tick_seconds(), self.heat_half_life);
        while let Some(event) = self.events.get(state.next_event) {
            let start = self.starts[state.next_event];
            if start > now {
                break;
            }
            state.begin(event, start);
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
        let targets = state.avatar_targets(now);
        for (avatar, target) in state.avatars.iter_mut().zip(targets) {
            let omega = 3.0;
            let acceleration = (target - avatar.position) * omega * omega
                - avatar.velocity * 2.0 * omega;
            avatar.velocity += acceleration * dt;
            avatar.velocity.y = 0.0;
            avatar.position += avatar.velocity * dt;
            let visible = now - avatar.last_active < AVATAR_IDLE;
            let fade = if visible { dt / 0.4 } else { -dt / 1.2 };
            avatar.presence = (avatar.presence + fade).clamp(0.0, 1.0);
        }
        state.place_view(dt);
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

    /// Smoothed camera focus and distance along [`view_direction`].
    pub(super) fn view(&self) -> (Vec3, f32) {
        (self.state.focus, self.state.distance)
    }

    pub(super) fn scale(&self) -> f32 {
        self.state.scale()
    }

    pub(super) fn commit(&self) -> usize {
        self.offset + self.state.next_event
    }

    pub(super) fn total(&self) -> usize {
        self.total
    }

    pub(super) fn caption(&self) -> String {
        let Some(event) = self
            .state
            .next_event
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
            "{}   |   {}   |   commit {} / {}   |   {}x",
            date(event.seconds),
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
                        to: to + Vec3::Y * 0.12,
                        width: envelope * (1.0 + weight.log2() * 0.35).min(3.0),
                    });
                }
            }
        }
        beams
    }

    pub(super) fn fragments(&self) -> Vec<Fragment> {
        let now = self.seconds();
        let mut fragments = Vec::new();
        for pulse in &self.state.pulses {
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
                fragments
                    .extend(diff_fragments(age, envelope, from, to, insertions, false));
                fragments
                    .extend(diff_fragments(age, envelope, from, to, removals, true));
            }
        }
        fragments
    }

    #[cfg(test)]
    pub(super) fn labels(&self) -> Vec<Label> {
        self.labels_for_detail(|_| true)
    }

    pub(super) fn labels_for_detail(
        &self,
        visible: impl Fn(usize) -> bool,
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
            .filter(|(index, dir)| dir.depth > 0 && visible(*index))
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
            })
            .collect();
        for pulse in &self.state.pulses {
            if pulse.targets.len() > 8 && !pulse.aggregated {
                continue;
            }
            for &(target, ..) in &pulse.targets {
                let text = match target {
                    Target::File(id) => {
                        layout.file(id).map(|file| file.name().to_owned())
                    }
                    Target::Dir(index) => Some(format!("{}/", layout.dir(index).path)),
                };
                if let (Some(text), Some(position)) = (text, self.state.point(target)) {
                    labels.push(Label {
                        position: position + Vec3::new(-0.2, 0.0, 0.45),
                        text,
                        kind: LabelKind::Activity,
                    });
                }
            }
        }
        for avatar in &self.state.avatars {
            if avatar.presence > 0.05 {
                labels.push(Label {
                    position: avatar.position + self.state.geometry.avatar_label_offset,
                    text: avatar.name.clone(),
                    kind: LabelKind::Avatar,
                });
            }
        }
        labels
    }
}

impl State {
    fn avatar_targets(&self, now: f32) -> Vec<Vec3> {
        let right = Vec3::new(view_direction().z, 0.0, -view_direction().x).normalize();
        let unit = 2.0 * self.distance * std::f32::consts::FRAC_PI_8.tan() / 720.0;
        let mut targets: Vec<_> =
            self.avatars.iter().map(|avatar| avatar.target).collect();
        let widths: Vec<_> = self
            .avatars
            .iter()
            .map(|avatar| {
                count(avatar.name.chars().count()) * self.geometry.label_height * 0.6
            })
            .collect();
        let active_width: f32 = self
            .avatars
            .iter()
            .zip(&widths)
            .filter(|(avatar, _)| now - avatar.last_active < AVATAR_IDLE)
            .map(|(_, &width)| width + 12.0)
            .sum();
        let (low, high) = self.layout.bounds();
        let unit = unit.min((high - low).length().max(4.0) / active_width.max(1.0));
        let mut pairs = Vec::new();
        for a in 0..targets.len() {
            for b in a + 1..targets.len() {
                if now - self.avatars[a].last_active < AVATAR_IDLE
                    && now - self.avatars[b].last_active < AVATAR_IDLE
                {
                    pairs.push((
                        a,
                        b,
                        [(widths[a] + 12.0) * unit, (widths[b] + 12.0) * unit],
                    ));
                }
            }
        }
        for _ in 0..16 {
            for &(a, b, separations) in &pairs {
                let delta = (targets[b] - targets[a]).dot(right);
                let separation = separations[usize::from(delta < 0.0)];
                if delta.abs() < separation {
                    let sign = delta.signum();
                    let correction = right * ((separation - delta.abs()) * 0.5 * sign);
                    targets[a] -= correction;
                    targets[b] += correction;
                }
            }
        }
        targets
    }

    fn author(&mut self, contributor: &Contributor, now: f32) -> usize {
        if let Some(&index) = self.authors.get(&contributor.key) {
            return index;
        }
        let index = self.avatars.len();
        self.avatars.push(Avatar {
            key: contributor.key.clone(),
            name: contributor.name.clone(),
            portrait: contributor.portrait.clone(),
            position: self.focus() + Vec3::Y * MIN_AVATAR_HEIGHT,
            velocity: Vec3::ZERO,
            target: self.focus(),
            last_active: now,
            presence: 0.0,
        });
        self.authors.insert(contributor.key.clone(), index);
        index
    }

    fn begin(&mut self, event: &Event, start: f32) {
        let author = self.author(
            event
                .contributor
                .as_ref()
                .expect("contributors are resolved"),
            start,
        );
        let mut touched = BTreeMap::new();
        for change in &event.changes {
            let id = apply(&mut self.layout, change, author);
            touched.insert(id, (change.insertions, change.removals));
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
        if centroid.1 > 0.0 {
            avatar.target = ground(centroid.0 / centroid.1);
        }
        if avatar.presence <= 0.0 {
            avatar.position = avatar.target + Vec3::Y * MIN_AVATAR_HEIGHT;
            avatar.velocity = Vec3::ZERO;
        }
        avatar.last_active = start;
        let previous = self.pulses.iter().rposition(|pulse| pulse.author == author);
        let mut index = 0;
        self.pulses.retain_mut(|pulse| {
            let keep = pulse.author != author || Some(index) == previous;
            if pulse.author == author {
                pulse.end = pulse.end.min(start + 0.3);
            }
            index += 1;
            keep
        });
        self.pulses.push_back(Pulse {
            start,
            end: start + PULSE_SECONDS,
            author,
            targets,
            aggregated,
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

    fn file_tops(&self) -> Vec<Vec3> {
        let mut tops: Vec<_> = self
            .layout
            .files()
            .flat_map(|(_, file)| {
                let low = self.geometry.file_min
                    * file.visible_presence(self.layout.dir(file.dir)).max(0.001);
                let high = self.geometry.file_max
                    * file.visible_presence(self.layout.dir(file.dir)).max(0.001);
                [
                    Vec3::new(low.x, high.y, low.z),
                    Vec3::new(low.x, high.y, high.z),
                    Vec3::new(high.x, high.y, low.z),
                    high,
                ]
                .map(|corner| ground(file.position) + corner)
            })
            .collect();
        for (centre, radius) in self.layout.forecast() {
            let radius = radius / std::f32::consts::FRAC_PI_8.cos();
            tops.extend((0_u8..8).map(|side| {
                let point = centre
                    + Vec2::from_angle(f32::from(side) * std::f32::consts::FRAC_PI_4)
                        * radius;
                ground(point) + Vec3::Y * self.geometry.file_max.y
            }));
        }
        tops
    }

    fn scene_points(&self, tops: &[Vec3]) -> Vec<Vec3> {
        let mut points: Vec<Vec3> = self
            .layout
            .dirs()
            .flat_map(|(_, dir)| {
                let centre = ground(dir.position);
                let radius = dir.radius();
                [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z]
                    .map(|side| centre + side * radius)
            })
            .collect();
        points.extend_from_slice(tops);
        points
    }

    fn fit(&self) -> (Vec3, f32) {
        let (low, high) = self.layout.bounds();
        let mut focus = ground((low + high) * 0.5);
        let tops = self.file_tops();
        let points = self.scene_points(&tops);
        let direction = view_direction();
        let ground_up = Vec3::new(-direction.x, 0.0, -direction.z).normalize();
        let ground_right = ground_up.cross(Vec3::Y);
        for _ in 0..4 {
            let distance = self.fit_distance(focus, &points, &tops);
            let lifted = self.lifted_points(focus, distance, &tops);
            let all: Vec<_> = points.iter().chain(&lifted).copied().collect();
            let (low, high) = screen_bounds(focus + direction * distance, focus, &all);
            let centre = (low + high) * 0.5 - Vec2::new(0.0, -0.04);
            let span = distance * (std::f32::consts::FRAC_PI_8).tan();
            focus += ground_right * centre.x * span * 16.0 / 9.0
                + ground_up * centre.y * span * 1.2;
        }
        (focus, self.fit_distance(focus, &points, &tops))
    }

    fn fit_distance(&self, focus: Vec3, points: &[Vec3], tops: &[Vec3]) -> f32 {
        let direction = view_direction();
        let (mut near, mut far) = (2.0_f32, 20_000.0_f32);
        for _ in 0..32 {
            let middle = f32::midpoint(near, far);
            let mut all = self.lifted_points(focus, middle, tops);
            all.extend_from_slice(points);
            if in_view(focus + direction * middle, focus, &all) {
                far = middle;
            } else {
                near = middle;
            }
        }
        far
    }

    fn lifted_points(&self, focus: Vec3, distance: f32, tops: &[Vec3]) -> Vec<Vec3> {
        let eye = focus + view_direction() * distance;
        let line =
            screen_bounds(eye, focus, tops).1.y.max(-0.8) + 2.0 * AVATAR_CLEARANCE;
        let scale = 1.0 + distance / 60.0;
        self.avatars
            .iter()
            .filter(|avatar| avatar.presence > 0.0)
            .flat_map(|avatar| {
                let height = self.lift(avatar, focus, distance, line);
                let position = Vec3::new(avatar.position.x, height, avatar.position.z);
                let mut points = box_corners(
                    Vec3::splat(-self.geometry.avatar_radius * scale),
                    Vec3::splat(self.geometry.avatar_radius * scale),
                )
                .map(|corner| position + corner)
                .to_vec();
                let label = position + self.geometry.avatar_label_offset;
                points.push(label);
                points
            })
            .collect()
    }

    fn lift(&self, avatar: &Avatar, focus: Vec3, distance: f32, line: f32) -> f32 {
        let eye = focus + view_direction() * distance;
        let forward = -view_direction();
        let up = forward.cross(Vec3::Y).normalize().cross(forward);
        let radius = self.geometry.avatar_radius * (1.0 + distance / 60.0);
        let base = Vec3::new(avatar.position.x, 0.0, avatar.position.z);
        let solve = |offset: Vec3, target: f32| {
            let slope = target * std::f32::consts::FRAC_PI_8.tan();
            let delta = base + offset - eye;
            (slope * delta.dot(forward) - delta.dot(up)) / (up.y - slope * forward.y)
        };
        let height = box_corners(Vec3::splat(-radius), Vec3::splat(radius))
            .into_iter()
            .map(|corner| solve(corner, line))
            .fold(MIN_AVATAR_HEIGHT, f32::max)
            .max(solve(
                self.geometry.avatar_label_offset,
                line + 2.0 * self.geometry.label_height / 720.0,
            ));
        // Immediate upward correction preserves clearance while downward easing avoids bobbing.
        height.max(
            avatar.position.y
                + (height - avatar.position.y) * (1.0 - (-tick_seconds() / 1.5).exp()),
        ) + 0.0001
    }

    fn place_view(&mut self, dt: f32) {
        let (focus, distance) = self.fit();
        let settle = if distance > self.distance { 0.3 } else { 1.5 };
        let ease = 1.0 - (-dt / settle).exp();
        self.focus += (focus - self.focus) * ease;
        self.distance += (distance - self.distance) * ease;
        let tops = self.file_tops();
        let points = self.scene_points(&tops);
        self.distance = self
            .distance
            .max(self.fit_distance(self.focus, &points, &tops));
        let line = screen_bounds(
            self.focus + view_direction() * self.distance,
            self.focus,
            &tops,
        )
        .1
        .y
        .max(-0.8)
            + 2.0 * AVATAR_CLEARANCE;
        let heights: Vec<_> = self
            .avatars
            .iter()
            .map(|avatar| self.lift(avatar, self.focus, self.distance, line))
            .collect();
        for (avatar, height) in self.avatars.iter_mut().zip(heights) {
            avatar.position.y = height;
        }
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
    Vec3::new(0.4, 0.44, 0.95).normalize()
}

fn box_corners(low: Vec3, high: Vec3) -> [Vec3; 8] {
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
    let right = forward.cross(Vec3::Y).normalize();
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

/// Whether every point projects inside the margins left by the title and caption.
fn in_view(eye: Vec3, focus: Vec3, points: &[Vec3]) -> bool {
    project(eye, focus, points).all(|point| {
        point.is_some_and(|point| {
            point.x.abs() <= 0.92 && (-0.8..=0.72).contains(&point.y)
        })
    })
}

fn screen_bounds(eye: Vec3, focus: Vec3, points: &[Vec3]) -> (Vec2, Vec2) {
    project(eye, focus, points).flatten().fold(
        (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)),
        |(low, high), point| (low.min(point), high.max(point)),
    )
}

/// Up to three pieces per direction, descending along the beam or rising from the file.
fn diff_fragments(
    age: f32,
    envelope: f32,
    from: Vec3,
    to: Vec3,
    lines: u32,
    rising: bool,
) -> impl Iterator<Item = Fragment> {
    let pieces = lines.bit_width().min(3);
    (0..pieces).map(move |piece| {
        let phase = (age * 1.1 + count_u32(piece) / count_u32(pieces)).fract();
        let position = if rising {
            to + Vec3::new(0.15, 0.2 + phase * 2.2, -0.1)
        } else {
            from.lerp(to + Vec3::Y * 0.2, phase)
        };
        Fragment {
            position,
            rising,
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
fn date(seconds: i64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let mut sim = simulation();
        let mut change = timeline::FileChange {
            path: "heat.rs".to_owned(),
            kind: Kind::Added,
            insertions: 0,
            removals: 0,
        };
        let id = apply(&mut sim.state.layout, &change, 0);
        let small = sim.layout().file(id).unwrap().heat;
        assert!(small > 0.0);
        let mut insertion = change.clone();
        insertion.path = "insertions.rs".to_owned();
        insertion.insertions = 100;
        let inserted = apply(&mut sim.state.layout, &insertion, 0);
        assert!(sim.layout().file(inserted).unwrap().heat > small);
        change.kind = Kind::Modified;
        change.removals = 100;
        apply(&mut sim.state.layout, &change, 0);
        let accumulated = sim.layout().file(id).unwrap().heat;
        assert!(accumulated > 2.0 * small);
        assert!(accumulated < 1.0);
        change.insertions = u32::MAX;
        change.removals = u32::MAX;
        apply(&mut sim.state.layout, &change, 0);
        assert_eq!(sim.layout().file(id).unwrap().heat, 1.0);
        let mut huge = change.clone();
        huge.path = "huge.rs".to_owned();
        huge.kind = Kind::Added;
        let huge_id = apply(&mut sim.state.layout, &huge, 0);
        assert_eq!(sim.layout().file(huge_id).unwrap().heat, 0.75);
        change.kind = Kind::Moved {
            from: change.path.clone(),
        };
        change.path = "moved.rs".to_owned();
        assert_eq!(apply(&mut sim.state.layout, &change, 0), id);
        assert_eq!(sim.layout().file(id).unwrap().heat, 1.0);
    }

    #[test]
    fn deletion_does_not_add_heat_and_halos_decay_independently() {
        let mut layout = Layout::planned_paths(&["a.rs"], 0);
        let mut change = timeline::FileChange {
            path: "a.rs".to_owned(),
            kind: Kind::Added,
            insertions: 0,
            removals: 0,
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
        assert_eq!(event_starts(&events, 2.0, 1.0), [0.5, 1.0, 1.5, 2.5, 3.0]);
        assert_eq!(event_starts(&events, 1.0, 1.0), [0.5, 0.75, 1.0, 2.0, 2.25]);
    }

    #[test]
    fn avatar_targets_separate_names_and_ignore_idle_contributors() {
        let mut sim = Simulation::new(
            timeline::synthetic(),
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        sim.advance_to(300, 60);
        assert!(sim.state.avatars.len() >= 2);
        for avatar in &mut sim.state.avatars {
            avatar.target = Vec3::ZERO;
            avatar.last_active = 5.0;
        }
        sim.state.avatars[0].name = "a very long contributor name".to_owned();
        sim.state.avatars[1].name = "X".to_owned();
        sim.state.distance = 2.0;
        let targets = sim.state.avatar_targets(5.0);
        assert!(targets[0].distance(targets[1]) > 1.0);
        let unit = 2.0 * sim.state.distance * std::f32::consts::FRAC_PI_8.tan() / 720.0;
        let width = count(sim.state.avatars[0].name.chars().count())
            * sim.state.geometry.label_height
            * 0.6;
        assert!(targets[0].distance(targets[1]) >= (width + 12.0) * unit * 0.99);
        sim.state.distance = 10_000.0;
        let bounded = sim.state.avatar_targets(5.0);
        let (low, high) = sim.state.layout.bounds();
        assert!(bounded[0].distance(bounded[1]) <= (high - low).length().max(4.0));
        let idle = sim.state.avatar_targets(5.0 + AVATAR_IDLE);
        assert!(idle.iter().all(|&target| target == Vec3::ZERO));
    }

    #[test]
    fn coincident_and_reversed_timestamps_remain_strictly_monotonic() {
        let mut events = timeline::synthetic();
        for (event, seconds) in events.iter_mut().zip([100, 100, 90, 95, 101]) {
            event.seconds = seconds;
        }
        for rate in [0.01, 0.5, 100.0] {
            let starts = event_starts(&events, rate, 1.0);
            assert!(starts.windows(2).all(|pair| pair[1] > pair[0]));
            assert!(
                starts[..5]
                    .windows(2)
                    .all(|pair| pair[1] - pair[0] >= tick_seconds() - 0.0001)
            );
        }
        assert_eq!(event_starts(&[], 1.0, 1.0), [] as [f32; 0]);
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
        let sim = Simulation::new(
            events,
            &Options {
                seconds_per_day: 86_400.0,
                max_idle_seconds: 86_400.0,
                ..Options::default()
            },
            &Theme::neon(),
            &Mappings::default(),
        );
        assert!(
            frames_for(sim.duration(), 240)
                .unwrap_err()
                .to_string()
                .contains("supported export range")
        );
        assert!(frames_for(sim.duration(), 30).is_ok());
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
        assert_eq!(sim.starts[0], LEAD_IN);
        assert_eq!(sim.starts[1], 1.0);
        assert!(sim.starts.windows(2).all(|pair| pair[1] > pair[0]));
        sim.change_rate(2.0);
        assert!((sim.starts[1] - 1.0).abs() < 0.0001);
        assert!(sim.caption().contains("1x"));
        sim.restart();
        assert_eq!(sim.starts, event_starts(&sim.events, 0.5, 1.0));
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
        assert_eq!(event_starts(&events, 2.0, 1.0), [0.5, 1.5, 1.7, 2.7, 3.7]);
    }

    #[test]
    fn rate_controls_stop_at_multiplier_bounds() {
        for (factor, bound) in [(0.5, 0.125), (2.0, 8.0)] {
            let mut sim = simulation();
            let starts = sim.starts.clone();
            for _ in 0..32 {
                sim.change_rate(factor);
            }
            assert_eq!(sim.playback_multiplier(), bound);
            assert_eq!(sim.starts, starts);
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
        assert!(sim.state.pulses[0].end <= sim.state.pulses[1].start + 0.3);
    }

    #[test]
    fn construction_preserves_event_buffers_when_selecting_a_range() {
        let events: Vec<_> = ["First", "Second"]
            .into_iter()
            .map(|name| Event {
                author: timeline::Identity {
                    name: name.as_bytes().to_vec(),
                    email: name.as_bytes().to_vec(),
                },
                contributor: None,
                commit: String::new(),
                seconds: 0,
                changes: vec![timeline::FileChange {
                    path: name.to_owned(),
                    kind: Kind::Added,
                    insertions: 1,
                    removals: 0,
                }],
            })
            .collect();
        let total = events.len();
        let changes = events[1].changes.as_ptr();
        let author = events[1].author.name.as_ptr();
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
        assert_eq!(sim.events[0].changes.as_ptr(), changes);
        assert_eq!(sim.events[0].author.name.as_ptr(), author);
    }

    fn assert_clearance(sim: &Simulation) {
        let state = &sim.state;
        let eye = state.focus + view_direction() * state.distance;
        let tops = state.file_tops();
        let highest = screen_bounds(eye, state.focus, &tops).1.y;
        for avatar in state.avatars.iter().filter(|avatar| avatar.presence > 0.0) {
            let radius = state.geometry.avatar_radius * sim.scale();
            let corners = box_corners(Vec3::splat(-radius), Vec3::splat(radius))
                .map(|corner| avatar.position + corner);
            let projected: Vec<_> = project(eye, state.focus, &corners)
                .map(|point| point.expect("avatar bounds are in front of the camera"))
                .collect();
            let lowest = projected
                .iter()
                .map(|point| point.y)
                .fold(f32::MAX, f32::min);
            assert!(
                lowest > highest + 2.0 * AVATAR_CLEARANCE,
                "tick {}: avatar {lowest}, file {highest}, distance {}",
                state.tick,
                state.distance
            );
            assert!(
                projected.iter().all(|point| point.x.abs() <= 0.9201
                    && (-0.8001..=0.7201).contains(&point.y)),
                "tick {}: avatar outside fit margins: {projected:?}",
                state.tick
            );
            let label = [avatar.position + state.geometry.avatar_label_offset];
            let label = project(eye, state.focus, &label)
                .next()
                .flatten()
                .expect("label is in front of the camera");
            assert!(
                label.y - 2.0 * state.geometry.label_height / 720.0
                    > highest + 2.0 * AVATAR_CLEARANCE
            );
            assert!(avatar.position.y >= MIN_AVATAR_HEIGHT);
        }
        assert!(
            project(eye, state.focus, &tops).all(|point| point.is_some()),
            "files are in front of the camera"
        );
    }

    #[test]
    fn synthetic_avatars_clear_file_tops_during_growth_and_rewind() {
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
    fn projection_check_rejects_fixed_height() {
        let mut events = timeline::synthetic();
        events[7]
            .changes
            .extend((0..256).map(|index| timeline::FileChange {
                path: format!("tests/fixtures/clearance_{index}.yaml"),
                kind: Kind::Added,
                insertions: 1,
                removals: 0,
            }));
        let mut sim = Simulation::new(
            events,
            &Options::default(),
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        sim.advance_to(13, 2);
        assert_clearance(&sim);
        let state = &sim.state;
        let eye = state.focus + view_direction() * state.distance;
        let highest = screen_bounds(eye, state.focus, &state.file_tops()).1.y;
        let points: Vec<_> = state
            .avatars
            .iter()
            .map(|avatar| {
                Vec3::new(avatar.position.x, MIN_AVATAR_HEIGHT, avatar.position.z)
            })
            .collect();
        let lowest = screen_bounds(eye, state.focus, &points).0.y;
        assert!(lowest < highest + 2.0 * AVATAR_CLEARANCE);
    }

    #[test]
    fn lift_and_fit_settle_without_camera_drift() {
        let mut sim = simulation();
        sim.advance_to(13, 2);
        sim.state.layout.settle(SETTLE_TICKS, tick_seconds());
        for _ in 0..600 {
            sim.state.place_view(tick_seconds());
        }
        let (focus, distance) = sim.view();
        for _ in 0..120 {
            sim.state.place_view(tick_seconds());
            assert_clearance(&sim);
        }
        assert!((sim.view().0 - focus).length() < 0.01);
        assert!((sim.view().1 - distance).abs() < 0.01);
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
        let crowded = sim.starts[7] + 0.5;
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

    #[test]
    fn landmarks_rank_large_nested_subtrees_before_small_top_level_directories() {
        let mut sim = simulation();
        let paths: Vec<_> = (0..30)
            .flat_map(|dir| (0..8).map(move |file| format!("big/d{dir}/f{file}.rs")))
            .chain(std::iter::once("small/one.rs".to_owned()))
            .collect();
        sim.state.layout = Layout::planned_paths(
            &paths.iter().map(String::as_str).collect::<Vec<_>>(),
            0,
        );
        for path in &paths {
            sim.state.layout.add(path, 0.0, 0);
        }
        let labels = sim.labels();
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
        let mut sim = simulation();
        let paths: Vec<_> = (0..50).map(|index| format!("d{index}/file.rs")).collect();
        sim.state.layout = Layout::planned_paths(
            &paths.iter().map(String::as_str).collect::<Vec<_>>(),
            0,
        );
        for path in &paths {
            sim.state.layout.add(path, 0.0, 0);
        }
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
        assert!(paths.contains(&"src/scene/layout.rs"));
        assert!(!paths.contains(&"src/old.rs"));
        assert!(!paths.contains(&"docs/guide.md"));
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
