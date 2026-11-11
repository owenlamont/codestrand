use std::io::Write;

use bevy::prelude::*;

use crate::model::{
    Options, Simulation, frames_for, framing, ground, project, view_direction,
};

#[test]
fn realistic_history_has_stable_framing_and_uniform_icons_at_sixty_hz() {
    let histories = (0..5)
        .map(|i| {
            let mut events = crate::timeline::synthetic();
            for event in &mut events {
                event.seconds += i * 3 * 86_400;
            }
            crate::repositories::namespace(&mut events, usize::try_from(i).unwrap());
            events
        })
        .collect();
    let mut sim = Simulation::with_repositories(
        crate::repositories::merge(histories),
        &Options {
            seconds_per_day: 0.08,
            max_idle_seconds: 0.1,
            ..default()
        },
        &crate::theme::Theme::neon(),
        &crate::contributors::Mappings::default(),
        &(0..5).map(|i| format!("repo-{i}")).collect::<Vec<_>>(),
    );
    let mut previous = [None; 5];
    let mut deltas = [Vec2::ZERO; 5];
    let mut previous_scales = [None; 5];
    for frame in 0..=frames_for(sim.duration(), 60).unwrap() {
        sim.advance_to(frame, 60);
        let (regions, _, scales) = sim.packing.sample(u64::from(frame)).unwrap();
        for (i, region) in regions.iter().enumerate().filter(|(_, r)| r.live) {
            if let Some(before) = previous_scales[i].filter(|_| scales[i] > 0.0) {
                let ratio: f32 = scales[i] / before;
                assert!(
                    ratio.ln().abs() * 60.0 <= 0.1201,
                    "zoom rate at {frame}/{i}"
                );
            }
            previous_scales[i] = (scales[i] > 0.0).then_some(scales[i]);
            let centre = framing::project_ground((region.low + region.high) * 0.5)
                * framing::VIEWPORT
                * 0.5;
            let tile = region.tile.unwrap();
            let usable = tile.usable();
            let anchor = (usable.low + usable.high) * framing::VIEWPORT * 0.25;
            assert!(
                centre.distance(anchor) < 0.002,
                "fit drifts within tile at {frame}/{i}"
            );
            if let Some(before) = previous[i] {
                let delta: Vec2 = centre - before;
                for axis in 0..2 {
                    assert!(
                        !(delta[axis] * deltas[i][axis] < 0.0
                            && delta[axis].abs() > 2.0
                            && deltas[i][axis].abs() > 2.0),
                        "framing reverses at {frame}/{i}: {:?} -> {delta:?}",
                        deltas[i]
                    );
                }
                deltas[i] = delta;
            }
            previous[i] = Some(centre);
        }
        if frame.is_multiple_of(6) {
            assert_icons(&sim);
        }
    }
}

fn assert_icons(sim: &Simulation) {
    let (focus, distance) = sim.view();
    let mut camera = crate::camera::PreviewCamera::new(sim.view());
    camera.update(
        &crate::camera::Input {
            orbit: Vec2::new(0.0, 10_000.0),
            ..default()
        },
        sim.view(),
        &[],
        0.1,
    );
    let (eye, target) = camera.pose();
    assert!(
        ((eye - target).normalize().y.asin() - view_direction().y.asin()).abs() < 1e-6
    );
    let rotation = Transform::from_translation(focus + view_direction() * distance)
        .looking_at(focus, Vec3::Y)
        .rotation;
    for (_, file) in sim.layout().files() {
        let dir = sim.layout().dir(file.dir);
        let scale = file.visible_presence(dir);
        if scale <= 0.001 {
            continue;
        }
        let turned = sim
            .layout()
            .repositories
            .get(dir.repository)
            .is_some_and(|r| r.rotated);
        let parent = Transform::from_translation(ground(file.position))
            .with_scale(Vec3::splat(scale))
            .with_rotation(Quat::from_rotation_y(if turned {
                -std::f32::consts::FRAC_PI_2
            } else {
                0.0
            }));
        let mut glyph = sim.theme().file_glyph_transform;
        glyph.rotation = crate::scene::glyph_rotation(parent.rotation, rotation);
        let matrix = parent.to_matrix() * glyph.to_matrix();
        let points = [Vec3::NEG_X, Vec3::X, Vec3::NEG_Y, Vec3::Y]
            .map(|p| matrix.transform_point3(p * 0.5));
        let projected: Vec<_> = project(eye, target, &points)
            .flatten()
            .map(|p| p * framing::VIEWPORT * 0.5)
            .collect();
        assert_eq!(projected.len(), 4);
        let aspect =
            projected[0].distance(projected[1]) / projected[2].distance(projected[3]);
        let intended = glyph.scale.x / glyph.scale.y;
        assert!(
            (aspect / intended - 1.0).abs() < 0.01,
            "icon aspect {aspect} at {}",
            file.path
        );
    }
}

