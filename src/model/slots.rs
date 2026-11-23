use std::sync::Arc;
pub(crate) mod diagnostics;
mod envelopes;
pub(super) mod labels;
#[cfg(test)]
mod motion_tests;
#[cfg(test)]
pub(super) mod name_oracle;
pub(super) use envelopes::Snapshots;

use bevy::math::{Rect, Vec2, Vec3};
use serde::Serialize;

use super::{
    MIN_AVATAR_HEIGHT, Simulation, State, Target, count, framing, ground, project,
    view_direction,
};
use crate::layout::packing::Tile;

const HALF: Vec2 = Vec2::new(640.0, 360.0);
const HALO: f32 = 2.0;

const TRAVEL_WEIGHT: f32 = 200_000.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Envelope {
    low: Vec2,
    high: Vec2,
    samples: u32,
    mean: f32,
    rays: Arc<Vec<Vec2>>,
}

impl Envelope {
    fn include(&mut self, points: &[Vec2], tile: Tile) {
        if points.is_empty() {
            return;
        }
        let size = (tile.high - tile.low).max(Vec2::splat(1e-8));
        let mut sum = 0.0;
        for &point in points {
            let relative = (point - tile.low) / size;
            if self.samples == 0 {
                self.low = relative;
                self.high = relative;
            }
            self.low = self.low.min(relative);
            self.high = self.high.max(relative);
            self.samples += 1;
            sum += relative.x;
        }
        if self.samples == u32::try_from(points.len()).unwrap_or(u32::MAX) {
            self.mean = sum / count(points.len());
        }
    }

