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
fn targets_use_fallbacks_and_report_ambiguous_repositories() {
    let text = "fps=30\n[[key]]\nframe=0\neye={file='src/main.rs',point=[1,2,3],offset=[0,6,8]}\nfocus={point=[0,0,0]}";
    let (file, sim) = fixture(text);
    let path = CameraPath::load(&file, &sim).unwrap();
    assert_eq!(
        path.pose(&sim, 0, 30).unwrap().translation,
        Vec3::new(1.0, 8.0, 11.0)
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
