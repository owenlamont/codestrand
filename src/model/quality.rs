//! CPU-only, tick-sampled quality measurements; definitions travel with every report.

use std::{collections::BTreeMap, io::Write};

use serde::Serialize;

use super::{Simulation, Vec2, Vec3, ground, project, view_direction};

/// Overlap is an unordered strict disc-intersection count, including hierarchy edges.
/// Frame counts and pair-frame sums use every explicit 60 Hz tick, including lead/tail.
/// Visibility measures four-cardinal-point disc spans with fade-scaled radius 0.27,
/// divided by viewport height through the simulation's smoothed 16:9 fit camera.
/// Mean size weights visible file-frame observations; the minimum includes fades.
/// Extent use divides the projected directory-cardinal-point AABB area by viewport area.
/// Movement sums retained-ID tick distances; births/reactivation placement are free,
/// while growth-induced motion of existing IDs and rename motion count.
/// Per-commit intervals start at the commit tick and end before the next commit tick.
/// Net movement sums lengths of accumulated displacement vectors per identity, so
/// backtracking cancels; path length and maximum individual tick jumps do not cancel.
const DEFINITIONS: &str = "Frames include t=0 and every 1/60 second through ceil(duration*60). Overlap counts unordered pairs of live directory discs with centre distance strictly less than summed radii (tangency excluded, parent/child and root included). Hotspots count pair-frames and record first/last commit, not necessarily a continuous interval. Movement is summed Euclidean ground-plane path length between consecutive ticks for IDs present in both snapshots; new entries and reactivated directories have no placement cost, retained IDs across renames do. Existing entries displaced by growth DO count. Per-commit movement belongs to the latest applied commit, including its idle interval and final tail; commit 0 is lead-in. Maximum jump is the largest individual retained-ID tick displacement. Visibility uses visible files (presence>0), a planar disc radius 0.27 times presence, and the actual smoothed fit camera at 16:9, 45 degree vertical FOV: diameter is the maximum horizontal/vertical span of four projected cardinal points as a fraction of viewport height, not pixels or glyph ink area. Extent use is the AABB of four projected cardinal points per live directory disc area divided by viewport area (unclipped, can exceed 1 during camera lag). Mean diameter weights file-frame observations; mean extent weights all frames; empty visibility is null. Net displacement sums magnitudes of retained-ID displacement vectors over the whole animation or each commit interval; unlike path length, backtracking cancels. Directory reactivation placement is omitted from both. Distances are world units; no occlusion or label readability is measured.";

#[derive(Default, Serialize)]
struct Movement {
    files: f64,
    directories: f64,
    max_file_jump: f64,
    max_directory_jump: f64,
}

impl Movement {
    fn accumulate(&mut self, movement: &Self) {
        self.files += movement.files;
        self.directories += movement.directories;
        self.max_file_jump = self.max_file_jump.max(movement.max_file_jump);
        self.max_directory_jump =
            self.max_directory_jump.max(movement.max_directory_jump);
    }
}
const FAST_SPEED: f64 = 6.0;

#[derive(Default)]
struct Speeds {
    values: Vec<f64>,
    fast_distance: f64,
}

impl Speeds {
    fn record<K: Ord>(
        &mut self,
        before: &BTreeMap<K, Vec2>,
        after: &BTreeMap<K, Vec2>,
    ) {
        for (id, position) in after {
            if let Some(previous) = before.get(id) {
                let distance = f64::from(position.distance(*previous));
                let speed = distance * 60.0;
                self.values.push(speed);
                if speed > FAST_SPEED {
                    self.fast_distance += distance;
                }
            }
        }
    }

    fn summary(&mut self) -> serde_json::Value {
        self.values.sort_by(f64::total_cmp);
        let percentile = |percent: usize| {
            self.values
                .get(
                    (self.values.len() * percent)
                        .div_ceil(100)
                        .saturating_sub(1),
                )
                .copied()
                .unwrap_or(0.0)
        };
        serde_json::json!({"p95": percentile(95), "p99": percentile(99),
            "max": self.values.last().copied().unwrap_or(0.0),
            "distance_above_threshold": self.fast_distance, "observations": self.values.len()})
    }
}

