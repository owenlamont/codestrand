use bevy::{
    ecs::system::RunSystemOnce,
    picking::{
        backend::HitData,
        events::{Pointer, PointerDrag, PointerDragStart},
        pointer::{Location, PointerButton, PointerId},
    },
    ui::UiScale,
    ui_widgets::SliderPlugin,
};

use super::super::{AvatarCamera, Page, Tab};
use super::*;
use crate::model::Simulation;

fn app(open: bool) -> App {
    let mut app = App::new();
    app.insert_resource(Settings { open, ..default() })
        .insert_resource(Playback(Simulation::new(
            crate::timeline::synthetic(),
            &crate::model::Options::default(),
            &crate::theme::Theme::neon(),
            &crate::contributors::Mappings::default(),
        )))
        .add_systems(Update, show);
    app.world_mut().spawn(page());
    app
}

fn choice(app: &mut App, wanted: Choice) -> Entity {
    app.world_mut()
        .query::<(Entity, &Choice)>()
        .iter(app.world())
        .find(|(_, choice)| **choice == wanted)
        .unwrap()
        .0
}

fn group(app: &App, button: Entity) -> Entity {
    app.world().get::<ChildOf>(button).unwrap().parent()
}

fn slider(app: &mut App) -> Entity {
    app.world_mut()
        .query_filtered::<Entity, With<Speed>>()
        .single(app.world())
        .unwrap()
}

fn simulation(app: &App) -> &Simulation {
    &app.world().resource::<Playback>().0
}

#[test]
fn widgets_edit_the_live_simulation() {
    let mut app = app(true);
    let phosphor = choice(&mut app, Choice::Theme(ThemeName::Phosphor));
    let chip = choice(&mut app, Choice::Caption(Style::Chip));
    let collapse = choice(&mut app, Choice::Collapse);
    let speed = slider(&mut app);
    for (source, value) in
        [(group(&app, phosphor), phosphor), (group(&app, chip), chip)]
    {
        app.world_mut().trigger(ValueChange {
            source,
            value,
            is_final: true,
        });
    }
    app.world_mut().trigger(ValueChange {
        source: collapse,
        value: false,
        is_final: true,
    });
    app.world_mut().trigger(ValueChange {
        source: speed,
        value: 2.2_f32,
        is_final: false,
    });
    app.world_mut().flush();
    assert_eq!(
        app.world().resource::<RequestedTheme>().0,
        ThemeName::Phosphor
    );
    assert_eq!(simulation(&app).caption_style, Style::Chip);
    assert!(!simulation(&app).collapse_directories);
    assert_eq!(simulation(&app).playback_multiplier(), 4.0);
}

#[test]
fn closed_settings_ignore_widget_changes() {
    let mut app = app(false);
    let speed = slider(&mut app);
    let collapse = choice(&mut app, Choice::Collapse);
    app.world_mut().trigger(ValueChange {
        source: speed,
        value: 3.0_f32,
        is_final: true,
    });
    app.world_mut().trigger(ValueChange {
        source: collapse,
        value: false,
        is_final: true,
    });
    assert_eq!(simulation(&app).playback_multiplier(), 1.0);
    assert!(simulation(&app).collapse_directories);
}

#[test]
fn widgets_show_changes_made_with_keys() {
    let mut app = app(true);
    app.world_mut()
        .resource_mut::<Playback>()
        .0
        .change_rate(0.5);
    app.world_mut().resource_mut::<Playback>().0.caption_style = Style::Chip;
    app.update();
    let speed = slider(&mut app);
    assert_eq!(app.world().get::<SliderValue>(speed).unwrap().0, -1.0);
    for (wanted, checked) in [
        (Choice::Caption(Style::Chip), true),
        (Choice::Caption(Style::Bar), false),
        (Choice::Theme(ThemeName::Neon), true),
        (Choice::Collapse, true),
    ] {
        let entity = choice(&mut app, wanted);
        assert_eq!(app.world().get::<Checked>(entity).is_some(), checked);
    }
}

#[test]
fn dragging_the_thumb_one_step_doubles_speed_under_ui_scale() {
    let mut app = app(true);
    let ui_scale = 1.5;
    app.insert_resource(UiScale(ui_scale))
        .init_resource::<bevy::picking::hover::PointerCaptureMap>()
        .add_plugins(SliderPlugin);
    let window = app.world_mut().spawn(Window::default()).id();
    let speed = slider(&mut app);
    // Six steps share the track left of the thumb: 300 logical pixels each 50 wide.
    let width = 300.0 + THUMB;
    app.world_mut().entity_mut(speed).insert((
        ComputedNode {
            size: Vec2::new(width, THUMB) * ui_scale,
            inverse_scale_factor: 1.0 / ui_scale,
            ..default()
        },
        UiGlobalTransform::default(),
    ));
    let thumb = app
        .world_mut()
        .query_filtered::<Entity, With<SliderThumb>>()
        .single(app.world())
        .unwrap();
    app.world_mut().entity_mut(thumb).insert(ComputedNode {
        size: Vec2::splat(THUMB * ui_scale),
        inverse_scale_factor: 1.0 / ui_scale,
        ..default()
    });
    let pointer = Pointer::new(
        PointerId::Mouse,
        Location {
            target: bevy::camera::RenderTarget::Window(
                bevy::window::WindowRef::Entity(window),
            )
            .normalize(Some(window))
            .unwrap(),
            // Letterboxing offsets positions; dragging reads only the distance.
            position: Vec2::new(1000.0, 400.0),
        },
    );
    let hit = HitData::new(window, 0.0, None, None);
    app.world_mut().trigger(PointerDragStart {
        entity: speed,
        pointer: pointer.clone(),
        button: PointerButton::Primary,
        hit,
    });
    app.world_mut().trigger(PointerDrag {
        entity: speed,
        pointer,
        button: PointerButton::Primary,
        distance: Vec2::new(50.0 * ui_scale, 0.0),
        delta: Vec2::new(50.0 * ui_scale, 0.0),
    });
    app.world_mut().flush();
    assert_eq!(simulation(&app).playback_multiplier(), 2.0);
}

#[test]
fn panel_spawns_both_pages() {
    let mut app = App::new();
    app.world_mut().spawn((Camera::default(), AvatarCamera));
    app.world_mut()
        .run_system_once(super::super::setup)
        .unwrap();
    let pages: Vec<_> = app
        .world_mut()
        .query_filtered::<&Page, Without<Tab>>()
        .iter(app.world())
        .copied()
        .collect();
    assert_eq!(pages, [Page::Display, Page::Playback]);
}
