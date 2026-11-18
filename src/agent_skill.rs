//! Keeps the published `skills/codestrand/SKILL.md` in step with the CLI, so a renamed,
//! hidden or removed option fails the build instead of misleading an agent.

use std::collections::BTreeSet;
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

/// Returns each `codestrand` command in the skill's `sh` fences as its words, with
/// backslash continuations joined.
fn commands(skill: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut in_shell = false;
    let mut pending = String::new();
    for line in skill.lines() {
        if line.starts_with("```") {
            in_shell = line == "```sh";
            continue;
        }
        if !in_shell {
            continue;
        }
        pending.push_str(line.trim_end_matches('\\'));
        if line.ends_with('\\') {
            continue;
        }
        let command = std::mem::take(&mut pending);
        if command.starts_with("codestrand ") {
            commands.push(command.split_whitespace().map(str::to_owned).collect());
        }
    }
    commands
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
fn skill_commands_parse_and_name_their_repositories() {
    let Some(skill) = skill() else { return };
    let commands = commands(&skill);
    assert!(
        commands.len() >= 5,
        "found only {} commands",
        commands.len()
    );
    for words in commands {
        let line = words.join(" ");
        assert!(!line.contains(['"', '\'']), "quoting is not parsed: {line}");
        if let Err(error) = Cli::try_parse_from(&words) {
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
