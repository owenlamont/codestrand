use serde::Serialize;

use super::{HALF, Simulation, Target, framing, project, segment, view_direction};
use bevy::math::{Rect, Vec3};

#[derive(Default, Serialize)]
pub(crate) struct MovingBeams {
    pair_frames: u64,
    endpoint_inside_foreign: u64,
    origin_inside_foreign: u64,
    first: Option<serde_json::Value>,
}

impl MovingBeams {
    pub(crate) fn observe(&mut self, simulation: &Simulation) {
        let state = &simulation.state;
        for (pulse, target) in state
            .pulses
            .iter()
            .filter(|p| {
                super::super::envelope(simulation.seconds() - p.start, p.end - p.start)
                    > 0.0
            })
            .flat_map(|p| p.targets.iter().map(move |&(target, ..)| (p, target)))
        {
            let Some(to) = state.point(target) else {
                continue;
            };
            let own = match target {
                Target::File(id) => {
                    state
                        .layout
                        .dir(state.layout.file(id).expect("existing beam file").dir)
                        .repository
                }
                Target::Dir(index) => state.layout.dir(index).repository,
            };
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
