use bevy::math::Vec2;

pub(super) struct Clearance {
    pairs: Vec<(usize, usize, f32)>,
    inverse_edge_clearance: Vec<f32>,
}

impl Clearance {
    pub(super) fn is_finite(&self) -> bool {
        self.inverse_edge_clearance
            .iter()
            .all(|value| value.is_finite())
    }

    pub(super) fn new(points: &[Vec2], children: &[Vec<usize>]) -> Self {
        let mut pairs = Vec::new();
        let edges = children.iter().enumerate().flat_map(|(parent, branches)| {
            branches.iter().map(move |&child| [parent, child])
        });
        for (a, point) in points.iter().enumerate() {
            for (b, other) in points.iter().enumerate().skip(a + 1) {
                pairs.push((a, b, point.distance(*other).recip()));
            }
        }
        let inverse_edge_clearance = (0..points.len())
            .map(|node| {
                edges
                    .clone()
                    .filter(|edge| !edge.contains(&node))
                    .map(|edge| {
                        edge_distance_squared(points, edge, node).sqrt().recip()
                    })
                    .fold(0.0_f32, f32::max)
            })
            .collect();
        Self {
            pairs,
            inverse_edge_clearance,
        }
    }

    pub(super) fn scale(&self, reserved: &[Vec<f32>], sample: usize) -> f32 {
        let radius = |id: usize| reserved[id][sample] + super::GAP * 0.125;
        let mut scale = 0.0_f32;
        for &(a, b, inverse_distance) in &self.pairs {
            scale = scale.max((radius(a) + radius(b)) * inverse_distance);
        }
        scale.max(self.edge_scale(reserved, sample))
    }

    pub(super) fn edge_scale(&self, reserved: &[Vec<f32>], sample: usize) -> f32 {
        self.inverse_edge_clearance
            .iter()
            .enumerate()
            .map(|(node, &inverse_distance)| {
                (reserved[node][sample] + super::GAP * 0.125) * inverse_distance
            })
            .fold(0.0_f32, f32::max)
    }
}

pub(super) fn compact_branches(
    points: &mut Vec<Vec2>,
    radii: &[f32],
    children: &[Vec<usize>],
) {
    let mut descendants = vec![Vec::new(); points.len()];
    let mut edges = Vec::new();
    for (parent, branches) in children.iter().enumerate().rev() {
        let mut nodes = vec![parent];
        for &child in branches {
            nodes.extend_from_slice(&descendants[child]);
            edges.push([parent, child]);
        }
        descendants[parent] = nodes;
    }
    let mut score = cost(points, radii);
    let mut candidate = points.clone();
    for _ in 0..6 {
        for &[parent, node] in edges.iter().rev() {
            if descendants[node].len() < 2 {
                continue;
            }
            let parent_position = points[parent];
            let direction = points[node] - parent_position;
            for (angle, inset) in [
                (0.55, 0.0),
                (-0.55, 0.0),
                (0.24, 0.0),
                (-0.24, 0.0),
                (0.09, 0.0),
                (-0.09, 0.0),
                (0.0, 0.24),
                (0.0, 0.10),
                (0.0, 0.04),
            ] {
                let rotation = Vec2::from_angle(angle);
                candidate.clone_from(points);
                for &child in &descendants[node] {
                    candidate[child] = parent_position
                        + rotation.rotate(points[child] - parent_position)
                        - direction * inset;
                }
                let trial = cost(&candidate, radii);
                if trial < score && valid(&candidate, radii, &edges) {
                    std::mem::swap(points, &mut candidate);
                    score = trial;
                }
            }
        }
    }
}

pub(super) fn validate_template(
    points: &[Vec2],
    radii: &[f32],
    children: &[Vec<usize>],
) -> bool {
    let edges: Vec<_> = children
        .iter()
        .enumerate()
        .flat_map(|(parent, branches)| {
            branches.iter().map(move |&child| [parent, child])
        })
        .collect();
    valid(points, radii, &edges)
}

fn valid(points: &[Vec2], radii: &[f32], edges: &[[usize; 2]]) -> bool {
    if points.iter().any(|point| !point.is_finite()) {
        return false;
    }
    for (index, (&point, &radius)) in points.iter().zip(radii).enumerate() {
        if points[index + 1..]
            .iter()
            .zip(&radii[index + 1..])
            .any(|(&other, &size)| {
                point.distance_squared(other) < (radius + size).powi(2)
            })
        {
            return false;
        }
    }
    for (index, &edge) in edges.iter().enumerate() {
        if (0..points.len()).any(|node| hits_cluster(points, radii, edge, node))
            || edges[index + 1..]
                .iter()
                .any(|&other| intersects(points, edge, other))
        {
            return false;
        }
    }
    true
}

