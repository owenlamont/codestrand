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
    assert_eq!(sim.layout().repositories.len(), 1);
    assert_eq!(sim.layout().repositories[0].label, "Synthetic history");
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
        {
            let point = project(
                sim.state.focus + view_direction() * sim.state.distance,
                sim.state.focus,
                &[avatar.position],
            )
            .next()
            .flatten()
            .unwrap();
            let radius = sim.state.avatar_radius();
            let centre = Vec2::new((point.x + 1.0) * 640.0, (1.0 - point.y) * 360.0);
            assert!(
                centre.cmpge(Vec2::splat(radius)).all()
                    && centre
                        .cmple(Vec2::new(
                            1280.0 - radius,
                            caption::MULTI_TOP - caption::BUFFER - radius
                        ))
                        .all(),
                "avatar disc clipped at {} for {}",
                sim.seconds(),
                avatar.name
            );
        }
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
        assert!(
            bounds.max.y <= caption::MULTI_TOP - caption::BUFFER,
            "label covers buffered caption: {bounds:?}"
        );
    }
    let beams = sim.beams();
    for fragment in sim.fragments() {
        let (author, target) = fragment.beam.expect("fragment belongs to a pulse");
        let to = sim.state.point(target).unwrap();
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
            avatar_size: std::env::var("CODESTRAND_AVATAR_SCALE")
                .ok()
                .map_or(2.0, |v| v.parse().unwrap()),
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
            travel_cap: std::env::var("CODESTRAND_TRAVEL_CAP")
                .ok()
                .map_or(Options::default().travel_cap, |value| {
                    crate::travel_seconds(&value).unwrap()
                }),
            gutter: std::env::var("CODESTRAND_GUTTER")
                .ok()
                .map_or(4.0, |v| v.parse().unwrap()),
            bot_travel: if std::env::var("CODESTRAND_BOT_TRAVEL").as_deref()
                == Ok("walk")
            {
                BotTravel::Walk
            } else {
                BotTravel::Fade
            },
            ..Default::default()
        },
        &Theme::neon(),
        &crate::contributors::load(
            std::env::var_os("CODESTRAND_CONTRIBUTORS")
                .as_ref()
                .map(std::path::Path::new),
        )
        .unwrap(),
        &repos
            .iter()
            .map(crate::repositories::Repository::name)
            .collect::<Vec<_>>(),
    );
    (sim, repos)
}
