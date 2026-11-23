use bevy::math::Vec2;

use super::{Beam, BotTravel, Mappings, Options, Simulation, Target, Theme, TravelCap};
use crate::timeline::{FileChange, Identity, synthetic};

fn rapid_repositories(count: usize) -> Simulation {
    let seed = 0..count;
    let rapid = (0..count).step_by(2).chain((0..count).rev().step_by(2));
    let event = synthetic().remove(0);
    let events = seed
        .map(|repo| (repo, true))
        .chain(rapid.map(|repo| (repo, false)))
        .enumerate()
        .map(|(index, (repo, seed))| {
            let mut event = event.clone();
            event.commit = format!("commit-{index}");
            event.seconds = i64::try_from(index).unwrap() * 86_400;
            event.author = if seed {
                Identity::new(
                    &format!("Seed {repo}"),
                    &format!("seed{repo}@test.invalid"),
                )
            } else {
                Identity::new("Rapid", "rapid@test.invalid")
            };
            let bot = Identity::new("GitHub Actions[bot]", "actions[bot]@test.invalid");
            if !seed && repo % 3 == 0 {
                event.co_authors = vec![event.author];
                event.author = bot;
            } else if !seed && repo % 3 == 1 {
                event.co_authors = vec![bot];
            }
            let phase = if seed { "seed" } else { "rapid" };
            event.changes = vec![FileChange {
                path: format!("{phase}/file.rs"),
                ..FileChange::default()
            }];
            let mut row = [event];
            crate::repositories::namespace(&mut row, repo);
            let [event] = row;
            event
        })
        .collect();
    let labels: Vec<_> = (0..count).map(|repo| format!("repo-{repo}")).collect();
    let options = Options {
        seconds_per_day: 0.02,
        max_idle_seconds: 0.05,
        travel_cap: TravelCap::Seconds(0.12),
        bot_travel: BotTravel::Fade,
        fragment_speed: 0.25,
        ..Options::default()
    };
    Simulation::with_repositories(
        events,
        &options,
        &Theme::neon(),
        &Mappings::default(),
        &labels,
    )
}

fn target_repository(simulation: &Simulation, beam: Beam) -> usize {
    let layout = simulation.layout();
    let dir = match beam.target {
        Target::File(id) => layout.file(id).unwrap().dir,
        Target::Dir(dir) => dir,
    };
    layout.dir(dir).repository
}

fn capped_motion_history(rows: &[(usize, &str, Option<&str>)]) -> Simulation {
    let template = synthetic().remove(0);
    let identity = |name: &str| {
        Identity::new(name, &format!("{}@test.invalid", name.to_ascii_lowercase()))
    };
    let events = rows
        .iter()
        .enumerate()
        .flat_map(|(index, &(repository, author, co_author))| {
            let mut event = template.clone();
            event.repository = repository;
            event.commit = format!("event-{index}");
            event.seconds = i64::try_from(index).unwrap() * 86_400;
            event.author = identity(author);
            if let Some(co_author) = co_author {
                event.co_authors.push(identity(co_author));
            }
            event.changes = vec![FileChange {
                path: format!("content/f{index}.rs"),
                ..FileChange::default()
            }];
            let mut row = [event];
            crate::repositories::namespace(&mut row, repository);
            row
        })
        .collect();
    let options = Options {
        seconds_per_day: 0.35,
        max_idle_seconds: 0.35,
        travel_cap: TravelCap::Seconds(0.45),
        fragment_speed: 0.35,
        ..Options::default()
    };
    Simulation::with_repositories(
        events,
        &options,
        &Theme::neon(),
        &Mappings::default(),
        &["one".to_owned(), "two".to_owned(), "three".to_owned()],
    )
}

#[test]
fn a_visible_contributor_never_steps_above_travel_speed() {
    let mut simulation = capped_motion_history(&[
        (0, "Ada", Some("Choi")),
        (1, "Ben", Some("Dara")),
        (2, "Choi", Some("Ada")),
        (0, "Ada", None),
        (2, "Choi", Some("Dara")),
        (0, "Ben", None),
        (1, "Ben", None),
        (1, "Ben", None),
        (2, "Dara", Some("Ada")),
    ]);
    let maximum = simulation.state.avatar_max_speed
        * super::framing::WORLD_UNITS_PER_REFERENCE_PIXEL
        / 60.0;
    let mut previous = Vec::<(super::Vec3, f32)>::new();
    for frame in 0..=simulation.frames(60).unwrap() {
        simulation.advance_to(frame, 60);
        for (avatar, &(position, presence)) in
            simulation.avatars().iter().zip(&previous)
        {
            if presence > 0.001 && avatar.presence > 0.001 {
                assert!(
                    position.distance(avatar.position) <= maximum + 0.0005,
                    "{} exceeded the travel speed at frame {frame}",
                    avatar.name,
                );
            }
        }
        previous = simulation
            .avatars()
            .iter()
            .map(|avatar| (avatar.position, avatar.presence))
            .collect();
    }
}