pub(super) fn large_template(
    children: &[Vec<usize>],
    radii: &[f32],
) -> Option<Vec<Vec2>> {
    [0.80, 1.05, 1.25]
        .into_iter()
        .map(|spread| place_sectors(children, radii, spread, 0.0))
        .filter(|points| validate_template(points, radii, children))
        .min_by(|left, right| cost(left, radii).total_cmp(&cost(right, radii)))
}

pub(super) fn place_sectors(
    children: &[Vec<usize>],
    radii: &[f32],
    spread: f32,
    phase: f32,
) -> Vec<Vec2> {
    let mut reserve = radii.to_vec();
    let mut transforms = vec![(Vec2::ZERO, 0.0); radii.len()];
    for (node, branches) in children.iter().enumerate().rev() {
        let root = node == 0;
        let total = branches.iter().map(|&child| reserve[child]).sum::<f32>();
        let fan = if root { std::f32::consts::PI } else { spread };
        let mut begin = if root { phase - fan } else { -fan };
        for &child in branches {
            let radius = reserve[child];
            let sector = fan * radius / total;
            let angle = begin + sector;
            begin += 2.0 * sector;
            let direction = Vec2::from_angle(angle);
            let gap = 0.02;
            let mut distance =
                (radius / sector.min(1.50).sin() + gap).max(radii[node] + radius + gap);
            if !root {
                distance = distance.max((radius + radii[node] + gap) / direction.x);
            }
            let centre = direction * distance;
            let root_distance = distance - radius + radii[child];
            transforms[child] = (direction * root_distance, angle);
            if !root {
                let offset = centre + Vec2::X * radii[node];
                let enclosing = (offset.length_squared() - radius * radius)
                    / (2.0 * (offset.x - radius));
                reserve[node] = reserve[node].max(enclosing + gap);
            }
        }
    }
    let mut points = vec![Vec2::ZERO; radii.len()];
    let mut directions = vec![0.0_f32; radii.len()];
    for (node, branches) in children.iter().enumerate() {
        let axis = Vec2::from_angle(directions[node]);
        for &child in branches {
            let (offset, angle) = transforms[child];
            points[child] = points[node] + axis.rotate(offset);
            directions[child] = directions[node] + angle;
        }
    }
    points
}

pub(super) fn place_subtrees(
    children: &[Vec<usize>],
    radii: &[f32],
    horizontal: f32,
    ordering: usize,
) -> Vec<Vec2> {
    let mut depth = vec![0; children.len()];
    let mut level_radius = vec![radii[0]];
    for (node, branches) in children.iter().enumerate() {
        let next = depth[node] + 1;
        for &child in branches {
            depth[child] = next;
            if level_radius.len() <= next {
                level_radius.push(0.0);
            }
            level_radius[next] = level_radius[next].max(radii[child]);
        }
    }
    let mut level_x = vec![0.0; level_radius.len()];
    for (level, pair) in level_radius.windows(2).enumerate() {
        level_x[level + 1] = level_x[level] + horizontal * (pair[0] + pair[1]);
    }
    let mut span = vec![0.0; children.len()];
    for (node, branches) in children.iter().enumerate().rev() {
        let descendant_span = branches.iter().map(|&child| span[child]).sum::<f32>();
        span[node] = (2.02 * radii[node]).max(descendant_span);
    }
    let mut points = vec![Vec2::ZERO; children.len()];
    let mut pending = vec![(0, -span[0] * 0.5)];
    while let Some((node, bottom)) = pending.pop() {
        points[node] = Vec2::new(level_x[depth[node]], bottom + span[node] * 0.5);
        let mut branches = children[node].clone();
        if ordering != 0 {
            branches.sort_by(|&left, &right| span[right].total_cmp(&span[left]));
            if ordering == 2 {
                let mut alternate = Vec::with_capacity(branches.len());
                alternate.extend(branches.iter().step_by(2).rev().copied());
                alternate.extend(branches.iter().skip(1).step_by(2).copied());
                branches = alternate;
            }
        }
        let total = branches.iter().map(|&child| span[child]).sum::<f32>();
        let mut next = bottom + (span[node] - total) * 0.5;
        for child in branches {
            pending.push((child, next));
            next += span[child];
        }
    }
    points
}

