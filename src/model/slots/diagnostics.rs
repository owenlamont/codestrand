use serde::Serialize;

use super::{HALF, Simulation, Target, framing, project, segment, view_direction};
use bevy::math::{Rect, Vec3};

#[derive(Default, Serialize)]
pub(crate) struct MovingBeams {
    pub label_label_pairs: u64,
    pub label_joint_avoidable_frames: u64,
    pub label_joint_exhausted_frames: u64,
    pub parked_close_pairs: u64,
    pub label_nearest_violations: u64,
    pub label_parked_violations: u64,
    pub label_avoidable_parked: u64,
    pub label_parked_max_distance: f32,
    pub label_clipped_frames: u64,
    pub label_violation_examples: Vec<serde_json::Value>,
    pub label_gap_max: f32,
    pair_frames: u64,
    endpoint_inside_foreign: u64,
    origin_inside_foreign: u64,
    first: Option<serde_json::Value>,
    missing_tile_beams: u64,
    travelling_disc_foreign_tile: u64,
    travelling_avatar_overlaps: u64,
    avatar_overlap_max_px: f32,
    worst_overlap_tick: u64,
    travelling_centre_interior: u64,
    travelling_off_network: u64,
    maximum_travel_speed: f32,
    first_disc: Option<serde_json::Value>,
    first_overlap: Option<serde_json::Value>,
    observed_beams: u64,
    avatar_content_frames: u64,
    name_content_frames: u64,
    avatar_content_max_px: f32,
    name_content_max_px: f32,
    worst_avatar_tick: u64,
    worst_name_tick: u64,
    visible_avatar_frames: u64,
    visible_name_frames: u64,
    #[serde(skip)]
    previous: std::collections::BTreeMap<usize, (u64, bevy::math::Vec2, bool)>,
}

