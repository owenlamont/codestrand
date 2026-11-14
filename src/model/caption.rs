use bevy::prelude::*;

use super::{Simulation, frames_for, schedule::Schedule};
use crate::timeline::Event;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum, serde::Serialize,
)]
pub(crate) enum Style {
    Chip,
    #[default]
    Bar,
}

pub(crate) const FONT_SIZE: f32 = 14.0;
pub(crate) const MULTI_TOP: f32 = 698.0;
pub(crate) const BAR_HEIGHT: f32 = 22.0;
pub(crate) const BUFFER: f32 = 4.0;
pub(crate) const BOTTOM: f32 = 360.0 - MULTI_TOP + BUFFER;
pub(crate) const TOP: f32 = 360.0 - BAR_HEIGHT - BUFFER;
pub(crate) const HEADING_LEFT: f32 = 24.0;
pub(crate) const HEADING_SIZE: f32 = 18.0;

pub(super) fn outside(rect: Rect, margin: f32) -> f32 {
    let low = Vec2::new(-640.0 + margin, BOTTOM + margin);
    let high = Vec2::new(640.0 - margin, TOP - margin);
    (rect.min - low).min(Vec2::ZERO).length_squared()
        + (rect.max - high).max(Vec2::ZERO).length_squared()
}

pub(super) fn frame_text_rects() -> [Rect; 2] {
    [
        Rect::from_corners(
            Vec2::new(HEADING_LEFT - 640.0, 360.0 - BAR_HEIGHT),
            Vec2::new(
                HEADING_LEFT - 640.0 + 10.0 * HEADING_SIZE * 0.6 + 4.0,
                360.0,
            ),
        ),
        Rect::from_corners(
            Vec2::new(-640.0, 360.0 - MULTI_TOP - BAR_HEIGHT),
            Vec2::new(640.0, BOTTOM),
        ),
    ]
}
// Embedded Fira Mono has 600-unit advances at 1,000 units per em.
pub(crate) const ADVANCE: f32 = FONT_SIZE * 0.6;
pub(crate) const SEPARATOR_WIDTH: f32 = ADVANCE * 3.0;

#[derive(Clone, Copy, Resource)]
pub(crate) struct Columns {
    pub widths: [usize; 6],
}

impl Columns {
    pub(crate) fn new(sim: &Simulation) -> Self {
        let longest = |field: usize| {
            sim.events
                .iter()
                .map(|event| match field {
                    0 => super::date(event.seconds).chars().count(),
                    2 => event
                        .contributor
                        .as_ref()
                        .expect("resolved contributor")
                        .name
                        .chars()
                        .count(),
                    _ => unreachable!(),
                })
                .max()
                .unwrap_or(0)
        };
        Self {
            widths: [
                longest(0),
                sim.layout()
                    .repositories
                    .iter()
                    .map(|r| r.label.chars().count())
                    .max()
                    .unwrap_or(0),
                longest(2),
                format!("commit {} / {}", sim.total, sim.total).len(),
                "0.125x".len(),
                crate::theme::ThemeName::ALL
                    .iter()
                    .map(|t| t.title().chars().count())
                    .max()
                    .expect("available themes"),
            ],
        }
    }

    pub(crate) fn layout(self) -> Vec<(usize, f32, f32)> {
        let mut left = 28.0;
        let mut columns = Vec::new();
        for (index, width) in self.widths.into_iter().enumerate() {
            let width = super::count(width) * ADVANCE + 8.0;
            columns.push((index, left, width));
            left += width + SEPARATOR_WIDTH;
        }
        columns
    }
}

const HOLD: u64 = 48;
const FADE: u64 = 18;

pub(super) struct Transition {
    tick: u64,
    from: usize,
    to: usize,
}