pub(super) fn place_clear(
    children: &[Vec<usize>],
    radii: &[f32],
    trial: usize,
) -> Option<Vec<Vec2>> {
    let mut queue = vec![0];
    let mut points = vec![Vec2::ZERO; children.len()];
    let mut placed = vec![0];
    let mut edges = Vec::new();
    let mut low = Vec2::splat(-radii[0]);
    let mut high = -low;
    let mut cursor = 0;
    let mut order = Vec::new();
    while let Some(&parent) = queue.get(cursor) {
        cursor += 1;
        let mut branches = children[parent].clone();
        branches.sort_by(|&a, &b| {
            (radii[b] + super::count(children[b].len()))
                .total_cmp(&(radii[a] + super::count(children[a].len())))
        });
        if trial.is_multiple_of(3) {
            queue.extend(branches.iter().copied());
        } else {
            queue.splice(cursor..cursor, branches.iter().copied());
        }
        order.extend(branches.into_iter().map(|node| (parent, node)));
    }
    if trial % 3 == 2 {
        let mut stack = vec![0];
        let by_parent = order;
        order = Vec::new();
        let mut parents = vec![0; children.len()];
        for &(parent, node) in &by_parent {
            parents[node] = parent;
        }
        while let Some(node) = stack.pop() {
            if node != 0 {
                order.push((parents[node], node));
            }
            stack.extend(
                by_parent
                    .iter()
                    .rev()
                    .filter_map(|&(parent, child)| (parent == node).then_some(child)),
            );
        }
    }
    for (parent, node) in order {
        let edge = [parent, node];
        let radius = radii[node];
        let spacing = radii[parent] + radius;
        let mut best = None;
        let mut best_score = f32::INFINITY;
        for ring in 0..32 {
            let distance = spacing * 1.01 + super::count(ring) * (radius + 0.3) * 0.8;
            for angle in 0..64 {
                let heading = (super::count(angle)
                    + super::count(node) * 0.618
                    + super::count(trial) * 0.383)
                    * (std::f32::consts::TAU / 64.0);
                let point = points[parent] + Vec2::from_angle(heading) * distance;
                let size = high.max(point + radius) - low.min(point - radius);
                let outward = if trial >= 9 && !children[node].is_empty() {
                    -0.3 * (low.midpoint(high) - point).length()
                } else {
                    0.0
                };
                let score = fit_span(size) + 0.05 * distance + outward;
                if score >= best_score {
                    continue;
                }
                points[node] = point;
                if placed.iter().any(|&other| {
                    point.distance(points[other]) < 1.001 * (radius + radii[other])
                        || hits_cluster(&points, radii, edge, other)
                }) || edges.iter().any(|&other| {
                    hits_cluster(&points, radii, other, node)
                        || intersects(&points, edge, other)
                }) {
                    continue;
                }
                best = Some(point);
                best_score = score;
            }
        }
        let point = best?;
        points[node] = point;
        low = low.min(point - radius);
        high = high.max(point + radius);
        placed.push(node);
        edges.push(edge);
    }
    Some(points)
}

pub(super) fn choose_template(
    children: &[Vec<usize>],
    radii: &[f32],
) -> Option<Vec<Vec2>> {
    let mut best = None;
    let mut best_score = f32::INFINITY;
    let mut consider = |points: Vec<Vec2>| {
        let score = cost(&points, radii);
        if score < best_score && validate_template(&points, radii, children) {
            best_score = score;
            best = Some(points);
        }
    };
    let mut improve = |mut points: Vec<Vec2>| {
        let expansion = clearance_scale(&points, radii, children);
        if expansion.is_finite() {
            for point in &mut points {
                *point *= expansion;
            }
            improve_with_clearance(&mut points, radii, children);
            consider(points);
        }
    };
    for spread in [0.55, 0.80, 1.05, 1.25] {
        for phase in [0.0, 0.4, 0.8] {
            improve(place_sectors(children, radii, spread, phase));
        }
    }
    for ordering in 0..3 {
        for horizontal in [1.0, 1.5, 2.0, 3.0] {
            improve(place_subtrees(children, radii, horizontal, ordering));
        }
    }
    for trial in 0..18 {
        if let Some(points) = place_clear(children, radii, trial) {
            consider(points);
        }
    }
    let mut best = best?;
    compact_branches(&mut best, radii, children);
    improve_with_clearance(&mut best, radii, children);
    validate_template(&best, radii, children).then_some(best)
}