    fn include_rays(&mut self, points: &[Vec2], touched: usize, tile: Tile) {
        if self.samples == 0 {
            let size = (tile.high - tile.low).max(Vec2::splat(1e-8));
            self.rays = Arc::new(
                points
                    .iter()
                    .take(touched)
                    .map(|p| (*p - tile.low) / size)
                    .collect(),
            );
        }
    }
    fn include_service(&mut self, points: &mut [Vec3], touched: usize, tile: Tile) {
        let initial_mean = (self.samples == 0)
            .then(|| touched_mean(&points[..touched], tile))
            .flatten();
        for point in points.iter_mut() {
            point.y += 0.12;
        }
        let points: Vec<_> =
            project(view_direction() * framing::DISTANCE, Vec3::ZERO, points)
                .flatten()
                .collect();
        self.include_rays(&points, touched, tile);
        self.include(&points, tile);
        if let Some(mean) = initial_mean {
            self.mean = mean;
        }
    }
    fn corners(&self, tile: Tile) -> [Vec2; 4] {
        let size = tile.high - tile.low;
        Tile {
            low: (tile.low + size * self.low) * HALF,
            high: (tile.low + size * self.high) * HALF,
        }
        .corners()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Dock {
    edge: u8,
    fraction: f32,
}

impl Dock {
    #[cfg(test)]
    pub(in crate::model) fn new(edge: u8, fraction: f32) -> Self {
        Self { edge, fraction }
    }
    pub(in crate::model) fn on_network(self, tile: Tile) -> Vec2 {
        let low = tile.low * HALF;
        let high = tile.high * HALF;
        let fraction = self.fraction.clamp(0.0, 1.0);
        match self.edge {
            0 => Vec2::new(low.x + (high.x - low.x) * fraction, high.y),
            1 => Vec2::new(high.x, low.y + (high.y - low.y) * fraction),
            2 => Vec2::new(low.x + (high.x - low.x) * fraction, low.y),
            _ => Vec2::new(low.x, low.y + (high.y - low.y) * fraction),
        }
    }
    fn point(self, tile: Tile, gap: Vec2) -> Vec2 {
        let fraction = self.fraction;
        let inset = Vec2::splat(0.05);
        let low = tile.low * HALF + inset;
        let high = tile.high * HALF - inset;
        match self.edge {
            0 => Vec2::new(low.x + (high.x - low.x) * fraction, high.y + gap.y * 0.5),
            1 => Vec2::new(high.x + gap.x * 0.5, low.y + (high.y - low.y) * fraction),
            2 => Vec2::new(low.x + (high.x - low.x) * fraction, low.y - gap.y * 0.5),
            _ => Vec2::new(low.x - gap.x * 0.5, low.y + (high.y - low.y) * fraction),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Placement {
    pub dock: Dock,
    pub commit: usize,
    pub point: Vec2,
    /// From the dock's point to this avatar's disc, which is nonzero in a group.
    pub offset: Vec2,
    pub text: String,
    pub half_plane: bool,
    pub crossings: usize,
    pub overlaps: usize,
    pub missing_tile: bool,
}

impl Placement {
    pub fn valid(&self) -> bool {
        !self.missing_tile
            && self.half_plane
            && self.crossings == 0
            && self.overlaps == 0
    }
    pub fn world(&self) -> Vec3 {
        world(self.point)
    }
    /// The dock's own point, where travel routes end and a group's ring is centred.
    pub fn centre(&self) -> Vec2 {
        self.point - self.offset
    }

    /// The centre of the ring round a group of `capacity` seats parked here.
    pub fn ring_centre(&self, capacity: u8, radius: f32) -> Vec2 {
        self.centre() + ring_offset(capacity, self.dock.edge, radius)
    }
}

/// Cost per radian of a ring member's name turning away from the point of the ring
/// nearest that member; below holding a name still and a crossed beam.
const NEAREST_NAME_WEIGHT: f32 = 20_000.0;

/// Space between neighbouring members packed in a group, so they touch without
/// overlapping.
const GROUP_GAP: f32 = 0.5;
/// Space between a group's members and its containing ring.
const RING_GAP: f32 = 2.0;
/// A group's members are drawn this much smaller than an avatar alone.
pub(crate) const MEMBER_SCALE: f32 = 0.75;

/// Distance from a group's centre to each seat: seats sit evenly round a circle just
/// wide enough for neighbouring members to touch.
fn seat_spread(capacity: u8, radius: f32) -> f32 {
    if capacity < 2 {
        return 0.0;
    }
    (radius * MEMBER_SCALE + GROUP_GAP)
        / (std::f32::consts::PI / f32::from(capacity)).sin()
}

/// Radius of the ring containing a group with `capacity` seats; one seat is an avatar
/// alone.
pub(super) fn group_radius(capacity: u8, radius: f32) -> f32 {
    if capacity < 2 {
        radius
    } else {
        seat_spread(capacity, radius) + radius * MEMBER_SCALE + RING_GAP
    }
}

/// A seat's place relative to its group's ring centre at a dock on `edge`: seat 0
/// faces into the tile, and the seats turn so a flat side, not a seat, faces out of it.
fn seat_around(seat: u8, capacity: u8, edge: u8, radius: f32) -> Vec2 {
    let capacity = capacity.max(1);
    let turn = if capacity.is_multiple_of(2) {
        std::f32::consts::PI / f32::from(capacity)
    } else {
        0.0
    };
    let angle = inward(edge).to_angle()
        + turn
        + std::f32::consts::TAU * f32::from(seat) / f32::from(capacity);
    Vec2::from_angle(angle) * seat_spread(capacity, radius)
}

/// The direction from a dock on `edge` into its tile.
fn inward(edge: u8) -> Vec2 {
    [-Vec2::Y, -Vec2::X, Vec2::Y, Vec2::X][usize::from(edge % 4)]
}

/// A group's ring centre relative to its dock: moved into the tile until the ring
/// reaches no further out than an avatar alone would, so it stays in view wherever
/// one fits.
pub(super) fn ring_offset(capacity: u8, edge: u8, radius: f32) -> Vec2 {
    inward(edge) * (group_radius(capacity, radius) - radius)
}

/// A seat's place relative to its group's dock.
fn seat_offset(seat: u8, capacity: u8, edge: u8, radius: f32) -> Vec2 {
    ring_offset(capacity, edge, radius) + seat_around(seat, capacity, edge, radius)
}

pub(super) fn world(point: Vec2) -> Vec3 {
    ground(framing::field_to_world(point / HALF)) + Vec3::Y * MIN_AVATAR_HEIGHT
}

pub(super) fn field_point(position: Vec3) -> Vec2 {
    framing::world_to_field(Vec2::new(position.x, position.z)) * HALF
}

pub(super) fn projection_scale(point: Vec3) -> f32 {
    let eye = view_direction() * framing::DISTANCE;
    let depth = (eye - point).dot(view_direction());
    depth * depth / (framing::DISTANCE * eye.distance(point))
}
#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct Record {
    pub fallback: bool,
    pub least_bad: bool,
    pub missing_tile: bool,
    pub observations: u32,
    pub half_plane_failures: u32,
    pub max_crossings: usize,
    pub max_overlaps: usize,
}

pub(super) fn segment(a: Vec2, b: Vec2, rect: Rect) -> bool {
    let delta = b - a;
    let mut enter = 0.0_f32;
    let mut exit = 1.0_f32;
    for axis in 0..2 {
        if delta[axis].abs() < 1e-7 {
            if a[axis] < rect.min[axis] || a[axis] > rect.max[axis] {
                return false;
            }
        } else {
            let lo = (rect.min[axis] - a[axis]) / delta[axis];
            let hi = (rect.max[axis] - a[axis]) / delta[axis];
            enter = enter.max(lo.min(hi));
            exit = exit.min(lo.max(hi));
        }
    }
    enter <= exit
}

fn fan(point: Vec2, corners: &[Vec2; 4]) -> f32 {
    let mut angles = corners.map(|p| (p - point).to_angle());
    if corners.iter().any(|p| p.distance_squared(point) < 1e-8) {
        return std::f32::consts::TAU;
    }
    angles.sort_by(f32::total_cmp);
    let gap = angles
        .windows(2)
        .map(|w| w[1] - w[0])
        .fold(angles[0] + std::f32::consts::TAU - angles[3], f32::max);
    std::f32::consts::TAU - gap
}

#[cfg(test)]
fn intersect(a: Rect, b: Rect) -> bool {
    !a.intersect(b).is_empty()
}
/// Frees an avatar's retained space while it chooses again.
fn release(occupied: &mut Vec<Rect>, own: Option<Rect>) {
    if let Some(own) = own
        && let Some(position) = occupied.iter().position(|r| *r == own)
    {
        occupied.remove(position);
    }
}

fn disk(point: Vec2, radius: f32) -> Rect {
    Rect::from_center_size(point, Vec2::splat(radius * 2.0))
}

fn tile_obstacles(state: &State) -> Vec<(usize, Rect)> {
    let mut result = Vec::new();
    for (index, region) in state.layout.repositories.iter().enumerate() {
        let Some(tile) = region.tile else {
            continue;
        };
        result.push((index, Rect::from_corners(tile.low * HALF, tile.high * HALF)));
    }
    result
}

pub(super) fn content_obstacles(state: &State) -> Vec<(usize, Rect)> {
    let mut result = Vec::new();
    for (index, region) in state.layout.repositories.iter().enumerate() {
        if region.tile.is_none() {
            continue;
        }
        let points: Vec<_> = region
            .outline
            .iter()
            .copied()
            .flat_map(|p| [ground(p), ground(p) + Vec3::Y * region.height])
            .collect();
        let projected =
            project(view_direction() * framing::DISTANCE, Vec3::ZERO, &points)
                .flatten()
                .map(|p| p * HALF);
        let (low, high) = projected.fold(
            (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)),
            |(lo, hi), p| (lo.min(p), hi.max(p)),
        );
        if low.is_finite() && high.is_finite() {
            result.push((index, Rect::from_corners(low, high).inflate(HALO)));
        }
    }
    result
}

fn short_name(name: &str, scale: f32, width: f32) -> String {
    let budget = width.min(112.0);
    let fits = |characters| 8.0 + 8.6 * count(characters) * scale <= budget;
    if fits(name.chars().count()) {
        return name.to_owned();
    }
    if !fits(3) {
        return String::new();
    }
    let mut text = String::new();
    for ch in name.chars() {
        if !fits(text.chars().count() + 4) {
            break;
        }
        text.push(ch);
    }
    text.push_str("...");
    text
}

fn place_point(point: Vec3, rotated: bool, scale: f32, origin: Vec2) -> Vec3 {
    let local = Vec2::new(point.x, point.z);
    let local = if rotated { local.perp() } else { local };
    ground(local * scale + origin)
}
impl Simulation {
    fn plan_ray_runs(&mut self) {
        let mut runs = Vec::<Vec<usize>>::new();
        let mut authors = std::collections::HashMap::new();
        for (commit, event) in self.events.iter().enumerate() {
            let key = &event.contributor.as_ref().expect("resolved author").key;
            let run = authors
                .get(key)
                .copied()
                .filter(|&run: &usize| {
                    self.events[runs[run][0]].repository == event.repository
                })
                .unwrap_or_else(|| {
                    runs.push(Vec::new());
                    runs.len() - 1
                });
            authors.insert(key.clone(), run);
            runs[run].push(commit);
        }
        for run in runs {
            let points: Vec<_> = run
                .iter()
                .flat_map(|&commit| self.envelopes[commit].rays.iter().copied())
                .collect();
            let shared = Arc::new(points);
            for commit in run {
                self.envelopes[commit].rays = shared.clone();
            }
        }
    }
    pub(super) fn plan_avatar_envelopes(&mut self, snapshots: Option<Snapshots>) {
        let _phase = self.trace.phase(super::trace::Phase::AvatarEnvelopes);
        if let Some(snapshots) = snapshots {
            self.envelopes = snapshots.project(&self.packing, self.events.len());
            self.plan_ray_runs();
            return;
        }
        let mut envelopes = vec![Envelope::default(); self.events.len()];
        let frames = super::frames_for(self.duration(), 60)
            .expect("validated playback frame bound");
        let frozen: Vec<_> = self
            .envelopes
            .iter()
            .take(self.travel.frozen)
            .cloned()
            .collect();
        self.restart();
        self.trace
            .count_replay(u64::from(frames).saturating_sub(self.state.tick));
        for _ in self.state.tick..u64::from(frames) {
            let files = self.file_motion(crate::layout::FileMotion::Glide);
            self.tick_local_layout(files);
            let now = super::tick_time(self.state.tick);
            self.state.pulses.retain(|p| now < p.end);
            if self.state.pulses.is_empty() {
                continue;
            }
            let mut needed: Vec<_> =
                self.state.pulses.iter().map(|p| p.repository).collect();
            needed.sort_unstable();
            needed.dedup();
            let (regions, origins, scales) = self
                .packing_sample(self.state.tick, Some(&needed))
                .expect("planned repository samples");
            for pulse in &self.state.pulses {
                let Some(tile) = regions[pulse.repository].tile else {
                    continue;
                };
                let origin = origins[pulse.repository];
                let transform = |point| {
                    place_point(
                        point,
                        regions[pulse.repository].rotated,
                        scales[pulse.repository],
                        origin,
                    )
                };
                let mut points: Vec<_> = pulse
                    .touched
                    .iter()
                    .filter_map(|&id| self.state.point(Target::File(id)))
                    .map(transform)
                    .collect();
                let touched = points.len();
                points.extend(
                    pulse
                        .targets
                        .iter()
                        .filter_map(|&(target, ..)| self.state.point(target))
                        .map(transform),
                );
                envelopes[pulse.commit].include_service(&mut points, touched, tile);
            }
        }
        self.trace.add_ticks(&mut self.tick_times);
        self.envelopes = envelopes;
        self.plan_ray_runs();
        self.envelopes[..frozen.len()].clone_from_slice(&frozen);
        self.restart();
    }
}

impl State {
    pub(super) fn service_corners(
        &self,
        repository: usize,
        envelope: &Envelope,
    ) -> Option<[Vec3; 4]> {
        let tile = self.layout.repositories[repository].tile?;
        Some(
            envelope
                .corners(tile)
                .map(|point| ground(framing::field_to_world(point / HALF))),
        )
    }

    #[cfg(test)]
    pub(super) fn place_slots(&mut self, envelopes: &[Envelope], now: super::Time) {
        self.place_slots_planned(envelopes, now, &super::travel::Plan::default());
    }
    fn slot_peers(
        &self,
        index: usize,
        plan: &super::travel::Plan,
        choosing: bool,
    ) -> Vec<(Vec2, u8, Vec2)> {
        if !choosing {
            return Vec::new();
        }
        let contents = content_obstacles(self);
        let repository = self.avatars[index].repository;
        let tile = self.layout.repositories[repository]
            .tile
            .expect("visible service tile");
        let centre = contents
            .iter()
            .find(|(owner, _)| *owner == repository)
            .map_or((tile.low + tile.high) * HALF * 0.5, |(_, r)| r.center());
        self.avatars
            .iter()
            .enumerate()
            .filter(|(i, a)| {
                *i != index
                    && a.repository == repository
                    && a.presence > 0.0
                    && !plan.travelling(*i, self.tick)
            })
            .filter_map(|(_, a)| {
                a.slot.as_ref().map(|s| (s.point, s.dock.edge, centre))
            })
            .collect()
    }
    fn retained_slots(&self, now: super::Time, radius: f32) -> Vec<Option<Rect>> {
        let capacities = self.compute_capacities();
        (0..self.avatars.len())
            .map(|index| {
                let avatar = &self.avatars[index];
                if now >= avatar.expires && !avatar.transmitting {
                    return None;
                }
                let slot = avatar.slot.as_ref()?;
                let tile = self.layout.repositories[avatar.repository].gutter?;
                let point = if avatar.transmitting {
                    field_point(avatar.position)
                } else {
                    slot.dock.on_network(tile) + slot.offset
                };
                self.held_space(index, point, &capacities, radius)
            })
            .collect()
    }

    /// The space an avatar parked at `point` holds: its group's ring for an author, none
    /// for a co-author inside that ring, else its own disc.
    fn held_space(
        &self,
        index: usize,
        point: Vec2,
        capacities: &[u8],
        radius: f32,
    ) -> Option<Rect> {
        let avatar = &self.avatars[index];
        let lead = avatar.group.map_or(index, |g| g.lead);
        if lead != index && self.in_party(index, lead) {
            return None;
        }
        let capacity = capacities[index];
        let centre = avatar
            .slot
            .as_ref()
            .filter(|_| capacity > 1)
            .map_or(point, |s| {
                point - s.offset + ring_offset(capacity, s.dock.edge, radius)
            });
        Some(disk(centre, group_radius(capacity, radius)))
    }

    /// Whether `index` parks in `lead`'s group: the author itself, or a co-author who
    /// joined it and has not since left for another commit, repository or dock.
    pub(super) fn in_party(&self, index: usize, lead: usize) -> bool {
        if index == lead {
            return true;
        }
        let (member, author) = (&self.avatars[index], &self.avatars[lead]);
        member.group.is_some_and(|g| g.lead == lead)
            && author.group.is_some_and(|g| g.lead == lead)
            && member.repository == author.repository
            && super::tick_time(self.tick) < member.expires
            && (member.commit == author.commit
                || member
                    .slot
                    .as_ref()
                    .zip(author.slot.as_ref())
                    .is_some_and(|(m, a)| m.dock == a.dock))
    }

    /// Seats in each avatar's group, counting the gaps members left; 1 for an avatar
    /// without co-authors.
    pub(super) fn compute_capacities(&self) -> Vec<u8> {
        let mut capacities = vec![1_u8; self.avatars.len()];
        for (index, avatar) in self.avatars.iter().enumerate() {
            if let Some(group) = avatar.group.filter(|g| g.lead != index)
                && self.in_party(index, group.lead)
            {
                let seats = &mut capacities[group.lead];
                *seats = (*seats).max(group.seat + 1);
            }
        }
        capacities
    }

    /// An author's group in seat order, the author first.
    fn party(&self, lead: usize) -> Vec<usize> {
        let mut party: Vec<_> = (0..self.avatars.len())
            .filter(|&i| self.in_party(i, lead))
            .collect();
        party.sort_by_key(|&i| (i != lead, self.avatars[i].group.map(|g| g.seat)));
        party
    }

    fn incumbent_discs(
        &self,
        index: usize,
        party: &[usize],
        now: super::Time,
        scales: &[f32],
        radius: f32,
    ) -> Vec<Rect> {
        let priority = |index: usize| {
            let avatar = &self.avatars[index];
            (avatar.dock_since, index)
        };
        let newcomer = self.avatars[index].slot.is_none();
        self.avatars
            .iter()
            .enumerate()
            .filter(|&(other, peer)| {
                !party.contains(&other)
                    && (peer.presence > 0.0 || now < peer.expires)
                    && (peer.transmitting
                        || newcomer
                        || priority(other) < priority(index))
            })
            .filter_map(|(other, peer)| {
                let slot = peer.slot.as_ref()?;
                let tile = self.layout.repositories[peer.repository].gutter?;
                let point = if peer.transmitting {
                    field_point(peer.position)
                } else {
                    slot.dock.on_network(tile) + slot.offset
                };
                Some(disk(point, radius * scales[other]))
            })
            .collect()
    }

    pub(super) fn place_slots_planned(
        &mut self,
        envelopes: &[Envelope],
        now: super::Time,
        plan: &super::travel::Plan,
    ) {
        let gap = Vec2::splat(self.gutter);
        let beams = tile_obstacles(self);
        let cells = super::travel::network(
            self.layout.repositories.iter().filter_map(|r| r.gutter),
        );
        let radius = self.avatar_radius();
        let name_scale = super::LabelKind::Avatar.font_scale() * self.field_growth;
        let capacities = self.compute_capacities();
        let scales = self.member_scales();
        let mut occupied = self.hold_idle(now, &capacities, radius);
        let retained = self.retained_slots(now, radius);
        occupied.extend(retained.iter().flatten().copied());
        for index in self.placing_order(now) {
            let avatar = &self.avatars[index];
            let commit = avatar.commit;
            if let Some(group) = avatar.group.filter(|g| g.lead != index) {
                if !self.in_party(index, group.lead)
                    || now - self.avatars[group.lead].last_active >= super::AVATAR_IDLE
                {
                    self.hold_place(index, &mut occupied);
                }
                continue;
            }
            let party = self.party(index);
            for &member in &party {
                release(&mut occupied, retained[member]);
            }
            let capacity = capacities[index];
            let envelope = envelopes[commit].clone();
            self.slot_records.entry(commit).or_default().observations += 1;
            let Some(tile) = self.layout.repositories[avatar.repository].tile else {
                let record = self.slot_records.entry(commit).or_default();
                record.least_bad = true;
                record.missing_tile = true;
                for &member in &party {
                    self.avatars[member].slot = None;
                }
                continue;
            };
            let text = short_name(&avatar.name, name_scale, tile.pixels().x - 24.0);
            let choosing = avatar.slot.as_ref().is_none_or(|s| s.commit != commit);
            let future = plan.reserved(
                commit,
                index,
                self.layout.repositories[avatar.repository].gutter,
                choosing,
            );
            let peers = self.slot_peers(index, plan, choosing);
            let clearance = self.incumbent_discs(index, &party, now, &scales, radius);
            let choice = Choice {
                field: super::travel::field_rect(self.field),
                tile,
                gutter: self.layout.repositories[avatar.repository].gutter,
                gap,
                radius,
                capacity,
                beams: &beams,
                peers: &peers,
                future: &future,
                occupied: &occupied,
                clearance: &clearance,
                repository: avatar.repository,
                text,
                scale: name_scale,
                envelope,
                cells: &cells,
                speed: self.avatar_max_speed,
            };
            let mut chosen = if let Some(slot) =
                avatar.slot.as_ref().filter(|_| avatar.transmitting)
            {
                choice.assess(slot.dock).0
            } else {
                choice.pick(
                    avatar.slot.as_ref(),
                    avatar.slot_origin.map(|(repository, dock, point)| {
                        self.layout.repositories[repository]
                            .gutter
                            .map_or(point, |t| dock.on_network(t))
                    }),
                    commit,
                    plan.docks.get(&(commit, index)).copied(),
                    !plan.replanning || commit < plan.frozen,
                )
            };
            chosen.commit = commit;
            self.record_placement(&chosen);
            let capacity = self.seat_party(&chosen, &party, &cells, plan);
            let ring_centre =
                chosen.point + ring_offset(capacity, chosen.dock.edge, radius);
            occupied.push(disk(ring_centre, group_radius(capacity, radius)));
        }
        self.capacities = self.compute_capacities();
    }

    /// Re-projects idle avatars onto their moving tiles and returns the space they hold.
    fn hold_idle(
        &mut self,
        now: super::Time,
        capacities: &[u8],
        radius: f32,
    ) -> Vec<Rect> {
        let mut occupied = Vec::new();
        for index in 0..self.avatars.len() {
            let avatar = &mut self.avatars[index];
            if now - avatar.last_active < super::AVATAR_IDLE {
                continue;
            }
            let alone =
                avatar.group.is_some_and(|g| g.lead == index) && capacities[index] < 2;
            let Some(slot) = &mut avatar.slot else {
                continue;
            };
            // An author whose group has gone walks back from its seat to its dock.
            if alone {
                slot.point -= slot.offset;
                slot.offset = Vec2::ZERO;
            }
            if let Some(tile) = self.layout.repositories[avatar.repository].gutter {
                slot.point = slot.dock.on_network(tile) + slot.offset;
            }
            let point = slot.point;
            if avatar.slot_presence > 0.0 || avatar.presence > 0.0 {
                occupied.extend(self.held_space(index, point, capacities, radius));
            }
        }
        occupied
    }

    /// Seats an author's group round its chosen dock and returns its seat count; a
    /// co-author from an earlier commit leaves the group, staying put, if the author has
    /// moved to another dock.
    fn seat_party(
        &mut self,
        chosen: &Placement,
        party: &[usize],
        cells: &[Rect],
        plan: &super::travel::Plan,
    ) -> u8 {
        let radius = self.avatar_radius();
        let author = party[0];
        let mut seated = Vec::with_capacity(party.len());
        for &member in party {
            let avatar = &self.avatars[member];
            if avatar.commit != chosen.commit
                && avatar.slot.as_ref().is_some_and(|s| s.dock != chosen.dock)
            {
                self.avatars[member].group = None;
            } else {
                let seat = avatar
                    .group
                    .filter(|_| member != author)
                    .map_or(0, |g| g.seat);
                seated.push((member, seat));
            }
        }
        let capacity = seated.iter().map(|&(_, seat)| seat + 1).max().unwrap_or(1);
        for (member, seat) in seated {
            let offset = seat_offset(seat, capacity, chosen.dock.edge, radius);
            let place = Placement {
                point: chosen.point + offset,
                offset,
                ..chosen.clone()
            };
            let avatar = &mut self.avatars[member];
            if let Some(previous) = avatar.slot.as_ref()
                && (previous.dock != place.dock
                    || previous.centre().distance(chosen.point) > 4.0 * radius)
                && avatar.presence > 0.0
                && (!avatar.travelling || avatar.dock_walk.is_some())
                && !plan.travelling(member, self.tick)
                && let Some(tile) = self.layout.repositories[avatar.repository].gutter
            {
                let from = field_point(avatar.position);
                let old_centre = previous.centre();
                let dock = if super::travel::on_network(old_centre, cells) {
                    old_centre
                } else {
                    previous.dock.on_network(tile)
                };
                avatar.dock_walk = super::travel::DockWalk::new(
                    self.tick,
                    [from, dock],
                    [chosen.point, place.point],
                    cells,
                    self.avatar_max_speed,
                );
            }
            if avatar
                .slot
                .as_ref()
                .is_none_or(|previous| previous.dock != place.dock)
            {
                avatar.dock_since = self.tick;
            }
            avatar.slot = Some(place);
        }
        capacity
    }

    /// Active avatars, most recently active first and each author before its co-authors.
    fn placing_order(&self, now: super::Time) -> Vec<usize> {
        let mut order: Vec<_> = (0..self.avatars.len())
            .filter(|&i| now - self.avatars[i].last_active < super::AVATAR_IDLE)
            .collect();
        let rank = |avatar: &super::Avatar| avatar.group.map_or(0, |g| g.seat);
        order.sort_by(|&a, &b| {
            self.avatars[b]
                .last_active
                .total_cmp(self.avatars[a].last_active)
                .then(rank(&self.avatars[a]).cmp(&rank(&self.avatars[b])))
                .then(a.cmp(&b))
        });
        order
    }

    fn record_placement(&mut self, chosen: &Placement) {
        let record = self.slot_records.entry(chosen.commit).or_default();
        record.fallback |= chosen.dock.edge != 0;
        record.least_bad |= !chosen.valid();
        record.half_plane_failures += u32::from(!chosen.half_plane);
        record.max_crossings = record.max_crossings.max(chosen.crossings);
        record.max_overlaps = record.max_overlaps.max(chosen.overlaps);
    }

    /// A co-author who has left its group holds its place as the tile moves.
    fn hold_place(&mut self, index: usize, occupied: &mut Vec<Rect>) {
        let radius = self.avatar_radius();
        let avatar = &mut self.avatars[index];
        if let (Some(tile), Some(slot)) = (
            self.layout.repositories[avatar.repository].gutter,
            avatar.slot.as_mut(),
        ) {
            slot.point = slot.dock.on_network(tile) + slot.offset;
            occupied.push(disk(slot.point, radius));
        }
    }
}

impl State {
    pub(super) fn screen(&self, point: Vec3) -> Option<Vec2> {
        let (focus, distance) = self.overview();
        project(focus + view_direction() * distance, focus, &[point])
            .next()
            .flatten()
            .map(|p| p * HALF)
    }

    pub(super) fn overlay_label_bounds(&self, label: &super::Label) -> Option<Rect> {
        Some(super::names::top_left_rect(
            self.screen(label.position)?,
            &label.text,
            label.kind.font_scale(),
            self.geometry.label_height,
        ))
    }

    fn name_radius(&self) -> f32 {
        self.avatar_radius() / self.field_growth
    }

    fn project_forecast(&self, mut points: Vec<Vec2>) -> Vec<Vec2> {
        if self.field_growth > 1.0 {
            points.retain_mut(|point| {
                let Some(screen) = self.screen(world(*point)) else {
                    return false;
                };
                *point = screen;
                true
            });
        }
        points
    }

    fn name_size(&self, text: &str) -> Vec2 {
        super::names::rect(
            Vec2::ZERO,
            text,
            super::LabelKind::Avatar.font_scale(),
            self.geometry.label_height,
        )
        .size()
    }

    /// The avatar's current position, or its destination seat while settling.
    fn standing(&self, index: usize) -> Vec3 {
        let avatar = &self.avatars[index];
        let shift = avatar
            .group
            .filter(|group| self.in_party(index, group.lead))
            .map_or(Vec3::ZERO, |group| self.avatars[group.lead].ring_shift());
        avatar
            .slot
            .as_ref()
            .filter(|_| avatar.settle.is_some())
            .map_or(avatar.position, |slot| slot.world() + shift)
    }

    /// Projected name anchors, shifted one avatar radius inward for members on a ring.
    fn name_bodies(&self) -> Vec<Option<Vec2>> {
        let radius = self.avatar_radius();
        let capacities = &self.capacities;
        (0..self.avatars.len())
            .map(|index| {
                let avatar = &self.avatars[index];
                let standing = self.standing(index);
                let lead = avatar.group.map_or(index, |g| g.lead);
                let anchor = avatar
                    .slot
                    .as_ref()
                    .filter(|s| {
                        capacities[lead] > 1
                            && self.in_party(index, lead)
                            && s.offset != Vec2::ZERO
                            && self.ring_of(index).is_some_and(|(centre, rim)| {
                                avatar.position.distance(centre) < rim
                            })
                    })
                    .map(|slot| {
                        let capacity = capacities[lead];
                        let centre = slot.centre()
                            + ring_offset(capacity, slot.dock.edge, radius);
                        let reach = group_radius(capacity, radius) - radius;
                        let toward = (slot.point - centre).normalize_or_zero();
                        standing + world(centre + toward * reach) - world(slot.point)
                    });
                self.screen(anchor.unwrap_or(standing))
            })
            .collect()
    }
    /// For each avatar in a ring, the screen angle from the ring's centre through it and
    /// the ring's screen disc, which its name keeps off.
    fn name_preferences(&self) -> Vec<Option<(f32, Rect)>> {
        (0..self.avatars.len())
            .map(|index| {
                let (centre, rim) = self.ring_of(index)?;
                if self.avatars[index].position.distance(centre) >= rim {
                    return None;
                }
                let from = self.screen(centre)?;
                let edge = self.screen(centre + Vec3::X * rim)?;
                let to = self.screen(self.standing(index))?;
                (to != from)
                    .then(|| ((to - from).to_angle(), disk(from, from.distance(edge))))
            })
            .collect()
    }
    fn name_rays(
        &self,
        bodies: &[Option<Vec2>],
        travel: &super::travel::Plan,
    ) -> Vec<(Vec2, Vec2)> {
        self.pulses
            .iter()
            .filter(|pulse| {
                super::tick_time(self.tick) >= pulse.start
                    && super::tick_time(self.tick) < pulse.end
                    && !travel.travelling(pulse.author, self.tick)
            })
            .flat_map(|pulse| {
                let author = pulse.author;
                pulse.targets.iter().filter_map(move |&(target, ..)| {
                    bodies[author]?;
                    let to = self.point(target)?;
                    Some((self.screen(self.beam_start(author, to))?, self.screen(to)?))
                })
            })
            .collect()
    }
    /// What one avatar's name candidates are scored against.
    fn label_space(
        &self,
        index: usize,
        body: Vec2,
        size: Vec2,
        bodies: &[Option<Vec2>],
        obstacles: &[(usize, Rect)],
        rays: &[(Vec2, Vec2)],
    ) -> LabelSpace {
        let radius = self.name_radius();
        let margin = 4.0 / 3.0;
        // Every candidate rect lies within this reach, so nothing outside it can
        // touch one; the skipped terms would each have added exactly zero.
        let reach = Rect::from_center_half_size(
            body,
            Vec2::splat(size.length() * 2.0 + radius + margin + 1.0),
        );
        let near = |rect: Rect| !reach.intersect(rect).is_empty();
        let body_reach = reach.inflate(radius + margin + 1.0);
        // A group's names all attach round its ring, so members' anchors do not
        // compete; their names still keep apart through the joint assignment.
        let lead = |i: usize| self.avatars[i].group.map(|g| g.lead);
        LabelSpace {
            foreign: bodies
                .iter()
                .enumerate()
                .filter(|&(other, _)| {
                    other != index
                        && self.avatars[other].presence > 0.0
                        && (lead(other).is_none() || lead(other) != lead(index))
                })
                .filter_map(|(_, &p)| p)
                .filter(|&p| body_reach.contains(p))
                .collect(),
            obstacles: obstacles
                .iter()
                .copied()
                .filter(|&(_, r)| near(r))
                .collect(),
            rays: rays
                .iter()
                .copied()
                .filter(|&(a, b)| near(Rect::from_corners(a, b).inflate(1.0)))
                .collect(),
            repository: self.avatars[index].repository,
            preferred: None,
            radius,
            margin,
        }
    }
    fn label_domains(
        &self,
        bodies: &[Option<Vec2>],
        travel: &super::travel::Plan,
        dt: f32,
        texts: &[Rect],
        displayed: &[String],
    ) -> (Vec<usize>, Vec<Vec<labels::Candidate>>) {
        let mut obstacles = tile_obstacles(self);
        if self.field_growth > 1.0 {
            for (_, rect) in &mut obstacles {
                *rect = Rect::from_corners(
                    self.screen(world(rect.min)).expect("visible tile"),
                    self.screen(world(rect.max)).expect("visible tile"),
                );
            }
        }
        obstacles.extend(texts.iter().map(|&rect| (usize::MAX, rect)));
        let rays = self.name_rays(bodies, travel);
        let preferred = self.name_preferences();
        let mut authors = Vec::new();
        let mut domains = Vec::new();
        for (index, avatar) in self
            .avatars
            .iter()
            .enumerate()
            .filter(|(_, a)| a.presence > 0.0)
        {
            let Some(body) = bodies[index] else { continue };
            let size = self.name_size(&displayed[index]);
            let space = LabelSpace {
                preferred: preferred[index],
                ..self.label_space(index, body, size, bodies, &obstacles, &rays)
            };
            let previous = avatar.name_visibility.placement.as_ref();
            let velocity = previous
                .map_or(Vec2::ZERO, |p| (body - p.body) / dt.max(1e-6))
                .clamp_length_max(480.0);
            let future = travel.forecast(index, self.tick).map_or_else(
                || {
                    (1..=30_u8)
                        .map(|step| {
                            (body
                                + velocity
                                    * (f32::from(step) * 4.0 * super::tick_seconds())
                                        .min(1.0))
                            .clamp(
                                Vec2::new(
                                    -HALF.x + space.radius,
                                    super::caption::BOTTOM + space.radius,
                                ),
                                Vec2::new(
                                    HALF.x - space.radius,
                                    super::caption::TOP - space.radius,
                                ),
                            )
                        })
                        .collect()
                },
                |points| self.project_forecast(points),
            );
            let rotation = Rotation {
                body,
                size,
                previous,
                future: &future,
                future_bounds: labels::bounds(&future),
            };
            let mut targets: Vec<_> = (0..32_u8)
                .map(|i| f32::from(i) * std::f32::consts::TAU / 32.0)
                .collect();
            let corner = size * 0.5
                + Vec2::splat(
                    (space.radius + space.margin) * std::f32::consts::FRAC_1_SQRT_2,
                );
            targets.extend(
                [
                    corner,
                    Vec2::new(-corner.x, corner.y),
                    -corner,
                    Vec2::new(corner.x, -corner.y),
                ]
                .map(Vec2::to_angle),
            );
            targets.extend(previous.map(|p| p.motion.goal));
            targets.extend(preferred[index].map(|(angle, _)| angle));
            let scored = targets
                .into_iter()
                .map(|target| space.candidate(target, &rotation))
                .collect();
            let candidates = eligible_angles(scored);
            authors.push(index);
            domains.push(candidates);
        }
        (authors, domains)
    }
    fn assign_names(
        &self,
        dt: f32,
        travel: &super::travel::Plan,
        texts: &[Rect],
    ) -> NameAssignment {
        let bodies = self.name_bodies();
        let mut displayed: Vec<_> = self
            .avatars
            .iter()
            .map(|a| {
                a.name_visibility
                    .placement
                    .as_ref()
                    .map_or_else(|| a.name.clone(), |p| p.text.clone())
            })
            .collect();
        let (mut authors, mut domains) =
            self.label_domains(&bodies, travel, dt, texts, &displayed);
        let mut assignment = labels::solve(&domains);
        for limit in [24, 16, 10] {
            let fits = assignment.clear
                && domains
                    .iter()
                    .zip(&assignment.choices)
                    .all(|(domain, &choice)| {
                        super::caption::outside(domain[choice].rect, 0.5) <= 0.001
                    });
            if fits {
                break;
            }
            let rectangles: Vec<_> = domains
                .iter()
                .zip(&assignment.choices)
                .map(|(domain, &choice)| domain[choice].rect)
                .collect();
            let mut shortened = false;
            for (node, &index) in authors.iter().enumerate() {
                if displayed[index].chars().count() <= limit {
                    continue;
                }
                let candidate = domains[node][assignment.choices[node]];
                if candidate.text_pairs > 0
                    || super::caption::outside(candidate.rect, 0.5) > 0.001
                    || rectangles.iter().enumerate().any(|(other, rect)| {
                        other != node && !candidate.rect.intersect(*rect).is_empty()
                    })
                {
                    displayed[index] = super::names::elide(&displayed[index], limit);
                    shortened = true;
                }
            }
            if !shortened {
                continue;
            }
            (authors, domains) =
                self.label_domains(&bodies, travel, dt, texts, &displayed);
            assignment = labels::solve(&domains);
        }
        NameAssignment {
            authors,
            domains,
            assignment,
            displayed,
        }
    }

    pub(super) fn place_slot_names(
        &mut self,
        dt: f32,
        travel: &super::travel::Plan,
        texts: &[Rect],
    ) {
        for avatar in self.avatars.iter_mut().filter(|a| a.presence <= 0.0) {
            avatar.name_visibility.placement = None;
            avatar.name_visibility.visible = false;
            avatar.name_visibility.opacity = 0.0;
            avatar.name_visibility.full_clear_since = None;
        }
        let bodies = self.name_bodies();
        #[cfg(test)]
        let assigned = name_oracle::assign(self, dt, travel, texts);
        #[cfg(not(test))]
        let assigned = self.assign_names(dt, travel, texts);
        let NameAssignment {
            authors,
            domains,
            assignment,
            displayed,
        } = assigned;
        for ((index, candidates), choice) in
            authors.into_iter().zip(domains).zip(assignment.choices)
        {
            let selected = candidates[choice];
            let body = bodies[index].expect("projected visible avatar");
            let size = self.name_size(&displayed[index]);
            let distance = self.name_radius() + 4.0 / 3.0;
            // A body that jumps further than its name's width has no on-screen motion to follow.
            let previous = self.avatars[index]
                .name_visibility
                .placement
                .as_ref()
                .filter(|placement| placement.body.distance(body) <= size.x);
            let (angle, angular_velocity, motion) = previous.map_or_else(
                || {
                    (
                        selected.angle,
                        0.0,
                        super::names::Motion {
                            goal: selected.angle,
                            ..Default::default()
                        },
                    )
                },
                |previous| {
                    let rect = labels::tangent_rect(
                        body,
                        size,
                        distance,
                        previous.motion.goal,
                    );
                    let covered = self.name_occlusion(index, rect, &bodies, texts);
                    let gain = (covered
                        - self.name_occlusion(index, selected.rect, &bodies, texts))
                        / covered.max(1e-6);
                    labels::move_name(previous, selected.angle, gain, self.tick, dt)
                },
            );
            let rect = labels::tangent_rect(body, size, distance, angle);
            let low = Vec2::new(-HALF.x + 0.5, super::caption::BOTTOM + 0.5);
            let high = Vec2::new(HALF.x - 0.5, super::caption::TOP - 0.5);
            let offset =
                (low - rect.min).max(Vec2::ZERO) + (high - rect.max).min(Vec2::ZERO);
            let placement = super::names::Placement {
                point: Vec2::new(rect.min.x, rect.max.y) + offset,
                body: bodies[index].expect("projected visible avatar"),
                text: displayed[index].clone(),
                scale: super::LabelKind::Avatar.font_scale(),
                angle,
                angular_velocity,
                motion,
                commit: self.avatars[index].commit,
            };
            let visibility = &mut self.avatars[index].name_visibility;
            visibility.visible = true;
            let wordmark =
                super::caption::frame_text_rects()[0].inflate(super::caption::BUFFER);
            visibility.opacity = f32::from(
                placement
                    .bounds(self.geometry.label_height)
                    .intersect(wordmark)
                    .is_empty(),
            );
            visibility.placement = Some(placement);
            visibility.joint_exhausted = assignment.exhausted;
            visibility.joint_clear = assignment.clear;
        }
        self.recover_name_lengths(&bodies, texts);
        self.dim_names_over_held_leads();
    }

    fn dim_names_over_held_leads(&mut self) {
        for member in 0..self.avatars.len() {
            let avatar = &self.avatars[member];
            let Some(group) = avatar.group.filter(|group| group.lead != member) else {
                continue;
            };
            let lead = &self.avatars[group.lead];
            let overlap = lead.transmitting
                && self.in_party(member, group.lead)
                && avatar
                    .name_visibility
                    .placement
                    .as_ref()
                    .zip(lead.name_visibility.placement.as_ref())
                    .is_some_and(|(member, lead)| {
                        !member
                            .bounds(self.geometry.label_height)
                            .intersect(lead.bounds(self.geometry.label_height))
                            .is_empty()
                    });
            if overlap {
                self.avatars[member].name_visibility.opacity *= 0.5;
            }
        }
    }

    fn recover_name_lengths(&mut self, bodies: &[Option<Vec2>], texts: &[Rect]) {
        for (index, body) in bodies.iter().enumerate() {
            let avatar = &self.avatars[index];
            let full = (*body)
                .zip(avatar.name_visibility.placement.as_ref())
                .filter(|(_, placement)| {
                    avatar.presence > 0.0 && placement.text != avatar.name
                })
                .map(|(body, placement)| {
                    let size = self.name_size(&avatar.name);
                    labels::tangent_rect(
                        body,
                        size,
                        self.name_radius() + 4.0 / 3.0,
                        placement.angle,
                    )
                })
                .filter(|rect| {
                    super::caption::outside(*rect, 0.5) <= 0.001
                        && self.name_occlusion(index, *rect, bodies, texts) <= 0.0
                });
            let avatar = &mut self.avatars[index];
            let Some(rect) = full else {
                avatar.name_visibility.full_clear_since = None;
                continue;
            };
            let since = avatar
                .name_visibility
                .full_clear_since
                .get_or_insert(self.tick);
            if self.tick.saturating_sub(*since) >= labels::DWELL_TICKS {
                let placement = avatar.name_visibility.placement.as_mut().unwrap();
                placement.point = Vec2::new(rect.min.x, rect.max.y);
                placement.text.clone_from(&avatar.name);
                avatar.name_visibility.full_clear_since = None;
            }
        }
    }

    fn name_occlusion(
        &self,
        index: usize,
        rect: Rect,
        bodies: &[Option<Vec2>],
        texts: &[Rect],
    ) -> f32 {
        let area = rect.size().element_product().max(1.0);
        let overlap = |other: Rect| {
            rect.intersect(other)
                .size()
                .max(Vec2::ZERO)
                .element_product()
        };
        let frame = Rect::from_corners(
            Vec2::new(-HALF.x, super::caption::BOTTOM),
            Vec2::new(HALF.x, super::caption::TOP),
        );
        let mut covered = area - overlap(frame);
        covered += texts.iter().map(|&r| overlap(r)).sum::<f32>();
        for (other, avatar) in self.avatars.iter().enumerate() {
            if other == index || avatar.presence <= 0.0 {
                continue;
            }
            if let Some(body) = bodies[other] {
                covered += overlap(disk(body, self.name_radius()));
            }
            if let Some(name) = &avatar.name_visibility.placement {
                covered += overlap(name.bounds(self.geometry.label_height));
            }
        }
        (covered / area).clamp(0.0, 1.0)
    }
}
fn eligible_angles(
    scored: Vec<(labels::Candidate, [f32; 4])>,
) -> Vec<labels::Candidate> {
    let best = scored
        .iter()
        .map(|(_, c)| *c)
        .min_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])))
        .expect("continuous angle samples");
    let safe_forecast = scored.iter().any(|(_, c)| {
        c[0].total_cmp(&best[0]).is_eq()
            && c[1].total_cmp(&best[1]).is_eq()
            && c[2] <= 0.0001
    });
    let mut candidates: Vec<_> = scored
        .into_iter()
        .filter(|(_, c)| {
            c[0].total_cmp(&best[0]).is_eq()
                && c[1].total_cmp(&best[1]).is_eq()
                && (!safe_forecast || c[2] <= 0.0001)
        })
        .map(|(candidate, _)| candidate)
        .collect();
    candidates.sort_by(|a, b| {
        a.text_pairs
            .cmp(&b.text_pairs)
            .then(a.text_area.total_cmp(&b.text_area))
            .then(a.soft.total_cmp(&b.soft))
            .then(a.angle.total_cmp(&b.angle))
    });
    candidates
        .dedup_by(|a, b| a.angle.to_bits() == b.angle.to_bits() && a.rect == b.rect);
    candidates
}
struct Rotation<'a> {
    body: Vec2,
    size: Vec2,
    previous: Option<&'a super::names::Placement>,
    future: &'a [Vec2],
    future_bounds: Rect,
}
struct NameAssignment {
    authors: Vec<usize>,
    domains: Vec<Vec<labels::Candidate>>,
    assignment: labels::Assignment,
    displayed: Vec<String>,
}

