use bevy::math::Vec2;

const WIDTH: usize = 160;
const HEIGHT: usize = 90;
const MARGIN: usize = 3;

fn interval(points: &[Vec2], y: f32) -> (f32, f32) {
    let mut low = f32::INFINITY;
    let mut high = f32::NEG_INFINITY;
    let mut before = points[points.len() - 1];
    for &point in points {
        if (point.y - before.y).abs() > 1e-8
            && y >= point.y.min(before.y) - 1e-7
            && y <= point.y.max(before.y) + 1e-7
        {
            let x = before.x
                + (point.x - before.x) * ((y - before.y) / (point.y - before.y));
            low = low.min(x);
            high = high.max(x);
        }
        before = point;
    }
    (low, high)
}

pub(super) fn largest(polygons: &[Vec<Vec2>]) -> f32 {
    let mut occupied = [false; WIDTH * HEIGHT];
    for polygon in polygons.iter().filter(|p| p.len() >= 3) {
        let (low, high) = crate::layout::outline::bounds(polygon);
        for row in MARGIN..HEIGHT - MARGIN {
            let y =
                -1.0 + 2.0 * f32::from(u16::try_from(row).expect("grid row")) / 90.0;
            if y < low.y - 1e-7 || y + 2.0 / 90.0 > high.y + 1e-7 {
                continue;
            }
            let a = interval(polygon, y);
            let b = interval(polygon, y + 2.0 / 90.0);
            let left = a.0.max(b.0);
            let right = a.1.min(b.1);
            if left >= right {
                continue;
            }
            let begin = Vec2::splat(((left + 1.0) * 80.0).ceil().clamp(0.0, 160.0))
                .as_uvec2()
                .x;
            let end = Vec2::splat(((right + 1.0) * 80.0).floor().clamp(0.0, 160.0))
                .as_uvec2()
                .x;
            for col in usize::try_from(begin).expect("grid column")
                ..usize::try_from(end).expect("grid column")
            {
                occupied[row * WIDTH + col] = true;
            }
        }
    }
    let mut heights = [0_usize; WIDTH];
    let mut largest = 0_usize;
    let mut stack = Vec::with_capacity(WIDTH);
    for row in MARGIN..HEIGHT - MARGIN {
        stack.clear();
        for col in MARGIN..=WIDTH - MARGIN {
            let height = if col == WIDTH - MARGIN {
                0
            } else {
                heights[col] = if occupied[row * WIDTH + col] {
                    0
                } else {
                    heights[col] + 1
                };
                heights[col]
            };
            while let Some(&before) = stack.last() {
                if heights[before] <= height {
                    break;
                }
                stack.pop();
                let left = stack.last().map_or(MARGIN, |p| p + 1);
                largest = largest.max(heights[before] * (col - left));
            }
            stack.push(col);
        }
    }
    crate::model::count_u32(u32::try_from(largest).expect("bounded pixel grid"))
        / 12_936.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_missing_half_and_rejects_a_filled_view() {
        let t = crate::layout::packing::Tile::viewport();
        assert!(largest(&[t.corners().to_vec()]) < 0.025);
        let half = crate::layout::outline::rectangle(t.low, Vec2::new(0.0, t.high.y));
        assert!(largest(&[half]) > 0.49);
        assert_eq!(largest(&[]), 1.0);
    }
}
