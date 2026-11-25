use super::*;
use crate::layout::packing::Tile;
use bevy::math::DVec2;

#[derive(Clone, Copy)]
struct Edge {
    ids: [usize; 2],
    from: [DVec2; 2],
    to: [DVec2; 2],
}

impl Edge {
    fn bounds(self) -> (DVec2, DVec2) {
        let [a, b] = self.from;
        let [c, d] = self.to;
        (a.min(b).min(c).min(d), a.max(b).max(c).max(d))
    }

    fn vector(self, origin: usize) -> [DVec2; 2] {
        let vector = |points: [DVec2; 2]| points[1 - origin] - points[origin];
        [vector(self.from), vector(self.to)]
    }

    fn toward(self, other: Self, point: usize) -> [DVec2; 2] {
        [
            other.from[point] - self.from[0],
            other.to[point] - self.to[0],
        ]
    }

    fn half(self, first: bool) -> Self {
        let middle = std::array::from_fn(|i| self.from[i].midpoint(self.to[i]));
        if first {
            Self { to: middle, ..self }
        } else {
            Self {
                from: middle,
                ..self
            }
        }
    }

    fn separated(self, other: Self, depth: u8) -> bool {
        let (low, high) = self.bounds();
        let (other_low, other_high) = other.bounds();
        if high.cmplt(other_low).any() || other_high.cmplt(low).any() {
            return true;
        }

        let shared = self.ids.iter().enumerate().find_map(|(index, id)| {
            other
                .ids
                .iter()
                .position(|other_id| id == other_id)
                .map(|other_index| (index, other_index))
        });
        let certified = if let Some((index, other_index)) = shared {
            let first = self.vector(index);
            let second = other.vector(other_index);
            let cross = quadratic(first, second, DVec2::perp_dot);
            let dot = quadratic(first, second, DVec2::dot);
            positive(&cross) || negative(&cross) || negative(&dot)
        } else {
            self.one_side(other) || other.one_side(self)
        };
        certified
            || (depth > 0
                && self.half(true).separated(other.half(true), depth - 1)
                && self.half(false).separated(other.half(false), depth - 1))
    }

    fn one_side(self, other: Self) -> bool {
        let axis = self.vector(0);
        let first = quadratic(axis, self.toward(other, 0), DVec2::perp_dot);
        let second = quadratic(axis, self.toward(other, 1), DVec2::perp_dot);
        (positive(&first) && positive(&second))
            || (negative(&first) && negative(&second))
    }

    fn nondegenerate(self) -> bool {
        let [start, end] = self.vector(0);
        start.length_squared() > 0.0
            && end.length_squared() > 0.0
            && (start.perp_dot(end) != 0.0 || start.dot(end) >= 0.0)
    }
}

fn quadratic(
    [a, b]: [DVec2; 2],
    [c, d]: [DVec2; 2],
    product: impl Fn(DVec2, DVec2) -> f64,
) -> [f64; 3] {
    // Bernstein coefficients bound the whole quadratic on the closed interval.
    [
        product(a, c),
        product(a, d).midpoint(product(b, c)),
        product(b, d),
    ]
}

fn positive(coefficients: &[f64; 3]) -> bool {
    coefficients.iter().all(|&value| value > 0.0)
}

fn negative(coefficients: &[f64; 3]) -> bool {
    coefficients.iter().all(|&value| value < 0.0)
}

#[derive(Clone, Copy)]
struct DirectoryFrame {
    position: DVec2,
    local: Vec2,
    scale: f32,
    visible_edge: bool,
}

#[derive(Default)]
pub(super) struct DirectoryAudit {
    previous: Vec<DirectoryFrame>,
    stretches: Vec<Vec2>,
}