#[test]
fn a_capped_walk_avoids_a_parked_contributor() {
    let mut simulation = capped_motion_history(&[
        (0, "Ada", None),
        (1, "Ben", None),
        (2, "Choi", None),
        (0, "Ben", None),
        (2, "Dara", Some("Choi")),
        (2, "Choi", None),
        (1, "Ada", Some("Dara")),
        (2, "Dara", None),
        (0, "Ada", None),
    ]);
    let mut audit = super::occlusion::Audit::default();
    for frame in 0..=simulation.frames(60).unwrap() {
        simulation.advance_to(frame, 60);
        audit.observe(&simulation);
        assert_eq!(audit.parked_avatar_observations, 0, "frame {frame}");
    }
}

#[test]
fn arrival_rays_end_when_a_contributor_transfers() {
    let mut simulation = rapid_repositories(4);
    let mut arrivals = 0;
    for frame in 0..=simulation.frames(60).unwrap() {
        simulation.advance_to(frame, 60);
        let files = simulation.file_arrival_beams().map(|(_, beam)| beam);
        let directories = crate::scene::directory_rays_for_test(&simulation);
        for beam in files.chain(directories) {
            arrivals += 1;
            let avatar = &simulation.avatars()[beam.author];
            assert_eq!(
                avatar.repository,
                target_repository(&simulation, beam),
                "frame {frame}, {} still emits an arrival ray to the previous repository",
                avatar.name,
            );
        }
    }
    assert!(arrivals > 0);
}

#[test]
fn arrivals_keep_their_ray_during_contributor_transfers() {
    let mut simulation = rapid_repositories(4);
    let mut file_flights = 0;
    let mut directory_flights = 0;
    for frame in 0..=simulation.frames(60).unwrap() {
        simulation.advance_to(frame, 60);
        for (id, file) in simulation.layout().arriving_files() {
            let dir = simulation.layout().dir(file.dir);
            let to = super::ground(file.position)
                + super::Vec3::Y * simulation.repository_effect(dir.repository).2;
            let (beam, progress) = simulation.arrival_beam(
                Target::File(id),
                file.arrival_author,
                to,
                file.presence,
                file.flight,
                file.arrival_revision,
            );
            let flight_end = file.flight.map_or(1.0, |_| super::ARRIVAL_FLIGHT);
            if progress > 0.0 && progress < flight_end {
                file_flights += 1;
                assert!(beam.is_some(), "file {id} has no ray at frame {frame}");
            }
        }
        for (index, dir) in simulation.layout().dirs() {
            let to = super::ground(dir.position)
                + super::Vec3::Y * simulation.repository_effect(dir.repository).2;
            let flight_end = dir
                .arrival
                .and_then(|arrival| arrival.flight)
                .map_or(1.0, |_| super::ARRIVAL_FLIGHT);
            if let Some((beam, _, progress)) = simulation.directory_arrival(index, to)
                && progress > 0.0
                && progress < flight_end
            {
                directory_flights += 1;
                assert!(
                    beam.is_some(),
                    "directory {index} has no ray at frame {frame}"
                );
            }
        }
    }
    assert!(file_flights > 0);
    assert!(directory_flights > 0);
}