/// Other visible bodies outside the avatar's ring, and the obstacles and rays near
/// enough to touch a candidate.
struct LabelSpace {
    foreign: Vec<Vec2>,
    obstacles: Vec<(usize, Rect)>,
    rays: Vec<(Vec2, Vec2)>,
    repository: usize,
    /// For an avatar in a ring, the screen angle from the ring's centre through it,
    /// where its name costs least, and the ring's screen disc.
    preferred: Option<(f32, Rect)>,
    radius: f32,
    margin: f32,
}
impl LabelSpace {
    fn candidate(
        &self,
        target: f32,
        motion: &Rotation<'_>,
    ) -> (labels::Candidate, [f32; 4]) {
        let distance = self.radius + self.margin;
        let rect = labels::tangent_rect(motion.body, motion.size, distance, target);
        let cost = self.cost(rect);
        let clip = labels::resting_clip(
            target,
            motion.size,
            distance,
            motion.future,
            motion.future_bounds,
        );
        let text = self.text_cost(rect);
        let soft = cost[2] * 2.0
            + clip * 1e8
            + cost[0]
            + cost[1] * 1e6
            + motion.previous.map_or(0.0, |p| {
                labels::difference(target, p.angle).abs() * 55_000.0
            })
            + self.preferred.map_or(0.0, |(angle, _)| {
                labels::difference(target, angle).abs() * NEAREST_NAME_WEIGHT
            });
        (
            labels::Candidate {
                angle: target,
                rect,
                text_pairs: text.0,
                text_area: text.1,
                soft,
            },
            [cost[0], cost[1], clip, soft],
        )
    }
    fn text_cost(&self, rect: Rect) -> (usize, f32) {
        self.obstacles
            .iter()
            .filter(|(owner, _)| *owner == usize::MAX)
            .map(|(_, r)| rect.intersect(*r))
            .filter(|r| !r.is_empty())
            .fold((0, 0.0), |(pairs, area), r| {
                (pairs + 1, area + r.size().element_product())
            })
    }
    fn cost(&self, rect: Rect) -> [f32; 3] {
        let foreign = self.foreign.iter().copied();
        let conflicts = count(
            foreign
                .clone()
                .filter(|p| {
                    p.distance(p.clamp(rect.min, rect.max))
                        <= self.radius + self.margin + 0.001
                })
                .count(),
        );
        let ownership = foreign
            .clone()
            .map(|p| {
                (self.radius + self.margin + 0.1
                    - p.distance(p.clamp(rect.min, rect.max)))
                .max(0.0)
            })
            .sum::<f32>();
        let area = |other: Rect| {
            rect.intersect(other)
                .size()
                .max(Vec2::ZERO)
                .element_product()
        };
        let outside = super::caption::outside(rect, 0.5);
        [
            outside * 1e8,
            conflicts,
            ownership * 1e6
                + foreign.map(|p| area(disk(p, self.radius))).sum::<f32>() * 100.0
                + self.preferred.map_or(0.0, |(_, ring)| area(ring) * 2_000.0)
                + self
                    .obstacles
                    .iter()
                    .filter(|(r, _)| *r != usize::MAX && *r != self.repository)
                    .map(|(_, r)| area(*r))
                    .sum::<f32>()
                + count(
                    self.rays
                        .iter()
                        .filter(|(a, b)| segment(*a, *b, rect))
                        .count(),
                ) * 20_000.0,
        ]
    }
}
#[cfg(test)]
fn attached_rect(body: Vec2, size: Vec2, distance: f32, attachment: u8) -> Rect {
    labels::tangent_rect(
        body,
        size,
        distance,
        f32::from(attachment) * std::f32::consts::TAU / 8.0,
    )
}
struct Choice<'a> {
    field: Rect,
    tile: Tile,
    gutter: Option<Tile>,
    gap: Vec2,
    radius: f32,
    /// Seats in the avatar's group, whose ring is kept clear of other avatars and the
    /// caption bar and whose members stay in view; 1 for an avatar alone.
    capacity: u8,
    beams: &'a [(usize, Rect)],
    occupied: &'a [Rect],
    clearance: &'a [Rect],
    peers: &'a [(Vec2, u8, Vec2)],
    future: &'a [Rect],
    repository: usize,
    text: String,
    scale: f32,
    envelope: Envelope,
    cells: &'a [Rect],
    speed: f32,
}

