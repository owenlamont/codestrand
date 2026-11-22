use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use bevy::{
    ecs::schedule::ScheduleLabel,
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems, render_resource::PipelineCache,
        view::ViewTarget,
    },
};

use super::{Playback, Title};
use crate::{camera::PreviewCamera, model::Simulation, progress::Update};

type Planned = anyhow::Result<(Simulation, String)>;

const SLOW_START: Duration = Duration::from_secs(60);
const TRACK_WIDTH: f32 = 480.0;
const INDICATOR_WIDTH: f32 = 96.0;

#[derive(Resource)]
pub(super) struct Preparing {
    started: Instant,
    scene: Option<(Simulation, String)>,
}

#[derive(Clone, Default, Resource)]
struct RendererReady(Arc<AtomicBool>);

pub(super) fn track_renderer(app: &mut App) {
    let ready = RendererReady::default();
    app.insert_resource(ready.clone())
        .sub_app_mut(RenderApp)
        .insert_resource(ready)
        .add_systems(Render, check_renderer.after(RenderSystems::Render));
}

fn check_renderer(
    ready: Res<RendererReady>,
    pipelines: Res<PipelineCache>,
    cameras: Query<(), (With<Camera3d>, With<ViewTarget>)>,
) {
    let ready = &ready.into_inner().0;
    if !ready.load(Ordering::Relaxed)
        && !cameras.is_empty()
        && pipelines.into_inner().waiting_pipelines().next().is_none()
    {
        ready.store(true, Ordering::Relaxed);
    }
}

#[derive(PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) name: String,
    pub(crate) start_date: Option<String>,
    pub(crate) replace_start_date: bool,
}

impl Selection {
    fn warning(&self) -> String {
        let name = &self.name;
        let status = format!("{name} preview is still preparing after a minute.");
        match &self.start_date {
            Some(date) if self.replace_start_date => format!(
                "{status} Keep waiting, or close preview and replace your start date with `--start-date {date}` to view a shorter range."
            ),
            Some(date) => format!(
                "{status} Keep waiting, or close preview and retry with `--start-date {date}` to view a shorter range."
            ),
            None => status,
        }
    }
}

/// Planning still running on a worker thread while the preview window is open.
#[derive(Resource)]
pub(crate) struct Loading {
    updates: Mutex<mpsc::Receiver<Update>>,
    selections: Mutex<mpsc::Receiver<Selection>>,
    worker: Option<JoinHandle<Planned>>,
    launched: Instant,
    selection: Selection,
    warned: bool,
}

impl Loading {
    pub(crate) fn new(
        updates: mpsc::Receiver<Update>,
        selections: mpsc::Receiver<Selection>,
        worker: JoinHandle<Planned>,
        launched: Instant,
    ) -> Self {
        Self {
            updates: Mutex::new(updates),
            selections: Mutex::new(selections),
            worker: Some(worker),
            launched,
            selection: Selection {
                name: "This history".to_owned(),
                start_date: None,
                replace_start_date: false,
            },
            warned: false,
        }
    }

    fn warning_at(&mut self, now: Instant) -> Option<String> {
        let latest = self
            .selections
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .try_iter()
            .last();
        let changed = latest.is_some_and(|selection| {
            let changed = self.selection != selection;
            self.selection = selection;
            changed
        });
        if now.duration_since(self.launched) < SLOW_START {
            return None;
        }
        (!std::mem::replace(&mut self.warned, true) || changed)
            .then(|| self.selection.warning())
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
struct Warning;

#[derive(Component)]
pub(super) struct WaitLabel;

#[derive(Resource)]
pub(super) struct WindowProgress {
    updates: Mutex<mpsc::Receiver<Update>>,
    latest: Option<Update>,
}

pub(super) fn show(mut commands: Commands) {
    let camera = commands
        .spawn((
            Screen,
            Camera2d,
            Camera {
                order: 2,
                ..default()
            },
        ))
        .id();
    commands
        .spawn((
            Screen,
            UiTargetCamera(camera),
            BackgroundColor(Color::srgb_u8(43, 44, 47)),
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
                        width: px(TRACK_WIDTH),
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
            screen.spawn((
                Warning,
                Text::default(),
                TextFont::from_font_size(18.0),
                TextColor(Color::WHITE),
                TextLayout::justify(Justify::Center),
                Node {
                    display: Display::None,
                    max_width: percent(90),
                    ..default()
                },
            ));
        });
}

pub(super) fn poll(world: &mut World) {
    poll_at(world, Instant::now());
}

fn poll_at(world: &mut World, now: Instant) {
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
    let warning = planned.is_none().then(|| loading.warning_at(now)).flatten();
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
            label.0 = format!("{update:.1}");
        }
    }
    if let Some(warning) = warning {
        for (mut text, mut node) in world
            .query_filtered::<(&mut Text, &mut Node), With<Warning>>()
            .iter_mut(world)
        {
            text.0.clone_from(&warning);
            node.display = Display::Flex;
        }
    }
    match planned {
        None => {}
        Some(Ok(Ok(scene))) => finish(world, scene, latest, now),
        Some(Ok(Err(error))) => fail(world, error),
        Some(Err(_)) => fail(world, anyhow::anyhow!("planning stopped unexpectedly")),
    }
}

