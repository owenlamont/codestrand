use bevy::{
    picking::hover::Hovered,
    prelude::*,
    ui::Checked,
    ui_widgets::{
        Checkbox, RadioButton, RadioGroup, Slider, SliderPrecision, SliderRange,
        SliderStep, SliderThumb, SliderValue, TrackClick, ValueChange, observe,
    },
};
use clap::ValueEnum;

use super::{Playback, Settings, label};
use crate::{model::caption::Style, theme::ThemeName};

/// A theme chosen in the panel, applied by the preview clock like a theme key.
#[derive(Resource)]
pub(in crate::scene) struct RequestedTheme(pub(in crate::scene) ThemeName);

#[derive(Component, Clone, Copy, PartialEq)]
pub(super) enum Choice {
    Theme(ThemeName),
    Caption(Style),
    Collapse,
}

#[derive(Component)]
pub(super) struct Speed;

#[derive(Component)]
pub(super) struct SpeedLabel;

const THUMB: f32 = 14.0;
const IDLE: Color = Color::srgb(0.15, 0.19, 0.26);
const HOVER: Color = Color::srgb(0.25, 0.32, 0.43);
const CHOSEN: Color = Color::srgb(0.20, 0.42, 0.62);

pub(super) fn page() -> impl Bundle {
    (
        super::page(),
        Children::spawn((
            Spawn(label("Theme · , and .", 16.0)),
            Spawn((
                choices(ThemeName::ALL.map(|name| (name.title(), Choice::Theme(name)))),
                observe(choose),
            )),
            Spawn((label("", 16.0), SpeedLabel)),
            Spawn(speed()),
            Spawn(label("Caption style", 16.0)),
            Spawn((
                choices(Style::value_variants().iter().map(|&style| {
                    let name = style
                        .to_possible_value()
                        .expect("caption styles are all visible to clap");
                    (name.get_name().to_owned(), Choice::Caption(style))
                })),
                observe(choose),
            )),
            Spawn((
                Checkbox,
                Choice::Collapse,
                Hovered::default(),
                Node {
                    align_self: AlignSelf::FlexStart,
                    padding: UiRect::all(px(6)),
                    ..default()
                },
                BackgroundColor(IDLE),
                children![label("Collapse small directories", 16.0)],
                observe(collapse),
            )),
        )),
    )
}

fn choices(
    options: impl IntoIterator<Item = (impl Into<String>, Choice)>,
) -> impl Bundle {
    let buttons: Vec<_> = options
        .into_iter()
        .map(|(name, choice)| {
            (
                RadioButton,
                choice,
                Hovered::default(),
                option(&name.into()),
            )
        })
        .collect();
    (
        RadioGroup,
        Node {
            flex_wrap: FlexWrap::Wrap,
            column_gap: px(6),
            row_gap: px(6),
            ..default()
        },
        Children::spawn(SpawnIter(buttons.into_iter())),
    )
}

fn option(name: &str) -> impl Bundle + use<> {
    (
        Node {
            padding: UiRect::all(px(6)),
            ..default()
        },
        BackgroundColor(IDLE),
        children![label(name, 16.0)],
    )
}

fn speed() -> impl Bundle {
    let multipliers = crate::model::PLAYBACK_MULTIPLIERS;
    (
        Speed,
        Slider {
            // Snap and step clicks read window coordinates, which a letterboxed
            // viewport offsets; dragging the thumb uses only pointer travel.
            track_click: TrackClick::Drag,
            ..default()
        },
        SliderRange::new(multipliers.start().log2(), multipliers.end().log2()),
        SliderValue(0.0),
        SliderStep(1.0),
        SliderPrecision(0),
        Node {
            height: px(THUMB),
            ..default()
        },
        BackgroundColor(Color::srgb(0.10, 0.12, 0.16)),
        children![(
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                right: px(THUMB),
                top: px(0),
                bottom: px(0),
                ..default()
            },
            children![(
                SliderThumb,
                Node {
                    position_type: PositionType::Absolute,
                    width: px(THUMB),
                    height: px(THUMB),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.5, 0.6, 0.7)),
            )],
        )],
        observe(change_speed),
    )
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy passes observer parameters by value"
)]
fn choose(
    change: On<ValueChange<Entity>>,
    settings: Res<Settings>,
    choices: Query<&Choice>,
    mut playback: ResMut<Playback>,
    mut commands: Commands,
) {
    if !settings.open {
        return;
    }
    match choices.get(change.value) {
        Ok(Choice::Theme(name)) => commands.insert_resource(RequestedTheme(*name)),
        Ok(Choice::Caption(style)) => playback.0.caption_style = *style,
        Ok(Choice::Collapse) | Err(_) => {}
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy passes observer parameters by value"
)]
fn collapse(
    change: On<ValueChange<bool>>,
    settings: Res<Settings>,
    mut playback: ResMut<Playback>,
) {
    if settings.open {
        playback.0.collapse_directories = change.value;
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy passes observer parameters by value"
)]
fn change_speed(
    change: On<ValueChange<f32>>,
    settings: Res<Settings>,
    mut playback: ResMut<Playback>,
) {
    if settings.open {
        playback
            .0
            .set_playback_multiplier(change.value.round().exp2());
    }
}

/// Shows the live simulation in the widgets, including changes made with keys.
pub(super) fn show(
    settings: Res<Settings>,
    playback: Res<Playback>,
    mut commands: Commands,
    mut choices: Query<(
        Entity,
        &Choice,
        Has<Checked>,
        &Hovered,
        &mut BackgroundColor,
    )>,
    slider: Single<(Entity, &SliderValue, &SliderRange), With<Speed>>,
    mut thumb: Single<&mut Node, With<SliderThumb>>,
    mut speed: Single<&mut Text, With<SpeedLabel>>,
) {
    if !settings.into_inner().open {
        return;
    }
    let simulation = &playback.into_inner().0;
    for (entity, choice, checked, hovered, mut background) in &mut choices {
        let chosen = match choice {
            Choice::Theme(name) => *name == simulation.theme().name,
            Choice::Caption(style) => *style == simulation.caption_style,
            Choice::Collapse => simulation.collapse_directories,
        };
        if chosen != checked {
            if chosen {
                commands.entity(entity).insert(Checked);
            } else {
                commands.entity(entity).remove::<Checked>();
            }
        }
        background.set_if_neq(BackgroundColor(match (chosen, hovered.get()) {
            (true, _) => CHOSEN,
            (false, true) => HOVER,
            (false, false) => IDLE,
        }));
    }
    let multiplier = simulation.playback_multiplier();
    let (entity, value, range) = slider.into_inner();
    let target = multiplier.log2();
    if value.0.to_bits() != target.to_bits() {
        commands.entity(entity).insert(SliderValue(target));
    }
    let left = percent(range.thumb_position(target) * 100.0);
    if thumb.left != left {
        thumb.left = left;
    }
    speed.set_if_neq(Text::new(format!(
        "Playback speed · {multiplier}x · [ and ]"
    )));
}

#[cfg(test)]
mod tests;
