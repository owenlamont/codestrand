use bevy::math::Rect;

#[derive(Clone, Copy)]
pub(super) struct Candidate {
    pub attachment: u8,
    pub rect: Rect,
    pub soft: f32,
}

pub(super) struct Assignment {
    pub choices: Vec<usize>,
    pub exhausted: bool,
    pub clear: bool,
}

fn overlap(a: Rect, b: Rect) -> f32 {
    a.intersect(b)
        .size()
        .max(bevy::math::Vec2::ZERO)
        .element_product()
}

pub(super) fn solve(domains: &[Vec<Candidate>]) -> Assignment {
    let mut result = Assignment {
        choices: vec![0; domains.len()],
        exhausted: false,
        clear: true,
    };
    let mut seen = vec![false; domains.len()];
    for root in 0..domains.len() {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        let mut component = vec![root];
        let mut cursor = 0;
        while cursor < component.len() {
            let node = component[cursor];
            for other in 0..domains.len() {
                if !seen[other]
                    && domains[node].iter().any(|a| {
                        domains[other].iter().any(|b| overlap(a.rect, b.rect) > 0.0)
                    })
                {
                    seen[other] = true;
                    component.push(other);
                }
            }
            cursor += 1;
        }
        component.sort_by_key(|&node| (domains[node].len(), node));
        let mut search = Search {
            domains,
            component: &component,
            selected: Vec::new(),
            best: vec![0; component.len()],
            score: (usize::MAX, f32::INFINITY, f32::INFINITY),
            remaining: 4096,
            exhausted: false,
        };
        let clear = search.visit((0, 0.0, 0.0));
        for (&node, &choice) in component.iter().zip(&search.best) {
            result.choices[node] = choice;
        }
        result.exhausted |= search.exhausted && !clear;
        result.clear &= clear;
    }
    result
}

struct Search<'a> {
    domains: &'a [Vec<Candidate>],
    component: &'a [usize],
    selected: Vec<usize>,
    best: Vec<usize>,
    score: (usize, f32, f32),
    remaining: usize,
    exhausted: bool,
}

impl Search<'_> {
    fn visit(&mut self, score: (usize, f32, f32)) -> bool {
        if score.0 > self.score.0 || (score.0 == self.score.0 && score.1 > self.score.1)
        {
            return false;
        }
        let depth = self.selected.len();
        if depth == self.component.len() {
            if score.0 < self.score.0
                || score
                    .1
                    .total_cmp(&self.score.1)
                    .then(score.2.total_cmp(&self.score.2))
                    .is_lt()
            {
                self.score = score;
                self.best.clone_from(&self.selected);
            }
            return score.0 == 0;
        }
        if self.remaining == 0 {
            self.exhausted = true;
            return false;
        }
        self.remaining -= 1;
        let node = self.component[depth];
        for choice in 0..self.domains[node].len() {
            let candidate = self.domains[node][choice];
            let mut next = (score.0, score.1, score.2 + candidate.soft);
            for (previous, &selected) in self.selected.iter().enumerate() {
                let area = overlap(
                    candidate.rect,
                    self.domains[self.component[previous]][selected].rect,
                );
                next.0 += usize::from(area > 0.0);
                next.1 += area;
            }
            self.selected.push(choice);
            let clear = self.visit(next);
            self.selected.pop();
            if clear {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec2;

    fn candidate(x: f32) -> Candidate {
        Candidate {
            attachment: 0,
            rect: Rect::from_center_size(Vec2::new(x, 0.0), Vec2::splat(2.0)),
            soft: 0.0,
        }
    }

    #[test]
    fn joint_assignment_escapes_a_greedy_dead_end() {
        let domains = vec![vec![candidate(0.0), candidate(4.0)], vec![candidate(0.0)]];
        let result = solve(&domains);
        assert_eq!(result.choices, [1, 0]);
        assert!(!result.exhausted);
    }

    #[test]
    fn three_interacting_labels_use_distinct_positions() {
        let options = vec![candidate(0.0), candidate(4.0), candidate(8.0)];
        let result = solve(&[options.clone(), options.clone(), options]);
        assert_eq!(result.choices, [0, 1, 2]);
        assert!(!result.exhausted);
    }

    #[test]
    fn impossible_assignment_reports_the_least_overlap() {
        let result = solve(&[
            vec![candidate(0.0)],
            vec![candidate(0.0)],
            vec![candidate(8.0)],
        ]);
        assert_eq!(result.choices, [0, 0, 0]);
        assert!(!result.exhausted);
    }
    #[test]
    fn dense_impossible_search_stops_at_its_budget() {
        let domains = vec![vec![candidate(0.0); 8]; 16];
        let result = solve(&domains);
        assert!(result.exhausted);
        assert_eq!(result.choices.len(), 16);
    }
}
