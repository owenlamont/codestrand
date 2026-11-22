use bevy::math::{Rect, Vec2, Vec3};
use serde::Serialize;

use super::{Simulation, framing, project, view_direction};

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
                let radius = sim.state.geometry.avatar_radius
                    * sim.scale()
                    * sim.avatar_disc_scale()
                    * avatar.projection_scale
                    * avatar.lifecycle_scale
                    * member_scale
                    * avatar.presence;
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
        let parked = |index: usize| {
            let avatar = &sim.state.avatars[index];
            !avatar.travelling && avatar.settle.is_none()
        };
        for (index, &(author, centre, radius)) in discs.iter().enumerate() {
            for &(other, point, other_radius) in &discs[index + 1..] {
                if centre.distance(point) + 0.01 < radius + other_radius {
                    self.avatar_avatar_observations += 1;
                    self.parked_avatar_observations +=
                        u64::from(parked(author) && parked(other));
                }
            }
        }
    }
}

fn overlaps(centre: Vec2, radius: f32, rect: Rect) -> bool {
    centre.distance(centre.clamp(rect.min, rect.max)) + 0.01 < radius
}
