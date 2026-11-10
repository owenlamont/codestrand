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

#[derive(Clone, Debug)]
pub(super) struct Title {
    pub point: Vec2,
    target: Vec2,
    content: Rect,
    tile: Rect,
    pub size: Vec2,
    pub scale: f32,
}
impl Title {
    pub fn rect(&self) -> Rect {
        Rect::from_corners(
            self.point - Vec2::Y * self.size.y,
            self.point + Vec2::X * self.size.x,
        )
    }
    pub fn position(&self, height: f32) -> Vec3 {
        ground(crate::layout::world_coordinates(
            super::framing::unproject_at(self.point / HALF, height),
        )) + Vec3::Y * height
    }
}
fn area(a: Rect, b: Rect) -> f32 {
    a.intersect(b).size().max(Vec2::ZERO).element_product()
}
fn clamp(point: Vec2, size: Vec2, tile: Rect) -> Vec2 {
    let low = tile.min + Vec2::Y * size.y;
    point.clamp(low, (tile.max - Vec2::X * size.x).max(low))
}
fn select(
    size: Vec2,
    tile: Rect,
    content: Rect,
    blocked: &[Rect],
    rays: &[(Vec2, Vec2)],
) -> Vec2 {
    let middle = content.center().x - size.x * 0.5;
    let below = clamp(Vec2::new(middle, content.min.y - 24.0), size, tile);
    let above = clamp(Vec2::new(middle, content.max.y + size.y + 8.0), size, tile);
    [below, above]
        .into_iter()
        .chain((0..=6_u8).flat_map(|x| {
            (0..=4_u8).map(move |y| {
                let low = tile.min + Vec2::Y * size.y;
                let high = tile.max - Vec2::X * size.x;
                Vec2::new(
                    low.x + (high.x - low.x) * f32::from(x) / 6.0,
                    low.y + (high.y - low.y) * f32::from(y) / 4.0,
                )
            })
        }))
        .min_by(|a, b| {
            let cost = |point: Vec2| {
                let rect = Rect::from_corners(
                    point - Vec2::Y * size.y,
                    point + Vec2::X * size.x,
                );
                (
                    blocked.iter().map(|r| area(rect, *r)).sum::<f32>(),
                    super::count(
                        rays.iter()
                            .filter(|(a, b)| super::slots::segment(*a, *b, rect))
                            .count(),
                    ),
                    point.distance_squared(below),
                )
            };
            let a = cost(*a);
            let b = cost(*b);
            a.0.total_cmp(&b.0)
                .then(a.1.total_cmp(&b.1))
                .then(a.2.total_cmp(&b.2))
        })
        .expect("title candidates")
}
fn waypoint(point: Vec2, target: Vec2, size: Vec2, tile: Rect, content: Rect) -> Vec2 {
    let guard = Rect::from_corners(
        content.min - Vec2::new(size.x + 4.0, 4.0),
        content.max + Vec2::new(4.0, size.y + 4.0),
    );
    let limits =
        Rect::from_corners(tile.min + Vec2::Y * size.y, tile.max - Vec2::X * size.x);
    let inside = |p: Vec2| p.cmpge(limits.min).all() && p.cmple(limits.max).all();
    let clear = |a, b| !super::slots::segment(a, b, guard.inflate(-0.01));
    if clear(point, target) {
        return target;
    }
    if point.cmpgt(guard.min + Vec2::splat(0.02)).all()
        && point.cmplt(guard.max - Vec2::splat(0.02)).all()
    {
        return [
            Vec2::new(point.x, guard.min.y),
            Vec2::new(point.x, guard.max.y),
            Vec2::new(guard.min.x, point.y),
            Vec2::new(guard.max.x, point.y),
        ]
        .into_iter()
        .filter(|p| inside(*p))
        .min_by(|a, b| {
            point
                .distance_squared(*a)
                .total_cmp(&point.distance_squared(*b))
        })
        .unwrap_or(target);
    }
    let corners = [
        guard.min,
        Vec2::new(guard.min.x, guard.max.y),
        guard.max,
        Vec2::new(guard.max.x, guard.min.y),
    ];
    let mut best = None;
    let mut length = f32::INFINITY;
    for a in corners
        .into_iter()
        .filter(|p| inside(*p) && clear(point, *p))
    {
        for b in corners
            .into_iter()
            .filter(|p| inside(*p) && clear(a, *p) && clear(*p, target))
        {
            let distance = point.distance(a) + a.distance(b) + b.distance(target);
            if distance < length {
                length = distance;
                best = Some(if point.distance(a) < 0.1 && clear(point, b) {
                    b
                } else {
                    a
                });
            }
        }
    }
    best.unwrap_or_else(|| {
        let centred = Vec2::new(target.x, point.y);
        if inside(centred) && clear(point, centred) {
            centred
        } else {
            point
        }
    })
}
fn move_clear(
    point: Vec2,
    target: Vec2,
    size: Vec2,
    tile: Rect,
    content: Rect,
    dt: f32,
) -> Vec2 {
    let destination = waypoint(point, target, size, tile, content);
    let desired = (destination - point) * (1.0 - (-20.0 * dt).exp());
    clamp(point + desired.clamp_length_max(480.0 * dt), size, tile)
}
impl Simulation {
    pub(super) fn place_titles(&mut self, dt: f32) {
        if self.state.layout.repositories.is_empty() {
            return;
        }
        let obstacles = obstacles(self);
        let rays: Vec<_> = self
            .beams()
            .iter()
            .map(|b| (screen(self, b.from), screen(self, b.to)))
            .collect();
        let names: Vec<_> = self
            .state
            .avatars
            .iter()
            .filter(|a| a.presence > 0.0)
            .filter_map(|a| a.name_visibility.placement.as_ref())
            .map(|n| {
                let size = super::names::rect(
                    Vec2::ZERO,
                    &n.text,
                    n.scale,
                    self.state.geometry.label_height,
                )
                .size();
                Rect::from_corners(
                    n.point - Vec2::Y * size.y,
                    n.point + Vec2::X * size.x,
                )
            })
            .collect();
        for (index, region) in self.state.layout.repositories.iter().enumerate() {
            let Some(tile) = region.tile else { continue };
            let tile =
                Rect::from_corners(tile.low * HALF, tile.high * HALF).inflate(-2.0);
            if tile.size().min_element() <= 8.0 {
                self.state.titles.remove(&index);
                continue;
            }
            let full = super::names::rect(
                Vec2::ZERO,
                &region.label,
                1.0,
                self.state.geometry.label_height,
            )
            .size();
            let scale = ((tile.width() - 8.0) / (full.x - 8.0).max(1.0))
                .min((tile.height() - 4.0) / (full.y - 4.0).max(1.0))
                .clamp(0.0, 1.0);
            let size = super::names::rect(
                Vec2::ZERO,
                &region.label,
                scale,
                self.state.geometry.label_height,
            )
            .size();
            let own: Vec<_> = obstacles
                .iter()
                .filter(|(owner, _)| *owner == index)
                .map(|(_, r)| *r)
                .collect();
            let content = own
                .iter()
                .copied()
                .reduce(|a, b| Rect::from_corners(a.min.min(b.min), a.max.max(b.max)))
                .unwrap_or(tile);
            let previous = self.state.titles.get(&index);
            let changed = previous.is_none_or(|p| {
                (p.content.min - content.min).abs().max_element() > 8.0
                    || (p.content.max - content.max).abs().max_element() > 8.0
                    || (p.tile.min - tile.min).abs().max_element() > 2.0
                    || (p.tile.max - tile.max).abs().max_element() > 2.0
            });
            let target = if changed {
                let blocked: Vec<_> = own.iter().chain(&names).copied().collect();
                select(size, tile, content, &blocked, &rays)
            } else {
                previous.expect("existing stable title").target
            };
            let point = previous.map_or(target, |p| {
                move_clear(p.point, target, size, tile, content, dt)
            });
            let signature = previous.filter(|_| !changed);
            self.state.titles.insert(
                index,
                Title {
                    point,
                    target,
                    size,
                    scale,
                    content: signature.map_or(content, |p| p.content),
                    tile: signature.map_or(tile, |p| p.tile),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn title_is_centred_below_clear_content_and_inside_its_tile() {
        let tile = Rect::from_corners(Vec2::splat(-200.0), Vec2::splat(200.0));
        let content =
            Rect::from_corners(Vec2::new(-80.0, -20.0), Vec2::new(80.0, 100.0));
        let size = Vec2::new(100.0, 24.0);
        let point = select(size, tile, content, &[content], &[]);
        let rect =
            Rect::from_corners(point - Vec2::Y * size.y, point + Vec2::X * size.x);
        assert!(rect.center().x.abs() < 0.001);
        assert!(rect.max.y < content.min.y);
        assert!(area(rect, content) <= 0.001);
        assert!(rect.min.cmpge(tile.min).all() && rect.max.cmple(tile.max).all());
    }
    #[test]
    fn title_motion_goes_around_content_instead_of_through_it() {
        let tile = Rect::from_corners(Vec2::splat(-200.0), Vec2::splat(200.0));
        let content =
            Rect::from_corners(Vec2::new(-80.0, -20.0), Vec2::new(80.0, 100.0));
        let size = Vec2::new(100.0, 24.0);
        let mut point = Vec2::new(-50.0, -100.0);
        let target = Vec2::new(-50.0, 160.0);
        for _ in 0..300 {
            let next = move_clear(point, target, size, tile, content, 1.0 / 60.0);
            assert!(next.distance(point) <= 8.001);
            let rect =
                Rect::from_corners(next - Vec2::Y * size.y, next + Vec2::X * size.x);
            assert!(area(rect, content) <= 0.001);
            point = next;
        }
        assert!(point.distance(target) < 1.0, "{point:?} -> {target:?}");
    }
}
