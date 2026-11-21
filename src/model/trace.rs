use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

/// A timed part of planning, listed in the order planning reaches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Phase {
    Build,
    ScheduledLayout,
    PlanRepositories,
    Regions,
    TravelPadding,
    Pass,
    ScheduleRebuild,
    CheckSchedule,
    WindowClock,
    Restart,
    AvatarEnvelopes,
    PlanTravel,
    Capture,
    Routes,
    AdjustPadding,
    CompactPadding,
    RefineServiceSlots,
    Reserve,
    Caption,
    CheckpointReplay,
    TickPacking,
    TickHeat,
    TickEvents,
    TickFade,
    TickCompact,
    TickPrune,
    TickRelax,
    TickFiles,
    TickPlace,
    TickSlots,
    TickAvatars,
    TickLabels,
    TickNames,
}

/// The sections of one playback tick, in the order the tick runs them.
const TICK_PHASES: [Phase; 13] = [
    Phase::TickPacking,
    Phase::TickHeat,
    Phase::TickEvents,
    Phase::TickFade,
    Phase::TickCompact,
    Phase::TickPrune,
    Phase::TickRelax,
    Phase::TickFiles,
    Phase::TickPlace,
    Phase::TickSlots,
    Phase::TickAvatars,
    Phase::TickLabels,
    Phase::TickNames,
];

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::ScheduledLayout => "scheduled_layout",
            Self::PlanRepositories => "plan_repositories",
            Self::Regions => "regions",
            Self::TravelPadding => "travel_padding",
            Self::Pass => "pass",
            Self::ScheduleRebuild => "schedule_rebuild",
            Self::CheckSchedule => "check_schedule",
            Self::WindowClock => "window_clock",
            Self::Restart => "restart",
            Self::AvatarEnvelopes => "avatar_envelopes",
            Self::PlanTravel => "plan_travel",
            Self::Capture => "capture",
            Self::Routes => "routes",
            Self::AdjustPadding => "adjust_padding",
            Self::CompactPadding => "compact_padding",
            Self::RefineServiceSlots => "refine_service_slots",
            Self::Reserve => "reserve",
            Self::Caption => "caption",
            Self::CheckpointReplay => "checkpoint_replay",
            Self::TickPacking => "tick_packing",
            Self::TickHeat => "tick_heat",
            Self::TickEvents => "tick_events",
            Self::TickFade => "tick_fade",
            Self::TickCompact => "tick_compact",
            Self::TickPrune => "tick_prune",
            Self::TickRelax => "tick_relax",
            Self::TickFiles => "tick_files",
            Self::TickPlace => "tick_place",
            Self::TickSlots => "tick_slots",
            Self::TickAvatars => "tick_avatars",
            Self::TickLabels => "tick_labels",
            Self::TickNames => "tick_names",
        }
    }
}

/// A count of work done, reported under the phase open when it was added.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Counter {
    Replays,
    TicksReplayed,
    RouteCandidates,
    PairComparisons,
    Rebuilds,
    Events,
    /// Events times padding entries: `padding_at` stops at its first match, so real
    /// comparisons are fewer.
    PaddingComparisonsBound,
    /// Envelope replays skipped because the pass began with unchanged padding.
    EnvelopeReuses,
    /// Refinement loops stopped because a round ended in the state it began in.
    SettledRefinements,
    Files,
    Directories,
    Avatars,
    Trips,
    /// Region tilings sampled, and those that kept the previous feasibility polygons.
    Tilings,
    TilingReuses,
    FileTicks,
    SteadyFileTicks,
    RestingFileTicks,
    ColdFileTicks,
    DirectoryTicks,
    StillDirectoryTicks,
}