impl Choice<'_> {
    fn pick(
        &self,
        previous: Option<&Placement>,
        origin: Option<Vec2>,
        commit: usize,
        planned: Option<Dock>,
        locked: bool,
    ) -> Placement {
        if let Some(parked) = previous {
            let selected = self.assess(parked.dock).0;
            if self.clearance_cost(&selected) > 0.001
                || (parked.commit != commit
                    && self.reserved_cost(selected.point) > 0.001)
            {
                return self.nearest(selected.point, true);
            }
            if parked.commit == commit || selected.valid() {
                return selected;
            }
        }
        if let Some(dock) = planned {
            let selected = self.assess(dock).0;
            if locked
                || (selected.valid() && self.reserved_cost(selected.point) <= 0.001)
            {
                return if self.clearance_cost(&selected) <= 0.001 {
                    selected
                } else {
                    self.nearest(origin.unwrap_or(selected.point), true)
                };
            }
        }
        if let Some(parked) = previous {
            return self.nearest(self.point(parked.dock), true);
        }
        origin.map_or_else(|| self.select(None), |p| self.nearest(p, false))
    }
    fn point(&self, dock: Dock) -> Vec2 {
        self.gutter.map_or_else(
            || dock.point(self.tile, self.gap),
            |tile| dock.on_network(tile),
        )
    }
    fn nearest(&self, point: Vec2, same_tile: bool) -> Placement {
        self.free_docks()
            .into_iter()
            .chain((0..4).flat_map(|edge| {
                (0..=16_u8).map(move |i| Dock {
                    edge,
                    fraction: f32::from(i) / 16.0,
                })
            }))
            .map(|dock| self.assess(dock).0)
            .filter(Placement::valid)
            .map(|slot| {
                let seconds = super::travel::slot_seconds(
                    point, slot.point, self.cells, self.speed,
                );
                let cost = if same_tile {
                    seconds
                } else {
                    self.ray_cost(slot.point)
                        + TRAVEL_WEIGHT * seconds
                        + self.spread_cost(slot.point, slot.dock.edge)
                        + self.spacing_cost(slot.point) * 1_000.0
                };
                (slot, cost)
            })
            .min_by(|a, b| {
                self.clearance_cost(&a.0)
                    .total_cmp(&self.clearance_cost(&b.0))
                    .then_with(|| {
                        if same_tile {
                            self.reserved_cost(a.0.point)
                                .total_cmp(&self.reserved_cost(b.0.point))
                        } else {
                            self.avatar_cost(a.0.point)
                                .total_cmp(&self.avatar_cost(b.0.point))
                        }
                    })
                    .then(a.1.total_cmp(&b.1))
                    .then(
                        a.0.point
                            .distance_squared(point)
                            .total_cmp(&b.0.point.distance_squared(point)),
                    )
            })
            .map_or_else(|| self.select(None), |(slot, _)| slot)
    }
    fn avatar_cost(&self, point: Vec2) -> f32 {
        self.occupied_cost(point) + self.reserved_cost(point)
    }
    fn clearance_cost(&self, slot: &Placement) -> f32 {
        if self.clearance.is_empty() {
            return 0.0;
        }
        let member_radius =
            self.radius * if self.capacity > 1 { MEMBER_SCALE } else { 1.0 };
        (0..self.capacity)
            .flat_map(|seat| {
                let point = slot.point
                    + seat_offset(seat, self.capacity, slot.dock.edge, self.radius);
                self.clearance.iter().map(move |disc| {
                    (member_radius + disc.half_size().x
                        - point.distance(disc.center())
                        - 0.001)
                        .max(0.0)
                })
            })
            .sum()
    }
    fn occupied_cost(&self, point: Vec2) -> f32 {
        self.occupied
            .iter()
            .map(|r| {
                (group_radius(self.capacity, self.radius) + r.half_size().x
                    - point.distance(r.center()))
                .max(0.0)
            })
            .sum()
    }
    fn reserved_cost(&self, point: Vec2) -> f32 {
        self.future
            .iter()
            .map(|r| (point - r.min).min(r.max - point).min_element().max(0.0))
            .sum::<f32>()
    }
    fn spread_cost(&self, point: Vec2, edge: u8) -> f32 {
        self.peers
            .iter()
            .map(|(other, side, centre)| {
                let separation = (point - *centre)
                    .normalize_or_zero()
                    .dot((*other - *centre).normalize_or_zero())
                    .clamp(-1.0, 1.0)
                    .acos();
                1_000.0 * (1.0 - separation / std::f32::consts::PI)
                    + if edge == *side { 1_000.0 } else { 0.0 }
            })
            .sum()
    }
    fn ray_cost(&self, point: Vec2) -> f32 {
        let size = self.tile.high - self.tile.low;
        self.envelope
            .rays
            .iter()
            .map(|p| point.distance((self.tile.low + *p * size) * HALF))
            .sum()
    }
    fn spacing_cost(&self, point: Vec2) -> f32 {
        let clearance = self.radius * 3.0 + self.width().min(80.0);
        self.occupied
            .iter()
            .map(|r| (clearance - point.distance(r.center())).max(0.0))
            .sum()
    }
    fn assess(&self, dock: Dock) -> (Placement, f32) {
        let point = self.point(dock);
        let centre = point + ring_offset(self.capacity, dock.edge, self.radius);
        let transition = if self.capacity > 1 {
            self.radius * (1.0 - MEMBER_SCALE)
        } else {
            0.0
        };
        let ring = group_radius(self.capacity, self.radius) + transition;
        let corners = self.envelope.corners(self.tile);
        let span = fan(point, &corners);
        let (low, high) = corners
            .iter()
            .fold((point.min(centre), point.max(centre)), |(low, high), &p| {
                (low.min(p), high.max(p))
            });
        let crossings = self
            .beams
            .iter()
            .filter(|(repo, rect)| {
                *repo != usize::MAX
                    && *repo != self.repository
                    && low.cmple(rect.max).all()
                    && high.cmpge(rect.min).all()
                    && (swept_crosses(point, &corners, rect.inflate(0.02))
                        || swept_crosses(centre, &corners, rect.inflate(0.02)))
            })
            .count();
        let overlaps = usize::from(
            !centre.cmpge(self.field.min + Vec2::splat(ring)).all()
                || !centre.cmple(self.field.max - Vec2::splat(ring)).all(),
        );
        (
            Placement {
                dock,
                commit: usize::MAX,
                point,
                offset: Vec2::ZERO,
                text: self.text.clone(),
                half_plane: self.envelope.samples > 0
                    && span < std::f32::consts::PI - 1e-5,
                crossings,
                overlaps,
                missing_tile: false,
            },
            span,
        )
    }

    fn width(&self) -> f32 {
        8.0 + 8.6 * count(self.text.chars().count()) * self.scale
    }

    fn select(&self, previous: Option<Dock>) -> Placement {
        if let Some((p, _)) = previous
            .map(|dock| self.assess(dock))
            .filter(|(p, _)| p.valid())
        {
            return p;
        }
        let low = self.tile.low.x * HALF.x + self.radius;
        let high =
            (self.tile.high.x * HALF.x - self.radius - self.width() - 4.0).max(low);
        let ideal = ((self.tile.low.x
            + (self.tile.high.x - self.tile.low.x) * self.envelope.mean)
            * HALF.x)
            .clamp(low, high);
        let fraction = (ideal / HALF.x - self.tile.low.x)
            / (self.tile.high.x - self.tile.low.x).max(1e-8);
        let mut docks = vec![Dock { edge: 0, fraction }];
        for edge in 1..4 {
            for fraction in [0.25, 0.5, 0.75] {
                docks.push(Dock { edge, fraction });
            }
        }
        let top = self.assess(docks[0]).0;
        if top.half_plane && top.crossings == 0 {
            for step in 0_u8..16 {
                docks.push(Dock {
                    edge: 0,
                    fraction: f32::from(step) / 15.0,
                });
            }
        }
        docks.extend((0..4).flat_map(|edge| {
            (0..=32_u8).map(move |i| Dock {
                edge,
                fraction: f32::from(i) / 32.0,
            })
        }));
        docks.extend(self.free_docks());
        self.best(docks.into_iter())
    }

    fn best(&self, docks: impl Iterator<Item = Dock>) -> Placement {
        let crowding = |p: &Placement| {
            self.ray_cost(p.point)
                + self.spread_cost(p.point, p.dock.edge)
                + self.spacing_cost(p.point) * 1_000.0
        };
        docks
            .enumerate()
            .map(|(id, dock)| {
                let (p, span) = self.assess(dock);
                let rank = match (dock.edge, id) {
                    (0, 0) => 0,
                    (0, _) => 1,
                    _ => 2,
                };
                let cost = (p.overlaps, p.crossings, usize::from(!p.half_plane), rank);
                (p, cost, span, id)
            })
            .min_by(|a, b| {
                (a.1.0, a.1.1, a.1.2)
                    .cmp(&(b.1.0, b.1.1, b.1.2))
                    .then(
                        self.clearance_cost(&a.0)
                            .total_cmp(&self.clearance_cost(&b.0)),
                    )
                    .then(
                        self.avatar_cost(a.0.point)
                            .total_cmp(&self.avatar_cost(b.0.point)),
                    )
                    .then(crowding(&a.0).total_cmp(&crowding(&b.0)))
                    .then(a.1.3.cmp(&b.1.3))
                    .then(a.2.total_cmp(&b.2))
                    .then(a.3.cmp(&b.3))
            })
            .expect("at least one candidate dock")
            .0
    }

    fn free_docks(&self) -> Vec<Dock> {
        let mut docks = Vec::new();
        let tile = self.gutter.unwrap_or(self.tile);
        for edge in 0..4 {
            let axis = usize::from(edge % 2 == 1);
            let cross = 1 - axis;
            let origin = self.point(Dock {
                edge,
                fraction: 0.0,
            });
            if origin[cross] - self.radius < self.field.min[cross]
                || origin[cross] + self.radius > self.field.max[cross]
            {
                continue;
            }
            let low = (tile.low * HALF)[axis].max(self.field.min[axis] + self.radius);
            let high = (tile.high * HALF)[axis].min(self.field.max[axis] - self.radius);
            if low > high {
                continue;
            }
            let mut blocked: Vec<_> = self
                .occupied
                .iter()
                .map(|r| (r, self.radius))
                .chain(self.future.iter().map(|r| (r, 0.0)))
                .filter(|(r, margin)| {
                    origin[cross] + margin > r.min[cross]
                        && origin[cross] - margin < r.max[cross]
                })
                .map(|(r, margin)| (r.min[axis] - margin, r.max[axis] + margin))
                .collect();
            blocked.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
            let mut cursor = low;
            for (start, end) in blocked {
                if end < cursor || start > high {
                    continue;
                }
                if start >= cursor {
                    docks.extend(self.interval_docks(edge, cursor, start.min(high)));
                }
                cursor = cursor.max(end);
                if cursor > high {
                    break;
                }
            }
            if cursor <= high {
                docks.extend(self.interval_docks(edge, cursor, high));
            }
        }
        docks
    }

    fn interval_docks(&self, edge: u8, low: f32, high: f32) -> [Dock; 3] {
        let axis = usize::from(edge % 2 == 1);
        let tile = self.gutter.unwrap_or(self.tile);
        let origin = (tile.low * HALF)[axis];
        let span = ((tile.high - tile.low) * HALF)[axis];
        let inset = ((high - low) * 0.25).min(0.001);
        [low + inset, low.midpoint(high), high - inset].map(|value| Dock {
            edge,
            fraction: (value - origin) / span,
        })
    }
}

