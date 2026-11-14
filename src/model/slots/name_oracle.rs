//! The contributor-name assignment as first shipped, kept as the reference that
//! every later shortcut in [`State::assign_names`] must match bit for bit.

use std::cell::{Cell, RefCell};

use bevy::math::{Rect, Vec2};

use super::labels::{self, Assignment, Candidate};
use super::{HALF, NameAssignment, State, count, disk, segment, tile_obstacles};
use crate::model::{caption, names, tick_seconds, travel::Plan};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Production,
    Checked,
    Replay,
}

/// What the reference met, so a test can prove its fixture reached each path.
#[derive(Debug, Default)]
struct Seen {
    ticks: u32,
    largest_component: usize,
    exhausted: u32,
    retries: u32,
    rays: u32,
    texts: u32,
    planned_forecasts: u32,
    fallback_forecasts: u32,
}

thread_local! {
    static MODE: Cell<Mode> = const { Cell::new(Mode::Production) };
    static EXPECTED: RefCell<Option<NameAssignment>> = const { RefCell::new(None) };
    static SEEN: RefCell<Seen> = RefCell::new(Seen::default());
}

fn seen(record: impl FnOnce(&mut Seen)) {
    SEEN.with_borrow_mut(record);
}

/// Places names twice from one state, once from the shipped assignment and once
/// from the reference one, and compares everything the tick wrote.
pub(in crate::model) fn place_checked(
    state: &mut State,
    dt: f32,
    travel: &Plan,
    texts: &[Rect],
) -> bool {
    if MODE.get() != Mode::Checked {
        return false;
    }
    let mut reference = state.clone();
    state.place_slot_names(dt, travel, texts);
    MODE.set(Mode::Replay);
    reference.place_slot_names(dt, travel, texts);
    MODE.set(Mode::Checked);
    assert_eq!(
        name_state(state),
        name_state(&reference),
        "names after tick {}",
        state.tick
    );
    seen(|seen| seen.ticks += 1);
    true
}

pub(super) fn assign(
    state: &State,
    dt: f32,
    travel: &Plan,
    texts: &[Rect],
) -> NameAssignment {
    match MODE.get() {
        Mode::Production => state.assign_names(dt, travel, texts),
        Mode::Replay => EXPECTED.take().expect("checked tick ran first"),
        Mode::Checked => {
            let actual = state.assign_names(dt, travel, texts);
            let expected = assign_names(state, dt, travel, texts);
            assert_same(&actual, &expected, state.tick);
            EXPECTED.set(Some(expected));
            actual
        }
    }
}

fn candidate_bits(candidate: &Candidate) -> ([u32; 7], usize) {
    (
        [
            candidate.angle,
            candidate.rect.min.x,
            candidate.rect.min.y,
            candidate.rect.max.x,
            candidate.rect.max.y,
            candidate.text_area,
            candidate.soft,
        ]
        .map(f32::to_bits),
        candidate.text_pairs,
    )
}

fn assert_same(actual: &NameAssignment, expected: &NameAssignment, tick: u64) {
    assert_eq!(actual.authors, expected.authors, "authors at tick {tick}");
    assert_eq!(actual.displayed, expected.displayed, "texts at tick {tick}");
    assert_eq!(
        actual.domains.len(),
        expected.domains.len(),
        "domains at tick {tick}"
    );
    for ((author, actual), expected) in actual
        .authors
        .iter()
        .zip(&actual.domains)
        .zip(&expected.domains)
    {
        assert_eq!(
            actual.iter().map(candidate_bits).collect::<Vec<_>>(),
            expected.iter().map(candidate_bits).collect::<Vec<_>>(),
            "candidates of avatar {author} at tick {tick}"
        );
    }
    assert_eq!(
        (
            &actual.assignment.choices,
            actual.assignment.exhausted,
            actual.assignment.clear
        ),
        (
            &expected.assignment.choices,
            expected.assignment.exhausted,
            expected.assignment.clear
        ),
        "assignment at tick {tick}"
    );
}

struct Space<'a> {
    bodies: &'a [Option<Vec2>],
    obstacles: &'a [(usize, Rect)],
    rays: &'a [(Vec2, Vec2)],
    avatars: &'a [crate::model::Avatar],
    radius: f32,
    margin: f32,
}

