use std::io::Write;

use bevy::math::Vec2;
use serde::Serialize;

use super::{Simulation, framing};
use crate::layout::{Layout, packing::Tile};

pub(crate) fn largest_empty(tiles: &[Tile]) -> f32 {
    let mut edges = vec![-1.0, 1.0];
    for tile in tiles {
        edges.extend([tile.low.x, tile.high.x]);
    }
    edges.sort_by(f32::total_cmp);
    edges.dedup();
    let mut largest = 0.0_f32;
    for (i, &left) in edges.iter().enumerate() {
        for &right in &edges[i + 1..] {
            let mut blocked: Vec<_> = tiles
                .iter()
                .filter(|tile| tile.low.x < right && tile.high.x > left)
                .map(|tile| (tile.low.y, tile.high.y))
                .collect();
            blocked.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut bottom = -1.0_f32;
            for (low, high) in blocked {
                largest = largest.max((right - left) * (low - bottom) * 0.25);
                bottom = bottom.max(high);
            }
            largest = largest.max((right - left) * (1.0 - bottom) * 0.25);
        }
    }
    largest
}

#[derive(Serialize)]
struct Repository {
    label: String,
    tile_low: [f32; 2],
    tile_high: [f32; 2],
    eased_files: f32,
    tile_area: f32,
    content_area: f32,
    content_fill: f32,
}

#[derive(Serialize)]
pub(crate) struct Metrics {
    pub coverage: f32,
    pub blank_fraction: f32,
    pub largest_empty_rectangle: f32,
    content_fill: f32,
    repositories: Vec<Repository>,
}

pub(crate) fn measure(layout: &Layout) -> Metrics {
    let mut repositories = Vec::new();
    let mut tiles = Vec::new();
    let mut content = 0.0;
    let mut coverage = 0.0;
    for (index, region) in layout.repositories.iter().enumerate() {
        let Some(tile) = region.tile else {
            continue;
        };
        let mut low = Vec2::splat(f32::INFINITY);
        let mut high = Vec2::splat(f32::NEG_INFINITY);
        for (_, dir) in layout.dirs().filter(|(_, dir)| dir.repository == index) {
            let centre = crate::layout::region_coordinates(dir.position);
            low = low.min(centre - Vec2::splat(dir.radius()));
            high = high.max(centre + Vec2::splat(dir.radius()));
        }
        for (_, file) in layout
            .files()
            .filter(|(_, file)| layout.dir(file.dir).repository == index)
        {
            let radius = 0.27 * file.visible_presence(layout.dir(file.dir));
            if radius <= 0.0 {
                continue;
            }
            let point = crate::layout::region_coordinates(file.position);
            low = low.min(point - Vec2::splat(radius));
            high = high.max(point + Vec2::splat(radius));
        }
        let content_area = if low.is_finite() {
            framing::project_rectangle(low, high)
        } else {
            0.0
        };
        let tile_area = tile.area();
        content += content_area;
        coverage += tile_area;
        tiles.push(tile);
        repositories.push(Repository {
            label: region.label.clone(),
            tile_low: tile.low.to_array(),
            tile_high: tile.high.to_array(),
            eased_files: region.files,
            tile_area,
            content_area,
            content_fill: content_area / tile_area.max(1e-12),
        });
    }
    Metrics {
        coverage,
        blank_fraction: 1.0 - coverage,
        largest_empty_rectangle: largest_empty(&tiles),
        content_fill: content / coverage.max(1e-12),
        repositories,
    }
}

