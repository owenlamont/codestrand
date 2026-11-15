use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    History,
    Directories,
    Layout,
    Travel,
    Reservations,
    Titles,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::History => "reading history",
            Self::Directories => "planning directories",
            Self::Layout => "replaying layout",
            Self::Travel => "planning travel",
            Self::Reservations => "reserving parking",
            Self::Titles => "placing titles",
        }
    }
}

/// How far one planning stage has come; travel planning repeats stages in passes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Update {
    pub(crate) stage: Stage,
    pub(crate) pass: u32,
    pub(crate) fraction: f32,
}

impl std::fmt::Display for Update {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Planning: {} {:.0}%",
            self.stage.label(),
            self.fraction * 100.0
        )?;
        if self.pass > 1 {
            write!(formatter, " (pass {})", self.pass)?;
        }
        Ok(())
    }
}

struct Channel {
    sender: mpsc::Sender<Update>,
    interval: Duration,
    created: Instant,
    sent_nanos: AtomicU64,
    pass: AtomicU32,
    stderr: AtomicBool,
}

/// The sending end of planning progress; a disabled one costs a branch per report.
#[derive(Clone, Default)]
pub(crate) struct Progress(Option<Arc<Channel>>);

impl Progress {
    /// A channel that carries at most one update per `interval`, the first after one.
    pub(crate) fn channel(interval: Duration) -> (Self, mpsc::Receiver<Update>) {
        let (sender, receiver) = mpsc::channel();
        let channel = Channel {
            sender,
            interval,
            created: Instant::now(),
            sent_nanos: AtomicU64::new(0),
            pass: AtomicU32::new(1),
            stderr: AtomicBool::new(false),
        };
        (Self(Some(Arc::new(channel))), receiver)
    }

    pub(crate) fn pass(&self, pass: u32) {
        if let Some(channel) = &self.0 {
            channel.pass.store(pass, Ordering::Relaxed);
        }
    }

    /// Keep export progress visible after the initial loader returns its first plan.
    pub(crate) fn continue_on_stderr(&self) {
        if let Some(channel) = &self.0 {
            channel.stderr.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn report(&self, stage: Stage, done: u64, total: u64) {
        if let Some(channel) = &self.0 {
            self.report_at(channel.created.elapsed(), stage, done, total);
        }
    }

    fn report_at(&self, elapsed: Duration, stage: Stage, done: u64, total: u64) {
        let Some(channel) = &self.0 else { return };
        let now = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        let interval = u64::try_from(channel.interval.as_nanos()).unwrap_or(u64::MAX);
        let due = |sent: u64| (now.saturating_sub(sent) >= interval).then_some(now);
        if channel
            .sent_nanos
            .try_update(Ordering::Relaxed, Ordering::Relaxed, due)
            .is_err()
        {
            return;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a displayed fraction needs no more than f32 precision"
        )]
        let fraction = (done as f32 / total.max(1) as f32).min(1.0);
        let update = Update {
            stage,
            pass: channel.pass.load(Ordering::Relaxed),
            fraction,
        };
        if channel.stderr.load(Ordering::Relaxed) {
            eprintln!("{update}");
        } else {
            // A closed receiver means nobody is watching; planning carries on.
            let _ = channel.sender.send(update);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_arrive_at_most_once_per_interval() {
        let (progress, updates) = Progress::channel(Duration::from_secs(1));
        for (millis, done) in [(0, 0), (500, 1), (1000, 2), (1200, 3), (2100, 4)] {
            progress.report_at(Duration::from_millis(millis), Stage::Layout, done, 4);
        }
        progress.pass(2);
        progress.report_at(Duration::from_millis(3100), Stage::Travel, 9, 4);
        drop(progress);
        let lines: Vec<_> = updates.iter().map(|update| update.to_string()).collect();
        assert_eq!(
            lines,
            [
                "Planning: replaying layout 50%",
                "Planning: replaying layout 100%",
                "Planning: planning travel 100% (pass 2)",
            ]
        );
    }

    #[test]
    fn a_disabled_or_unwatched_channel_accepts_reports() {
        Progress::default().report(Stage::History, 1, 2);
        let (progress, updates) = Progress::channel(Duration::ZERO);
        drop(updates);
        progress.report(Stage::History, 1, 2);
    }
}
