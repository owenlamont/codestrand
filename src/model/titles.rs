use super::{Simulation, ground, project, view_direction};
use bevy::math::{Rect, Vec2, Vec3};

pub(super) const HALF: Vec2 = Vec2::new(640.0, 360.0);
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
pub(super) fn label_bounds(sim: &Simulation, label: &super::Label) -> Rect {
    let point = screen(sim, label.position);
    let size = super::names::rect(
        Vec2::ZERO,
        &label.text,
        label.scale,
        sim.state.geometry.label_height,
    )
    .size();
    Rect::from_corners(point - Vec2::Y * size.y, point + Vec2::X * size.x)
}
pub(super) fn obstacles(sim: &Simulation) -> Vec<(usize, Rect)> {
    let mut result = Vec::new();
    for (_, file) in sim.state.layout.files() {
        let dir = sim.state.layout.dir(file.dir);
        let scale = file.visible_presence(dir);
        if scale <= 0.0 {
            continue;
        }
        let origin = ground(file.position);
        let points = super::box_corners(
            sim.state.geometry.file_min * scale,
            sim.state.geometry.file_max * scale,
        );
        let (low, high) = points.iter().map(|p| screen(sim, origin + *p)).fold(
            (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)),
            |(low, high), p| (low.min(p), high.max(p)),
        );
        result.push((dir.repository, Rect::from_corners(low, high)));
    }
    for label in sim.labels_for_detail(|_| true).iter().filter(|l| {
        matches!(
            l.kind,
            super::LabelKind::Landmark | super::LabelKind::Activity
        )
    }) {
        let rect = label_bounds(sim, label);
        if let Some(owner) = sim.state.layout.repositories.iter().position(|r| {
            r.tile.is_some_and(|t| {
                rect.center().cmpge(t.low * HALF).all()
                    && rect.center().cmple(t.high * HALF).all()
            })
        }) {
            result.push((owner, rect));
        }
    }
    result
}

const INSET: f32 = 34.0;
const STEP: u32 = 15;
const CHANGE_COST: f64 = 20_000.0;

