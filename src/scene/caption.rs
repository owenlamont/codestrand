use bevy::prelude::*;

use super::Playback;
use crate::model::caption::Style;

#[derive(Component)]
struct Bar;

#[derive(Component)]
enum Part {
    Date,
    Repository,
    Rest,
}

pub(super) fn setup(world: &mut World, camera: Entity) {
    let root = world
        .spawn((
            Bar,
            Node {
                position_type: PositionType::Absolute,
                left: px(0.0),
                top: px(698.0),
                width: percent(100.0),
                height: px(22.0),
                align_items: AlignItems::Center,
                padding: UiRect::horizontal(px(28.0)),
                ..default()
            },
            BackgroundColor(Color::BLACK),
            UiTargetCamera(camera),
        ))
        .id();
    for part in [Part::Date, Part::Repository, Part::Rest] {
        let child = world
            .spawn((
                part,
                Text::new(" "),
                TextFont::from_font_size(14.0),
                TextColor(Color::WHITE),
                Node {
                    padding: UiRect::horizontal(px(4.0)),
                    flex_shrink: 0.0,
                    ..default()
                },
                BackgroundColor(Color::NONE),
            ))
            .id();
        world.entity_mut(root).add_child(child);
    }
}

pub(super) fn update(world: &mut World, full: &str) {
    let simulation = &world.resource::<Playback>().0;
    if simulation.layout().repositories.is_empty() {
        return;
    }
    let repository = simulation.caption_repository().map(|(_, r)| r.to_owned());
    let style = simulation.caption_style;
    let tint = simulation.caption_background();
    let (date, rest) = repository.as_ref().map_or((full, ""), |name| {
        let prefix = full.find("   |   ").expect("repository caption has a date")
            + "   |   ".len();
        (&full[..prefix], &full[prefix + name.len()..])
    });
    for mut background in world
        .query_filtered::<&mut BackgroundColor, With<Bar>>()
        .iter_mut(world)
    {
        background.set_if_neq(BackgroundColor(if style == Style::Bar {
            tint
        } else {
            Color::BLACK
        }));
    }
    for (part, mut text, mut background) in world
        .query::<(&Part, &mut Text, &mut BackgroundColor)>()
        .iter_mut(world)
    {
        let value = match part {
            Part::Date => date,
            Part::Repository => repository.as_deref().unwrap_or(""),
            Part::Rest => rest,
        };
        text.set_if_neq(Text::new(value));
        background.set_if_neq(BackgroundColor(
            if matches!(part, Part::Repository)
                && style == Style::Chip
                && repository.is_some()
            {
                tint
            } else {
                Color::NONE
            },
        ));
    }
}