#[test]
fn a_new_directory_waits_for_a_contributor_fading_in() {
    let template = synthetic().remove(0);
    let mover = Identity::new("Relay[bot]", "relay@test.invalid");
    let seed = Identity::new("Seed", "seed@test.invalid");
    let mut events = Vec::new();
    for (index, (repository, author, path, kind)) in [
        (0, mover.clone(), "source.rs", crate::timeline::Kind::Added),
        (1, seed, "original.rs", crate::timeline::Kind::Added),
        (
            1,
            mover,
            "new/renamed.rs",
            crate::timeline::Kind::Moved {
                from: "original.rs".to_owned(),
            },
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut event = template.clone();
        event.commit = format!("commit-{index}");
        event.seconds = i64::try_from(index).unwrap() * 86_400;
        event.author = author;
        event.changes = vec![FileChange {
            path: path.to_owned(),
            kind,
            ..FileChange::default()
        }];
        let mut row = [event];
        crate::repositories::namespace(&mut row, repository);
        events.extend(row);
    }
    let options = Options {
        seconds_per_day: 0.3,
        max_idle_seconds: 0.3,
        travel_cap: TravelCap::Seconds(0.12),
        bot_travel: BotTravel::Fade,
        ..Options::default()
    };
    let mut simulation = Simulation::with_repositories(
        events,
        &options,
        &Theme::neon(),
        &Mappings::default(),
        &["origin".to_owned(), "destination".to_owned()],
    );

    let mut observed_flight = false;
    for frame in 0..=simulation.frames(60).unwrap() {
        simulation.advance_to(frame, 60);
        for (index, directory) in simulation.layout().dirs() {
            if !directory.path.ends_with("/new") {
                continue;
            }
            if let Some((beam, _, progress)) =
                simulation.directory_arrival(index, super::ground(directory.position))
                && progress > 0.0
                && progress < 1.0
            {
                observed_flight = true;
                assert!(beam.is_some(), "directory has no ray at frame {frame}");
            }
        }
    }
    assert!(observed_flight);
}

#[test]
fn commit_pulse_waits_for_the_displayed_contributor_to_reach_the_gutter() {
    let mut simulation = Simulation::new(
        vec![synthetic().remove(0)],
        &Options::default(),
        &Theme::neon(),
        &Mappings::default(),
    );
    simulation.advance_to(60, 60);
    assert!(!simulation.beams().is_empty());
    let gutter = simulation.layout().repositories[0].gutter.unwrap();
    let rect = super::travel::field_rect(gutter);
    simulation.state.avatars[0].position =
        super::slots::world(rect.max + Vec2::splat(50.0));
    assert!(simulation.beams().is_empty());
}

#[test]
fn visible_rays_stay_local_over_whole_playback() {
    let mut simulation = rapid_repositories(48);
    let mut observed = 0;
    let mut crossing = None;
    let mut detached = None;
    for frame in 0..=simulation.frames(60).unwrap() {
        simulation.advance_to(frame, 60);
        let arrivals = simulation.file_arrival_beams().map(|(_, beam)| beam);
        let directories = crate::scene::directory_rays_for_test(&simulation);
        for beam in simulation
            .beams()
            .into_iter()
            .chain(arrivals)
            .chain(directories)
        {
            observed += 1;
            let repository = target_repository(&simulation, beam);
            let avatar = &simulation.avatars()[beam.author];
            let from = super::slots::field_point(beam.from);
            let to = super::slots::field_point(beam.to);
            let crossed = simulation
                .layout()
                .repositories
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != repository)
                .find_map(|(other, region)| {
                    let interior =
                        super::travel::field_rect(region.tile?).inflate(-0.01);
                    (interior.size().cmpgt(Vec2::ZERO).all()
                        && super::slots::segment(from, to, interior))
                    .then_some(other)
                });
            if let Some(other) = crossed {
                crossing.get_or_insert((frame, beam.author, repository, other));
            }
            let gutter = simulation.layout().repositories[repository].gutter.unwrap();
            if avatar.repository != repository
                || avatar.presence <= 0.0
                || !super::travel::field_rect(gutter)
                    .inflate(simulation.state.avatar_radius())
                    .contains(super::slots::field_point(avatar.position))
            {
                detached.get_or_insert((frame, beam.author, repository));
            }
            let (centre, radius) =
                simulation.state.beam_ring(beam.author).unwrap_or_else(|| {
                    let radius = simulation.state.geometry.avatar_radius
                        * simulation.scale()
                        * simulation.avatar_disc_scale()
                        * avatar.projection_scale
                        * avatar.lifecycle_scale
                        * avatar.presence;
                    (avatar.position, radius)
                });
            assert!(
                beam.from == simulation.state.beam_start(beam.author, beam.to)
                    || (beam.from.distance(centre) - radius).abs() < 0.001
            );
        }
    }
    assert!(observed > 0);
    assert_eq!(
        crossing, None,
        "(frame, author, target repository, crossed tile)"
    );
    assert_eq!(detached, None, "(frame, author, target repository)");
}

#[test]
fn contributors_hold_position_and_presence_while_transmitting() {
    let mut simulation = rapid_repositories(4);
    let mut previous = Vec::new();
    let mut observed = 0;
    for frame in 0..=simulation.frames(60).unwrap() {
        simulation.advance_to(frame, 60);
        let files = simulation.file_arrival_beams().map(|(_, beam)| beam);
        let directories = crate::scene::directory_rays_for_test(&simulation);
        for beam in simulation
            .beams()
            .into_iter()
            .chain(files)
            .chain(directories)
        {
            let avatar = &simulation.avatars()[beam.author];
            let Some(&(position, presence)) = previous.get(beam.author) else {
                continue;
            };
            assert_eq!(
                avatar.position, position,
                "frame {frame}: {} moved",
                avatar.name
            );
            assert!(
                avatar.presence >= presence,
                "frame {frame}: {} faded",
                avatar.name
            );
            observed += 1;
        }
        previous.clear();
        previous.extend(
            simulation
                .avatars()
                .iter()
                .map(|a| (a.position, a.presence)),
        );
    }
    assert!(observed > 0);
}
