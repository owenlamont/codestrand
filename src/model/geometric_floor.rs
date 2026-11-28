use bevy::math::Vec2;

use crate::layout::packing::{BOUND, Tile};

use super::framing;

pub(super) fn bounds(tiles: impl Iterator<Item = Tile>) -> Tile {
    tiles.fold(
        Tile {
            low: Vec2::splat(f32::INFINITY),
            high: Vec2::splat(f32::NEG_INFINITY),
        },
        |field, tile| Tile {
            low: field.low.min(tile.low),
            high: field.high.max(tile.high),
        },
    )
}

pub(super) fn minimum_area(tile: Tile, field: Tile) -> f32 {
    let square = 16.0 * 16.0;
    let spanning = (tile.low - field.low).abs().cmple(Vec2::splat(1e-5))
        & (tile.high - field.high).abs().cmple(Vec2::splat(1e-5));
    let size = field.pixels();
    let pixels = match (spanning.x, spanning.y) {
        (true, false) => size.x * (size.x / BOUND).max(16.0),
        (false, true) => size.y * (size.y / BOUND).max(16.0),
        _ => square,
    }
    .max(square);
    pixels / framing::VIEWPORT.element_product()
}

pub(super) fn bound(ideal: f32, actual: f32, floor: f32) -> bool {
    ideal < floor && (actual / floor - 1.0).abs() <= 5e-4
}
