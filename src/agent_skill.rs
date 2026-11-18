//! Keeps the published `skills/codestrand/SKILL.md` in step with the CLI, so a renamed,
//! hidden or removed option fails the build instead of misleading an agent.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::Path;

use clap::{CommandFactory, Parser};

use crate::Cli;

/// Returns the skill, or `None` in the packaged crate, which leaves `skills/` out.
fn skill() -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/codestrand/SKILL.md");
    path.is_file()
        .then(|| std::fs::read_to_string(path).expect("SKILL.md is readable"))
}

/// Collects every `--long-flag` token, ignoring a `--` that continues a word.
fn long_flags(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let mut flags = BTreeSet::new();
    for (index, _) in text.match_indices("--") {
        if index > 0 && bytes[index - 1].is_ascii_alphanumeric() {
            continue;
        }
        let name: String = text[index + 2..]
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || *c == '-')
            .collect();
        if name.len() >= 2 && !name.starts_with('-') && !name.ends_with('-') {
            flags.insert(format!("--{name}"));
        }
    }
    flags
}

/// Returns the lines of every fenced block whose info string is exactly `language`.
fn fences<'a>(skill: &'a str, language: &str) -> Vec<Vec<&'a str>> {
    let mut blocks = Vec::new();
    let mut open: Option<(bool, Vec<&str>)> = None;
    for line in skill.lines() {
        match (line.strip_prefix("```"), &mut open) {
            (Some(info), None) => open = Some((info == language, Vec::new())),
            (Some(_), Some(_)) => {
                if let Some((true, block)) = open.take() {
                    blocks.push(block);
                }
            }
            (None, Some((_, block))) => block.push(line),
            (None, None) => {}
        }
    }
    blocks
}

/// Returns each `codestrand` command in the skill's `sh` fences as its words, with
/// backslash continuations joined.
fn commands(skill: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    for block in fences(skill, "sh") {
        let mut pending = String::new();
        for line in block {
            pending.push_str(line.trim_end_matches('\\'));
            if line.ends_with('\\') {
                continue;
            }
            let command = std::mem::take(&mut pending);
            if command.starts_with("codestrand ") {
                commands.push(command.split_whitespace().map(str::to_owned).collect());
            }
        }
    }
    commands
}

/// Writes each `toml` fence to `directory` under the name its first line gives, as in
/// `# codestrand.toml; ...`, and returns the names.
fn write_toml_examples(skill: &str, directory: &Path) -> Vec<String> {
    fences(skill, "toml")
        .into_iter()
        .map(|block| {
            let name = block[0]
                .strip_prefix("# ")
                .and_then(|comment| comment.split(';').next())
                .expect("a TOML example opens with `# NAME.toml;`");
            std::fs::write(directory.join(name), block.join("\n")).unwrap();
            name.to_owned()
        })
        .collect()
}

#[test]
fn skill_flags_are_in_visible_help() {
    let Some(skill) = skill() else { return };
    let cli = Cli::command();
    let visible: BTreeSet<String> = std::iter::once(&cli)
        .chain(
            cli.get_subcommands()
                .filter(|command| !command.is_hide_set()),
        )
        .flat_map(clap::Command::get_arguments)
        .filter(|argument| !argument.is_hide_set())
        .filter_map(clap::Arg::get_long)
        .map(|long| format!("--{long}"))
        .chain(["--help".to_owned(), "--version".to_owned()])
        .collect();
    let missing: Vec<_> = long_flags(&skill).difference(&visible).cloned().collect();
    assert!(
        missing.is_empty(),
        "SKILL.md names flags absent from help: {missing:?}"
    );
}

#[test]
fn skill_shows_the_export_manifest_header() {
    let Some(skill) = skill() else { return };
    assert!(skill.contains(crate::export::MANIFEST_HEADER));
}

#[test]
fn skill_commands_parse_with_their_config_files_and_name_their_repositories() {
    let Some(skill) = skill() else { return };
    let directory = tempfile::tempdir().unwrap();
    let examples = write_toml_examples(&skill, directory.path());
    let commands = commands(&skill);
    assert!(
        commands.len() >= 5,
        "found only {} commands",
        commands.len()
    );
    for words in commands {
        let line = words.join(" ");
        assert!(!line.contains(['"', '\'']), "quoting is not parsed: {line}");
        let arguments = words
            .iter()
            .map(|word| {
                if examples.contains(word) {
                    directory.path().join(word).into_os_string()
                } else {
                    OsString::from(word)
                }
            })
            .collect();
        let parsed = crate::config::arguments(arguments)
            .map_err(|error| error.to_string())
            .and_then(|arguments| {
                Cli::try_parse_from(arguments).map_err(|error| error.to_string())
            });
        if let Err(error) = parsed {
            panic!("`{line}` does not parse:\n{error}");
        }
        if words[1] != "inspect" {
            assert!(
                words.iter().any(|word| {
                    ["--repository", "--repositories-file", "--config"]
                        .contains(&word.as_str())
                }),
                "`{line}` must name its repository rather than rely on the current directory"
            );
        }
    }
}

#[test]
fn commands_reject_an_option_missing_what_it_requires() {
    let skill =
        "```sh\ncodestrand export out --repository . \\\n  --ffmpeg ffmpeg\n```\n";
    let words = commands(skill).pop().expect("one command");
    assert_eq!(words.len(), 7);
    assert!(Cli::try_parse_from(words).is_err());
}

#[test]
fn fences_skip_blocks_in_other_languages() {
    let skill = "```text\ncodestrand nope\n```\n```sh\ncodestrand inspect .\n```\n";
    assert_eq!(commands(skill), [["codestrand", "inspect", "."]]);
}
