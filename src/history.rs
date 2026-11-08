use std::{
    collections::{BTreeMap, BTreeSet},
    ops::ControlFlow,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use gix::object::tree::diff::Change as TreeChange;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("open local repository {}", path.display())]
    Open { path: PathBuf, source: gix::Error },
    #[error("resolve revision {revision:?} to a commit")]
    Revision {
        revision: String,
        source: gix::Error,
    },
    #[error("import reachable history at {tip}")]
    History {
        tip: gix::ObjectId,
        source: anyhow::Error,
    },
}

#[derive(Serialize)]
pub(super) struct History {
    pub(super) tip: String,
    merge_policy: &'static str,
    rename_policy: &'static str,
    diff_policy: &'static str,
    pub(super) commits: Vec<Commit>,
}

#[derive(Serialize)]
pub(super) struct Commit {
    pub(super) id: String,
    pub(super) parents: Vec<String>,
    pub(super) author: Identity,
    committer: Identity,
    message: Vec<u8>,
    pub(super) changes: Vec<Change>,
}

#[derive(Serialize)]
pub(super) struct Identity {
    raw_signature: Vec<u8>,
    pub(super) name: Vec<u8>,
    pub(super) email: Vec<u8>,
    pub(super) seconds: i64,
    offset_seconds: i32,
}

#[derive(Serialize)]
struct Entry {
    id: String,
    mode: u16,
}

#[derive(Serialize)]
pub(super) struct Change {
    pub(super) path: Vec<u8>,
    pub(super) source_path: Option<Vec<u8>>,
    pub(super) kind: ChangeKind,
    before: Option<Entry>,
    after: Option<Entry>,
    pub(super) diff: BlobDiff,
}

#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(super) enum ChangeKind {
    Addition,
    Deletion,
    Modification,
    RenameCandidate,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum BlobDiff {
    Text {
        insertions: u32,
        removals: u32,
        hunks: Vec<Hunk>,
        samples: Vec<crate::diff_samples::Sample>,
    },
    Binary,
    Gitlink,
}

#[derive(Serialize)]
pub(super) struct Hunk {
    before_start: u32,
    before_end: u32,
    after_start: u32,
    after_end: u32,
}

pub(super) fn import(path: &Path, revision: &str) -> Result<History, Error> {
    let (repo, tip) = resolve(path, revision)?;
    import_resolved(&repo, tip)
}

pub(super) fn resolve(
    path: &Path,
    revision: &str,
) -> Result<(gix::Repository, gix::ObjectId), Error> {
    let mut repo = gix::open(path).map_err(|source| Error::Open {
        path: path.to_owned(),
        source,
    })?;
    repo.object_cache_size(64 * 1024 * 1024);
    let tip = repo
        .rev_parse_single(revision)
        .and_then(|id| id.object())
        .and_then(gix::Object::peel_to_commit)
        .map_err(|source| Error::Revision {
            revision: revision.to_owned(),
            source,
        })?
        .id;
    Ok((repo, tip))
}

pub(super) fn import_resolved(
    repo: &gix::Repository,
    tip: gix::ObjectId,
) -> Result<History, Error> {
    load(repo, tip).map_err(|source| Error::History { tip, source })
}

fn load(repo: &gix::Repository, tip: gix::ObjectId) -> anyhow::Result<History> {
    let mut commits = BTreeMap::new();
    let mut pending = vec![tip];
    while let Some(id) = pending.pop() {
        if commits.contains_key(&id) {
            continue;
        }
        let commit = repo.find_commit(id).with_context(|| {
            format!("read commit {id}; complete local history is required")
        })?;
        let parents: Vec<_> = commit.parent_ids().map(gix::Id::detach).collect();
        pending.extend(&parents);
        commits.insert(id, parents);
    }
    let mut children: BTreeMap<_, Vec<_>> = BTreeMap::new();
    let mut remaining = BTreeMap::new();
    let mut ready = BTreeSet::new();
    for (&id, parents) in &commits {
        let unique: BTreeSet<_> = parents.iter().copied().collect();
        remaining.insert(id, unique.len());
        if unique.is_empty() {
            ready.insert(id);
        }
        for parent in unique {
            children.entry(parent).or_default().push(id);
        }
    }
    let mut resources = repo.diff_resource_cache(
        gix::diff::blob::pipeline::Mode::ToGit,
        gix::diff::blob::pipeline::WorktreeRoots::default(),
    )?;
    let mut ordered = Vec::with_capacity(commits.len());
    while let Some(id) = ready.pop_first() {
        let commit = repo.find_commit(id)?;
        let parents = &commits[&id];
        let before = if let Some(parent) = parents.first() {
            repo.find_commit(*parent)?.tree()?
        } else {
            repo.empty_tree()
        };
        let after = commit.tree()?;
        let mut changes = Vec::new();
        before
            .changes()?
            .options(|options| {
                options
                    .track_path()
                    .track_rewrites(Some(gix::diff::Rewrites {
                        copies: None,
                        percentage: None,
                        limit: 1000,
                        track_empty: false,
                    }));
            })
            .for_each_to_obtain_tree_with_cache(&after, &mut resources, |change| {
                changes.extend(records(change).into_iter().flatten());
                Ok(ControlFlow::Continue(()))
            })?;
        resources.clear_resource_cache();
        changes.sort_by(|a, b| {
            (&a.path, &a.source_path, &a.kind).cmp(&(&b.path, &b.source_path, &b.kind))
        });
        for change in &mut changes {
            change.diff = blob_diff(repo, change).with_context(|| {
                format!("diff path {:?} in commit {id}", change.path)
            })?;
        }
        let decoded = commit.decode()?;
        ordered.push(Commit {
            id: id.to_string(),
            parents: parents.iter().map(ToString::to_string).collect(),
            author: identity(decoded.author)?,
            committer: identity(decoded.committer)?,
            message: decoded.message.to_vec(),
            changes,
        });
        for child in children.get(&id).into_iter().flatten() {
            let count = remaining
                .get_mut(child)
                .context("reachable child has a parent count")?;
            *count -= 1;
            if *count == 0 {
                ready.insert(*child);
            }
        }
    }
    if ordered.len() != commits.len() {
        bail!("commit graph contains a cycle");
    }
    Ok(History {
        tip: tip.to_string(),
        merge_policy: "first-parent",
        rename_policy: "exact-content-nonempty-no-copies",
        diff_policy: "raw-blobs-histogram-nul-in-first-8000-bytes",
        commits: ordered,
    })
}

