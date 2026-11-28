mod active;
mod compact;

use bevy::math::{DVec2, Vec2};
use std::{
    collections::VecDeque,
    f64::consts::{PI, TAU},
};

use super::GAP;

const GRID_TRIALS: usize = 4;
const POINT_VISITS_PER_DIRECTORY: usize = 64;
const ANGULAR_MARGIN: f64 = 1e-8;
const MAX_COMPACT_DIRECTORIES: usize = 384;

#[derive(Clone, Copy)]
struct Site {
    point: Vec2,
    capacity: Vec2,
    column: usize,
    row: usize,
}

pub(super) enum Tree {
    Template(Template),
    Active(active::Tree),
}

pub(super) struct Template {
    sites: Vec<Site>,
    reservation: Reservation,
    has_late_directories: bool,
}

enum Reservation {
    Grid(Option<compact::Clearance>),
    Compact(compact::Clearance),
}

impl Tree {
    pub(super) fn new(children: &[Vec<usize>], required: &[Vec<f32>]) -> Self {
        let opening = required
            .iter()
            .skip(1)
            .filter_map(|radii| radii.iter().position(|&radius| radius > 0.0))
            .min()
            .unwrap_or(0);
        let end = (opening + 2).min(required[0].len() - 1);
        let mut radii: Vec<_> = required
            .iter()
            .map(|row| row[..=end].iter().copied().fold(0.0, f32::max))
            .collect();
        let has_late_directories = radii.contains(&0.0);
        if children.len() <= MAX_COMPACT_DIRECTORIES
            && (children.len() > 5 || !has_late_directories)
        {
            // Dormant directories use the opening size only to choose a planar embedding.
            let seed_radius = radii.iter().copied().fold(0.4_f32, f32::max);
            for radius in &mut radii {
                if *radius == 0.0 {
                    *radius = seed_radius;
                }
            }
            if let Some(template) = Template::new(children, radii, has_late_directories)
            {
                return Self::Template(template);
            }
        }
        Self::Active(active::Tree::new(children))
    }

    pub(super) fn retry_with_depth_bands(&mut self, children: &[Vec<usize>]) -> bool {
        if matches!(self, Self::Template(template) if template.has_late_directories) {
            *self = Self::Active(active::Tree::new(children));
            true
        } else {
            false
        }
    }

    pub(super) fn pack(
        &self,
        reserved: &[Vec<f32>],
        report: impl Fn(usize, usize),
    ) -> Vec<Vec<Vec2>> {
        match self {
            Self::Template(tree) => tree.pack(reserved, report),
            Self::Active(tree) => tree.pack(reserved, report),
        }
    }
}

