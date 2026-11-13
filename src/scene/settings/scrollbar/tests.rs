use super::*;
use bevy::{
    camera::{RenderTarget, Viewport},
    picking::{
        backend::HitData,
        events::{Pointer, PointerDrag, PointerDragStart, PointerPress},
        pointer::{Location, PointerButton, PointerId},
    },
    ui::ComputedUiRenderTargetInfo,
    ui_widgets::ScrollbarPlugin,
    window::{PrimaryWindow, WindowRef},
};

struct Fixture {
    app: App,
    target: Entity,
    bar: Entity,
    window: Entity,
    camera: Entity,
    offset: UVec2,
    ui_scale: f32,
}

impl Fixture {
    fn new(offset: UVec2, dpi: f32, ui_scale: f32) -> Self {
        let mut app = App::new();
        app.insert_resource(UiScale(ui_scale))
            .add_plugins(ScrollbarPlugin);
        let mut window = Window::default();
        window.resolution.set_scale_factor(dpi);
        let window = app.world_mut().spawn((window, PrimaryWindow)).id();
        let camera = app
            .world_mut()
            .spawn((
                Camera {
                    viewport: Some(Viewport {
                        physical_position: (offset.as_vec2() * dpi).as_uvec2(),
                        physical_size: UVec2::new(1920, 1080),
                        ..default()
                    }),
                    ..default()
                },
                super::super::super::AvatarCamera,
            ))
            .id();
        let scale = dpi * ui_scale;
        let target = app
            .world_mut()
            .spawn((
                ScrollPosition(Vec2::new(0.0, 350.0)),
                ComputedNode {
                    size: Vec2::new(300.0, 390.0) * scale,
                    content_size: Vec2::new(300.0, 1000.0) * scale,
                    inverse_scale_factor: 1.0 / scale,
                    ..default()
                },
            ))
            .id();
        let bar = app
            .world_mut()
            .spawn(widget(target))
            .insert((
                ComputedNode {
                    size: Vec2::new(14.0, 390.0) * scale,
                    inverse_scale_factor: 1.0 / scale,
                    ..default()
                },
                ComputedUiRenderTargetInfo::default(),
                UiGlobalTransform::from_translation(Vec2::new(400.0, 345.0) * scale),
            ))
            .id();
        Self {
            app,
            target,
            bar,
            window,
            camera,
            offset,
            ui_scale,
        }
    }

    fn pointer(&self, point: Vec2) -> Pointer {
        Pointer::new(
            PointerId::Mouse,
            Location {
                target: RenderTarget::Window(WindowRef::Entity(self.window))
                    .normalize(Some(self.window))
                    .unwrap(),
                position: point * self.ui_scale + self.offset.as_vec2(),
            },
        )
    }

    fn press(&mut self, entity: Entity, point: Vec2) {
        let pointer = self.pointer(point);
        self.app.world_mut().trigger(PointerPress {
            entity,
            pointer,
            button: PointerButton::Primary,
            hit: HitData::new(self.camera, 0.0, None, None),
            count: 1,
        });
    }
}

#[test]
fn track_pages_in_the_same_direction_with_a_letterboxed_viewport() {
    for offset in [UVec2::ZERO, UVec2::new(1080, 180)] {
        let mut fixture = Fixture::new(offset, 1.0, 1.0);
        let track = fixture
            .app
            .world()
            .get::<Children>(fixture.bar)
            .unwrap()
            .iter()
            .find(|child| fixture.app.world().get::<ScrollbarThumb>(*child).is_none())
            .unwrap_or(fixture.bar);
        fixture.press(track, Vec2::new(400.0, 240.0));
        assert_eq!(
            fixture
                .app
                .world()
                .get::<ScrollPosition>(fixture.target)
                .unwrap()
                .y,
            0.0,
            "offset {offset:?}"
        );
        fixture
            .app
            .world_mut()
            .get_mut::<ScrollPosition>(fixture.target)
            .unwrap()
            .y = 350.0;
        fixture.press(track, Vec2::new(400.0, 500.0));
        assert_eq!(
            fixture
                .app
                .world()
                .get::<ScrollPosition>(fixture.target)
                .unwrap()
                .y,
            610.0
        );
    }
}

#[test]
fn track_click_coordinates_account_for_hidpi_and_ui_scale() {
    let mut fixture = Fixture::new(UVec2::new(1080, 180), 2.0, 1.5);
    let track = fixture
        .app
        .world()
        .get::<Children>(fixture.bar)
        .unwrap()
        .iter()
        .find(|child| fixture.app.world().get::<ScrollbarThumb>(*child).is_none())
        .unwrap();
    fixture.press(track, Vec2::new(400.0, 240.0));
    assert_eq!(
        fixture
            .app
            .world()
            .get::<ScrollPosition>(fixture.target)
            .unwrap()
            .y,
        0.0
    );
}

#[test]
fn core_thumb_dragging_uses_displacement_under_offset_and_scaling() {
    for offset in [UVec2::ZERO, UVec2::new(1080, 180)] {
        for (dpi, ui_scale) in [(1.0, 1.0), (2.0, 1.5)] {
            let mut fixture = Fixture::new(offset, dpi, ui_scale);
            let thumb = fixture
                .app
                .world()
                .get::<Children>(fixture.bar)
                .unwrap()
                .iter()
                .find(|child| {
                    fixture.app.world().get::<ScrollbarThumb>(*child).is_some()
                })
                .unwrap();
            fixture.press(thumb, Vec2::new(400.0, 400.0));
            assert_eq!(
                fixture
                    .app
                    .world()
                    .get::<ScrollPosition>(fixture.target)
                    .unwrap()
                    .y,
                350.0
            );
            let pointer = fixture.pointer(Vec2::new(400.0, 400.0));
            fixture.app.world_mut().trigger(PointerDragStart {
                entity: thumb,
                pointer,
                button: PointerButton::Primary,
                hit: HitData::new(fixture.camera, 0.0, None, None),
            });
            let pointer = fixture.pointer(Vec2::new(400.0, 439.0));
            fixture.app.world_mut().trigger(PointerDrag {
                entity: thumb,
                pointer,
                button: PointerButton::Primary,
                distance: Vec2::new(0.0, 39.0 * ui_scale),
                delta: Vec2::new(0.0, 39.0 * ui_scale),
            });
            assert!(
                (fixture
                    .app
                    .world()
                    .get::<ScrollPosition>(fixture.target)
                    .unwrap()
                    .y
                    - 450.0)
                    .abs()
                    < 0.001
            );
        }
    }
}