impl DirectoryAudit {
    pub(super) fn observe(
        &mut self,
        sim: &Simulation,
        previous_tiles: &[Option<Tile>],
    ) -> Result<(usize, usize), proptest::test_runner::TestCaseError> {
        let layout = sim.layout();
        self.audit_stretch(sim, previous_tiles)?;
        let current: Vec<_> = (0..layout.dir_count())
            .map(|id| {
                let dir = layout.dir(id);
                DirectoryFrame {
                    position: dir.position.as_dvec2(),
                    local: dir.local_position(),
                    scale: dir.scale,
                    visible_edge: dir.live
                        && dir.has_landed_file
                        && dir.scale > 0.0
                        && sim.repository_effect(dir.repository).1 > 0.0,
                }
            })
            .collect();
        audit_footprints(sim)?;
        let mut edges = Vec::new();
        let mut retained = 0;
        let mut resizing = 0;
        for (child, after) in current.iter().enumerate() {
            let dir = layout.dir(child);
            let Some(parent) = dir.parent else { continue };
            let before = self.previous.get(child);
            let swept = before.filter(|frame| frame.scale > 0.0 && after.scale > 0.0);
            if !after.visible_edge && !before.is_some_and(|frame| frame.visible_edge) {
                continue;
            }
            if let Some(before) =
                swept.filter(|frame| frame.visible_edge && after.visible_edge)
            {
                retained += 1;
                let repo = dir.repository;
                let new_tile = layout.repositories[repo].tile;
                let old_tile = previous_tiles.get(repo).copied().flatten();
                let local_edge = after.local - current[parent].local;
                let old_edge = before.local - self.previous[parent].local;
                if local_edge.distance(old_edge) > 1e-5
                    && let Some((old, new)) = old_tile.zip(new_tile)
                    && old.high - old.low != new.high - new.low
                {
                    resizing += 1;
                }
            }
            let ids = [parent, child];
            let mut to = ids.map(|id| current[id].position);
            let mut from =
                before.map_or(to, |_| ids.map(|id| self.previous[id].position));
            // A collapsed fit makes every edge a homothetic copy of the visible endpoint.
            if before.is_some_and(|frame| frame.scale <= 0.0) {
                proptest::prop_assert_eq!(from[0], from[1]);
                from = to;
            } else if after.scale <= 0.0 {
                proptest::prop_assert_eq!(to[0], to[1]);
                to = from;
            }
            let edge = Edge { ids, from, to };
            proptest::prop_assert!(
                edge.nondegenerate(),
                "tick {}: directory edge {ids:?} has zero length",
                sim.state.tick
            );
            edges.push((dir.repository, edge, edge.bounds()));
        }

        edges.sort_unstable_by(|(repo, _, (a, _)), (other, _, (b, _))| {
            repo.cmp(other).then_with(|| a.x.total_cmp(&b.x))
        });
        for (index, &(repository, first, (_, high))) in edges.iter().enumerate() {
            for &(other_repository, second, (low, _)) in &edges[index + 1..] {
                if repository != other_repository || low.x > high.x {
                    break;
                }
                proptest::prop_assert!(
                    first.separated(second, 12),
                    "tick {}: repo {repository}, directory edges {:?} and {:?} are not separated over the interval ending here",
                    sim.state.tick,
                    first.ids,
                    second.ids,
                );
            }
        }
        self.previous = current;
        Ok((retained, resizing))
    }
    fn audit_stretch(
        &mut self,
        sim: &Simulation,
        previous_tiles: &[Option<Tile>],
    ) -> Result<(), proptest::test_runner::TestCaseError> {
        let layout = sim.layout();
        for (repo, region) in layout.repositories.iter().enumerate() {
            if let Some((old, new)) =
                previous_tiles.get(repo).copied().flatten().zip(region.tile)
                && ((old.low - new.low).abs().max_element() > 1e-5
                    || (old.high - new.high).abs().max_element() > 1e-5)
            {
                proptest::prop_assert_eq!(
                    self.stretches[repo],
                    region.stretch,
                    "tick {}: repo {} reshaped while its tile was moving",
                    sim.state.tick,
                    repo
                );
            }
        }
        self.stretches = layout
            .repositories
            .iter()
            .map(|region| region.stretch)
            .collect();
        Ok(())
    }
}
fn audit_footprints(
    sim: &Simulation,
) -> Result<(), proptest::test_runner::TestCaseError> {
    let layout = sim.layout();
    for (_, dir) in layout.dirs().filter(|(_, dir)| dir.scale > 0.0) {
        let region = &layout.repositories[dir.repository];
        let Some(tile) = region.tile else {
            continue;
        };
        let usable = tile.usable();
        let pad = region.stretch * dir.radius();
        for corner in [dir.position - pad, dir.position + pad] {
            let point = framing::world_to_field(corner);
            proptest::prop_assert!(
                point.cmpge(usable.low - Vec2::splat(0.0001)).all()
                    && point.cmple(usable.high + Vec2::splat(0.0001)).all(),
                "tick {}: directory {} escaped its usable tile",
                sim.state.tick,
                dir.path
            );
        }
    }
    Ok(())
}

#[test]
fn an_edge_cannot_collapse_between_nonzero_samples() {
    let edge = Edge {
        ids: [0, 1],
        from: [DVec2::ZERO, DVec2::X],
        to: [DVec2::ZERO, DVec2::NEG_X * 48.0],
    };
    assert!(!edge.nondegenerate());
}

#[test]
fn edge_crossing_inside_an_interval_fails_with_legal_ends_and_midpoint() {
    let horizontal = [DVec2::NEG_X, DVec2::X];
    let first = Edge {
        ids: [0, 1],
        from: horizontal,
        to: horizontal,
    };
    let vertical = |x| [DVec2::new(x, -1.0), DVec2::new(x, 1.0)];
    let second = Edge {
        ids: [2, 3],
        from: vertical(-2.0),
        to: vertical(6.0),
    };
    for x in [-2.0, 2.0, 6.0] {
        let points = vertical(x);
        assert!(first.separated(
            Edge {
                from: points,
                to: points,
                ..second
            },
            0
        ));
    }
    assert!(!first.separated(second, 12));
    let shift = DVec2::Y * 2.0;
    let safe = Edge {
        from: second.from.map(|p| p + shift),
        to: second.to.map(|p| p + shift),
        ..second
    };
    assert!(first.separated(safe, 12));
}

#[test]
fn edges_may_meet_only_at_a_shared_endpoint() {
    let edge = |ids, endpoints| Edge {
        ids,
        from: endpoints,
        to: endpoints,
    };
    let first = edge([0, 1], [DVec2::ZERO, DVec2::X]);
    assert!(first.separated(edge([0, 2], [DVec2::ZERO, DVec2::Y]), 0));
    assert!(first.separated(edge([0, 2], [DVec2::ZERO, DVec2::NEG_X]), 0));
    assert!(!first.separated(edge([0, 2], [DVec2::ZERO, DVec2::X * 2.0]), 0));
    assert!(!first.separated(edge([2, 3], [DVec2::X, DVec2::ONE]), 0));
}
