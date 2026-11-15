#![forbid(unsafe_code)]
#![deny(clippy::all, clippy::pedantic)]

use std::{
    fs,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires a graphics adapter"]
fn native_4k_and_ultrawide_exports_repeat_byte_identically() {
    let scratch = tempfile::tempdir().unwrap();
    for (resolution, dimensions) in
        [("3840x2160", (3840, 2160)), ("5120x1440", (5120, 1440))]
    {
        let mut previous = Vec::new();
        for repeat in 0..2 {
            let directory = scratch.path().join(format!("{resolution}-{repeat}"));
            let log_path = directory.with_extension("log");
            let log = fs::File::create(&log_path).unwrap();
            let child = Command::new(env!("CARGO_BIN_EXE_codestrand"))
                .arg("export")
                .arg(&directory)
                .args([
                    "--test-input",
                    "synthetic",
                    "--resolution",
                    resolution,
                    "--frames",
                    "2",
                    "--fps",
                    "2",
                ])
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap();
            assert!(
                wait(child, &log_path).success(),
                "{}",
                fs::read_to_string(&log_path).unwrap()
            );
            let frames: Vec<_> = (0..2)
                .map(|frame| {
                    let path = directory.join(format!("frame-{frame:05}.png"));
                    let image = image::open(&path).unwrap().to_rgb8();
                    assert_eq!(image.dimensions(), dimensions);
                    assert!(
                        image
                            .pixels()
                            .any(|pixel| pixel.0.iter().any(|&channel| channel > 100))
                    );
                    fs::read(path).unwrap()
                })
                .collect();
            if repeat == 1 {
                assert!(
                    previous == frames,
                    "repeat changed PNG bytes at {resolution}"
                );
            }
            previous = frames;
        }
    }
}
#[test]
#[ignore = "requires a graphics adapter and installed Devanagari/CJK fonts"]
fn contributor_scripts_render_without_missing_glyphs() {
    let scratch = tempfile::tempdir().unwrap();
    let proof = std::env::var_os("CODESTRAND_SCRIPT_PROOF")
        .map_or_else(|| scratch.path().join("scripts"), std::path::PathBuf::from);
    let frame = render_names(
        &proof,
        &["Pérez João", "Ελληνικά", "Кириллица", "अभिजीत", "中文姓名"],
    );
    assert_eq!(frame.avatars, 5);
    assert!(frame.glyphs > 40);
    assert_eq!(
        frame.missing, 0,
        "a visible label used the missing-glyph glyph"
    );
    assert_eq!(frame.pixels.dimensions(), (1280, 720));
    for index in 1..=5 {
        let pixels = image::open(proof.join(format!("frames/frame-{index:05}.png")))
            .unwrap()
            .to_rgb8();
        let ink = (700..718)
            .flat_map(|y| (266..296).map(move |x| (x, y)))
            .filter(|&(x, y)| pixels.get_pixel(x, y).0.iter().all(|&v| v > 200))
            .count();
        assert!(ink > 10, "frame {index} has no contributor text");
    }
    let tofu = render_names(&scratch.path().join("tofu"), &["Missing \u{10ffff}"]);
    assert!(
        tofu.missing > 0,
        "the missing-glyph detector accepted its negative control"
    );
}

struct NameFrame {
    pixels: image::RgbImage,
    glyphs: usize,
    missing: usize,
    avatars: usize,
}

fn render_names(root: &Path, names: &[&str]) -> NameFrame {
    fs::create_dir_all(root).unwrap();
    let repository = root.join("repository");
    names_repository(&repository, names);
    let contributors = root.join("contributors.toml");
    fs::write(&contributors, "").unwrap();
    let directory = root.join("frames");
    let log_path = root.join("export.log");
    let log = fs::File::create(&log_path).unwrap();
    let executable = std::env::var_os("CODESTRAND_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_codestrand").into());
    let child = Command::new(executable)
        .arg("export")
        .arg(&directory)
        .arg("--repository")
        .arg(&repository)
        .arg("--contributors")
        .arg(&contributors)
        .args(["--frames", "8", "--fps", "2", "--no-cache"])
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    assert!(
        wait(child, &log_path).success(),
        "{}",
        fs::read_to_string(log_path).unwrap()
    );
    let manifest = fs::read_to_string(directory.join("frames.csv")).unwrap();
    assert!(
        manifest
            .lines()
            .next()
            .unwrap()
            .ends_with(",glyphs,missing_glyphs")
    );
    let row: Vec<_> = manifest.lines().last().unwrap().split(',').collect();
    NameFrame {
        pixels: image::open(directory.join("frame-00007.png"))
            .unwrap()
            .to_rgb8(),
        glyphs: row[6].parse().unwrap(),
        missing: manifest
            .lines()
            .skip(1)
            .map(|line| line.split(',').nth(7).unwrap().parse().unwrap())
            .max()
            .unwrap(),
        avatars: row[4].parse().unwrap(),
    }
}

