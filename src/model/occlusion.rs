use bevy::math::{Rect, Vec2, Vec3, Vec3Swizzles};
use serde::Serialize;

use super::{
    Avatar, Simulation, State, framing, project, slots, travel, view_direction,
};

const HALF: Vec2 = Vec2::new(640.0, 360.0);
pub(super) fn screen(sim: &Simulation, point: Vec3) -> Vec2 {
    project(
        sim.state.focus + view_direction() * sim.state.distance,
        sim.state.focus,
        &[point],
    )
    .next()
    .flatten()
    .expect("visible model point")
        * HALF
}
pub(in crate::model) fn label_bounds(sim: &Simulation, label: &super::Label) -> Rect {
    super::names::top_left_rect(
        screen(sim, label.position),
        &label.text,
        label.scale,
        sim.state.geometry.label_height,
    )
}

fn parked(avatar: &Avatar) -> bool {
    !avatar.travelling && avatar.settle.is_none()
}

fn disc_radius(avatar: &Avatar, base: f32, member_scale: f32) -> f32 {
    base * member_scale
        * avatar.projection_scale
        * avatar.lifecycle_scale
        * avatar.presence
}

const SEAT_CLEARANCE: f32 = 0.75 * framing::WORLD_UNITS_PER_REFERENCE_PIXEL;

#[derive(Clone, Copy)]
struct Disc {
    centre: Vec2,
    radius: f32,
    blocks_walkers: bool,
}

fn escape_points(
    from: Vec2,
    target: Vec2,
    obstacle: Disc,
    extent: f32,
    step: f32,
) -> [Vec2; 5] {
    let centre = obstacle.centre;
    let toward = (centre - from).try_normalize().unwrap_or(Vec2::X);
    let normal = (target - centre).try_normalize().unwrap_or(-toward);
    let closest = centre + normal * extent;
    let distance = from.distance(centre);
    let along = ((distance.powi(2) + step.powi(2) - extent.powi(2))
        / (2.0 * distance.max(SEAT_CLEARANCE)))
    .clamp(-step, step);
    let across = (step.powi(2) - along.powi(2)).max(0.0).sqrt();
    let tangent = toward.perp() * across;
    [
        from,
        from + (closest - from).clamp_length_max(step),
        from - toward * step,
        from + toward * along + tangent,
        from + toward * along - tangent,
    ]
}

fn clear_position(
    from: Vec2,
    target: Vec2,
    step: f32,
    obstacles: &[Disc],
    radius_at: impl Fn(Vec2) -> f32,
    allowed: impl Fn(Vec2) -> bool,
) -> Vec2 {
    let cost = |point: Vec2| {
        let radius = radius_at(point);
        let penetration = obstacles
            .iter()
            .map(|disc| {
                (radius + disc.radius + SEAT_CLEARANCE - point.distance(disc.centre))
                    .max(0.0)
            })
            .fold(0.0, f32::max);
        (penetration, point.distance_squared(target))
    };
    let radius = radius_at(target);
    obstacles
        .iter()
        .copied()
        .filter_map(|disc| {
            let extent = radius + disc.radius + 2.0 * SEAT_CLEARANCE;
            (target.distance(disc.centre) < extent)
                .then(|| escape_points(from, target, disc, extent, step))
        })
        .flatten()
        .filter(|&point| allowed(point))
        .chain(std::iter::once(target))
        .map(|point| (cost(point), point))
        .min_by(|a, b| a.0.partial_cmp(&b.0).expect("finite disc positions"))
        .map_or(target, |(_, point)| point)
}

