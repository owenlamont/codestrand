use super::*;

#[test]
#[ignore = "requires local five-repository manifest and output path"]
fn local_v19_audit() {
    let started = std::time::Instant::now();
    let (mut sim, _) = nitpicks::local_simulation();
    let planning = started.elapsed().as_secs_f64();
    let plan_only = std::env::var_os("CODESTRAND_PLAN_ONLY").is_some();
    sim.visual_updates = !plan_only;
    let frames = frames_for(sim.duration(), 60).unwrap();
    let mut label_rays = 0_u64;
    let mut label_pairs = 0_u64;
    let mut title_overlaps = TitleOverlaps::default();
    let mut motion = Motion::default();
    let mut label_clipped = 0_u64;
    let mut own_nearest = 0_u64;
    let mut clip_cases = Vec::new();
    let mut service_ticks = vec![0_u64; sim.events.len()];
    let mut ray_lengths = vec![0.0_f32; sim.events.len()];
    let mut ray_counts = vec![0_u32; sim.events.len()];
    let mut fade_motion = 0_u64;
    for frame in 0..=frames {
        sim.advance_to(frame, 60);
        for pulse in &sim.state.pulses {
            if ray_counts[pulse.commit] > 0 {
                continue;
            }
            service_ticks[pulse.commit] = sim.state.tick;
            let from = titles::screen(&sim, sim.state.avatars[pulse.author].position);
            for &id in &pulse.touched {
                if let Some(to) = sim.state.point(Target::File(id)) {
                    ray_lengths[pulse.commit] +=
                        from.distance(titles::screen(&sim, to));
                    ray_counts[pulse.commit] += 1;
                }
            }
        }
        if plan_only {
            continue;
        }

        let rays: Vec<_> = sim
            .beams()
            .iter()
            .map(|b| (titles::screen(&sim, b.from), titles::screen(&sim, b.to)))
            .collect();
        let labels = sim.labels_for_detail(|_| true);
        let names: Vec<_> = labels
            .iter()
            .filter(|l| l.kind == LabelKind::Avatar)
            .map(|l| titles::label_bounds(&sim, l))
            .collect();
        let content = titles::obstacles(&sim);
        let (clipped, nearest) = label_constraints(&sim, &names);
        label_clipped += clipped;
        if clipped > 0 && clip_cases.len() < 8 {
            clip_cases.push(clip_case(&sim, &names));
        }
        own_nearest += nearest;
        title_overlaps.observe(&sim, &labels, &content, &names);
        for (i, rect) in names.iter().enumerate() {
            label_pairs += u64::try_from(
                names[i + 1..]
                    .iter()
                    .filter(|r| !rect.intersect(**r).is_empty())
                    .count(),
            )
            .unwrap();
            label_rays += u64::try_from(
                rays.iter()
                    .filter(|(a, b)| slots::segment(*a, *b, *rect))
                    .count(),
            )
            .unwrap();
        }
        motion.observe(&sim);
        fade_motion += sim.travel.faded_motion_samples(sim.state.tick);
    }
    let measured: Vec<_> = ray_lengths
        .iter()
        .zip(&ray_counts)
        .filter(|(_, n)| **n > 0)
        .map(|(length, n)| *length / count(usize::try_from(*n).unwrap()))
        .collect();
    let value = serde_json::json!({"duration":sim.duration(),"clip_cases":clip_cases,"label_clipped_frames":label_clipped,"own_nearest_violations":own_nearest,"service_ticks":service_ticks,"planning_seconds":planning,"frames":frames+1,"label_ray_pairs":label_rays,"label_label_pairs":label_pairs,"parked_disc_pairs":motion.disc_pairs,"worst_park_tick":motion.worst_park_tick,"title_content_pairs":title_overlaps.files+title_overlaps.directories,"title_file_pairs":title_overlaps.files,"title_directory_pairs":title_overlaps.directories,"mean_total_ray_length_per_commit":ray_lengths.iter().sum::<f32>()/count(measured.len().max(1)),"frame_text_pairs":title_overlaps.frame_texts,"other_text_pairs":title_overlaps.other_texts,"title_avatar_pairs":title_overlaps.avatars,"angle_jump_samples":motion.jumps,"park_cases":motion.park_cases,"max_angle_delta":motion.max_angle,"max_angular_acceleration":motion.max_acceleration,"mean_ray_length_per_commit":measured.iter().sum::<f32>()/count(measured.len().max(1)),"measured_commits":measured.len(),"faded_motion_samples":fade_motion,"travel":sim.travel.report()});
    std::fs::write(
        std::env::var("CODESTRAND_V19_AUDIT").unwrap(),
        serde_json::to_vec_pretty(&value).unwrap(),
    )
    .unwrap();
}

