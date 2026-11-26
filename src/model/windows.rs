use super::{
    Options, Schedule, Simulation, State, Time, TravelCap, caption, schedule,
    trace::{Counter, Phase, Scope},
    travel,
};

pub(super) use super::streaming::{Events, Stream};
pub(super) const MARGIN: f32 = 8.0;

#[derive(Clone)]
pub(super) struct Planning {
    pub state: std::sync::Arc<State>,
    pub end: Time,
    pub services: Vec<travel::Service>,
    pub captured: Vec<travel::Service>,
}

impl Simulation {
    #[cfg(test)]
    pub(super) fn assert_stitched_travel_rules(&mut self) {
        self.window = None;
        self.restart();
        let mut diagnostic = super::slots::diagnostics::MovingBeams::default();
        for tick in 0..=super::frames_for(self.duration(), 60).unwrap() {
            self.advance_to(tick, 60);
            diagnostic.observe(self);
        }
        diagnostic.assert_clear(self.state.avatar_max_speed);
    }

    fn update_window_clock(&mut self, history: &Schedule, departures: &[Time]) {
        self.clock = schedule::Clock::new(history.clone(), self.schedule.clone());
        self.departures = departures.iter().map(|&t| self.clock.present(t)).collect();
        self.births = self.births_for(&self.schedule);
        self.replan_display_aspects();
    }

    fn finish_window(&mut self, end: Time, complete: bool) -> anyhow::Result<u64> {
        let begin = self.window.as_ref().expect("planning window").state.tick;
        {
            let _phase = self.trace.phase(Phase::Caption);
            caption::extend(
                &self.schedule,
                &self.events,
                begin + u64::from(begin > 0),
                &mut self.caption_plan,
            );
        }
        let boundary = if complete {
            self.end_tick()
        } else {
            self.clock.present(end).ticks(true)
        };
        {
            let _phase = self.trace.phase(Phase::CheckpointReplay);
            self.trace.count_replay(boundary.saturating_sub(begin));
            self.restart();
            for frame in u32::try_from(begin)?..=u32::try_from(boundary)? {
                self.progress.report_range(
                    crate::progress::Stage::Checkpoint,
                    frame.into(),
                    begin,
                    boundary,
                );
                self.advance_to(frame, 60);
            }
            self.trace.add_ticks(&mut self.tick_times);
        }
        let clock = &self.clock;
        let births = &self.births;
        self.packing.checkpoint_display_aspects(boundary, |tick| {
            Self::packing_rows(clock, births, tick)
        });
        self.boundaries.push(self.state.0.clone());
        if self.trace.on() {
            eprintln!(
                "travel_window end={} commits={} boundary_tick={boundary}",
                end.lossy_secs_f32(),
                self.events.len()
            );
        }
        Ok(boundary)
    }

    pub(super) fn plan_windows(
        self,
        all_events: Events,
        options: &Options,
        repositories: &[String],
        seconds: f32,
    ) -> anyhow::Result<Self> {
        #[cfg(test)]
        if options.eager_windows {
            return self.plan_windows_with_padding(
                &all_events,
                options,
                repositories,
                seconds,
                Vec::new(),
            );
        }
        Stream::start(
            self,
            all_events,
            options.clone(),
            repositories.to_vec(),
            seconds,
        )
    }

    #[cfg(test)]
    fn plan_windows_with_padding(
        self,
        all_events: &[crate::timeline::Event],
        options: &Options,
        repositories: &[String],
        seconds: f32,
        padding: Vec<(usize, u32)>,
    ) -> anyhow::Result<Self> {
        self.run_windows(all_events, options, repositories, seconds, padding, None)
    }