#[test]
fn single_repository_icons_keep_the_same_pitch_and_aspect() {
    let mut sim = Simulation::new(
        crate::timeline::synthetic(),
        &Options::default(),
        &crate::theme::Theme::neon(),
        &crate::contributors::Mappings::default(),
    );
    for frame in (0..=frames_for(sim.duration(), 60).unwrap()).step_by(6) {
        sim.advance_to(frame, 60);
        assert_icons(&sim);
    }
}

#[test]
#[ignore = "requires local repository manifest and diagnostic output path"]
fn local_framing_trace() {
    let manifest = std::env::var("CODESTRAND_SLOT_REPOSITORIES").unwrap();
    let repos = crate::repositories::inputs(&[], Some(std::path::Path::new(&manifest)))
        .unwrap();
    let histories = repos
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut events =
                crate::cache::load(&r.path, "HEAD", crate::cache::root().as_deref())
                    .unwrap();
            crate::repositories::namespace(&mut events, i);
            events
        })
        .collect();
    let theme = crate::theme::Theme::neon();
    let mut sim = Simulation::with_repositories(
        crate::repositories::merge(histories),
        &Options {
            seconds_per_day: 1.0,
            max_idle_seconds: 0.1,
            ..default()
        },
        &theme,
        &crate::contributors::Mappings::default(),
        &repos
            .iter()
            .map(crate::repositories::Repository::name)
            .collect::<Vec<_>>(),
    );
    let mut writer = std::io::BufWriter::new(
        std::fs::File::create(std::env::var("CODESTRAND_FRAMING_TRACE").unwrap())
            .unwrap(),
    );
    for frame in 0..=frames_for(sim.duration(), 60).unwrap() {
        sim.advance_to(frame, 60);
        let (regions, origins, scales) = sim.packing.sample(u64::from(frame)).unwrap();
        let (focus, distance) = sim.view();
        let eye = focus + view_direction() * distance;
        let rows: Vec<_> = regions.iter().enumerate().filter(|(_, r)| r.live).map(|(i, r)| {
            let origin = framing::project_ground(origins[i]) * framing::VIEWPORT * 0.5;
            let centre = framing::project_ground((r.low + r.high) * 0.5) * framing::VIEWPORT * 0.5;
            let mut aspects = Vec::new();
            let mut incidence = Vec::new();
            for (_, file) in sim.layout().files().filter(|(_, f)| sim.layout().dir(f.dir).repository == i) {
                let scale = file.visible_presence(sim.layout().dir(file.dir));
                if scale <= 0.001 { continue; }
                let parent = Transform::from_translation(ground(file.position)).with_rotation(Quat::from_rotation_y(if r.rotated {-std::f32::consts::FRAC_PI_2} else {0.0})).with_scale(Vec3::splat(scale));
                let camera = Transform::from_translation(eye).looking_at(focus, Vec3::Y);
                let mut glyph = theme.file_glyph_transform;
                glyph.rotation = crate::scene::glyph_rotation(parent.rotation, camera.rotation);
                let matrix = parent.to_matrix() * glyph.to_matrix();
                let points = [Vec3::NEG_X * 0.5, Vec3::X * 0.5, Vec3::NEG_Y * 0.5, Vec3::Y * 0.5].map(|p| matrix.transform_point3(p));
                let projected: Vec<_> = project(eye, focus, &points).flatten().map(|p| p * framing::VIEWPORT * 0.5).collect();
                if projected.len() == 4 { aspects.push(projected[0].distance(projected[1]) / projected[2].distance(projected[3])); }
                incidence.push((eye - ground(file.position)).normalize().y.asin().to_degrees());
            }
            let tile = r.tile.unwrap();
            let anchor = (tile.low + tile.high) * framing::VIEWPORT * 0.25;
            serde_json::json!({"tile":r.label,"origin":origin.to_array(),"centre":centre.to_array(),"tile_low":tile.low.to_array(),"tile_high":tile.high.to_array(),"anchor_error":(centre-anchor).to_array(),"scale":scales[i],"rotated":r.rotated,"pitch":view_direction().y.asin().to_degrees(),"incidence":incidence,"aspects":aspects,"intended_aspect":theme.file_glyph_transform.scale.x/theme.file_glyph_transform.scale.y})
        }).collect();
        serde_json::to_writer(&mut writer, &serde_json::json!({"frame":frame,"camera":[focus.to_array(),distance],"tiles":rows})).unwrap();
        writeln!(writer).unwrap();
    }
}
