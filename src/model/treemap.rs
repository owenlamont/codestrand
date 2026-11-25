use std::io::Write;

use bevy::math::Vec2;
use serde::Serialize;

use super::{Simulation, framing};
use crate::layout::{Layout, packing::Tile};
mod density;
mod gaps;

const EMPTY_RECTANGLE_WORK: usize = 1_000_000;
const DEFINITIONS: &str = "Tile and content areas use fixed world coordinates expressed in reference-field units. Coverage and empty fractions normalize the grown field back to the reference viewport. Glass tiles are disjoint world rectangles represented in this field, inside a 24 reference-pixel margin plus the caption reservation below, separated by shared gutters. Largest empty rectangle is the exact maximum axis-aligned rectangle outside their union, including the margin, or null when the work budget is reached. Content is the world-XZ AABB of current directory discs and file positions padded by their model footprint size, converted with the fixed affine field scale; not projected ground-plane area, glyph ink or opaque pixel occupancy. Content fill divides content-envelope area by tile area; overall fill is area weighted. The first repository owns the empty lead-in tile. Weights are live file and directory counts, held until their shares drift and then eased over two seconds; empty active repositories weigh one until archived. Files and directories count live files not leaving and live directories. Density uses live directories including roots and files not leaving, including queued arrivals. Its reference is total live items per total active world-space tile area. Samples include every canonical 1/60 second. Content AABBs are clipped to their own tiles; actual footprints use convex outlines. Content largest-empty rectangles use fully covered cells on a 160x90 grid inside the usable tile viewport, excluding the caption reservation, with one grid cell of boundary uncertainty.";

fn largest_empty(tiles: &[Tile]) -> Option<f32> {
    let count = tiles.len().max(1);
    let setup = count.saturating_mul(4 + 4 * count.ilog2() as usize);
    let mut budget = EMPTY_RECTANGLE_WORK.checked_sub(setup)?;
    let mut edges = vec![-1.0, 1.0];
    for tile in tiles {
        edges.extend([tile.low.x, tile.high.x]);
    }
    edges.sort_by(f32::total_cmp);
    edges.dedup();
    let mut tiles = tiles.to_vec();
    tiles.sort_by(|a, b| a.low.y.total_cmp(&b.low.y));
    let mut largest = 0.0_f32;
    for (i, &left) in edges.iter().enumerate() {
        for &right in &edges[i + 1..] {
            budget = budget.checked_sub(count)?;
            let blocked = tiles
                .iter()
                .filter(|tile| tile.low.x < right && tile.high.x > left);
            let mut bottom = -1.0_f32;
            for tile in blocked {
                largest = largest.max((right - left) * (tile.low.y - bottom) * 0.25);
                bottom = bottom.max(tile.high.y);
            }
            largest = largest.max((right - left) * (1.0 - bottom) * 0.25);
        }
    }
    Some(largest)
}

#[derive(Serialize)]
struct Repository {
    index: usize,
    active: bool,
    label: String,
    tile_low: [f32; 2],
    tile_high: [f32; 2],
    eased_items: f32,
    /// Live files, not counting those leaving, and live directories.
    files: usize,
    directories: usize,
    tile_area: f32,
    density: Option<density::Sample>,
    content_area: f32,
    content_fill: f32,
    content_low: [f32; 2],
    content_high: [f32; 2],
    reserved_low: [f32; 2],
    reserved_high: [f32; 2],
    current_low: [f32; 2],
    current_high: [f32; 2],
    scale: f32,
    rotated: bool,
    usable_fill: f32,
    cropped: bool,
}

#[derive(Serialize)]
pub(crate) struct Metrics {
    density: Option<f64>,
    pub coverage: f32,
    pub blank_fraction: f32,
    pub largest_empty_rectangle: Option<f32>,
    content_fill: f32,
    repositories: Vec<Repository>,
    pub content_largest_empty_inset: f32,
}

fn framing_metrics(
    region: &crate::layout::RepositoryRegion,
    tile: Tile,
) -> (f32, bool) {
    let usable = tile.usable();
    let projected: Vec<_> = region
        .outline
        .iter()
        .copied()
        .map(framing::world_to_field)
        .collect();
    let usable_fill =
        crate::layout::outline::area(&projected) * 0.25 / usable.area().max(1e-12);
    let cropped = region
        .outline
        .iter()
        .flat_map(|&p| {
            [
                framing::world_to_field(p),
                framing::project_field_at(p, region.height),
            ]
        })
        .any(|p| {
            p.cmplt(usable.low - Vec2::splat(1e-5)).any()
                || p.cmpgt(usable.high + Vec2::splat(1e-5)).any()
        });
    (usable_fill, cropped)
}

