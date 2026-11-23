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

fn full_disc_radius(avatar: &Avatar, base: f32, member_scale: f32) -> f32 {
    base * member_scale * avatar.projection_scale * avatar.lifecycle_scale
}

fn disc_radius(avatar: &Avatar, base: f32, member_scale: f32) -> f32 {
    full_disc_radius(avatar, base, member_scale) * avatar.presence
}

fn seating_disc_radius(
    avatar: &Avatar,
    base: f32,
    member_scale: f32,
    now: super::Time,
) -> f32 {
    if avatar.transmitting || (!avatar.travelling && now < avatar.expires) {
        full_disc_radius(avatar, base, member_scale)
    } else {
        disc_radius(avatar, base, member_scale)
    }
}

const SEAT_CLEARANCE: f32 = 0.75 * framing::WORLD_UNITS_PER_REFERENCE_PIXEL;

#[derive(Clone, Copy, Debug)]
pub(super) struct BodyDisc {
    centre: Vec2,
    radius: f32,
}

impl BodyDisc {
    fn clears(self, other: Self) -> bool {
        self.centre.distance_squared(other.centre)
            >= (self.radius + other.radius + SEAT_CLEARANCE).powi(2)
    }
}

fn seating_base(state: &State) -> f32 {
    state.geometry.avatar_radius
        * state.scale()
        * state.avatar_scale
        * state.avatar_size
}

pub(super) fn parked_disc(
    state: &State,
    author: usize,
    member_scale: f32,
) -> Option<BodyDisc> {
    let avatar = state.avatars.get(author)?;
    (avatar.presence > 0.001 && parked(avatar)).then(|| BodyDisc {
        centre: avatar.position.xz(),
        radius: seating_disc_radius(
            avatar,
            seating_base(state),
            member_scale,
            super::tick_time(state.tick),
        ),
    })
}

#[derive(Debug)]
pub(super) struct Transmitters {
    base: f32,
    discs: Vec<(usize, BodyDisc)>,
}

impl Transmitters {
    pub(super) fn observe(state: &State) -> Self {
        let base = seating_base(state);
        let discs = state
            .avatars
            .iter()
            .zip(state.member_scales())
            .enumerate()
            .filter(|(_, (avatar, _))| avatar.transmitting && avatar.presence > 0.001)
            .map(|(author, (avatar, scale))| {
                let disc = BodyDisc {
                    centre: avatar.position.xz(),
                    radius: full_disc_radius(avatar, base, scale),
                };
                (author, disc)
            })
            .collect();
        Self { base, discs }
    }

    fn walker(point: Vec2, base: f32) -> BodyDisc {
        let position = slots::world(point);
        BodyDisc {
            centre: position.xz(),
            radius: base * slots::projection_scale(position),
        }
    }

    pub(super) fn clears(&self, author: usize, point: Vec2) -> bool {
        let walker = Self::walker(point, self.base);
        self.discs
            .iter()
            .all(|&(other, disc)| other != author && walker.clears(disc))
    }

    pub(super) fn clears_body(&self, point: Vec2, disc: BodyDisc) -> bool {
        Self::walker(point, self.base).clears(disc)
    }
}

