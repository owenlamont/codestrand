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
const EDGE_INSET: f32 = 4.0;
const ELISION_HYSTERESIS: f32 = 2.0;
const STEP: u32 = 15;
const CHANGE_COST: f64 = 20_000.0;

#[derive(Clone, Debug)]
pub(super) struct Title {
    pub text: String,
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
                ground(
                    super::framing::field_to_world(point / HALF)
                        * (1.0 - height / super::framing::DISTANCE),
                ) + Vec3::Y * height,
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
    growth: f32,
) -> Option<Title> {
    stable_title(tile, label, height, corner, growth, None)
}

fn stable_title(
    tile: crate::layout::packing::Tile,
    label: &str,
    height: f32,
    corner: u8,
    growth: f32,
    previous_length: Option<usize>,
) -> Option<Title> {
    let bounds = Rect::from_corners(tile.low * HALF, tile.high * HALF);
    let scale = super::LabelKind::Repository.font_scale() * growth;
    let width = 8.6 * scale * super::count(label.chars().count()) + 8.01;
    let room = ((bounds.width() - width) * 0.5)
        .min((bounds.height() - height * scale - 4.0) * 0.5);
    let inset = INSET.min(room.max(EDGE_INSET));
    inset_title(
        bounds.inflate(-inset),
        label,
        height,
        corner,
        scale,
        growth,
        previous_length,
    )
}

