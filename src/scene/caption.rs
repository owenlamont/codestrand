use bevy::prelude::*;

use super::{Caption, Playback};
use crate::{
    camera::PreviewCamera,
    model::caption::{Columns, FONT_SIZE, SEPARATOR_WIDTH, Style},
};

#[derive(Component)]
struct Bar;

#[derive(Component)]
pub(super) struct CaptionText;

#[derive(Component)]
enum Part {
    Field(usize),
    Separator,
    Status,
}

fn text(world: &mut World, part: Part, value: &str, left: f32, width: Val) -> Entity {
    world
        .spawn((
            part,
            CaptionText,
            Text::new(value),
            TextFont::from_font_size(FONT_SIZE),
            TextColor(Color::WHITE),
            TextLayout::new(Justify::Left, bevy::text::LineBreak::NoWrap),
            Node {
                position_type: PositionType::Absolute,
                left: px(left),
                top: px(0.0),
                width,
                height: px(22.0),
                padding: UiRect::axes(px(4.0), px(1.0)),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(Color::NONE),
        ))
        .id()
}

pub(super) fn setup(world: &mut World, camera: Entity) {
    let columns = Columns::new(&world.resource::<Playback>().0);
    let root = world
        .spawn((
            Bar,
            Node {
                position_type: PositionType::Absolute,
                left: px(0.0),
                top: px(if columns.multi { 698.0 } else { 668.0 }),
                width: percent(100.0),
                height: px(22.0),
                ..default()
            },
            BackgroundColor(Color::NONE),
            UiTargetCamera(camera),
        ))
        .id();
    let layout = columns.layout();
    for &(index, left, width) in &layout {
        let child = text(world, Part::Field(index), "", left, px(width));
        if index == 5 {
            world.entity_mut(child).insert(Caption);
        }
        world.entity_mut(root).add_child(child);
        if index != 5 {
            let separator = text(
                world,
                Part::Separator,
                " | ",
                left + width,
                px(SEPARATOR_WIDTH),
            );
            world
                .entity_mut(separator)
                .get_mut::<Node>()
                .unwrap()
                .padding = UiRect::vertical(px(1.0));
            world.entity_mut(root).add_child(separator);
        }
    }
    let (_, left, width) = *layout.last().expect("caption fields");
    let status = text(world, Part::Status, "", left + width, Val::Auto);
    world.entity_mut(root).add_child(status);
    world.insert_resource(columns);
}

pub(super) fn update(world: &mut World) {
    let simulation = &world.resource::<Playback>().0;
    let fields = simulation.caption_fields();
    let multi = !simulation.layout().repositories.is_empty();
    let style = simulation.caption_style;
    let tint = simulation.caption_background();
    let ink = if multi {
        Color::WHITE
    } else {
        simulation.theme().heading
    };
    let status = world
        .get_resource::<PreviewCamera>()
        .map_or_else(String::new, |c| format!(" | {}", c.caption()));
    for mut background in world
        .query_filtered::<&mut BackgroundColor, With<Bar>>()
        .iter_mut(world)
    {
        background.set_if_neq(BackgroundColor(if multi {
            if style == Style::Bar {
                tint
            } else {
                Color::BLACK
            }
        } else {
            Color::NONE
        }));
    }
    for (part, mut label, mut background, mut colour) in world
        .query::<(&Part, &mut Text, &mut BackgroundColor, &mut TextColor)>()
        .iter_mut(world)
    {
        let value = match part {
            Part::Field(index) => &fields[*index],
            Part::Separator => " | ",
            Part::Status => &status,
        };
        label.set_if_neq(Text::new(value));
        colour.set_if_neq(TextColor(ink));
        background.set_if_neq(BackgroundColor(
            if multi
                && matches!(part, Part::Field(1))
                && style == Style::Chip
                && !fields[1].is_empty()
            {
                tint
            } else {
                Color::NONE
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separator_positions_stay_fixed_across_every_caption_in_realistic_history() {
        let histories = (0..5)
            .map(|i| {
                let mut events = crate::timeline::synthetic();
                for (j, event) in events.iter_mut().enumerate() {
                    event.seconds += i * 3 * 86_400;
                    event.author.name = format!(
                        "{} {j}",
                        "Contributor".repeat(usize::try_from(i % 3 + 1).unwrap())
                    )
                    .into_bytes();
                }
                crate::repositories::namespace(
                    &mut events,
                    usize::try_from(i).unwrap(),
                );
                events
            })
            .collect();
        let simulation = crate::model::Simulation::with_repositories(
            crate::repositories::merge(histories),
            &crate::model::Options {
                seconds_per_day: 0.08,
                max_idle_seconds: 0.1,
                ..default()
            },
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &[
                "r".into(),
                "repository-two".into(),
                "three".into(),
                "four".into(),
                "five".into(),
            ],
        );
        let frames = crate::model::frames_for(simulation.duration(), 60).unwrap();
        let mut world = World::new();
        world.insert_resource(Playback(simulation));
        let camera = world.spawn_empty().id();
        setup(&mut world, camera);
        let separators = |world: &mut World| {
            world
                .query::<(&Part, &Node)>()
                .iter(world)
                .filter_map(|(part, node)| {
                    if !matches!(part, Part::Separator) {
                        return None;
                    }
                    assert_eq!(node.position_type, PositionType::Absolute);
                    let Val::Px(left) = node.left else {
                        panic!("separator must have a fixed pixel position")
                    };
                    Some(left)
                })
                .collect::<Vec<_>>()
        };
        let expected = separators(&mut world);
        assert_eq!(expected.len(), 5);
        assert!(expected.windows(2).all(|pair| pair[0] < pair[1]));
        for frame in 0..=frames {
            world.resource_mut::<Playback>().0.advance_to(frame, 60);
            update(&mut world);
            assert_eq!(
                separators(&mut world),
                expected,
                "separator moved at {frame}"
            );
            let columns = *world.resource::<Columns>();
            let fields = world.resource::<Playback>().0.caption_fields();
            assert!(
                fields
                    .iter()
                    .zip(columns.widths)
                    .all(|(text, width)| text.chars().count() <= width)
            );
        }
    }
}
