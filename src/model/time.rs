use std::ops::{Add, AddAssign, Sub};

use super::{TICK_RATE, count_u32, tick_seconds};

const SPAN_SECONDS: u32 = 16_384;
const SPAN: f32 = 16_384.0;
const SPAN_TICKS: u64 = SPAN_SECONDS as u64 * TICK_RATE as u64;

/// A timeline position: whole 16,384-second spans plus `f32` seconds into the span.
///
/// Within the first span every operation is the plain `f32` operation on seconds.
/// Later spans keep that span's resolution, so a tick stays resolvable at any length.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub(crate) struct Time {
    span: u32,
    seconds: f32,
}

impl Time {
    pub(crate) const ZERO: Self = Self::seconds(0.0);
    pub(crate) const INFINITY: Self = Self {
        span: u32::MAX,
        seconds: f32::INFINITY,
    };
    pub(crate) const NEG_INFINITY: Self = Self::seconds(f32::NEG_INFINITY);

    /// A position in the first span; `seconds` must be below one span.
    pub(crate) const fn seconds(seconds: f32) -> Self {
        debug_assert!(seconds < SPAN, "later spans come from arithmetic");
        Self { span: 0, seconds }
    }

    fn new(span: u32, seconds: f32) -> Self {
        if seconds == f32::INFINITY {
            return Self::INFINITY;
        }
        if seconds >= SPAN {
            let spans = f64::from(seconds / SPAN).floor();
            if spans >= f64::from(u32::MAX - span) {
                return Self::INFINITY;
            }
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a whole number between one and the remaining spans"
            )]
            let whole = spans as u32;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a remainder below one span, exact in f32"
            )]
            let rest = (f64::from(seconds) - spans * f64::from(SPAN)) as f32;
            return Self {
                span: span + whole,
                seconds: rest,
            };
        }
        if seconds < 0.0 && span > 0 {
            if seconds == f32::NEG_INFINITY {
                return Self::NEG_INFINITY;
            }
            let spans = f64::from(-seconds / SPAN)
                .ceil()
                .clamp(1.0, f64::from(span));
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a whole number between one and the current span"
            )]
            let whole = spans as u32;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "seconds within a few spans round to the nearest f32"
            )]
            let rest = (f64::from(seconds) + spans * f64::from(SPAN)) as f32;
            return Self::new(span - whole, rest);
        }
        Self { span, seconds }
    }

    /// The time of a simulation tick; ticks past the last span are infinitely late.
    pub(crate) fn from_tick(tick: u64) -> Self {
        let Ok(span) = u32::try_from(tick / SPAN_TICKS) else {
            return Self::INFINITY;
        };
        let rest = u32::try_from(tick % SPAN_TICKS).expect("ticks within a span");
        Self {
            span,
            seconds: count_u32(rest / TICK_RATE)
                + count_u32(rest % TICK_RATE) * tick_seconds(),
        }
    }

    /// Whole ticks up to this time, rounded up or down; negative times have none.
    pub(crate) fn ticks(self, round_up: bool) -> u64 {
        let nanoticks = std::time::Duration::from_secs_f32(self.seconds.max(0.0))
            .as_nanos()
            * u128::from(TICK_RATE);
        let rest = if round_up {
            nanoticks.div_ceil(1_000_000_000)
        } else {
            nanoticks / 1_000_000_000
        };
        u64::from(self.span) * SPAN_TICKS
            + u64::try_from(rest).expect("ticks within a span")
    }

    /// The whole `step`s before this time, at most `last`, and the seconds past them.
    pub(crate) fn steps(self, step: f32, last: usize) -> (usize, f32) {
        if self.span == 0 {
            let whole = (self.seconds.max(0.0) / step).floor();
            let index =
                usize::try_from(std::time::Duration::from_secs_f32(whole).as_secs())
                    .unwrap_or(usize::MAX)
                    .min(last);
            let before =
                count_u32(u32::try_from(index).expect("step index fits u32")) * step;
            return (index, self.seconds - before);
        }
        let whole = (self.as_secs_f64() / f64::from(step)).floor();
        let index =
            usize::try_from(std::time::Duration::from_secs_f64(whole).as_secs())
                .unwrap_or(usize::MAX)
                .min(last);
        let before = f64::from(u32::try_from(index).expect("step index fits u32"))
            * f64::from(step);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "seconds past a step round to the nearest f32"
        )]
        let rest = (self.as_secs_f64() - before) as f32;
        (index, rest)
    }

    pub(crate) fn as_secs_f64(self) -> f64 {
        f64::from(self.span) * f64::from(SPAN) + f64::from(self.seconds)
    }

    /// Seconds as one `f32`, for reports; it loses tick resolution in later spans.
    pub(crate) fn lossy_secs_f32(self) -> f32 {
        if self.span == 0 {
            return self.seconds;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "display rounds to the nearest f32"
        )]
        let seconds = self.as_secs_f64() as f32;
        seconds
    }

    /// This position moved later by `elapsed`, a length of time counted from zero.
    pub(crate) fn after(self, elapsed: Self) -> Self {
        if elapsed.span == u32::MAX || self.span.checked_add(elapsed.span).is_none() {
            return Self::INFINITY;
        }
        Self::new(self.span + elapsed.span, self.seconds + elapsed.seconds)
    }

    /// This time as it rounds when added to `origin` and taken back in one `f32`.
    ///
    /// Exports in the first span were made with that rounding; later spans keep `self`.
    pub(crate) fn rounded_through(self, origin: Self) -> Self {
        if self.span > 0 {
            return self;
        }
        Self::ZERO + (origin.after(self) - origin)
    }

    pub(crate) fn min(self, other: Self) -> Self {
        if other < self { other } else { self }
    }

    pub(crate) fn max(self, other: Self) -> Self {
        if other > self { other } else { self }
    }

    pub(crate) fn total_cmp(self, other: Self) -> std::cmp::Ordering {
        self.span
            .cmp(&other.span)
            .then(self.seconds.total_cmp(&other.seconds))
    }

    pub(crate) fn is_finite(self) -> bool {
        self.seconds.is_finite()
    }
}

