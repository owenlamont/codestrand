use std::time::{Duration, Instant};

use crate::model::Time;

/// How often export writes a progress or waiting line.
pub(super) const INTERVAL: Duration = Duration::from_secs(10);

/// What export knows about the whole video at one moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Length {
    /// `--frames` fixed the frame count.
    Requested(u32),
    /// Planning finished, so the frame count is exact.
    Planned(u32),
    /// Later windows are unplanned; walk waits planned so far scale to the rest.
    Projected(u32),
}

impl Length {
    fn frames(self) -> u32 {
        let (Self::Requested(frames) | Self::Planned(frames) | Self::Projected(frames)) =
            self;
        frames
    }
}

#[derive(Clone, Copy)]
struct Arrival {
    at: Instant,
    written: u32,
}

/// Paces export progress lines and remembers when planning windows arrived.
pub(super) struct Report {
    started: Instant,
    printed: Instant,
    fps: u16,
    planned: Option<Time>,
    /// The two latest window arrivals, older first.
    arrivals: [Option<Arrival>; 2],
    blocked: Option<Instant>,
}

impl Report {
    pub(super) fn new(now: Instant, fps: u16) -> Self {
        Self {
            started: now,
            printed: now,
            fps,
            planned: None,
            arrivals: [None, None],
            blocked: None,
        }
    }

    /// Notes a newly planned window when `planned` history has moved on.
    pub(super) fn planned(&mut self, now: Instant, planned: Time, written: u32) {
        self.blocked = None;
        if self.planned != Some(planned) {
            self.planned = Some(planned);
            self.arrivals = [self.arrivals[1], Some(Arrival { at: now, written })];
        }
    }

    /// A progress line once per interval.
    pub(super) fn progress(
        &mut self,
        now: Instant,
        written: u32,
        length: Length,
        history: (Time, Time),
    ) -> Option<String> {
        self.due(now)?;
        let eta = self.eta(now, written, length.frames());
        Some(progress_line(
            written,
            length,
            history,
            self.fps,
            now - self.started,
            eta,
        ))
    }

    /// A waiting line once per interval after rendering has been blocked on planning
    /// for an interval; shorter waits stay quiet.
    pub(super) fn waiting(&mut self, now: Instant, planned: Time) -> Option<String> {
        let blocked = *self.blocked.get_or_insert(now);
        if now - blocked < INTERVAL {
            return None;
        }
        self.due(now)?;
        let arrivals = match self.arrivals {
            [_, None] => String::new(),
            [None, Some(latest)] => {
                format!("; last window arrived {} ago", clock(now - latest.at))
            }
            [Some(previous), Some(latest)] => format!(
                "; last window arrived {} ago, {} after the one before",
                clock(now - latest.at),
                clock(latest.at - previous.at)
            ),
        };
        Some(format!(
            "Waiting for planning beyond history {:.1}s: rendering blocked for {}{arrivals}",
            planned.lossy_secs_f32(),
            clock(now - blocked),
        ))
    }

    fn due(&mut self, now: Instant) -> Option<()> {
        (now - self.printed >= INTERVAL).then(|| self.printed = now)
    }

    /// Remaining frames at the rate achieved since the older of the two latest
    /// window arrivals, which spans at least one whole planning cycle.
    fn eta(&self, now: Instant, written: u32, frames: u32) -> Option<Duration> {
        let base = self.arrivals[0].unwrap_or(Arrival {
            at: self.started,
            written: 0,
        });
        let done = written.checked_sub(base.written).filter(|&done| done > 0)?;
        let remaining = frames.saturating_sub(written);
        Some((now - base.at).mul_f64(f64::from(remaining) / f64::from(done)))
    }
}

/// Paces lines about planning the first window, which renders nothing until it lands.
pub(crate) struct Loading {
    started: Instant,
    printed: Instant,
    stage: Option<crate::progress::Stage>,
}

impl Loading {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            started: now,
            printed: now,
            stage: None,
        }
    }

    /// Notes the latest planning stage, and once per interval says which it is.
    pub(crate) fn observe(
        &mut self,
        now: Instant,
        update: Option<crate::progress::Update>,
    ) -> Option<String> {
        if let Some(update) = update {
            self.stage = Some(update.stage);
        }
        (now - self.printed >= INTERVAL).then(|| {
            self.printed = now;
            let stage = self.stage.map_or("starting", crate::progress::Stage::label);
            format!(
                "Planning the first window: {stage}, {} elapsed",
                clock(now - self.started)
            )
        })
    }
}

fn progress_line(
    written: u32,
    length: Length,
    (history, total): (Time, Time),
    fps: u16,
    elapsed: Duration,
    eta: Option<Duration>,
) -> String {
    // Playback runs past the last commit for repository exits and the tail.
    let history = history.min(total);
    let history = format!(
        "history {:.1}/{:.1}s",
        history.lossy_secs_f32(),
        total.lossy_secs_f32()
    ) + &format!(
        " ({:.1}%)",
        percent(history.as_secs_f64(), total.as_secs_f64())
    );
    let video = |frames: u32| {
        clock(Duration::from_secs_f64(f64::from(frames) / f64::from(fps)))
    };
    let progress = match length {
        Length::Requested(frames) => format!("{written}/{frames} frames; {history}"),
        Length::Planned(frames) => format!(
            "{written} frames; {history}; video {} of {} ({frames} frames)",
            video(written),
            video(frames),
        ),
        Length::Projected(frames) => format!(
            "{written} frames; {history}; video {} of ~{} (~{frames} frames)",
            video(written),
            video(frames),
        ),
    };
    let eta = eta.map_or_else(
        || "ETA unknown".to_owned(),
        |eta| {
            let approximate = if matches!(length, Length::Projected(_)) {
                "~"
            } else {
                ""
            };
            format!("ETA {approximate}{}", clock(eta))
        },
    );
    format!("Exported {progress}; {} elapsed, {eta}", clock(elapsed))
}