impl MovingBeams {
    fn overlap_depth(&mut self, depth: f32, tick: u64) {
        if depth > self.avatar_overlap_max_px {
            self.avatar_overlap_max_px = depth;
            self.worst_overlap_tick = tick;
        }
    }
    #[cfg(test)]
    pub(in crate::model) fn assert_clear(&self, speed: f32) {
        assert_eq!(self.pair_frames, 0, "beam/foreign tile intersections");
        assert!(
            self.observed_beams > 0,
            "diagnostic must observe live beams"
        );
        assert_eq!(self.missing_tile_beams, 0, "beam before tile birth");
        assert_eq!(
            self.travelling_centre_interior, 0,
            "route cut across tile interior"
        );
        assert_eq!(
            self.travelling_off_network, 0,
            "travel outside gutter centrelines"
        );

        assert!(
            self.maximum_travel_speed <= speed + 0.1,
            "maximum travel speed {} exceeds {speed}",
            self.maximum_travel_speed
        );
    }
    fn observe_content(
        &mut self,
        state: &crate::model::State,
        avatar: &crate::model::Avatar,
        point: bevy::math::Vec2,
        radius: f32,
        content: &[(usize, Rect)],
    ) {
        if avatar.presence > 0.0 {
            self.visible_avatar_frames += 1;
            let depth = content
                .iter()
                .map(|(_, r)| {
                    (radius - point.distance(point.clamp(r.min, r.max))).max(0.0)
                })
                .fold(0.0_f32, f32::max);
            self.avatar_content_frames += u64::from(depth > 0.001);
            if depth > self.avatar_content_max_px {
                self.avatar_content_max_px = depth;
                self.worst_avatar_tick = state.tick;
            }
            if let Some(name) = &avatar.name_visibility.placement {
                self.visible_name_frames += 1;
                let bounds = Rect::from_corners(
                    name.point
                        - bevy::math::Vec2::Y
                            * (state.geometry.label_height * name.scale + 4.0),
                    name.point
                        + bevy::math::Vec2::X
                            * (8.0
                                + 8.6
                                    * crate::model::count(name.text.chars().count())
                                    * name.scale),
                );
                let depth = content
                    .iter()
                    .map(|(_, r)| bounds.intersect(*r).size().min_element().max(0.0))
                    .fold(0.0_f32, f32::max);
                self.name_content_frames += u64::from(depth > 0.001);
                if depth > self.name_content_max_px {
                    self.name_content_max_px = depth;
                    self.worst_name_tick = state.tick;
                }
            }
        }
    }
    fn classify_label_violation(
        &mut self,
        state: &crate::model::State,
        other: usize,
        body: bevy::math::Vec2,
        point: bevy::math::Vec2,
        moving: bool,
    ) {
        if moving {
            return;
        }
        self.label_parked_max_distance =
            self.label_parked_max_distance.max(body.distance(point));
        let name = state.avatars[other]
            .name_visibility
            .placement
            .as_ref()
            .unwrap();
        let size = crate::model::names::rect(
            bevy::math::Vec2::ZERO,
            &name.text,
            name.scale,
            state.geometry.label_height,
        )
        .size();
        let distance = state.avatar_radius() + 4.0 / 3.0;
        let possible = (0..32_u8).any(|attachment| {
            let rect = super::labels::tangent_rect(
                body,
                size,
                distance,
                f32::from(attachment) * std::f32::consts::TAU / 32.0,
            );
            rect.min.cmpge(-HALF + bevy::math::Vec2::splat(0.5)).all()
                && rect.max.cmple(HALF - bevy::math::Vec2::splat(0.5)).all()
                && state
                    .avatars
                    .iter()
                    .enumerate()
                    .filter(|(i, a)| *i != other && a.presence > 0.0)
                    .all(|(_, a)| {
                        let p = project(
                            state.focus + view_direction() * state.distance,
                            state.focus,
                            &[a.position],
                        )
                        .next()
                        .flatten()
                        .unwrap()
                            * HALF;
                        p.distance(p.clamp(rect.min, rect.max)) > distance + 0.001
                    })
        });
        self.label_avoidable_parked += u64::from(possible);
    }
    fn record_label_violation(&mut self, example: serde_json::Value) {
        if self.label_violation_examples.len() < 12 {
            self.label_violation_examples.push(example);
        }
    }
    fn observe_labels(
        &mut self,
        simulation: &Simulation,
        author: usize,
        point: bevy::math::Vec2,
        radius: f32,
        projected: &[Option<bevy::math::Vec2>],
    ) {
        let state = &simulation.state;
        let avatar = &state.avatars[author];
        if avatar.presence <= 0.0 {
            return;
        }
        let bounds = |name: &crate::model::names::Placement| {
            let size = crate::model::names::rect(
                bevy::math::Vec2::ZERO,
                &name.text,
                name.scale,
                state.geometry.label_height,
            )
            .size();
            Rect::from_corners(
                name.point - bevy::math::Vec2::Y * size.y,
                name.point + bevy::math::Vec2::X * size.x,
            )
        };
        let distance = |rect: Rect| {
            (point.distance(point.clamp(rect.min, rect.max)) - radius).max(0.0)
        };
        if let Some(name) = &avatar.name_visibility.placement {
            let gap = distance(bounds(name));
            self.label_gap_max = self.label_gap_max.max(gap);
            let rect = bounds(name);
            self.label_clipped_frames += u64::from(
                rect.min.cmplt(-HALF + bevy::math::Vec2::splat(0.25)).any()
                    || rect.max.cmpgt(HALF - bevy::math::Vec2::splat(0.25)).any(),
            );
            for (i, other) in state
                .avatars
                .iter()
                .enumerate()
                .filter(|(i, a)| *i != author && a.presence > 0.0)
            {
                if let Some(other_name) = &other.name_visibility.placement
                    && distance(bounds(other_name)) <= gap + 0.0001
                {
                    self.label_nearest_violations += 1;
                    let moving =
                        simulation.travel.travelling(author, simulation.state.tick)
                            || simulation.travel.travelling(i, simulation.state.tick);
                    self.label_parked_violations += u64::from(!moving);
                    let other_body = projected[i].unwrap() * HALF;
                    self.classify_label_violation(state, i, other_body, point, moving);
                    self.record_label_violation(serde_json::json!({"tick":state.tick,"author":author,"other":i,"moving":moving,"distance":projected[i].map(|p| (p * HALF).distance(point))}));
                    break;
                }
            }
        }
        for (other, position) in projected.iter().enumerate().skip(author + 1) {
            self.parked_close_pairs += u64::from(
                state.avatars[other].presence > 0.0
                    && !simulation.travel.travelling(author, simulation.state.tick)
                    && !simulation.travel.travelling(other, simulation.state.tick)
                    && position
                        .is_some_and(|p| (p * HALF).distance(point) < 3.0 * radius),
            );
        }
    }
    fn observe_label_pairs(&mut self, state: &crate::model::State) {
        let before = self.label_label_pairs;
        self.label_joint_exhausted_frames += u64::from(
            state
                .avatars
                .iter()
                .any(|a| a.presence > 0.0 && a.name_visibility.joint_exhausted),
        );
        let labels: Vec<_> = state
            .avatars
            .iter()
            .filter(|a| a.presence > 0.0)
            .filter_map(|a| a.name_visibility.placement.as_ref())
            .map(|name| {
                let size = crate::model::names::rect(
                    bevy::math::Vec2::ZERO,
                    &name.text,
                    name.scale,
                    state.geometry.label_height,
                )
                .size();
                Rect::from_corners(
                    name.point - bevy::math::Vec2::Y * size.y,
                    name.point + bevy::math::Vec2::X * size.x,
                )
            })
            .collect();
        for (i, rect) in labels.iter().enumerate() {
            self.label_label_pairs += u64::try_from(
                labels[i + 1..]
                    .iter()
                    .filter(|other| !rect.intersect(**other).is_empty())
                    .count(),
            )
            .expect("bounded avatars");
        }
        self.label_joint_avoidable_frames += u64::from(
            self.label_label_pairs > before
                && state
                    .avatars
                    .iter()
                    .filter(|a| a.presence > 0.0)
                    .all(|a| a.name_visibility.joint_clear),
        );
    }
    fn observe_avatars(&mut self, simulation: &Simulation) {
        let state = &simulation.state;
        if state.layout.repositories.is_empty() {
            return;
        }
        let radius = state.avatar_radius();
        let content = super::content_obstacles(state);
        let projected: Vec<_> = project(
            view_direction() * framing::DISTANCE,
            Vec3::ZERO,
            &state.avatars.iter().map(|a| a.position).collect::<Vec<_>>(),
        )
        .collect();
        for (author, avatar) in state.avatars.iter().enumerate() {
            let Some(point) = projected[author].map(|p| p * HALF) else {
                continue;
            };
            self.observe_labels(simulation, author, point, radius, &projected);
            self.observe_content(state, avatar, point, radius, &content);
            let moving = avatar.presence > 0.0
                && simulation.travel.travelling(author, state.tick);
            if moving && simulation.travel.walking(author, state.tick) {
                let cells: Vec<_> = state
                    .layout
                    .repositories
                    .iter()
                    .filter_map(|r| {
                        r.gutter
                            .map(|t| Rect::from_corners(t.low * HALF, t.high * HALF))
                    })
                    .collect();
                self.travelling_off_network +=
                    u64::from(!super::super::travel::on_network(point, &cells));
            }
            if let Some((before, position, was_moving)) =
                self.previous.insert(author, (state.tick, point, moving))
                && (moving || was_moving)
                && state.tick > before
            {
                self.maximum_travel_speed = self.maximum_travel_speed.max(
                    point.distance(position)
                        / crate::model::tick_time(state.tick - before),
                );
            }
            for (other, position) in projected.iter().enumerate().skip(author + 1) {
                if avatar.presence > 0.0
                    && state.avatars[other].presence > 0.0
                    && position
                        .is_some_and(|p| (p * HALF).distance(point) < 2.0 * radius)
                {
                    self.travelling_avatar_overlaps += 1;
                    let depth =
                        2.0 * radius - (position.unwrap() * HALF).distance(point);
                    self.overlap_depth(depth, state.tick);
                    self.first_overlap.get_or_insert_with(|| serde_json::json!({"frame":state.tick,"author":author,"other":other,"point":point.to_array(),"other_point":position.map(|p| (p * HALF).to_array()),"radius":radius,"distance":position.map(|p| (p * HALF).distance(point)),"sample":simulation.travel.sample(author,state.tick).map(|(p,a)| (p.to_array(),a)),"other_sample":simulation.travel.sample(other,state.tick).map(|(p,a)| (p.to_array(),a))}));
                }
            }
        }
        for (author, avatar) in state.avatars.iter().enumerate().filter(|(i, a)| {
            a.presence > 0.0 && simulation.travel.travelling(*i, state.tick)
        }) {
            let Some(point) = projected[author].map(|p| p * HALF) else {
                continue;
            };
            for (index, tile) in state
                .layout
                .repositories
                .iter()
                .enumerate()
                .filter_map(|(i, r)| r.tile.map(|t| (i, t)))
            {
                self.travelling_centre_interior += u64::from(
                    crate::model::travel::in_guard(tile, radius + 0.05, point),
                );
                if index == avatar.repository {
                    continue;
                }
                let nearest = point.clamp(tile.low * HALF, tile.high * HALF);
                if point.distance(nearest) > radius {
                    continue;
                }
                self.travelling_disc_foreign_tile += 1;
                self.first_disc.get_or_insert_with(|| serde_json::json!({"frame":state.tick,"author":author,"commit":avatar.commit,"point":point.to_array(),"radius":radius,"foreign":index,"tile":[tile.low.to_array(),tile.high.to_array()],"travel_sample":simulation.travel.sample(author,state.tick).map(|(p,a)| (p.to_array(),a))}));
            }
        }
    }