#[derive(Clone, Debug)]
pub(super) struct Title {
    pub point: Vec2,
    pub size: Vec2,
    pub scale: f32,
    old: Option<Vec2>,
    opacity: f32,
}
impl Title {
    pub fn rect(&self) -> Rect {
        Rect::from_corners(
            self.point - Vec2::Y * self.size.y,
            self.point + Vec2::X * self.size.x,
        )
    }
    pub fn positions(&self, height: f32) -> impl Iterator<Item = (Vec3, f32)> {
        [
            (self.point, self.opacity),
            (
                self.old.unwrap_or(self.point),
                if self.old.is_some() {
                    1.0 - self.opacity
                } else {
                    0.0
                },
            ),
        ]
        .into_iter()
        .filter(|(_, opacity)| *opacity > 0.0)
        .map(move |(point, opacity)| {
            (
                ground(super::framing::field_to_world(point / HALF)) + Vec3::Y * height,
                opacity,
            )
        })
    }
}
fn title(
    tile: crate::layout::packing::Tile,
    label: &str,
    height: f32,
    corner: u8,
) -> Option<Title> {
    let tile = Rect::from_corners(tile.low * HALF, tile.high * HALF).inflate(-INSET);
    if tile.size().min_element() <= 8.0 {
        return None;
    }
    let full = super::names::rect(Vec2::ZERO, label, 1.0, height).size();
    let scale = ((tile.width() - 8.0) / (full.x - 8.0).max(1.0))
        .min((tile.height() - 4.0) / (full.y - 4.0).max(1.0))
        .clamp(0.0, 1.0);
    let size = super::names::rect(Vec2::ZERO, label, scale, height).size();
    let point = Vec2::new(
        if corner & 1 == 0 {
            tile.min.x
        } else {
            tile.max.x - size.x
        },
        if corner & 2 == 0 {
            tile.min.y + size.y
        } else {
            tile.max.y
        },
    );
    Some(Title {
        point,
        size,
        scale,
        old: None,
        opacity: 1.0,
    })
}
fn area(a: Rect, b: Rect) -> f32 {
    a.intersect(b).size().max(Vec2::ZERO).element_product()
}
fn plan(costs: &[[f64; 4]], change: f64) -> Vec<u8> {
    let Some(&first) = costs.first() else {
        return Vec::new();
    };
    let mut totals = first;
    let mut previous = vec![[0_u8; 4]; costs.len()];
    for (time, cost) in costs.iter().enumerate().skip(1) {
        let mut next = [0.0; 4];
        for (corner, total) in next.iter_mut().enumerate() {
            let from = (0..4)
                .min_by(|&a, &b| {
                    let value =
                        |i: usize| totals[i] + if i == corner { 0.0 } else { change };
                    value(a)
                        .total_cmp(&value(b))
                        .then((a != corner).cmp(&(b != corner)))
                        .then(a.cmp(&b))
                })
                .expect("four corners");
            *total =
                cost[corner] + totals[from] + if from == corner { 0.0 } else { change };
            previous[time][corner] = u8::try_from(from).expect("four corners");
        }
        totals = next;
    }
    let mut corner = (0..4)
        .min_by(|&a, &b| totals[a].total_cmp(&totals[b]).then(a.cmp(&b)))
        .expect("four corners");
    let mut result = vec![0; costs.len()];
    for time in (0..costs.len()).rev() {
        result[time] = u8::try_from(corner).expect("four corners");
        corner = usize::from(previous[time][corner]);
    }
    result
}
impl Simulation {
    fn corner_cost(
        &self,
        repository: usize,
        corner: u8,
        blocked: &[(usize, Rect)],
        names: &[Rect],
        rays: &[(Vec2, Vec2)],
        bodies: &[Vec2],
    ) -> f64 {
        let region = &self.state.layout.repositories[repository];
        let Some(t) = region.tile.and_then(|tile| {
            title(
                tile,
                &region.label,
                self.state.geometry.label_height,
                corner,
            )
        }) else {
            return 0.0;
        };
        let rect = t.rect().inflate(4.0);
        let cost = blocked
            .iter()
            .map(|(_, r)| {
                area(rect, *r)
                    + if rect.intersect(*r).is_empty() {
                        0.0
                    } else {
                        1_000.0
                    }
            })
            .sum::<f32>()
            + names.iter().map(|r| area(rect, *r) * 50.0).sum::<f32>()
            + bodies
                .iter()
                .map(|p| {
                    area(
                        rect,
                        Rect::from_center_size(
                            *p,
                            Vec2::splat(2.0 * self.state.avatar_radius()),
                        ),
                    ) * 20.0
                })
                .sum::<f32>()
            + super::count(
                rays.iter()
                    .filter(|(a, b)| super::slots::segment(*a, *b, rect))
                    .count(),
            ) * 1_000.0;
        f64::from(cost) * 0.25
    }
    pub(super) fn plan_titles(&mut self) {
        let mut costs = vec![Vec::new(); self.state.layout.repositories.len()];
        self.restart();
        let frames = super::frames_for(self.duration(), 60).expect("bounded schedule");
        for frame in (0..=frames + STEP).step_by(STEP as usize) {
            self.advance_to(frame, 60);
            let blocked = obstacles(self);
            let names: Vec<_> = self
                .labels_for_detail(|_| true)
                .iter()
                .filter(|l| l.kind == super::LabelKind::Avatar && l.opacity > 0.0)
                .map(|l| label_bounds(self, l))
                .collect();
            let rays: Vec<_> = self
                .beams()
                .iter()
                .map(|b| (screen(self, b.from), screen(self, b.to)))
                .collect();
            let bodies: Vec<_> = self
                .avatars()
                .iter()
                .filter(|a| a.presence > 0.0)
                .map(|a| screen(self, a.position))
                .collect();
            for (index, _) in self.state.layout.repositories.iter().enumerate() {
                let row = std::array::from_fn(|corner| {
                    self.corner_cost(
                        index,
                        u8::try_from(corner).expect("four corners"),
                        &blocked,
                        &names,
                        &rays,
                        &bodies,
                    )
                });
                costs[index].push(row);
            }
        }
        self.title_plan = costs.iter().map(|row| plan(row, CHANGE_COST)).collect();
        self.restart();
    }
    pub(super) fn place_titles(&mut self) {
        let sample =
            usize::try_from(self.state.tick / u64::from(STEP)).expect("bounded tick");
        self.state.titles.clear();
        for (index, region) in self.state.layout.repositories.iter().enumerate() {
            let Some(tile) = region.tile else { continue };
            let row = self.title_plan.get(index).map_or(&[][..], Vec::as_slice);
            let at = |sample: usize| {
                row.get(sample)
                    .copied()
                    .or_else(|| row.last().copied())
                    .unwrap_or(0)
            };
            let corner = at(sample);
            let Some(mut current) = title(
                tile,
                &region.label,
                self.state.geometry.label_height,
                corner,
            ) else {
                continue;
            };
            if let Some(change) =
                (1..=sample).rev().take(2).find(|&t| at(t) != at(t - 1))
            {
                let age = self.state.tick
                    - u64::try_from(change).expect("bounded sample") * u64::from(STEP);
                if age < 18 {
                    current.old = title(
                        tile,
                        &region.label,
                        self.state.geometry.label_height,
                        at(change - 1),
                    )
                    .map(|t| t.point);
                    current.opacity = super::tick_time(age) / 0.3;
                }
            }
            self.state.titles.insert(index, current);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn title_height_does_not_move_its_world_anchor() {
        let title =
            title(crate::layout::packing::Tile::viewport(), "repo", 20.0, 0).unwrap();
        let low = title.positions(1.6).next().unwrap().0;
        let high = title.positions(3.2).next().unwrap().0;
        assert_eq!(low.with_y(0.0), high.with_y(0.0));
    }

    #[test]
    fn repository_titles_sit_on_the_chosen_label_plane() {
        use crate::repositories::LabelPlane;
        let mut sim = crate::model::Simulation::two_synthetic_repositories();
        sim.advance_to(90, 30);
        let mut titles = |plane| -> Vec<Vec3> {
            sim.repository_label_plane = plane;
            sim.labels()
                .iter()
                .filter(|label| label.kind == crate::model::LabelKind::Repository)
                .map(|label| label.position)
                .collect()
        };
        let (files, raised) = (titles(LabelPlane::Files), titles(LabelPlane::Raised));
        assert_eq!(files.len(), 2);
        assert_eq!(raised.len(), 2);
        for (low, high) in files.iter().zip(&raised) {
            assert_eq!(low.y.to_bits(), 0.15_f32.to_bits());
            assert_eq!(high.y.to_bits(), crate::model::framing::LABEL_Y.to_bits());
            assert_eq!(low.with_y(0.0), high.with_y(0.0));
        }
    }

    #[test]
    fn whole_animation_plan_avoids_a_transient_switch_and_makes_one_necessary_change() {
        let costs = [
            [0.0, 1000.0, 1000.0, 1000.0],
            [2.0, 0.0, 1000.0, 1000.0],
            [0.0, 8.0, 1000.0, 1000.0],
            [1000.0, 0.0, 1000.0, 1000.0],
            [1000.0, 0.0, 1000.0, 1000.0],
        ];
        let result = plan(&costs, 10.0);
        assert_eq!(result, [0, 0, 0, 1, 1]);
        assert_eq!(result.windows(2).filter(|p| p[0] != p[1]).count(), 1);
    }
    #[test]
    fn corners_leave_an_avatar_diameter_between_the_gutter_and_the_title() {
        let tile = crate::layout::packing::Tile {
            low: Vec2::splat(-0.5),
            high: Vec2::splat(0.5),
        };
        for corner in 0..4 {
            let t = title(tile, "repository", 24.0, corner).unwrap();
            assert!(
                t.rect()
                    .min
                    .cmpge(tile.low * HALF + Vec2::splat(INSET - 0.001))
                    .all()
            );
            assert!(
                t.rect()
                    .max
                    .cmple(tile.high * HALF - Vec2::splat(INSET - 0.001))
                    .all()
            );
        }
    }
}