fn finish(
    world: &mut World,
    scene: (Simulation, String),
    latest: Option<Update>,
    now: Instant,
) {
    for mut text in world
        .query_filtered::<&mut Text, With<Label>>()
        .iter_mut(world)
    {
        "Preparing renderer".clone_into(&mut text.0);
    }
    for mut node in world
        .query_filtered::<&mut Node, With<Warning>>()
        .iter_mut(world)
    {
        node.display = Display::None;
    }
    move_indicator(world, Duration::ZERO);
    world.insert_resource(Preparing {
        started: now,
        scene: Some(scene),
    });
    let loading = world
        .remove_resource::<Loading>()
        .expect("loader completed");
    world.insert_resource(WindowProgress {
        updates: loading.updates,
        latest,
    });
}

pub(super) fn prepare(world: &mut World) {
    prepare_at(world, Instant::now());
}

#[cfg(test)]
pub(super) fn build_scene_for_test(world: &mut World) {
    world.init_resource::<RendererReady>();
    prepare(world);
}

fn prepare_at(world: &mut World, now: Instant) {
    if world.resource::<RendererReady>().0.load(Ordering::Relaxed) {
        let screen: Vec<_> = world
            .query_filtered::<Entity, With<Screen>>()
            .iter(world)
            .collect();
        for entity in screen {
            world.despawn(entity);
        }
        world.remove_resource::<Preparing>();
        return;
    }
    let mut preparing = world.resource_mut::<Preparing>();
    let elapsed = now.duration_since(preparing.started);
    let scene = preparing.scene.take();
    move_indicator(world, elapsed);
    if let Some((simulation, title)) = scene {
        build_scene(world, simulation, title);
    }
}

fn move_indicator(world: &mut World, elapsed: Duration) {
    let phase = (elapsed.as_secs_f32() % 2.0 - 1.0).abs();
    for mut bar in world
        .query_filtered::<&mut Node, With<Bar>>()
        .iter_mut(world)
    {
        bar.width = px(INDICATOR_WIDTH);
        bar.left = px((1.0 - phase) * (TRACK_WIDTH - INDICATOR_WIDTH));
    }
}

