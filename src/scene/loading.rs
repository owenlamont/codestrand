use std::{
    sync::{Arc, Mutex, PoisonError, mpsc},
    thread::JoinHandle,
};

use bevy::{ecs::schedule::ScheduleLabel, prelude::*};

use super::{Playback, Title};
use crate::{camera::PreviewCamera, model::Simulation, progress::Update};

type Planned = anyhow::Result<(Simulation, String)>;

/// Planning still running on a worker thread while the preview window is open.
#[derive(Resource)]
pub(crate) struct Loading {
    updates: Mutex<mpsc::Receiver<Update>>,
    worker: Option<JoinHandle<Planned>>,
}

impl Loading {
    pub(crate) fn new(
        updates: mpsc::Receiver<Update>,
        worker: JoinHandle<Planned>,
    ) -> Self {
        Self {
            updates: Mutex::new(updates),
            worker: Some(worker),
        }
    }
}

/// Runs once, when planning hands over the simulation, to build the scene from it.
#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Loaded;

/// Why planning failed, kept for the caller after the window closes.
#[derive(Resource, Clone, Default)]
pub(super) struct Failure(pub(super) Arc<Mutex<Option<anyhow::Error>>>);

#[derive(Component)]
struct Screen;

#[derive(Component)]
struct Bar;

#[derive(Component)]
struct Label;

#[derive(Component)]
pub(super) struct WaitLabel;

#[derive(Resource)]
pub(super) struct WindowProgress {
    updates: Mutex<mpsc::Receiver<Update>>,
    latest: Option<Update>,
}

pub(super) fn show(mut commands: Commands) {
    commands.spawn((Screen, Camera2d));
    commands
        .spawn((
            Screen,
            Node {
                width: percent(100),
                height: percent(100),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: px(16),
                ..default()
            },
        ))
        .with_children(|screen| {
            screen.spawn((
                Label,
                Text::new("Planning"),
                TextFont::from_font_size(20.0),
                TextColor(Color::WHITE),
            ));
            screen
                .spawn((
                    Node {
                        width: px(480),
                        height: px(10),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.12, 0.14, 0.2)),
                ))
                .with_children(|track| {
                    track.spawn((
                        Bar,
                        Node {
                            width: percent(0),
                            height: percent(100),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.2, 0.8, 1.0)),
                    ));
                });
        });
}

pub(super) fn poll(world: &mut World) {
    let mut loading = world.resource_mut::<Loading>();
    let latest = loading
        .updates
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .try_iter()
        .last();
    let planned = loading
        .worker
        .take_if(|worker| worker.is_finished())
        .map(JoinHandle::join);
    if let Some(update) = latest {
        for mut bar in world
            .query_filtered::<&mut Node, With<Bar>>()
            .iter_mut(world)
        {
            bar.width = percent(update.fraction * 100.0);
        }
        for mut label in world
            .query_filtered::<&mut Text, With<Label>>()
            .iter_mut(world)
        {
            label.0 = update.to_string();
        }
    }
    match planned {
        None => {}
        Some(Ok(Ok((simulation, title)))) => finish(world, simulation, title, latest),
        Some(Ok(Err(error))) => fail(world, error),
        Some(Err(_)) => fail(world, anyhow::anyhow!("planning stopped unexpectedly")),
    }
}

fn finish(
    world: &mut World,
    simulation: Simulation,
    title: String,
    latest: Option<Update>,
) {
    let screen: Vec<_> = world
        .query_filtered::<Entity, With<Screen>>()
        .iter(world)
        .collect();
    for entity in screen {
        world.despawn(entity);
    }
    let loading = world
        .remove_resource::<Loading>()
        .expect("loader completed");
    world.insert_resource(WindowProgress {
        updates: loading.updates,
        latest,
    });
    let indicator = world
        .spawn((
            WaitLabel,
            Text::new("Planning next window"),
            TextFont::from_font_size(16.0),
            TextColor(Color::WHITE),
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.8)),
            Node {
                display: Display::None,
                position_type: PositionType::Absolute,
                left: px(24),
                top: px(44),
                padding: UiRect::axes(px(8), px(4)),
                ..default()
            },
            ZIndex(10),
        ))
        .id();
    world.insert_resource(ClearColor(simulation.theme().clear));
    world.insert_resource(PreviewCamera::new(simulation.view()));
    world.insert_resource(Playback(simulation));
    world.insert_resource(Title(title));
    world.run_schedule(Loaded);
    if let Ok(camera) = world
        .query_filtered::<Entity, With<super::SceneCamera>>()
        .single(world)
    {
        world.entity_mut(indicator).insert(UiTargetCamera(camera));
    }
}