fn improve_with_clearance(points: &mut [Vec2], radii: &[f32], children: &[Vec<usize>]) {
    let edges: Vec<_> = children
        .iter()
        .enumerate()
        .flat_map(|(parent, children)| {
            children.iter().map(move |&child| [parent, child])
        })
        .collect();
    let mut neighbours = vec![Vec::new(); points.len()];
    for &[a, b] in &edges {
        neighbours[a].push(b);
        neighbours[b].push(a);
    }
    let mut score = cost(points, radii);
    for _ in 0..12 {
        for node in 1..points.len() {
            let old = points[node];
            let centre = neighbours[node]
                .iter()
                .map(|&other| points[other])
                .sum::<Vec2>()
                / super::count(neighbours[node].len());
            let lateral = (centre - old).perp().normalize_or_zero() * radii[node];
            let targets = [
                centre,
                points[0],
                old + lateral,
                old - lateral,
                old + Vec2::Y * radii[node],
                old - Vec2::Y * radii[node],
            ];
            let candidates = targets.into_iter().flat_map(|target| {
                [0.5, 0.24, 0.10, 0.04].map(|damping| old.lerp(target, damping))
            });
            for candidate in candidates {
                if points.iter().zip(radii).enumerate().any(
                    |(other, (&point, &radius))| {
                        other != node
                            && point.distance(candidate) < radius + radii[node]
                    },
                ) {
                    continue;
                }
                points[node] = candidate;
                let trial = cost(points, radii);
                if trial < score
                    && affected_hits(points, radii, &edges, node) == 0
                    && remains_planar(points, &edges, node)
                {
                    score = trial;
                    break;
                }
                points[node] = old;
            }
        }
    }
}

fn affected_hits(
    points: &[Vec2],
    radii: &[f32],
    edges: &[[usize; 2]],
    node: usize,
) -> usize {
    edges
        .iter()
        .map(|edge| {
            if edge.contains(&node) {
                (0..points.len())
                    .filter(|other| hits_cluster(points, radii, *edge, *other))
                    .count()
            } else {
                usize::from(hits_cluster(points, radii, *edge, node))
            }
        })
        .sum()
}

fn hits_cluster(points: &[Vec2], radii: &[f32], edge: [usize; 2], node: usize) -> bool {
    if edge.contains(&node) {
        return false;
    }
    edge_distance_squared(points, edge, node) < radii[node] * radii[node]
}

pub(super) fn clearance_scale(
    points: &[Vec2],
    radii: &[f32],
    children: &[Vec<usize>],
) -> f32 {
    let mut scale = 1.0_f32;
    for (parent, children) in children.iter().enumerate() {
        for &child in children {
            for (node, &radius) in radii.iter().enumerate() {
                if node != parent && node != child {
                    let distance =
                        edge_distance_squared(points, [parent, child], node).sqrt();
                    scale = scale.max(1.001 * radius / distance);
                }
            }
        }
    }
    scale
}

fn edge_distance_squared(points: &[Vec2], edge: [usize; 2], node: usize) -> f32 {
    let [start, end] = edge.map(|id| points[id]);
    let offset = points[node] - start;
    let segment = end - start;
    let t = (offset.dot(segment) / segment.length_squared()).clamp(0.0, 1.0);
    (offset - t * segment).length_squared()
}

pub(super) fn cost(points: &[Vec2], radii: &[f32]) -> f32 {
    let (low, high) = points.iter().zip(radii).fold(
        (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)),
        |(low, high), (&point, &radius)| {
            (low.min(point - radius), high.max(point + radius))
        },
    );
    let centre = low.midpoint(high);
    let mean_square_radius = points
        .iter()
        .map(|point| point.distance_squared(centre))
        .sum::<f32>()
        / super::count(points.len());
    fit_span(high - low) + 0.15 * mean_square_radius.sqrt()
}

pub(super) fn fit_span(size: Vec2) -> f32 {
    size.min_element().max(size.max_element() / 1.45)
}

fn remains_planar(points: &[Vec2], edges: &[[usize; 2]], node: usize) -> bool {
    for &incident in edges.iter().filter(|edge| edge.contains(&node)) {
        for &other in edges.iter().filter(|edge| **edge != incident) {
            if intersects(points, incident, other) {
                return false;
            }
        }
    }
    true
}

fn intersects(points: &[Vec2], a: [usize; 2], b: [usize; 2]) -> bool {
    let point = |id: usize| points[id].as_dvec2();
    if let Some(joint) = a.into_iter().find(|id| b.contains(id)) {
        let first = point(if joint == a[0] { a[1] } else { a[0] }) - point(joint);
        let second = point(if joint == b[0] { b[1] } else { b[0] }) - point(joint);
        return first.dot(second) >= 0.0 && first.perp_dot(second).abs() <= 1e-8;
    }
    let [start, end] = a.map(point);
    let [left, right] = b.map(point);
    if start.max(end).cmplt(left.min(right)).any()
        || left.max(right).cmplt(start.min(end)).any()
    {
        return false;
    }
    let side = |p, q, r| {
        let edge: bevy::math::DVec2 = q - p;
        edge.perp_dot(r - p)
    };
    side(start, end, left) * side(start, end, right) <= 0.0
        && side(left, right, start) * side(left, right, end) <= 0.0
}