impl Template {
    fn new(
        children: &[Vec<usize>],
        mut radii: Vec<f32>,
        has_late_directories: bool,
    ) -> Option<Self> {
        for radius in &mut radii {
            *radius += GAP;
        }
        if children.len() > 5 {
            if let Some(points) = compact::choose_template(children, &radii) {
                let reservation =
                    Reservation::Compact(compact::Clearance::new(&points, children));
                let mut sites = depth_order(children);
                for (site, point) in sites.iter_mut().zip(points) {
                    site.point = point;
                }
                return Some(Self {
                    sites,
                    reservation,
                    has_late_directories,
                });
            }
            return None;
        }
        let mut counts = vec![1; children.len()];
        for (parent, children) in children.iter().enumerate().rev() {
            counts[parent] +=
                children.iter().map(|&child| counts[child]).sum::<usize>();
        }
        let columns = (children.len().saturating_mul(7).div_ceil(5) - 1).isqrt() + 1;
        let rows = children.len().div_ceil(columns);
        let mut widths = vec![1.0_f32; columns];
        let mut heights = vec![1.0_f32; rows];
        let mut best = None;
        let mut best_score = f32::INFINITY;
        for trial in 0..GRID_TRIALS {
            let slots = grid(children.len(), &widths, &heights, trial);
            let heading = DVec2::from_angle(f64::from(count(trial)) * 0.43);
            let Some(sites) = embed(children, &counts, slots, heading) else {
                continue;
            };
            let stretch = scale(&sites, |index| radii[index]);
            let (low, high) = sites.iter().zip(&radii).fold(
                (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)),
                |(low, high), (site, &radius)| {
                    (
                        low.min(site.point * stretch - Vec2::splat(radius)),
                        high.max(site.point * stretch + Vec2::splat(radius)),
                    )
                },
            );
            let score = (high - low).element_product();
            widths.fill(GAP);
            heights.fill(GAP);
            for (site, &radius) in sites.iter().zip(&radii) {
                widths[site.column] = widths[site.column].max(radius);
                heights[site.row] = heights[site.row].max(radius);
            }
            if score < best_score {
                best_score = score;
                best = Some(sites);
            }
        }
        let sites = best.unwrap_or_else(|| depth_order(children));
        let clearance = (children.len() >= 3).then(|| {
            let points: Vec<_> = sites.iter().map(|site| site.point).collect();
            compact::Clearance::new(&points, children)
        });
        if clearance
            .as_ref()
            .is_some_and(|clearance| !clearance.is_finite())
        {
            return None;
        }
        Some(Self {
            sites,
            reservation: Reservation::Grid(clearance),
            has_late_directories,
        })
    }

    pub(super) fn pack(
        &self,
        reserved: &[Vec<f32>],
        report: impl Fn(usize, usize),
    ) -> Vec<Vec<Vec2>> {
        let count = reserved[0].len();
        let mut positions = vec![vec![Vec2::ZERO; count]; reserved.len()];
        for sample in 0..count {
            report(sample, count);
            let stretch = match &self.reservation {
                Reservation::Grid(clearance) => {
                    let stretch =
                        scale(&self.sites, |index| reserved[index][sample] + GAP);
                    clearance.as_ref().map_or(stretch, |clearance| {
                        stretch.max(Vec2::splat(clearance.edge_scale(reserved, sample)))
                    })
                }
                Reservation::Compact(clearance) => {
                    Vec2::splat(if self.has_late_directories {
                        clearance.occupied_scale(reserved, sample)
                    } else {
                        clearance.scale(reserved, sample)
                    })
                }
            };
            let root = self.sites[0].point;
            // A common positive stretch keeps every separation line valid through the spline.
            for (row, site) in positions.iter_mut().zip(&self.sites) {
                row[sample] = (site.point - root) * stretch;
            }
        }
        positions
    }
}

fn count(value: usize) -> f32 {
    crate::model::count_u32(value.try_into().expect("directory count fits u32"))
}

fn scale(points: &[Site], radius: impl Fn(usize) -> f32) -> Vec2 {
    points
        .iter()
        .enumerate()
        .fold(Vec2::ZERO, |scale, (index, site)| {
            scale.max(Vec2::splat(radius(index)) / site.capacity)
        })
}

fn centers(sizes: &[f32]) -> Vec<f32> {
    let mut end = 0.0;
    sizes
        .iter()
        .map(|&width| {
            let center = end + width;
            end += 2.0 * width;
            center
        })
        .collect()
}

fn grid(length: usize, widths: &[f32], heights: &[f32], trial: usize) -> Vec<Site> {
    let x = centers(widths);
    let y = centers(heights);
    (0..length)
        .map(|index| {
            let row = index / widths.len();
            let column = if row.is_multiple_of(2) {
                index % widths.len()
            } else {
                widths.len() - 1 - index % widths.len()
            };
            let half = Vec2::new(widths[column], heights[row]);
            let phase = count(index) + 1.37 * count(trial);
            let jitter = Vec2::new((phase * 57.13).sin(), (phase * 39.71).sin()) * 0.08;
            Site {
                point: Vec2::new(x[column], y[row]) + half * jitter,
                capacity: half * (Vec2::ONE - jitter.abs()),
                row,
                column,
            }
        })
        .collect()
}

struct Subtree {
    node: usize,
    sites: Vec<Site>,
    axis: DVec2,
}

