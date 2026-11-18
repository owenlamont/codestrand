use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use clap::Parser;

use crate::{Cli, Command, Source, repositories};

fn config(directory: &Path, text: &str) -> PathBuf {
    let path = directory.join("codestrand.toml");
    std::fs::write(&path, text).unwrap();
    path
}

fn arguments(command: &[&str], config: &Path, rest: &[&str]) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = ["codestrand"]
        .iter()
        .chain(command)
        .map(OsString::from)
        .collect();
    arguments.extend([OsString::from("--config"), config.into()]);
    arguments.extend(rest.iter().map(OsString::from));
    super::arguments(arguments).unwrap()
}

fn preview(config: &Path, rest: &[&str]) -> Source {
    let Some(Command::Preview { source, .. }) =
        Cli::try_parse_from(arguments(&["preview"], config, rest))
            .unwrap()
            .command
    else {
        panic!("preview")
    };
    source
}

fn error(command: &[&str], config: &Path) -> String {
    let arguments: Vec<OsString> = ["codestrand"]
        .iter()
        .chain(command)
        .map(OsString::from)
        .chain([OsString::from("--config"), config.into()])
        .collect();
    format!("{:#}", super::arguments(arguments).unwrap_err())
}

#[test]
fn file_sets_options_and_command_line_flags_override_them() {
    let scratch = tempfile::tempdir().unwrap();
    let file = config(
        scratch.path(),
        "gutter = 8\nseconds-per-day = 2\nno-cache = true\n\
         start-date = 2026-05-01\nrepository = ['one', 'two']\n",
    );
    let source = preview(&file, &[]);
    assert!((source.gutter - 8.0).abs() < f32::EPSILON);
    assert!((source.seconds_per_day - 2.0).abs() < f32::EPSILON);
    assert!(source.no_cache);
    assert_eq!(
        source.start_date,
        Some(crate::start_date("2026-05-01").unwrap())
    );
    assert_eq!(
        source.repository,
        [scratch.path().join("one"), scratch.path().join("two")]
    );

    let source = preview(&file, &["--gutter", "2", "--repository", "three"]);
    assert!((source.gutter - 2.0).abs() < f32::EPSILON);
    assert!((source.seconds_per_day - 2.0).abs() < f32::EPSILON);
    assert_eq!(source.repository, [PathBuf::from("three")]);
}

#[test]
fn one_file_serves_preview_and_export() {
    let scratch = tempfile::tempdir().unwrap();
    let file = config(
        scratch.path(),
        "fps = 24\nvideo = 'out.mp4'\nffmpeg = 'ffmpeg'\n",
    );
    let Some(Command::Export { fps, .. }) =
        Cli::try_parse_from(arguments(&["export", "frames"], &file, &[]))
            .unwrap()
            .command
    else {
        panic!("export")
    };
    assert_eq!(fps, 24);
    preview(&file, &[]);

    let mut video = OsString::from("--video=");
    video.push(scratch.path().join("out.mp4"));
    let spliced = arguments(&["export", "frames"], &file, &[]);
    assert!(spliced.contains(&video), "{spliced:?}");
    assert!(spliced.contains(&"--ffmpeg=ffmpeg".into()), "{spliced:?}");

    let file = config(scratch.path(), "fps = 0\n");
    assert!(error(&["preview"], &file).contains("key `fps`"));
}

#[test]
fn a_required_companion_flag_can_come_from_the_file() {
    let scratch = tempfile::tempdir().unwrap();
    let file = config(scratch.path(), "video = 'out.mp4'\n");
    Cli::try_parse_from(arguments(&["export", "frames"], &file, &["--ffmpeg", "x"]))
        .unwrap();
}

#[test]
fn unknown_keys_are_errors_that_name_the_key_and_file() {
    let scratch = tempfile::tempdir().unwrap();
    for key in ["bogus", "test-input", "config", "help", "directory"] {
        let file = config(scratch.path(), &format!("{key} = 'x'\n"));
        let message = error(&["preview"], &file);
        assert!(
            message.contains(&format!("unknown key `{key}`")),
            "{message}"
        );
        assert!(message.contains("codestrand.toml"), "{message}");
    }
}

#[test]
fn invalid_values_are_errors_that_name_the_key() {
    let scratch = tempfile::tempdir().unwrap();
    for (text, reason) in [
        ("gutter = 99", "between 0 and 20"),
        ("caption-style = 'nope'", "expected one of"),
        ("collapse-directories = false", "expected on or off"),
        ("no-cache = 'yes'", "must be true or false"),
        ("gutter = [1]", "takes a single value"),
        ("start-date = '2026-02-30'", "YYYY-MM-DD"),
    ] {
        let file = config(scratch.path(), text);
        let message = error(&["preview"], &file);
        let key = text.split(' ').next().unwrap();
        assert!(message.contains(&format!("key `{key}`")), "{message}");
        assert!(message.contains(reason), "{message}");
    }
}

#[test]
fn the_repository_list_can_live_in_the_file() {
    let scratch = tempfile::tempdir().unwrap();
    std::fs::create_dir(scratch.path().join("repo")).unwrap();
    let file = config(
        scratch.path(),
        "gutter = 8\n[[repositories]]\npath = 'repo'\n",
    );
    let source = preview(&file, &[]);
    assert_eq!(source.repositories_file.as_deref(), Some(file.as_path()));
    let listed =
        repositories::inputs_with(&[], Some(&file), repositories::Keys::AlsoOptions)
            .unwrap();
    assert_eq!(listed.len(), 1);
    let strict = repositories::inputs(&[], Some(&file)).unwrap_err();
    assert!(
        format!("{strict:#}").contains("unknown key `gutter`"),
        "{strict:#}"
    );

    let source = preview(&file, &["--repository", "elsewhere"]);
    assert_eq!(source.repositories_file, None);

    let file = config(
        scratch.path(),
        "repositories-file = 'other.toml'\n[[repositories]]\npath = 'repo'\n",
    );
    assert!(error(&["preview"], &file).contains("not both"));
}