impl Counter {
    fn name(self) -> &'static str {
        match self {
            Self::Replays => "replays",
            Self::TicksReplayed => "ticks_replayed",
            Self::RouteCandidates => "route_candidates",
            Self::PairComparisons => "pair_comparisons",
            Self::Rebuilds => "rebuilds",
            Self::Events => "events",
            Self::PaddingComparisonsBound => "padding_comparisons_bound",
            Self::EnvelopeReuses => "envelope_reuses",
            Self::SettledRefinements => "settled_refinements",
            Self::Files => "files",
            Self::Directories => "directories",
            Self::Avatars => "avatars",
            Self::Trips => "trips",
            Self::Tilings => "tilings",
            Self::TilingReuses => "tiling_reuses",
            Self::FileTicks => "file_ticks",
            Self::SteadyFileTicks => "steady_file_ticks",
            Self::RestingFileTicks => "resting_file_ticks",
            Self::ColdFileTicks => "cold_file_ticks",
            Self::DirectoryTicks => "directory_ticks",
            Self::StillDirectoryTicks => "still_directory_ticks",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    Global,
    Window(usize),
}

impl fmt::Display for Scope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global => formatter.write_str("global"),
            Self::Window(index) => index.fmt(formatter),
        }
    }
}

type Path = Vec<Phase>;

#[derive(Default)]
struct Totals {
    open: Path,
    phases: BTreeMap<Path, (Duration, u64)>,
    counters: BTreeMap<(Path, Counter), u64>,
    #[cfg(test)]
    reports: Vec<Report>,
}

/// One scope's totals; a nested phase is keyed by the phases open around it.
#[derive(Clone, Debug)]
pub(crate) struct Report {
    scope: Scope,
    commits: usize,
    passes: u32,
    total: Duration,
    phases: BTreeMap<Path, (Duration, u64)>,
    counters: BTreeMap<(Path, Counter), u64>,
}

fn key(path: &[Phase]) -> String {
    path.iter()
        .map(|phase| phase.name())
        .collect::<Vec<_>>()
        .join(".")
}

impl Report {
    fn children(&self, parent: &[Phase]) -> impl Iterator<Item = Duration> {
        self.phases
            .iter()
            .filter(move |(path, _)| {
                path.len() == parent.len() + 1 && path.starts_with(parent)
            })
            .map(|(_, &(time, _))| time)
    }

    #[cfg(test)]
    pub(crate) fn passes(&self) -> u32 {
        self.passes
    }

    #[cfg(test)]
    pub(crate) fn calls(&self, path: &[Phase]) -> u64 {
        self.phases.get(path).map_or(0, |&(_, calls)| calls)
    }

    #[cfg(test)]
    pub(crate) fn counter(&self, path: &[Phase], counter: Counter) -> u64 {
        self.counters
            .get(&(path.to_vec(), counter))
            .copied()
            .unwrap_or(0)
    }

    /// Whether no phase ran longer than the phase or scope that contains it.
    #[cfg(test)]
    pub(crate) fn nests(&self) -> bool {
        self.children(&[]).sum::<Duration>() <= self.total
            && self
                .phases
                .iter()
                .all(|(path, &(time, _))| self.children(path).sum::<Duration>() <= time)
    }

    /// Everything two runs of one input must agree on: the line without its times.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> String {
        self.to_string()
            .split(' ')
            .filter(|field| !field.contains("_s="))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl fmt::Display for Report {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "planning_trace window={} commits={} passes={} total_s={:.6}",
            self.scope,
            self.commits,
            self.passes,
            self.total.as_secs_f64()
        )?;
        for (path, &(time, calls)) in &self.phases {
            let name = key(path);
            write!(
                formatter,
                " {name}_s={:.6} {name}_calls={calls}",
                time.as_secs_f64()
            )?;
            let nested = self.children(path).sum::<Duration>();
            if !nested.is_zero() {
                write!(
                    formatter,
                    " {name}.rest_s={:.6}",
                    time.saturating_sub(nested).as_secs_f64()
                )?;
            }
        }
        for ((path, counter), count) in &self.counters {
            let separator = if path.is_empty() { "" } else { "." };
            write!(
                formatter,
                " {}{separator}{}={count}",
                key(path),
                counter.name()
            )?;
        }
        write!(
            formatter,
            " unattributed_s={:.6}",
            self.total
                .saturating_sub(self.children(&[]).sum())
                .as_secs_f64()
        )
    }
}

/// Where planning time goes; a disabled trace costs a branch per call.
#[derive(Clone, Default)]
pub(crate) struct Trace(Option<Arc<Mutex<Totals>>>);

impl fmt::Debug for Trace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Trace").field(&self.on()).finish()
    }
}