    pub(crate) fn observe(&mut self, simulation: &Simulation) {
        let state = &simulation.state;
        if state.layout.repositories.is_empty() {
            return;
        }
        self.observe_avatars(simulation);
        self.observe_label_pairs(state);
        for (pulse, target) in state
            .pulses
            .iter()
            .filter(|p| {
                simulation.pulse_beams_visible(p)
                    && super::super::envelope(
                        simulation.seconds() - p.start,
                        p.end - p.start,
                    ) > 0.0
            })
            .flat_map(|p| p.targets.iter().map(move |&(target, ..)| (p, target)))
        {
            let Some(to) = state.point(target) else {
                continue;
            };
            self.observed_beams += 1;
            let own = match target {
                Target::File(id) => {
                    state
                        .layout
                        .dir(state.layout.file(id).expect("existing beam file").dir)
                        .repository
                }
                Target::Dir(index) => state.layout.dir(index).repository,
            };
            self.missing_tile_beams +=
                u64::from(state.layout.repositories[own].tile.is_none());
            let points = [state.avatars[pulse.author].position, to + Vec3::Y * 0.12];
            let mut projected =
                project(view_direction() * framing::DISTANCE, Vec3::ZERO, &points)
                    .flatten();
            let (Some(from), Some(to)) = (projected.next(), projected.next()) else {
                continue;
            };
            for (index, region) in state.layout.repositories.iter().enumerate() {
                let Some(tile) = region.tile.filter(|_| index != own) else {
                    continue;
                };
                if !segment(
                    from * HALF,
                    to * HALF,
                    Rect::from_corners(tile.low * HALF, tile.high * HALF),
                ) {
                    continue;
                }
                self.pair_frames += 1;
                self.endpoint_inside_foreign +=
                    u64::from(to.cmpgt(tile.low).all() && to.cmplt(tile.high).all());
                self.origin_inside_foreign += u64::from(
                    from.cmpgt(tile.low).all() && from.cmplt(tile.high).all(),
                );
                self.first.get_or_insert_with(|| serde_json::json!({"frame":state.tick,"seconds":simulation.seconds(),"commit":simulation.commit(),"target_repository":own,"crossed_repository":index,"from":from.to_array(),"to":to.to_array(),"target_tile":state.layout.repositories[own].tile.map(|t| [t.low.to_array(),t.high.to_array()]),"tile_low":tile.low.to_array(),"tile_high":tile.high.to_array()}));
            }
        }
    }
}