impl State {
    pub(super) fn avatar_label(&self, avatar: &super::Avatar) -> super::Label {
        let fade = 2.0 * avatar.settled - 1.0;
        if let Some(name) = &avatar.name_visibility.placement {
            let point = avatar.leaving_name.filter(|_| fade < 0.0);
            let shown = if point.is_some() {
                -fade
            } else {
                fade.max(0.0)
            };
            let (focus, distance) = self.overview();
            let reference = (distance - MIN_AVATAR_HEIGHT)
                / (framing::DISTANCE - MIN_AVATAR_HEIGHT);
            return super::Label {
                repository: None,
                position: world(point.unwrap_or(name.point) * reference) + focus,
                text: name.text.clone(),
                kind: super::LabelKind::Avatar,
                scale: name.scale,
                opacity: avatar.name_visibility.opacity * avatar.presence * shown,
                priority: avatar.activity,
            };
        }
        super::Label {
            repository: None,
            position: avatar.position,
            text: avatar
                .slot
                .as_ref()
                .map_or_else(|| avatar.name.clone(), |s| s.text.clone()),
            kind: super::LabelKind::Avatar,
            scale: super::LabelKind::Avatar.font_scale(),
            opacity: avatar.name_visibility.opacity
                * avatar.presence
                * f32::from(u8::from(avatar.name_visibility.visible)),
            priority: avatar.activity,
        }
    }
}

fn swept_crosses(point: Vec2, corners: &[Vec2; 4], rect: Rect) -> bool {
    if rect.min.cmpge(rect.max).any() {
        return false;
    }
    if corners.iter().any(|&p| segment(point, p, rect)) {
        return true;
    }
    let rectangle = [
        rect.min,
        Vec2::new(rect.max.x, rect.min.y),
        rect.max,
        Vec2::new(rect.min.x, rect.max.y),
    ];
    corners
        .iter()
        .zip(corners.iter().cycle().skip(1))
        .take(4)
        .any(|(&a, &b)| {
            if (a - point).perp_dot(b - point).abs() < 1e-6 {
                return false;
            }
            rectangle.iter().any(|&p| {
                let signs = [
                    (a - point).perp_dot(p - point),
                    (b - a).perp_dot(p - a),
                    (point - b).perp_dot(p - b),
                ];
                signs.iter().all(|&v| v >= -1e-5) || signs.iter().all(|&v| v <= 1e-5)
            }) || segment(a, b, rect)
        })
}

pub(super) fn report(state: &State) -> serde_json::Value {
    serde_json::json!({
        "definitions": "Parked slots use a swept touched-file AABB in tile-relative projected coordinates, including appearing/departing files and aggregated endpoints, sampled at canonical 60 Hz throughout live service. Beam fans avoid foreign tiles; bodies and names prefer projected content rectangles with a two-pixel margin, allowing edge overlap. Fallback means any non-top dock; least-bad means any infeasible observation, including a missing birth tile. Counts are unique commits, even when service observations repeat. Moving beams remain a Step 4 diagnostic, not a parked-slot failure.",
        "commits": state.slot_records.len(),
        "fallback_commits": state.slot_records.values().filter(|r| r.fallback).count(),
        "least_bad_commits": state.slot_records.values().filter(|r| r.least_bad).count(),
        "missing_tile_commits": state.slot_records.values().filter(|r| r.missing_tile).count(),
        "per_commit": state.slot_records,
    })
}