/// Birth and reactivation placement is excluded by intersecting consecutive live IDs.
fn displacement<K: Ord>(
    before: &BTreeMap<K, Vec2>,
    after: &BTreeMap<K, Vec2>,
) -> (f64, f64) {
    after
        .iter()
        .filter_map(|(id, position)| {
            before
                .get(id)
                .map(|previous| f64::from(position.distance(*previous)))
        })
        .fold((0.0, 0.0), |(sum, max), distance| {
            (sum + distance, max.max(distance))
        })
}

fn overlaps(a: (Vec2, f32), b: (Vec2, f32)) -> bool {
    a.0.distance_squared(b.0) < (a.1 + b.1).powi(2)
}

#[derive(Default, Serialize)]
struct Hotspot {
    frames: u32,
    first_commit: usize,
    last_commit: usize,
}

#[derive(Serialize)]
struct Sample {
    frame: u32,
    commit: usize,
    overlap_pairs: u32,
    file_clearance: FileClearance,
    visible_file_clearance: FileClearance,
    mean_diameter: Option<f64>,
    min_diameter: Option<f64>,
    extent_use: f64,
    movement: Movement,
}

#[derive(Default, Serialize)]
struct FileClearance {
    foreign_pair_ticks: u64,
    outside_own_ticks: u64,
    worst_foreign_depth: f32,
    worst_outside_depth: f32,
}

impl FileClearance {
    fn measure(layout: &crate::layout::Layout, visible: bool) -> Self {
        let mut result = Self::default();
        for (_, file) in layout.files().filter(|(_, file)| file.presence > 0.0) {
            let presence = if visible {
                file.visible_presence(layout.dir(file.dir))
            } else {
                file.presence
            };
            if presence <= 0.0 {
                continue;
            }
            let radius = 0.27 * presence;
            let own = layout.dir(file.dir);
            let outside =
                footprint_distance(file.position - own.position, own.footprint_radii())
                    + radius;
            result.outside_own_ticks += u64::from(outside > 0.0001);
            result.worst_outside_depth = result.worst_outside_depth.max(outside);
            for (_, dir) in layout.dirs().filter(|(id, _)| *id != file.dir) {
                let offset = file.position - dir.position;
                let radii = dir.footprint_radii();
                if offset.abs().cmpge(radii + radius).any() {
                    continue;
                }
                let depth = radius - footprint_distance(offset, radii);
                result.foreign_pair_ticks += u64::from(depth > 0.0001);
                result.worst_foreign_depth = result.worst_foreign_depth.max(depth);
            }
        }
        result
    }

    fn accumulate(&mut self, sample: &Self) {
        self.foreign_pair_ticks += sample.foreign_pair_ticks;
        self.outside_own_ticks += sample.outside_own_ticks;
        self.worst_foreign_depth =
            self.worst_foreign_depth.max(sample.worst_foreign_depth);
        self.worst_outside_depth =
            self.worst_outside_depth.max(sample.worst_outside_depth);
    }
}

fn visibility(simulation: &Simulation) -> (f64, u32, Option<f64>, f64) {
    let (focus, distance) = simulation.view();
    let eye = focus + view_direction() * distance;
    let mut sum = 0.0;
    let mut count = 0;
    let mut minimum = f64::INFINITY;
    for (_, file) in simulation.layout().files().filter(|(_, file)| {
        file.visible_presence(simulation.layout().dir(file.dir)) > 0.0
    }) {
        let centre = ground(file.position);
        let radius = 0.27 * file.visible_presence(simulation.layout().dir(file.dir));
        let points = [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z]
            .map(|side| centre + side * radius);
        let diameter = projected_extent(eye, focus, &points);
        let size = f64::from((diameter.x * 16.0 / 9.0).max(diameter.y) * 0.5);
        sum += size;
        minimum = minimum.min(size);
        count += 1;
    }
    let points: Vec<_> = simulation
        .layout()
        .dirs()
        .flat_map(|(_, dir)| {
            let centre = ground(dir.position);
            [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z]
                .map(|side| centre + side * dir.radius())
        })
        .collect();
    let extent = projected_extent(eye, focus, &points);
    (
        sum,
        count,
        (count > 0).then_some(minimum),
        f64::from(extent.x * extent.y * 0.25),
    )
}

fn projected_extent(eye: Vec3, focus: Vec3, points: &[Vec3]) -> Vec2 {
    let (low, high) = project(eye, focus, points).flatten().fold(
        (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)),
        |(low, high), point| (low.min(point), high.max(point)),
    );
    (high - low).max(Vec2::ZERO)
}