    fn settle_window(
        &mut self,
        all_events: &[crate::timeline::Event],
        options: &Options,
        repositories: &[String],
        (history, departures): (&mut Schedule, &[Time]),
        events: &Events,
        padding: &mut Vec<(usize, u32)>,
    ) -> anyhow::Result<u32> {
        let trace = &options.trace;
        let mut pass = 0;
        let mut compactions = 0;
        let mut planned: Option<Vec<(usize, u32)>> = None;
        loop {
            pass += 1;
            if pass > 32 {
                anyhow::bail!(
                    "window travel did not converge at commit {}",
                    self.travel
                        .first_unresolved(options.bot_travel)
                        .unwrap_or(self.travel.frozen)
                        + self.offset
                );
            }
            options.progress.pass(pass);
            let complete_schedule = {
                let _phase = trace.phase(Phase::ScheduleRebuild);
                trace.count_rebuild(self.events.len(), padding.len());
                trace.count_rebuild(events.len(), padding.len());
                self.schedule = history.with_travel(self.events.len(), |i| {
                    schedule::padding_at(padding, self.offset + i)
                });
                history.with_travel(events.len(), |i| {
                    schedule::padding_at(padding, self.offset + i)
                })
            };
            {
                let _phase = trace.phase(Phase::CheckSchedule);
                let exit_tail = complete_schedule.present_tail_tick(
                    history,
                    super::lifecycle::exit_end(&self.repository_exits),
                );
                Self::check_schedule(
                    all_events,
                    options,
                    repositories,
                    &complete_schedule,
                    Some(exit_tail),
                )?;
            }
            {
                let _phase = trace.phase(Phase::WindowClock);
                self.update_window_clock(history, departures);
            }
            {
                let _phase = trace.phase(Phase::Restart);
                self.restart();
            }
            if !options.replay_everything && planned.as_ref() == Some(padding) {
                trace.count(Counter::EnvelopeReuses, 1);
            } else {
                self.plan_avatar_envelopes(None);
                planned = Some(padding.clone());
            }
            self.plan_travel(options, padding);
            if options.travel_cap == TravelCap::None
                && self.travel.protect_birth_walks(history)
            {
                planned = None;
                compactions = 0;
                continue;
            }
            let adjusted = self.adjust_travel_padding(options, padding);
            if adjusted != *padding
                && (options.travel_cap == TravelCap::None
                    || self.travel.unsettled_relocation()
                    || pass < 4)
            {
                *padding = adjusted;
                continue;
            }
            if compactions == 0 {
                compactions = 1;
                *padding = self.compact_travel_padding(padding);
                continue;
            }
            let refined = self.refine_service_slots(options, padding);
            if refined != *padding {
                *padding = refined;
                continue;
            }
            break;
        }
        if options.travel_cap == TravelCap::None {
            self.travel.require_birth_routes(options.bot_travel)?;
        }
        self.align_travel_arrivals(options);
        self.repair_walk_departures(options, padding);
        Ok(pass)
    }

    pub(super) fn run_windows(
        mut self,
        all_events: &[crate::timeline::Event],
        options: &Options,
        repositories: &[String],
        seconds: f32,
        mut padding: Vec<(usize, u32)>,
        mut publish: Option<&mut dyn FnMut(Self) -> anyhow::Result<bool>>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            seconds.is_finite() && seconds >= super::tick_seconds(),
            "travel window must be at least one tick of finite history time"
        );
        let events = self.events.clone();
        self.windowed = true;
        let mut history = self.schedule.clone();
        let departures = self.departures.clone();
        let duration = self.duration();
        let mut end = (Time::ZERO + seconds).min(duration);
        self.window = Some(Planning {
            state: self.initial.clone(),
            end: (end + MARGIN).min(duration),
            services: Vec::new(),
            captured: Vec::new(),
        });
        let mut window_index = 0;
        loop {
            let started = std::time::Instant::now();
            let limit = self.window.as_ref().expect("planning window").end;
            let count = history.starts.partition_point(|&start| start < limit);
            self.events = events.prefix(count);
            let passes = self.settle_window(
                all_events,
                options,
                repositories,
                (&mut history, &departures),
                &events,
                &mut padding,
            )?;
            #[cfg(test)]
            if let Some(hook) = &options.window_hook {
                hook(window_index, &mut self);
            }
            if options.travel_cap == TravelCap::None
                && let Some(commit) = self.travel.first_unresolved(options.bot_travel)
            {
                anyhow::bail!(
                    "window travel has no route for commit {}",
                    commit + self.offset
                );
            }
            let complete = end >= duration;
            let boundary = self.finish_window(end, complete)?;
            let elapsed = started.elapsed();
            if options.trace.on() {
                eprintln!(
                    "window_plan_seconds={} history_window_seconds={seconds}",
                    elapsed.as_secs_f64()
                );
            }
            self.count_sizes();
            options.trace.report(
                Scope::Window(window_index),
                self.events.len(),
                passes,
                elapsed,
            );
            window_index += 1;
            if let Some(publish) = &mut publish {
                let ready = super::streaming::Ready {
                    end: boundary,
                    complete,
                    unpadded: duration,
                };
                let frame = self.window_packet(events.clone(), ready);
                if !publish(frame)? {
                    return Ok(self);
                }
            }
            if complete {
                break;
            }
            self.travel
                .freeze_through(boundary + 120, self.schedule.applied(boundary + 120));
            let window = self.window.as_mut().expect("planning window");
            window.services = window
                .captured
                .iter()
                .filter(|s| s.commit < self.state.next_event)
                .cloned()
                .collect();
            window.state = self
                .boundaries
                .last()
                .cloned()
                .unwrap_or_else(|| self.state.0.clone());
            if publish.is_some() {
                self.boundaries.clear();
                self.discard_consumed(boundary);
            }
            end = (end + seconds).min(duration);
            self.window.as_mut().expect("window").end = (end + MARGIN).min(duration);
        }
        self.window = None;
        self.events = events;
        self.progress = crate::progress::Progress::default();
        self.restart();
        Ok(self)
    }
}

