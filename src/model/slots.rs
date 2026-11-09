pub(crate) mod diagnostics;
mod envelopes;
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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Envelope {
    low: Vec2,
    high: Vec2,
    samples: u32,
    mean: f32,
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
        self.include(&points, tile);
        if let Some(mean) = initial_mean {
            self.mean = mean;
        }
    }
    fn corners(self, tile: Tile) -> [Vec2; 4] {
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
    pub(in crate::model) fn middle_top() -> Self {
        Self {
            edge: 0,
            fraction: 0.5,
        }
    }
    fn point(self, tile: Tile, gap: Vec2) -> Vec2 {
        let low = tile.low * HALF;
        let high = tile.high * HALF;
        match self.edge {
            0 => Vec2::new(
                low.x + (high.x - low.x) * self.fraction,
                high.y + gap.y * 0.5,
            ),
            1 => Vec2::new(
                high.x + gap.x * 0.5,
                low.y + (high.y - low.y) * self.fraction,
            ),
            2 => Vec2::new(
                low.x + (high.x - low.x) * self.fraction,
                low.y - gap.y * 0.5,
            ),
            _ => Vec2::new(
                low.x - gap.x * 0.5,
                low.y + (high.y - low.y) * self.fraction,
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Placement {
    pub dock: Dock,
    pub point: Vec2,
    pub name: Option<Vec2>,
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

fn intersect(a: Rect, b: Rect) -> bool {
    !a.intersect(b).is_empty()
}
fn disk(point: Vec2, radius: f32) -> Rect {
    Rect::from_center_size(point, Vec2::splat(radius * 2.0))
}

fn obstacles(state: &State) -> Vec<(usize, Rect)> {
    let mut result = Vec::new();
    for (index, region) in state.layout.repositories.iter().enumerate() {
        let Some(tile) = region.tile else {
            continue;
        };
        result.push((
            index,
            Rect::from_corners(
                tile.low * HALF - Vec2::splat(HALO),
                tile.high * HALF + Vec2::splat(HALO),
            ),
        ));
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
            Rect::from_corners(
                anchor - Vec2::Y * size.y - Vec2::splat(HALO),
                anchor + Vec2::X * size.x + Vec2::splat(HALO),
            ),
        ));
    }
    for rect in [
        Rect::from_corners(Vec2::new(-618.0, 340.0), Vec2::new(-498.0, 360.0)),
        Rect::from_corners(Vec2::new(-640.0, -360.0), Vec2::new(640.0, -338.0)),
    ] {
        result.push((usize::MAX, rect.inflate(HALO)));
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

fn name_obstacles(
    obstacles: &[(usize, Rect)],
    owner: usize,
) -> impl Iterator<Item = &Rect> {
    let own_tile = obstacles
        .iter()
        .position(|(repository, _)| *repository == owner);
    obstacles
        .iter()
        .enumerate()
        .filter_map(move |(index, (_, rect))| (Some(index) != own_tile).then_some(rect))
}
fn place_point(point: Vec3, rotated: bool, scale: f32, origin: Vec2) -> Vec3 {
    let local = Vec2::new(point.x, point.z);
    let local = if rotated { local.perp() } else { local };
    ground(local * scale + origin)
}
impl Simulation {
    pub(super) fn plan_avatar_envelopes(&mut self, snapshots: Option<Snapshots>) {
        if let Some(snapshots) = snapshots {
            self.envelopes = snapshots.project(&self.packing, self.events.len());
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
        self.state = self.initial.clone();
    }
}

impl State {
    pub(super) fn place_slots(&mut self, envelopes: &[Envelope], now: f32) {
        let gap = crate::layout::packing::gutters(count(
            self.layout.repositories.iter().filter(|r| r.live).count(),
        ));
        let obstacles = obstacles(self);
        let radius = 6.5 * self.avatar_scale + 1.0;
        let name_scale = self.name_scale();
        let mut occupied: Vec<Rect> = Vec::new();
        for avatar in &mut self.avatars {
            if now - avatar.last_active >= super::AVATAR_IDLE
                && let Some(slot) = &mut avatar.slot
            {
                slot.name = None;
                if avatar.slot_presence > 0.0 {
                    occupied.push(disk(slot.point, radius));
                }
            }
        }
        let mut order: Vec<_> = (0..self.avatars.len())
            .filter(|&i| now - self.avatars[i].last_active < super::AVATAR_IDLE)
            .collect();
        order.sort_by(|&a, &b| {
            self.avatars[b]
                .last_active
                .total_cmp(&self.avatars[a].last_active)
                .then(a.cmp(&b))
        });
        for index in order {
            let avatar = &self.avatars[index];
            let commit = avatar.commit;
            let envelope = envelopes[commit];
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
                gap,
                radius,
                obstacles: &obstacles,
                occupied: &occupied,
                repository: avatar.repository,
                text,
                scale: name_scale,
                envelope,
            };
            let mut chosen = avatar.slot_origin.map_or_else(
                || choice.select(avatar.slot.as_ref().map(|s| s.dock)),
                |point| choice.nearest(point),
            );
            record.fallback |= chosen.dock.edge != 0;
            record.least_bad |= !chosen.valid();
            record.half_plane_failures += u32::from(!chosen.half_plane);
            record.max_crossings = record.max_crossings.max(chosen.crossings);
            record.max_overlaps = record.max_overlaps.max(chosen.overlaps);
            let name = choice.name_rect(chosen.point, self.geometry.label_height);
            occupied.push(disk(chosen.point, radius));
            if let Some(rect) = name {
                chosen.name = Some(Vec2::new(rect.min.x, rect.max.y));
                occupied.push(rect);
            }
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

    pub(super) fn slot_name_point(&self, avatar: &super::Avatar) -> Option<Vec2> {
        let slot = avatar.slot.as_ref()?;
        let point = project(
            self.focus + view_direction() * self.distance,
            self.focus,
            &[avatar.position],
        )
        .next()
        .flatten()?
            * HALF;
        Some(if point.distance(slot.point) <= 12.0 {
            slot.name?
        } else {
            slot.name? + point - slot.point
        })
    }

    pub(super) fn place_slot_names(&mut self, _dt: f32) {
        let obstacles = if self.layout.repositories.is_empty() {
            Vec::new()
        } else {
            obstacles(self)
        };
        let bodies: Vec<_> = self
            .avatars
            .iter()
            .filter(|a| a.presence > 0.0)
            .filter_map(|a| {
                project(
                    self.focus + view_direction() * self.distance,
                    self.focus,
                    &[a.position],
                )
                .next()
                .flatten()
                .map(|p| disk(p * HALF, 6.5 * self.avatar_scale + 1.0))
            })
            .collect();
        let mut order: Vec<_> = (0..self.avatars.len()).collect();
        order.sort_by(|&a, &b| {
            let priority = |i: usize| {
                super::names::priority(
                    self.avatars[i].activity,
                    self.avatars[i].name_visibility.visible,
                )
            };
            priority(b).total_cmp(&priority(a)).then(a.cmp(&b))
        });
        let mut placed: Vec<Rect> = Vec::new();
        for index in order {
            let avatar = &self.avatars[index];
            let body = project(
                self.focus + view_direction() * self.distance,
                self.focus,
                &[avatar.position],
            )
            .next()
            .flatten()
            .map(|p| p * HALF);
            let base = self.name_scale();
            let blocked: Vec<_> = name_obstacles(&obstacles, avatar.repository)
                .map(|r| r.inflate(-HALO))
                .chain(bodies.iter().copied())
                .chain(placed.iter().copied())
                .collect();
            let placement = body.filter(|_| avatar.presence > 0.0).and_then(|body| {
                persistent_name(
                    body,
                    &avatar.name,
                    base,
                    self.geometry.label_height,
                    avatar.name_visibility.placement.as_ref(),
                    &blocked,
                )
            });
            if let Some((_, rect)) = &placement {
                placed.push(*rect);
            }
            let visibility = &mut self.avatars[index].name_visibility;
            visibility.visible = placement.is_some();
            visibility.opacity = f32::from(u8::from(placement.is_some()));
            visibility.placement = placement.map(|(name, _)| name);
        }
    }
}

fn persistent_name(
    body: Vec2,
    name: &str,
    base: f32,
    height: f32,
    previous: Option<&super::names::Placement>,
    blocked: &[Rect],
) -> Option<(super::names::Placement, Rect)> {
    let minimum = (12.0 / height).min(base);
    let free = |rect: Rect| {
        rect.min.cmpge(-HALF + Vec2::splat(0.5)).all()
            && rect.max.cmple(HALF - Vec2::splat(0.5)).all()
            && blocked.iter().all(|&other| !intersect(rect, other))
    };
    let size =
        |text: &str, scale| super::names::rect(Vec2::ZERO, text, scale, height).size();
    if let Some(previous) =
        previous.filter(|p| p.text == name && p.scale <= base && p.scale >= minimum)
    {
        let point = previous.point + body - previous.body;
        let size = size(name, previous.scale);
        let rect =
            Rect::from_corners(point - Vec2::Y * size.y, point + Vec2::X * size.x);
        if free(rect) {
            return Some((
                super::names::Placement {
                    point,
                    body,
                    ..previous.clone()
                },
                rect,
            ));
        }
    }
    let candidates = |text: &str, scale: f32, lanes: bool| {
        let size = size(text, scale);
        (0..=u8::from(lanes) * 24).flat_map(move |lane| {
            let dy = f32::from(lane.div_ceil(2))
                * (size.y + 2.0)
                * if lane % 2 == 0 { 1.0 } else { -1.0 };
            [0.0, -24.0, 24.0, -48.0, 48.0, -96.0, 96.0]
                .into_iter()
                .take(if lanes { 7 } else { 1 })
                .flat_map(move |dx| {
                    [
                        body + Vec2::new(12.0 + dx, size.y * 0.5 + dy),
                        body + Vec2::new(-12.0 - size.x + dx, size.y * 0.5 + dy),
                    ]
                })
                .map(move |point| {
                    (
                        point,
                        Rect::from_corners(
                            point - Vec2::Y * size.y,
                            point + Vec2::X * size.x,
                        ),
                    )
                })
        })
    };
    let scales =
        [base, base * 0.9, base * 0.8, base * 0.7, minimum].map(|s| s.max(minimum));
    for lanes in [false, true] {
        for scale in scales {
            if let Some((point, rect)) =
                candidates(name, scale, lanes).find(|(_, r)| free(*r))
            {
                return Some((
                    super::names::Placement {
                        point,
                        body,
                        text: name.to_owned(),
                        scale,
                    },
                    rect,
                ));
            }
        }
    }
    if let Some((point, rect)) = free_lane(body, size(name, minimum), blocked) {
        return Some((
            super::names::Placement {
                point,
                body,
                text: name.to_owned(),
                scale: minimum,
            },
            rect,
        ));
    }
    for length in (1..name.chars().count().min(240)).rev() {
        let text = format!("{}…", name.chars().take(length).collect::<String>());
        if let Some((point, rect)) =
            candidates(&text, minimum, true).find(|(_, r)| free(*r))
        {
            return Some((
                super::names::Placement {
                    point,
                    body,
                    text,
                    scale: minimum,
                },
                rect,
            ));
        }
    }
    None
}

fn free_lane(body: Vec2, size: Vec2, blocked: &[Rect]) -> Option<(Vec2, Rect)> {
    let xs = [-HALF.x + 0.5, HALF.x - size.x - 0.5, body.x + 12.0]
        .into_iter()
        .chain(
            blocked
                .iter()
                .flat_map(|r| [r.min.x - size.x - 0.01, r.max.x + 0.01]),
        )
        .collect::<Vec<_>>();
    let ys = [-HALF.y + size.y + 0.5, HALF.y - 0.5, body.y + size.y * 0.5]
        .into_iter()
        .chain(
            blocked
                .iter()
                .flat_map(|r| [r.min.y - 0.01, r.max.y + size.y + 0.01]),
        );
    ys.flat_map(|y| xs.iter().map(move |&x| Vec2::new(x, y)))
        .map(|point| {
            (
                point,
                Rect::from_corners(point - Vec2::Y * size.y, point + Vec2::X * size.x),
            )
        })
        .filter(|(_, r)| {
            r.min.cmpge(-HALF + Vec2::splat(0.5)).all()
                && r.max.cmple(HALF - Vec2::splat(0.5)).all()
                && blocked.iter().all(|&other| !intersect(*r, other))
        })
        .min_by(|(a, _), (b, _)| {
            a.distance_squared(body)
                .total_cmp(&b.distance_squared(body))
        })
}

struct Choice<'a> {
    tile: Tile,
    gap: Vec2,
    radius: f32,
    obstacles: &'a [(usize, Rect)],
    occupied: &'a [Rect],
    repository: usize,
    text: String,
    scale: f32,
    envelope: Envelope,
}

impl Choice<'_> {
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
            .min_by(|a, b| {
                a.point
                    .distance_squared(point)
                    .total_cmp(&b.point.distance_squared(point))
            })
            .unwrap_or_else(|| self.select(None))
    }
    fn assess(&self, dock: Dock) -> (Placement, f32) {
        let point = dock.point(self.tile, self.gap);
        let corners = self.envelope.corners(self.tile);
        let span = fan(point, &corners);
        let (low, high) = corners
            .iter()
            .fold((point, point), |(low, high), &p| (low.min(p), high.max(p)));
        let crossings = self
            .obstacles
            .iter()
            .filter(|(repo, rect)| {
                *repo != usize::MAX
                    && *repo != self.repository
                    && low.cmple(rect.max).all()
                    && high.cmpge(rect.min).all()
                    && swept_crosses(point, &corners, *rect)
            })
            .count();
        let bounds = disk(point, self.radius);
        let overlaps = self
            .occupied
            .iter()
            .filter(|&&other| intersect(bounds, other))
            .count()
            + self
                .obstacles
                .iter()
                .filter(|(_, other)| intersect(bounds, other.inflate(-HALO)))
                .count()
            + usize::from(!point.abs().cmple(HALF - Vec2::splat(self.radius)).all());
        (
            Placement {
                dock,
                point,
                name: None,
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
        let fixed = self.best(docks.iter().copied());
        if fixed.valid() {
            return fixed;
        }
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
            .min_by(|a, b| a.1.cmp(&b.1).then(a.2.total_cmp(&b.2)).then(a.3.cmp(&b.3)))
            .expect("at least one candidate dock")
            .0
    }

    fn free_docks(&self) -> Vec<Dock> {
        let mut docks = Vec::new();
        for edge in 0..4 {
            let axis = usize::from(edge % 2 == 1);
            let cross = 1 - axis;
            let origin = Dock {
                edge,
                fraction: 0.0,
            }
            .point(self.tile, self.gap);
            if origin[cross].abs() + self.radius > HALF[cross] {
                continue;
            }
            let low = (self.tile.low * HALF)[axis].max(-HALF[axis] + self.radius);
            let high = (self.tile.high * HALF)[axis].min(HALF[axis] - self.radius);
            if low > high {
                continue;
            }
            let mut blocked: Vec<_> = self
                .occupied
                .iter()
                .copied()
                .chain(self.obstacles.iter().map(|(_, r)| r.inflate(-HALO)))
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
        let origin = (self.tile.low * HALF)[axis];
        let span = ((self.tile.high - self.tile.low) * HALF)[axis];
        let inset = ((high - low) * 0.25).min(0.001);
        [low + inset, low.midpoint(high), high - inset].map(|value| Dock {
            edge,
            fraction: (value - origin) / span,
        })
    }
    fn name_rect(&self, point: Vec2, label_height: f32) -> Option<Rect> {
        if self.text.is_empty() {
            return None;
        }
        let height = label_height * self.scale + 4.0;
        let width = self.width();
        let x = (point.x + self.radius + 3.0).clamp(
            self.tile.low.x * HALF.x,
            (self.tile.high.x * HALF.x - width).max(self.tile.low.x * HALF.x),
        );
        [
            (x, point.y + height * 0.5),
            (point.x - self.radius - 3.0 - width, point.y + height * 0.5),
            (x, point.y + self.radius + height + 2.0),
            (x, point.y - self.radius - 2.0),
        ]
        .into_iter()
        .map(|(x, top)| {
            let top = top.clamp(-HALF.y + height + 0.5, HALF.y - 0.5);
            Rect::from_corners(Vec2::new(x, top - height), Vec2::new(x + width, top))
        })
        .find(|rect| {
            rect.min.cmpge(-HALF).all()
                && !intersect(*rect, disk(point, self.radius))
                && rect.max.cmple(HALF).all()
                && !name_obstacles(self.obstacles, self.repository)
                    .any(|other| intersect(*rect, other.inflate(-HALO)))
                && !self.occupied.iter().any(|&other| intersect(*rect, other))
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
                self.slot_name_point(avatar).map_or(avatar.position, world)
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
        "definitions": "Parked slots use a swept touched-file AABB in tile-relative projected coordinates, including appearing/departing files and aggregated endpoints, sampled at canonical 60 Hz throughout live service. Foreign tiles and repository labels are inflated by two reference pixels for beam halo. Fallback means any non-top dock; least-bad means any infeasible observation, including a missing birth tile. Counts are unique commits, even when service observations repeat. Moving beams remain a Step 4 diagnostic, not a parked-slot failure.",
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
        for scale in [0.75, 1.0] {
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
                    .x
            );
        }
    }
    fn choice<'a>(obstacles: &'a [(usize, Rect)], occupied: &'a [Rect]) -> Choice<'a> {
        Choice {
            tile: Tile {
                low: Vec2::splat(-0.3),
                high: Vec2::splat(0.3),
            },
            gap: Vec2::new(16.0, 32.0),
            radius: 7.5,
            obstacles,
            occupied,
            repository: 0,
            text: "Ada".into(),
            scale: 1.0,
            envelope: Envelope {
                low: Vec2::splat(0.1),
                high: Vec2::splat(0.9),
                samples: 4,
                mean: 0.5,
            },
        }
    }

    #[test]
    fn narrow_gutter_names_can_cover_own_tile_but_never_foreign_tiles_or_labels() {
        let tile = Rect::from_corners(Vec2::splat(-200.0), Vec2::splat(200.0));
        let label = Rect::from_center_size(Vec2::new(-160.0, 180.0), Vec2::splat(20.0));
        let foreign =
            Rect::from_corners(Vec2::new(212.0, -200.0), Vec2::new(400.0, 200.0));
        let obstacles = [
            (0, tile.inflate(HALO)),
            (0, label.inflate(HALO)),
            (1, foreign.inflate(HALO)),
        ];
        let mut choice = choice(&obstacles, &[]);
        choice.gap = Vec2::new(12.0, 16.0);
        choice.radius = 4.25;
        choice.scale = 0.5;
        let point = Vec2::new(0.0, 116.0);
        let name = choice.name_rect(point, 20.0).unwrap();
        assert!(intersect(name, tile));
        assert!(!intersect(name, label));
        assert!(!intersect(name, foreign));
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
        let full = [Rect::from_corners(-HALF, HALF)];
        let blocked = choice(&[], &full);
        assert_eq!(blocked.free_docks(), Vec::new());
        let least_bad = blocked.select(None);
        assert!(!least_bad.valid());
        assert!(least_bad.overlaps > 0);
    }
    #[test]
    fn geometric_top_failure_uses_another_edge_before_spreading() {
        let obstacles = [(
            1,
            Rect::from_center_size(Vec2::new(0.0, 124.0), Vec2::splat(4.0)),
        )];
        let selected = choice(&obstacles, &[]).select(None);
        assert!(selected.valid());
        assert_ne!(selected.dock.edge, 0);
    }
    #[test]
    fn separates_parked_bodies_and_name_lanes_and_bounds_long_names() {
        let first = choice(&[], &[]).select(None);
        let mut occupied = vec![disk(first.point, 7.5)];
        let name = choice(&[], &occupied).name_rect(first.point, 20.0).unwrap();
        occupied.push(name);
        let next = choice(&[], &occupied).select(None);
        assert!(next.valid());
        assert!(!intersect(disk(next.point, 7.5), occupied[0]));
        if let Some(name2) = choice(&[], &occupied).name_rect(next.point, 20.0) {
            assert!(!intersect(name, name2));
        }
        let text =
            short_name("A contributor with a very long display name", 1.0, 112.0);
        assert!(text.ends_with("..."));
        assert!(8.0 + 8.6 * count(text.chars().count()) <= 112.0);
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
            let envelope = sim.envelopes[pulse.commit];
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
                envelopes[pulse.commit].include(&points, tile);
                if let Some(mean) = initial_mean {
                    envelopes[pulse.commit].mean = mean;
                }
            }
        }
        sim.envelopes = envelopes;
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
            "slot":a.slot.as_ref().map(|s| serde_json::json!({"edge":s.dock.edge,"fraction":s.dock.fraction,"point":s.point.to_array(),"name":s.name.map(|p| p.to_array()),"text":s.text,"valid":s.valid()}))
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
            let obstacles = obstacles(&sim.state);
            let mut bodies = Vec::new();
            let mut names = Vec::new();
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
                        && swept_crosses(slot.point, &corners, *rect)));
                }
                let body = disk(slot.point, 6.5 * sim.state.avatar_scale + 1.0);
                assert!(bodies.iter().all(|&other| !intersect(body, other)));
                bodies.push(body);
                if let Some(point) = slot.name {
                    let size = super::super::names::rect(
                        Vec2::ZERO,
                        &slot.text,
                        sim.state.name_scale(),
                        sim.state.geometry.label_height,
                    )
                    .size();
                    let rect = Rect::from_corners(
                        point - Vec2::Y * size.y,
                        point + Vec2::X * size.x,
                    );
                    assert!(names.iter().all(|&other| !intersect(rect, other)));
                    assert!(
                        name_obstacles(&obstacles, avatar.repository)
                            .all(|other| !intersect(rect, other.inflate(-HALO)))
                    );
                    names.push(rect);
                }
            }
            assert!(
                names
                    .iter()
                    .all(|&name| bodies.iter().all(|&body| !intersect(name, body)))
            );
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
        assert!(sim.travel.stats.temporary_capture_bytes <= 8 * 1024 * 1024);
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
        let default = Options::default();
        let cap = std::env::var("CODESTRAND_TRAVEL_CAP")
            .ok()
            .map_or(default.travel_cap, |v| {
                crate::travel_seconds(&v).expect("test cap")
            });
        if repos.len() == 5
            && sim.travel.stats.max_speed.to_bits()
                == default.avatar_max_speed.to_bits()
            && cap == default.travel_cap
        {
            sim.travel.assert_human_motion();
        }
    }
}
