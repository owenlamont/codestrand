use bevy::math::Vec2;

use super::GAP;

pub(in super::super) struct Tree {
    levels: Vec<Vec<usize>>,
}

impl Tree {
    pub(super) fn new(children: &[Vec<usize>]) -> Self {
        let mut levels: Vec<Vec<usize>> = Vec::new();
        let mut stack = vec![(0, 0)];
        while let Some((node, depth)) = stack.pop() {
            if levels.len() == depth {
                levels.push(Vec::new());
            }
            levels[depth].push(node);
            stack.extend(children[node].iter().rev().map(|&child| (child, depth + 1)));
        }
        Self { levels }
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
            let radius = |id: usize| reserved[id][sample];
            let mut x = 0.0;
            let mut previous_radius = 0.0;
            for (depth, level) in self.levels.iter().enumerate() {
                let widest = level.iter().copied().map(radius).fold(0.0, f32::max);
                if depth != 0 {
                    x += previous_radius + widest + GAP;
                }
                previous_radius = widest;
                let spacing = |id| diameter_with_gap(radius(id));
                let extent: f32 = level.iter().copied().map(spacing).sum();
                let mut y = -extent * 0.5;
                for &id in level {
                    let span = spacing(id);
                    positions[id][sample] = Vec2::new(x, y + span * 0.5);
                    y += span;
                }
            }
        }
        positions
    }
}

fn diameter_with_gap(radius: f32) -> f32 {
    if radius > 0.0 {
        2.0 * radius + GAP
    } else {
        0.0
    }
}
