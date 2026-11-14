use std::time::Duration;

use super::{LEAD_IN, TAIL, TICK_RATE, Time, tick_seconds, tick_time};
use crate::timeline::Event;

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
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Service {
    tick: u64,
    padding: u32,
}

impl Schedule {
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
        let mut previous = events.first().map_or(0, |event| event.seconds);
        let mut now = Time::seconds(LEAD_IN);
        let mut extra = 0_u64;
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
                let start = now.after(tick_time(extra));
                services.push(Service {
                    tick: first_tick(start),
                    padding,
                });
                start
            })
            .collect();
        Self { starts, services }
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

fn first_tick(start: Time) -> u64 {
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
