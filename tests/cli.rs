#![forbid(unsafe_code)]
#![deny(clippy::all, clippy::pedantic)]

use std::{fmt::Write as _, process::Command};

#[test]
fn repository_limit_is_checked_before_import_or_contributor_loading() {
    let scratch = tempfile::tempdir().unwrap();
    for index in 0..60 {
        std::fs::create_dir(scratch.path().join(format!("repo-{index}"))).unwrap();
    }
    let manifest = scratch.path().join("repos.toml");
    for flags in [0, 30] {
        let mut text = String::new();
        for i in flags..60 {
            writeln!(text, "[[repositories]]\npath='repo-{i}'").unwrap();
        }
        std::fs::write(&manifest, text).unwrap();
        let destination = scratch.path().join(format!("quality-{flags}.jsonl"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_codestrand"));
        command
            .arg("layout-quality")
            .arg(&destination)
            .arg("--repositories-file")
            .arg(&manifest)
            .arg("--contributors")
            .arg(scratch.path().join("missing.toml"))
            .arg("--no-cache");
        for index in 0..flags {
            command
                .arg("--repository")
                .arg(scratch.path().join(format!("repo-{index}")));
        }
        let output = command.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(stderr.contains("at most 30 repositories"), "{stderr}");
        assert!(!stderr.contains("repository="), "{stderr}");
        assert!(!destination.exists());
    }
}

#[test]
fn usage_error_preserves_clap_layout_and_exit_status() {
    let output = Command::new(env!("CARGO_BIN_EXE_codestrand"))
        .args(["inspect", "--bogus", "x"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let name = std::path::Path::new(env!("CARGO_BIN_EXE_codestrand"))
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "error: unexpected argument '--bogus' found\n\n  tip: to pass '--bogus' as a value, use '-- --bogus'\n\nUsage: {name} inspect [OPTIONS] <REPOSITORY>\n\nFor more information, try '--help'.\n"
        )
    );
    assert_eq!(output.stdout, [] as [u8; 0]);
}

#[test]
fn value_errors_print_clap_message_once() {
    let output = Command::new(env!("CARGO_BIN_EXE_codestrand"))
        .args(["export", "x", "--fps", "0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.matches("0 is not in 1..=240").count(), 1, "{stderr}");
    assert!(
        stderr.ends_with("For more information, try '--help'.\n"),
        "{stderr}"
    );
}

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
