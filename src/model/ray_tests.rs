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
            if progress > 0.0 && progress < super::ARRIVAL_FLIGHT {
                file_flights += 1;
                assert!(beam.is_some(), "file {id} has no ray at frame {frame}");
            }
        }
        for (index, dir) in simulation.layout().dirs() {
            let to = super::ground(dir.position)
                + super::Vec3::Y * simulation.repository_effect(dir.repository).2;
            if let Some((beam, _, progress)) = simulation.directory_arrival(index, to)
                && progress > 0.0
                && progress < super::ARRIVAL_FLIGHT
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
            if avatar.presence <= 0.0
                || !super::travel::field_rect(gutter)
                    .inflate(0.1)
                    .contains(super::slots::field_point(avatar.position))
            {
                detached.get_or_insert((frame, beam.author, repository));
            }
            assert_eq!(beam.from, simulation.state.beam_start(beam.author, beam.to));
        }
    }
    assert!(observed > 0);
    assert_eq!(
        crossing, None,
        "(frame, author, target repository, crossed tile)"
    );
    assert_eq!(detached, None, "(frame, author, target repository)");
}
