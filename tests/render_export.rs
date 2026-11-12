#![forbid(unsafe_code)]
#![deny(clippy::all, clippy::pedantic)]

use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

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

fn export(directory: &Path, delay: &str) {
    let log_path = directory.with_extension("log");
    let log = fs::File::create(&log_path).expect("capture log can be created");
    let mut child = Command::new(env!("CARGO_BIN_EXE_codestrand"))
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
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("export process can be polled") {
            assert!(
                status.success(),
                "export failed: {}",
                fs::read_to_string(&log_path).expect("log is readable")
            );
            return;
        }
        if started.elapsed() > Duration::from_secs(120) {
            child.kill().expect("timed out export can be stopped");
            child.wait().expect("stopped export can be reaped");
            panic!("export timed out: {}", log_path.display());
        }
        thread::sleep(Duration::from_millis(50));
    }
}
