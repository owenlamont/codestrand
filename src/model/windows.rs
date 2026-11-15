use super::{
    Options, Schedule, Simulation, State, Time, TravelCap, caption, schedule, travel,
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

    fn update_window_clock(
        &mut self,
        history: &Schedule,
        departures: &[Time],
        endings: &[Time],
        options: &Options,
    ) {
        self.clock = schedule::Clock::new(history.clone(), self.schedule.clone());
        self.departures = departures.iter().map(|&t| self.clock.present(t)).collect();
        self.repository_ends = endings.iter().map(|&t| self.clock.present(t)).collect();
        let clock = self.clock.clone();
        let schedule = self.schedule.clone();
        self.plan_waiting_births(options, &clock, &schedule);
    }

    fn finish_window(&mut self, end: Time) -> anyhow::Result<u64> {
        let begin = self.window.as_ref().expect("planning window").state.tick;
        caption::extend(
            &self.schedule,
            &self.events,
            begin + u64::from(begin > 0),
            &mut self.caption_plan,
        );
        self.plan_titles();
        let boundary = self.clock.present(end).ticks(true);
        self.restart();
        self.advance_to(u32::try_from(boundary)?, 60);
        self.boundaries.push(self.state.0.clone());
        eprintln!(
            "travel_window end={} commits={} boundary_tick={boundary}",
            end.lossy_secs_f32(),
            self.events.len()
        );
        Ok(boundary)
    }

    pub(super) fn plan_windows(
        self,
        all_events: &[crate::timeline::Event],
        options: &Options,
        repositories: &[String],
        seconds: f32,
    ) -> anyhow::Result<Self> {
        #[cfg(test)]
        if options.eager_windows {
            return self.plan_windows_with_padding(
                all_events,
                options,
                repositories,
                seconds,
                Vec::new(),
            );
        }
        Stream::start(
            self,
            all_events.to_vec(),
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
        (history, departures, endings): (&Schedule, &[Time], &[Time]),
        events: &Events,
        padding: &mut Vec<(usize, u32)>,
    ) -> anyhow::Result<()> {
        let mut pass = 0;
        let mut compactions = 0;
        loop {
            pass += 1;
            if pass > 32 {
                anyhow::bail!(
                    "window travel did not converge at commit {}",
                    self.travel.first_unresolved().unwrap_or(self.travel.frozen)
                        + self.offset
                );
            }
            options.progress.pass(pass);
            self.schedule = Schedule::with_padding(
                &self.events,
                options.seconds_per_day,
                options.max_idle_seconds,
                |i| schedule::padding_at(padding, self.offset + i),
            );
            let complete_schedule = Schedule::with_padding(
                events,
                options.seconds_per_day,
                options.max_idle_seconds,
                |i| schedule::padding_at(padding, self.offset + i),
            );
            Self::check_schedule(
                all_events,
                options,
                repositories,
                &complete_schedule,
            )?;
            self.update_window_clock(history, departures, endings, options);
            self.restart();
            self.plan_avatar_envelopes(None);
            self.plan_travel(options, padding);
            let adjusted = self.adjust_travel_padding(options, padding);
            if adjusted != *padding
                && (options.travel_cap == TravelCap::None || pass < 4)
            {
                *padding = adjusted;
                continue;
            }
            if compactions == 0 {
                compactions = 1;
                *padding = self.compact_travel_padding(options, padding);
                continue;
            }
            let refined = self.refine_service_slots(options, padding);
            if refined != *padding {
                *padding = refined;
                continue;
            }
            break;
        }
        Ok(())
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
        let history = self.schedule.clone();
        let departures = self.departures.clone();
        let endings = self.repository_ends.clone();
        let duration = self.duration();
        let mut end = (Time::ZERO + seconds).min(duration);
        self.window = Some(Planning {
            state: self.initial.clone(),
            end: (end + MARGIN).min(duration),
            services: Vec::new(),
            captured: Vec::new(),
        });
        #[cfg(test)]
        let mut window_index = 0;
        loop {
            let started = std::time::Instant::now();
            let limit = self.window.as_ref().expect("planning window").end;
            let count = history.starts.partition_point(|&start| start < limit);
            self.events = events.prefix(count);
            self.settle_window(
                all_events,
                options,
                repositories,
                (&history, &departures, &endings),
                &events,
                &mut padding,
            )?;
            #[cfg(test)]
            {
                if let Some(hook) = &options.window_hook {
                    hook(window_index, &mut self);
                }
                window_index += 1;
            }
            if options.travel_cap == TravelCap::None {
                anyhow::ensure!(
                    self.travel.first_unresolved().is_none(),
                    "window travel has no route for commit {}",
                    self.travel.first_unresolved().unwrap_or(0) + self.offset
                );
            }
            let boundary = self.finish_window(end)?;
            eprintln!(
                "window_plan_seconds={} history_window_seconds={seconds}",
                started.elapsed().as_secs_f64()
            );
            if let Some(publish) = &mut publish {
                let frame =
                    self.window_packet(events.clone(), boundary, end >= duration);
                if !publish(frame)? {
                    return Ok(self);
                }
            }
            if end >= duration {
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
    fn window_packet(&self, events: Events, boundary: u64, complete: bool) -> Self {
        let initial = self.window.as_ref().expect("planning window").state.clone();
        Self {
            file_ids: self.file_ids.clone(),
            first_arrivals: self.first_arrivals.clone(),
            departures: self.departures.clone(),
            repository_ends: self.repository_ends.clone(),
            caption_style: self.caption_style,
            caption_plan: self.caption_plan.clone(),
            packing: self.packing.clone(),
            envelopes: self.envelopes.clone(),
            repository_boundary: self.repository_boundary,
            collapse_directories: self.collapse_directories,
            diff_text_distance: self.diff_text_distance,
            repository_label_plane: self.repository_label_plane,
            events,
            offset: self.offset,
            total: self.total,
            schedule: self.schedule.clone(),
            clock: self.clock.clone(),
            births: self.births.clone(),
            playback_multiplier: self.playback_multiplier,
            heat_half_life: self.heat_half_life,
            state: super::SharedState(initial.clone()),
            initial,
            theme: self.theme.clone(),
            travel: self.travel.clone(),
            visual_updates: self.visual_updates,
            title_plan: self.title_plan.clone(),
            progress: crate::progress::Progress::default(),
            window: None,
            boundaries: Vec::new(),
            windowed: true,
            stream: None,
            ready_end: Some((boundary, complete)),
            title_start: self.title_start,
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
        let first = usize::try_from(boundary / 15)
            .expect("bounded timeline")
            .saturating_sub(2);
        for row in &mut self.title_plan {
            let count = first.saturating_sub(self.title_start).min(row.len());
            row.drain(..count);
        }
        self.title_start = first;
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
    use super::*;

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
            events,
            &options,
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &labels,
        );
        assert!(sim.travel.first_unresolved().is_none());
        assert!(sim.boundaries.iter().any(|s| s.avatars.len() >= 2
            && super::super::tick_time(s.tick) - s.avatars[0].last_active > MARGIN
            && s.avatars[0].presence > 0.0));
        verify_boundaries(&mut sim);
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
            travel_window_seconds: Some(0.25),
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
                let titles = sim
                    .title_plan
                    .iter()
                    .map(|row| {
                        row[..usize::try_from(boundary.tick / 15 + 1).unwrap()].to_vec()
                    })
                    .collect::<Vec<_>>();
                frozen.push(format!(
                    "{:?}{:?}{:?}{:?}{:?}{:?}{:?}{}",
                    boundary.avatars,
                    boundary.titles,
                    boundary.pulses,
                    &sim.schedule.starts[..count],
                    &sim.travel.docks[..count],
                    trips,
                    titles,
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
    fn thirty_repositories_stitch_with_capped_travel() {
        let (events, labels) = super::super::tests::short_repository_history(30);
        let options = Options {
            eager_windows: true,
            travel_window_seconds: Some(5.0),
            ..Options::default()
        };
        let mut sim = Simulation::with_repositories(
            events,
            &options,
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &labels,
        );
        assert!(sim.boundaries.len() >= 4);
        let mut diagnostic = super::super::slots::diagnostics::MovingBeams::default();
        for tick in 0..=super::super::frames_for(sim.duration(), 60).unwrap() {
            sim.advance_to(tick, 60);
            diagnostic.observe(&sim);
        }
        diagnostic.assert_clear(options.avatar_max_speed);
        for saved in sim.boundaries.clone() {
            sim.restart();
            sim.advance_to(u32::try_from(saved.tick).unwrap(), 60);
            assert_eq!(format!("{:?}", sim.state), format!("{saved:?}"));
        }
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
