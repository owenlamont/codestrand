use clap::Parser;

use super::*;
use crate::{Cli, Command};

fn fixture(text: &str) -> (std::path::PathBuf, Simulation) {
    let directory = tempfile::tempdir().unwrap().keep();
    let file = directory.join("camera.toml");
    fs::write(&file, text).unwrap();
    let cli = Cli::try_parse_from([
        "codestrand",
        "export",
        "unused-output",
        "--camera-path",
        file.to_str().unwrap(),
    ])
    .unwrap();
    let Some(Command::Export { source, .. }) = cli.command else {
        panic!("export command")
    };
    (file, source.load().unwrap().0)
}

const POINT: &str =
    "eye = { point = [0.0, 12.0, 0.0] }\nfocus = { point = [0.0, 0.0, 0.0] }\n";

#[test]
fn one_second_pan_at_one_fps_passes_but_pole_crossing_fails() {
    let (file, mut sim) = fixture(
        r"fps=30
[[key]]
frame=0
eye={point=[0,12,0]}
focus={point=[0,12,-10]}
up=[0,1,0]
[[key]]
frame=30
eye={point=[0,12,0]}
focus={point=[-8.660254,12,-5]}
",
    );
    let path = CameraPath::load(&file, &sim).unwrap();
    let previous = path.pose(&sim, 0, 1).unwrap();
    sim.advance_to(1, 1);
    let current = path.pose(&sim, 1, 1).unwrap();
    assert!((previous.forward().dot(*current.forward()) - 0.5).abs() < 0.0001);
    path.validate_step(&previous, &current, 1).unwrap();

    fs::write(
        &file,
        r"fps=30
[[key]]
frame=0
eye={point=[1,12,0]}
focus={point=[0,12,2]}
[[key]]
frame=1
eye={point=[-1,12,0]}
focus={point=[0,12,2]}
",
    )
    .unwrap();
    let path = CameraPath::load(&file, &sim).unwrap();
    let previous = path.pose(&sim, 0, 30).unwrap();
    let current = path.pose(&sim, 1, 30).unwrap();
    let error = path
        .validate_step(&previous, &current, 1)
        .unwrap_err()
        .to_string();
    for advice in [
        "output frames 0..1",
        "intermediate key",
        "raise the export fps",
        "lengthen the move",
    ] {
        assert!(error.contains(advice), "{error}");
    }
}

#[test]
fn review_renamed_file_keeps_identity_for_old_and_new_names() {
    let (file, _) = fixture("fps=30\nkey=[]");
    let mut events = crate::timeline::synthetic();
    events.truncate(2);
    events[0].changes.truncate(2);
    events[0].changes[0].path = "other/stable.rs".into();
    events[0].changes[1].path = "source/old.rs".into();
    events[1].changes.truncate(1);
    events[1].changes[0].path = "destination/new.rs".into();
    events[1].changes[0].kind = crate::timeline::Kind::Moved {
        from: "source/old.rs".into(),
    };
    let mut sim = Simulation::new(
        events,
        &crate::model::Options::default(),
        &crate::theme::Theme::neon(),
        &crate::contributors::Mappings::default(),
    );
    let mut paths = Vec::new();
    for name in ["source/old.rs", "destination/new.rs"] {
        fs::write(&file, format!("fps=30\n[[key]]\nframe=0\neye={{file='{name}',offset=[0,8,0]}}\nfocus={{point=[0,0,0]}}")).unwrap();
        paths.push(CameraPath::load(&file, &sim).unwrap());
    }
    for frame in [180, 45, 180] {
        sim.advance_to(frame, 60);
        let target = sim
            .layout()
            .files()
            .find(|(_, file)| {
                file.path.ends_with("/old.rs") || file.path.ends_with("/new.rs")
            })
            .unwrap()
            .1;
        let expected = sim.file_effect(target).0 + Vec3::Y * 8.0;
        assert!(expected.x.abs() + expected.z.abs() > 0.01);
        for path in &paths {
            assert!(
                path.pose(&sim, frame, 60)
                    .unwrap()
                    .translation
                    .abs_diff_eq(expected, 0.0001)
            );
        }
    }
}

