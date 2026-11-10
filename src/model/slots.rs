use std::sync::Arc;
pub(crate) mod diagnostics;
mod envelopes;
mod labels;
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

const TRAVEL_WEIGHT: f32 = 100_000.0;

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
}

pub(super) fn world(point: Vec2) -> Vec3 {
    ground(crate::layout::world_coordinates(framing::unproject_at(
        point / HALF,
        MIN_AVATAR_HEIGHT,
    ))) + Vec3::Y * MIN_AVATAR_HEIGHT
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
            .map(crate::layout::world_coordinates)
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
        if let Some(title) = state.titles.get(&index) {
            result.push((index, title.rect().inflate(HALO)));
            continue;
        }
        let anchor = project(
            view_direction() * framing::DISTANCE,
            Vec3::ZERO,
            &[framing::label_position(region)],
        )
        .next()
        .flatten()
        .unwrap()
            * HALF;
        let size = framing::label_size(&region.label, 20.0);
        result.push((
            index,
            Rect::from_corners(anchor - Vec2::Y * size.y, anchor + Vec2::X * size.x)
                .inflate(HALO),
        ));
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
        if let Some(snapshots) = snapshots {
            self.envelopes = snapshots.project(&self.packing, self.events.len());
            self.plan_ray_runs();
            return;
        }
        let mut envelopes = vec![Envelope::default(); self.events.len()];
        let frames = super::frames_for(self.duration(), 60)
            .expect("validated playback frame bound");
        for _ in 0..frames {
            self.tick_local_layout();
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
                .packing
                .sample_repositories(self.state.tick, &needed)
                .expect("planned repository samples");
            for pulse in &self.state.pulses {
                let Some(tile) = regions[pulse.repository].tile else {
                    continue;
                };
                let origin =
                    crate::layout::world_coordinates(origins[pulse.repository]);
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
        self.envelopes = envelopes;
        self.plan_ray_runs();
        self.state = self.initial.clone();
    }
}

impl State {
    #[cfg(test)]
    pub(super) fn place_slots(&mut self, envelopes: &[Envelope], now: f32) {
        self.place_slots_planned(envelopes, now, &[]);
    }
    pub(super) fn place_slots_planned(
        &mut self,
        envelopes: &[Envelope],
        now: f32,
        docks: &[Option<Dock>],
    ) {
        let gap = Vec2::splat(self.gutter);
        let beams = tile_obstacles(self);
        let cells: Vec<_> = self
            .layout
            .repositories
            .iter()
            .filter_map(|r| r.gutter)
            .map(|t| Rect::from_corners(t.low * HALF, t.high * HALF))
            .collect();
        let radius = self.avatar_radius();
        let name_scale = self.name_scale();
        let mut occupied: Vec<Rect> = Vec::new();
        for avatar in &mut self.avatars {
            if now - avatar.last_active >= super::AVATAR_IDLE
                && let Some(slot) = &mut avatar.slot
            {
                if let Some(tile) = self.layout.repositories[avatar.repository].gutter {
                    slot.point = slot.dock.on_network(tile);
                }
                if avatar.slot_presence > 0.0 || avatar.presence > 0.0 {
                    occupied.push(disk(slot.point, radius));
                }
            }
        }
        let mut order: Vec<_> = (0..self.avatars.len())
            .filter(|&i| now - self.avatars[i].last_active < super::AVATAR_IDLE)
            .collect();
        let retained: Vec<_> = self
            .avatars
            .iter()
            .map(|avatar| {
                if now - avatar.last_active >= super::AVATAR_IDLE {
                    return None;
                }
                let slot = avatar.slot.as_ref()?;
                let tile = self.layout.repositories[avatar.repository].gutter?;
                Some(disk(slot.dock.on_network(tile), radius))
            })
            .collect();
        occupied.extend(retained.iter().flatten().copied());
        order.sort_by(|&a, &b| {
            self.avatars[b]
                .last_active
                .total_cmp(&self.avatars[a].last_active)
                .then(a.cmp(&b))
        });
        for index in order {
            if let Some(own) = retained[index]
                && let Some(position) = occupied.iter().position(|r| *r == own)
            {
                occupied.remove(position);
            }
            let avatar = &self.avatars[index];
            let commit = avatar.commit;
            let envelope = envelopes[commit].clone();
            let record = self.slot_records.entry(commit).or_default();
            record.observations += 1;
            let Some(tile) = self.layout.repositories[avatar.repository].tile else {
                record.least_bad = true;
                record.missing_tile = true;
                self.avatars[index].slot = None;
                continue;
            };
            let text = short_name(&avatar.name, name_scale, tile.pixels().x - 24.0);
            let choice = Choice {
                tile,
                gutter: self.layout.repositories[avatar.repository].gutter,
                gap,
                radius,
                beams: &beams,
                occupied: &occupied,
                repository: avatar.repository,
                text,
                scale: name_scale,
                envelope,
                cells: &cells,
                speed: self.avatar_max_speed,
            };
            let mut chosen = choice.pick(
                avatar.slot.as_ref(),
                avatar.slot_origin.map(|(repository, dock, point)| {
                    self.layout.repositories[repository]
                        .gutter
                        .map_or(point, |t| dock.on_network(t))
                }),
                commit,
                docks.get(commit).copied().flatten(),
            );
            chosen.commit = commit;
            record.fallback |= chosen.dock.edge != 0;
            record.least_bad |= !chosen.valid();
            record.half_plane_failures += u32::from(!chosen.half_plane);
            record.max_crossings = record.max_crossings.max(chosen.crossings);
            record.max_overlaps = record.max_overlaps.max(chosen.overlaps);
            occupied.push(disk(chosen.point, radius));
            self.avatars[index].slot = Some(chosen);
            self.avatars[index].slot_origin = None;
        }
    }
}

impl State {
    pub(super) fn name_scale(&self) -> f32 {
        self.avatar_scale
            * if self.layout.repositories.is_empty() {
                1.0
            } else {
                0.95
            }
    }

    fn name_bodies(&self) -> Vec<Option<Vec2>> {
        self.avatars
            .iter()
            .map(|a| {
                project(
                    self.focus + view_direction() * self.distance,
                    self.focus,
                    &[a.position],
                )
                .next()
                .flatten()
                .map(|p| p * HALF)
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
                pulse.targets.iter().filter_map(|&(target, ..)| {
                    let author = pulse.author;
                    Some((
                        bodies[author]?,
                        project(
                            self.focus + view_direction() * self.distance,
                            self.focus,
                            &[self.point(target)?],
                        )
                        .next()
                        .flatten()?
                            * HALF,
                    ))
                })
            })
            .collect()
    }
    fn label_emergency(
        &self,
        index: usize,
        motion: &Rotation<'_>,
        texts: &[Rect],
    ) -> bool {
        motion.previous.is_some_and(|p| {
            let mut threats = texts.to_vec();
            threats.extend(
                self.avatars
                    .iter()
                    .enumerate()
                    .filter(|(i, a)| *i != index && a.presence > 0.0)
                    .filter_map(|(_, a)| a.name_visibility.placement.as_ref())
                    .map(|n| {
                        let size = super::names::rect(
                            Vec2::ZERO,
                            &n.text,
                            n.scale,
                            self.geometry.label_height,
                        )
                        .size();
                        Rect::from_corners(
                            n.point - Vec2::Y * size.y,
                            n.point + Vec2::X * size.x,
                        )
                    }),
            );
            motion.future.iter().take(12).any(|&point| {
                let rect = labels::tangent_rect(
                    point,
                    motion.size,
                    self.avatar_radius() + 4.0 / 3.0,
                    p.angle,
                )
                .inflate(12.0);
                !rect.min.cmpge(-HALF).all()
                    || !rect.max.cmple(HALF).all()
                    || threats.iter().any(|r| !rect.intersect(*r).is_empty())
            })
        })
    }
    fn label_domains(
        &self,
        bodies: &[Option<Vec2>],
        travel: &super::travel::Plan,
        dt: f32,
        texts: &[Rect],
    ) -> (Vec<usize>, Vec<Vec<labels::Candidate>>) {
        let mut obstacles = tile_obstacles(self);
        obstacles.extend(texts.iter().map(|&rect| (usize::MAX, rect)));
        let rays = self.name_rays(bodies, travel);
        let space = LabelSpace {
            bodies,
            obstacles: &obstacles,
            rays: &rays,
            avatars: &self.avatars,
            radius: self.avatar_radius(),
            margin: 4.0 / 3.0,
        };
        let mut authors = Vec::new();
        let mut domains = Vec::new();
        for (index, avatar) in self
            .avatars
            .iter()
            .enumerate()
            .filter(|(_, a)| a.presence > 0.0)
        {
            let Some(body) = bodies[index] else { continue };
            let size = super::names::rect(
                Vec2::ZERO,
                &avatar.name,
                self.name_scale(),
                self.geometry.label_height,
            )
            .size();
            let previous = avatar.name_visibility.placement.as_ref();
            let velocity = previous
                .map_or(Vec2::ZERO, |p| (body - p.body) / dt.max(1e-6))
                .clamp_length_max(480.0);
            let future = travel.forecast(index, self.tick).unwrap_or_else(|| {
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
                            HALF - Vec2::splat(space.radius),
                        )
                    })
                    .collect()
            });
            let rotation = Rotation {
                body,
                size,
                previous,
                dt,
                future: &future,
            };
            let emergency = self.label_emergency(index, &rotation, texts);
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
            targets.extend(previous.map(|p| p.angle));
            let scored = targets
                .into_iter()
                .flat_map(|target| {
                    std::iter::once(space.candidate(index, target, &rotation, false))
                        .chain(
                            emergency.then(|| {
                                space.candidate(index, target, &rotation, true)
                            }),
                        )
                })
                .collect();
            let candidates = eligible_angles(scored);
            authors.push(index);
            domains.push(candidates);
        }
        (authors, domains)
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
        }
        let bodies = self.name_bodies();
        let (authors, domains) = self.label_domains(&bodies, travel, dt, texts);
        let assignment = labels::solve(&domains);
        for ((index, candidates), choice) in
            authors.into_iter().zip(domains).zip(assignment.choices)
        {
            let selected = candidates[choice];
            let placement = super::names::Placement {
                point: Vec2::new(selected.rect.min.x, selected.rect.max.y),
                body: bodies[index].expect("projected visible avatar"),
                text: self.avatars[index].name.clone(),
                scale: self.name_scale(),
                angle: selected.angle,
                angular_velocity: selected.angular_velocity,
                commit: self.avatars[index].commit,
            };
            let visibility = &mut self.avatars[index].name_visibility;
            visibility.visible = true;
            visibility.opacity = 1.0;
            visibility.placement = Some(placement);
            visibility.joint_exhausted = assignment.exhausted;
            visibility.joint_clear = assignment.clear;
        }
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
        .dedup_by(|a, b| a.angle.to_bits() == b.angle.to_bits() && a.goal == b.goal);
    candidates
}
struct Rotation<'a> {
    body: Vec2,
    size: Vec2,
    previous: Option<&'a super::names::Placement>,
    dt: f32,
    future: &'a [Vec2],
}
struct LabelSpace<'a> {
    bodies: &'a [Option<Vec2>],
    obstacles: &'a [(usize, Rect)],
    rays: &'a [(Vec2, Vec2)],
    avatars: &'a [super::Avatar],
    radius: f32,
    margin: f32,
}
impl LabelSpace<'_> {
    fn candidate(
        &self,
        index: usize,
        target: f32,
        motion: &Rotation<'_>,
        emergency: bool,
    ) -> (labels::Candidate, [f32; 4]) {
        let (angle, angular_velocity) = motion.previous.map_or((target, 0.0), |p| {
            labels::steer(p.angle, p.angular_velocity, target, motion.dt, emergency)
        });
        let distance = self.radius + self.margin;
        let rect = labels::tangent_rect(motion.body, motion.size, distance, angle);
        let goal = labels::tangent_rect(motion.body, motion.size, distance, target);
        let cost = self.cost(index, rect);
        let goal_cost = self.cost(index, goal);
        let clip = labels::forecast_clip(
            angle,
            angular_velocity,
            target,
            motion.size,
            distance,
            motion.future,
            emergency,
        );
        let text = self.text_cost(rect);
        let soft = cost[2]
            + clip * 1e8
            + f32::from(u8::from(emergency)) * 0.01
            + goal_cost[0]
            + goal_cost[1] * 1e6
            + goal_cost[2]
            + motion
                .previous
                .map_or(0.0, |p| (angular_velocity - p.angular_velocity).abs());
        (
            labels::Candidate {
                angle,
                angular_velocity,
                rect,
                goal,
                text_pairs: text.0,
                text_area: text.1,
                goal_text_pairs: self.text_cost(goal).0,
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
    fn cost(&self, index: usize, rect: Rect) -> [f32; 3] {
        let foreign = self
            .bodies
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index && self.avatars[*i].presence > 0.0)
            .filter_map(|(_, p)| *p);
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
                + self
                    .obstacles
                    .iter()
                    .filter(|(r, _)| {
                        *r != usize::MAX && *r != self.avatars[index].repository
                    })
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
    tile: Tile,
    gutter: Option<Tile>,
    gap: Vec2,
    radius: f32,
    beams: &'a [(usize, Rect)],
    occupied: &'a [Rect],
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
    ) -> Placement {
        if let Some(dock) = planned {
            return self.assess(dock).0;
        }
        let previous = previous
            .filter(|s| {
                s.commit == commit || self.avatar_cost(self.point(s.dock)) <= 0.001
            })
            .map(|s| s.dock);
        origin.map_or_else(|| self.select(previous), |p| self.nearest(p))
    }
    fn point(&self, dock: Dock) -> Vec2 {
        self.gutter.map_or_else(
            || dock.point(self.tile, self.gap),
            |tile| dock.on_network(tile),
        )
    }
    fn nearest(&self, point: Vec2) -> Placement {
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
                let ray = self.ray_cost(slot.point);
                let cost = ray + TRAVEL_WEIGHT * seconds;
                (slot, cost)
            })
            .min_by(|a, b| {
                self.avatar_cost(a.0.point)
                    .total_cmp(&self.avatar_cost(b.0.point))
                    .then(
                        self.spacing_cost(a.0.point)
                            .total_cmp(&self.spacing_cost(b.0.point)),
                    )
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
        self.occupied
            .iter()
            .map(|r| (self.radius * 2.0 - point.distance(r.center())).max(0.0))
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
        let corners = self.envelope.corners(self.tile);
        let span = fan(point, &corners);
        let (low, high) = corners
            .iter()
            .fold((point, point), |(low, high), &p| (low.min(p), high.max(p)));
        let crossings = self
            .beams
            .iter()
            .filter(|(repo, rect)| {
                *repo != usize::MAX
                    && *repo != self.repository
                    && low.cmple(rect.max).all()
                    && high.cmpge(rect.min).all()
                    && swept_crosses(point, &corners, rect.inflate(0.02))
            })
            .count();
        let overlaps = usize::from(
            !point.abs().cmple(HALF - Vec2::splat(self.radius)).all()
                || point.y - self.radius < super::caption::BOTTOM,
        );
        (
            Placement {
                dock,
                commit: usize::MAX,
                point,
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
        docks
            .enumerate()
            .map(|(id, dock)| {
                let (p, span) = self.assess(dock);
                let rank = match (dock.edge, id) {
                    (0, 0) => 0,
                    (0, _) => 1,
                    _ => 2,
                };
                let cost = (p.overlaps, usize::from(!p.half_plane), p.crossings, rank);
                (p, cost, span, id)
            })
            .min_by(|a, b| {
                (a.1.0, a.1.1, a.1.2)
                    .cmp(&(b.1.0, b.1.1, b.1.2))
                    .then(
                        self.avatar_cost(a.0.point)
                            .total_cmp(&self.avatar_cost(b.0.point)),
                    )
                    .then(
                        self.spacing_cost(a.0.point)
                            .total_cmp(&self.spacing_cost(b.0.point)),
                    )
                    .then(
                        self.ray_cost(a.0.point)
                            .total_cmp(&self.ray_cost(b.0.point)),
                    )
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
            if origin[cross].abs() + self.radius > HALF[cross] {
                continue;
            }
            let low = (tile.low * HALF)[axis].max(-HALF[axis] + self.radius);
            let high = (tile.high * HALF)[axis].min(HALF[axis] - self.radius);
            if low > high {
                continue;
            }
            let mut blocked: Vec<_> = self
                .occupied
                .iter()
                .copied()
                .filter(|r| {
                    origin[cross] + self.radius > r.min[cross]
                        && origin[cross] - self.radius < r.max[cross]
                })
                .map(|r| (r.min[axis] - self.radius, r.max[axis] + self.radius))
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
        let multi = !self.layout.repositories.is_empty();
        if let Some(name) = &avatar.name_visibility.placement {
            let position = if multi {
                world(name.point)
            } else {
                let eye = self.focus + view_direction() * self.distance;
                let forward = (self.focus - eye).normalize();
                let right = forward.cross(Vec3::Y).normalize();
                let up = right.cross(forward);
                let depth = (avatar.position - eye).dot(forward);
                let half_height = depth * std::f32::consts::FRAC_PI_8.tan();
                eye + forward * depth
                    + right * (name.point.x / HALF.x * half_height * 16.0 / 9.0)
                    + up * (name.point.y / HALF.y * half_height)
            };
            return super::Label {
                position,
                text: name.text.clone(),
                kind: super::LabelKind::Avatar,
                scale: name.scale,
                opacity: avatar.presence,
                priority: avatar.activity,
            };
        }
        super::Label {
            position: if multi {
                avatar.position
            } else {
                avatar.position + self.geometry.avatar_label_offset
            },
            text: avatar
                .slot
                .as_ref()
                .map_or_else(|| avatar.name.clone(), |s| s.text.clone()),
            kind: super::LabelKind::Avatar,
            scale: self.name_scale(),
            opacity: avatar.name_visibility.opacity
                * avatar.presence
                * f32::from(u8::from(!multi || avatar.name_visibility.visible)),
            priority: avatar.activity,
        }
    }
}

fn swept_crosses(point: Vec2, corners: &[Vec2; 4], rect: Rect) -> bool {
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
    fn arrival_prefers_a_clear_disc_over_the_nearest_occupied_slot() {
        let first = choice(&[], &[]).select(None);
        let occupied = [disk(first.point, 7.5)];
        let chooser = choice(&[], &occupied);
        let selected = chooser.nearest(first.point);
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
                    < crate::layout::packing::gutters(if scale < 1.0 {
                        30.0
                    } else {
                        5.0
                    })
                    .x * scale.max(1.0)
            );
        }
    }
    fn choice<'a>(obstacles: &'a [(usize, Rect)], occupied: &'a [Rect]) -> Choice<'a> {
        Choice {
            gutter: None,
            tile: Tile {
                low: Vec2::splat(-0.3),
                high: Vec2::splat(0.3),
            },
            gap: Vec2::new(16.0, 32.0),
            radius: 7.5,
            occupied,
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
        let near = chooser.nearest(source);
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
        let near = free.nearest(source);
        assert!(near.valid());
        assert_eq!(near.dock.edge, 2);
        assert!(near.point.distance(source) < free.select(None).point.distance(source));
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
        sim.state = sim.initial.clone();
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
            "commit":a.commit,"repository":a.repository,"position":a.position.to_array(),"velocity":a.velocity.to_array(),
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
        sim.advance_to(120, 60);
        let mut state = sim.state.clone();
        state.avatars[0].last_active = 0.0;
        state.avatars[0].presence = 0.0;
        state.avatars[0].slot_presence = 0.0;
        state.avatars[1].last_active = 12.0;
        state.place_slots(&sim.envelopes, 12.0);
        let expected = state.avatars[1].slot.as_ref().unwrap().point;
        state.avatars[0].slot.as_mut().unwrap().point = expected;
        state.avatars[0].presence = 1.0;
        state.place_slots(&sim.envelopes, 12.0);
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
                    assert!(fan(slot.point, &corners) < std::f32::consts::PI);
                    assert!(!obstacles.iter().any(|(r, rect)| *r != usize::MAX
                        && *r != avatar.repository
                        && swept_crosses(slot.point, &corners, rect.inflate(-0.02))));
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
        let summary = serde_json::json!({"avatar_slots":first,"moving_beams":moving,"travel":sim.travel.report(),"duration":sim.duration()});
        if let Ok(path) = std::env::var("CODESTRAND_SLOT_DIAGNOSTIC") {
            std::fs::write(path, serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
        }
        eprintln!("SLOT_SUMMARY {summary}");
        moving.assert_clear(sim.travel.stats.max_speed);
    }

    #[test]
    #[ignore = "requires local repositories and diagnostic path"]
    fn local_travel_activity_audit() {
        let started = std::time::Instant::now();
        let (mut sim, _) = super::super::nitpicks::local_simulation();
        let planning = started.elapsed().as_secs_f64();
        let nominal = super::super::Schedule::new(&sim.events, 1.0, 0.1);
        let frames = super::super::frames_for(sim.duration(), 60).unwrap();
        let mut idle = 0_u64;
        let mut animation = 0_u64;
        let mut motion = 0_u64;
        let mut previous = Vec::<(usize, Vec3, Option<Dock>)>::new();
        let mut lane_changes = 0_u64;
        let mut lane_moves = 0_u64;
        let mut animated = vec![0_u64; sim.events.len()];
        let mut services = Vec::new();
        let mut seen = 0;
        let mut moving = diagnostics::MovingBeams::default();
        for frame in 0..=frames {
            sim.advance_to(frame, 60);
            moving.observe(&sim);
            let active = !sim.beams().is_empty() || !sim.fragments().is_empty();
            let mut moved = false;
            for (index, avatar) in sim.state.avatars.iter().enumerate() {
                if let Some(&(commit, point, dock)) = previous.get(index) {
                    lane_changes += u64::from(
                        commit == avatar.commit
                            && dock != avatar.slot.as_ref().map(|s| s.dock)
                            && avatar.presence > 0.0,
                    );
                    let changed = point.distance(avatar.position) > 0.00001;
                    moved |= changed && avatar.presence > 0.0;
                    lane_moves += u64::from(
                        changed
                            && commit == avatar.commit
                            && avatar.presence > 0.0
                            && !sim.travel.travelling(index, sim.state.tick),
                    );
                }
            }
            previous = sim
                .state
                .avatars
                .iter()
                .map(|a| (a.commit, a.position, a.slot.as_ref().map(|s| s.dock)))
                .collect();
            for pulse in &sim.state.pulses {
                if sim.pulse_beams_visible(pulse)
                    && super::super::envelope(
                        sim.seconds() - pulse.start,
                        pulse.end - pulse.start,
                    ) > 0.0
                    && pulse
                        .targets
                        .iter()
                        .any(|&(target, ..)| sim.state.point(target).is_some())
                {
                    animated[pulse.commit] += 1;
                }
            }
            for commit in seen..sim.state.next_event {
                let contributor = sim.events[commit].contributor.as_ref().unwrap();
                services.push(serde_json::json!({"commit":commit,"tick":frame,"author":sim.state.authors[&contributor.key],"nominal":nominal.starts[commit]}));
            }
            seen = sim.state.next_event;
            idle += u64::from(!active && !moved);
            animation += u64::from(active);
            motion += u64::from(moved);
        }
        let value = serde_json::json!({"duration":sim.duration(),"plan_seconds":planning,
            "frames":frames+1,"idle_frames":idle,"animation_frames":animation,
            "motion_frames":motion,"lane_move_frames":lane_moves,"lane_changes":lane_changes,
            "commits_without_animation":animated.iter().filter(|&&a| a==0).count(),
            "animation_frame_counts":animated,"brief_commits_100ms":animated.iter().filter(|&&n|n>0 && n<=6).count(),"services":services,"travel":sim.travel.report(),
            "label_gap_p95":moving.label_gap_p95(),"moving_beams":moving});
        std::fs::write(
            std::env::var("CODESTRAND_TRAVEL_DIAGNOSTIC").unwrap(),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[ignore = "requires local five-repository manifest and travel diagnostic path"]
    fn local_uncapped_travel_and_bot_classification() {
        let started = std::time::Instant::now();
        let (mut sim, _) = super::super::nitpicks::local_simulation();
        let mut seen = std::collections::HashSet::new();
        let contributors: Vec<_> = sim.events.iter().filter_map(|event| {
            let c = event.contributor.as_ref().unwrap();
            seen.insert(c.key.clone()).then(|| serde_json::json!({"name":c.name,"email":String::from_utf8_lossy(&event.author.email),"rule":c.bot_rule(&event.author.email),"bot":c.is_bot(&event.author.email)}))
        }).collect();
        let report = serde_json::json!({"duration":sim.duration(),"plan_seconds":started.elapsed().as_secs_f64(),"contributors":contributors,"travel":sim.travel.report()});
        std::fs::write(
            std::env::var("CODESTRAND_TRAVEL_DIAGNOSTIC").unwrap(),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
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
            assert_eq!(sim.state.avatars[0].position, parked);
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
            sim.events.clone(),
            &options,
            &Theme::neon(),
            &Mappings::default(),
            &[],
            &labels,
        );
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
                let mut events = crate::cache::load(
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
                travel_cap: std::env::var("CODESTRAND_TRAVEL_CAP")
                    .ok()
                    .map_or(Options::default().travel_cap, |v| {
                        crate::travel_seconds(&v).expect("test cap")
                    }),
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