pub(super) fn waiting(
    mut progress: ResMut<WindowProgress>,
    wait: Option<Res<super::PlanningWait>>,
    playback: Option<Res<Playback>>,
    mut labels: Query<(&mut Text, &mut Node), With<WaitLabel>>,
) {
    let latest = progress
        .updates
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .try_iter()
        .last();
    if latest.is_some() {
        progress.latest = latest;
    }
    let waiting = wait.is_some_and(|wait| wait.0);
    let replanning = waiting
        && playback.is_some_and(|playback| playback.0.replanning_after_replay_limit());
    for (mut text, mut node) in &mut labels {
        if waiting {
            node.display = Display::Flex;
            let planning = progress.latest.map_or_else(
                || "Planning next window".to_owned(),
                |update| update.to_string(),
            );
            text.0 = if replanning {
                format!("Replay memory limit reached · {planning}")
            } else {
                planning
            };
        } else {
            node.display = Display::None;
        }
    }
}

pub(super) fn fail(world: &mut World, error: anyhow::Error) {
    world.remove_resource::<Loading>();
    *world
        .get_resource_or_init::<Failure>()
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(error);
    world.write_message(AppExit::error());
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::progress::{Progress, Stage};

    #[derive(Resource, Default)]
    struct Built(u32);

    /// A loading world whose worker returns `planned` once the returned sender fires.
    fn loading_world(planned: Planned) -> (World, Progress, mpsc::Sender<()>) {
        let (progress, updates) = Progress::channel(Duration::ZERO);
        let (release, held) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            held.recv().expect("the test releases planning");
            planned
        });
        let mut world = World::new();
        world.init_resource::<Messages<AppExit>>();
        world.init_resource::<Built>();
        world.insert_resource(Loading::new(updates, worker));
        let mut built = Schedule::new(Loaded);
        built.add_systems(|mut built: ResMut<Built>| built.0 += 1);
        world.add_schedule(built);
        let mut startup = Schedule::default();
        startup.add_systems(show);
        startup.run(&mut world);
        (world, progress, release)
    }

    fn bar(world: &mut World) -> Val {
        world
            .query_filtered::<&Node, With<Bar>>()
            .single(world)
            .expect("one bar")
            .width
    }

    fn poll_until_planned(world: &mut World) {
        for _ in 0..2_000 {
            poll(world);
            if !world.contains_resource::<Loading>() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("planning never arrived");
    }

    #[test]
    fn the_bar_advances_then_gives_way_to_the_scene() {
        let simulation = Simulation::new(
            crate::timeline::synthetic(),
            &crate::model::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        let (mut world, progress, release) =
            loading_world(Ok((simulation, "Fixture".to_owned())));
        poll(&mut world);
        assert_eq!(bar(&mut world), percent(0));
        for (done, expected) in [(1, 25.0), (3, 75.0)] {
            progress.report(Stage::Travel, done, 4);
            poll(&mut world);
            assert_eq!(bar(&mut world), percent(expected));
        }
        let label = world
            .query_filtered::<&Text, With<Label>>()
            .single(&world)
            .unwrap();
        assert_eq!(label.0, "Planning: planning travel 75%");
        assert!(!world.contains_resource::<Playback>());

        release.send(()).unwrap();
        poll_until_planned(&mut world);
        assert_eq!(world.resource::<Title>().0, "Fixture");
        assert!(world.contains_resource::<Playback>());
        assert!(world.contains_resource::<PreviewCamera>());
        assert_eq!(world.resource::<Built>().0, 1);
        assert_eq!(world.query::<&Screen>().iter(&world).count(), 0);
        assert!(world.resource::<Messages<AppExit>>().is_empty());

        let waiting = world.register_system(waiting);
        world.insert_resource(super::super::PlanningWait(true));
        progress.report(Stage::Reservations, 1, 4);
        world.run_system(waiting).unwrap();
        let (text, node) = world
            .query_filtered::<(&Text, &Node), With<WaitLabel>>()
            .single(&world)
            .unwrap();
        assert_eq!(text.0, "Planning: reserving parking 25%");
        assert_eq!(node.display, Display::Flex);
        world.resource_mut::<super::super::PlanningWait>().0 = false;
        world.run_system(waiting).unwrap();
        assert_eq!(
            world
                .query_filtered::<&Node, With<WaitLabel>>()
                .single(&world)
                .unwrap()
                .display,
            Display::None
        );
    }

    #[test]
    fn a_planning_error_closes_the_window_and_is_kept() {
        let (mut world, _progress, release) =
            loading_world(Err(anyhow::anyhow!("no such repository")));
        release.send(()).unwrap();
        poll_until_planned(&mut world);
        assert!(!world.contains_resource::<Playback>());
        assert_eq!(world.resource::<Built>().0, 0);
        let failure = world.resource::<Failure>().0.lock().unwrap().take();
        assert_eq!(failure.unwrap().to_string(), "no such repository");
        let exits = world.resource::<Messages<AppExit>>();
        assert_eq!(
            exits.iter_current_update_messages().collect::<Vec<_>>(),
            [&AppExit::error()]
        );
    }
}