#[test]
fn review_history_only_directory_is_not_found_instead_of_panicking() {
    let (file, _) = fixture("fps=30\nkey=[]");
    let mut events = crate::timeline::synthetic();
    events.truncate(1);
    events[0].changes.truncate(1);
    events[0].changes[0].path = "new.rs".into();
    events[0].changes[0].kind = crate::timeline::Kind::Moved {
        from: "ghost/old.rs".into(),
    };
    let sim = Simulation::new(
        events,
        &crate::model::Options::default(),
        &crate::theme::Theme::neon(),
        &crate::contributors::Mappings::default(),
    );
    assert!(
        (0..sim.layout().dir_count())
            .all(|index| sim.layout().dir(index).path != "0/ghost")
    );
    fs::write(&file, "fps=30\n[[key]]\nframe=0\neye={directory='ghost',offset=[0,8,0]}\nfocus={point=[0,0,0]}").unwrap();
    let error = CameraPath::load(&file, &sim)
        .err()
        .expect("history-only directory is rejected");
    assert!(error.to_string().contains("path \"ghost\" not found"));
}

#[test]
fn review_missing_avatar_errors_and_seek_has_no_camera_state() {
    let (file, mut sim) = fixture(
        "fps=30\n[[key]]\nframe=0\neye={avatar='Ada',offset=[0,8,0]}\nfocus={point=[0,0,0]}",
    );
    let path = CameraPath::load(&file, &sim).unwrap();
    let error = path.pose(&sim, 0, 60).unwrap_err().to_string();
    assert!(
        error.contains("output frame 0")
            && error.contains("Ada")
            && error.contains("has no position"),
        "{error}"
    );
    sim.advance_to(90, 60);
    let pose = path.pose(&sim, 90, 60).unwrap();
    sim.advance_to(0, 60);
    assert_eq!(path.pose(&sim, 0, 60).unwrap_err().to_string(), error);
    sim.advance_to(90, 60);
    assert_eq!(path.pose(&sim, 90, 60).unwrap(), pose);
}

#[test]
fn frame_keys_ease_and_clamp_with_file_fps() {
    let (file, sim) = fixture(&format!(
        "fps=24\n[[key]]\nframe=24\n{POINT}[[key]]\nframe=72\neye={{point=[8.0,12.0,0.0]}}\nfocus={{point=[8.0,0.0,0.0]}}"
    ));
    let path = CameraPath::load(&file, &sim).unwrap();
    for (frame, x) in [
        (0, 0.0),
        (30, 0.0),
        (45, 1.25),
        (60, 4.0),
        (90, 8.0),
        (120, 8.0),
    ] {
        assert!((path.pose(&sim, frame, 30).unwrap().translation.x - x).abs() < 0.0001);
    }
}

#[test]
fn example_tracks_live_targets_and_replays_identically() {
    let text = fs::read_to_string("docs/examples/demo-camera.toml").unwrap();
    let (file, mut sim) = fixture(&text);
    let path = CameraPath::load(&file, &sim).unwrap();
    let mut poses = Vec::new();
    for frame in 0..=240 {
        sim.advance_to(frame, 30);
        let pose = path.pose(&sim, frame, 30).unwrap();
        if let Some(previous) = poses.last() {
            let previous: &Transform = previous;
            path.validate_step(previous, &pose, frame).unwrap();
            assert!(pose.rotation.angle_between(previous.rotation) < 0.5);
        }
        if (60..=120).contains(&frame) {
            let avatar = sim
                .avatars()
                .iter()
                .find(|avatar| avatar.name == "Ada")
                .unwrap();
            assert!(
                pose.translation
                    .abs_diff_eq(avatar.position + Vec3::new(0.0, 6.0, 8.0), 0.0001)
            );
        }
        if frame >= 180 {
            assert!(pose.up().dot(Vec3::NEG_Z) > 0.9999);
        }
        poses.push(pose);
    }
    sim.advance_to(0, 30);
    for (frame, pose) in (0..=240).zip(poses) {
        sim.advance_to(frame, 30);
        assert_eq!(path.pose(&sim, frame, 30).unwrap(), pose);
        assert_eq!(path.pose(&sim, frame, 30).unwrap(), pose);
    }
}

