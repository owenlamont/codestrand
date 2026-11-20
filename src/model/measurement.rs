use std::{fs::File, io::Write, path::Path, time::Instant};

use super::*;

fn overlapping_pairs_per_dir(dirs: &[(Vec2, f32)]) -> f64 {
    let mut pairs = 0_u32;
    for (index, a) in dirs.iter().enumerate() {
        for b in &dirs[index + 1..] {
            if a.0.distance(b.0) < a.1 + b.1 {
                pairs += 1;
            }
        }
    }
    let count = u32::try_from(dirs.len()).expect("directory count fits u32");
    f64::from(pairs) / f64::from(count.max(1))
}

fn compactness(layout: &Layout) -> (f64, f64, f64) {
    let mut low = Vec2::splat(f32::INFINITY);
    let mut high = Vec2::splat(f32::NEG_INFINITY);
    let mut area = 0.0;
    let mut edges = Vec::new();
    for (_, dir) in layout.dirs() {
        let radius = dir.radius();
        low = low.min(dir.position - Vec2::splat(radius));
        high = high.max(dir.position + Vec2::splat(radius));
        area += f64::from(radius).powi(2) * std::f64::consts::PI;
        if let Some(parent) = dir.parent {
            let parent = layout.dir(parent);
            edges.push(f64::from(
                dir.position.distance(parent.position) / (radius + parent.radius()),
            ));
        }
    }
    edges.sort_by(f64::total_cmp);
    let span = high - low;
    let spread = f64::from(span.x) * f64::from(span.y) / area;
    let n = u32::try_from(edges.len()).unwrap();
    let mean = edges.iter().sum::<f64>() / f64::from(n.max(1));
    let p95 = edges
        .get((edges.len() * 95).div_ceil(100).saturating_sub(1))
        .copied()
        .unwrap_or(0.0);
    (spread, mean, p95)
}

#[test]
#[ignore = "requires CODESTRAND_MEASURE_REPOSITORY and CODESTRAND_MEASURE_OUTPUT"]
fn sampled_history_overlap() {
    let repository = std::env::var("CODESTRAND_MEASURE_REPOSITORY").unwrap();
    let output = std::env::var("CODESTRAND_MEASURE_OUTPUT").unwrap();
    let revision = std::env::var("CODESTRAND_MEASURE_REVISION")
        .unwrap_or_else(|_| "HEAD".to_owned());
    let started = Instant::now();
    let events = crate::cache::load_whole(
        Path::new(&repository),
        &revision,
        crate::cache::root().as_deref(),
    )
    .expect("measurement repository imports");
    eprintln!(
        "load_seconds={:.6},events={}",
        started.elapsed().as_secs_f64(),
        events.len()
    );
    let started = Instant::now();
    let mut layout = Layout::planned(&events);
    eprintln!(
        "plan_seconds={:.6},directories={}",
        started.elapsed().as_secs_f64(),
        layout.dir_count()
    );
    if std::env::var_os("CODESTRAND_MEASURE_PLAN_ONLY").is_some() {
        return;
    }
    let mut csv = File::create(output).unwrap();
    writeln!(
        csv,
        "tick,commit,dirs,overlapping_pairs_per_dir,spread,edge_mean,edge_p95"
    )
    .unwrap();
    let total = frames_for(
        Time::seconds(LEAD_IN + count(events.len()) * 0.02 + TAIL),
        60,
    )
    .unwrap();
    let intervals = total.min(300);
    let mut next_event = 0;
    let mut sum = 0.0;
    let mut maximum = 0.0_f64;
    let mut spread_sum = 0.0;
    let mut edge_sum = 0.0;
    let mut p95_sum = 0.0;
    let mut sample = 0;
    for tick in 0..=total {
        if tick > 0 {
            let now = tick_time(u64::from(tick));
            while let Some(event) = events.get(next_event) {
                if Time::seconds(LEAD_IN + 0.02 * count(next_event)) > now {
                    break;
                }
                for change in &event.changes {
                    apply(&mut layout, change, 0);
                }
                next_event += 1;
            }
            layout.step(tick_seconds());
        }
        if tick == total * sample / intervals {
            let discs: Vec<_> = layout
                .dirs()
                .map(|(_, dir)| (dir.position, dir.radius()))
                .collect();
            let value = overlapping_pairs_per_dir(&discs);
            let (spread, edge_mean, edge_p95) = compactness(&layout);
            writeln!(csv, "{tick},{next_event},{},{value:.9},{spread:.9},{edge_mean:.9},{edge_p95:.9}", discs.len()).unwrap();
            sum += value;
            spread_sum += spread;
            edge_sum += edge_mean;
            p95_sum += edge_p95;
            maximum = maximum.max(value);
            if sample % 30 == 0 {
                eprintln!(
                    "sample={sample}/{intervals},commit={next_event}/{}",
                    events.len()
                );
            }
            sample += 1;
        }
    }
    eprintln!(
        "samples={sample},mean={:.9},max={maximum:.9}",
        sum / f64::from(sample)
    );
    eprintln!(
        "spread={:.9},edge_mean={:.9},edge_p95={:.9}",
        spread_sum / f64::from(sample),
        edge_sum / f64::from(sample),
        p95_sum / f64::from(sample)
    );
}

#[test]
fn metric_counts_each_pair_once_and_excludes_tangencies() {
    assert_eq!(overlapping_pairs_per_dir(&[]), 0.0);
    assert_eq!(overlapping_pairs_per_dir(&[(Vec2::ZERO, 1.0); 3]), 1.0);
    assert_eq!(
        overlapping_pairs_per_dir(&[(Vec2::ZERO, 1.0), (Vec2::X * 2.0, 1.0)]),
        0.0
    );
}

#[test]
fn compactness_excludes_dormant_future_directories() {
    let layout = Layout::planned(&crate::timeline::synthetic());
    assert!(layout.dir_count() > 1);
    let (spread, mean, p95) = compactness(&layout);
    assert!((spread - 4.0 / std::f64::consts::PI).abs() < 1e-6);
    assert_eq!((mean, p95), (0.0, 0.0));
}