fn embed(
    children: &[Vec<usize>],
    counts: &[usize],
    mut points: Vec<Site>,
    heading: DVec2,
) -> Option<Vec<Site>> {
    let mut layout = vec![points[0]; points.len()];
    let centre =
        points.iter().map(|site| site.point).sum::<Vec2>() / count(points.len());
    let central = points
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            a.point
                .distance_squared(centre)
                .total_cmp(&b.point.distance_squared(centre))
        })?
        .0;
    let root = points.swap_remove(central);
    let mut pending = if let Some(groups) =
        divide(&children[0], counts, &mut points, root, heading, true)
    {
        layout[0] = root;
        VecDeque::from(groups)
    } else {
        points.push(root);
        VecDeque::from([Subtree {
            node: 0,
            sites: points,
            axis: DVec2::X,
        }])
    };
    let mut work = children.len().saturating_mul(POINT_VISITS_PER_DIRECTORY);
    while let Some(Subtree {
        node,
        mut sites,
        axis,
    }) = pending.pop_front()
    {
        work = work.checked_sub(sites.len())?;
        let first = sites
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                a.point
                    .as_dvec2()
                    .dot(axis)
                    .total_cmp(&b.point.as_dvec2().dot(axis))
            })?
            .0;
        let origin = sites.swap_remove(first);
        layout[node] = origin;
        // The incoming edge stays outside the half-plane occupied by the descendants.
        if sites.iter().any(|site| {
            let delta = site.point.as_dvec2() - origin.point.as_dvec2();
            delta.dot(axis) <= delta.length() * ANGULAR_MARGIN
        }) {
            return None;
        }
        pending.extend(divide(
            &children[node],
            counts,
            &mut sites,
            origin,
            axis,
            false,
        )?);
    }
    Some(layout)
}

fn divide(
    children: &[usize],
    counts: &[usize],
    sites: &mut [Site],
    origin: Site,
    axis: DVec2,
    around_root: bool,
) -> Option<Vec<Subtree>> {
    let angle = |site: &Site| {
        let delta = site.point.as_dvec2() - origin.point.as_dvec2();
        axis.perp_dot(delta).atan2(axis.dot(delta))
    };
    if children.len() > 1 || around_root {
        sites.sort_unstable_by(|a, b| angle(a).total_cmp(&angle(b)));
    }
    let mut previous = None;
    let mut remaining: &[_] = sites;
    let mut groups = Vec::with_capacity(children.len());
    for &node in children {
        let (group, tail) = remaining.split_at(counts[node]);
        remaining = tail;
        let first = angle(&group[0]);
        let last = angle(group.last()?);
        if previous.is_some_and(|previous| first - previous <= ANGULAR_MARGIN)
            || (around_root && last - first >= PI - ANGULAR_MARGIN)
        {
            return None;
        }
        previous = Some(last);
        groups.push(Subtree {
            node,
            sites: group.to_vec(),
            axis: if around_root {
                DVec2::from_angle(axis.to_angle() + first.midpoint(last))
            } else {
                axis
            },
        });
    }
    if around_root && groups.len() > 1 {
        let first = angle(&groups[0].sites[0]);
        if previous? - first >= TAU - ANGULAR_MARGIN {
            return None;
        }
    }
    Some(groups)
}

fn depth_order(children: &[Vec<usize>]) -> Vec<Site> {
    let mut points = vec![
        Site {
            point: Vec2::ZERO,
            capacity: Vec2::splat(0.5),
            column: 0,
            row: 0,
        };
        children.len()
    ];
    let mut stack = vec![(0, 0)];
    for order in 0..children.len() {
        let (node, depth) = stack.pop().expect("preorder visits every directory");
        points[node].point = Vec2::new(count(depth), count(order));
        stack.extend(children[node].iter().rev().map(|&child| (child, depth + 1)));
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distant_directories_do_not_reserve_opening_space() {
        let count = MAX_COMPACT_DIRECTORIES + 1;
        let mut children = vec![Vec::new(); count];
        children[0] = (1..count).collect();
        let mut required = vec![vec![0.0, 0.0, 0.0, 0.0, 2.0]; count];
        required[..3].fill(vec![0.5; 5]);
        let tree = Tree::new(&children, &required);
        let positions = tree.pack(&required, |_, _| {});
        assert!(positions[1][0].distance(positions[2][0]) < 2.0);
        assert!(positions[1][4].distance(positions[count - 1][4]) > 100.0);
    }
}