#[derive(Clone, Copy)]
struct Disc {
    centre: Vec2,
    radius: f32,
    blocks_walkers: bool,
    transmitting: bool,
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

fn disc_intersections(a: Disc, b: Disc, margin: f32) -> Option<[Vec2; 2]> {
    let between = b.centre - a.centre;
    let distance = between.length_squared();
    if distance <= f32::EPSILON {
        return None;
    }
    let a_radius = a.radius + margin;
    let b_radius = b.radius + margin;
    let along = (distance + a_radius.powi(2) - b_radius.powi(2)) / (2.0 * distance);
    let across = a_radius.powi(2) / distance - along.powi(2);
    if across < 0.0 {
        return None;
    }
    let centre = a.centre + between * along;
    let offset = between.perp() * across.sqrt();
    Some([centre + offset, centre - offset])
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
    let (mut best_cost, mut best) = obstacles
        .iter()
        .copied()
        .filter_map(|disc| {
            let extent = radius + disc.radius + 2.0 * SEAT_CLEARANCE;
            (target.distance(disc.centre) < extent)
                .then(|| escape_points(from, target, disc, extent, step))
        })
        .flatten()
        .chain(std::iter::once(target))
        .filter(|&point| point == from || allowed(point))
        .map(|point| (cost(point), point))
        .min_by(|a, b| a.0.partial_cmp(&b.0).expect("finite disc positions"))
        .unwrap_or_else(|| (cost(from), from));
    if best_cost.0 == 0.0 {
        return best;
    }
    let intersections = obstacles.iter().enumerate().flat_map(|(index, &a)| {
        obstacles[index + 1..]
            .iter()
            .filter_map(move |&b| disc_intersections(a, b, radius + SEAT_CLEARANCE))
            .flatten()
    });
    for point in intersections {
        if from.distance_squared(point) > step * step || !allowed(point) {
            continue;
        }
        let candidate = cost(point);
        if candidate < best_cost {
            (best_cost, best) = (candidate, point);
        }
    }
    best
}

fn seat_bounds(state: &State) -> Rect {
    let (focus, distance) = state.overview();
    let zoom = (distance - framing::FIELD_HEIGHT)
        / (framing::DISTANCE - framing::FIELD_HEIGHT);
    let margin = state.avatar_radius()
        + SEAT_CLEARANCE / framing::WORLD_UNITS_PER_REFERENCE_PIXEL;
    let field = travel::field_rect(crate::layout::packing::Tile::inset(margin));
    let corner = |point| focus.xz() + slots::world(point).xz() * zoom;
    Rect::from_corners(corner(field.min), corner(field.max))
}

fn previous_seats(
    state: &State,
    previous: &[Option<Vec3>],
    scales: &[f32],
    base: f32,
) -> Vec<Option<Disc>> {
    let now = super::tick_time(state.tick);
    state
        .avatars
        .iter()
        .zip(previous)
        .zip(scales)
        .map(|((avatar, position), scale)| {
            Some(Disc {
                centre: position.as_ref()?.xz(),
                radius: seating_disc_radius(avatar, base, *scale, now),
                blocks_walkers: true,
                transmitting: avatar.transmitting,
            })
        })
        .collect()
}

fn seat_order(
    state: &State,
    plan: &travel::Plan,
    previous: &[Option<Vec3>],
) -> Vec<(usize, bool, bool)> {
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
            !state.avatars[index].transmitting,
            blocks,
            previous[index].is_none(),
            !parked,
            state.avatars[index].dock_since,
            index,
        )
    });
    order
}

#[expect(
    clippy::too_many_lines,
    reason = "a placement and its forecast share the same obstacles"
)]
pub(super) fn clear_seat_transitions(
    state: &mut State,
    plan: &travel::Plan,
    previous: &[Option<Vec3>],
    max_step: f32,
) {
    let base = seating_base(state);
    let mut scales = state.member_scales();
    for (index, avatar) in state.avatars.iter().enumerate() {
        if avatar.transmitting || avatar.dismissed {
            continue;
        }
        if let Some((centre, rim)) = state.ring_of(index)
            && previous[index]
                .is_some_and(|from| from.distance(centre) + max_step >= rim)
        {
            scales[index] = 1.0;
        }
    }
    let birth_step = max_step
        + 2.0 * state.avatar_radius() * framing::WORLD_UNITS_PER_REFERENCE_PIXEL;
    let bounds = seat_bounds(state);
    let order = seat_order(state, plan, previous);
    let mut gutters = None;
    let mut placed: Vec<Disc> = Vec::with_capacity(order.len());
    let mut approaching: Vec<Disc> = Vec::new();
    let mut obstacles = Vec::with_capacity(order.len());
    let old_seats = previous_seats(state, previous, &scales, base);
    let now = super::tick_time(state.tick);
    for (rank, &(index, blocks_walkers, parked_here)) in order.iter().enumerate() {
        let avatar = &mut state.avatars[index];
        let target = if !avatar.transmitting
            && !avatar.travelling
            && avatar
                .slot
                .as_ref()
                .is_some_and(|slot| slot.offset != Vec2::ZERO)
        {
            let target = avatar.position.xz().clamp(bounds.min, bounds.max);
            previous[index]
                .map_or(target, |from| from.xz().move_towards(target, max_step))
        } else {
            avatar.position.xz()
        };
        let y = avatar.position.y;
        let nominal = seating_disc_radius(avatar, base, scales[index], now)
            / avatar.projection_scale;
        let world = |point: Vec2| Vec3::new(point.x, y, point.y);
        let radius_at = |point: Vec2| nominal * slots::projection_scale(world(point));
        let radius = radius_at(target);
        let step = if previous[index].is_some() {
            max_step
        } else {
            birth_step
        };
        let nearby = |disc: &Disc| {
            target.distance(disc.centre) < radius + disc.radius + 2.0 * step
        };
        obstacles.clear();
        obstacles.extend(
            placed
                .iter()
                .copied()
                .filter(|disc| (blocks_walkers || disc.blocks_walkers) && nearby(disc)),
        );
        if blocks_walkers {
            obstacles.extend(approaching.iter().copied().filter(nearby));
            if parked_here {
                obstacles.extend(order[rank + 1..].iter().filter_map(
                    |&(other, ..)| {
                        let disc = old_seats[other]?;
                        nearby(&disc).then_some(disc)
                    },
                ));
            }
        }
        let mut point = target;
        if !avatar.transmitting
            && obstacles.iter().any(|disc| {
                (blocks_walkers || disc.transmitting)
                    && target.distance(disc.centre)
                        < radius + disc.radius + SEAT_CLEARANCE
            })
        {
            let from = previous[index].map_or(target, Vec3::xz);
            let network = (!blocks_walkers).then(|| {
                gutters
                    .get_or_insert_with(|| {
                        travel::network(
                            state.layout.repositories.iter().filter_map(|r| r.gutter),
                        )
                    })
                    .as_slice()
            });
            point =
                clear_position(from, target, step, &obstacles, radius_at, |point| {
                    network.map_or_else(
                        || bounds.contains(point),
                        |cells| {
                            travel::on_network(slots::field_point(world(point)), cells)
                        },
                    )
                });
        }
        avatar.position = world(point);
        avatar.projection_scale = slots::projection_scale(avatar.position);
        placed.push(Disc {
            centre: point,
            radius: radius_at(point),
            blocks_walkers,
            transmitting: avatar.transmitting,
        });
        if !blocks_walkers {
            approaching.extend(
                plan.walking_forecast(index, state.tick, [4, 8, 12])
                    .filter_map(|(ticks, future)| {
                        let centre = slots::world(future).xz();
                        (centre.distance(point)
                            <= super::ticks_duration(ticks) * 60.0 * max_step)
                            .then(|| Disc {
                                centre,
                                radius: radius_at(centre),
                                blocks_walkers: false,
                                transmitting: false,
                            })
                    }),
            );
        }
    }
    // A parked avatar may have room to yield once the arriving seats are placed.
    clear_remaining_parked_contacts(
        state,
        previous,
        max_step,
        bounds,
        &order,
        &mut placed,
    );
}