fn clip_content(low: Vec2, high: Vec2, tile: Tile) -> Vec<Vec2> {
    let mut polygon: Vec<_> = crate::layout::outline::rectangle(low, high)
        .into_iter()
        .map(framing::world_to_field)
        .collect();
    for axis in [Vec2::X, Vec2::Y] {
        polygon = crate::layout::outline::clip(&polygon, axis, axis.dot(tile.high));
        polygon = crate::layout::outline::clip(&polygon, -axis, -axis.dot(tile.low));
    }
    polygon
}

fn measure(layout: &Layout, growth: f32) -> Metrics {
    let viewport = Tile::viewport();
    let center = viewport.low.midpoint(viewport.high);
    let normalize = |point: Vec2| center + (point - center) / growth;
    let mut repositories = Vec::new();
    let mut tiles = Vec::new();
    let mut content = 0.0;
    let mut coverage = 0.0;
    let mut polygons = Vec::new();
    for (index, region) in layout.repositories.iter().enumerate() {
        let Some(tile) = region.tile else {
            continue;
        };
        let mut low = Vec2::splat(f32::INFINITY);
        let mut high = Vec2::splat(f32::NEG_INFINITY);
        let mut current_low = low;
        let mut current_high = high;
        let mut scale = 0.0_f32;
        let (mut files, mut directories) = (0, 0);
        for (_, dir) in layout.dirs().filter(|(_, dir)| dir.repository == index) {
            directories += 1;
            let centre = dir.position;
            scale = dir.scale;
            low = low.min(centre - Vec2::splat(dir.radius()));
            high = high.max(centre + Vec2::splat(dir.radius()));
            current_low = current_low.min(centre - Vec2::splat(dir.radius() + scale));
            current_high = current_high.max(centre + Vec2::splat(dir.radius() + scale));
        }
        for (_, file) in layout
            .files()
            .filter(|(_, file)| layout.dir(file.dir).repository == index)
        {
            files += usize::from(file.state != crate::layout::Presence::Leaving);
            let point = file.position;
            scale = layout.dir(file.dir).scale;
            current_low = current_low.min(point - Vec2::splat(scale));
            current_high = current_high.max(point + Vec2::splat(scale));
            let radius = 0.27 * file.visible_presence(layout.dir(file.dir));
            if radius <= 0.0 {
                continue;
            }
            low = low.min(point - Vec2::splat(radius));
            high = high.max(point + Vec2::splat(radius));
        }
        let content_area = if low.is_finite() {
            let polygon = clip_content(low, high, tile);
            let area = crate::layout::outline::area(&polygon) * 0.25;
            polygons.push(polygon.into_iter().map(normalize).collect());
            area
        } else {
            0.0
        };
        let tile_area = tile.area();
        let (usable_fill, cropped) = framing_metrics(region, tile);
        content += content_area;
        coverage += tile_area / growth.powi(2);
        tiles.push(Tile {
            low: normalize(tile.low),
            high: normalize(tile.high),
        });
        repositories.push(Repository {
            index,
            active: region.live,
            density: None,
            label: region.label.clone(),
            tile_low: tile.low.to_array(),
            tile_high: tile.high.to_array(),
            eased_items: region.items,
            files,
            directories,
            tile_area,
            content_area,
            content_fill: content_area / tile_area.max(1e-12),
            content_low: low.to_array(),
            content_high: high.to_array(),
            reserved_low: region.low.to_array(),
            reserved_high: region.high.to_array(),
            current_low: current_low.to_array(),
            current_high: current_high.to_array(),
            scale,
            rotated: region.rotated,
            usable_fill,
            cropped,
        });
    }
    let density = density::measure(&mut repositories);
    Metrics {
        density,
        coverage,
        blank_fraction: 1.0 - coverage,
        largest_empty_rectangle: largest_empty(&tiles),
        content_fill: content / (coverage * growth.powi(2)).max(1e-12),
        repositories,
        content_largest_empty_inset: gaps::largest(&polygons),
    }
}