impl std::fmt::Display for Time {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.as_secs_f64(), formatter)
    }
}

impl Add<f32> for Time {
    type Output = Self;

    fn add(self, seconds: f32) -> Self {
        Self::new(self.span, self.seconds + seconds)
    }
}

impl AddAssign<f32> for Time {
    fn add_assign(&mut self, seconds: f32) {
        *self = *self + seconds;
    }
}

impl Sub<f32> for Time {
    type Output = Self;

    fn sub(self, seconds: f32) -> Self {
        Self::new(self.span, self.seconds - seconds)
    }
}

impl Sub for Time {
    type Output = f32;

    fn sub(self, earlier: Self) -> f32 {
        if self.span == earlier.span {
            return self.seconds - earlier.seconds;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "an elapsed time rounds to the nearest f32"
        )]
        let elapsed = ((f64::from(self.span) - f64::from(earlier.span))
            * f64::from(SPAN)
            + (f64::from(self.seconds) - f64::from(earlier.seconds)))
            as f32;
        elapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl PartialEq<f32> for Time {
        fn eq(&self, seconds: &f32) -> bool {
            self.as_secs_f64() == f64::from(*seconds)
        }
    }

    impl PartialEq<f64> for Time {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "an untyped literal in a test means the f32 nearest to it"
        )]
        fn eq(&self, seconds: &f64) -> bool {
            *self == *seconds as f32
        }
    }

    impl Time {
        pub(crate) fn next_up(self) -> Self {
            Self::new(self.span, self.seconds.next_up())
        }

        pub(crate) fn next_down(self) -> Self {
            Self::new(self.span, self.seconds.next_down())
        }
    }

    #[test]
    fn first_span_arithmetic_is_plain_f32_arithmetic() {
        for (a, b) in [(0.5_f32, 1.3), (1234.567, 0.016_666_668), (16_000.0, 383.9)] {
            assert_eq!(
                (Time::seconds(a) + b).lossy_secs_f32().to_bits(),
                (a + b).to_bits()
            );
            assert_eq!(
                (Time::seconds(a) - b).lossy_secs_f32().to_bits(),
                (a - b).to_bits()
            );
            assert_eq!(
                (Time::seconds(a) - Time::seconds(b)).to_bits(),
                (a - b).to_bits()
            );
            assert_eq!(Time::seconds(a) < Time::seconds(b), a < b);
        }
        for tick in [0, 1, 59, 60, 61, 863_999, SPAN_TICKS - 1] {
            let whole = u32::try_from(tick / 60).unwrap();
            let part = u32::try_from(tick % 60).unwrap();
            assert_eq!(
                Time::from_tick(tick),
                Time::seconds(count_u32(whole) + count_u32(part) * tick_seconds())
            );
        }
    }

    #[test]
    fn values_past_a_span_keep_their_f32_meaning() {
        for (a, b) in [
            (16_000.0_f32, 500.0_f32),
            (14_400.0, 86_400.0),
            (0.5, 1.0e9),
        ] {
            let late = Time::seconds(a) + b;
            assert!(late.span > 0 && late.seconds < SPAN);
            assert_eq!(late.lossy_secs_f32().to_bits(), (a + b).to_bits());
            assert_eq!((late - Time::seconds(a)).to_bits(), (a + b - a).to_bits());
            assert_eq!((late - b).lossy_secs_f32().to_bits(), (a + b - b).to_bits());
            assert!(late > Time::seconds(a) && late < Time::INFINITY);
        }
        assert_eq!(Time::ZERO + f32::INFINITY, Time::INFINITY);
        assert_eq!(Time::ZERO + f32::MAX, Time::INFINITY);
        assert_eq!(Time::from_tick(u64::MAX), Time::INFINITY);
        assert_eq!(Time::INFINITY - Time::ZERO, f32::INFINITY);
        assert!(Time::NEG_INFINITY < Time::seconds(-1.0));
        let boundary = Time::from_tick(3 * SPAN_TICKS);
        assert!(boundary.next_down() <= boundary && boundary - 1.0 < boundary);
        assert_eq!(boundary - (boundary - 1.0), 1.0);
        assert!(!Time::INFINITY.is_finite());
    }

    /// Ticks within five seconds either side of the start of spans 1, 11 and 40.
    fn seam_ticks() -> impl Iterator<Item = u64> {
        [1, 11, 40]
            .into_iter()
            .flat_map(|span| span * SPAN_TICKS - 300..span * SPAN_TICKS + 300)
    }

    fn assert_normal(time: Time) {
        assert!(
            (0.0..SPAN).contains(&time.seconds),
            "{time:?} has a remainder outside its span"
        );
    }

    #[test]
    fn steps_across_a_span_boundary_stay_within_one_f32_step_of_exact() {
        let step = f64::from(SPAN - SPAN.next_down());
        for span in [1, 11, 40] {
            let mut now = Time::from_tick(span * SPAN_TICKS - 300);
            for _ in 0..600 {
                let exact = now.as_secs_f64() + f64::from(tick_seconds());
                now += tick_seconds();
                assert_normal(now);
                assert!((now.as_secs_f64() - exact).abs() <= step, "{now:?}");
            }
            assert_eq!(now.span, u32::try_from(span).unwrap());
        }
    }

    #[test]
    fn differences_across_a_span_boundary_are_exact_both_ways() {
        let step = f64::from(SPAN - SPAN.next_down());
        for span in [1, 11, 40] {
            let boundary = span * SPAN_TICKS;
            for (before, after) in [(1, 1), (7, 130), (299, 299)] {
                let early = Time::from_tick(boundary - before);
                let late = Time::from_tick(boundary + after);
                let exact = late.as_secs_f64() - early.as_secs_f64();
                assert!((f64::from(late - early) - exact).abs() <= step);
                assert!((f64::from(early - late) + exact).abs() <= step);
                assert!(
                    ((early + (late - early)) - late).abs() <= SPAN - SPAN.next_down()
                );
                assert!(
                    ((late - (late - early)) - early).abs() <= SPAN - SPAN.next_down()
                );
            }
        }
    }

    #[test]
    fn ticks_across_a_span_boundary_keep_one_order_and_one_representation() {
        for tick in seam_ticks() {
            let (time, next) = (Time::from_tick(tick), Time::from_tick(tick + 1));
            assert_normal(time);
            assert!(time < next);
            assert_ne!(time, next);
            assert_eq!(time.total_cmp(next), std::cmp::Ordering::Less);
            assert_eq!(time.min(next), time);
            assert_eq!(time.max(next), next);
            assert_normal(time - tick_seconds());
            assert_normal(time.after(Time::seconds(tick_seconds())));
        }
        let boundary = Time::from_tick(SPAN_TICKS);
        assert_eq!(Time::ZERO + SPAN, boundary);
        assert_eq!(Time::seconds(SPAN - 1.0) + 1.0, boundary);
        assert_eq!(
            Time::seconds(SPAN - 1.0).after(Time::seconds(1.0)),
            boundary
        );
        assert_eq!((boundary + 1.0) - 1.0, boundary);
    }

    #[test]
    fn every_tick_is_distinct_and_exact_two_days_in() {
        let two_days = 48 * 60 * 60 * u64::from(TICK_RATE);
        for tick in two_days..two_days + 2 * SPAN_TICKS {
            let time = Time::from_tick(tick);
            assert!(time < Time::from_tick(tick + 1));
            assert!((Time::from_tick(tick + 1) - time - tick_seconds()).abs() < 1e-3);
            for round_up in [false, true] {
                assert_eq!(
                    time.ticks(round_up)
                        - Time::from_tick(tick % SPAN_TICKS).ticks(round_up),
                    tick / SPAN_TICKS * SPAN_TICKS
                );
            }
        }
    }
}