fn inset_title(
    tile: Rect,
    label: &str,
    height: f32,
    corner: u8,
    scale: f32,
    growth: f32,
    previous_length: Option<usize>,
) -> Option<Title> {
    let character_width = 8.6 * scale;
    let available = tile.width() - 8.0;
    let fits = (available / character_width).floor();
    if fits < 1.0 || tile.height() < height * scale + 4.0 {
        return None;
    }
    let stable_fit = ((available - ELISION_HYSTERESIS * growth) / character_width)
        .floor()
        .max(1.0);
    let fits = previous_length.map_or(fits, |length| {
        fits.min(stable_fit.max(super::count(length)))
    });
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a positive whole character count bounded by the tile width"
    )]
    let text = super::names::elide(label, fits as usize);
    let size = super::names::rect(Vec2::ZERO, &text, scale, height).size();
    let point = Vec2::new(
        if corner & 1 == 0 {
            tile.min.x
        } else {
            let full_width =
                8.0 + character_width * super::count(label.chars().count());
            (tile.max.x - full_width).max(tile.min.x)
        },
        if corner & 2 == 0 {
            tile.min.y + size.y
        } else {
            tile.max.y
        },
    );
    Some(Title {
        text,
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
                self.state.field_growth,
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
        let _phase = self.trace.phase(super::trace::Phase::PlanTitles);
        let mut costs = vec![Vec::new(); self.state.layout.repositories.len()];
        let visuals = std::mem::replace(&mut self.visuals, super::Visuals::Titles);
        self.restart();
        let frames = super::frames_for(self.duration(), 60).expect("bounded schedule");
        let first = if self.state.tick == 0 {
            0
        } else {
            u32::try_from(self.state.tick).expect("bounded window") / STEP + 1
        };
        let begin = first * STEP;
        let last = (frames + STEP) / STEP * STEP;
        self.trace
            .count_replay(u64::from(last).saturating_sub(self.state.tick));
        self.trace.count(
            super::trace::Counter::TitleCorners,
            (begin..=frames + STEP).step_by(STEP as usize).count() * costs.len() * 4,
        );
        for frame in (begin..=frames + STEP).step_by(STEP as usize) {
            self.progress.report_range(
                crate::progress::Stage::Titles,
                frame.into(),
                begin.into(),
                last.into(),
            );
            self.advance_to(frame, 60);
            let blocked = obstacles(self);
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
                        &rays,
                        &bodies,
                    )
                });
                costs[index].push(row);
            }
        }
        self.title_plan = costs
            .iter_mut()
            .enumerate()
            .map(|(index, row)| {
                let prefix = self.title_plan.get(index).map_or(&[][..], |p| {
                    &p[..(first as usize)
                        .saturating_sub(self.title_start)
                        .min(p.len())]
                });
                if let Some(&corner) = prefix.last() {
                    let mut fixed = [f64::INFINITY; 4];
                    fixed[usize::from(corner)] = 0.0;
                    row.insert(0, fixed);
                }
                let planned = plan(row, CHANGE_COST);
                prefix
                    .iter()
                    .copied()
                    .chain(planned.into_iter().skip(usize::from(!prefix.is_empty())))
                    .collect()
            })
            .collect();
        self.trace.add_ticks(&mut self.tick_times);
        self.visuals = visuals;
        self.restart();
    }
    pub(super) fn place_titles(&mut self) {
        let state = &mut *self.state;
        let sample =
            usize::try_from(state.tick / u64::from(STEP)).expect("bounded tick");
        state
            .titles
            .retain(|&index, _| state.layout.repositories[index].tile.is_some());
        for (index, region) in state.layout.repositories.iter().enumerate() {
            let Some(tile) = region.tile else { continue };
            let row = self.title_plan.get(index).map_or(&[][..], Vec::as_slice);
            let at = |sample: usize| {
                sample
                    .checked_sub(self.title_start)
                    .and_then(|sample| row.get(sample))
                    .copied()
                    .or_else(|| row.last().copied())
                    .unwrap_or(0)
            };
            let corner = at(sample);
            let previous = state
                .titles
                .get(&index)
                .map(|title| title.text.chars().count());
            let Some(mut current) = stable_title(
                tile,
                &region.label,
                state.geometry.label_height,
                corner,
                state.field_growth,
                previous,
            ) else {
                state.titles.remove(&index);
                continue;
            };
            if let Some(change) =
                (1..=sample).rev().take(2).find(|&t| at(t) != at(t - 1))
            {
                let age = state.tick
                    - u64::try_from(change).expect("bounded sample") * u64::from(STEP);
                if age < 18 {
                    current.old = title(
                        tile,
                        &region.label,
                        state.geometry.label_height,
                        at(change - 1),
                        state.field_growth,
                    )
                    .map(|t| t.point);
                    current.opacity = super::ticks_duration(age) / 0.3;
                }
            }
            state.titles.insert(index, current);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_extension_preserves_a_title_fade() {
        let mut sim = Simulation::two_synthetic_repositories();
        sim.title_plan[0].fill(0);
        sim.title_plan[0][4..].fill(1);
        sim.advance_to(65, 60);
        assert!(sim.state.titles[&0].old.is_some());
        let saved = sim.state.clone();
        let expected: Vec<_> = (66..=78)
            .map(|tick| {
                sim.advance_to(tick, 60);
                format!("{:?}", sim.state.titles)
            })
            .collect();
        sim.window = Some(super::super::windows::Planning {
            state: saved.0,
            end: sim.duration(),
            services: Vec::new(),
            captured: Vec::new(),
        });
        sim.plan_titles();
        for (tick, expected) in (66..=78).zip(expected) {
            sim.advance_to(tick, 60);
            assert_eq!(format!("{:?}", sim.state.titles), expected);
        }
        sim.assert_stitched_travel_rules();
    }
    #[test]
    fn raised_titles_project_onto_their_ground_point_in_the_default_view() {
        let title = title(
            crate::layout::packing::Tile::viewport(),
            "repo",
            20.0,
            0,
            1.0,
        )
        .unwrap();
        let screen = |height: f32| {
            let point = title.positions(height).next().unwrap().0;
            super::super::framing::project_at(Vec2::new(point.x, point.z), height)
        };
        assert!(screen(0.0).distance(screen(1.6)) < 1e-5);
        assert!(screen(0.0).distance(screen(3.2)) < 1e-5);
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
            let screen =
                |p: &Vec3| crate::model::framing::project_at(Vec2::new(p.x, p.z), p.y);
            assert!(screen(low).distance(screen(high)) < 1e-5);
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
    fn titles_share_one_size_and_elide_to_fit_narrow_tiles() {
        let tile = |width: f32| crate::layout::packing::Tile {
            low: Vec2::new(-width, -0.5),
            high: Vec2::new(width, 0.5),
        };
        let label = "a-long-repository-name";
        let wide = title(tile(0.9), label, 20.0, 0, 1.0).unwrap();
        let narrow_tile = tile(0.12);
        let narrow = title(narrow_tile, label, 20.0, 0, 1.0).unwrap();
        assert_eq!(wide.text, label);
        assert!(narrow.text.ends_with('…'));
        assert_eq!(wide.size.y, narrow.size.y);
        assert!(narrow.rect().max.x <= narrow_tile.high.x * HALF.x - EDGE_INSET);
    }

    #[test]
    fn right_title_origin_moves_continuously_through_elision() {
        let scale = super::super::LabelKind::Repository.font_scale();
        let threshold = 8.0 + 8.6 * scale * 6.0;
        let title = |width: f32| {
            inset_title(
                Rect::new(-width, 0.0, 0.0, 30.0),
                "repository",
                10.0,
                1,
                scale,
                1.0,
                None,
            )
            .unwrap()
        };
        let narrow = title(threshold - 0.01);
        let wide = title(threshold + 0.01);
        assert_ne!(narrow.text, wide.text);
        assert!((narrow.point.x - wide.point.x).abs() < 0.03);
    }

    #[test]
    fn edge_inset_preserves_a_title_that_would_otherwise_be_elided() {
        let tile = crate::layout::packing::Tile {
            low: Vec2::ZERO,
            high: Vec2::new(135.0, 150.0) / HALF,
        };
        let caption = title(tile, "codestrand", 20.0, 0, 1.0).unwrap();
        assert_eq!(caption.text, "codestrand");
        assert!(caption.point.x < INSET);
        let mut previous = caption.point;
        for width in 136_u16..=170 {
            let growing = crate::layout::packing::Tile {
                high: Vec2::new(f32::from(width), 150.0) / HALF,
                ..tile
            };
            let next = title(growing, "codestrand", 20.0, 0, 1.0).unwrap();
            assert_eq!(next.text, "codestrand");
            assert!(next.point.distance(previous) < 1.0);
            previous = next.point;
        }
    }

    #[test]
    fn tiles_too_small_for_the_avatar_inset_keep_an_edge_title() {
        let tile = crate::layout::packing::Tile {
            low: Vec2::new(-0.2, -0.05),
            high: Vec2::new(0.2, 0.05),
        };
        let edge = title(tile, "repository", 20.0, 0, 1.0).unwrap();
        assert!(edge.rect().min.cmpge(tile.low * HALF + EDGE_INSET).all());
        assert!(edge.rect().max.cmple(tile.high * HALF - EDGE_INSET).all());
    }

    #[test]
    fn corners_leave_an_avatar_diameter_between_the_gutter_and_the_title() {
        let tile = crate::layout::packing::Tile {
            low: Vec2::splat(-0.5),
            high: Vec2::splat(0.5),
        };
        for corner in 0..4 {
            let t = title(tile, "repository", 24.0, corner, 1.0).unwrap();
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
