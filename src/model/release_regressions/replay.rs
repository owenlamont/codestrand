use super::{directory_edges::DirectoryAudit, *};
use crate::layout::packing::Tile;
use bevy::math::{Rect, Vec3Swizzles};
use proptest::{prelude::*, test_runner::TestCaseResult};
use std::time::Instant;

#[derive(Clone, Copy)]
pub(super) enum Fault {
    None,
    ForeignRayAtSeam,
}

#[derive(Debug, Default)]
pub(super) struct Replay {
    pub ticks: u32,
    pub directory_intervals: usize,
    pub resizing_directory_intervals: usize,
    pub reparented_files: usize,
    pub windows: u32,
    pub arrivals_at_seam: u32,
    pub moving_docks: u32,
    pub cross_repo_services: u32,
    pub ring_frames: u32,
    pub ring_seams: u32,
    pub passing_contacts: u64,
    pub longest_parked_contact: u32,
    pub peak_live_items: usize,
    pub peak_live_files: usize,
    pub peak_live_directories: usize,
    pub peak_visible_contributors: usize,
    pub peak_contributors_per_repository: usize,
    pub max_tile_step_px: f32,
    pub max_tile_step_at: Option<(usize, u64, u64)>,
    pub max_carried_step: f32,
    pub final_gap: f32,
    pub settled_glass_error_px: f32,
    pub last_service_tile_tick: u64,
    pub last_archive_tick: u64,
    pub flat_lifecycles: (usize, usize),
    pub overlapping_exits: bool,
    pub planner_wait: Duration,
    pub worst_glyph_ratio: f32,
    pub worst_glyph_tick: u64,
    pub comparable_glyph_frames: usize,
    pub glyph_ratio_p90: f32,
    pub out_of_band_glyph_frames: usize,
    pub glyph_settle_ticks: Vec<u64>,
}