struct Turn<'a> {
    body: Vec2,
    size: Vec2,
    previous: Option<&'a names::Placement>,
    future: &'a [Vec2],
}

fn assign_names(
    state: &State,
    dt: f32,
    travel: &Plan,
    texts: &[Rect],
) -> NameAssignment {
    let bodies = state.name_bodies();
    let mut displayed: Vec<_> = state
        .avatars
        .iter()
        .map(|a| {
            a.name_visibility
                .placement
                .as_ref()
                .map_or_else(|| a.name.clone(), |p| p.text.clone())
        })
        .collect();
    let (mut authors, mut domains) =
        label_domains(state, &bodies, travel, dt, texts, &displayed);
    let mut assignment = solve(&domains);
    for limit in [24, 16, 10] {
        let fits = assignment.clear
            && domains
                .iter()
                .zip(&assignment.choices)
                .all(|(domain, &choice)| {
                    caption::outside(domain[choice].rect, 0.5) <= 0.001
                });
        if fits {
            break;
        }
        let rectangles: Vec<_> = domains
            .iter()
            .zip(&assignment.choices)
            .map(|(domain, &choice)| domain[choice].rect)
            .collect();
        let mut shortened = false;
        for (node, &index) in authors.iter().enumerate() {
            if displayed[index].chars().count() <= limit {
                continue;
            }
            let candidate = domains[node][assignment.choices[node]];
            if candidate.text_pairs > 0
                || caption::outside(candidate.rect, 0.5) > 0.001
                || rectangles.iter().enumerate().any(|(other, rect)| {
                    other != node && !candidate.rect.intersect(*rect).is_empty()
                })
            {
                displayed[index] = names::elide(&displayed[index], limit);
                shortened = true;
            }
        }
        if !shortened {
            continue;
        }
        seen(|seen| seen.retries += 1);
        (authors, domains) =
            label_domains(state, &bodies, travel, dt, texts, &displayed);
        assignment = solve(&domains);
    }
    NameAssignment {
        authors,
        domains,
        assignment,
        displayed,
    }
}

fn label_domains(
    state: &State,
    bodies: &[Option<Vec2>],
    travel: &Plan,
    dt: f32,
    texts: &[Rect],
    displayed: &[String],
) -> (Vec<usize>, Vec<Vec<Candidate>>) {
    let mut obstacles = tile_obstacles(state);
    obstacles.extend(texts.iter().map(|&rect| (usize::MAX, rect)));
    let rays = state.name_rays(bodies, travel);
    seen(|seen| {
        seen.rays += u32::from(!rays.is_empty());
        seen.texts += u32::from(!texts.is_empty());
    });
    let space = Space {
        bodies,
        obstacles: &obstacles,
        rays: &rays,
        avatars: &state.avatars,
        radius: state.avatar_radius(),
        margin: 4.0 / 3.0,
    };
    let mut authors = Vec::new();
    let mut domains = Vec::new();
    for (index, avatar) in state
        .avatars
        .iter()
        .enumerate()
        .filter(|(_, a)| a.presence > 0.0)
    {
        let Some(body) = bodies[index] else { continue };
        let size = names::rect(
            Vec2::ZERO,
            &displayed[index],
            state.name_scale(),
            state.geometry.label_height,
        )
        .size();
        let previous = avatar.name_visibility.placement.as_ref();
        let velocity = previous
            .map_or(Vec2::ZERO, |p| (body - p.body) / dt.max(1e-6))
            .clamp_length_max(480.0);
        let planned = travel.forecast(index, state.tick);
        seen(|seen| {
            seen.planned_forecasts += u32::from(planned.is_some());
            seen.fallback_forecasts += u32::from(planned.is_none());
        });
        let future = planned.unwrap_or_else(|| {
            (1..=30_u8)
                .map(|step| {
                    (body
                        + velocity * (f32::from(step) * 4.0 * tick_seconds()).min(1.0))
                    .clamp(
                        Vec2::new(
                            -HALF.x + space.radius,
                            caption::BOTTOM + space.radius,
                        ),
                        Vec2::new(HALF.x - space.radius, caption::TOP - space.radius),
                    )
                })
                .collect()
        });
        let turn = Turn {
            body,
            size,
            previous,
            future: &future,
        };
        let mut targets: Vec<_> = (0..32_u8)
            .map(|i| f32::from(i) * std::f32::consts::TAU / 32.0)
            .collect();
        let corner = size * 0.5
            + Vec2::splat(
                (space.radius + space.margin) * std::f32::consts::FRAC_1_SQRT_2,
            );
        targets.extend(
            [
                corner,
                Vec2::new(-corner.x, corner.y),
                -corner,
                Vec2::new(corner.x, -corner.y),
            ]
            .map(Vec2::to_angle),
        );
        targets.extend(previous.map(|p| p.motion.goal));
        let scored = targets
            .into_iter()
            .map(|target| space.candidate(index, target, &turn))
            .collect();
        authors.push(index);
        domains.push(super::eligible_angles(scored));
    }
    (authors, domains)
}