fn positions(
    layout: &crate::layout::Layout,
) -> (BTreeMap<u64, Vec2>, BTreeMap<usize, Vec2>) {
    (
        layout
            .files()
            .map(|(id, file)| (id, file.position))
            .collect(),
        layout.dirs().map(|(id, dir)| (id, dir.position)).collect(),
    )
}
/// Writes definitions, per-tick records, and a summary as JSON Lines without GPU startup.
fn write_record(
    writer: &mut impl Write,
    record: &impl serde::Serialize,
) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *writer, record)?;
    writeln!(writer)?;
    Ok(())
}
/// Runtime is deliberately excluded so identical inputs produce identical report bytes.
pub(crate) fn run(
    simulation: &mut Simulation,
    writer: &mut impl Write,
) -> anyhow::Result<()> {
    definitions(writer)?;
    let mut frames;
    let (mut previous_files, mut previous_dirs) = (BTreeMap::new(), BTreeMap::new());
    let mut hotspots: BTreeMap<String, Hotspot> = BTreeMap::new();
    let mut commits: BTreeMap<usize, Movement> = BTreeMap::new();
    let mut total = Movement::default();
    let (mut file_speeds, mut directory_speeds) =
        (Speeds::default(), Speeds::default());
    let (mut net, mut interval) = (NetMovement::default(), NetMovement::default());
    let mut interval_commit = simulation.commit();
    let mut per_commit_net = BTreeMap::new();
    let (mut overlapped_frames, mut worst_pairs, mut pair_frames) = (0, 0, 0_u64);
    let (mut diameter_sum, mut observations, mut minimum, mut extent_sum) =
        (0.0, 0.0, f64::INFINITY, 0.0);
    let (mut file_clearance, mut visible_file_clearance) =
        (FileClearance::default(), FileClearance::default());
    let mut moving_beams = super::slots::diagnostics::MovingBeams::default();
    let mut frame = 0;
    loop {
        simulation.prepare_frame(frame, 60, true)?;
        frames = simulation.frames(60)?;
        if simulation.planning_complete() && frame > frames {
            break;
        }
        simulation.advance_to(frame, 60);
        moving_beams.observe(simulation);
        progress(frame, frames, simulation.commit());
        let commit = simulation.commit();
        let layout = simulation.layout();
        let (files, dirs) = positions(layout);
        let (file_distance, file_jump) = displacement(&previous_files, &files);
        let (dir_distance, dir_jump) = displacement(&previous_dirs, &dirs);
        if commit != interval_commit {
            per_commit_net.insert(interval_commit, interval.measure());
            interval = NetMovement::default();
            interval_commit = commit;
        }
        net.record(&previous_files, &files, &previous_dirs, &dirs);
        interval.record(&previous_files, &files, &previous_dirs, &dirs);
        file_speeds.record(&previous_files, &files);
        directory_speeds.record(&previous_dirs, &dirs);
        previous_files = files;
        previous_dirs = dirs;
        let movement = Movement {
            files: file_distance,
            directories: dir_distance,
            max_file_jump: file_jump,
            max_directory_jump: dir_jump,
        };
        for accumulator in [&mut total, commits.entry(commit).or_default()] {
            accumulator.accumulate(&movement);
        }
        let pairs = count_overlaps(layout, commit, &mut hotspots);
        overlapped_frames += u32::from(pairs > 0);
        worst_pairs = worst_pairs.max(pairs);
        pair_frames += u64::from(pairs);
        let (sum, count, min, extent) = visibility(simulation);
        diameter_sum += sum;
        observations += f64::from(count);
        minimum = minimum.min(min.unwrap_or(f64::INFINITY));
        extent_sum += extent;
        let record = Sample {
            frame,
            commit,
            overlap_pairs: pairs,
            file_clearance: FileClearance::measure(layout, false),
            visible_file_clearance: FileClearance::measure(layout, true),
            mean_diameter: (count > 0).then(|| sum / f64::from(count)),
            min_diameter: min,
            extent_use: extent,
            movement,
        };
        file_clearance.accumulate(&record.file_clearance);
        visible_file_clearance.accumulate(&record.visible_file_clearance);
        write_record(writer, &record)?;
        frame += 1;
    }
    per_commit_net.insert(interval_commit, interval.measure());
    write_record(
        &mut *writer,
        &serde_json::json!({
            "summary": {
                "moving_beams": moving_beams, "avatar_slots": super::slots::report(&simulation.state),
                "frames": frames + 1, "overlapped_frames": overlapped_frames,
                "worst_pairs": worst_pairs, "pair_frames": pair_frames,
                "file_clearance": file_clearance,
                "visible_file_clearance": visible_file_clearance,
                "mean_diameter": (observations > 0.0).then(|| diameter_sum / observations),
                "min_diameter": minimum.is_finite().then_some(minimum),
                "mean_extent_use": extent_sum / f64::from(frames + 1),
                "movement": total, "per_commit": commits, "hotspots": hotspots,
                "speed_threshold": FAST_SPEED,
                "file_speed": file_speeds.summary(),
                "directory_speed": directory_speeds.summary(),
                "net_file_directory_displacement": net.measure(),
                "per_commit_net_file_directory_displacement": per_commit_net
            }
        }),
    )?;
    Ok(())
}