fn identity(raw: &[u8]) -> anyhow::Result<Identity> {
    let signature = gix::actor::SignatureRef::from_bytes(raw)?;
    let time = signature.time()?;
    Ok(Identity {
        raw_signature: raw.to_vec(),
        name: signature.name.to_vec(),
        email: signature.email.to_vec(),
        seconds: time.seconds,
        offset_seconds: time.offset,
    })
}

fn entry(id: gix::Id<'_>, mode: gix::objs::tree::EntryMode) -> Entry {
    Entry {
        id: id.to_string(),
        mode: mode.value(),
    }
}

fn records(change: TreeChange<'_, '_, '_>) -> [Option<Change>; 2] {
    if let TreeChange::Rewrite {
        source_location,
        source_entry_mode,
        source_relation,
        source_id,
        location,
        entry_mode,
        relation,
        id,
        ..
    } = change
        && source_id.is_empty_blob()
    {
        [
            record(TreeChange::Deletion {
                location: source_location,
                entry_mode: source_entry_mode,
                relation: source_relation,
                id: source_id,
            }),
            record(TreeChange::Addition {
                location,
                entry_mode,
                relation,
                id,
            }),
        ]
    } else {
        [record(change), None]
    }
}

fn record(change: TreeChange<'_, '_, '_>) -> Option<Change> {
    let (path, source_path, kind, before, after) = match change {
        TreeChange::Addition {
            location,
            entry_mode,
            id,
            ..
        } if !entry_mode.is_tree() => (
            location.to_vec(),
            None,
            ChangeKind::Addition,
            None,
            Some(entry(id, entry_mode)),
        ),
        TreeChange::Deletion {
            location,
            entry_mode,
            id,
            ..
        } if !entry_mode.is_tree() => (
            location.to_vec(),
            None,
            ChangeKind::Deletion,
            Some(entry(id, entry_mode)),
            None,
        ),
        TreeChange::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } if !entry_mode.is_tree() && !previous_entry_mode.is_tree() => (
            location.to_vec(),
            None,
            ChangeKind::Modification,
            Some(entry(previous_id, previous_entry_mode)),
            Some(entry(id, entry_mode)),
        ),
        TreeChange::Rewrite {
            source_location,
            source_entry_mode,
            source_id,
            location,
            entry_mode,
            id,
            ..
        } if !entry_mode.is_tree() && !source_entry_mode.is_tree() => (
            location.to_vec(),
            Some(source_location.to_vec()),
            ChangeKind::RenameCandidate,
            Some(entry(source_id, source_entry_mode)),
            Some(entry(id, entry_mode)),
        ),
        _ => return None,
    };
    Some(Change {
        path,
        source_path,
        kind,
        before,
        after,
        diff: BlobDiff::Gitlink,
    })
}

fn blob_diff(repo: &gix::Repository, change: &Change) -> anyhow::Result<BlobDiff> {
    if change
        .before
        .iter()
        .chain(&change.after)
        .any(|entry| entry.mode == 0o160_000)
    {
        return Ok(BlobDiff::Gitlink);
    }
    let load = |entry: &Option<Entry>| -> anyhow::Result<Vec<u8>> {
        match entry {
            Some(entry) => Ok(repo
                .find_blob(gix::ObjectId::from_hex(entry.id.as_bytes())?)?
                .data
                .clone()),
            None => Ok(Vec::new()),
        }
    };
    let before = load(&change.before)?;
    let after = load(&change.after)?;
    if before
        .iter()
        .take(8000)
        .chain(after.iter().take(8000))
        .any(|&byte| byte == 0)
    {
        return Ok(BlobDiff::Binary);
    }
    let input =
        gix::diff::blob::InternedInput::new(before.as_slice(), after.as_slice());
    let diff = gix::diff::blob::diff_with_slider_heuristics(
        gix::diff::blob::Algorithm::Histogram,
        &input,
    );
    let mut samples = Vec::new();
    for hunk in diff.hunks() {
        for token in &input.before[hunk.before.start as usize..hunk.before.end as usize]
        {
            crate::diff_samples::consider(&mut samples, input.interner[*token], false);
        }
        for token in &input.after[hunk.after.start as usize..hunk.after.end as usize] {
            crate::diff_samples::consider(&mut samples, input.interner[*token], true);
        }
    }
    let hunks = diff
        .hunks()
        .map(|hunk| Hunk {
            before_start: hunk.before.start,
            before_end: hunk.before.end,
            after_start: hunk.after.start,
            after_end: hunk.after.end,
        })
        .collect();
    Ok(BlobDiff::Text {
        insertions: diff.count_additions(),
        removals: diff.count_removals(),
        hunks,
        samples,
    })
}