#[test]
fn targets_require_positions_and_report_ambiguous_repositories() {
    let text = "fps=30\n[[key]]\nframe=0\neye={file='src/main.rs',offset=[0,6,8]}\nfocus={point=[0,0,0]}";
    let (file, sim) = fixture(text);
    let path = CameraPath::load(&file, &sim).unwrap();
    assert!(
        path.pose(&sim, 0, 30)
            .unwrap_err()
            .to_string()
            .contains("has no position")
    );
    let mut events = Vec::new();
    for index in 0..2 {
        let mut history = crate::timeline::synthetic();
        crate::repositories::namespace(&mut history, index);
        events.extend(history);
    }
    let mut sim = Simulation::with_repositories(
        events,
        &crate::model::Options::default(),
        &crate::theme::Theme::neon(),
        &crate::contributors::Mappings::default(),
        &["One".into(), "Two".into()],
    );
    let error = CameraPath::load(&file, &sim).err().unwrap().to_string();
    assert!(
        error.contains("qualify repository: \"One\", \"Two\""),
        "{error}"
    );
    fs::write(
        &file,
        text.replace("file='src/main.rs'", "file='src/main.rs',repository='Two'"),
    )
    .unwrap();
    let path = CameraPath::load(&file, &sim).unwrap();
    sim.advance_to(600, 30);
    let target = sim
        .layout()
        .files()
        .find(|(_, file)| file.path == "1/src/main.rs")
        .unwrap()
        .1;
    assert!(
        path.pose(&sim, 600, 30)
            .unwrap()
            .translation
            .abs_diff_eq(sim.file_effect(target).0 + Vec3::new(0.0, 6.0, 8.0), 0.0001)
    );
}

#[test]
fn malformed_paths_and_orientation_fail_with_specific_diagnostics() {
    let cases = [
        (
            "fps=30\n[[key]]\nframe=0\neye={avatar='Nobody',offset=[0,6,8]}\nfocus={point=[0,0,0]}",
            "contributor \"Nobody\" not found",
        ),
        (
            "fps=30\n[[key]]\nframe=0\neye={file='absent.rs',offset=[0,6,8]}\nfocus={point=[0,0,0]}",
            "path \"absent.rs\" not found",
        ),
        (
            "fps=30\n[[key]]\nframe=0\neye={point=[nan,12,0]}\nfocus={point=[0,0,0]}",
            "coordinates must be finite",
        ),
        (
            "fps=30\n[[key]]\nframe=0\neye={point=[0,0,0]}\nfocus={point=[0,0,0]}",
            "EYE_FLOOR",
        ),
        (
            "fps=30\n[[key]]\nframe=0\neye={point=[0,12,0]}\nfocus={point=[0,12,0]}",
            "eye and focus must differ",
        ),
        (
            "fps=30\n[[key]]\nframe=0\neye={point=[0,12,0]}\nfocus={point=[0,0,0]}\nup=[0,1,0]",
            "up must not be parallel",
        ),
        ("fps=30\nunknown=true", "unknown field"),
    ];
    for (text, expected) in cases {
        let (file, sim) = fixture(text);
        let result =
            CameraPath::load(&file, &sim).and_then(|path| path.pose(&sim, 0, 30));
        let error = anyhow::Error::from(result.unwrap_err());
        assert!(format!("{error:#}").contains(expected), "{error:#}");
    }
    for frame in [0, 1] {
        let (file, sim) = fixture(&format!(
            "fps=30\n[[key]]\nframe=1\n{POINT}[[key]]\nframe={frame}\n{POINT}"
        ));
        assert!(
            CameraPath::load(&file, &sim)
                .err()
                .unwrap()
                .to_string()
                .contains("increase strictly")
        );
    }
    let (file, sim) = fixture(&format!(
        "fps=30\n[[key]]\nframe=0\n{POINT}[[key]]\nframe=30\n{POINT}up=[0,0,1]"
    ));
    assert!(
        CameraPath::load(&file, &sim)
            .err()
            .unwrap()
            .to_string()
            .contains("must not be opposite")
    );
}