fn footprint_distance(offset: Vec2, radii: Vec2) -> f32 {
    use bevy::math::DVec2;

    if radii.x.to_bits() == radii.y.to_bits() {
        return offset.length() - radii.x;
    }

    let mut point = offset.abs().as_dvec2();
    let mut axes = radii.as_dvec2();
    if axes.x < axes.y {
        point = DVec2::new(point.y, point.x);
        axes = DVec2::new(axes.y, axes.x);
    }
    let squared = axes * axes;
    let difference = squared.x - squared.y;
    let inside = (point / axes).length_squared() < 1.0;

    // The closest point lies in the same quadrant. On the major axis it can
    // either be the tip or have a nonzero minor coordinate.
    let closest = if point.y == 0.0 {
        let u = (axes.x * point.x / difference).min(1.0);
        axes * DVec2::new(u, (1.0 - u * u).sqrt())
    } else {
        // Bisect the monotone ellipse constraint on its positive multiplier.
        let product = axes * point;
        let bias = DVec2::new(difference, 0.0);
        let mut low = product.y;
        let mut high = product.length();
        let axis_ratio = product.x / difference;
        if axis_ratio < 1.0 {
            high = high.min(product.y / (1.0 - axis_ratio * axis_ratio).sqrt());
        }
        for _ in 0..36 {
            let middle = low.midpoint(high);
            if (product / (bias + middle)).length_squared() > 1.0 {
                low = middle;
            } else {
                high = middle;
            }
        }
        squared * point / (bias + low.midpoint(high))
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "distance uses world precision"
    )]
    let distance = (closest - point).length() as f32;
    if inside { -distance } else { distance }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn representative_glyph_widths(
        simulation: &Simulation,
        files_per_repo: usize,
    ) -> Vec<f32> {
        use crate::layout::Presence;

        let layout = simulation.layout();
        let (focus, distance) = simulation.view();
        let eye = focus + view_direction() * distance;
        let glyph = simulation.theme().glyph_transform();
        let corners = [
            Vec3::new(-0.5, -0.5, 0.0),
            Vec3::new(0.5, -0.5, 0.0),
            Vec3::new(-0.5, 0.5, 0.0),
            Vec3::new(0.5, 0.5, 0.0),
        ]
        .map(|corner| glyph.transform_point(corner));
        let mut sizes = vec![Vec::new(); layout.repositories.len()];
        for (id, file) in layout
            .files()
            .filter(|(_, file)| file.state == Presence::Present)
        {
            let directory = layout.dir(file.dir);
            let (position, _, alpha) = simulation.file_effect(id, file);
            assert!(alpha >= 1.0, "the glyph has not settled");
            let scale = file.draw_scale(directory);
            let points = corners.map(|corner| position + scale * corner);
            sizes[directory.repository]
                .push(projected_extent(eye, focus, &points).x * 960.0);
        }
        sizes
            .into_iter()
            .map(|mut repo| {
                assert_eq!(repo.len(), files_per_repo);
                repo.sort_unstable_by(f32::total_cmp);
                repo[repo.len() / 2]
            })
            .collect()
    }

    fn glyph_settling_history(
        repository: usize,
        files: usize,
        initial_files: usize,
        steady_edits: bool,
    ) -> Vec<crate::timeline::Event> {
        use crate::timeline::{Event, FileChange, Identity, Kind};

        let event = |day: i64, kind: Kind, range: std::ops::Range<usize>| Event {
            repository,
            author: Identity::new(
                &format!("Author {repository}"),
                &format!("author-{repository}@example.invalid"),
            ),
            co_authors: Vec::new(),
            commit: format!("repo-{repository}-day-{day}"),
            contributor: None,
            co_contributors: Vec::new(),
            seconds: day * 86_400,
            changes: range
                .map(|file| FileChange {
                    path: format!("group-{}/file-{file}.rs", file % 4),
                    kind: kind.clone(),
                    ..Default::default()
                })
                .collect(),
        };
        let mut events = vec![event(0, Kind::Added, 0..initial_files)];
        if initial_files > files {
            events.push(event(8, Kind::Deleted, files..initial_files));
            if steady_edits {
                events.extend((10..20).map(|day| event(day, Kind::Modified, 0..1)));
            }
            events.push(event(20, Kind::Modified, 0..1));
        }
        crate::repositories::namespace(&mut events, repository);
        events
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(16))]

        #[test]
        fn comparable_repositories_settle_to_comparable_file_glyph_sizes(
            files_per_group in 6_usize..=10,
            deleted_from in 0_usize..4,
            multiplier in 2_usize..=3,
            steady_edits in proptest::bool::ANY,
        ) {
            let files = 4 * files_per_group;
            let names: Vec<_> = (0..4).map(|repo| format!("Repository {repo}")).collect();
            let histories = (0..4)
                .map(|repo| {
                    let initial = if repo == deleted_from { files * multiplier } else { files };
                    glyph_settling_history(repo, files, initial, steady_edits)
                })
                .collect();
            let mut simulation = Simulation::with_repositories(
                crate::repositories::merge(histories),
                &crate::model::Options {
                    seconds_per_day: 1.0,
                    max_idle_seconds: 20.0,
                    ..Default::default()
                },
                &crate::theme::Theme::neon(),
                &crate::contributors::Mappings::default(),
                &names,
            );
            let last = simulation.frames(60).unwrap();
            for frame in last - 120..=last {
                simulation.advance_to(frame, 60);
                let widths = representative_glyph_widths(&simulation, files);
                let mean = widths.iter().sum::<f32>() / crate::model::count(widths.len());
                proptest::prop_assert!(widths.iter().all(|&width| {
                    width >= 8.0 && (0.75 * mean..=1.25 * mean).contains(&width)
                }), "file glyph widths at frame {}: {:?}", frame, widths);
            }
        }
    }

    #[test]
    fn clearance_uses_world_distance_to_a_stretched_footprint() {
        let radii = Vec2::new(4.52, 2.26);
        let glyph_radius = 0.27;
        let space = footprint_distance(Vec2::new(4.92, 0.0), radii) - glyph_radius;
        assert!((space - 0.13).abs() < 1e-5);
        let vertical = footprint_distance(Vec2::new(0.0, 4.92), Vec2::new(2.26, 4.52));
        assert!((vertical - 0.4).abs() < 1e-5);
        let near_boundary = footprint_distance(Vec2::new(4.52015, 0.0), radii);
        assert!((near_boundary - 0.00015).abs() < 1e-6);

        let radii = Vec2::new(2.0, 1.0);
        let major = footprint_distance(Vec2::X, radii);
        assert!((major + (2.0_f32 / 3.0).sqrt()).abs() < 1e-5);
        let boundary = footprint_distance(Vec2::new(1.2, 0.8), radii);
        assert!(boundary.abs() < 1e-5);
    }

    #[test]
    fn clearance_detects_cross_tree_transit_and_matches_visible_scaling() {
        let mut layout = crate::layout::Layout::planned_paths(&["a/file", "z/file"], 0);
        layout.add("a/file", 0.0, 0);
        layout.add("z/file", 0.0, 0);
        layout.settle(120, 1.0 / 60.0);
        let clear = FileClearance::measure(&layout, false);
        assert_eq!(clear.foreign_pair_ticks, 0);
        assert_eq!(clear.outside_own_ticks, 0);
        layout.rename("a/file", "z/moved", 0);
        let transit = FileClearance::measure(&layout, false);
        assert!(transit.foreign_pair_ticks > 0);
        assert!(transit.outside_own_ticks > 0);
        let visible = FileClearance::measure(&layout, true);
        assert_eq!(visible.foreign_pair_ticks, 0);
        assert_eq!(visible.outside_own_ticks, 0);
    }

    #[test]
    fn speeds_include_stationary_ids_exclude_births_and_use_strict_threshold() {
        let mut speeds = Speeds::default();
        assert_eq!(speeds.summary()["p99"], 0.0);
        let before = BTreeMap::from([(1, Vec2::ZERO), (2, Vec2::ZERO)]);
        let after = BTreeMap::from([
            (1, Vec2::ZERO),
            (2, Vec2::X * 0.25),
            (3, Vec2::X * 100.0),
        ]);
        speeds.record(&before, &after);
        let summary = speeds.summary();
        assert_eq!(summary["observations"], 2);
        assert_eq!(summary["p95"], 15.0);
        assert_eq!(summary["p99"], 15.0);
        assert_eq!(summary["max"], 15.0);
        assert_eq!(summary["distance_above_threshold"], 0.25);
        assert_eq!(speeds.values[0], 0.0);
    }

    #[test]
    fn overlap_excludes_tangency_and_separation() {
        assert!(overlaps((Vec2::ZERO, 1.0), (Vec2::X, 1.0)));
        assert!(!overlaps((Vec2::ZERO, 1.0), (Vec2::X * 2.0, 1.0)));
        assert!(!overlaps((Vec2::ZERO, 1.0), (Vec2::X * 3.0, 1.0)));
    }

    #[test]
    fn movement_uses_identity_excludes_births_and_counts_known_path() {
        let before = BTreeMap::from([(1, Vec2::ZERO), (2, Vec2::X)]);
        assert_eq!(displacement(&before, &before), (0.0, 0.0));
        let after = BTreeMap::from([(1, Vec2::new(3.0, 4.0)), (3, Vec2::splat(100.0))]);
        assert_eq!(displacement(&before, &after), (5.0, 5.0));
    }

    #[test]
    fn pair_count_counts_each_unordered_pair_once() {
        let discs = [
            (0, Vec2::ZERO, 1.0),
            (1, Vec2::X, 1.0),
            (2, Vec2::X * 5.0, 1.0),
        ];
        assert_eq!(overlap_pairs(&discs).collect::<Vec<_>>(), vec![(0, 1)]);
        assert_eq!(
            overlap_pairs(&[(0, Vec2::ZERO, 1.0), (1, Vec2::X * 2.0, 1.0)]).count(),
            0
        );
        assert_eq!(overlap_pairs(&[]).count(), 0);
    }
    #[test]
    fn net_displacement_cancels_backtracking() {
        let origin = BTreeMap::from([(1, Vec2::ZERO)]);
        let middle = BTreeMap::from([(1, Vec2::new(3.0, 4.0))]);
        let mut vectors = BTreeMap::new();
        add_vectors(&mut vectors, &origin, &middle);
        assert_eq!(vectors[&1].length(), 5.0);
        add_vectors(&mut vectors, &middle, &origin);
        assert_eq!(vectors[&1], Vec2::ZERO);
    }

    #[test]
    fn static_inline_history_has_zero_movement_and_repeats_byte_for_byte() {
        use crate::{contributors::Mappings, model::Options, theme::Theme, timeline};
        let events = vec![timeline::Event::at(
            0,
            vec![timeline::FileChange {
                path: "a.rs".to_owned(),
                kind: timeline::Kind::Added,
                insertions: 1,
                removals: 0,
                samples: Vec::new(),
            }],
        )];
        let mut simulation = Simulation::new(
            events,
            &Options::default(),
            &Theme::neon(),
            &Mappings::default(),
        );
        let mut first = Vec::new();
        run(&mut simulation, &mut first).unwrap();
        simulation.restart();
        let mut second = Vec::new();
        run(&mut simulation, &mut second).unwrap();
        assert_eq!(first, second);
        let text = String::from_utf8(first).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(text.lines().last().unwrap()).unwrap();
        let summary = &record["summary"];
        assert_eq!(summary["overlapped_frames"], 0);
        assert_eq!(summary["movement"]["files"], 0.0);
        assert_eq!(summary["movement"]["directories"], 0.0);
        assert_eq!(
            summary["net_file_directory_displacement"],
            serde_json::json!([0.0, 0.0])
        );
        assert!(summary["mean_diameter"].as_f64().unwrap() > 0.0);
        assert!(summary["min_diameter"].as_f64().unwrap() > 0.0);
    }
    #[test]
    fn projection_has_known_size() {
        let extent =
            projected_extent(Vec3::Z * 10.0, Vec3::ZERO, &[Vec3::NEG_X, Vec3::X]);
        let expected = 2.0 / (10.0 * std::f32::consts::FRAC_PI_8.tan() * 16.0 / 9.0);
        assert!((extent.x - expected).abs() < 1e-6);
        assert_eq!(extent.y, 0.0);
    }
}

