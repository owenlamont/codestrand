use super::*;

fn fixture() -> Simulation {
    let mut events = crate::timeline::synthetic();
    events.truncate(1);
    events[0].changes.truncate(1);
    let mut sim = Simulation::new(
        events,
        &super::super::Options::default(),
        &crate::theme::Theme::neon(),
        &crate::contributors::Mappings::default(),
    );
    sim.advance_to(120, 60);
    sim.state.avatars.truncate(1);
    sim.state.avatars[0].name = "Reader".into();
    sim.state.avatars[0].presence = 1.0;
    sim.state.avatars[0].name_visibility.placement = None;
    sim.state
        .place_slot_names(1.0 / 60.0, &super::super::travel::Plan::default(), &[]);
    sim
}

fn step(sim: &mut Simulation, texts: &[Rect]) -> super::super::names::Placement {
    sim.state.tick += 1;
    sim.state.place_slot_names(
        1.0 / 60.0,
        &super::super::travel::Plan::default(),
        texts,
    );
    sim.state.avatars[0]
        .name_visibility
        .placement
        .clone()
        .unwrap()
}

fn bounds(sim: &Simulation) -> Rect {
    let p = sim.state.avatars[0]
        .name_visibility
        .placement
        .as_ref()
        .unwrap();
    let size = super::super::names::rect(
        Vec2::ZERO,
        &p.text,
        p.scale,
        sim.state.geometry.label_height,
    )
    .size();
    labels::tangent_rect(p.body, size, sim.state.avatar_radius() + 4.0 / 3.0, p.angle)
}

#[test]
fn label_speed_accepts_occlusion_instead_of_accelerating() {
    let mut sim = fixture();
    let mut previous = sim.state.avatars[0]
        .name_visibility
        .placement
        .as_ref()
        .unwrap()
        .angle;
    for _ in 0..600 {
        let blocked = bounds(&sim).inflate(12.0);
        let p = step(&mut sim, &[blocked]);
        assert!((p.angle - previous).abs() * 60.0 <= std::f32::consts::PI / 6.0 + 1e-5);
        previous = p.angle;
    }
}

fn rotate(p: &mut super::super::names::Placement, target: f32, tick: u64) {
    let (angle, velocity, motion) = labels::move_name(p, target, 1.0, tick, 1.0 / 60.0);
    p.angle = angle;
    p.angular_velocity = velocity;
    p.motion = motion;
}

#[test]
fn label_reversals_remain_three_seconds_apart_across_stops_and_commits() {
    let sim = fixture();
    let mut p = sim.state.avatars[0]
        .name_visibility
        .placement
        .clone()
        .unwrap();
    p.angle = 0.0;
    p.angular_velocity = 0.0;
    p.motion = super::super::names::Motion::default();
    let mut direction = 0_i8;
    let mut last = None;
    let mut reversals = 0;
    for tick in 0..3600_u64 {
        p.commit = usize::try_from(tick / 90).unwrap();
        let target = if tick % 120 < 60 { 0.3 } else { -0.3 };
        let previous = p.angle;
        rotate(&mut p, target, tick);
        let delta = p.angle - previous;
        if delta.abs() * 60.0 > 0.001_f32.to_radians() {
            let next_direction = if delta > 0.0 { 1 } else { -1 };
            if direction != 0 && next_direction != direction {
                if let Some(last) = last {
                    assert!(tick - last >= 180, "reversal at {tick}, previous {last}");
                }
                last = Some(tick);
                reversals += 1;
            }
            direction = next_direction;
        }
    }
    assert!(
        reversals >= 2,
        "lockout must still permit sustained direction changes"
    );
}

#[test]
fn label_hysteresis_ignores_brief_relative_gains() {
    let mut sim = fixture();
    let initial = sim.state.avatars[0]
        .name_visibility
        .placement
        .as_ref()
        .unwrap()
        .angle;
    let full = bounds(&sim);
    let small =
        Rect::from_center_size(full.center(), full.size() * Vec2::new(0.1, 1.0));
    for tick in 0..120 {
        let blocked = if tick < 60 { small } else { full };
        let texts = if tick % 60 >= 44 {
            Vec::new()
        } else {
            vec![blocked]
        };
        let p = step(&mut sim, &texts);
        assert!(
            (p.angle - initial).abs() < 1e-6,
            "moved before dwell at {tick}"
        );
    }
    for _ in 0..120 {
        step(&mut sim, &[full]);
    }
    let angle = sim.state.avatars[0]
        .name_visibility
        .placement
        .as_ref()
        .unwrap()
        .angle;
    assert!(
        (angle - initial).abs() > 0.05,
        "sustained gain must allow movement"
    );
}
