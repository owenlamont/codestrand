use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

use crate::timeline::{Event, Kind};

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

pub(super) fn inputs(
    paths: &[PathBuf],
    file: Option<&Path>,
) -> anyhow::Result<Vec<Repository>> {
    let mut repositories: Vec<_> = paths
        .iter()
        .map(|path| Repository {
            path: path.clone(),
            label: None,
        })
        .collect();
    if let Some(file) = file {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("read repositories file {}", file.display()))?;
        let document: Document =
            toml::from_str(&text).context("parse repositories file")?;
        if document.repositories.is_empty() {
            bail!("repositories file must list at least one repository");
        }
        let parent = file.parent().unwrap_or(Path::new("."));
        repositories.extend(document.repositories.into_iter().map(|mut repository| {
            repository.path = parent.join(repository.path);
            repository
        }));
    }
    let mut seen = std::collections::HashSet::new();
    for repository in &mut repositories {
        let path = repository.path.canonicalize().with_context(|| {
            format!("resolve repository {}", repository.path.display())
        })?;
        if !seen.insert(path.clone()) {
            bail!(
                "repository listed more than once: {}",
                repository.path.display()
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
    }
    Ok(repositories)
}

pub(super) fn prefix(index: usize) -> String {
    format!("{index}/")
}

pub(super) fn namespace(events: &mut [Event], index: usize) {
    let prefix = prefix(index);
    for event in events {
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
    let mut events: Vec<_> = histories.into_iter().flatten().collect();
    events.sort_by_key(|event| event.seconds);
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_paths_are_relative_to_manifest_and_duplicate_clones_are_rejected() {
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
        assert!(inputs(&[temp.path().join("repo")], Some(&file)).is_err());
        std::fs::write(&file, "repositories = []").unwrap();
        assert!(inputs(&[], Some(&file)).is_err());
    }

    #[test]
    fn merging_orders_author_times_stably_and_namespaces_rename_sources() {
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
            [1, 1, 4, 4]
        );
        assert!(merged[0].changes[0].path.starts_with("0/"));
        assert!(merged[1].changes[0].path.starts_with("1/"));
        assert_eq!(
            merged[0].changes[0].kind,
            Kind::Moved {
                from: "0/old.rs".to_owned()
            }
        );
    }
}