#[derive(Default)]
struct NetMovement {
    files: BTreeMap<u64, Vec2>,
    directories: BTreeMap<usize, Vec2>,
}

impl NetMovement {
    fn record(
        &mut self,
        previous_files: &BTreeMap<u64, Vec2>,
        files: &BTreeMap<u64, Vec2>,
        previous_dirs: &BTreeMap<usize, Vec2>,
        dirs: &BTreeMap<usize, Vec2>,
    ) {
        add_vectors(&mut self.files, previous_files, files);
        add_vectors(&mut self.directories, previous_dirs, dirs);
    }
    fn measure(&self) -> (f64, f64) {
        (
            self.files
                .values()
                .map(|delta| f64::from(delta.length()))
                .sum(),
            self.directories
                .values()
                .map(|delta| f64::from(delta.length()))
                .sum(),
        )
    }
}

fn add_vectors<K: Ord + Copy>(
    vectors: &mut BTreeMap<K, Vec2>,
    before: &BTreeMap<K, Vec2>,
    after: &BTreeMap<K, Vec2>,
) {
    for (id, position) in after {
        if let Some(previous) = before.get(id) {
            *vectors.entry(*id).or_default() += *position - *previous;
        }
    }
}

fn count_overlaps(
    layout: &crate::layout::Layout,
    commit: usize,
    hotspots: &mut BTreeMap<String, Hotspot>,
) -> u32 {
    let discs: Vec<_> = layout
        .dirs()
        .map(|(id, dir)| (id, dir.position, dir.radius()))
        .collect();
    let mut pairs = 0;
    for (a, b) in overlap_pairs(&discs) {
        pairs += 1;
        let a = layout.dir(a);
        let b = layout.dir(b);
        let key = serde_json::json!([a.path, b.path]).to_string();
        let hotspot = hotspots.entry(key).or_insert(Hotspot {
            first_commit: commit,
            ..Hotspot::default()
        });
        hotspot.frames += 1;
        hotspot.last_commit = commit;
    }
    pairs
}

