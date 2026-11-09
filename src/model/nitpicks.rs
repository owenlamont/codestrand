use super::*;

#[test]
fn beam_finishes_the_last_emitted_flight_before_fading() {
    let mut events = crate::timeline::synthetic();
    events.truncate(1);
    events[0].changes.truncate(1);
    events[0].changes[0].insertions = 7;
    let mut sim = Simulation::new(
        events,
        &Options::default(),
        &Theme::neon(),
        &Mappings::default(),
    );
    let mut late_flight = false;
    for frame in 31..180 {
        sim.advance_to(frame, 60);
        assert_labels_and_beams(&sim);
        let flying = sim.fragments().iter().any(|f| !f.rising);
        if flying && sim.seconds() > LEAD_IN + PULSE_SECONDS {
            late_flight = true;
            assert!(sim.beams().iter().all(|b| b.width >= 0.999));
        }
    }
    assert!(
        late_flight,
        "the final flight must extend beyond the old beam lifetime"
    );
    assert!(sim.fragments().is_empty());
    assert!(sim.beams().is_empty());
}

pub(super) fn assert_labels_and_beams(sim: &Simulation) {
    for avatar in sim.avatars().iter().filter(|a| a.presence > 0.0) {
        let label = sim.state.avatar_label(avatar);
        assert!(
            label.opacity > 0.0 && !label.text.is_empty(),
            "missing label at {} for {}",
            sim.seconds(),
            avatar.name
        );
        let point = project(
            sim.state.focus + view_direction() * sim.state.distance,
            sim.state.focus,
            &[label.position],
        )
        .next()
        .flatten()
        .unwrap();
        let point = Vec2::new((point.x + 1.0) * 640.0, (1.0 - point.y) * 360.0);
        let bounds = names::rect(
            point,
            &label.text,
            label.scale,
            sim.state.geometry.label_height,
        );
        assert!(
            bounds.min.cmpge(Vec2::splat(0.25)).all()
                && bounds.max.cmple(Vec2::new(1279.75, 719.75)).all(),
            "label would be clipped at {} for {}: {bounds:?}",
            sim.seconds(),
            avatar.name
        );
    }
    let beams = sim.beams();
    for fragment in sim.fragments() {
        let (author, target) = fragment.beam.expect("fragment belongs to a pulse");
        let to = sim.state.point(target).unwrap() + Vec3::Y * 0.12;
        assert!(
            beams.iter().any(|b| b.author == author && b.to == to),
            "orphan fragment at {}",
            sim.seconds()
        );
    }
}

#[test]
fn fragments_and_visible_names_survive_switches_seeks_and_rewind() {
    let histories = (0..3)
        .map(|i| {
            let mut events = crate::timeline::synthetic();
            for (j, e) in events.iter_mut().enumerate() {
                e.seconds = i * 86_400 + i64::try_from(j).unwrap() * 4 * 86_400;
            }
            crate::repositories::namespace(&mut events, usize::try_from(i).unwrap());
            events
        })
        .collect();
    let mut sim = Simulation::with_repositories(
        crate::repositories::merge(histories),
        &Options {
            seconds_per_day: 0.2,
            max_idle_seconds: 0.1,
            ..Default::default()
        },
        &Theme::neon(),
        &Mappings::default(),
        &["a".into(), "b".into(), "c".into()],
    );
    for frame in 0..=frames_for(sim.duration(), 60).unwrap() {
        sim.advance_to(frame, 60);
        assert_labels_and_beams(&sim);
    }
    for frame in [300, 60, 180, 0, 120] {
        sim.advance_to(frame, 60);
        assert_labels_and_beams(&sim);
    }
}

pub(super) fn local_simulation() -> (Simulation, Vec<crate::repositories::Repository>) {
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
    let sim = Simulation::with_repositories(
        crate::repositories::merge(histories),
        &Options {
            seconds_per_day: 1.0,
            max_idle_seconds: 0.1,
            ..Default::default()
        },
        &Theme::neon(),
        &Mappings::default(),
        &repos
            .iter()
            .map(crate::repositories::Repository::name)
            .collect::<Vec<_>>(),
    );
    (sim, repos)
}

#[test]
#[ignore = "requires local repository manifest and diagnostic output path"]
fn local_nitpick_measurements() {
    let (mut sim, repos) = local_simulation();
    let mut previous = vec![None::<f32>; repos.len()];
    let mut rates = vec![0.0_f32; repos.len()];
    let mut rate_frames = vec![0; repos.len()];
    let mut labels = [0_u64; 5];
    let mut orphans = Vec::new();
    let mut missing_fragments = 0;
    for frame in 0..=frames_for(sim.duration(), 60).unwrap() {
        sim.advance_to(frame, 60);
        let (_, _, scales) = sim.packing.sample(u64::from(frame)).unwrap();
        for (i, scale) in scales.into_iter().enumerate() {
            if scale > 0.0
                && sim.events[..sim.state.next_event]
                    .iter()
                    .any(|e| e.repository == i)
            {
                if let Some(before) = previous[i] {
                    let rate = (scale / before).ln().abs() * 60.0;
                    rate_frames[i] =
                        [rate_frames[i], frame][usize::from(rate > rates[i])];
                    rates[i] = rates[i].max(rate);
                }
                previous[i] = Some(scale);
            } else {
                previous[i] = None;
            }
        }
        for avatar in sim.avatars().iter().filter(|a| a.presence > 0.0) {
            labels[0] += 1;
            let label = sim.state.avatar_label(avatar);
            labels[1] += u64::from(label.opacity <= 0.0 || label.text.is_empty());
            labels[2] += u64::from(label.scale < sim.state.name_scale() - 0.001);
            labels[3] += u64::from(label.text != avatar.name);
            let points: Vec<_> = project(
                sim.state.focus + view_direction() * sim.state.distance,
                sim.state.focus,
                &[avatar.position, label.position],
            )
            .flatten()
            .collect();
            if points.len() == 2 {
                let delta = (points[1] - points[0]) * Vec2::new(640.0, 360.0);
                let ideal = Vec2::new(
                    12.0,
                    (sim.state.geometry.label_height * label.scale).midpoint(4.0),
                );
                labels[4] += u64::from(delta.distance(ideal) > 0.1);
            }
        }
        let beams = sim.beams();
        let missing = sim
            .fragments()
            .iter()
            .filter(|f| !f.rising && f.alpha > 0.00001)
            .filter(|f| {
                !beams.iter().any(|b| {
                    let line = b.to - b.from;
                    let phase = (f.position - b.from).dot(line)
                        / line.length_squared().max(1e-9);
                    f.position.distance(b.from + line * phase.clamp(0.0, 1.0)) < 0.081
                })
            })
            .count();
        missing_fragments += missing;
        if missing > 0 {
            orphans.push(frame);
        }
    }
    let result = serde_json::json!({"tiles":repos.iter().enumerate().map(|(i,r)| serde_json::json!({"name":r.name(),"max_log_zoom_per_second":rates[i],"frame":rate_frames[i]})).collect::<Vec<_>>(),"label_avatar_frames":labels,"orphan_frames":orphans,"orphan_fragment_frames":missing_fragments});
    std::fs::write(
        std::env::var("CODESTRAND_NITPICKS").unwrap(),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
}