impl Replay {
    pub(super) fn check_glyph_ratio(&self) -> TestCaseResult {
        prop_assert!(
            self.worst_glyph_ratio <= 2.0 + 8.0 * f32::EPSILON,
            "nominal glyph width ratio {:.4} at frame {} exceeds 2.0",
            self.worst_glyph_ratio,
            self.worst_glyph_tick
        );
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ContributorFrame {
    repository: usize,
    position: Vec3,
    presence: f32,
    dock: Option<(slots::Dock, Vec3)>,
    can_carry: bool,
    travelling: bool,
    fading_lifecycle: bool,
}

impl ContributorFrame {
    fn capture(sim: &Simulation, author: usize, avatar: &Avatar) -> Self {
        let gutter = sim.layout().repositories[avatar.repository].gutter;
        let ring_dock =
            sim.state
                .casting_ring_seat(author)
                .and_then(|(ring, offset)| {
                    let seat = ring.centre(gutter?) + offset;
                    (avatar.position.distance_squared(seat) < 1e-8).then_some(ring.dock)
                });
        let dock = ring_dock.or_else(|| avatar.slot.as_ref().map(|slot| slot.dock));
        Self {
            repository: avatar.repository,
            position: avatar.position,
            presence: avatar.presence,
            dock: dock
                .zip(gutter)
                .map(|(dock, tile)| (dock, slots::world(dock.on_network(tile)))),
            can_carry: ring_dock.is_some()
                || (!avatar.travelling
                    && avatar.dock_walk.is_none()
                    && avatar.settle.is_none()
                    && avatar.slot.as_ref().is_some_and(|slot| {
                        avatar.transmitting
                            || avatar.position.distance_squared(slot.world()) < 1e-8
                    })),
            travelling: avatar.travelling
                || sim.travel.travelling(author, sim.state.tick),
            fading_lifecycle: avatar.dismissed || sim.seconds() >= avatar.expires,
        }
    }
}

#[derive(Default)]
struct Audit {
    report: Replay,
    directory_edges: DirectoryAudit,
    visible_files: BTreeMap<u64, (usize, Vec2)>,
    files: BTreeMap<u64, u64>,
    contributors: Vec<ContributorFrame>,
    tiles: Vec<Option<Tile>>,
    contacts: BTreeMap<(usize, usize), u32>,
    rings: BTreeMap<usize, f32>,
    arrivals: usize,
    applied: usize,
    glyph_scale: crate::model::glyph_scale::GlyphScale,
    glyph_ratios: Vec<f32>,
    glyph_motion_start: Vec<Option<u64>>,
}

pub(super) fn replay(
    events: Vec<Event>,
    labels: &[String],
    options: &Options,
    tick_limit: u32,
    fault: Fault,
) -> Result<Replay, TestCaseError> {
    const FPS: u16 = 60;
    let started = Instant::now();
    let mut sim = Simulation::try_with_repositories(
        events,
        options,
        &Theme::neon(),
        &Mappings::default(),
        labels,
    )
    .map_err(|error| TestCaseError::fail(format!("initial planning: {error:#}")))?;
    let commits = sim.events.len();
    let mut audit = Audit::default();
    audit.report.planner_wait = started.elapsed();
    for tick in 0..=tick_limit {
        let window = sim.initial.tick;
        let mut ending_arrivals = None;
        if !sim.frame_ready(tick, FPS) {
            // The preceding packet already planned its boundary tick. Playback
            // normally draws that tick from the incoming checkpoint instead.
            let mut ending = sim.clone();
            ending.stream = None;
            ending.advance_to(tick, FPS);
            ending_arrivals = Some(arrival_revisions(&ending));
            let waiting = Instant::now();
            sim.prepare_frame(tick, FPS, true).map_err(|error| {
                TestCaseError::fail(format!("tick {tick}: {error:#}"))
            })?;
            audit.report.planner_wait += waiting.elapsed();
        }
        let seam = sim.initial.tick != window;
        audit.report.windows += u32::from(seam);
        let retained_file = retained_identity(&sim, u64::from(tick));
        sim.advance_to(tick, FPS);
        if let Some((path, id)) = retained_file {
            prop_assert_eq!(
                sim.layout().file_identity(&path),
                Some(id),
                "tick {}: {} replaced a retained file identity",
                tick,
                path,
            );
        }
        if let Some(ending) = ending_arrivals {
            prop_assert_eq!(
                arrival_revisions(&sim),
                ending,
                "tick {}: the incoming packet changed the pending arrivals",
                tick
            );
        }
        audit.observe(&sim, seam, fault)?;
        if sim.planning_complete() && u64::from(tick) == sim.end_tick() {
            prop_assert_eq!(
                sim.state.next_event,
                commits,
                "tick {}: commits played",
                tick
            );
            prop_assert!(audit.arrivals > 0, "no live arrivals observed");
            for (repo, tile) in audit.tiles.iter().enumerate() {
                let archived =
                    sim.repository_exits.get(repo).is_some_and(Option::is_some);
                prop_assert_eq!(
                    tile.is_none(),
                    archived,
                    "tick {}: unexpected final tile presence for repo {} (archived {})",
                    tick,
                    repo,
                    archived
                );
            }
            audit.report.last_archive_tick = lifecycle::exit_end(&sim.repository_exits);
            audit.report.flat_lifecycles = sim.packing.flat_lifecycle_counts();
            audit.report.settled_glass_error_px = sim.packing.settled_glass_error_px();
            prop_assert!(
                audit.report.settled_glass_error_px <= 0.01,
                "settled glass does not fill its field around the gutters: {:?}",
                audit.report
            );
            let mut exits: Vec<_> =
                sim.repository_exits.iter().flatten().copied().collect();
            exits.sort_unstable_by_key(|exit| exit.start);
            audit.report.overlapping_exits =
                exits.windows(2).any(|pair| pair[1].start < pair[0].end);
            audit.glyph_ratios.sort_unstable_by(f32::total_cmp);
            let last = audit.glyph_ratios.len().saturating_sub(1);
            audit.report.glyph_ratio_p90 = audit.glyph_ratios[last * 9 / 10];
            return Ok(audit.report);
        }
    }
    Err(TestCaseError::fail(format!(
        "playback exceeded {tick_limit} ticks"
    )))
}

fn arrival_revisions(sim: &Simulation) -> BTreeMap<Target, u64> {
    sim.layout()
        .arriving_files()
        .map(|(id, file)| (Target::File(id), file.arrival_revision))
        .chain(sim.layout().dirs().filter_map(|(id, dir)| {
            dir.arrival
                .map(|arrival| (Target::Dir(id), arrival.revision))
        }))
        .collect()
}

fn arrival_destination(sim: &Simulation, target: Target, repository: usize) -> Vec3 {
    let position = match target {
        Target::File(id) => sim.layout().file(id).unwrap().position,
        Target::Dir(id) => sim.layout().dir(id).position,
    };
    ground(position) + Vec3::Y * sim.repository_effect(repository).2
}

fn retained_identity(sim: &Simulation, tick: u64) -> Option<(String, u64)> {
    let applied = sim.state.next_event;
    if sim.schedule.applied(tick) != applied + 1 {
        return None;
    }
    let [change] = sim.events.get(applied)?.changes.as_slice() else {
        return None;
    };
    let source = match &change.kind {
        Kind::Modified => change.path.as_str(),
        Kind::Moved { from } => from.as_str(),
        Kind::Added | Kind::Deleted => return None,
    };
    let id = sim.layout().file_identity(source)?;
    Some((change.path.clone(), id))
}

impl Audit {
    fn observe(
        &mut self,
        sim: &Simulation,
        seam: bool,
        fault: Fault,
    ) -> TestCaseResult {
        let tick = sim.state.tick;
        self.report.ticks = u32::try_from(tick).unwrap();
        prop_assert_eq!(
            sim.state.next_event,
            sim.schedule.applied(tick),
            "tick {}: playback deviated from the scheduled commit prefix",
            tick
        );
        if sim.state.next_event > self.applied {
            self.report.last_service_tile_tick = sim.clock.rows(tick).0;
            self.applied = sim.state.next_event;
        }
        let layout = sim.layout();
        let files = layout
            .files()
            .filter(|(_, file)| file.state != crate::layout::Presence::Leaving)
            .count();
        let directories = layout.dirs().count();
        self.report.peak_live_files = self.report.peak_live_files.max(files);
        self.report.peak_live_directories =
            self.report.peak_live_directories.max(directories);
        self.report.peak_live_items =
            self.report.peak_live_items.max(files + directories);
        let (retained, resizing) = self.directory_edges.observe(sim, &self.tiles)?;
        self.report.directory_intervals += retained;
        self.report.resizing_directory_intervals += resizing;
        self.measure_glyphs(sim);
        self.check_tiles(sim)?;
        let visible_files = layout
            .files()
            .filter(|(_, file)| file.presence > 0.0)
            .map(|(id, file)| (id, (file.dir, file.local_position())))
            .collect::<BTreeMap<_, _>>();
        for (&id, &(dir, position)) in &visible_files {
            if let Some(&(previous_dir, previous_position)) =
                self.visible_files.get(&id)
            {
                self.report.reparented_files += usize::from(dir != previous_dir);
                let movement = position.distance(previous_position);
                prop_assert!(
                    movement <= (crate::layout::MAX_SPEED + 0.001) * tick_seconds(),
                    "tick {tick}: retained file {id} moved {movement} units in one frame",
                );
            }
        }
        self.visible_files = visible_files;
        let held = self.check_rays(sim, seam, fault)?;
        self.check_contributors(sim, &held)?;
        self.check_circles(sim, seam)?;
        self.measure_contacts(sim);
        Ok(())
    }

    fn measure_glyphs(&mut self, sim: &Simulation) {
        let layout = sim.layout();
        let tick = sim.state.tick;
        self.glyph_scale.sample(sim);
        self.glyph_motion_start
            .resize(layout.repositories.len(), None);
        for (repo, start) in self.glyph_motion_start.iter_mut().enumerate() {
            let visible = layout.repositories[repo].tile.is_some();
            let last = self.glyph_scale.last_motion(repo);
            if visible && last == tick {
                start.get_or_insert(tick);
            } else if (self.glyph_scale.settled(repo) || !visible)
                && let Some(start) = start.take()
            {
                self.report.glyph_settle_ticks.push(last - start + 1);
            }
        }
        let mut minimum = f32::INFINITY;
        let mut maximum = 0.0_f32;
        let mut first_repository = None;
        let mut comparable = false;
        for (_, file) in layout.files() {
            let dir = layout.dir(file.dir);
            if self.glyph_scale.settled(dir.repository)
                && file.state != crate::layout::Presence::Leaving
            {
                let scale = self.glyph_scale.scale(dir.repository, dir.scale);
                let width = self.glyph_scale.overview_width(scale);
                comparable |=
                    *first_repository.get_or_insert(dir.repository) != dir.repository;
                minimum = minimum.min(width);
                maximum = maximum.max(width);
            }
        }
        let ratio = if maximum > 0.0 {
            maximum / minimum
        } else {
            1.0
        };
        self.report.comparable_glyph_frames += usize::from(comparable);
        self.report.out_of_band_glyph_frames +=
            usize::from(ratio > 2.0 + 8.0 * f32::EPSILON);
        self.glyph_ratios.push(ratio);
        if ratio > self.report.worst_glyph_ratio {
            self.report.worst_glyph_ratio = ratio;
            self.report.worst_glyph_tick = tick;
        }
    }

    fn check_rays(
        &mut self,
        sim: &Simulation,
        seam: bool,
        fault: Fault,
    ) -> Result<BTreeSet<usize>, TestCaseError> {
        let tick = sim.state.tick;
        let layout = sim.layout();
        let mut files = BTreeMap::new();
        let mut held = BTreeSet::new();
        let mut checked_targets = BTreeSet::new();
        for (file, arrival) in layout.arriving_files() {
            let repository = layout.dir(arrival.dir).repository;
            let to = ground(arrival.position)
                + Vec3::Y * sim.repository_effect(repository).2;
            let (beam, progress) = sim.arrival_beam(
                Target::File(file),
                arrival.arrival_author,
                to,
                arrival.presence,
                arrival.flight,
                arrival.arrival_revision,
            );
            let flight_end = if arrival.flight.is_some() {
                ARRIVAL_FLIGHT
            } else {
                1.0
            };
            if progress <= 0.0 || progress >= flight_end {
                continue;
            }
            let Some(mut beam) = beam else {
                return Err(TestCaseError::fail(format!(
                    "tick {tick}: repo {repository}, author {}, file {} has no arrival ray",
                    arrival.arrival_author, arrival.path,
                )));
            };
            prop_assert_eq!(
                beam.to,
                arrival_destination(sim, beam.target, repository),
                "tick {}: moving arrival {} has a stale destination",
                tick,
                arrival.path,
            );
            self.arrivals += 1;
            files.insert(file, arrival.arrival_revision);
            let carried = self.files.get(&file) == Some(&arrival.arrival_revision);
            if carried {
                held.insert(beam.author);
                self.report.arrivals_at_seam += u32::from(seam);
            }
            if !checked_targets.insert((beam.author, beam.target)) {
                continue;
            }
            if seam
                && carried
                && matches!(fault, Fault::ForeignRayAtSeam)
                && let Some(tile) =
                    self.tiles.iter().enumerate().find_map(|(other, tile)| {
                        (other != repository).then_some(*tile).flatten()
                    })
            {
                let centre = ground_rect(tile).center();
                beam.from.x = centre.x;
                beam.from.z = centre.y;
            }
            self.check_ray(sim, beam, repository, &arrival.path)?;
        }
        for (directory, dir) in layout.dirs() {
            let to = ground(dir.position)
                + Vec3::Y * sim.repository_effect(dir.repository).2;
            if let Some((beam, _, progress)) = sim.directory_arrival(directory, to)
                && progress > 0.0
                && progress < ARRIVAL_FLIGHT
            {
                let Some(beam) = beam else {
                    return Err(TestCaseError::fail(format!(
                        "tick {tick}: repo {}, directory {directory} has no arrival ray",
                        dir.repository,
                    )));
                };
                self.check_ray(
                    sim,
                    beam,
                    dir.repository,
                    &format!("directory {directory}"),
                )?;
            }
        }
        for beam in sim.beams() {
            let directory = match beam.target {
                Target::File(id) => layout.file(id).unwrap().dir,
                Target::Dir(id) => id,
            };
            self.check_ray(
                sim,
                beam,
                layout.dir(directory).repository,
                &format!("{:?}", beam.target),
            )?;
        }
        self.files = files;
        Ok(held)
    }

    fn check_ray(
        &self,
        sim: &Simulation,
        beam: Beam,
        repository: usize,
        target: &str,
    ) -> TestCaseResult {
        let tick = sim.state.tick;
        let avatar = &sim.state.avatars[beam.author];
        let gutter = sim.layout().repositories[repository].gutter;
        prop_assert!(
            avatar.repository == repository
                && avatar.presence > 0.0
                && gutter.is_some_and(|tile| travel::field_rect(tile)
                    .inflate(sim.state.avatar_radius())
                    .contains(slots::field_point(avatar.position))),
            "tick {}: author {} detached from repository {} while casting to {}",
            tick,
            avatar.name,
            repository,
            target,
        );
        let from = beam.from.xz();
        let to = beam.to.xz();
        prop_assert!(
            from.is_finite() && to.is_finite(),
            "tick {}: non-finite ray to {}",
            tick,
            target
        );
        for (other, tile) in self.tiles.iter().enumerate() {
            if let Some(tile) = tile.filter(|_| other != repository) {
                prop_assert!(
                    !slots::segment(from, to, ground_rect(tile)),
                    "tick {}: author {}, {}, repository {}, crossed repository {}",
                    tick,
                    avatar.name,
                    target,
                    repository,
                    other,
                );
            }
        }
        Ok(())
    }

    fn check_contributors(
        &mut self,
        sim: &Simulation,
        held: &BTreeSet<usize>,
    ) -> TestCaseResult {
        let tick = sim.state.tick;
        let mut next = Vec::with_capacity(sim.state.avatars.len());
        for (author, avatar) in sim.state.avatars.iter().enumerate() {
            let after = ContributorFrame::capture(sim, author, avatar);
            next.push(after);
            let Some(before) = self.contributors.get(author) else {
                continue;
            };
            let visible = before.presence > 0.001 && after.presence > 0.001;
            let step = after.position.distance(before.position);
            let name = &avatar.name;
            if held.contains(&author) {
                let Some(((old_dock, old_anchor), (dock, anchor))) =
                    before.dock.zip(after.dock)
                else {
                    return Err(TestCaseError::fail(format!(
                        "tick {tick}: {name} lost their casting dock"
                    )));
                };
                prop_assert!(
                    before.repository == after.repository
                        && old_dock == dock
                        && after.presence >= before.presence - 1e-5
                        && (after.position - before.position)
                            .distance(anchor - old_anchor)
                            < 1e-4,
                    "tick {}: {} moved or faded while casting in repo {}",
                    tick,
                    name,
                    after.repository,
                );
                self.report.moving_docks += u32::from(step > 1e-5);
            } else if visible {
                let cap = sim.state.avatar_max_speed
                    * framing::WORLD_UNITS_PER_REFERENCE_PIXEL
                    * tick_seconds();
                let carried = before
                    .dock
                    .zip(sim.layout().repositories[before.repository].gutter)
                    .filter(|_| {
                        before.repository == after.repository
                            && before.can_carry
                            && !sim.travel.travelling(author, tick)
                    })
                    .map_or(Vec3::ZERO, |((dock, anchor), tile)| {
                        slots::world(dock.on_network(tile)) - anchor
                    });
                self.report.max_carried_step =
                    self.report.max_carried_step.max(carried.length());
                let relative_step =
                    (after.position - before.position - carried).length();
                prop_assert!(
                    relative_step <= cap + 0.001,
                    "tick {}: {} stepped {} relative to dock (world {}, carried {}, cap {})",
                    tick,
                    name,
                    relative_step,
                    step,
                    carried.length(),
                    cap
                );
            }
            if before.repository != after.repository {
                self.report.cross_repo_services += 1;
            } else if before.presence > 0.001
                && !before.fading_lifecycle
                && !after.fading_lifecycle
                && !before.travelling
                && !after.travelling
                && avatar.group.is_none()
            {
                prop_assert!(
                    after.presence >= before.presence - 1e-5,
                    "tick {}: parked {} faded in repo {}",
                    tick,
                    name,
                    after.repository
                );
            }
        }
        self.contributors = next;
        Ok(())
    }

    fn check_circles(&mut self, sim: &Simulation, seam: bool) -> TestCaseResult {
        let tick = sim.state.tick;
        let mut rings = BTreeMap::new();
        for (author, avatar) in sim.state.avatars.iter().enumerate() {
            let Some(ring) = avatar.casting_ring else {
                continue;
            };
            let name = &avatar.name;
            let Some((_, radius)) = sim.state.visible_ring(author) else {
                return Err(TestCaseError::fail(format!(
                    "tick {tick}: {name} cast from a hidden circle"
                )));
            };
            prop_assert!(
                avatar.ring_presence >= 1.0 && (ring.radius - radius).abs() < 1e-4,
                "tick {}: {} cast before their circle reached full radius",
                tick,
                name
            );
            self.report.ring_frames += 1;
            if let Some(previous) = self.rings.get(&author) {
                prop_assert!(
                    (previous - radius).abs() < 1e-4,
                    "tick {}: {} changed circle radius while casting",
                    tick,
                    name
                );
                self.report.ring_seams += u32::from(seam);
            }
            rings.insert(author, radius);
        }
        self.rings = rings;
        Ok(())
    }

    fn measure_contacts(&mut self, sim: &Simulation) {
        let mut contributors = vec![0; self.tiles.len()];
        for avatar in sim
            .avatars()
            .iter()
            .filter(|avatar| avatar.presence > 0.001)
        {
            contributors[avatar.repository] += 1;
        }
        self.report.peak_visible_contributors = self
            .report
            .peak_visible_contributors
            .max(contributors.iter().sum());
        self.report.peak_contributors_per_repository = self
            .report
            .peak_contributors_per_repository
            .max(contributors.into_iter().max().unwrap_or(0));
        let mut contacts = BTreeMap::new();
        for (a, b) in occlusion::avatar_contacts_for_test(sim) {
            let parked = |author: usize| {
                !self.contributors[author].travelling
                    && !self.contributors[author].fading_lifecycle
                    && sim.state.avatars[author].settle.is_none()
            };
            if parked(a) && parked(b) {
                let duration = self.contacts.get(&(a, b)).copied().unwrap_or(0) + 1;
                contacts.insert((a, b), duration);
                self.report.longest_parked_contact =
                    self.report.longest_parked_contact.max(duration);
            } else {
                self.report.passing_contacts += 1;
            }
        }
        self.contacts = contacts;
    }

    fn check_tiles(&mut self, sim: &Simulation) -> TestCaseResult {
        let tick = sim.state.tick;
        let tiles: Vec<_> = sim
            .layout()
            .repositories
            .iter()
            .map(|region| region.tile)
            .collect();
        for (a, tile) in tiles.iter().enumerate() {
            let Some(tile) = *tile else { continue };
            let before = self.tiles.get(a).copied().flatten();
            if let Some(before) = before {
                let step = (before.low - tile.low)
                    .abs()
                    .max((before.high - tile.high).abs());
                let step_px = (step * framing::VIEWPORT * 0.5).max_element();
                if step_px > self.report.max_tile_step_px {
                    self.report.max_tile_step_px = step_px;
                    self.report.max_tile_step_at =
                        Some((a, tick, sim.clock.rows(tick).0));
                }
            }
            for (b, other) in tiles.iter().enumerate().skip(a + 1) {
                let Some(other) = *other else { continue };
                let overlap = tile.high.min(other.high) - tile.low.max(other.low);
                prop_assert!(
                    overlap.min_element() <= TILE_TOLERANCE,
                    "tick {}: tile interiors overlap for repos {} and {}",
                    tick,
                    a,
                    b
                );
                if let Some((old, old_other)) =
                    before.zip(self.tiles.get(b).copied().flatten())
                {
                    prop_assert!(
                        !swept_tile_overlap((old, tile), (old_other, other)),
                        "interval ending at tick {}: swept tile interiors overlap for repos {} and {}",
                        tick,
                        a,
                        b
                    );
                }
            }
        }
        let occupied: f32 = sim
            .layout()
            .repositories
            .iter()
            .filter_map(|r| r.gutter)
            .map(Tile::area)
            .sum();
        self.report.final_gap = (sim.packing.gutter_field().area() - occupied).max(0.0);
        self.tiles = tiles;
        Ok(())
    }
}

const TILE_TOLERANCE: f32 = 1e-5;

// Interior overlap requires all four separating-axis inequalities to hold at
// the same fraction of the interval. Each edge moves linearly in this contract.
fn swept_tile_overlap((a0, a1): (Tile, Tile), (b0, b1): (Tile, Tile)) -> bool {
    let separations = |a: Tile, b: Tile| {
        [
            a.high.x - b.low.x,
            b.high.x - a.low.x,
            a.high.y - b.low.y,
            b.high.y - a.low.y,
        ]
    };
    let mut enter = 0.0_f32;
    let mut leave = 1.0_f32;
    for (start, end) in separations(a0, b0).into_iter().zip(separations(a1, b1)) {
        if start <= TILE_TOLERANCE && end <= TILE_TOLERANCE {
            return false;
        }
        if start <= TILE_TOLERANCE {
            enter = enter.max((TILE_TOLERANCE - start) / (end - start));
        } else if end <= TILE_TOLERANCE {
            leave = leave.min((TILE_TOLERANCE - start) / (end - start));
        }
    }
    enter < leave
}

fn ground_rect(tile: Tile) -> Rect {
    let corners = tile.ground();
    Rect::from_corners(corners[0], corners[2])
}

#[test]
fn a_tile_exchange_can_overlap_between_clear_endpoints() {
    let tile = |x, y| Tile {
        low: Vec2::new(x, y),
        high: Vec2::new(x + 1.0, y + 1.0),
    };
    let (left, right) = (tile(0.0, 0.0), tile(2.0, 0.0));
    assert!(!swept_tile_overlap((left, left), (right, right)));
    assert!(!swept_tile_overlap((right, right), (left, left)));
    assert!(swept_tile_overlap((left, right), (right, left)));
    assert!(!swept_tile_overlap(
        (left, right),
        (tile(2.0, 1.0), tile(0.0, 1.0))
    ));
}
