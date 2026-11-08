#![forbid(unsafe_code)]
#![deny(clippy::all, clippy::pedantic)]

use std::process::Command;

#[test]
fn invalid_cli_values_cannot_emit_terminal_controls() {
    let output = Command::new(env!("CARGO_BIN_EXE_codestrand"))
        .args(["preview", "--seconds-per-day", "\u{1b}[31m\u{7}"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!output.stderr.contains(&0x1b));
    assert!(!output.stderr.contains(&7));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid value"));
}

#[test]
fn help_is_available_without_initialising_the_renderer() {
    let output = Command::new(env!("CARGO_BIN_EXE_codestrand"))
        .arg("--help")
        .output()
        .expect("CLI can start");
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    assert!(help.contains("preview"));
    assert!(help.contains("export"));
    assert_eq!(output.stderr, [] as [u8; 0]);
}

#[test]
fn playback_help_describes_calendar_controls_and_rejects_old_flag() {
    for command in ["preview", "export"] {
        let output = Command::new(env!("CARGO_BIN_EXE_codestrand"))
            .args([command, "--help"])
            .output()
            .expect("CLI can start");
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).expect("help is UTF-8");
        assert!(help.contains("--no-diff-samples"));
        assert!(help.contains("--seconds-per-day"));
        assert!(help.contains("--max-idle-seconds"));
        assert!(!help.contains("--commit-seconds"));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_codestrand"))
        .args(["preview", "--commit-seconds", "0.5"])
        .output()
        .expect("CLI can start");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn invalid_timing_is_rejected_before_creating_output() {
    for (flag, value) in [
        ("--fps", "0"),
        ("--frames", "1"),
        ("--fps", "241"),
        ("--seconds-per-day", "0"),
        ("--seconds-per-day", "NaN"),
        ("--seconds-per-day", "86401"),
        ("--seconds-per-day", "inf"),
        ("--max-idle-seconds", "0"),
        ("--max-idle-seconds", "0.01"),
        ("--max-idle-seconds", "0.016666665"),
        ("--max-idle-seconds", "86401"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_codestrand"))
            .args(["export", ".", flag, value])
            .output()
            .expect("CLI can start");
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid value"));
    }
}

#[test]
fn export_refuses_an_existing_directory() {
    let output = Command::new(env!("CARGO_BIN_EXE_codestrand"))
        .args(["export", env!("CARGO_MANIFEST_DIR")])
        .output()
        .expect("CLI can start");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("create new export directory")
    );
    assert_eq!(output.stdout, [] as [u8; 0]);
}

#[test]
fn failed_import_leaves_no_export_directory() {
    let scratch = tempfile::tempdir().expect("temporary directory is available");
    let output_dir = scratch.path().join("frames");
    let output = Command::new(env!("CARGO_BIN_EXE_codestrand"))
        .arg("export")
        .arg(&output_dir)
        .arg("--repository")
        .arg(scratch.path())
        .output()
        .expect("CLI can start");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("open local repository"));
    assert!(!output_dir.exists());
}