fn build_scene(world: &mut World, simulation: Simulation, title: String) {
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
                |update| format!("{update:.1}"),
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
    use super::*;
    use crate::progress::{Progress, Stage};

    #[derive(Resource, Default)]
    struct Built(u32);

    /// A loading world whose worker returns `planned` once the returned sender fires.
    fn loading_world(
        planned: Planned,
        selections: mpsc::Receiver<Selection>,
        launched: Instant,
    ) -> (World, Progress, mpsc::Sender<()>) {
        let (progress, updates) = Progress::channel(Duration::ZERO);
        let (release, held) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            held.recv().expect("the test releases planning");
            planned
        });
        let mut world = World::new();
        world.init_resource::<Messages<AppExit>>();
        world.init_resource::<Built>();
        world.init_resource::<RendererReady>();
        world.insert_resource(Loading::new(updates, selections, worker, launched));
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
    fn preparing_renderer_animates_until_the_scene_is_ready() {
        let simulation = Simulation::new(
            crate::timeline::synthetic(),
            &crate::model::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        let (selection, selections) = mpsc::channel();
        let launched = Instant::now();
        let (mut world, progress, release) =
            loading_world(Ok((simulation, "Fixture".to_owned())), selections, launched);
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
        assert_eq!(label.0, "Planning: planning travel 75.0%");
        assert!(!world.contains_resource::<Playback>());

        let mut warnings = world.query_filtered::<(&Text, &Node), With<Warning>>();
        poll_at(&mut world, launched + Duration::from_millis(59_999));
        assert_eq!(warnings.single(&world).unwrap().1.display, Display::None);

        poll_at(&mut world, launched + Duration::from_secs(60));
        let (text, node) = warnings.single(&world).unwrap();
        assert_eq!(node.display, Display::Flex);
        assert_eq!(
            text.0,
            "This history preview is still preparing after a minute."
        );

        selection
            .send(Selection {
                name: "Fixture".to_owned(),
                start_date: Some("2023-02-08".to_owned()),
                replace_start_date: false,
            })
            .unwrap();
        poll_at(&mut world, launched + Duration::from_secs(61));
        let (text, _) = warnings.single(&world).unwrap();
        assert_eq!(
            text.0,
            "Fixture preview is still preparing after a minute. Keep waiting, or close preview and retry with `--start-date 2023-02-08` to view a shorter range."
        );

        release.send(()).unwrap();
        poll_until_planned(&mut world);
        assert!(!world.contains_resource::<Playback>());
        let label = world
            .query_filtered::<&Text, With<Label>>()
            .single(&world)
            .unwrap();
        assert_eq!(label.0, "Preparing renderer");
        assert_eq!(bar(&mut world), px(INDICATOR_WIDTH));
        assert_eq!(warnings.single(&world).unwrap().1.display, Display::None);
        let started = world.resource::<Preparing>().started;
        prepare_at(&mut world, started);
        assert_eq!(world.resource::<Title>().0, "Fixture");
        assert!(world.contains_resource::<Playback>());
        assert!(world.contains_resource::<PreviewCamera>());
        assert_eq!(world.resource::<Built>().0, 1);
        let mut indicator = world.query_filtered::<&Node, With<Bar>>();
        assert_eq!(indicator.single(&world).unwrap().left, px(0));
        prepare_at(&mut world, started + Duration::from_millis(500));
        assert_eq!(indicator.single(&world).unwrap().left, px(192));
        assert_eq!(world.resource::<Built>().0, 1);
        world
            .resource::<RendererReady>()
            .0
            .store(true, Ordering::Relaxed);
        prepare_at(&mut world, started + Duration::from_secs(1));
        assert!(!world.contains_resource::<Preparing>());
        assert_eq!(world.query::<&Screen>().iter(&world).count(), 0);
        assert_eq!(warnings.iter(&world).count(), 0);
        assert!(world.resource::<Messages<AppExit>>().is_empty());

        assert_waiting_label(&mut world, &progress);
    }

    fn assert_waiting_label(world: &mut World, progress: &Progress) {
        let waiting = world.register_system(waiting);
        world.insert_resource(super::super::PlanningWait(true));
        progress.report(Stage::Reservations, 1, 4);
        world.run_system(waiting).unwrap();
        let (text, node) = world
            .query_filtered::<(&Text, &Node), With<WaitLabel>>()
            .single(world)
            .unwrap();
        assert_eq!(text.0, "Planning: reserving parking 25.0%");
        assert_eq!(node.display, Display::Flex);
        world.resource_mut::<super::super::PlanningWait>().0 = false;
        world.run_system(waiting).unwrap();
        assert_eq!(
            world
                .query_filtered::<&Node, With<WaitLabel>>()
                .single(world)
                .unwrap()
                .display,
            Display::None
        );
    }

    #[test]
    fn the_warning_asks_to_replace_an_existing_start_date() {
        let warning = Selection {
            name: "Fixture".to_owned(),
            start_date: Some("2023-02-08".to_owned()),
            replace_start_date: true,
        }
        .warning();
        assert_eq!(
            warning,
            "Fixture preview is still preparing after a minute. Keep waiting, or close preview and replace your start date with `--start-date 2023-02-08` to view a shorter range."
        );
    }

    #[test]
    fn a_planning_error_closes_the_window_and_is_kept() {
        let (_, selections) = mpsc::channel();
        let (mut world, _progress, release) = loading_world(
            Err(anyhow::anyhow!("no such repository")),
            selections,
            Instant::now(),
        );
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
