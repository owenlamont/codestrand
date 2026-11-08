use std::{collections::HashMap, fmt::Write};

use crate::history::{BlobDiff, ChangeKind, Commit, History};

// Bump when the snapshot format, history import policies or timeline conversion change.
pub(super) const CACHE_VERSION: u32 = 6;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) enum Kind {
    #[default]
    Added,
    Deleted,
    Modified,
    Moved {
        from: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct FileChange {
    pub path: String,
    pub kind: Kind,
    pub insertions: u32,
    pub removals: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<crate::diff_samples::Sample>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Identity {
    pub name: Vec<u8>,
    pub email: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Event {
    #[serde(skip)]
    pub repository: usize,
    pub author: Identity,
    pub commit: String,
    #[serde(skip)]
    pub contributor: Option<crate::contributors::Contributor>,
    pub seconds: i64,
    pub changes: Vec<FileChange>,
}

/// Events along the first-parent chain, whose first-parent diffs transition each
/// playback tree to the next; side-branch work arrives with the merge that brings it in.
pub(super) fn from_history(history: &History) -> Vec<Event> {
    let by_id: HashMap<&str, &Commit> = history
        .commits
        .iter()
        .map(|commit| (commit.id.as_str(), commit))
        .collect();
    let mut chain = Vec::new();
    let mut current = by_id.get(history.tip.as_str()).copied();
    while let Some(commit) = current {
        chain.push(commit);
        current = commit
            .parents
            .first()
            .and_then(|parent| by_id.get(parent.as_str()).copied());
    }
    chain
        .into_iter()
        .rev()
        .filter(|commit| !commit.changes.is_empty())
        .map(|commit| Event {
            author: Identity {
                name: commit.author.name.clone(),
                email: commit.author.email.clone(),
            },
            repository: 0,
            contributor: None,
            commit: commit.id.clone(),
            seconds: commit.author.seconds,
            changes: sampled_changes(commit),
        })
        .collect()
}

fn sampled_changes(commit: &Commit) -> Vec<FileChange> {
    let mut budget = crate::diff_samples::PER_COMMIT;
    commit
        .changes
        .iter()
        .map(|change| {
            let (insertions, removals) = match change.diff {
                BlobDiff::Text {
                    insertions,
                    removals,
                    ..
                } => (insertions, removals),
                BlobDiff::Binary | BlobDiff::Gitlink => (1, 1),
                BlobDiff::Skipped => (0, 0),
            };
            let kind = match (&change.kind, &change.source_path) {
                (ChangeKind::Addition, _) => Kind::Added,
                (ChangeKind::Deletion, _) => Kind::Deleted,
                (ChangeKind::RenameCandidate, Some(from)) => {
                    Kind::Moved { from: path(from) }
                }
                (ChangeKind::Modification | ChangeKind::RenameCandidate, _) => {
                    Kind::Modified
                }
            };
            FileChange {
                path: path(&change.path),
                kind,
                insertions,
                removals,
                samples: match &change.diff {
                    BlobDiff::Text { samples, .. } => {
                        let chosen: Vec<_> =
                            samples.iter().take(budget).cloned().collect();
                        budget -= chosen.len();
                        chosen
                    }
                    _ => Vec::new(),
                },
            }
        })
        .collect()
}

/// Decodes a raw Git path injectively: invalid UTF-8 bytes become `\xNN` and backslashes
/// double, so distinct byte paths keep distinct identities.
fn path(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        for character in chunk.valid().chars() {
            if character == '\\' {
                text.push_str("\\\\");
            } else {
                text.push(character);
            }
        }
        for byte in chunk.invalid() {
            write!(text, "\\x{byte:02x}").expect("writing to a String cannot fail");
        }
    }
    text
}

fn synthetic_event(author: &str, changes: Vec<FileChange>, index: usize) -> Event {
    Event {
        author: Identity {
            name: author.as_bytes().to_vec(),
            email: format!("{author}@example.invalid").into_bytes(),
        },
        repository: 0,
        contributor: None,
        commit: String::new(),
        seconds: 1_700_000_000 + 86_400 * i64::try_from(index).unwrap_or(0),
        changes,
    }
}
/// A small scripted history exercising growth, moves, deletion and a crowded commit.
pub(super) fn synthetic() -> Vec<Event> {
    let mut events = Vec::new();
    let mut push = |author: &str, changes: Vec<FileChange>| {
        events.push(synthetic_event(author, changes, events.len()));
    };
    let add = |path: &str, lines| change(path, Kind::Added, lines, 0);
    let edit =
        |path: &str, added, removed| change(path, Kind::Modified, added, removed);
    push(
        "Ada",
        vec![
            add("Cargo.toml", 20),
            add("README.md", 40),
            add("src/main.rs", 60),
        ],
    );
    push(
        "Ada",
        vec![add("src/model.rs", 110), edit("src/main.rs", 8, 2)],
    );
    push(
        "Grace",
        vec![add(".github/workflows/ci.yml", 45), add("tests/cli.rs", 40)],
    );
    push(
        "Ada",
        vec![
            add("src/scene.rs", 300),
            add("src/layout.rs", 80),
            edit("src/main.rs", 4, 1),
        ],
    );
    push(
        "Grace",
        vec![edit("README.md", 12, 30), add("docs/guide.md", 70)],
    );
    push(
        "Ada",
        vec![
            add("src/export.rs", 150),
            add("src/old.rs", 30),
            edit("Cargo.toml", 3, 0),
        ],
    );
    push(
        "Linus",
        vec![
            edit("src/scene.rs", 40, 25),
            edit("src/model.rs", 9, 9),
            add("tests/render.rs", 85),
        ],
    );
    let crowd: Vec<_> = (0..64)
        .map(|index| add(&format!("tests/fixtures/case_{index:02}.yaml"), 12))
        .chain([edit("tests/cli.rs", 30, 2)])
        .collect();
    push("Grace", crowd);
    push(
        "Ada",
        vec![
            change(
                "src/scene/layout.rs",
                Kind::Moved {
                    from: "src/layout.rs".to_owned(),
                },
                10,
                4,
            ),
            change("src/old.rs", Kind::Deleted, 0, 30),
            add("src/scene/labels.rs", 55),
        ],
    );
    push(
        "Linus",
        vec![
            edit("src/scene.rs", 5, 60),
            edit(".github/workflows/ci.yml", 6, 2),
        ],
    );
    let sweep: Vec<_> = (0..64)
        .step_by(2)
        .map(|index| edit(&format!("tests/fixtures/case_{index:02}.yaml"), 1, 1))
        .collect();
    push("Grace", sweep);
    push(
        "Ada",
        vec![
            edit("README.md", 20, 4),
            change("docs/guide.md", Kind::Deleted, 0, 70),
            add("docs/user/guide.md", 90),
        ],
    );
    events
}

fn change(path: &str, kind: Kind, insertions: u32, removals: u32) -> FileChange {
    FileChange {
        path: path.to_owned(),
        kind,
        insertions,
        removals,
        samples: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use super::*;

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_AUTHOR_NAME", "Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.invalid")
            .env("GIT_COMMITTER_NAME", "Author")
            .env("GIT_COMMITTER_EMAIL", "author@example.invalid")
            .output()
            .expect("Git can start");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn merges_discarding_a_branch_leave_no_ghost_files() {
        let scratch = tempfile::tempdir().expect("temporary directory is available");
        let path = scratch.path();
        git(path, &["init", "--initial-branch=main"]);
        fs::write(path.join("kept.txt"), "kept\n").expect("fixture is writable");
        git(path, &["add", "-A"]);
        git(path, &["commit", "-m", "root"]);
        git(path, &["checkout", "-b", "side"]);
        fs::write(path.join("ghost.txt"), "side\n").expect("fixture is writable");
        git(path, &["add", "-A"]);
        git(path, &["commit", "-m", "side"]);
        git(path, &["checkout", "main"]);
        git(
            path,
            &["merge", "--no-ff", "-s", "ours", "side", "-m", "discard"],
        );
        let history = crate::history::import(path, "HEAD").expect("fixture imports");
        let paths: Vec<_> = from_history(&history)
            .into_iter()
            .flat_map(|event| event.changes)
            .map(|change| change.path)
            .collect();
        assert_eq!(paths, ["kept.txt"]);
    }

    #[test]
    fn raw_paths_keep_distinct_identities() {
        let decoded = [
            path(b"\xff.txt"),
            path(b"\xfe.txt"),
            path(b"\\xff.txt"),
            path("é.txt".as_bytes()),
        ];
        assert_eq!(decoded[0], "\\xff.txt");
        assert_eq!(decoded[2], "\\\\xff.txt");
        assert_eq!(decoded[3], "é.txt");
        assert_ne!(decoded[0], decoded[1]);
    }
}
