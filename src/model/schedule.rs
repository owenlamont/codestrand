use std::time::Duration;

use super::{LEAD_IN, TAIL, TICK_RATE, Time, tick_seconds, tick_time};
use crate::timeline::Event;

pub(crate) const BIRTH_TICKS: u64 = 90;

pub(super) fn arrival_ticks() -> u64 {
    u64::from(super::travel::seconds_ticks(super::ARRIVAL_SECONDS, true))
}

fn creates_arrivals(event: &Event) -> bool {
    use crate::timeline::Kind;
    event
        .changes
        .iter()
        .any(|change| matches!(change.kind, Kind::Added | Kind::Moved { .. }))
}

pub(super) fn padding_at(padding: &[(usize, u32)], index: usize) -> u32 {
    padding
        .iter()
        .find(|&&(event, _)| event == index)
        .map_or(0, |&(_, ticks)| ticks)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Schedule {
    pub(crate) starts: Vec<Time>,
    services: Vec<Service>,
    pub(super) birth_protections: Vec<(usize, usize)>,
    pub(super) walk_protections: Vec<(usize, usize)>,
    motion_protections: Vec<(usize, u64)>,
    ray_holds: Vec<(usize, usize, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Service {
    tick: u64,
    padding: u32,
    birth: bool,
    can_form_ring: bool,
    tile_floor: u64,
}

#[derive(Clone, Debug)]
pub(super) struct Clock {
    pub(super) history: Schedule,
    presentation: Schedule,
}

impl Clock {
    pub(super) fn replay_bytes(&self) -> u128 {
        self.history.replay_bytes() + self.presentation.replay_bytes()
    }

    pub(super) fn new(history: Schedule, presentation: Schedule) -> Self {
        Self {
            history,
            presentation,
        }
    }

    pub(super) fn rows(&self, tick: u64) -> (u64, u64) {
        let next = self.presentation.applied(tick);
        let (floor, running, tile_floor) =
            next.checked_sub(1).map_or((0, tick, 0), |last| {
                let floor = self.history.services[last].tick;
                let shown = &self.presentation.services[last];
                (floor, floor + tick - shown.tick, shown.tile_floor)
            });
        self.history.services.get(next).map_or(
            (running.max(tile_floor), running),
            |service| {
                let limit = self.presentation.services.get(next).map_or(
                    service.tick,
                    |shown| {
                        let wait = self.presentation.motion_wait(&self.history, next);
                        service.tick + wait.saturating_sub(shown.tick - tick)
                    },
                );
                (
                    running.min(limit).max(tile_floor),
                    running.min(service.tick.saturating_sub(1).max(floor)),
                )
            },
        )
    }

    pub(super) fn present(&self, history: Time) -> Time {
        let next = self
            .history
            .starts
            .partition_point(|&start| start <= history);
        next.min(self.presentation.starts.len())
            .checked_sub(1)
            .map_or(history, |last| {
                history.after(
                    self.presentation.starts[last].since(self.history.starts[last]),
                )
            })
    }

    /// The presentation tick for a history sample after the final commit.
    pub(super) fn present_tail_tick(&self, history_tick: u64) -> u64 {
        self.presentation
            .present_tail_tick(&self.history, history_tick)
    }
}

impl Schedule {
    pub(super) fn replay_bytes(&self) -> u128 {
        use super::checkpoint_memory::{buffer_bytes, retained_fields};

        let (
            starts,
            services,
            birth_protections,
            walk_protections,
            motion_protections,
            ray_holds,
        ) = retained_fields!(self, Schedule {
            starts, services, birth_protections, walk_protections, motion_protections, ray_holds;
        });
        buffer_bytes::<Time>(starts.capacity())
            + buffer_bytes::<Service>(services.capacity())
            + buffer_bytes::<(usize, usize)>(birth_protections.capacity())
            + buffer_bytes::<(usize, usize)>(walk_protections.capacity())
            + buffer_bytes::<(usize, u64)>(motion_protections.capacity())
            + buffer_bytes::<(usize, usize, u64)>(ray_holds.capacity())
    }

    pub(super) fn for_replay(events: &[Event], options: &super::Options) -> Self {
        let range = options.selected_range(events.len());
        let mut seen: std::collections::HashSet<_> = events[..range.start]
            .iter()
            .map(|event| event.repository)
            .collect();
        let selected = &events[range];
        let opening = seen.is_empty();
        let mut schedule = Self::build(
            selected,
            options.seconds_per_day,
            options.max_idle_seconds,
            |_| 0,
            |index| seen.insert(selected[index].repository) && !(opening && index == 0),
        );
        let mut transmitters = std::collections::HashMap::new();
        let mut latest_arrival = None;
        for (index, event) in selected.iter().enumerate() {
            let mut hold = latest_arrival
                .filter(|&earlier| schedule.birth_starts_after(index, earlier));
            for contributor in event.participants() {
                if let Some((repository, earlier)) =
                    transmitters.get(&contributor.key).copied()
                    && repository != event.repository
                {
                    hold = hold.max(Some(earlier));
                    transmitters.remove(&contributor.key);
                }
            }
            if let Some(author) = &event.contributor
                && creates_arrivals(event)
            {
                transmitters.insert(&author.key, (event.repository, index));
                latest_arrival = Some(index);
            }
            if let Some(earlier) = hold {
                let settle = if schedule.birth_starts_after(index, earlier) {
                    BIRTH_TICKS + 1
                } else {
                    0
                };
                schedule.ray_holds.push((index, earlier, settle));
            }
        }
        schedule
    }

    pub(super) fn protect_opening_rays(&mut self, events: &[Event], end: u64) {
        // Overlapping births can move an opening gutter past a stationary source.
        let mut latest_arrival = None;
        for (index, (event, service)) in events.iter().zip(&self.services).enumerate() {
            if service.tick > end {
                break;
            }
            if service.birth
                && let Some(earlier) = latest_arrival
            {
                self.ray_holds.push((index, earlier, BIRTH_TICKS + 1));
            }
            if creates_arrivals(event) {
                latest_arrival = Some(index);
            }
        }
        self.ray_holds.sort_unstable_by_key(|&(commit, ..)| commit);
    }

    pub(super) fn protect_walk_after(&mut self, commit: usize) -> bool {
        let Some((birth, _)) = self.births().find(|&(index, _)| index > commit) else {
            return false;
        };
        match self
            .walk_protections
            .binary_search_by_key(&birth, |&(birth, _)| birth)
        {
            Ok(index) => {
                let earlier = &mut self.walk_protections[index].1;
                let changed = commit > *earlier;
                *earlier = (*earlier).max(commit);
                changed
            }
            Err(index) => {
                self.walk_protections.insert(index, (birth, commit));
                true
            }
        }
    }

    pub(super) fn needs_presentation(&self, padding: &[(usize, u32)]) -> bool {
        !self.birth_protections.is_empty()
            || !self.walk_protections.is_empty()
            || !self.motion_protections.is_empty()
            || !self.ray_holds.is_empty()
            || padding.iter().any(|&(_, ticks)| ticks > 0)
    }

    pub(super) fn protect_tile_motion(
        &mut self,
        intervals: impl Iterator<Item = (u64, u64)>,
        events: &[Event],
        repository_ends: &[Time],
    ) {
        let mut intervals: Vec<_> =
            intervals.filter(|&(start, end)| start < end).collect();
        intervals.sort_unstable();
        let mut closing: Vec<(u64, u64)> = Vec::new();
        for (start, end) in intervals {
            if let Some(last) = closing.last_mut()
                && start < last.1
            {
                last.1 = last.1.max(end);
            } else {
                closing.push((start, end));
            }
        }
        self.motion_protections = closing
            .into_iter()
            .filter_map(|(start, end)| {
                let commit = self
                    .services
                    .partition_point(|service| service.tick < start);
                let first = self.services.get(commit)?.tick;
                let tile_time = tick_time(end);
                // The pause advances the tile clock for pending commits too. Stop
                // at the first service whose repository would already be archiving.
                let end = self.services[commit..]
                    .iter()
                    .zip(&events[commit..])
                    .take_while(|(service, _)| service.tick < end)
                    .find(|(_, event)| {
                        repository_ends
                            .get(event.repository)
                            .is_some_and(|&archive| archive <= tile_time)
                    })
                    .map_or(end, |(service, _)| service.tick);
                (first < end).then_some((commit, end))
            })
            .collect();
    }

    pub(super) fn with_travel(
        &self,
        len: usize,
        padding: impl Fn(usize) -> u32,
    ) -> Self {
        let mut extra = 0;
        let mut services: Vec<Service> = Vec::with_capacity(len);
        let mut protections = self.birth_protections.iter().peekable();
        let mut walks = self.walk_protections.iter().peekable();
        let mut motions = self.motion_protections.iter().peekable();
        let mut rays = self.ray_holds.iter().peekable();
        let ray_ticks = arrival_ticks() + super::travel::FADE_TICKS;
        let ring_ticks =
            u64::from(super::travel::seconds_ticks(super::DISBAND, false)) + 1;
        let mut tile_floor = 0;
        let starts = self.starts[..len]
            .iter()
            .enumerate()
            .map(|(index, &start)| {
                let wait = padding(index);
                extra += u64::from(wait);
                let service = &self.services[index];
                let mut tick = service.tick.saturating_add(extra);
                let write = protections.next_if(|&&(birth, _)| birth == index);
                let walk = walks.next_if(|&&(birth, _)| birth == index);
                if let Some(earlier) = write
                    .into_iter()
                    .chain(walk)
                    .map(|&(_, earlier)| earlier)
                    .max()
                {
                    let ready = services[earlier].tick.saturating_add(BIRTH_TICKS + 2);
                    let floor = ready.saturating_add(u64::from(wait));
                    extra += floor.saturating_sub(tick);
                    tick = tick.max(floor);
                }
                if let Some(&(_, end)) =
                    motions.next_if(|&&(commit, _)| commit == index)
                {
                    let motion = end.saturating_sub(tile_floor.max(service.tick));
                    let shared = if service.birth { 0 } else { u64::from(wait) };
                    let remaining = motion.saturating_sub(shared);
                    extra += remaining;
                    tick = tick.saturating_add(remaining);
                    tile_floor = tile_floor.max(end);
                }
                let mut ready = tick;
                while let Some(&(_, earlier, settle)) =
                    rays.next_if(|&&(commit, ..)| commit == index)
                {
                    let earlier = &services[earlier];
                    let prelude = u64::from(earlier.can_form_ring) * ring_ticks;
                    ready = ready
                        .max(earlier.tick.saturating_add(ray_ticks + settle + prelude));
                }
                extra = extra.saturating_add(ready - tick);
                tick = ready;
                let start = start.after(tick_time(extra));
                services.push(Service {
                    tick,
                    padding: service.padding.saturating_add(wait),
                    birth: service.birth,
                    can_form_ring: service.can_form_ring,
                    tile_floor,
                });
                if first_tick(start) == tick || tick == u64::MAX {
                    start
                } else {
                    tick_time(tick)
                }
            })
            .collect();
        Self {
            starts,
            services,
            birth_protections: Vec::new(),
            walk_protections: Vec::new(),
            motion_protections: Vec::new(),
            ray_holds: Vec::new(),
        }
    }

    fn birth_starts_after(&self, birth: usize, earlier: usize) -> bool {
        let service = &self.services[birth];
        service.birth
            && self.services[earlier].tick.saturating_add(BIRTH_TICKS) < service.tick
    }

    pub(super) fn births(&self) -> impl Iterator<Item = (usize, u64)> + '_ {
        self.services
            .iter()
            .enumerate()
            .filter_map(|(index, service)| {
                service.birth.then_some((index, service.tick))
            })
    }

    pub(super) fn last_ring_service(&self) -> Option<Time> {
        let &latest = self.starts.last()?;
        self.starts
            .iter()
            .zip(&self.services)
            .rev()
            .take_while(|&(start, _)| {
                latest - *start <= 2.0 * super::DISBAND + tick_seconds()
            })
            .find(|(_, service)| service.can_form_ring)
            .map(|(&start, _)| start + super::DISBAND + tick_seconds())
    }

    pub(super) fn motion_wait(&self, history: &Self, index: usize) -> u64 {
        let earlier_floor = index
            .checked_sub(1)
            .map_or(0, |i| self.services[i].tile_floor);
        self.services[index]
            .tile_floor
            .saturating_sub(earlier_floor.max(history.services[index].tick))
    }

    pub(super) fn present_tail_tick(&self, history: &Self, history_tick: u64) -> u64 {
        self.services.last().map_or(history_tick, |last| {
            let history_last = history.services[self.services.len() - 1].tick;
            history_tick.saturating_add(last.tick - history_last)
        })
    }

    #[cfg(test)]
    pub(super) fn commit_ticks(&self) -> impl Iterator<Item = u64> + '_ {
        self.services.iter().map(|service| service.tick)
    }

    pub(super) fn padding(&self, index: usize) -> u32 {
        self.services[index].padding
    }
    #[cfg(test)]
    pub(crate) fn new(events: &[Event], rate: f32, idle: f32) -> Self {
        Self::with_padding(events, rate, idle, |_| 0)
    }

    pub(crate) fn with_padding(
        events: &[Event],
        rate: f32,
        idle: f32,
        padding: impl Fn(usize) -> u32,
    ) -> Self {
        Self::build(events, rate, idle, padding, |_| false)
    }

    fn build(
        events: &[Event],
        rate: f32,
        idle: f32,
        padding: impl Fn(usize) -> u32,
        mut is_birth: impl FnMut(usize) -> bool,
    ) -> Self {
        let mut previous = events.first().map_or(0, |event| event.seconds);
        let mut now = Time::seconds(LEAD_IN);
        let mut extra = 0_u64;
        let mut earliest_birth = BIRTH_TICKS + 1;
        let mut next_service = 0;
        let mut has_birth = false;
        let mut services = Vec::with_capacity(events.len());
        let starts = events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                if index > 0 {
                    let seconds = u64::try_from(event.seconds.saturating_sub(previous))
                        .unwrap_or(0);
                    let days = Duration::from_secs(seconds).as_secs_f32() / 86_400.0;
                    now += (days.max(0.1) * rate).min(idle).max(tick_seconds());
                }
                previous = event.seconds;
                let padding = padding(index);
                extra += u64::from(padding);
                let birth = is_birth(index);
                let mut start = now.after(tick_time(extra));
                let service = first_tick(start);
                let earliest = if birth { earliest_birth } else { 0 };
                let tick = service.max(earliest).max(next_service);
                let birth_wait = (tick - service) * u64::from(birth);
                extra += birth_wait;
                if tick != service {
                    start = tick_time(tick);
                }
                has_birth |= birth;
                if index == 0 {
                    earliest_birth = tick.saturating_add(BIRTH_TICKS + 1);
                }
                if has_birth {
                    next_service = tick.saturating_add(1);
                }
                services.push(Service {
                    tick,
                    padding: padding.saturating_add(
                        u32::try_from(birth_wait).expect("bounded birth wait"),
                    ),
                    birth,
                    can_form_ring: !event.co_authors.is_empty(),
                    tile_floor: 0,
                });
                start
            })
            .collect();
        Self {
            starts,
            services,
            birth_protections: Vec::new(),
            walk_protections: Vec::new(),
            motion_protections: Vec::new(),
            ray_holds: Vec::new(),
        }
    }

    pub(crate) fn start(&self, index: usize, tick: u64) -> Option<Time> {
        self.services
            .get(index)
            .filter(|service| service.tick <= tick)
            .map(|_| self.starts[index])
    }

    pub(crate) fn applied(&self, tick: u64) -> usize {
        self.services
            .partition_point(|service| service.tick <= tick)
    }

    pub(crate) fn duration(&self) -> Time {
        self.starts
            .last()
            .copied()
            .unwrap_or(Time::seconds(LEAD_IN))
            + TAIL
    }

    pub(crate) fn frame_tick(frame: u32, fps: u16) -> u64 {
        u64::from(frame) * u64::from(TICK_RATE) / u64::from(fps)
    }

    pub(crate) fn layout_time(&self, time: Time) -> Time {
        let next = self.starts.partition_point(|&start| start <= time);
        if next == 0 {
            return time;
        }
        let previous = self.starts[next - 1];
        // Preserve launch-timeline rounding so existing exports and quality reports stay identical.
        if next == self.starts.len() {
            return time.rounded_through(previous);
        }
        let gap = self.starts[next] - previous;
        previous + (time - previous) / gap * gap
    }
}