pub(super) fn plan(schedule: &Schedule, events: &[Event]) -> Vec<Transition> {
    let mut changes: Vec<Transition> = Vec::new();
    for tick in 0..=u64::from(
        frames_for(schedule.duration(), 60).expect("bounded caption timeline"),
    ) {
        let Some(event) = schedule
            .applied(tick)
            .checked_sub(1)
            .and_then(|i| events.get(i))
        else {
            continue;
        };
        let Some(last) = changes.last() else {
            changes.push(Transition {
                tick,
                from: event.repository,
                to: event.repository,
            });
            continue;
        };
        if event.repository != last.to && tick - last.tick >= HOLD {
            changes.push(Transition {
                tick,
                from: last.to,
                to: event.repository,
            });
        }
    }
    changes
}

pub(crate) fn background(colour: Color) -> LinearRgba {
    let c = colour.to_linear();
    LinearRgba::new(
        c.red.clamp(0.0, 1.0) * 0.14,
        c.green.clamp(0.0, 1.0) * 0.14,
        c.blue.clamp(0.0, 1.0) * 0.14,
        1.0,
    )
}

impl Simulation {
    fn applied_event(&self) -> Option<&Event> {
        self.schedule
            .applied(self.state.tick)
            .checked_sub(1)
            .and_then(|i| self.events.get(i))
    }

    pub(crate) fn caption_fields(&self) -> [String; 6] {
        let event = self.applied_event();
        [
            event.map_or_else(String::new, |e| super::date(e.seconds)),
            self.caption_repository()
                .map_or_else(String::new, |(_, r)| r.to_owned()),
            event.map_or_else(String::new, |e| {
                e.contributor
                    .as_ref()
                    .expect("resolved contributor")
                    .name
                    .clone()
            }),
            format!(
                "commit {:>digits$} / {}",
                self.commit(),
                self.total,
                digits = self.total.to_string().len()
            ),
            format!("{}x", self.playback_multiplier),
            self.theme.name.title().to_owned(),
        ]
    }
    pub(crate) fn caption_repository(&self) -> Option<(usize, &str)> {
        let event = self.applied_event()?;
        self.layout()
            .repositories
            .get(event.repository)
            .map(|r| (event.repository, r.label.as_str()))
    }