fn progress(frame: u32, frames: u32, commit: usize) {
    if frame.is_multiple_of(600) {
        eprintln!("quality frame={frame}/{frames} commit={commit}");
    }
}

fn overlap_pairs(
    discs: &[(usize, Vec2, f32)],
) -> impl Iterator<Item = (usize, usize)> + '_ {
    discs.iter().enumerate().flat_map(move |(index, a)| {
        discs[index + 1..]
            .iter()
            .filter_map(move |b| overlaps((a.1, a.2), (b.1, b.2)).then_some((a.0, b.0)))
    })
}

fn definitions(writer: &mut impl Write) -> anyhow::Result<()> {
    write_record(
        &mut *writer,
        &serde_json::json!({"definitions": DEFINITIONS,
            "file_clearance_definitions": "Raw file_clearance uses lifecycle presence > 0 and radius 0.27 * presence. visible_file_clearance uses the rendered boundary-limited presence, excluding hidden transit. Foreign pair-ticks count each file/live-foreign-directory strict intersection with the directory footprint. Outside-own ticks count files whose disc exceeds their own directory footprint. A stretched footprint is an ellipse. Penetrations <= 0.0001 world units are excluded. Worst depths are world units.",
            "speed_definitions": "Nearest-rank p95/p99/max over retained-ID per-tick speeds, including stationary observations, separately for files and live directories. World units per playback second (tick distance times 60). Birth/reactivation placement excluded. Fast distance sums the full tick distance when speed strictly exceeds 6 world units/s: 0.1 units per 60 Hz tick, approximately 19% of a full-size file's 0.54-unit diameter per tick. Threshold is world-space, independent of camera zoom."}),
    )?;
    Ok(())
}