impl Simulation {
    fn count_sizes(&self) {
        if self.trace.on() {
            let layout = self.layout();
            self.trace.count(Counter::Files, layout.files().count());
            self.trace.count(Counter::Directories, layout.dir_count());
            self.trace.count(Counter::Avatars, self.state.avatars.len());
            self.trace.count(Counter::Trips, self.travel.trip_count());
        }
        self.count_tilings();
    }

    /// Moves the packing's tiling counts into the trace, under the open phase.
    pub(super) fn count_tilings(&self) {
        if self.trace.on() {
            let (tiled, reused) = self.packing.take_tiling_counts();
            self.trace.count(Counter::Tilings, tiled);
            self.trace.count(Counter::TilingReuses, reused);
        }
    }

    fn window_packet(&self, events: Events, ready: super::streaming::Ready) -> Self {
        let initial = self.window.as_ref().expect("planning window").state.clone();
        Self {
            file_ids: self.file_ids.clone(),
            first_arrivals: self.first_arrivals.clone(),
            finals: self.finals.clone(),
            departures: self.departures.clone(),
            repository_ends: self.repository_ends.clone(),
            repository_exits: self.repository_exits.clone(),
            caption_style: self.caption_style,
            caption_plan: self.caption_plan.clone(),
            packing: self.packing.for_playback(),
            envelopes: self.envelopes.clone(),
            repository_boundary: self.repository_boundary,
            collapse_directories: self.collapse_directories,
            diff_text_distance: self.diff_text_distance,
            events,
            offset: self.offset,
            unread_before: self.unread_before,
            total: self.total,
            schedule: self.schedule.clone(),
            clock: self.clock.clone(),
            births: self.births.clone(),
            playback_multiplier: self.playback_multiplier,
            heat_half_life: self.heat_half_life,
            state: super::SharedState(initial.clone()),
            initial,
            theme: self.theme.clone(),
            ray_view: None,
            travel: self.travel.clone(),
            visuals: self.visuals,
            progress: crate::progress::Progress::default(),
            trace: super::Trace::default(),
            tick_times: super::trace::TickTimes::default(),
            window: None,
            boundaries: Vec::new(),
            windowed: true,
            stream: None,
            ready_end: Some(ready),
            #[cfg(test)]
            advance_all_files: self.advance_all_files,
        }
    }