fn lock(totals: &Mutex<Totals>) -> MutexGuard<'_, Totals> {
    totals.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Trace {
    const VARIABLE: &str = "CODESTRAND_TRACE_PLANNING";

    /// A trace that is on when `CODESTRAND_TRACE_PLANNING` is set to any value.
    pub(crate) fn from_env() -> Self {
        if std::env::var_os(Self::VARIABLE).is_some() {
            Self::enabled()
        } else {
            Self::default()
        }
    }

    pub(crate) fn enabled() -> Self {
        Self(Some(Arc::default()))
    }

    pub(crate) fn on(&self) -> bool {
        self.0.is_some()
    }

    /// Times from now until the returned span drops, nested inside any open span.
    pub(crate) fn phase(&self, phase: Phase) -> Span {
        Span(self.0.as_ref().map(|totals| {
            lock(totals).open.push(phase);
            (totals.clone(), Instant::now())
        }))
    }

    /// Adds to a counter of the open phase; call once per loop, not per iteration.
    pub(crate) fn count(&self, counter: Counter, amount: usize) {
        self.add(counter, u64::try_from(amount).unwrap_or(u64::MAX));
    }

    fn add(&self, counter: Counter, amount: u64) {
        if let Some(totals) = &self.0 {
            let mut totals = lock(totals);
            let path = totals.open.clone();
            *totals.counters.entry((path, counter)).or_default() += amount;
        }
    }

    pub(crate) fn count_replay(&self, ticks: u64) {
        self.add(Counter::Replays, 1);
        self.add(Counter::TicksReplayed, ticks);
    }

    pub(crate) fn count_rebuild(&self, events: usize, padding: usize) {
        self.count(Counter::Rebuilds, 1);
        self.count(Counter::Events, events);
        self.count(Counter::PaddingComparisonsBound, events * padding);
    }

    /// Moves the tick sections and layout counts since the last call under the open phase.
    pub(crate) fn add_ticks(&self, times: &mut TickTimes) {
        let Some(totals) = &self.0 else { return };
        let counts = std::mem::take(&mut times.counts);
        for (counter, amount) in [
            (Counter::FileTicks, counts.files),
            (Counter::SteadyFileTicks, counts.steady_files),
            (Counter::RestingFileTicks, counts.resting_files),
            (Counter::ColdFileTicks, counts.cold_files),
            (Counter::DirectoryTicks, counts.directories),
            (Counter::StillDirectoryTicks, counts.still_directories),
        ] {
            if amount > 0 {
                self.count(counter, amount);
            }
        }
        let times = &mut times.laps;
        let mut totals = lock(totals);
        for (phase, time) in TICK_PHASES
            .into_iter()
            .zip(std::mem::take(&mut times.totals))
        {
            if let Some(time) = time {
                let mut path = totals.open.clone();
                path.push(phase);
                let (total, calls) = totals.phases.entry(path).or_default();
                *total += time;
                *calls += 1;
            }
        }
    }

    /// Prints the totals since the last report as one stderr line, then resets them.
    pub(crate) fn report(
        &self,
        scope: Scope,
        commits: usize,
        passes: u32,
        total: Duration,
    ) {
        let Some(totals) = &self.0 else { return };
        let mut totals = lock(totals);
        let report = Report {
            scope,
            commits,
            passes,
            total,
            phases: std::mem::take(&mut totals.phases),
            counters: std::mem::take(&mut totals.counters),
        };
        eprintln!("{report}");
        #[cfg(test)]
        totals.reports.push(report);
    }

    #[cfg(test)]
    pub(crate) fn reports(&self) -> Vec<Report> {
        self.0
            .as_ref()
            .map_or_else(Vec::new, |totals| lock(totals).reports.clone())
    }
}

/// Time per section of a tick, kept without a lock and moved to the trace per replay.
#[derive(Clone, Default)]
pub(crate) struct TickTimes {
    pub(crate) laps: Laps,
    pub(crate) counts: crate::layout::StepCounts,
}

#[derive(Clone, Default)]
pub(crate) struct Laps {
    last: Option<Instant>,
    totals: [Option<Duration>; TICK_PHASES.len()],
}

impl Laps {
    pub(crate) fn begin(&mut self, trace: &Trace) {
        self.last = trace.on().then(Instant::now);
    }