pub(crate) fn run(
    simulation: &mut Simulation,
    writer: &mut impl Write,
) -> anyhow::Result<()> {
    serde_json::to_writer(
        &mut *writer,
        &serde_json::json!({"definitions":
        "All areas are fractions of the full 1280x720 frame. Glass tiles are disjoint screen rectangles inside the 24 px margin. Largest empty rectangle is the exact maximum axis-aligned rectangle outside their union, including the margin. Content is the projected ground-plane AABB of current directory discs and file positions padded by their rendered size; not glyph ink or opaque pixel occupancy. Content fill divides content-envelope area by tile area; overall fill is area weighted. The first repository owns the empty lead-in tile. Weights are eased live file counts; empty active repositories retain weight one until archived. Samples include every commit boundary and every playback second."}),
    )?;
    writeln!(writer)?;
    let frames = super::frames_for(simulation.duration(), 60)?;
    let started = std::time::Instant::now();
    let placement = simulation.packing.sample(u64::from(frames));
    let elapsed = started.elapsed().as_secs_f64();
    std::hint::black_box(placement);
    eprintln!("treemap_single_sample_seconds={elapsed:.9}");
    let mut previous_commit = usize::MAX;
    for frame in 0..=frames {
        simulation.advance_to(frame, 60);
        if simulation.commit() != previous_commit
            || frame.is_multiple_of(60)
            || frame == frames
        {
            serde_json::to_writer(
                &mut *writer,
                &serde_json::json!({"frame": frame,
                "seconds": super::tick_time(u64::from(frame)), "commit": simulation.commit(),
                "metrics": measure(simulation.layout())}),
            )?;
            writeln!(writer)?;
            previous_commit = simulation.commit();
        }
        if frame.is_multiple_of(600) {
            eprintln!("treemap frame={frame}/{frames}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_rectangle_control_detects_a_missing_tile() {
        let full = Tile::viewport();
        let left = Tile {
            high: Vec2::new(0.0, full.high.y),
            ..full
        };
        let right = Tile {
            low: Vec2::new(0.0, full.low.y),
            ..full
        };
        assert!((largest_empty(&[left, right]) - 24.0 / 720.0).abs() < 1e-6);
        assert!(largest_empty(&[left]) > 0.45);
        assert_eq!(largest_empty(&[]), 1.0);
    }

    #[test]
    fn realistic_thirty_repository_history_fills_every_live_tick_and_replays() {
        let histories = (0..30)
            .map(|index| {
                let mut events = crate::timeline::synthetic();
                for event in &mut events {
                    event.seconds += i64::try_from(index).unwrap() * 86_400 * 3;
                }
                crate::repositories::namespace(&mut events, index);
                events
            })
            .collect();
        let names: Vec<_> = (0..30).map(|i| format!("Repository {i}")).collect();
        let mut simulation = Simulation::with_repositories(
            crate::repositories::merge(histories),
            &super::super::Options {
                seconds_per_day: 0.02,
                max_idle_seconds: 0.05,
                ..Default::default()
            },
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &names,
        );
        let frames = super::super::frames_for(simulation.duration(), 60).unwrap();
        let expected = Tile::viewport().area();
        for frame in 0..=frames {
            simulation.advance_to(frame, 60);
            let regions = &simulation.layout().repositories;
            let tiles: Vec<_> = regions.iter().filter_map(|r| r.tile).collect();
            assert!(!tiles.is_empty(), "blank lead-in at frame {frame}");
            assert!(
                (tiles.iter().map(|t| t.area()).sum::<f32>() - expected).abs() < 2e-6
            );
            for (i, a) in tiles.iter().enumerate() {
                assert!(a.low.cmpge(Tile::viewport().low - Vec2::splat(1e-6)).all());
                assert!(
                    a.high
                        .cmple(Tile::viewport().high + Vec2::splat(1e-6))
                        .all()
                );
                for b in &tiles[i + 1..] {
                    assert!(
                        a.high.x <= b.low.x + 1e-6
                            || b.high.x <= a.low.x + 1e-6
                            || a.high.y <= b.low.y + 1e-6
                            || b.high.y <= a.low.y + 1e-6
                    );
                }
            }
            if frame.is_multiple_of(30) {
                assert!(largest_empty(&tiles) < 0.034);
            }
            assert_eq!(
                simulation.view(),
                (bevy::math::Vec3::ZERO, framing::DISTANCE)
            );
            for avatar in simulation.avatars().iter().filter(|a| a.presence > 0.0) {
                let projected = super::super::project(
                    super::super::view_direction() * framing::DISTANCE,
                    bevy::math::Vec3::ZERO,
                    &[avatar.position],
                )
                .next()
                .flatten()
                .unwrap();
                assert!((projected.y - 0.60).abs() < 1e-5);
                assert!(projected.x.abs() <= 0.8001);
            }
        }
        let snapshot = serde_json::to_vec(&measure(simulation.layout())).unwrap();
        simulation.advance_to(0, 60);
        simulation.advance_to(frames, 60);
        assert_eq!(
            snapshot,
            serde_json::to_vec(&measure(simulation.layout())).unwrap()
        );
    }
}