pub(super) fn clear_seat_transitions(
    state: &mut State,
    plan: &travel::Plan,
    previous: &[Option<Vec3>],
    max_step: f32,
) {
    let base = state.geometry.avatar_radius
        * state.scale()
        * state.avatar_scale
        * state.avatar_size;
    let scales = state.member_scales();
    let birth_step = max_step
        + 2.0 * state.avatar_radius() * framing::WORLD_UNITS_PER_REFERENCE_PIXEL;
    let (focus, distance) = state.overview();
    let zoom = (distance - framing::FIELD_HEIGHT)
        / (framing::DISTANCE - framing::FIELD_HEIGHT);
    let field =
        travel::field_rect(crate::layout::packing::Tile::inset(state.avatar_radius()));
    let corner = |point| focus.xz() + slots::world(point).xz() * zoom;
    let bounds = Rect::from_corners(corner(field.min), corner(field.max));
    let mut order: Vec<_> = state
        .avatars
        .iter()
        .enumerate()
        .filter(|(_, avatar)| avatar.presence > 0.001)
        .map(|(index, avatar)| {
            (index, !plan.walking(index, state.tick), parked(avatar))
        })
        .collect();
    order.sort_by_key(|&(index, blocks, parked)| {
        (
            blocks,
            previous[index].is_none(),
            !parked,
            state.avatars[index].dock_since,
            index,
        )
    });
    let mut placed: Vec<Disc> = Vec::with_capacity(order.len());
    let mut obstacles = Vec::with_capacity(order.len());
    for (index, blocks_walkers, _) in order {
        let avatar = &mut state.avatars[index];
        let target = avatar.position.xz();
        let y = avatar.position.y;
        let nominal =
            disc_radius(avatar, base, scales[index]) / avatar.projection_scale;
        let world = |point: Vec2| Vec3::new(point.x, y, point.y);
        let radius_at = |point: Vec2| nominal * slots::projection_scale(world(point));
        let radius = radius_at(target);
        obstacles.clear();
        obstacles.extend(placed.iter().copied().filter(|disc| {
            (blocks_walkers || disc.blocks_walkers)
                && target.distance(disc.centre) < radius + disc.radius + 2.0 * max_step
        }));
        let mut point = target;
        if blocks_walkers
            && obstacles.iter().any(|disc| {
                target.distance(disc.centre) < radius + disc.radius + SEAT_CLEARANCE
            })
        {
            let from = previous[index].map_or(target, Vec3::xz);
            let step = if previous[index].is_some() {
                max_step
            } else {
                birth_step
            };
            point =
                clear_position(from, target, step, &obstacles, radius_at, |point| {
                    bounds.contains(point)
                });
        }
        avatar.position = world(point);
        avatar.projection_scale = slots::projection_scale(avatar.position);
        placed.push(Disc {
            centre: point,
            radius: radius_at(point),
            blocks_walkers,
        });
    }
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Audit {
    pub maximum_avatar_radius: f32,
    pub avatar_text_observations: u64,
    pub avatar_avatar_observations: u64,
    pub parked_avatar_observations: u64,
    pub examples: Vec<serde_json::Value>,
}

impl Audit {
    pub(crate) fn observe(&mut self, sim: &Simulation) {
        let (focus, distance) = sim.view();
        let eye = focus + view_direction() * distance;
        let right = Vec3::X;
        let screen = |point| {
            project(eye, focus, &[point])
                .next()
                .flatten()
                .map(|p| p * framing::VIEWPORT * 0.5)
        };
        let labels: Vec<_> = sim
            .labels_for_detail(|_| true)
            .into_iter()
            .filter(|label| label.opacity > 0.001 && !label.text.is_empty())
            .map(|label| (label_bounds(sim, &label), label.text))
            .collect();
        let discs: Vec<_> = sim
            .avatars()
            .iter()
            .enumerate()
            .zip(sim.member_scales())
            .filter(|((_, avatar), _)| avatar.presence > 0.001)
            .filter_map(|((index, avatar), member_scale)| {
                let base = sim.state.geometry.avatar_radius
                    * sim.scale()
                    * sim.avatar_disc_scale();
                let radius = disc_radius(avatar, base, member_scale);
                if radius <= 0.0 {
                    return None;
                }
                let centre = screen(avatar.position)?;
                let edge = screen(avatar.position + right * radius)?;
                Some((index, centre, centre.distance(edge)))
            })
            .collect();
        for &(author, centre, radius) in &discs {
            self.maximum_avatar_radius = self.maximum_avatar_radius.max(radius);
            for (bounds, text) in &labels {
                if !overlaps(centre, radius, *bounds) {
                    continue;
                }
                self.avatar_text_observations += 1;
                if self.examples.len() < 12 {
                    self.examples.push(serde_json::json!({"seconds":sim.seconds().lossy_secs_f32(),"author":author,"text":text,"centre":centre.to_array(),"radius":radius}));
                }
            }
        }
        for (index, &(author, centre, radius)) in discs.iter().enumerate() {
            for &(other, point, other_radius) in &discs[index + 1..] {
                if centre.distance(point) + 0.01 < radius + other_radius {
                    self.avatar_avatar_observations += 1;
                    self.parked_avatar_observations += u64::from(
                        parked(&sim.state.avatars[author])
                            || parked(&sim.state.avatars[other]),
                    );
                }
            }
        }
    }
}

fn overlaps(centre: Vec2, radius: f32, rect: Rect) -> bool {
    centre.distance(centre.clamp(rect.min, rect.max)) + 0.01 < radius
}
