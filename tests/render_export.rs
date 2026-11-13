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
                .args(["--resolution", resolution, "--frames", "2", "--fps", "2"])
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
#[ignore = "requires a graphics adapter; repeats sibling-directory junction exports"]
fn sibling_directory_exports_are_identical() {
    let scratch = tempfile::tempdir().unwrap();
    let repository = scratch.path().join("repository");
    let repo = gix::init_bare(&repository).unwrap();
    let blob = repo.write_blob(b"inline fixture\n").unwrap().detach();
    let tree = junction_tree(&repo, blob, 3);
    let author = gix::actor::Signature {
        name: "Fixture".into(),
        email: "fixture@example.invalid".into(),
        time: gix::date::Time {
            seconds: 1_700_000_000,
            offset: 0,
        },
    };
    let commit = repo
        .write_object(&gix::objs::Commit {
            tree,
            parents: std::iter::empty().collect(),
            author: author.clone(),
            committer: author,
            encoding: None,
            message: "sibling directories".into(),
            extra_headers: Vec::new(),
        })
        .unwrap()
        .detach();
    fs::write(repository.join("HEAD"), format!("{commit}\n")).unwrap();
    let camera = scratch.path().join("camera.toml");
    fs::write(&camera, "fps=30\n[[key]]\nframe=0\neye={point=[0.0,12.0,0.0]}\nfocus={point=[0.0,0.0,0.0]}\n").unwrap();
    let first = scratch.path().join("first");
    let second = scratch.path().join("second");
    let executable = std::env::var_os("CODESTRAND_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_codestrand").into());
    for directory in [&first, &second] {
        let log_path = directory.with_extension("log");
        let log = fs::File::create(&log_path).unwrap();
        let child = Command::new(&executable)
            .arg("export")
            .arg(directory)
            .arg("--repository")
            .arg(&repository)
            .arg("--camera-path")
            .arg(&camera)
            .args([
                "--start",
                "1",
                "--frames",
                "24",
                "--fps",
                "30",
                "--no-cache",
            ])
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        assert!(
            wait(child, &log_path).success(),
            "{}",
            fs::read_to_string(log_path).unwrap()
        );
    }
    for frame in 0..24 {
        let filename = format!("frame-{frame:05}.png");
        let first = image::open(first.join(&filename)).unwrap().to_rgb8();
        let second = image::open(second.join(&filename)).unwrap().to_rgb8();
        assert_eq!(first.dimensions(), second.dimensions());
        let mut count = 0;
        let mut bounds = (u32::MAX, u32::MAX, 0, 0);
        for (x, y, pixel) in first.enumerate_pixels() {
            if pixel != second.get_pixel(x, y) {
                count += 1;
                bounds = (
                    bounds.0.min(x),
                    bounds.1.min(y),
                    bounds.2.max(x),
                    bounds.3.max(y),
                );
            }
        }
        assert_eq!(
            count, 0,
            "{filename}: {count} unequal pixels, inclusive bounds {bounds:?}"
        );
    }
}

fn junction_tree(
    repo: &gix::Repository,
    blob: gix::ObjectId,
    depth: u8,
) -> gix::ObjectId {
    let entries = if depth == 0 {
        vec![gix::objs::tree::Entry {
            mode: gix::objs::tree::EntryKind::Blob.into(),
            filename: "file.rs".into(),
            oid: blob,
        }]
    } else {
        let child = junction_tree(repo, blob, depth - 1);
        ["a", "b", "c", "d"]
            .into_iter()
            .map(|name| gix::objs::tree::Entry {
                mode: gix::objs::tree::EntryKind::Tree.into(),
                filename: name.into(),
                oid: child,
            })
            .collect()
    };
    repo.write_object(&gix::objs::Tree { entries })
        .unwrap()
        .detach()
}

#[test]
#[ignore = "requires a graphics adapter; renders two short PNG sequences"]
fn export_is_complete_and_independent_of_render_pacing() {
    let scratch = tempfile::tempdir().expect("temporary directory is available");
    let fast = scratch.path().join("fast");
    let slow = scratch.path().join("slow");
    export(&fast, "0");
    export(&slow, "50");
    let expected: Vec<_> = (0..8)
        .map(|index| format!("frame-{index:05}.png"))
        .collect();
    for directory in [&fast, &slow] {
        let mut actual: Vec<_> = fs::read_dir(directory)
            .expect("export directory exists")
            .map(|entry| entry.expect("directory entry is readable").file_name())
            .filter(|name| Path::new(name).extension().is_some_and(|ext| ext == "png"))
            .collect();
        actual.sort();
        let expected_names: Vec<_> =
            expected.iter().map(std::ffi::OsString::from).collect();
        assert_eq!(actual, expected_names);
    }
    for filename in &expected {
        let fast_image = image::open(fast.join(filename))
            .expect("PNG is decodable")
            .to_rgb8();
        let slow_image = image::open(slow.join(filename))
            .expect("PNG is decodable")
            .to_rgb8();
        assert_eq!(fast_image.dimensions(), (1280, 720));
        assert!(fast_image == slow_image, "render pacing changed {filename}");
    }
    let first = fs::read(fast.join(&expected[0])).expect("first frame exists");
    let last = fs::read(fast.join(&expected[7])).expect("last frame exists");
    assert!(first != last, "scene did not advance");
    let fast_manifest =
        fs::read_to_string(fast.join("frames.csv")).expect("manifest exists");
    let slow_manifest =
        fs::read_to_string(slow.join("frames.csv")).expect("manifest exists");
    assert_eq!(fast_manifest.lines().count(), 9);
    assert_eq!(fast_manifest, slow_manifest);
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
        .args(["--frames", "2", "--fps", "30"])
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

fn export(directory: &Path, delay: &str) {
    let log_path = directory.with_extension("log");
    let log = fs::File::create(&log_path).expect("capture log can be created");
    let child = Command::new(env!("CARGO_BIN_EXE_codestrand"))
        .arg("export")
        .arg(directory)
        .arg("--camera-path")
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("docs/examples/demo-camera.toml"),
        )
        .args(["--frames", "8", "--fps", "2", "--frame-delay-ms", delay])
        .stdout(Stdio::from(
            log.try_clone().expect("log handle can be cloned"),
        ))
        .stderr(Stdio::from(log))
        .spawn()
        .expect("export process can start");
    let status = wait(child, &log_path);
    assert!(
        status.success(),
        "export failed: {}",
        fs::read_to_string(&log_path).unwrap()
    );
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
