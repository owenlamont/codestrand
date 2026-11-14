use std::{
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde::Deserialize;

use crate::timeline::{Event, Kind};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_MANIFEST_ENTRIES: usize = 4096;
const MAX_PATH_BYTES: usize = 4096;
pub(super) const MAX_REPOSITORIES: usize = 30;

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum, serde::Serialize)]
pub(super) enum Boundary {
    #[default]
    None,
    Glass,
}

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum, serde::Serialize)]
pub(super) enum LabelPlane {
    #[default]
    Files,
    Raised,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Repository {
    pub path: PathBuf,
    pub label: Option<String>,
    #[serde(default)]
    pub archived: bool,
    pub ended: Option<String>,
}

impl Repository {
    pub(super) fn name(&self) -> String {
        self.label.clone().unwrap_or_else(|| {
            self.path.file_name().map_or_else(
                || self.path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    repositories: Vec<Repository>,
}

fn read_document(path: &Path) -> anyhow::Result<Document> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= MAX_MANIFEST_BYTES,
        "repositories file exceeds 1 MiB or is not a regular file"
    );
    let mut text = String::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_string(&mut text)?;
    anyhow::ensure!(
        u64::try_from(text.len())? <= MAX_MANIFEST_BYTES,
        "repositories file exceeds 1 MiB"
    );
    let document: Document =
        toml::from_str(&text).context("parse repositories file")?;
    anyhow::ensure!(
        document.repositories.len() <= MAX_MANIFEST_ENTRIES,
        "repositories file exceeds 4096 entries"
    );
    Ok(document)
}

pub(super) fn inputs(
    paths: &[PathBuf],
    file: Option<&Path>,
) -> anyhow::Result<Vec<Repository>> {
    let mut repositories: Vec<_> = paths
        .iter()
        .map(|path| {
            (
                path.clone(),
                Repository {
                    path: path.clone(),
                    label: None,
                    archived: false,
                    ended: None,
                },
            )
        })
        .collect();
    if let Some(file) = file {
        let document = read_document(file)
            .with_context(|| format!("read repositories file {}", file.display()))?;
        if document.repositories.is_empty() {
            bail!("repositories file must list at least one repository");
        }
        let parent = file.parent().unwrap_or(Path::new("."));
        repositories.extend(document.repositories.into_iter().map(|mut repository| {
            let spelling = repository.path.clone();
            repository.path = parent.join(repository.path);
            (spelling, repository)
        }));
    }
    let mut seen = std::collections::HashMap::new();
    let mut labels = std::collections::HashSet::new();
    let mut unique = Vec::new();
    for (spelling, mut repository) in repositories {
        if let Some(ended) = &repository.ended {
            parse_ended(ended)?;
        }
        anyhow::ensure!(
            repository.path.as_os_str().len() <= MAX_PATH_BYTES,
            "repository path exceeds 4096 bytes"
        );
        let path = repository.path.canonicalize().with_context(|| {
            format!("resolve repository {}", repository.path.display())
        })?;
        if let Some(first) = seen.insert(path.clone(), spelling.clone()) {
            bail!(
                "repository paths '{}' and '{}' are the same repository",
                first.display(),
                spelling.display()
            );
        }
        if repository
            .label
            .as_ref()
            .is_some_and(|label| label.trim().is_empty())
        {
            bail!("repository label must not be empty");
        }
        repository.path = path;
        if !labels.insert(repository.name()) {
            bail!("repository labels must be unique: {}", repository.name());
        }
        unique.push(repository);
    }
    Ok(unique)
}

pub(super) fn parse_ended(value: &str) -> anyhow::Result<i64> {
    anyhow::ensure!(
        value.len() == 10 && value.is_ascii(),
        "ended must be a UTC date YYYY-MM-DD"
    );
    let mut low = -719_162_i64;
    let mut high = 2_932_896_i64;
    while low < high {
        let middle = low + (high - low) / 2;
        if crate::model::date(middle * 86_400).as_str() < value {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    anyhow::ensure!(
        crate::model::date(low * 86_400) == value,
        "ended must be a valid UTC date YYYY-MM-DD"
    );
    Ok(low * 86_400)
}

pub(super) fn prefix(index: usize) -> String {
    format!("{index}/")
}

pub(super) fn namespace(events: &mut [Event], index: usize) {
    let prefix = prefix(index);
    for event in events {
        event.repository = index;
        for change in &mut event.changes {
            change.path.insert_str(0, &prefix);
            if let Kind::Moved { from } = &mut change.kind {
                from.insert_str(0, &prefix);
            }
        }
    }
}

pub(super) fn local_event(event: &Event, prefix: &str) -> Event {
    let mut event = event.clone();
    for change in &mut event.changes {
        change.path = change.path[prefix.len()..].to_owned();
        if let Kind::Moved { from } = &mut change.kind {
            *from = from[prefix.len()..].to_owned();
        }
    }
    event
}

pub(super) fn merge(histories: Vec<Vec<Event>>) -> Vec<Event> {
    let mut histories: Vec<_> = histories
        .into_iter()
        .map(|history| history.into_iter().peekable())
        .collect();
    let mut events = Vec::new();
    loop {
        let next = histories
            .iter_mut()
            .enumerate()
            .filter_map(|(index, history)| {
                history.peek().map(|event| (event.seconds, index))
            })
            .min();
        let Some((_, index)) = next else { break };
        events.push(histories[index].next().expect("selected head exists"));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ended_dates_reject_invalid_calendar_values_and_archival_is_opt_in() {
        for value in [
            "2026-02-30",
            "2026-13-01",
            "26-05-01",
            "2026-5-01",
            "arbitrary",
        ] {
            assert!(parse_ended(value).is_err(), "{value}");
        }
        assert_eq!(
            crate::model::date(parse_ended("2026-05-01").unwrap()),
            "2026-05-01"
        );
        let repository: Repository =
            toml::from_str("path='repo'\narchived=true\nended='2026-05-01'").unwrap();
        assert!(repository.archived);
        assert_eq!(repository.ended.as_deref(), Some("2026-05-01"));
    }

    #[test]
    fn distinct_repository_paths_pass() {
        let temp = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..30)
            .map(|i| temp.path().join(format!("repo-{i}")))
            .collect();
        for path in &paths {
            std::fs::create_dir(path).unwrap();
        }
        let repositories = inputs(&paths, None).unwrap();
        assert_eq!(repositories.len(), 30);
        assert_eq!(repositories[0].path, paths[0].canonicalize().unwrap());
        assert_eq!(repositories[0].name(), "repo-0");
    }

    #[test]
    fn duplicate_paths_report_both_original_spellings() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("repo");
        std::fs::create_dir(&path).unwrap();
        for other in [path.clone(), path.join("..").join("repo"), path.join("")] {
            let error = inputs(&[path.clone(), other.clone()], None)
                .unwrap_err()
                .to_string();
            assert_eq!(
                error,
                format!(
                    "repository paths '{}' and '{}' are the same repository",
                    path.display(),
                    other.display()
                )
            );
        }
    }

    /// Loads a manifest written beside an empty `repo` directory.
    fn manifest(document: &str) -> anyhow::Result<Vec<Repository>> {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("repo")).unwrap();
        let file = temp.path().join("repos.toml");
        std::fs::write(&file, document).unwrap();
        inputs(&[], Some(&file))
    }

    fn manifest_error(document: &str) -> String {
        format!("{:#}", manifest(document).unwrap_err())
    }

    #[test]
    fn manifest_limits_reject_a_four_mib_comment() {
        let document = format!(
            "[[repositories]]\npath='repo'\n#{}\n",
            "x".repeat(4 * 1024 * 1024)
        );
        assert!(manifest_error(&document).contains("exceeds 1 MiB"));
    }

    #[test]
    fn manifest_limits_reject_excess_entries_before_resolving_paths() {
        let document = "[[repositories]]\npath='missing'\n".repeat(4097);
        assert!(manifest_error(&document).contains("4096 entries"));
    }

    #[test]
    fn manifest_limits_reject_oversized_paths_before_resolving_them() {
        let document = format!("[[repositories]]\npath='{}'\n", "x".repeat(4097));
        assert!(manifest_error(&document).contains("path exceeds 4096 bytes"));
    }

    #[test]
    fn manifest_limits_allow_a_document_at_the_byte_cap() {
        let mut document = "[[repositories]]\npath='repo'\n#".to_owned();
        document.push_str(&"x".repeat(1024 * 1024 - document.len()));
        assert_eq!(manifest(&document).unwrap().len(), 1);
    }

    #[test]
    fn file_paths_are_relative_to_manifest_and_flag_duplicates_are_errors() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("repo")).unwrap();
        let file = temp.path().join("repos.toml");
        std::fs::write(
            &file,
            "[[repositories]]\npath = 'repo'\nlabel = 'Project'\n",
        )
        .unwrap();
        let repositories = inputs(&[], Some(&file)).unwrap();
        assert_eq!(
            repositories[0].path,
            temp.path().join("repo").canonicalize().unwrap()
        );
        assert_eq!(repositories[0].name(), "Project");
        let error = inputs(&[temp.path().join("repo")], Some(&file))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!(
                "repository paths '{}' and 'repo' are the same repository",
                temp.path().join("repo").display()
            )
        );
        std::fs::write(&file, "repositories = []").unwrap();
        assert!(inputs(&[], Some(&file)).is_err());
    }

