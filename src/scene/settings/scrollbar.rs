use bevy::{
    picking::{events::PointerPress, pointer::PointerButton},
    prelude::*,
    ui_widgets::{ControlOrientation, Scrollbar, ScrollbarThumb, observe},
    window::PrimaryWindow,
};

use super::AvatarCamera;

#[derive(Component)]
struct Track;

pub(super) fn widget(list: Entity) -> impl Bundle {
    (
        Node {
            width: px(14),
            flex_shrink: 0.0,
            ..default()
        },
        BackgroundColor(Color::srgb(0.10, 0.12, 0.16)),
        Scrollbar {
            target: list,
            orientation: ControlOrientation::Vertical,
            min_thumb_length: 24.0,
        },
        children![
            (
                Track,
                Node {
                    position_type: PositionType::Absolute,
                    width: percent(100),
                    height: percent(100),
                    ..default()
                },
                observe(track_press),
            ),
            (
                ScrollbarThumb::default(),
                ZIndex(1),
                BackgroundColor(Color::srgb(0.5, 0.6, 0.7)),
            ),
        ],
    )
}

// The child target prevents Bevy's window-coordinate track handler from also paging.
fn track_press(
    mut press: On<PointerPress>,
    tracks: Query<&ChildOf, With<Track>>,
    bars: Query<(&Scrollbar, &ComputedNode, &UiGlobalTransform)>,
    mut lists: Query<(&mut ScrollPosition, &ComputedNode)>,
    camera: Single<&Camera, With<AvatarCamera>>,
    window: Single<&Window, With<PrimaryWindow>>,
) {
    press.propagate(false);
    if press.button != PointerButton::Primary {
        return;
    }
    let Ok(parent) = tracks.get(press.entity) else {
        return;
    };
    let Ok((bar, node, transform)) = bars.get(parent.parent()) else {
        return;
    };
    let camera = camera.into_inner();
    let offset = camera
        .viewport
        .as_ref()
        .map_or(Vec2::ZERO, |viewport| viewport.physical_position.as_vec2());
    let point = press.pointer.position * window.into_inner().scale_factor() - offset;
    let Some(point) = node.normalize_point(*transform, point) else {
        return;
    };
    let Ok((mut position, content)) = lists.get_mut(bar.target) else {
        return;
    };
    let visible =
        (content.size().y - content.scrollbar_size.y) * content.inverse_scale_factor();
    let height = content.content_size().y * content.inverse_scale_factor();
    let step = if (point.y + 0.5) * height > position.y {
        visible
    } else {
        -visible
    };
    position.y = (position.y + step).clamp(0.0, (height - visible).max(0.0));
}

#[cfg(test)]
mod tests;
