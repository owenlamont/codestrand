use serde::Serialize;

use super::{HALF, Simulation, Target, framing, project, segment, view_direction};
use bevy::math::{Rect, Vec3};

#[derive(Default, Serialize)]
pub(crate) struct MovingBeams {
    pair_frames: u64,
    endpoint_inside_foreign: u64,
    origin_inside_foreign: u64,
    first: Option<serde_json::Value>,
    missing_tile_beams: u64,
    travelling_disc_foreign_tile: u64,
    travelling_avatar_overlaps: u64,
    maximum_travel_speed: f32,
    first_disc: Option<serde_json::Value>,
    observed_beams: u64,
    #[serde(skip)]
    previous: std::collections::BTreeMap<usize, (u64, bevy::math::Vec2, bool)>,
}

impl MovingBeams {
    #[cfg(test)]
    pub(in crate::model) fn assert_clear(&self, speed: f32) {
        assert_eq!(self.pair_frames, 0, "beam/foreign tile intersections");
        assert!(
            self.observed_beams > 0,
            "diagnostic must observe live beams"
        );
        assert_eq!(self.missing_tile_beams, 0, "beam before tile birth");
        assert_eq!(
            self.travelling_disc_foreign_tile, 0,
            "travelling disc inside foreign tile"
        );
        assert_eq!(
            self.travelling_avatar_overlaps, 0,
            "travelling avatars overlap"
        );
        assert!(
            self.maximum_travel_speed <= speed + 0.1,
            "maximum travel speed {} exceeds {speed}",
            self.maximum_travel_speed
        );
    }
    fn observe_avatars(&mut self, simulation: &Simulation) {
        let state = &simulation.state;
        if state.layout.repositories.is_empty() {
            return;
        }
        let radius = 6.5 * state.avatar_scale + 1.0;
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
            let moving = avatar.presence > 0.0
                && simulation.travel.travelling(author, state.tick);
            if let Some((before, position, was_moving)) =
                self.previous.insert(author, (state.tick, point, moving))
                && moving
                && was_moving
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
                    && (moving || simulation.travel.travelling(other, state.tick))
                    && position
                        .is_some_and(|p| (p * HALF).distance(point) < 2.0 * radius)
                {
                    self.travelling_avatar_overlaps += 1;
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
