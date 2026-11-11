use bevy::math::{Rect, Vec2, Vec3};
use serde::Serialize;

use super::{Simulation, framing, project, titles, view_direction};

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
            .map(|label| (titles::label_bounds(sim, &label), label.text))
            .collect();
        let discs: Vec<_> = sim
            .avatars()
            .iter()
            .enumerate()
            .filter(|(_, avatar)| avatar.presence > 0.001)
            .filter_map(|(index, avatar)| {
                let radius = sim.state.geometry.avatar_radius
                    * sim.scale()
                    * sim.avatar_disc_scale()
                    * avatar.projection_scale
                    * avatar.lifecycle_scale
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
                    self.examples.push(serde_json::json!({"seconds":sim.seconds(),"author":author,"text":text,"centre":centre.to_array(),"radius":radius}));
                }
            }
        }
        for (index, &(author, centre, radius)) in discs.iter().enumerate() {
            for &(other, point, other_radius) in &discs[index + 1..] {
                if centre.distance(point) + 0.01 < radius + other_radius {
                    self.avatar_avatar_observations += 1;
                    self.parked_avatar_observations += u64::from(
                        !sim.travel.travelling(author, sim.state.tick)
                            && !sim.travel.travelling(other, sim.state.tick),
                    );
                }
            }
        }
    }
}

fn overlaps(centre: Vec2, radius: f32, rect: Rect) -> bool {
    centre.distance(centre.clamp(rect.min, rect.max)) + 0.01 < radius
}