impl Space<'_> {
    fn candidate(
        &self,
        index: usize,
        target: f32,
        motion: &Turn<'_>,
    ) -> (Candidate, [f32; 4]) {
        let distance = self.radius + self.margin;
        let rect = labels::tangent_rect(motion.body, motion.size, distance, target);
        let cost = self.cost(index, rect);
        let clip = labels::forecast_clip(
            target,
            0.0,
            target,
            motion.size,
            distance,
            motion.future,
        );
        let text = self.text_cost(rect);
        let soft = cost[2] * 2.0
            + clip * 1e8
            + cost[0]
            + cost[1] * 1e6
            + motion.previous.map_or(0.0, |p| {
                let distance = (target - p.angle + std::f32::consts::PI)
                    .rem_euclid(std::f32::consts::TAU)
                    - std::f32::consts::PI;
                distance.abs() * 55_000.0
            });
        (
            Candidate {
                angle: target,
                rect,
                text_pairs: text.0,
                text_area: text.1,
                soft,
            },
            [cost[0], cost[1], clip, soft],
        )
    }

    fn text_cost(&self, rect: Rect) -> (usize, f32) {
        self.obstacles
            .iter()
            .filter(|(owner, _)| *owner == usize::MAX)
            .map(|(_, r)| rect.intersect(*r))
            .filter(|r| !r.is_empty())
            .fold((0, 0.0), |(pairs, area), r| {
                (pairs + 1, area + r.size().element_product())
            })
    }

    fn cost(&self, index: usize, rect: Rect) -> [f32; 3] {
        let foreign = self
            .bodies
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index && self.avatars[*i].presence > 0.0)
            .filter_map(|(_, p)| *p);
        let conflicts = count(
            foreign
                .clone()
                .filter(|p| {
                    p.distance(p.clamp(rect.min, rect.max))
                        <= self.radius + self.margin + 0.001
                })
                .count(),
        );
        let ownership = foreign
            .clone()
            .map(|p| {
                (self.radius + self.margin + 0.1
                    - p.distance(p.clamp(rect.min, rect.max)))
                .max(0.0)
            })
            .sum::<f32>();
        let area = |other: Rect| {
            rect.intersect(other)
                .size()
                .max(Vec2::ZERO)
                .element_product()
        };
        let outside = caption::outside(rect, 0.5);
        [
            outside * 1e8,
            conflicts,
            ownership * 1e6
                + foreign.map(|p| area(disk(p, self.radius))).sum::<f32>() * 100.0
                + self
                    .obstacles
                    .iter()
                    .filter(|(r, _)| {
                        *r != usize::MAX && *r != self.avatars[index].repository
                    })
                    .map(|(_, r)| area(*r))
                    .sum::<f32>()
                + count(
                    self.rays
                        .iter()
                        .filter(|(a, b)| segment(*a, *b, rect))
                        .count(),
                ) * 20_000.0,
        ]
    }
}

fn overlap(a: Rect, b: Rect) -> f32 {
    a.intersect(b).size().max(Vec2::ZERO).element_product()
}