    fn discard_consumed(&mut self, boundary: u64) {
        let mut keep = std::collections::BTreeSet::new();
        keep.extend(self.state.avatars.iter().map(|avatar| avatar.commit));
        keep.extend(self.state.pulses.iter().map(|pulse| pulse.commit));
        self.travel
            .discard_before(boundary, self.state.next_event, &keep);
        for (commit, envelope) in self.envelopes.iter_mut().enumerate() {
            if commit < self.state.next_event && !keep.contains(&commit) {
                *envelope = super::slots::Envelope::default();
            }
        }
        caption::discard_before(&mut self.caption_plan, boundary);
        let window = self.window.as_mut().expect("planning window");
        let mut last = std::collections::BTreeMap::new();
        for service in window.services.drain(..) {
            last.insert(service.author, service);
        }
        window.services = last.into_values().collect();
        window.services.sort_by_key(|service| service.commit);
        window.captured.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::super::trace::Report;
    use super::*;

    #[test]
    fn each_window_reports_local_passes_and_checkpoint_progress() {
        use crate::progress::{Progress, Stage};
        let (progress, updates) = Progress::channel(std::time::Duration::ZERO);
        let (events, labels) = super::super::tests::short_repository_history(2);
        let mut sim = Simulation::with_repositories(
            events,
            &Options {
                eager_windows: true,
                travel_window_seconds: Some(0.5),
                progress,
                ..Options::default()
            },
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &labels,
        );
        let mut previous: Option<crate::progress::Update> = None;
        let mut checkpoints = 0;
        for update in updates.try_iter().filter(|update| {
            matches!(
                update.stage,
                Stage::Travel | Stage::Reservations | Stage::Checkpoint
            )
        }) {
            let beginning = previous.is_none_or(|old| {
                old.stage != update.stage
                    || old.pass != update.pass
                    || update.fraction < old.fraction
            });
            if beginning {
                assert_eq!(update.fraction, 0.0, "{update:?}");
                if let Some(old) = previous {
                    assert_eq!(old.fraction, 1.0, "{old:?}");
                }
                checkpoints += usize::from(update.stage == Stage::Checkpoint);
            }
            previous = Some(update);
        }
        assert_eq!(previous.unwrap().fraction, 1.0);
        assert_eq!(checkpoints, sim.boundaries.len());
        assert!(checkpoints > 1);
        verify_boundaries(&mut sim);
    }

    fn traced(window: Option<f32>) -> (Simulation, Vec<Report>) {
        plan_traced(
            super::super::tests::short_repository_history(2),
            Options {
                eager_windows: window.is_some(),
                travel_window_seconds: window,
                ..Options::default()
            },
        )
    }

    fn plan_traced(
        (events, labels): (Vec<crate::timeline::Event>, Vec<String>),
        options: Options,
    ) -> (Simulation, Vec<Report>) {
        let trace = super::super::Trace::enabled();
        let sim = Simulation::with_repositories(
            events,
            &Options {
                trace: trace.clone(),
                ..options
            },
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &labels,
        );
        (sim, trace.reports())
    }

    #[test]
    fn trace_counts_repeat_and_match_every_window() {
        let (sim, reports) = traced(Some(1.0));
        let counts = |reports: &[Report]| -> Vec<_> {
            reports.iter().map(Report::counts).collect()
        };
        assert_eq!(counts(&reports), counts(&traced(Some(1.0)).1));
        assert_eq!(reports[0].calls(&[Phase::Build]), 1);
        let build = [Phase::Build, Phase::PlanRepositories];
        assert_eq!(reports[0].calls(&build), 1);
        assert!(reports[0].counter(&build, Counter::Tilings) > 0);
        let windows = &reports[1..];
        assert_eq!(windows.len(), sim.boundaries.len());
        assert!(windows.len() > 1);
        let mut replayed = 0;
        for window in windows {
            assert!(window.nests(), "{window}");
            let passes = u64::from(window.passes());
            for phase in [
                Phase::ScheduleRebuild,
                Phase::CheckSchedule,
                Phase::WindowClock,
                Phase::Restart,
                Phase::PlanTravel,
                Phase::AdjustPadding,
            ] {
                assert_eq!(window.calls(&[phase]), passes, "{window}");
            }
            assert_eq!(
                window.calls(&[Phase::AvatarEnvelopes])
                    + window.counter(&[], Counter::EnvelopeReuses),
                passes
            );
            assert_eq!(
                window.counter(&[Phase::ScheduleRebuild], Counter::Rebuilds),
                2 * passes
            );
            assert_eq!(window.calls(&[Phase::PlanTravel, Phase::Capture]), passes);
            assert_eq!(
                window.calls(&[Phase::CheckpointReplay, Phase::TickNames]),
                1
            );
            assert!(window.counter(&[], Counter::Files) > 0);
            let capture = [Phase::PlanTravel, Phase::Capture];
            assert_eq!(
                window.calls(&[Phase::PlanTravel, Phase::Capture, Phase::TickRelax]),
                passes
            );
            assert_eq!(
                window.counter(&capture, Counter::DirectoryTicks),
                window.counter(&[], Counter::Directories)
                    * window.counter(&capture, Counter::TicksReplayed)
            );
            assert!(
                window.counter(&capture, Counter::StillDirectoryTicks)
                    <= window.counter(&capture, Counter::DirectoryTicks)
            );
            assert_eq!(window.counter(&capture, Counter::FileTicks), 0);
            let checkpoint = [Phase::CheckpointReplay];
            assert!(
                window.counter(&checkpoint, Counter::SteadyFileTicks)
                    <= window.counter(&checkpoint, Counter::FileTicks)
            );
            replayed +=
                window.counter(&[Phase::CheckpointReplay], Counter::TicksReplayed);
        }
        assert_eq!(replayed, sim.boundaries.last().unwrap().tick);
    }

    /// Commits a few seconds apart from more authors than repositories, two per repository in turn.
    fn crowded_history(
        repositories: usize,
        commits: usize,
        authors: usize,
        gap: i64,
    ) -> (Vec<crate::timeline::Event>, Vec<String>) {
        let events = (0..commits)
            .map(|index| {
                let mut row = vec![crate::timeline::synthetic().remove(index % 3)];
                row[0].seconds = i64::try_from(index).unwrap() * gap;
                row[0].author.name = format!("person{}", index % authors).into_bytes();
                row[0].author.email =
                    format!("p{}@test.invalid", index % authors).into_bytes();
                crate::repositories::namespace(&mut row, (index / 2) % repositories);
                row.remove(0)
            })
            .collect();
        let labels = (0..repositories).map(|i| format!("repo{i}")).collect();
        (events, labels)
    }

    fn planned(
        history: (Vec<crate::timeline::Event>, Vec<String>),
        window: f32,
        travel_cap: TravelCap,
        replay_everything: bool,
    ) -> (Simulation, Vec<Report>) {
        plan_traced(
            history,
            Options {
                eager_windows: true,
                travel_window_seconds: Some(window),
                travel_cap,
                replay_everything,
                ..Options::default()
            },
        )
    }

    /// One history planned with replay reuse and again with every replay run.
    fn reused_and_replayed(
        history: &(Vec<crate::timeline::Event>, Vec<String>),
        window: f32,
        travel_cap: TravelCap,
    ) -> [(Simulation, Vec<Report>); 2] {
        [false, true].map(|replay_everything| {
            planned(history.clone(), window, travel_cap, replay_everything)
        })
    }

    fn total(reports: &[Report], path: &[Phase], counter: Counter) -> u64 {
        reports.iter().map(|r| r.counter(path, counter)).sum()
    }

    fn assert_same_playback(a: &mut Simulation, b: &mut Simulation) {
        assert_eq!(a.travel.round_state(None), b.travel.round_state(None));
        assert_eq!(a.travel.report(), b.travel.report());
        assert_eq!(a.boundaries.len(), b.boundaries.len());
        for (left, right) in a.boundaries.clone().into_iter().zip(b.boundaries.clone())
        {
            a.state = super::super::SharedState(left);
            b.state = super::super::SharedState(right);
            assert_eq!(
                super::super::streaming::tests::visible(a),
                super::super::streaming::tests::visible(b),
                "boundary at tick {}",
                a.state.tick
            );
        }
        a.restart();
        b.restart();
        for frame in 0..=super::super::frames_for(a.duration(), 60).unwrap() {
            a.advance_to(frame, 60);
            b.advance_to(frame, 60);
            assert_eq!(
                super::super::streaming::tests::visible(a),
                super::super::streaming::tests::visible(b),
                "tick {frame}"
            );
            assert_eq!(
                a.layout().exact_state(),
                b.layout().exact_state(),
                "layout at tick {frame}"
            );
        }
    }

    #[test]
    fn holding_files_outside_drawn_frames_changes_no_plan_or_playback() {
        let history = crowded_history(4, 12, 5, 20_000);
        let empty_envelope = super::super::slots::Envelope::default();
        for travel_window_seconds in [None, Some(5.0)] {
            let [mut held, mut advanced] = [false, true].map(|advance_all_files| {
                plan_traced(
                    history.clone(),
                    Options {
                        eager_windows: true,
                        travel_window_seconds,
                        travel_cap: TravelCap::None,
                        advance_all_files,
                        ..Options::default()
                    },
                )
                .0
            });
            assert!(
                held.envelopes
                    .iter()
                    .any(|envelope| *envelope != empty_envelope)
            );
            assert_eq!(held.envelopes, advanced.envelopes);
            assert_eq!(held.packing.exact_state(), advanced.packing.exact_state());
            let layouts = |sim: &Simulation| -> Vec<_> {
                sim.boundaries
                    .iter()
                    .map(|s| s.layout.exact_state())
                    .collect()
            };
            assert_eq!(layouts(&held), layouts(&advanced));
            assert_same_playback(&mut held, &mut advanced);
        }
    }

    #[test]
    #[ignore = "prints digests for comparing playback between two commits"]
    fn playback_digests() {
        use std::hash::Hasher as _;
        let plans = vec![
            traced(Some(1.0)).0,
            traced(None).0,
            planned(
                crowded_history(4, 12, 5, 20_000),
                5.0,
                TravelCap::None,
                false,
            )
            .0,
        ];
        for (index, mut sim) in plans.into_iter().enumerate() {
            let mut hash = std::hash::DefaultHasher::new();
            hash.write(sim.travel.round_state(None).as_bytes());
            for frame in 0..=super::super::frames_for(sim.duration(), 60).unwrap() {
                sim.advance_to(frame, 60);
                hash.write(sim.layout().exact_state().as_bytes());
                hash.write(super::super::streaming::tests::visible(&sim).as_bytes());
            }
            eprintln!("PLAYBACK_DIGEST {index} {:016x}", hash.finish());
        }
    }

    #[test]
    fn an_unchanged_padding_pass_reuses_envelopes_without_changing_playback() {
        let (mut events, labels) = super::super::tests::short_repository_history(6);
        events.truncate(6);
        let [(mut reused, fast), (mut replayed, slow)] =
            reused_and_replayed(&(events, labels), 1.0, TravelCap::Seconds(0.4));
        let replays = |reports: &[Report]| {
            total(reports, &[Phase::AvatarEnvelopes], Counter::Replays)
        };
        let skipped = total(&fast, &[], Counter::EnvelopeReuses);
        assert!(skipped > 0);
        assert!(total(&fast, &[], Counter::TilingReuses) > 0);
        assert_eq!(total(&slow, &[], Counter::TilingReuses), 0);
        assert_eq!(replays(&fast) + skipped, replays(&slow));
        assert_same_playback(&mut reused, &mut replayed);
    }

    #[test]
    fn passing_avatars_do_not_trigger_slot_replanning() {
        let (mut events, labels) = super::super::tests::short_repository_history(6);
        events.truncate(12);
        let [(mut reused, fast), (mut replayed, slow)] =
            [false, true].map(|replay_everything| {
                plan_traced(
                    (events.clone(), labels.clone()),
                    Options {
                        start: labels.len(),
                        eager_windows: true,
                        travel_window_seconds: Some(3.0),
                        travel_cap: TravelCap::None,
                        replay_everything,
                        ..Options::default()
                    },
                )
            });
        let rounds = |reports: &[Report]| {
            total(
                reports,
                &[Phase::RefineServiceSlots, Phase::Reserve],
                Counter::Replays,
            )
        };
        assert!(rounds(&fast) <= rounds(&slow));
        for reports in [&fast, &slow] {
            assert!(reports.iter().any(|report| {
                report.calls(&[Phase::RefineServiceSlots, Phase::Reserve]) > 0
            }));
            assert!(reports.iter().all(|report| {
                report.calls(&[Phase::RefineServiceSlots, Phase::PlanTravel]) == 0
            }));
        }
        assert_same_playback(&mut reused, &mut replayed);
    }

    #[test]
    fn envelopes_are_replanned_when_padding_changes_between_passes() {
        let [(mut reused, fast), (mut replayed, slow)] = reused_and_replayed(
            &crowded_history(4, 12, 5, 20_000),
            5.0,
            TravelCap::None,
        );
        let replays = [Phase::AvatarEnvelopes];
        assert_eq!(
            total(&fast, &replays, Counter::Replays)
                + total(&fast, &[], Counter::EnvelopeReuses),
            total(&slow, &replays, Counter::Replays)
        );
        assert_same_playback(&mut reused, &mut replayed);
    }

    #[test]
    fn refinement_whose_rounds_keep_changing_runs_every_round() {
        let [(mut reused, fast), (mut replayed, slow)] = reused_and_replayed(
            &crowded_history(2, 24, 9, 3_000),
            20.0,
            TravelCap::None,
        );
        let settled = [Phase::RefineServiceSlots];
        assert_eq!(total(&fast, &settled, Counter::SettledRefinements), 0);
        let rounds = [Phase::RefineServiceSlots, Phase::Reserve];
        assert_eq!(
            total(&fast, &rounds, Counter::Replays),
            total(&slow, &rounds, Counter::Replays)
        );
        assert_same_playback(&mut reused, &mut replayed);
    }

    #[test]
    fn thread_count_changes_neither_the_packing_nor_the_trace_counts() {
        let built = |region_threads: usize| {
            let (mut events, labels) = super::super::tests::short_repository_history(6);
            let mappings = crate::contributors::Mappings::default();
            mappings.apply(&mut events);
            let trace = super::super::Trace::enabled();
            let options = Options {
                region_threads,
                trace: trace.clone(),
                ..Options::default()
            };
            let theme = crate::theme::Theme::neon();
            let sim = Simulation::build(
                events,
                &options,
                &theme,
                &mappings,
                &[],
                &labels,
                &mut None,
            )
            .unwrap();
            sim.report_global(0, std::time::Duration::ZERO);
            (sim.packing.exact_state(), trace.reports()[0].counts())
        };
        let single = built(1);
        assert!(
            single
                .1
                .contains("build.plan_repositories.regions_calls=2 "),
            "{}",
            single.1
        );
        for threads in [2, 0, 8, 8, 8, 8, 8, 8, 8, 8] {
            assert!(built(threads) == single, "{threads} threads");
        }
    }

    #[test]
    fn whole_timeline_trace_times_every_pass() {
        let (_, reports) = traced(None);
        let [report] = &reports[..] else {
            panic!("one global report, found {}", reports.len());
        };
        assert!(report.nests(), "{report}");
        let passes = u64::from(report.passes());
        assert!(passes > 0);
        assert_eq!(report.calls(&[Phase::Build]), 1);
        assert_eq!(report.calls(&[Phase::TravelPadding]), 1);
        assert_eq!(report.calls(&[Phase::Pass]), passes);
        assert_eq!(report.calls(&[Phase::Pass, Phase::PlanTravel]), passes);
        assert!(report.counter(&[], Counter::Tilings) > 0);
    }

    #[test]
    fn parked_author_and_occupied_dock_cross_uncapped_boundaries() {
        let (mut events, labels) = super::super::tests::short_repository_history(2);
        events.truncate(3);
        let second_repository = events[1].changes.clone();
        events[1].repository = 0;
        events[1].changes = events[0].changes.clone();
        events[2].author = events[0].author.clone();
        events[2].repository = 1;
        events[2].changes = second_repository;
        let options = Options {
            eager_windows: true,
            travel_window_seconds: Some(2.0),
            seconds_per_day: 12.0,
            max_idle_seconds: 12.0,
            travel_cap: TravelCap::None,
            bot_travel: super::super::BotTravel::Walk,
            ..Options::default()
        };
        let mut sim = Simulation::with_repositories(
            events.clone(),
            &options,
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &labels,
        );
        assert!(sim.travel.first_unresolved(options.bot_travel).is_none());
        assert!(sim.boundaries.iter().any(|s| s.avatars.len() >= 2
            && super::super::tick_time(s.tick) - s.avatars[0].last_active > MARGIN
            && s.avatars[0].presence > 0.0));
        verify_boundaries(&mut sim);
        verify_streamed(&mut sim, events, &options, &labels);
    }

    #[test]
    fn a_fading_bot_does_not_stop_uncapped_window_planning() {
        let (mut events, labels) = super::super::tests::short_repository_history(2);
        events.truncate(3);
        events[0].author.name = b"helper[bot]".to_vec();
        events[1].author = events[0].author.clone();
        let options = Options {
            eager_windows: true,
            travel_window_seconds: Some(2.0),
            travel_cap: TravelCap::None,
            ..Options::default()
        };
        let sim = Simulation::try_with_repositories(
            events,
            &options,
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &labels,
        )
        .unwrap();
        assert_eq!(sim.travel.stats.bot_fades, 1);
    }

    #[test]
    fn repository_birth_and_live_beam_cross_boundaries() {
        let (events, labels) = super::super::tests::short_repository_history(2);
        let options = Options {
            eager_windows: true,
            travel_window_seconds: Some(0.5),
            ..Options::default()
        };
        let mut sim = Simulation::with_repositories(
            events,
            &options,
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &labels,
        );
        assert_eq!(sim.boundaries[0].tick, 30);
        assert!(!sim.boundaries[0].pulses.is_empty());
        assert!(sim.boundaries[0].layout.repositories[0].live);
        verify_boundaries(&mut sim);
    }

    #[test]
    fn a_trip_in_flight_crosses_a_boundary() {
        let (events, labels) = super::super::tests::short_repository_history(2);
        let options = Options {
            eager_windows: true,
            travel_cap: TravelCap::None,
            travel_window_seconds: Some(0.2),
            ..Options::default()
        };
        let mut sim = Simulation::with_repositories(
            events,
            &options,
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &labels,
        );
        assert!(
            sim.boundaries
                .iter()
                .any(|s| (0..s.avatars.len()).any(|a| sim.travel.walking(a, s.tick)))
        );
        verify_boundaries(&mut sim);
    }

    fn verify_boundaries(sim: &mut Simulation) {
        let saved = sim.boundaries.clone();
        sim.restart();
        let mut diagnostic = super::super::slots::diagnostics::MovingBeams::default();
        for tick in 0..=super::super::frames_for(sim.duration(), 60).unwrap() {
            sim.advance_to(tick, 60);
            diagnostic.observe(sim);
            if let Some(state) = saved.iter().find(|s| s.tick == u64::from(tick)) {
                assert_eq!(format!("{:?}", sim.state), format!("{state:?}"));
            }
        }
        diagnostic.assert_clear(480.0);
    }

    fn verify_streamed(
        eager: &mut Simulation,
        events: Vec<crate::timeline::Event>,
        options: &Options,
        labels: &[String],
    ) {
        let mut streamed = Simulation::with_repositories(
            events,
            &Options {
                eager_windows: false,
                ..options.clone()
            },
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            labels,
        );
        eager.restart();
        for frame in 0..=super::super::frames_for(eager.duration(), 60).unwrap() {
            streamed.prepare_frame(frame, 60, true).unwrap();
            eager.advance_to(frame, 60);
            streamed.advance_to(frame, 60);
            assert_eq!(
                super::super::streaming::tests::visible(eager),
                super::super::streaming::tests::visible(&streamed),
                "streamed frame {frame}"
            );
            assert!(streamed.boundaries.is_empty());
        }
        assert!(streamed.planning_complete());
    }

    #[test]
    fn later_route_and_wait_leave_every_earlier_window_frozen() {
        let (mut events, labels) = super::super::tests::short_repository_history(5);
        for (index, event) in events.iter_mut().enumerate() {
            event.seconds = i64::try_from(index).unwrap() * 10 * 86_400;
        }
        events[12].author.email = b"late@example.invalid".to_vec();
        let options = Options {
            max_idle_seconds: 5.0,
            ..Options::default()
        };
        let mappings = crate::contributors::Mappings::default();
        mappings.apply(&mut events);
        let theme = crate::theme::Theme::neon();
        let mut states = Vec::new();
        for changed in 0..3 {
            let mut sim = Simulation::build(
                events.clone(),
                &options,
                &theme,
                &mappings,
                &[],
                &labels,
                &mut None,
            )
            .unwrap();
            if changed == 1 {
                sim.events[10].contributor = sim.events[11].contributor.clone();
            }
            let padding = if changed == 2 {
                vec![(12, 120)]
            } else {
                Vec::new()
            };
            let sim = sim
                .plan_windows_with_padding(&events, &options, &labels, 5.0, padding)
                .unwrap();
            assert_eq!(sim.schedule.padding(12), if changed == 2 { 120 } else { 0 });
            let mut frozen = Vec::new();
            for boundary in sim.boundaries.iter().take(8) {
                let count = sim.schedule.applied(boundary.tick);
                let trips = sim.travel.report()["trips"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|t| {
                        t["departure"].as_f64().unwrap()
                            <= f64::from(
                                super::super::tick_time(boundary.tick).lossy_secs_f32(),
                            )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                frozen.push(format!(
                    "{:?}{:?}{:?}{:?}{:?}{}",
                    boundary.avatars,
                    boundary.pulses,
                    &sim.schedule.starts[..count],
                    sim.travel.docks.range(..(count, 0)).collect::<Vec<_>>(),
                    trips,
                    caption::frozen(&sim.caption_plan, boundary.tick)
                ));
            }
            assert_eq!(frozen.len(), 8);
            states.push(frozen);
        }
        assert_eq!(states[0], states[1]);
        assert_eq!(states[0], states[2]);
        eprintln!(
            "frozen_windows=8 mutations=2 bytes={}",
            states[0].iter().map(String::len).sum::<usize>()
        );
    }

    #[test]
    fn boundary_states_replay_the_stitched_trajectory() {
        let options = Options {
            eager_windows: true,
            travel_window_seconds: Some(1.0),
            ..Options::default()
        };
        let mut simulation = Simulation::new(
            crate::timeline::synthetic(),
            &options,
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        assert!(simulation.boundaries.len() >= 4);
        for saved in simulation.boundaries.clone() {
            simulation.restart();
            simulation.advance_to(u32::try_from(saved.tick).unwrap(), 60);
            assert_eq!(format!("{:?}", simulation.state), format!("{saved:?}"));
            let next = u32::try_from(saved.tick + 1).unwrap();
            simulation.advance_to(next, 60);
            let expected = format!("{:?}", simulation.state);
            simulation.state = super::super::SharedState(saved.clone());
            simulation.advance_to(next, 60);
            assert_eq!(format!("{:?}", simulation.state), expected);
        }
    }

    #[test]
    fn multiple_repositories_restore_boundary_state() {
        let mut histories = Vec::new();
        for index in 0..2 {
            let mut events = crate::timeline::synthetic();
            for event in &mut events {
                event.seconds += i64::try_from(index).unwrap() * 86_400;
            }
            crate::repositories::namespace(&mut events, index);
            histories.push(events);
        }
        let options = Options {
            eager_windows: true,
            travel_window_seconds: Some(1.0),
            ..Options::default()
        };
        let mut sim = Simulation::with_repositories(
            crate::repositories::merge(histories),
            &options,
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &["a".into(), "b".into()],
        );
        for saved in sim.boundaries.clone() {
            sim.restart();
            sim.advance_to(u32::try_from(saved.tick).unwrap(), 60);
            assert_eq!(format!("{:?}", sim.state), format!("{saved:?}"));
        }
    }
}