fn percent(done: f64, total: f64) -> f64 {
    if total > 0.0 {
        (done / total * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// Wall or video time as `M:SS`, or `H:MM:SS` from an hour.
fn clock(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HISTORY: (Time, Time) = (Time::seconds(20.0), Time::seconds(80.0));

    fn at(start: Instant, seconds: u64) -> Instant {
        start + Duration::from_secs(seconds)
    }

    #[test]
    fn lines_say_how_sure_the_video_length_is() {
        let line = |length| {
            progress_line(
                900,
                length,
                HISTORY,
                30,
                Duration::from_secs(75),
                Some(Duration::from_secs(3725)),
            )
        };
        assert_eq!(
            line(Length::Projected(5400)),
            "Exported 900 frames; history 20.0/80.0s (25.0%); video 0:30 of ~3:00 \
             (~5400 frames); 1:15 elapsed, ETA ~1:02:05"
        );
        assert_eq!(
            line(Length::Planned(5400)),
            "Exported 900 frames; history 20.0/80.0s (25.0%); video 0:30 of 3:00 \
             (5400 frames); 1:15 elapsed, ETA 1:02:05"
        );
        assert_eq!(
            progress_line(
                90,
                Length::Planned(90),
                (Time::seconds(4.0), Time::seconds(3.0)),
                30,
                Duration::ZERO,
                None
            ),
            "Exported 90 frames; history 3.0/3.0s (100.0%); video 0:03 of 0:03 (90 frames); \
             0:00 elapsed, ETA unknown"
        );
        assert_eq!(
            line(Length::Requested(1000)),
            "Exported 900/1000 frames; history 20.0/80.0s (25.0%); 1:15 elapsed, \
             ETA 1:02:05"
        );
    }

    #[test]
    fn progress_prints_once_per_interval() {
        let start = Instant::now();
        let mut report = Report::new(start, 30);
        let mut lines = Vec::new();
        for second in [1, 9, 10, 15, 19, 20] {
            let length = Length::Planned(3000);
            lines.push(
                report
                    .progress(at(start, second), 300, length, HISTORY)
                    .is_some(),
            );
        }
        assert_eq!(lines, [false, false, true, false, false, true]);
    }

    #[test]
    fn eta_follows_the_rate_since_the_older_of_the_latest_two_windows() {
        let start = Instant::now();
        let mut report = Report::new(start, 30);
        let first = Time::seconds(60.0);
        report.planned(start, first, 0);
        assert_eq!(report.eta(at(start, 10), 0, 900), None);
        assert_eq!(
            report.eta(at(start, 10), 100, 900),
            Some(Duration::from_secs(80))
        );
        report.planned(at(start, 30), first, 300);
        report.planned(at(start, 40), Time::seconds(120.0), 400);
        report.planned(at(start, 100), Time::seconds(180.0), 500);
        // 100 frames in the 60 s since the second window: 400 left take 240 s.
        assert_eq!(
            report.eta(at(start, 100), 500, 900),
            Some(Duration::from_secs(240))
        );
    }

    #[test]
    fn a_stall_reports_how_long_rendering_has_waited() {
        let start = Instant::now();
        let mut report = Report::new(start, 30);
        report.planned(start, Time::seconds(60.0), 0);
        report.planned(at(start, 50), Time::seconds(120.0), 1800);
        assert_eq!(report.waiting(at(start, 55), Time::seconds(120.0)), None);
        assert_eq!(
            report
                .waiting(at(start, 125), Time::seconds(120.0))
                .unwrap(),
            "Waiting for planning beyond history 120.0s: rendering blocked for 1:10; \
             last window arrived 1:15 ago, 0:50 after the one before"
        );
        report.planned(at(start, 130), Time::seconds(180.0), 1800);
        assert_eq!(report.waiting(at(start, 140), Time::seconds(180.0)), None);
        assert_eq!(
            report
                .waiting(at(start, 151), Time::seconds(180.0))
                .unwrap(),
            "Waiting for planning beyond history 180.0s: rendering blocked for 0:11; \
             last window arrived 0:21 ago, 1:20 after the one before"
        );
    }

    #[test]
    fn first_window_planning_keeps_reporting_without_updates() {
        let start = Instant::now();
        let mut loading = Loading::new(start);
        let update = crate::progress::Update {
            stage: crate::progress::Stage::Travel,
            pass: 3,
            fraction: 0.4,
        };
        assert_eq!(loading.observe(at(start, 1), Some(update)), None);
        assert_eq!(
            loading.observe(at(start, 10), None).unwrap(),
            "Planning the first window: planning travel, 0:10 elapsed"
        );
        assert_eq!(loading.observe(at(start, 15), None), None);
        assert_eq!(
            loading.observe(at(start, 3600), None).unwrap(),
            "Planning the first window: planning travel, 1:00:00 elapsed"
        );
    }
}