fn solve(domains: &[Vec<Candidate>]) -> Assignment {
    let mut result = Assignment {
        choices: vec![0; domains.len()],
        exhausted: false,
        clear: true,
    };
    let mut visited = vec![false; domains.len()];
    for root in 0..domains.len() {
        if visited[root] {
            continue;
        }
        visited[root] = true;
        let mut component = vec![root];
        let mut cursor = 0;
        while cursor < component.len() {
            let node = component[cursor];
            for other in 0..domains.len() {
                if !visited[other]
                    && domains[node].iter().any(|a| {
                        domains[other].iter().any(|b| overlap(a.rect, b.rect) > 0.0)
                    })
                {
                    visited[other] = true;
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
        seen(|seen| {
            seen.largest_component = seen.largest_component.max(component.len());
            seen.exhausted += u32::from(search.exhausted && !clear);
        });
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
            let mut next = (
                score.0 + candidate.text_pairs,
                score.1 + candidate.text_area,
                score.2 + candidate.soft,
            );
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

/// Thirty resident and three roaming long-named contributors in three repositories.
fn crowd() -> crate::model::Simulation {
    let histories = (0..3)
        .map(|repository| {
            let mut history: Vec<_> = (0..20)
                .map(|commit| {
                    let author = if commit % 5 == 0 {
                        30 + (commit / 5 + repository) % 3
                    } else {
                        repository * 10 + commit * 7 % 10
                    };
                    let mut event = crate::timeline::synthetic().remove(0);
                    event.author.name =
                        format!("Contributor Number {author} Of The Crowd")
                            .into_bytes();
                    event.author.email =
                        format!("author{author}@example.org").into_bytes();
                    event.seconds = i64::try_from(commit * 3 + repository).unwrap();
                    event
                })
                .collect();
            crate::repositories::namespace(&mut history, repository);
            history
        })
        .collect();
    crate::model::Simulation::with_repositories(
        crate::repositories::merge(histories),
        &crate::model::Options {
            seconds_per_day: 8640.0,
            max_idle_seconds: 0.1,
            ..Default::default()
        },
        &crate::theme::Theme::neon(),
        &crate::contributors::Mappings::default(),
        &["first".to_owned(), "second".to_owned(), "third".to_owned()],
    )
}

fn name_state(state: &State) -> Vec<(Vec<u32>, String)> {
    state
        .avatars
        .iter()
        .map(|avatar| {
            let name = &avatar.name_visibility;
            let floats = name.placement.iter().flat_map(|p| {
                [
                    p.point.x,
                    p.point.y,
                    p.body.x,
                    p.body.y,
                    p.angle,
                    p.angular_velocity,
                    p.scale,
                    p.motion.goal,
                ]
            });
            let rest = name.placement.as_ref().map(|p| {
                (
                    &p.text,
                    p.commit,
                    p.motion.pending,
                    p.motion.direction,
                    p.motion.reversal,
                )
            });
            (
                floats.chain([name.opacity]).map(f32::to_bits).collect(),
                format!(
                    "{:?}",
                    (
                        name.joint_exhausted,
                        name.joint_clear,
                        name.visible,
                        name.full_clear_since,
                        rest
                    )
                ),
            )
        })
        .collect()
}

#[test]
fn name_assignment_matches_the_reference_on_every_tick() {
    let mut sim = crowd();
    MODE.set(Mode::Checked);
    for frame in 0..=420 {
        sim.advance_to(frame, 60);
    }
    let end = name_state(&sim.state);
    sim.advance_to(240, 60);
    for frame in 0..=168 {
        sim.advance_to(frame, 24);
    }
    MODE.set(Mode::Production);
    assert_eq!(name_state(&sim.state), end);

    let seen = SEEN.take();
    assert_eq!(seen.ticks, 420 + 240 + 420);
    assert!(seen.largest_component >= 3, "{seen:?}");
    assert!(seen.exhausted > 0, "{seen:?}");
    assert!(seen.retries > 0, "{seen:?}");
    assert!(seen.rays > 0 && seen.texts > 0, "{seen:?}");
    assert!(
        seen.planned_forecasts > 0 && seen.fallback_forecasts > 0,
        "{seen:?}"
    );
}

#[test]
fn an_overlap_that_is_not_a_number_still_reaches_the_score() {
    let tall = |x: f32| Rect::new(x, f32::MIN, x + 1.0, f32::MAX);
    let wide = |height: f32| Rect::new(0.0, 0.0, 3.0, height);
    let domains: Vec<Vec<_>> =
        [vec![tall(0.0)], vec![tall(2.0)], vec![wide(1.0), wide(0.5)]]
            .into_iter()
            .map(|rects| {
                rects
                    .into_iter()
                    .map(|rect| Candidate {
                        angle: 0.0,
                        rect,
                        text_pairs: 1,
                        text_area: 0.0,
                        soft: 0.0,
                    })
                    .collect()
            })
            .collect();
    assert_eq!(labels::solve(&domains).choices, solve(&domains).choices);
}