    pub(crate) fn caption_background(&self) -> Color {
        let colour = |index: usize| {
            background(self.theme.glass[index % self.theme.glass.len()].base_color)
        };
        let Some((repository, _)) = self.caption_repository() else {
            return Color::BLACK;
        };
        if self.caption_style == Style::Chip {
            return colour(repository).into();
        }
        let index = self
            .caption_plan
            .partition_point(|t| t.tick <= self.state.tick);
        let Some(change) = index.checked_sub(1).and_then(|i| self.caption_plan.get(i))
        else {
            return Color::BLACK;
        };
        let alpha = (super::tick_time(self.state.tick - change.tick)
            / super::tick_time(FADE))
        .min(1.0);
        colour(change.from).mix(&colour(change.to), alpha).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wordmark_strip_has_the_same_clearance_as_the_caption() {
        let wordmark = frame_text_rects()[0].inflate(-6.0);
        assert!(outside(wordmark, 0.5) > 0.0);
        let tile = crate::layout::packing::Tile::viewport();
        assert!(tile.high.y * 360.0 + super::super::framing::MARGIN <= 334.001);
    }

    #[test]
    #[ignore = "requires local repositories and caption diagnostic output path"]
    fn local_caption_audit() {
        let (mut sim, _) = super::super::nitpicks::local_simulation();
        let columns = Columns::new(&sim);
        sim.caption_style = Style::Bar;
        let mut commits = Vec::new();
        let mut previous = 0;
        for frame in 0..=frames_for(sim.duration(), 60).unwrap() {
            sim.advance_to(frame, 60);
            assert!(
                sim.caption_fields()
                    .iter()
                    .zip(columns.widths)
                    .all(|(text, width)| text.chars().count() <= width)
            );
            if sim.commit() != previous {
                let (repo, name) = sim.caption_repository().unwrap();
                commits.push(serde_json::json!({"frame":frame,"seconds":f64::from(frame)/60.0,"commit":sim.commit(),"repository":repo,"name":name,"caption":sim.caption()}));
                previous = sim.commit();
            }
        }
        let transitions: Vec<_> = sim
            .caption_plan
            .iter()
            .map(|t| serde_json::json!({"frame":t.tick,"from":t.from,"to":t.to}))
            .collect();
        let result = serde_json::json!({"commits":commits,"transitions":transitions,"column_characters":columns.widths,"layout":columns.layout()});
        std::fs::write(
            std::env::var("CODESTRAND_CAPTION_AUDIT").unwrap(),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn repository_name_tracks_the_applied_commit_and_colour_seeks_exactly() {
        let histories = (0..2)
            .map(|i| {
                let mut events = crate::timeline::synthetic();
                for e in &mut events {
                    e.seconds += i * 86_400;
                }
                crate::repositories::namespace(
                    &mut events,
                    usize::try_from(i).unwrap(),
                );
                events
            })
            .collect();
        let mut sim = Simulation::with_repositories(
            crate::repositories::merge(histories),
            &super::super::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &["first".into(), "second".into()],
        );
        sim.caption_style = Style::Bar;
        sim.advance_to(90, 30);
        let caption = sim.caption();
        let colour = sim.caption_background();
        let event = &sim.events[sim.schedule.applied(sim.state.tick) - 1];
        assert!(caption.contains(&format!(
            "   |   {}   |   ",
            sim.layout().repositories[event.repository].label
        )));
        sim.advance_to(240, 30);
        sim.advance_to(180, 60);
        assert_eq!(sim.caption(), caption);
        assert_eq!(sim.caption_background(), colour);
        let changes: Vec<_> = sim
            .caption_plan
            .iter()
            .skip(1)
            .map(|t| (t.tick, t.from, t.to))
            .collect();
        for (tick, from, to) in changes {
            for step in [0_u16, 9, 18] {
                sim.advance_to(u32::try_from(tick + u64::from(step)).unwrap(), 60);
                let expected = background(
                    sim.theme.glass[from % sim.theme.glass.len()].base_color,
                )
                .mix(
                    &background(sim.theme.glass[to % sim.theme.glass.len()].base_color),
                    f32::from(step) / 18.0,
                );
                assert_eq!(sim.caption_background(), Color::LinearRgba(expected));
            }
        }
    }

    #[test]
    fn internal_simulation_uses_the_shared_repository_caption() {
        let mut sim = Simulation::new(
            crate::timeline::synthetic(),
            &super::super::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        sim.advance_to(31, 60);
        let event = &sim.events[0];
        assert_eq!(
            sim.caption(),
            format!(
                "{}   |   Synthetic history   |   {}   |   commit 1 / {}   |   1x",
                super::super::date(event.seconds),
                event.contributor.as_ref().unwrap().name,
                sim.total
            )
        );
        assert_eq!(sim.caption_repository(), Some((0, "Synthetic history")));
    }

    #[test]
    fn alternating_commits_hold_colour_and_replay_at_explicit_ticks() {
        let events: Vec<_> = (0..100)
            .map(|i| {
                let mut e = crate::timeline::synthetic().remove(0);
                e.seconds = i;
                e.repository = usize::try_from(i % 2).unwrap();
                e
            })
            .collect();
        let schedule = Schedule::new(&events, 0.5, 0.05);
        let changes = plan(&schedule, &events);
        assert!(changes.len() > 2);
        assert!(
            changes
                .windows(2)
                .all(|pair| pair[1].tick - pair[0].tick >= HOLD)
        );
        assert!(changes.iter().all(|t| t.from < 2 && t.to < 2));
    }

    #[test]
    fn white_text_exceeds_four_point_five_contrast_for_every_palette_and_blend() {
        for name in crate::theme::ThemeName::ALL {
            let theme = name.build();
            for (a, b) in theme
                .glass
                .iter()
                .flat_map(|a| theme.glass.iter().map(move |b| (a, b)))
            {
                for step in 0..=18_u16 {
                    let c = background(a.base_color)
                        .mix(&background(b.base_color), f32::from(step) / 18.0);
                    let luminance = 0.2126 * c.red + 0.7152 * c.green + 0.0722 * c.blue;
                    assert!(1.05 / (luminance + 0.05) >= 4.5);
                }
            }
        }
    }
}