fn clip_case(sim: &Simulation, names: &[bevy::math::Rect]) -> serde_json::Value {
    serde_json::json!({"tick":sim.state.tick,"bounds":names.iter().map(|r|[r.min.to_array(),r.max.to_array()]).collect::<Vec<_>>(),"avatars":sim.state.avatars.iter().filter(|a|a.presence>0.0).map(|a|serde_json::json!({"name":a.name,"body":titles::screen(sim,a.position).to_array(),"placement":a.name_visibility.placement.as_ref().map(|p|(p.angle,p.angular_velocity))})).collect::<Vec<_>>()})
}
#[derive(Default)]
struct TitleOverlaps {
    files: u64,
    directories: u64,
    avatars: u64,
    frame_texts: u64,
    other_texts: u64,
}
impl TitleOverlaps {
    fn observe(
        &mut self,
        sim: &Simulation,
        labels: &[Label],
        content: &[(usize, bevy::math::Rect)],
        names: &[bevy::math::Rect],
    ) {
        for name in names {
            self.frame_texts += u64::try_from(
                caption::frame_text_rects()
                    .iter()
                    .filter(|r| !name.intersect(**r).is_empty())
                    .count(),
            )
            .unwrap();
            self.other_texts += u64::try_from(
                labels
                    .iter()
                    .filter(|l| l.kind != LabelKind::Avatar)
                    .filter(|l| {
                        !name.intersect(titles::label_bounds(sim, l)).is_empty()
                    })
                    .count(),
            )
            .unwrap();
        }
        let file_count = sim
            .state
            .layout
            .files()
            .filter(|(_, f)| f.visible_presence(sim.state.layout.dir(f.dir)) > 0.0)
            .count();
        for label in labels.iter().filter(|l| l.kind == LabelKind::Repository) {
            let rect = titles::label_bounds(sim, label);
            self.files += u64::try_from(
                content[..file_count]
                    .iter()
                    .filter(|(_, r)| !rect.intersect(*r).is_empty())
                    .count(),
            )
            .unwrap();
            self.directories += u64::try_from(
                content[file_count..]
                    .iter()
                    .filter(|(_, r)| !rect.intersect(*r).is_empty())
                    .count(),
            )
            .unwrap();
            self.avatars += u64::try_from(
                names
                    .iter()
                    .filter(|r| !rect.intersect(**r).is_empty())
                    .count(),
            )
            .unwrap();
        }
    }
}
fn label_constraints(sim: &Simulation, names: &[bevy::math::Rect]) -> (u64, u64) {
    let radius = sim.state.avatar_radius();
    let clipped = names
        .iter()
        .filter(|r| {
            r.min.cmplt(-titles::HALF + Vec2::splat(0.25)).any()
                || r.max.cmpgt(titles::HALF - Vec2::splat(0.25)).any()
        })
        .count();
    let mut nearest = 0;
    for (i, avatar) in sim
        .state
        .avatars
        .iter()
        .filter(|a| a.presence > 0.0)
        .enumerate()
    {
        let body = titles::screen(sim, avatar.position);
        let distance = |rect: bevy::math::Rect| {
            (body.distance(body.clamp(rect.min, rect.max)) - radius).max(0.0)
        };
        let gap = distance(names[i]);
        nearest += usize::from(
            names
                .iter()
                .enumerate()
                .any(|(j, r)| j != i && distance(*r) <= gap + 0.0001),
        );
    }
    (
        u64::try_from(clipped).unwrap(),
        u64::try_from(nearest).unwrap(),
    )
}
fn angle_delta(angle: f32) -> f32 {
    (angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
        - std::f32::consts::PI
}

#[derive(Default)]
struct Motion {
    previous: std::collections::BTreeMap<usize, (f32, f32)>,
    max_angle: f32,
    max_acceleration: f32,
    disc_pairs: u64,
    worst_park_tick: u64,
    jumps: u64,
    park_cases: Vec<serde_json::Value>,
}
impl Motion {
    fn park_case(&mut self, sim: &Simulation, a: usize, b: usize) {
        if self.park_cases.len() >= 8 {
            return;
        }
        let newcomer =
            if sim.state.avatars[a].last_active >= sim.state.avatars[b].last_active {
                a
            } else {
                b
            };
        let others: Vec<_> = sim
            .state
            .avatars
            .iter()
            .enumerate()
            .filter(|(i, a)| {
                *i != newcomer
                    && a.presence > 0.0
                    && !sim.travel.travelling(*i, sim.state.tick)
            })
            .map(|(_, a)| titles::screen(sim, a.position))
            .collect();
        let avatar = &sim.state.avatars[newcomer];
        let clear = sim.state.clear_parking_option(
            newcomer,
            &sim.envelopes[avatar.commit],
            &others,
        );
        self.park_cases.push(serde_json::json!({"tick":sim.state.tick,"author":avatar.name,"other":sim.state.avatars[if newcomer==a {b}else{a}].name,"commit":avatar.commit,"clear_slot_available":clear}));
    }
    fn observe(&mut self, sim: &Simulation) {
        self.previous
            .retain(|i, _| sim.state.avatars[*i].presence > 0.0);
        let radius = sim.state.avatar_radius();
        for (i, avatar) in sim
            .state
            .avatars
            .iter()
            .enumerate()
            .filter(|(_, a)| a.presence > 0.0)
        {
            let body = titles::screen(sim, avatar.position);
            if let Some(name) = &avatar.name_visibility.placement {
                let size = names::rect(
                    Vec2::ZERO,
                    &name.text,
                    name.scale,
                    sim.state.geometry.label_height,
                )
                .size();
                let centre = name.point + Vec2::new(size.x * 0.5, -size.y * 0.5);
                let theta = (centre - body).to_angle();
                if let Some((before, velocity)) = self.previous.insert(i, (theta, 0.0))
                {
                    let delta = angle_delta(theta - before);
                    self.max_angle = self.max_angle.max(delta.abs());
                    self.jumps +=
                        u64::from(delta.abs() > slots::angular_limit() / 60.0 + 0.0001);
                    let speed = name.angular_velocity;
                    self.max_acceleration =
                        self.max_acceleration.max((speed - velocity).abs() * 60.0);
                    self.previous.insert(i, (theta, speed));
                }
            }
            if sim.travel.travelling(i, sim.state.tick) {
                continue;
            }
            for (j, other) in sim
                .state
                .avatars
                .iter()
                .enumerate()
                .skip(i + 1)
                .filter(|(_, a)| a.presence > 0.0)
            {
                if !sim.travel.travelling(j, sim.state.tick)
                    && body.distance(titles::screen(sim, other.position))
                        < 2.0 * radius - 0.001
                {
                    self.disc_pairs += 1;
                    self.worst_park_tick = sim.state.tick;
                    self.park_case(sim, i, j);
                }
            }
        }
    }
}