pub(crate) fn run(
    simulation: &mut Simulation,
    writer: &mut impl Write,
) -> anyhow::Result<()> {
    serde_json::to_writer(
        &mut *writer,
        &serde_json::json!({"definitions":
        DEFINITIONS}),
    )?;
    writeln!(writer)?;
    let mut frames = simulation.frames(60)?;
    let started = std::time::Instant::now();
    let placement = simulation.packing.sample(u64::from(frames));
    let elapsed = started.elapsed().as_secs_f64();
    std::hint::black_box(placement);
    eprintln!("treemap_single_sample_seconds={elapsed:.9}");
    let mut moving_beams = super::slots::diagnostics::MovingBeams::default();
    let mut occlusion = super::occlusion::Audit::default();
    let mut density = density::Audit::default();
    let mut frame = 0;
    loop {
        simulation.prepare_frame(frame, 60, true)?;
        frames = simulation.frames(60)?;
        if simulation.planning_complete() && frame > frames {
            break;
        }
        simulation.advance_to(frame, 60);
        moving_beams.observe(simulation);
        occlusion.observe(simulation);
        let metrics =
            measure(simulation.layout(), simulation.packing.overview_growth());
        let in_transition = density::in_transition(simulation);
        density.observe(frame, in_transition, &metrics.repositories);
        serde_json::to_writer(
            &mut *writer,
            &serde_json::json!({
                "frame": frame, "seconds": super::tick_time(u64::from(frame)).lossy_secs_f32(),
                "commit": simulation.commit(), "tile_transition": in_transition, "metrics": metrics
            }),
        )?;
        writeln!(writer)?;
        if frame.is_multiple_of(600) {
            eprintln!("treemap frame={frame}/{frames}");
        }
        frame += 1;
    }
    serde_json::to_writer(
        &mut *writer,
        &serde_json::json!({"density": density.report, "occlusion": occlusion, "moving_beams": moving_beams, "avatar_slots": super::slots::report(&simulation.state), "travel": simulation.travel.report()}),
    )?;
    writeln!(writer)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Options, frames_for};
    use bevy::math::Vec3;

    #[test]
    fn dense_treemap_omits_the_empty_rectangle_within_the_work_budget() {
        let tiles: Vec<_> = (0..200_u16)
            .map(|i| Tile {
                low: Vec2::new(f32::from(i) / 100.0 - 1.0, -0.5),
                high: Vec2::new(f32::from(i + 1) / 100.0 - 1.0, 0.5),
            })
            .collect();
        assert!(DEFINITIONS.contains("null when the work budget is reached"));
        assert_eq!(
            serde_json::to_value(largest_empty(&tiles)).unwrap(),
            serde_json::Value::Null
        );
    }

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
        assert!(
            (largest_empty(&[left, right]).unwrap() - f32::midpoint(1.0, full.low.y))
                .abs()
                < 1e-6
        );
        assert!(largest_empty(&[left]).unwrap() > 0.45);
        assert_eq!(largest_empty(&[]), Some(1.0));
    }

    fn histories() -> Vec<Vec<crate::timeline::Event>> {
        (0..30)
            .map(|index| {
                let mut events = crate::timeline::synthetic();
                for event in &mut events {
                    event.seconds += i64::try_from(index).unwrap() * 86_400 * 3;
                }
                crate::repositories::namespace(&mut events, index);
                events
            })
            .collect()
    }

    #[test]
    fn realistic_thirty_repository_history_fills_every_live_tick_and_replays() {
        let histories = histories();
        let names: Vec<_> = (0..30).map(|i| format!("Repository {i}")).collect();
        let mut simulation = Simulation::with_repositories(
            crate::repositories::merge(histories),
            &Options {
                seconds_per_day: 0.02,
                max_idle_seconds: 0.05,
                ..Default::default()
            },
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
            &names,
        );
        let frames = frames_for(simulation.duration(), 60).unwrap();

        let rotations: Vec<_> = simulation
            .layout()
            .repositories
            .iter()
            .map(|r| r.rotated)
            .collect();

        for frame in 0..=frames {
            simulation.advance_to(frame, 60);
            let regions = &simulation.layout().repositories;
            assert_eq!(
                regions.iter().map(|r| r.rotated).collect::<Vec<_>>(),
                rotations
            );
            let tiles: Vec<_> = regions.iter().filter_map(|r| r.tile).collect();
            assert!(!tiles.is_empty(), "blank lead-in at frame {frame}");

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
            assert_eq!(simulation.view(), (Vec3::ZERO, framing::DISTANCE));
            for region in regions {
                if let Some(tile) = region.tile {
                    assert!(
                        !framing_metrics(region, tile).1,
                        "cropped {} at {frame}",
                        region.label
                    );
                }
            }
        }
        assert_eq!(simulation.state.slot_records.len(), simulation.events.len());
        let snapshot = serde_json::to_vec(&measure(
            simulation.layout(),
            simulation.packing.overview_growth(),
        ))
        .unwrap();
        simulation.advance_to(0, 60);
        simulation.advance_to(frames, 60);
        assert_eq!(
            snapshot,
            serde_json::to_vec(&measure(
                simulation.layout(),
                simulation.packing.overview_growth()
            ))
            .unwrap()
        );
    }
}