pub(super) fn first_tick(start: Time) -> u64 {
    let mut low = 0;
    let mut high = u64::from(u32::MAX) * u64::from(TICK_RATE);
    if start > tick_time(high) {
        return u64::MAX;
    }
    while low < high {
        let middle = low + (high - low) / 2;
        if tick_time(middle) < start {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dense_opening_shares_one_growth_interval() {
        let (events, _) = super::super::tests::short_repository_history(200);
        let options = super::super::Options {
            seconds_per_day: 0.1,
            max_idle_seconds: 0.1,
            ..Default::default()
        };
        let nominal =
            Schedule::new(&events, options.seconds_per_day, options.max_idle_seconds);
        let replay = Schedule::for_replay(&events, &options);
        let actual: Vec<_> = replay.commit_ticks().collect();
        assert_eq!(replay.births().count(), 199);
        assert_eq!(actual[0], nominal.commit_ticks().next().unwrap());
        assert_eq!(actual[1] - actual[0], BIRTH_TICKS + 1);
        assert!(actual.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            actual
                .iter()
                .copied()
                .zip(nominal.commit_ticks())
                .all(|(actual, nominal)| { actual - nominal <= BIRTH_TICKS })
        );
    }

    #[test]
    fn an_expanding_neighbours_last_write_precedes_birth_motion() {
        let (events, _) = super::super::tests::short_repository_history(3);
        let options = super::super::Options {
            seconds_per_day: 0.1,
            max_idle_seconds: 0.1,
            ..Default::default()
        };
        let mut history = Schedule::for_replay(&events, &options);
        history.birth_protections.push((2, 1));
        let presentation =
            history.with_travel(3, |index| if index == 2 { 24 } else { 0 });
        let ticks: Vec<_> = presentation.commit_ticks().collect();
        assert_eq!(ticks[1] - ticks[0], BIRTH_TICKS + 1);
        assert_eq!(ticks[2] - 24 - ticks[1], BIRTH_TICKS + 2);
    }

    #[test]
    fn travel_and_reflow_share_the_same_write_pause() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        let mut history = Schedule::new(&events, 1.0, 1.0);
        let write = history.commit_ticks().nth(1).unwrap();
        history.protect_tile_motion(
            std::iter::once((write, write + 120)),
            &events,
            &[],
        );
        let presentation =
            history.with_travel(2, |index| if index == 1 { 24 } else { 0 });
        let service = presentation.commit_ticks().nth(1).unwrap();
        let clock = super::Clock::new(history, presentation);
        assert_eq!(service, write + 120);
        for tick in service - 120..=service {
            assert_eq!(clock.rows(tick).0, write + tick - (service - 120));
        }
    }

    #[test]
    fn a_later_wait_leaves_an_idle_gap_departure_unchanged() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        events[0].seconds = 0;
        events[1].seconds = 100 * 86_400;
        let history = Schedule::new(&events, 1.0, 100.0);
        let departure = crate::model::lifecycle::departures(
            &events,
            &history,
            30,
            crate::model::FragmentPace::new(1.0),
        )[0];
        let padded =
            Schedule::with_padding(
                &events,
                1.0,
                100.0,
                |i| if i == 1 { 600 } else { 0 },
            );
        let clock = Clock::new(history.clone(), padded);
        assert!((departure - Time::seconds(30.5)).abs() < 0.00001);
        assert_eq!(clock.present(departure), departure);
        let tick = first_tick(departure);
        assert_eq!(clock.rows(tick), (tick, tick));
        assert_eq!(clock.history, history);
    }

    #[test]
    fn accumulated_padding_remains_beyond_the_clock_range() {
        let mut event = crate::timeline::synthetic().remove(0);
        event.seconds = 0;
        let events = vec![event; 120];
        let padded = Schedule::with_padding(&events, 0.5, 1.0, |_| u32::MAX);
        let expected = 120.0 * f64::from(u32::MAX) / f64::from(TICK_RATE);
        assert!(padded.starts[119].as_secs_f64() >= expected);
        assert_eq!(padded.services[119].tick, u64::MAX);
        let clock_limit = u64::from(u32::MAX) * u64::from(TICK_RATE);
        assert_eq!(padded.applied(clock_limit), 59);
        assert!(padded.starts.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn zero_padding_preserves_legacy_seconds_and_application_ticks() {
        let mut events = crate::timeline::synthetic();
        for (event, seconds) in events.iter_mut().zip([0, 0, -1, 86_400, 1]) {
            event.seconds = seconds;
        }
        for (rate, idle) in [(0.01, 1.0), (0.5, 1.0), (2.0, 0.02), (86_400.0, 86_400.0)]
        {
            let schedule = Schedule::new(&events, rate, idle);
            let mut now = LEAD_IN;
            let mut previous = events[0].seconds;
            for (index, event) in events.iter().enumerate() {
                if index > 0 {
                    let seconds = u64::try_from(event.seconds.saturating_sub(previous))
                        .unwrap_or(0);
                    let days = Duration::from_secs(seconds).as_secs_f32() / 86_400.0;
                    now += (days.max(0.1) * rate).min(idle).max(tick_seconds());
                }
                previous = event.seconds;
                assert_eq!(schedule.starts[index], now);
                let tick = schedule.services[index].tick;
                assert!(tick_time(tick) >= Time::ZERO + now);
                assert!(tick_time(tick - 1) < Time::ZERO + now);
                assert_eq!(schedule.services[index].padding, 0);
            }
        }
    }

    #[test]
    fn commits_days_in_keep_their_order_and_tick_exact_starts() {
        let mut event = crate::timeline::synthetic().remove(0);
        event.seconds = 0;
        let mut events = vec![event; 10_000];
        let waits = [2, 8].map(|days| {
            let wait = days * 24 * 60 * 60 * TICK_RATE;
            Schedule::with_padding(&events, 0.5, tick_seconds(), |index| {
                if index == 0 { wait } else { 0 }
            })
        });
        // At a tenth of a second a day, this gap alone puts later commits 8 days in.
        for event in &mut events[1..] {
            event.seconds = 7_000_000 * 86_400;
        }
        let gap = Schedule::new(&events, 0.1, 86_400.0 * 30.0);
        let two_days = tick_time(u64::from(2 * 24 * 60 * 60 * TICK_RATE));
        for schedule in waits.iter().chain([&gap]) {
            assert!(schedule.starts[1] > two_days);
            for pair in schedule.starts[1..].windows(2) {
                assert!(pair[0] < pair[1]);
                assert!((pair[1] - pair[0] - tick_seconds()).abs() < 0.001);
            }
            for (&start, service) in schedule.starts.iter().zip(&schedule.services) {
                assert!(tick_time(service.tick) >= start);
                assert!(tick_time(service.tick - 1) < start);
            }
            assert!(
                schedule
                    .services
                    .windows(2)
                    .all(|pair| pair[0].tick <= pair[1].tick)
            );
        }
    }

    #[test]
    fn padding_shifts_only_the_selected_commit_and_its_successors() {
        let events = crate::timeline::synthetic();
        let base = Schedule::new(&events, 0.5, 1.0);
        let padded = Schedule::with_padding(&events, 0.5, 1.0, |index| {
            [0, 0, 60, 0, 30].get(index).copied().unwrap_or(0)
        });
        for (index, (&start, &original)) in
            padded.starts.iter().zip(&base.starts).enumerate()
        {
            let shift = match index {
                0..2 => 0.0,
                2..4 => 1.0,
                _ => 1.5,
            };
            assert_eq!(start, original + shift);
        }
        assert!(
            padded
                .services
                .windows(2)
                .all(|pair| pair[0].tick <= pair[1].tick)
        );
        assert_eq!(padded.duration(), base.duration() + 1.5);
        assert_eq!(Schedule::new(&[], 0.5, 1.0).duration(), LEAD_IN + TAIL);
    }

    #[test]
    fn tick_search_matches_float_boundaries_and_stops_beyond_clock_range() {
        for tick in [1, 29, 30, 59, 60, 61, 1000, 1_000_000, 1_000_000_000] {
            let time = tick_time(tick);
            for start in [time.next_down(), time, time.next_up()] {
                let applied = first_tick(start);
                assert!(tick_time(applied) >= start);
                assert!(tick_time(applied - 1) < start);
            }
        }
        assert_eq!(first_tick(Time::INFINITY), u64::MAX);
        let span = 16_384 * u64::from(TICK_RATE);
        for boundary in [span, 11 * span, 40 * span] {
            for tick in boundary - 300..boundary + 300 {
                assert_eq!(first_tick(tick_time(tick)), tick);
            }
        }
    }
}