fn touched_mean(points: &[Vec3], tile: Tile) -> Option<f32> {
    if points.is_empty() {
        return None;
    }
    let mean = project(view_direction() * framing::DISTANCE, Vec3::ZERO, points)
        .flatten()
        .map(|p| p.x)
        .sum::<f32>()
        / count(points.len());
    Some((mean - tile.low.x) / (tile.high.x - tile.low.x).max(1e-8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{contributors::Mappings, model::Options, theme::Theme};

    #[test]
    fn replanning_keeps_a_new_dock_for_the_whole_commit() {
        let chooser = choice(&[], &[]);
        let mut parked = chooser.select(None);
        parked.commit = 7;
        let old = Dock {
            edge: 3,
            fraction: 0.5,
        };
        assert_ne!(parked.dock, old);
        assert_eq!(
            chooser.pick(Some(&parked), None, 8, Some(old), false).dock,
            parked.dock
        );
        assert_eq!(
            chooser.pick(Some(&parked), None, 7, Some(old), false).dock,
            parked.dock
        );
    }

    #[test]
    fn a_new_service_avoids_a_reserved_incumbent_dock() {
        let mut dock = choice(&[], &[]).select(None);
        dock.commit = 5;
        let reservation = [disk(dock.point, 12.0)];
        let mut chooser = choice(&[], &[]);
        chooser.future = &reservation;

        let next = chooser.pick(Some(&dock), None, 6, None, false);

        assert!(next.valid());
        assert!(!reservation[0].contains(next.point));
    }

    #[test]
    fn spread_prefers_an_opposite_fan_and_edge() {
        let peers = [(Vec2::new(-100.0, 0.0), 3, Vec2::ZERO)];
        let mut chooser = choice(&[], &[]);
        chooser.peers = &peers;
        assert!(
            chooser.spread_cost(Vec2::new(-100.0, 0.0), 3)
                > chooser.spread_cost(Vec2::new(100.0, 0.0), 1)
        );
    }
    #[test]
    fn arrival_prefers_a_clear_disc_over_the_nearest_occupied_slot() {
        let first = choice(&[], &[]).select(None);
        let occupied = [disk(first.point, 7.5)];
        let chooser = choice(&[], &occupied);
        let selected = chooser.nearest(first.point, false);
        assert!(selected.valid());
        assert_eq!(chooser.avatar_cost(selected.point), 0.0);
    }
    #[test]
    fn touched_file_distances_choose_the_nearest_eligible_edge() {
        let mut chooser = choice(&[], &[]);
        chooser.envelope.rays = Arc::new(vec![Vec2::new(0.5, 0.15)]);
        assert_eq!(chooser.select(None).dock.edge, 2);
    }
    #[test]
    fn attachments_touch_the_disc_and_translate_rigidly() {
        let body = Vec2::new(35.0, -28.0);
        let delta = Vec2::new(213.0, 57.0);
        for attachment in 0..8 {
            let rect = attached_rect(
                body,
                Vec2::new(112.0, 24.0),
                15.0 + 4.0 / 3.0,
                attachment,
            );
            assert!(
                (body.distance(body.clamp(rect.min, rect.max)) - 15.0 - 4.0 / 3.0)
                    .abs()
                    < 0.0001
            );
            let moved =
                attached_rect(body + delta, rect.size(), 15.0 + 4.0 / 3.0, attachment);
            assert!(moved.min.abs_diff_eq(rect.min + delta, 0.0001));
        }
    }

    #[test]
    fn spacing_prefers_room_without_invalidating_crowded_slots() {
        let occupied = [disk(Vec2::ZERO, 7.5)];
        let choice = choice(&[], &occupied);
        assert!(
            choice.spacing_cost(Vec2::new(20.0, 0.0))
                > choice.spacing_cost(Vec2::new(90.0, 0.0))
        );
        assert_eq!(choice.avatar_cost(Vec2::new(20.0, 0.0)), 0.0);
    }
    #[test]
    fn open_half_plane_accepts_obtuse_rays_but_rejects_surrounding_targets() {
        let corners = Tile {
            low: Vec2::new(-100.0, -10.0),
            high: Vec2::new(100.0, 0.0),
        }
        .corners();
        let angle = fan(Vec2::new(0.0, 10.0), &corners);
        assert!(angle > std::f32::consts::FRAC_PI_2 && angle < std::f32::consts::PI);
        assert!(fan(Vec2::new(0.0, -5.0), &corners) > std::f32::consts::PI);
    }

    #[test]
    fn swept_intersection_detects_an_obstacle_between_corner_rays_and_labels() {
        let corners = Tile {
            low: Vec2::new(-100.0, -100.0),
            high: Vec2::new(100.0, -90.0),
        }
        .corners();
        let point = Vec2::new(0.0, 100.0);
        let rect = Rect::from_center_size(Vec2::ZERO, Vec2::splat(4.0));
        assert!(corners.iter().all(|&p| !segment(point, p, rect)));
        assert!(swept_crosses(point, &corners, rect));
        assert!(!swept_crosses(
            point,
            &corners,
            rect.translate(Vec2::X * 200.0)
        ));
    }

    #[test]
    fn off_axis_spheres_fit_the_gutter_clearance_budget() {
        let eye = view_direction() * framing::DISTANCE;
        let directions: Vec<_> = (-1_i8..=1)
            .flat_map(|x| {
                (-1_i8..=1).flat_map(move |y| {
                    (-1_i8..=1).map(move |z| {
                        Vec3::new(f32::from(x), f32::from(y), f32::from(z))
                    })
                })
            })
            .filter(|d| d.length_squared() > 0.0)
            .map(Vec3::normalize)
            .collect();
        for scale in [0.5_f32, 0.75, 1.0, 1.5, 2.0] {
            for point in [
                Vec2::new(-620.0, -350.0),
                Vec2::new(620.0, -350.0),
                Vec2::new(-620.0, 350.0),
                Vec2::new(620.0, 350.0),
                Vec2::ZERO,
            ] {
                let centre = world(point);
                let radius = Theme::neon().geometry().avatar_radius
                    * 2.0
                    * scale
                    * projection_scale(centre);
                let points: Vec<_> =
                    directions.iter().map(|&d| centre + d * radius).collect();
                assert!(
                    project(eye, Vec3::ZERO, &points)
                        .flatten()
                        .all(|p| (p * HALF - point).length() < 6.5 * scale + 1.0)
                );
            }
            assert!(
                (6.5 * scale + 1.0) * 2.0
                    < crate::layout::packing::AVATAR_GUTTER * scale.max(1.0)
            );
        }
    }
    fn choice<'a>(obstacles: &'a [(usize, Rect)], occupied: &'a [Rect]) -> Choice<'a> {
        Choice {
            field: super::super::travel::field_rect(Tile::inset(0.0)),
            gutter: None,
            tile: Tile {
                low: Vec2::splat(-0.3),
                high: Vec2::splat(0.3),
            },
            gap: Vec2::new(16.0, 32.0),
            radius: 7.5,
            capacity: 1,
            occupied,
            clearance: &[],
            peers: &[],
            future: &[],
            beams: obstacles,
            cells: &[],
            speed: 480.0,
            repository: 0,
            text: "Ada".into(),
            scale: 1.0,
            envelope: Envelope {
                low: Vec2::splat(0.1),
                high: Vec2::splat(0.9),
                samples: 4,
                mean: 0.5,
                rays: Arc::default(),
            },
        }
    }

    #[test]
    fn travel_seconds_can_outweigh_a_distant_ray_saving() {
        let cells = [Rect::from_corners(
            Vec2::new(-200.0, -124.0),
            Vec2::new(200.0, 124.0),
        )];
        let mut chooser = choice(&[], &[]);
        chooser.cells = &cells;
        chooser.gutter = Some(Tile {
            low: cells[0].min / HALF,
            high: cells[0].max / HALF,
        });
        chooser.envelope.rays = Arc::new(vec![Vec2::ONE; 100]);
        let source = Vec2::new(-200.0, -124.0);
        let far = chooser.select(None);
        let near = chooser.nearest(source, false);
        assert!(near.point.distance(source) < 50.0);
        assert!(chooser.ray_cost(near.point) > chooser.ray_cost(far.point));
        let near_time =
            super::super::travel::slot_seconds(source, near.point, &cells, 480.0);
        let far_time =
            super::super::travel::slot_seconds(source, far.point, &cells, 480.0);
        assert!(
            near_time < far_time,
            "near {:?} {near_time}, far {:?} {far_time}",
            near.point,
            far.point
        );
    }

    #[test]
    fn content_cannot_move_a_gutter_dock() {
        let first = choice(&[], &[]).select(None);
        let content = [(0, disk(first.point, 7.5))];
        let mut next = choice(&content, &[]);
        next.beams = &[];
        assert!(next.assess(first.dock).0.valid());
        assert_eq!(next.select(Some(first.dock)).dock, first.dock);
        let fresh = next.select(None);
        assert_eq!(fresh.dock, first.dock);
    }
    #[test]
    fn prefers_top_keeps_a_valid_dock_and_uses_other_edges_when_blocked() {
        let free = choice(&[], &[]);
        let first = free.select(None);
        assert!(first.valid());
        assert_eq!(first.dock.edge, 0);
        let previous = Dock {
            edge: 2,
            fraction: 0.25,
        };
        assert_eq!(free.select(Some(previous)).dock, previous);
        let obstacles = [(
            1,
            Rect::from_corners(Vec2::new(-300.0, 110.0), Vec2::new(300.0, 150.0)),
        )];
        let blocked = choice(&obstacles, &[]);
        let fallback = blocked.select(None);
        assert!(fallback.valid());
        assert_ne!(fallback.dock.edge, 0);
        assert_eq!(fallback.dock, blocked.select(None).dock);
    }

    #[test]
    fn repository_switch_uses_a_near_valid_edge_instead_of_the_top() {
        let free = choice(&[], &[]);
        let source = Vec2::new(0.0, -250.0);
        let near = free.nearest(source, false);
        assert!(near.valid());
        assert_eq!(near.dock.edge, 2);
        assert!(near.point.distance(source) < free.select(None).point.distance(source));
    }

    #[test]
    fn a_co_author_with_no_later_commit_fades_with_its_disbanding_ring() {
        let mut events = crate::timeline::synthetic();
        events.truncate(2);
        events[0].co_authors = vec![crate::timeline::Identity {
            name: b"Pair".to_vec(),
            email: b"pair@example.invalid".to_vec(),
        }];
        let mut sim = Simulation::new(
            events,
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let solo = sim.schedule.starts[1];
        let gone = crate::model::frames_for(solo + 0.5, 60).unwrap();
        let mut last = [None::<Vec3>; 2];
        let mut largest_step = 0.0_f32;
        let mut ring_shown = 0.0_f32;
        for frame in 0..=gone {
            sim.advance_to(frame, 60);
            ring_shown = ring_shown
                .max(sim.state.avatars.first().map_or(0.0, |a| a.ring_presence));
            for (index, last) in last.iter_mut().enumerate() {
                let parked = sim.state.avatars.get(index).filter(|avatar| {
                    avatar.presence > 0.0
                        && !sim.travel.travelling(index, sim.state.tick)
                });
                let Some(avatar) = parked else {
                    continue;
                };
                let step = last.map_or(0.0, |last| avatar.position.distance(last));
                largest_step = largest_step.max(step);
                *last = Some(avatar.position);
            }
        }
        assert_eq!(ring_shown, 1.0);
        assert!(
            largest_step < 0.1 * sim.state.settle_reach(),
            "an avatar jumped {largest_step}"
        );
        assert_eq!(
            sim.state.avatars[0].slot.as_ref().unwrap().offset,
            Vec2::ZERO
        );
        let (author, pair) = (&sim.state.avatars[0], &sim.state.avatars[1]);
        assert!(pair.dismissed);
        assert_eq!(pair.presence, 0.0);
        assert_eq!(author.ring_presence, 0.0);
        assert!(author.presence > 0.0);
    }

    /// Whether a co-author left behind by its author's next commit fades with the ring,
    /// when its own next commit comes `days` later under a ten-day inactivity horizon.
    fn left_behind_fades(days: i64) -> bool {
        let mut events = crate::timeline::synthetic();
        events.truncate(3);
        let pair = crate::timeline::Identity::new("Pair", "pair@example.invalid");
        events[0].co_authors = vec![pair.clone()];
        events[1].seconds = events[0].seconds + 3_600;
        events[2].seconds = events[0].seconds + days * 86_400;
        events[2].author = pair;
        let mut sim = Simulation::new(
            events,
            &Options {
                contributor_inactivity_days: 10,
                ..Default::default()
            },
            &Theme::neon(),
            &Mappings::default(),
        );
        let solo = sim.schedule.starts[1];
        sim.advance_to(crate::model::frames_for(solo + 0.1, 60).unwrap(), 60);
        sim.state.avatars[1].dismissed
    }

    #[test]
    fn a_co_author_waits_with_its_author_only_for_a_commit_within_its_horizon() {
        assert!(!left_behind_fades(5));
        assert!(left_behind_fades(30));
    }

    #[test]
    fn a_group_never_outgrows_its_seats() {
        let mut events = crate::timeline::synthetic();
        events.truncate(1);
        let template = events.remove(0);
        let events = (0..3_u8)
            .map(|commit| {
                let mut event = template.clone();
                event.seconds += i64::from(commit) * 3_600;
                event.co_authors = (0..8)
                    .map(|n| crate::timeline::Identity {
                        name: format!("C{commit}-{n}").into_bytes(),
                        email: format!("c{commit}-{n}@example.invalid").into_bytes(),
                    })
                    .collect();
                event
            })
            .collect();
        let mut sim = Simulation::new(
            events,
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let frames = crate::model::frames_for(sim.duration(), 60).unwrap();
        for frame in (0..=frames).step_by(5) {
            sim.advance_to(frame, 60);
            let state = &sim.state;
            assert!(state.capacities.iter().all(|&seats| seats <= 9));
            let mut seated: Vec<_> = (0..state.avatars.len())
                .filter_map(|i| {
                    let group = state.avatars[i].group?;
                    state
                        .in_party(i, group.lead)
                        .then_some((group.lead, group.seat))
                })
                .collect();
            let all = seated.len();
            seated.sort_unstable();
            seated.dedup();
            assert_eq!(seated.len(), all, "frame {frame}: two members share a seat");
        }
    }

    #[test]
    fn group_seats_touch_inside_their_ring() {
        for capacity in 2..=9 {
            let ring = group_radius(capacity, 7.5);
            let seats: Vec<_> = (0..capacity)
                .map(|s| seat_around(s, capacity, 0, 7.5))
                .collect();
            let reach = ring_offset(capacity, 0, 7.5).y + ring;
            assert!(
                (reach - 7.5).abs() < 1e-3,
                "the ring reaches out as one avatar"
            );
            let member = 7.5 * MEMBER_SCALE;
            for (index, seat) in seats.iter().enumerate() {
                assert!(seat.length() + member < ring);
                let next = seats[(index + 1) % seats.len()];
                assert!(
                    (seat.distance(next) - 2.0 * (member + GROUP_GAP)).abs() < 1e-3
                );
            }
        }
        assert_eq!(group_radius(1, 7.5), 7.5);
        assert_eq!(seat_offset(0, 1, 0, 7.5), Vec2::ZERO);
    }

    #[test]
    fn crowded_fixed_docks_use_free_edge_space() {
        let mut occupied = Vec::new();
        for index in 0..40 {
            let selected = choice(&[], &occupied).select(None);
            if index == 24 {
                assert!(
                    choice(&[], &occupied)
                        .assess(Dock {
                            edge: 2,
                            fraction: 0.1
                        })
                        .0
                        .valid()
                );
            }
            assert!(
                selected.valid(),
                "contributor {} at {:?}: {} overlaps",
                index + 1,
                selected.point,
                selected.overlaps
            );
            let body = disk(selected.point, 7.5);
            assert!(occupied.iter().all(|&other| !intersect(body, other)));
            occupied.push(body);
        }
    }
    #[test]
    fn free_intervals_keep_exact_body_fits_and_reject_full_reservations() {
        let occupied = [
            Rect::from_corners(-HALF, Vec2::new(-7.5, HALF.y)),
            Rect::from_corners(Vec2::new(7.5, -HALF.y), HALF),
        ];
        let exact = choice(&[], &occupied);
        assert!(
            exact
                .free_docks()
                .into_iter()
                .any(|dock| exact.assess(dock).0.valid())
        );
        let empty = choice(&[], &[]);
        let full: Vec<_> = (0..4)
            .flat_map(|edge| {
                (0..=128_u8).map(move |i| Dock {
                    edge,
                    fraction: f32::from(i) / 128.0,
                })
            })
            .map(|dock| disk(empty.point(dock), 7.5))
            .collect();
        let blocked = choice(&[], &full);
        assert_eq!(blocked.free_docks(), Vec::new());
        let least_bad = blocked.select(None);
        assert!(least_bad.valid());
        assert!(blocked.avatar_cost(least_bad.point) > 0.0);
    }
    #[test]
    fn geometric_top_failure_uses_another_valid_dock() {
        let obstacles = [(
            1,
            Rect::from_center_size(Vec2::new(0.0, 124.0), Vec2::splat(4.0)),
        )];
        let selected = choice(&obstacles, &[]).select(None);
        assert!(selected.valid());
        assert_eq!(selected.crossings, 0);
        assert_ne!(selected.dock, choice(&[], &[]).select(None).dock);
    }
    #[test]
    fn newcomers_cannot_take_a_retained_parking_slot() {
        let mut events = vec![crate::timeline::synthetic().remove(0)];
        crate::repositories::namespace(&mut events, 0);
        let mut sim = Simulation::with_repositories(
            events,
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
            &["a".into(), "b".into()],
        );
        sim.advance_to(60, 60);
        let mut state = sim.state.clone();
        let old = state.avatars[0].slot.as_ref().unwrap().dock;
        let mut newcomer = state.avatars[0].clone();
        newcomer.slot = None;
        newcomer.group = None;
        newcomer.last_active = sim.seconds();
        state.avatars.push(newcomer);
        state.place_slots(&sim.envelopes, sim.seconds());
        let first = state.avatars[0].slot.as_ref().unwrap();
        let next = state.avatars[1].slot.as_ref().unwrap();
        assert_eq!(first.dock, old);
        assert!(
            first.point.distance(next.point) >= 2.0 * state.avatar_radius() - 0.001
        );
    }

    fn thirty() -> Simulation {
        let histories = (0..30)
            .map(|i| {
                let mut events = crate::timeline::synthetic();
                for event in &mut events {
                    event.seconds += i64::try_from(i).unwrap() * 86_400 * 3;
                }
                crate::repositories::namespace(&mut events, i);
                events
            })
            .collect();
        Simulation::with_repositories(
            crate::repositories::merge(histories),
            &Options {
                seconds_per_day: 0.02,
                max_idle_seconds: 0.05,
                ..Default::default()
            },
            &Theme::neon(),
            &Mappings::default(),
            &(0..30)
                .map(|i| format!("Repository {i}"))
                .collect::<Vec<_>>(),
        )
    }

    fn validate_envelopes(sim: &Simulation) {
        for pulse in &sim.state.pulses {
            let Some(tile) = sim.layout().repositories[pulse.repository].tile else {
                continue;
            };
            let envelope = sim.envelopes[pulse.commit].clone();
            let targets = pulse
                .touched
                .iter()
                .copied()
                .map(Target::File)
                .chain(pulse.targets.iter().map(|&(t, ..)| t));
            for target in targets {
                let Some(point) = sim.state.point(target) else {
                    continue;
                };
                let projected = project(
                    view_direction() * framing::DISTANCE,
                    Vec3::ZERO,
                    &[point + Vec3::Y * 0.12],
                )
                .next()
                .flatten()
                .unwrap();
                let relative = (projected - tile.low) / (tile.high - tile.low);
                assert!(relative.cmpge(envelope.low - Vec2::splat(1e-4)).all());
                assert!(relative.cmple(envelope.high + Vec2::splat(1e-4)).all());
            }
        }
    }
    fn reference_avatar_envelopes(sim: &mut Simulation) {
        let mut envelopes = vec![Envelope::default(); sim.events.len()];
        let frames = crate::model::frames_for(sim.duration(), 60)
            .expect("validated playback frame bound");
        for _ in 0..frames {
            sim.tick_layout();
            let now = crate::model::tick_time(sim.state.tick);
            sim.state.pulses.retain(|p| now < p.end);
            for pulse in &sim.state.pulses {
                let Some(tile) = sim.state.layout.repositories[pulse.repository].tile
                else {
                    continue;
                };
                let mut points: Vec<_> = pulse
                    .touched
                    .iter()
                    .filter_map(|&id| sim.state.point(Target::File(id)))
                    .collect();
                let touched_count = points.len();
                let initial_mean = (envelopes[pulse.commit].samples == 0)
                    .then(|| touched_mean(&points, tile))
                    .flatten();
                points.extend(
                    pulse
                        .targets
                        .iter()
                        .filter_map(|&(target, ..)| sim.state.point(target)),
                );
                for point in &mut points {
                    point.y += 0.12;
                }
                let points: Vec<_> =
                    project(view_direction() * framing::DISTANCE, Vec3::ZERO, &points)
                        .flatten()
                        .collect();
                envelopes[pulse.commit].include_rays(&points, touched_count, tile);
                envelopes[pulse.commit].include(&points, tile);
                if let Some(mean) = initial_mean {
                    envelopes[pulse.commit].mean = mean;
                }
            }
        }
        sim.envelopes = envelopes;
        sim.plan_ray_runs();
        sim.state = super::super::SharedState(sim.initial.clone());
    }
    #[test]
    fn selective_envelopes_match_full_scene_replay_exactly() {
        let mut sim = thirty();
        let selected = sim.envelopes.clone();
        sim.plan_avatar_envelopes(None);
        assert_eq!(selected, sim.envelopes);
        reference_avatar_envelopes(&mut sim);
        assert_eq!(selected, sim.envelopes);
    }
    fn snapshot(sim: &Simulation) -> serde_json::Value {
        serde_json::Value::Array(sim.state.avatars.iter().map(|a| serde_json::json!({
            "commit":a.commit,"repository":a.repository,"position":a.position.to_array(),
            "name_opacity":a.name_visibility.opacity,"name_visible":a.name_visibility.visible,
            "placement":a.name_visibility.placement.as_ref().map(|p| serde_json::json!({"point":p.point.to_array(),"body":p.body.to_array(),"text":p.text,"scale":p.scale})),
            "slot":a.slot.as_ref().map(|s| serde_json::json!({"edge":s.dock.edge,"fraction":s.dock.fraction,"point":s.point.to_array(),"text":s.text,"valid":s.valid()}))
        })).collect())
    }
    fn assert_traveller_presence(sim: &Simulation, frame: u32) {
        for (author, avatar) in sim.state.avatars.iter().enumerate() {
            if sim
                .travel
                .sample(author, sim.state.tick)
                .is_some_and(|(_, opacity)| opacity > 0.0)
            {
                assert!(avatar.presence > 0.0, "walking avatar idled out at {frame}");
            }
        }
    }

    #[test]
    fn visible_idle_walkers_do_not_reserve_their_old_service_slot() {
        let histories = (0..2)
            .map(|index| {
                let mut events = vec![crate::timeline::synthetic().remove(0)];
                events[0].author.email =
                    format!("author{index}@example.com").into_bytes();
                events[0].seconds = i64::try_from(index).unwrap();
                crate::repositories::namespace(&mut events, index);
                events
            })
            .collect();
        let mut sim = Simulation::with_repositories(
            crate::repositories::merge(histories),
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
            &["a".into(), "b".into()],
        );
        let second_write = sim.schedule.commit_ticks().nth(1).unwrap();
        sim.advance_to(u32::try_from(second_write).unwrap(), 60);
        let mut state = sim.state.clone();
        state.avatars[0].last_active = crate::model::Time::ZERO;
        state.avatars[0].presence = 0.0;
        state.avatars[0].slot_presence = 0.0;
        state.avatars[1].last_active = crate::model::Time::seconds(12.0);
        state.place_slots(&sim.envelopes, crate::model::Time::seconds(12.0));
        let expected = state.avatars[1].slot.as_ref().unwrap().point;
        state.avatars[0].slot.as_mut().unwrap().point = expected;
        state.avatars[0].presence = 1.0;
        state.place_slots(&sim.envelopes, crate::model::Time::seconds(12.0));
        assert_eq!(state.avatars[1].slot.as_ref().unwrap().point, expected);
        assert!(state.avatars[1].slot.as_ref().unwrap().valid());
    }

    fn validate(sim: &mut Simulation) {
        let frames = crate::model::frames_for(sim.duration(), 60).unwrap();
        let mut moving = diagnostics::MovingBeams::default();
        let mut checkpoints = Vec::new();
        let mut previous_commit = usize::MAX;
        for frame in 0..=frames {
            sim.advance_to(frame, 60);
            super::super::nitpicks::assert_labels_and_beams(sim);
            moving.observe(sim);
            validate_envelopes(sim);
            if sim.commit() != previous_commit {
                checkpoints.push((frame, snapshot(sim)));
                previous_commit = sim.commit();
            }
            let obstacles = tile_obstacles(&sim.state);
            assert_traveller_presence(sim, frame);
            for avatar in sim.state.avatars.iter().filter(|a| {
                sim.seconds() - a.last_active < super::super::AVATAR_IDLE
                    || a.presence > 0.0
            }) {
                // A traveller's retained service slot is not its current position.
                if sim
                    .travel
                    .travelling(sim.state.authors[&avatar.key], sim.state.tick)
                {
                    continue;
                }
                let Some(slot) = &avatar.slot else {
                    continue;
                };
                if !slot.valid() {
                    assert!(sim.state.slot_records[&avatar.commit].least_bad);
                }
                // Idle lane proofs are stale; MovingBeams checks actual travelling
                // discs and live beams independently on every frame.
                if slot.valid()
                    && sim.seconds() - avatar.last_active < super::super::AVATAR_IDLE
                {
                    let tile =
                        sim.layout().repositories[avatar.repository].tile.unwrap();
                    let corners = sim.envelopes[avatar.commit].corners(tile);
                    let origin = slot.point - slot.offset;
                    assert!(fan(origin, &corners) < std::f32::consts::PI);
                    assert!(
                        !obstacles.iter().any(|(r, rect)| *r != usize::MAX
                            && *r != avatar.repository
                            && swept_crosses(origin, &corners, rect.inflate(-0.02))),
                        "frame {frame}, avatar {}, commit {}, slot {:?}",
                        avatar.name,
                        avatar.commit,
                        slot
                    );
                }
            }
        }
        assert_eq!(sim.state.slot_records.len(), sim.events.len());
        let first = report(&sim.state);
        sim.advance_to(0, 60);
        for (frame, expected) in checkpoints {
            sim.advance_to(frame, 60);
            assert_eq!(
                snapshot(sim),
                expected,
                "rewound slot state at frame {frame}"
            );
        }
        sim.advance_to(frames, 60);
        assert_eq!(first, report(&sim.state));
        let summary = serde_json::json!({"avatar_slots":first,"moving_beams":moving,"travel":sim.travel.report(),"duration":sim.duration().lossy_secs_f32()});
        if let Ok(path) = std::env::var("CODESTRAND_SLOT_DIAGNOSTIC") {
            std::fs::write(path, serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
        }
        eprintln!("SLOT_SUMMARY {summary}");
        moving.assert_clear(sim.travel.stats.max_speed);
    }

    /// The parked members of a pulse's group.
    fn parked_group(
        sim: &Simulation,
        pulse: &super::super::Pulse,
        parked: &[usize],
    ) -> Vec<usize> {
        parked
            .iter()
            .copied()
            .filter(|&i| {
                sim.state.avatars[i].commit == pulse.commit
                    && sim.state.in_party(i, pulse.author)
            })
            .collect()
    }

    /// A group's members sit inside its ring, clear of each other and of every other
    /// parked avatar.
    fn assert_group_clear(
        sim: &Simulation,
        author: usize,
        members: &[usize],
        parked: &[usize],
        frame: u32,
    ) {
        let state = &sim.state;
        let radius = state.avatar_radius();
        let point = |i: usize| state.avatars[i].slot.as_ref().unwrap().point;
        let scales = state.member_scales();
        let size = |i: usize| radius * scales[i];
        let slot = state.avatars[author].slot.as_ref().unwrap();
        let centre =
            slot.centre() + ring_offset(state.capacity(author), slot.dock.edge, radius);
        let ring = group_radius(state.capacity(author), radius);
        for &member in members {
            assert!(point(member).distance(centre) + size(member) < ring + 0.01);
            for &other in parked.iter().filter(|&&o| o != member) {
                let distance = point(member).distance(point(other));
                assert!(
                    distance >= size(member) + size(other) - 0.01,
                    "frame {frame}: avatars {member} and {other} overlap"
                );
            }
        }
    }

    #[test]
    fn ring_members_name_themselves_at_their_nearest_point() {
        let mut events = crate::timeline::synthetic();
        events.truncate(3);
        for event in &mut events {
            event.author = events_author();
            event.co_authors = ["Pair", "Codex"]
                .map(|name| crate::timeline::Identity {
                    name: name.as_bytes().to_vec(),
                    email: format!("{name}@example.invalid").into_bytes(),
                })
                .to_vec();
        }
        let mut sim = Simulation::new(
            events,
            &Options {
                seconds_per_day: 2.0,
                ..Default::default()
            },
            &Theme::neon(),
            &Mappings::default(),
        );
        let settled = sim.schedule.starts[2] + 1.5;
        sim.advance_to(crate::model::frames_for(settled, 60).unwrap(), 60);
        let state = &sim.state;
        let preferred = state.name_preferences();
        let names: Vec<_> = (0..3)
            .map(|i| state.avatars[i].name_visibility.placement.as_ref().unwrap())
            .collect();
        for (index, name) in names.iter().enumerate() {
            let (angle, _) = preferred[index].expect("each member sits in the ring");
            let turn = labels::difference(name.angle, angle).abs();
            assert!(
                turn < 0.8,
                "name {index} turned {turn} from its nearest point"
            );
        }
        let bounds: Vec<_> = names
            .iter()
            .map(|name| name.bounds(state.geometry.label_height))
            .collect();
        assert!(
            bounds.iter().enumerate().all(|(i, a)| bounds[i + 1..]
                .iter()
                .all(|b| a.intersect(*b).is_empty()))
        );
    }

    #[test]
    fn names_stay_apart_while_a_ring_forms() {
        let identity = |name: &str| crate::timeline::Identity {
            name: name.as_bytes().to_vec(),
            email: format!("{name}@example.invalid").into_bytes(),
        };
        let mut events = crate::timeline::synthetic();
        events.truncate(3);
        for (event, author) in events.iter_mut().zip(["Ada", "Grace", "Ada"]) {
            event.author = identity(author);
        }
        events[2].co_authors = vec![identity("Grace"), identity("Codex")];
        let mut sim = Simulation::new(
            events,
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let first = crate::model::frames_for(sim.schedule.starts[2], 60).unwrap();
        let mut drawn = std::collections::HashMap::new();
        for frame in first - 30..first + 120 {
            sim.advance_to(frame, 60);
            let state = &sim.state;
            for avatar in &state.avatars {
                let label = state.avatar_label(avatar);
                let shown = avatar
                    .name_visibility
                    .placement
                    .as_ref()
                    .filter(|_| label.opacity > 0.5)
                    .and_then(|_| state.screen(label.position));
                if let (Some(&before), Some(now)) = (drawn.get(&avatar.name), shown) {
                    let step = now.distance(before);
                    assert!(step < 4.0, "frame {frame}: {} jumped {step}", avatar.name);
                }
                match shown {
                    Some(point) => drawn.insert(avatar.name.clone(), point),
                    None => drawn.remove(&avatar.name),
                };
            }
            let names: Vec<_> = state
                .avatars
                .iter()
                .filter(|a| a.settled >= 0.5 && state.avatar_label(a).opacity > 0.5)
                .filter_map(|a| a.name_visibility.placement.as_ref())
                .map(|name| (&name.text, name.bounds(state.geometry.label_height)))
                .collect();
            for (i, (text, rect)) in names.iter().enumerate() {
                for (other, bounds) in &names[i + 1..] {
                    assert!(
                        rect.intersect(*bounds).is_empty(),
                        "frame {frame}: {text} overlaps {other}"
                    );
                }
            }
        }
    }

    /// Every parked avatar drawn at member size sits inside its group's ring.
    fn assert_members_ringed(sim: &Simulation, frame: u32) {
        let state = &sim.state;
        let scales = state.member_scales();
        for (index, avatar) in state.avatars.iter().enumerate() {
            let parked = avatar.presence > 0.0
                && avatar.settle.is_none()
                && !avatar.dismissed
                && !sim.travel.travelling(index, state.tick);
            if parked && scales[index] < 1.0 {
                let inside = state.ring_of(index).is_some_and(|(centre, rim)| {
                    avatar.position.distance(centre) < rim
                });
                assert!(
                    inside,
                    "frame {frame}: {} drawn as a member outside its ring",
                    avatar.name
                );
            }
        }
    }

    #[test]
    fn a_shrinking_group_walks_into_its_seats_and_stays_ringed() {
        let identity = |name: &str| crate::timeline::Identity {
            name: name.as_bytes().to_vec(),
            email: format!("{name}@example.invalid").into_bytes(),
        };
        // The bot and Actions commit in one repository, then in a second with Owen and
        // Codex, who come from and return to their own commits in a third.
        let start = crate::timeline::synthetic()[0].seconds;
        let histories = (0..3)
            .map(|repository| {
                let mut events = crate::timeline::synthetic();
                if repository < 2 {
                    events.truncate(1);
                    events[0].author = identity("Bot");
                    events[0].co_authors = if repository == 0 {
                        vec![identity("Actions")]
                    } else {
                        ["Actions", "Owen", "Codex"].map(identity).to_vec()
                    };
                    events[0].seconds = start + 600 * i64::from(repository == 1);
                }
                for (index, event) in events.iter_mut().enumerate() {
                    if repository == 2 {
                        event.seconds =
                            start + 300 + 600 * i64::try_from(index).unwrap();
                        event.author = identity("Owen");
                        event.co_authors = vec![identity("Codex")];
                    }
                }
                crate::repositories::namespace(&mut events, repository);
                events
            })
            .collect();
        let mut sim = Simulation::with_repositories(
            crate::repositories::merge(histories),
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
            &["a".into(), "b".into(), "c".into()],
        );
        let frames = crate::model::frames_for(sim.duration(), 60).unwrap();
        let mut walking = std::collections::HashMap::new();
        for frame in 0..=frames {
            sim.advance_to(frame, 60);
            assert_members_ringed(&sim, frame);
            let state = &sim.state;
            // A walk into a seat lasts 0.4 s; seats drifting as avatars rescale must not
            // keep restarting it.
            for avatar in &state.avatars {
                let run = walking.entry(avatar.name.clone()).or_insert(0);
                *run = if avatar.settle.is_some() { *run + 1 } else { 0 };
                assert!(
                    *run <= 26,
                    "frame {frame}: {} never reaches its seat",
                    avatar.name
                );
            }
            let member = world(Vec2::X * state.avatar_radius() * MEMBER_SCALE)
                .distance(world(Vec2::ZERO));
            for (index, avatar) in state.avatars.iter().enumerate() {
                let lead = avatar.group.map_or(index, |g| g.lead);
                let drawn = state.avatars[lead].ring_presence >= 1.0
                    && state.avatars[lead].presence >= 1.0
                    && avatar.settle.is_none()
                    && !sim.travel.travelling(index, state.tick);
                if let Some((centre, rim)) = state
                    .beam_ring(index)
                    .filter(|_| drawn && state.in_party(index, lead))
                {
                    let reach = avatar.position.distance(centre) + member;
                    assert!(
                        reach <= rim * 1.01,
                        "frame {frame}: {} reaches {reach} past its drawn ring {rim}",
                        avatar.name
                    );
                }
            }
        }
    }

    fn events_author() -> crate::timeline::Identity {
        crate::timeline::Identity {
            name: b"Ada".to_vec(),
            email: b"Ada@example.invalid".to_vec(),
        }
    }

    #[test]
    fn co_authors_gather_inside_one_ring_and_beam_from_it() {
        let co_author = |name: &str| crate::timeline::Identity {
            name: name.as_bytes().to_vec(),
            email: format!("{name}@example.invalid").into_bytes(),
        };
        let histories = (0..2)
            .map(|repository| {
                let mut events = crate::timeline::synthetic();
                for (index, event) in events.iter_mut().enumerate() {
                    event.seconds += i64::try_from(repository).unwrap() * 86_400 * 3;
                    if index % 3 == 1 {
                        event.co_authors = vec![co_author("Pair"), co_author("Codex")];
                    }
                }
                crate::repositories::namespace(&mut events, repository);
                events
            })
            .collect();
        let mut sim = Simulation::with_repositories(
            crate::repositories::merge(histories),
            &Options {
                seconds_per_day: 0.02,
                max_idle_seconds: 0.05,
                ..Default::default()
            },
            &Theme::neon(),
            &Mappings::default(),
            &["a".into(), "b".into()],
        );
        let shared: Vec<_> = (0..sim.events.len())
            .filter(|&commit| sim.events[commit].co_contributors.len() == 2)
            .collect();
        assert_eq!(shared.len(), 8);
        let mut gathered = std::collections::BTreeSet::new();
        let frames = crate::model::frames_for(sim.duration(), 60).unwrap();
        for frame in 0..=frames {
            sim.advance_to(frame, 60);
            let state = &sim.state;
            let parked: Vec<_> = (0..state.avatars.len())
                .filter(|&i| {
                    let avatar = &state.avatars[i];
                    avatar.presence > 0.0
                        && !sim.travel.travelling(i, state.tick)
                        && avatar
                            .slot
                            .as_ref()
                            .is_some_and(|s| s.world().distance(avatar.position) < 0.01)
                })
                .collect();
            for pulse in state.pulses.iter().filter(|p| sim.pulse_beams_visible(p)) {
                let members = parked_group(&sim, pulse, &parked);
                if parked.contains(&pulse.author) {
                    assert_group_clear(&sim, pulse.author, &members, &parked, frame);
                }
                let seated = (0..state.avatars.len())
                    .filter(|&i| {
                        state.avatars[i].commit == pulse.commit
                            && state.in_party(i, pulse.author)
                            && !sim.travel.travelling(i, state.tick)
                    })
                    .count();
                if seated == 3 {
                    gathered.insert(pulse.commit);
                }
            }
            for beam in sim.beams() {
                let state = &sim.state;
                let start = match state.beam_ring(beam.author) {
                    Some((centre, rim)) => (beam.from.distance(centre) - rim).abs(),
                    None => beam.from.distance(state.avatars[beam.author].position),
                };
                assert!(start < 1e-3, "frame {frame}: beam starts off its ring");
            }
            assert_members_ringed(&sim, frame);
        }
        assert!(
            !gathered.contains(&shared[1]),
            "a parked co-author should retain an independent dock"
        );
        assert_eq!(
            gathered.into_iter().collect::<Vec<_>>(),
            shared
                .into_iter()
                .enumerate()
                .filter_map(|(index, commit)| (index != 1).then_some(commit))
                .collect::<Vec<_>>()
        );
        validate(&mut sim);
    }

    #[test]
    fn realistic_thirty_repository_slots_sample_every_commit_and_replay() {
        let mut sim = thirty();
        assert!(sim.travel.stats.temporary_capture_bytes <= 32 * 1024 * 1024);
        assert!(sim.travel.stats.retained_route_bytes <= 1024 * 1024);
        assert_eq!(sim.envelopes.len(), sim.events.len());
        validate(&mut sim);
    }

    #[test]
    fn idle_avatar_parks_releases_its_lane_and_reenters_without_travel() {
        let template = crate::timeline::synthetic().remove(0);
        let events = [0, 12].map(|day| {
            let mut events = vec![template.clone()];
            events[0].seconds = day * 86_400;
            crate::repositories::namespace(&mut events, usize::from(day > 0));
            events.remove(0)
        });
        let mut sim = Simulation::with_repositories(
            events.into(),
            &Options {
                seconds_per_day: 1.0,
                max_idle_seconds: 20.0,
                contributor_inactivity_days: 5,
                ..Default::default()
            },
            &Theme::neon(),
            &Mappings::default(),
            &["First".into(), "Second".into()],
        );
        sim.advance_to(360, 60);
        let parked = sim.state.avatars[0].position;
        let dock = sim.state.avatars[0].slot.as_ref().unwrap().dock;
        let observations = sim.state.slot_records[&0].observations;
        for frame in 361..660 {
            sim.advance_to(frame, 60);
            if sim.state.avatars[0].presence > 0.0 {
                assert_eq!(sim.state.avatars[0].position, parked);
            }
            assert_eq!(sim.state.avatars[0].slot.as_ref().unwrap().dock, dock);
        }
        assert_eq!(sim.state.avatars[0].presence, 0.0);
        assert_eq!(sim.state.slot_records[&0].observations, observations);
        let mut state = sim.state.clone();
        let mut newcomer = state.avatars[0].clone();
        newcomer.last_active = sim.seconds();
        newcomer.presence = 1.0;
        state.avatars.push(newcomer);
        state.place_slots(&sim.envelopes, sim.seconds());
        assert_eq!(state.avatars[1].slot.as_ref().unwrap().dock, dock);
        sim.advance_to(780, 60);
        assert_eq!(sim.state.avatars[0].repository, 1);
        assert!(sim.state.avatars[0].presence > 0.0);
        assert_eq!(
            sim.state.avatars[0].position,
            sim.state.avatars[0].slot.as_ref().unwrap().world()
        );
        assert_eq!(sim.travel.stats.routed + sim.travel.stats.faded, 0);
    }

    #[test]
    #[ignore = "release timing probe; run with --release --nocapture"]
    fn thirty_repository_timing() {
        for run in 0..3 {
            let start = std::time::Instant::now();
            let mut sim = thirty();
            let plan = start.elapsed();
            let frames = crate::model::frames_for(sim.duration(), 60).unwrap();
            let start = std::time::Instant::now();
            for frame in 0..=frames {
                sim.advance_to(frame, 60);
                std::hint::black_box(&sim.state);
            }
            let sampling = start.elapsed();
            let tiles: f32 = sim
                .layout()
                .repositories
                .iter()
                .filter_map(|r| r.tile)
                .map(Tile::area)
                .sum();
            eprintln!(
                "SCALE_TIMING run={run} plan_ms={} tick_us={} frames={frames} gutter_share={} capture_bytes={} route_bytes={}",
                plan.as_secs_f64() * 1000.0,
                sampling.as_secs_f64() * 1e6 / f64::from(frames),
                Tile::viewport().area() - tiles,
                sim.travel.stats.temporary_capture_bytes,
                sim.travel.stats.retained_route_bytes
            );
        }
        let sim = thirty();
        let options = Options {
            seconds_per_day: 0.02,
            max_idle_seconds: 0.05,
            ..Default::default()
        };
        let labels: Vec<_> = (0..30).map(|i| format!("Repository {i}")).collect();
        let start = std::time::Instant::now();
        let mut base = Simulation::build(
            sim.events.to_vec(),
            &options,
            &Theme::neon(),
            &Mappings::default(),
            &[],
            &labels,
            &mut None,
        )
        .unwrap();
        let build = start.elapsed();
        let start = std::time::Instant::now();
        let padding = base.travel_padding(&options);
        let routing = start.elapsed();
        eprintln!(
            "SCALE_STAGES unpadded_build_ms={} initial_routing_ms={} padded_commits={}",
            build.as_secs_f64() * 1000.0,
            routing.as_secs_f64() * 1000.0,
            padding.len()
        );
    }

    #[test]
    #[ignore = "requires local five-repository manifest"]
    fn owen_repositories_slots_sample_every_commit_and_replay() {
        let manifest = std::env::var("CODESTRAND_SLOT_REPOSITORIES").unwrap();
        let repos =
            crate::repositories::inputs(&[], Some(std::path::Path::new(&manifest)))
                .unwrap();
        let histories = repos
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let mut events = crate::cache::load_whole(
                    &r.path,
                    "HEAD",
                    crate::cache::root().as_deref(),
                )
                .unwrap();
                crate::repositories::namespace(&mut events, i);
                events
            })
            .collect();
        let start = std::time::Instant::now();
        let mut sim = Simulation::with_repositories(
            crate::repositories::merge(histories),
            &Options {
                seconds_per_day: 1.0,
                max_idle_seconds: 0.1,
                avatar_max_speed: std::env::var("CODESTRAND_TRAVEL_SPEED")
                    .ok()
                    .map_or(Options::default().avatar_max_speed, |v| {
                        v.parse().expect("test speed")
                    }),
                travel_cap: std::env::var("CODESTRAND_TRAVEL_CAP").ok().map_or(
                    Options::default().travel_cap,
                    |v| {
                        if v == "none" {
                            crate::model::TravelCap::None
                        } else {
                            crate::model::TravelCap::Seconds(
                                v.parse().expect("test cap"),
                            )
                        }
                    },
                ),
                ..Default::default()
            },
            &Theme::neon(),
            &Mappings::default(),
            &repos
                .iter()
                .map(crate::repositories::Repository::name)
                .collect::<Vec<_>>(),
        );
        let plan = start.elapsed();
        let frames = crate::model::frames_for(sim.duration(), 60).unwrap();
        let start = std::time::Instant::now();
        for frame in 0..=frames {
            sim.advance_to(frame, 60);
            std::hint::black_box(&sim.state);
        }
        let sampling = start.elapsed();
        let tiles: f32 = sim
            .layout()
            .repositories
            .iter()
            .filter_map(|r| r.tile)
            .map(Tile::area)
            .sum();
        eprintln!(
            "LOCAL_TIMING plan_ms={} tick_us={} frames={frames} gutter_share={} duration={}",
            plan.as_secs_f64() * 1000.0,
            sampling.as_secs_f64() * 1e6 / f64::from(frames),
            Tile::viewport().area() - tiles,
            sim.duration()
        );
        validate(&mut sim);
    }
}