fn clear_remaining_parked_contacts(
    state: &mut State,
    previous: &[Option<Vec3>],
    max_step: f32,
    bounds: Rect,
    order: &[(usize, bool, bool)],
    placed: &mut [Disc],
) {
    let mut obstacles = Vec::new();
    for (rank, &(index, local, parked)) in order.iter().enumerate() {
        let avatar = &state.avatars[index];
        let can_yield = (parked || local && avatar.travelling) && !avatar.transmitting;
        let Some(from) = previous[index].filter(|_| can_yield) else {
            continue;
        };
        let current = placed[rank];
        obstacles.clear();
        obstacles.extend(placed.iter().enumerate().filter_map(|(other, &disc)| {
            (other != rank
                && (parked || order[other].2)
                && disc.centre.distance(current.centre)
                    < current.radius + disc.radius + 2.0 * max_step)
                .then_some(disc)
        }));
        if !obstacles.iter().any(|disc| {
            disc.centre.distance(current.centre) < current.radius + disc.radius
        }) {
            continue;
        }
        let avatar = &mut state.avatars[index];
        let y = avatar.position.y;
        let nominal = current.radius / avatar.projection_scale;
        let world = |point: Vec2| Vec3::new(point.x, y, point.y);
        let radius_at = |point| nominal * slots::projection_scale(world(point));
        let point = clear_position(
            from.xz(),
            current.centre,
            max_step,
            &obstacles,
            radius_at,
            |point| bounds.contains(point),
        );
        avatar.position = world(point);
        avatar.projection_scale = slots::projection_scale(avatar.position);
        placed[rank].centre = point;
        placed[rank].radius = radius_at(point);
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

fn visible_discs(sim: &Simulation) -> Vec<(usize, Vec2, f32)> {
    let (focus, distance) = sim.view();
    let eye = focus + view_direction() * distance;
    let screen = |point| {
        project(eye, focus, &[point])
            .next()
            .flatten()
            .map(|p| p * framing::VIEWPORT * 0.5)
    };
    let base = sim.state.geometry.avatar_radius * sim.scale() * sim.avatar_disc_scale();
    sim.avatars()
        .iter()
        .enumerate()
        .zip(sim.member_scales())
        .filter(|((_, avatar), _)| avatar.presence > 0.001)
        .filter_map(|((index, avatar), member_scale)| {
            let radius = disc_radius(avatar, base, member_scale);
            if radius <= 0.0 {
                return None;
            }
            let centre = screen(avatar.position)?;
            let edge = screen(avatar.position + Vec3::X * radius)?;
            Some((index, centre, centre.distance(edge)))
        })
        .collect()
}

fn discs_overlap(centre: Vec2, radius: f32, other: Vec2, other_radius: f32) -> bool {
    centre.distance(other) + 0.01 < radius + other_radius
}

pub(in crate::model) fn walk_contacts(
    sim: &Simulation,
    walkers: &[(usize, usize)],
) -> Vec<(usize, usize)> {
    let mut contacts: Vec<_> = walk_contact_pairs(sim, walkers)
        .into_iter()
        .map(|(walk, _)| walk)
        .collect();
    contacts.dedup();
    contacts
}

pub(super) fn walk_contact_pairs(
    sim: &Simulation,
    walkers: &[(usize, usize)],
) -> Vec<((usize, usize), usize)> {
    if walkers.is_empty() {
        return Vec::new();
    }
    let discs = visible_discs(sim);
    let parked_discs: Vec<_> = discs
        .iter()
        .filter(|&&(other, ..)| parked(&sim.avatars()[other]))
        .collect();
    let mut contacts = Vec::new();
    for &(commit, author) in walkers {
        let Some(&(_, centre, radius)) =
            discs.iter().find(|&&(index, ..)| index == author)
        else {
            continue;
        };
        for &&(other, point, other_radius) in &parked_discs {
            if other != author && discs_overlap(centre, radius, point, other_radius) {
                contacts.push(((commit, author), other));
            }
        }
    }
    contacts
}

impl Audit {
    pub(crate) fn observe(&mut self, sim: &Simulation) {
        let labels: Vec<_> = sim
            .labels_for_detail(|_| true)
            .into_iter()
            .filter(|label| label.opacity > 0.001 && !label.text.is_empty())
            .map(|label| (label_bounds(sim, &label), label.text))
            .collect();
        let discs = visible_discs(sim);
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
                if discs_overlap(centre, radius, point, other_radius) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{contributors::Mappings, model::Options, theme::Theme, timeline};

    #[test]
    fn a_crowded_arrival_can_fit_between_two_disc_boundaries() {
        let obstacles = [
            (Vec2::new(-0.9, 0.6), 0.36),
            (Vec2::new(0.6, 0.6), 0.36),
            (Vec2::new(0.0, 0.3), 0.8),
            (Vec2::new(0.0, -0.9), 0.36),
        ]
        .map(|(centre, radius)| Disc {
            centre,
            radius,
            blocks_walkers: true,
            transmitting: false,
        });
        let radius = 0.35;
        let point = clear_position(
            Vec2::ZERO,
            Vec2::ZERO,
            1.0,
            &obstacles,
            |_| radius,
            |_| true,
        );

        assert!(point.length() <= 1.0);
        for disc in obstacles {
            assert!(point.distance(disc.centre) >= radius + disc.radius);
        }
    }

    #[test]
    fn a_fading_in_seat_leaves_room_for_the_visible_avatar() {
        let mut sim = Simulation::new(
            vec![timeline::synthetic().remove(0)],
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        sim.advance_to(60, 60);
        let state = &mut *sim.state;
        let base = state.geometry.avatar_radius
            * state.scale()
            * state.avatar_scale
            * state.avatar_size;
        let incumbent = &mut state.avatars[0];
        let position = Vec3::Y * super::super::MIN_AVATAR_HEIGHT;
        incumbent.position = position;
        incumbent.group = None;
        incumbent.transmitting = true;
        incumbent.presence = 1.0;
        incumbent.lifecycle_scale = 1.0;
        incumbent.projection_scale = slots::projection_scale(position);

        let mut newcomer = incumbent.clone();
        newcomer.transmitting = false;
        newcomer.presence = 1.0 / 24.0;
        newcomer.position += Vec3::X * (1.5 * base);
        newcomer.projection_scale = slots::projection_scale(newcomer.position);
        state.avatars.push(newcomer);
        clear_seat_transitions(
            state,
            &travel::Plan::default(),
            &[Some(position), None],
            0.5,
        );

        let [incumbent, newcomer] = &mut state.avatars[..] else {
            unreachable!();
        };
        newcomer.presence = 1.0;
        assert_eq!(incumbent.position, position);
        assert!(
            incumbent.position.distance(newcomer.position)
                >= disc_radius(incumbent, base, 1.0) + disc_radius(newcomer, base, 1.0)
        );
    }
}