    /// Charges the time since the previous section ended to `phase`.
    pub(crate) fn end(&mut self, phase: Phase) {
        if let Some(last) = &mut self.last {
            let now = Instant::now();
            let index = TICK_PHASES
                .iter()
                .position(|&tick| tick == phase)
                .expect("a tick phase");
            *self.totals[index].get_or_insert_default() += now - *last;
            *last = now;
        }
    }
}

/// An open phase; dropping it adds its elapsed time and one call to the phase.
#[must_use = "a span times the scope that holds it"]
pub(crate) struct Span(Option<(Arc<Mutex<Totals>>, Instant)>);

impl Drop for Span {
    fn drop(&mut self) {
        if let Some((totals, started)) = self.0.take() {
            let elapsed = started.elapsed();
            let mut totals = lock(&totals);
            let path = totals.open.clone();
            totals.open.pop();
            let (time, calls) = totals.phases.entry(path).or_default();
            *time += elapsed;
            *calls += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_spans_report_under_their_parent_then_reset() {
        let trace = Trace::enabled();
        {
            let _travel = trace.phase(Phase::PlanTravel);
            let _capture = trace.phase(Phase::Capture);
            trace.count(Counter::TicksReplayed, 7);
        }
        drop(trace.phase(Phase::Capture));
        trace.report(Scope::Window(0), 3, 2, Duration::from_secs(1));
        trace.report(Scope::Window(1), 0, 0, Duration::ZERO);
        let reports = trace.reports();
        assert_eq!(
            reports[0].counts(),
            "planning_trace window=0 commits=3 passes=2 plan_travel_calls=1 \
             plan_travel.capture_calls=1 capture_calls=1 \
             plan_travel.capture.ticks_replayed=7"
        );
        assert!(reports[0].nests());
        assert_eq!(
            reports[1].counts(),
            "planning_trace window=1 commits=0 passes=0"
        );
    }

    #[test]
    fn tick_sections_join_the_open_phase_once_per_replay() {
        let trace = Trace::enabled();
        let mut times = TickTimes::default();
        {
            let _capture = trace.phase(Phase::Capture);
            for _ in 0..2 {
                times.laps.begin(&trace);
                times.laps.end(Phase::TickPacking);
                times.laps.end(Phase::TickSlots);
            }
            trace.add_ticks(&mut times);
        }
        trace.add_ticks(&mut times);
        trace.report(Scope::Window(0), 1, 1, Duration::from_secs(1));
        assert_eq!(
            trace.reports()[0].counts(),
            "planning_trace window=0 commits=1 passes=1 capture_calls=1 \
             capture.tick_packing_calls=1 capture.tick_slots_calls=1"
        );
    }

    #[test]
    fn a_disabled_trace_records_nothing() {
        let trace = Trace::default();
        drop(trace.phase(Phase::Build));
        trace.count(Counter::Replays, 1);
        trace.report(Scope::Global, 1, 1, Duration::from_secs(1));
        assert!(!trace.on());
        assert!(trace.reports().is_empty());
    }

    #[test]
    fn a_line_gives_each_level_its_remainder() {
        let seconds = Duration::from_secs;
        let report = Report {
            scope: Scope::Global,
            commits: 5,
            passes: 1,
            total: seconds(10),
            phases: BTreeMap::from([
                (vec![Phase::Build], (seconds(2), 1)),
                (vec![Phase::PlanTravel], (seconds(6), 2)),
                (vec![Phase::PlanTravel, Phase::Capture], (seconds(4), 2)),
            ]),
            counters: BTreeMap::from([
                ((Vec::new(), Counter::Events), 5),
                (
                    (vec![Phase::PlanTravel, Phase::Capture], Counter::Replays),
                    2,
                ),
            ]),
        };
        assert_eq!(
            report.to_string(),
            "planning_trace window=global commits=5 passes=1 total_s=10.000000 \
             build_s=2.000000 build_calls=1 plan_travel_s=6.000000 plan_travel_calls=2 \
             plan_travel.rest_s=2.000000 plan_travel.capture_s=4.000000 \
             plan_travel.capture_calls=2 events=5 plan_travel.capture.replays=2 \
             unattributed_s=2.000000"
        );
        assert!(report.nests());
    }
}