fn names_repository(path: &Path, names: &[&str]) {
    let repo = gix::init_bare(path).unwrap();
    let mut parent = None;
    for (index, name) in names.iter().enumerate() {
        let blob = repo
            .write_blob(format!("fixture line {index}\n"))
            .unwrap()
            .detach();
        let tree = repo
            .write_object(&gix::objs::Tree {
                entries: vec![gix::objs::tree::Entry {
                    mode: gix::objs::tree::EntryKind::Blob.into(),
                    filename: "file.rs".into(),
                    oid: blob,
                }],
            })
            .unwrap()
            .detach();
        let author = gix::actor::Signature {
            name: (*name).into(),
            email: format!("author{index}@example.invalid").into(),
            time: gix::date::Time {
                seconds: 1_700_000_000 + i64::try_from(index).unwrap() * 86400,
                offset: 0,
            },
        };
        parent = Some(
            repo.write_object(&gix::objs::Commit {
                tree,
                parents: parent.into_iter().collect(),
                author: author.clone(),
                committer: author,
                encoding: None,
                message: "script fixture".into(),
                extra_headers: Vec::new(),
            })
            .unwrap()
            .detach(),
        );
    }
    fs::write(path.join("HEAD"), format!("{}\n", parent.unwrap())).unwrap();
}

#[test]
#[ignore = "requires a graphics adapter; validates motion before the second capture"]
fn pole_crossing_is_rejected_before_second_capture() {
    let scratch = tempfile::tempdir().unwrap().keep();
    let path = scratch.join("pole-crossing.toml");
    fs::write(
        &path,
        r"fps=30
[[key]]
frame=0
eye={point=[1.0,12.0,0.0]}
focus={point=[0.0,12.0,2.0]}
[[key]]
frame=1
eye={point=[-1.0,12.0,0.0]}
focus={point=[0.0,12.0,2.0]}
",
    )
    .unwrap();
    let directory = scratch.join("frames");
    let log_path = scratch.join("export.log");
    let log = fs::File::create(&log_path).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_codestrand"))
        .arg("export")
        .arg(&directory)
        .arg("--camera-path")
        .arg(&path)
        .args(["--test-input", "synthetic", "--frames", "2", "--fps", "30"])
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let status = wait(child, &log_path);
    let error = fs::read_to_string(&log_path).unwrap();
    assert!(!status.success(), "pole crossing was accepted: {error}");
    assert!(error.contains("output frames 0..1"), "{error}");
    assert!(error.contains("intermediate key"), "{error}");
    assert!(directory.join("frame-00000.png").exists());
    assert!(!directory.join("frame-00001.png").exists());
}

fn wait(mut child: Child, log_path: &Path) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("export process can be polled") {
            return status;
        }
        if started.elapsed() > Duration::from_secs(120) {
            child.kill().expect("timed out export can be stopped");
            child.wait().expect("stopped export can be reaped");
            panic!("export timed out: {}", log_path.display());
        }
        thread::sleep(Duration::from_millis(50));
    }
}
