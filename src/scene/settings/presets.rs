use bevy::prelude::*;

pub(super) const PRESETS: &[(u32, u32, &str)] = &[
    (1280, 720, "16:9"),
    (1600, 900, "16:9"),
    (1920, 1080, "16:9"),
    (2560, 1440, "16:9"),
    (3840, 2160, "16:9"),
    (1280, 800, "16:10"),
    (1920, 1200, "16:10"),
    (2560, 1600, "16:10"),
    (2560, 1080, "21:9"),
    (3440, 1440, "21:9"),
    (3840, 1600, "21:9"),
    (3840, 1080, "32:9"),
    (5120, 1440, "32:9"),
];

pub(super) fn fits(size: UVec2, monitor: UVec2) -> bool {
    size.x <= monitor.x && size.y <= monitor.y
}

pub(super) fn label(size: UVec2) -> String {
    let aspect = [
        (16, 9, "16:9"),
        (16, 10, "16:10"),
        (21, 9, "21:9"),
        (32, 9, "32:9"),
    ]
    .into_iter()
    .find_map(|(width, height, aspect)| {
        let actual = u64::from(size.x) * height;
        let nominal = u64::from(size.y) * width;
        (actual.abs_diff(nominal) * 100 <= nominal * 3).then_some(aspect)
    });
    let ratio = aspect.map_or_else(
        || {
            let (mut a, mut b) = (size.x, size.y);
            while b != 0 {
                (a, b) = (b, a % b);
            }
            format!("{}:{}", size.x / a.max(1), size.y / a.max(1))
        },
        str::to_owned,
    );
    format!("{} × {} ({ratio})", size.x, size.y)
}

pub(super) fn rectangle(monitor: UVec2, chosen: Option<UVec2>) -> URect {
    let size = chosen.unwrap_or(monitor).min(monitor);
    let origin = (monitor - size) / 2;
    URect::from_corners(origin, origin + size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_use_marketing_aspects_and_presets_are_sorted_within_groups() {
        for &(width, height, aspect) in PRESETS {
            assert_eq!(
                label(UVec2::new(width, height)),
                format!("{width} × {height} ({aspect})")
            );
        }
        for pair in PRESETS.windows(2) {
            assert!(
                pair[0].2 != pair[1].2 || pair[0].0 * pair[0].1 < pair[1].0 * pair[1].1
            );
        }
        assert_eq!(label(UVec2::new(3000, 2000)), "3000 × 2000 (3:2)");
        assert_eq!(label(UVec2::new(3840, 2400)), "3840 × 2400 (16:10)");
        assert_eq!(label(UVec2::new(1366, 768)), "1366 × 768 (16:9)");
    }

    #[test]
    fn ultrawide_monitor_rejects_sizes_exceeding_either_dimension() {
        let monitor = UVec2::new(3440, 1440);
        assert!(fits(UVec2::new(3440, 1440), monitor));
        assert!(fits(UVec2::new(2560, 1440), monitor));
        assert!(!fits(UVec2::new(3840, 1080), monitor));
        assert!(!fits(UVec2::new(2560, 1600), monitor));
        assert!(!fits(UVec2::new(3840, 2160), monitor));
    }

    #[test]
    fn viewports_are_native_centred_and_never_upscaled() {
        let monitor = UVec2::new(3440, 1440);
        assert_eq!(
            rectangle(monitor, Some(UVec2::new(1920, 1080))),
            URect::new(760, 180, 2680, 1260)
        );
        assert_eq!(
            rectangle(monitor, Some(UVec2::new(2560, 1440))),
            URect::new(440, 0, 3000, 1440)
        );
        assert_eq!(
            rectangle(UVec2::new(3840, 2160), Some(UVec2::new(3840, 1600))),
            URect::new(0, 280, 3840, 1880)
        );
        assert_eq!(rectangle(monitor, None), URect::new(0, 0, 3440, 1440));
        assert_eq!(
            rectangle(UVec2::new(1281, 721), Some(UVec2::new(1280, 720))).size(),
            UVec2::new(1280, 720)
        );
    }
}