    #[test]
    fn duplicate_manifest_entries_are_errors() {
        assert_eq!(
            manifest_error(
                "[[repositories]]\npath='repo'\n[[repositories]]\npath='./repo'\n"
            ),
            "repository paths 'repo' and './repo' are the same repository"
        );
    }

    #[test]
    fn merging_preserves_parent_before_older_child_and_namespaces_rename_sources() {
        let mut first = crate::timeline::synthetic();
        first.truncate(2);
        first[0].seconds = 4;
        first[1].seconds = 1;
        first[1].changes[0].kind = Kind::Moved {
            from: "old.rs".to_owned(),
        };
        let mut second = first.clone();
        namespace(&mut first, 0);
        namespace(&mut second, 1);
        let merged = merge(vec![first, second]);
        assert_eq!(
            merged.iter().map(|event| event.seconds).collect::<Vec<_>>(),
            [4, 1, 4, 1]
        );
        assert!(merged[0].changes[0].path.starts_with("0/"));
        assert!(merged[2].changes[0].path.starts_with("1/"));
        assert_eq!(
            merged[1].changes[0].kind,
            Kind::Moved {
                from: "0/old.rs".to_owned()
            }
        );
    }

    #[test]
    fn older_child_renames_and_modifies_the_parent_file_identity() {
        let mut parent = crate::timeline::synthetic().remove(0);
        parent.seconds = 100;
        parent.changes.truncate(1);
        parent.changes[0].path = "src/old.rs".to_owned();
        parent.changes[0].kind = Kind::Added;
        let mut child = parent.clone();
        child.seconds = 10;
        child.changes[0].path = "src/new.rs".to_owned();
        child.changes[0].kind = Kind::Moved {
            from: "src/old.rs".to_owned(),
        };
        let mut modified = child.clone();
        modified.seconds = 20;
        modified.changes[0].kind = Kind::Modified;
        let mut history = vec![parent, child, modified];
        namespace(&mut history, 0);
        let mut other = crate::timeline::synthetic();
        other.truncate(1);
        other[0].seconds = 50;
        namespace(&mut other, 1);
        let events = merge(vec![history, other]);
        let starts = crate::model::schedule::Schedule::new(&events, 0.5, 1.0).starts;
        let mut layout = crate::layout::Layout::scheduled_repositories(
            &events,
            &starts,
            &["First".to_owned(), "Second".to_owned()],
        );
        let mut identity = None;
        for event in events.iter().filter(|event| event.repository == 0) {
            let id = crate::model::apply(&mut layout, &event.changes[0], 0);
            assert_eq!(*identity.get_or_insert(id), id);
        }
        assert_eq!(layout.file(identity.unwrap()).unwrap().path, "0/src/new.rs");
        assert_eq!(layout.files().count(), 1);
    }

    #[test]
    fn duplicate_labels_are_rejected_for_distinct_repositories() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("first")).unwrap();
        std::fs::create_dir(temp.path().join("second")).unwrap();
        let file = temp.path().join("repos.toml");
        std::fs::write(&file, "[[repositories]]\npath='first'\nlabel='Same'\n[[repositories]]\npath='second'\nlabel='Same'\n").unwrap();
        assert!(
            inputs(&[], Some(&file))
                .unwrap_err()
                .to_string()
                .contains("labels must be unique")
        );
    }
}
