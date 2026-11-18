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
        source: anyhow::Error,
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
    pub(super) truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    truncation_reason: Option<String>,
}

#[derive(Clone, Copy, Default, clap::Args, Serialize)]
#[group(id = "HistoryOptions")]
pub(super) struct Options {
    /// Accept completed commits when a history budget is exhausted.
    #[arg(long)]
    allow_truncated_history: bool,
}

impl Options {
    fn accept(self, history: History) -> anyhow::Result<History> {
        if let Some(reason) = &history.truncation_reason {
            if !self.allow_truncated_history {
                bail!(
                    "history truncated: {reason}; use --allow-truncated-history to accept completed commits only"
                );
            }
            eprintln!(
                "history truncated: {}; showing completed commits only",
                crate::terminal_escape(reason)
            );
        }
        Ok(history)
    }
}

#[derive(Clone, Copy)]
struct Limits {
    commits: usize,
    entries: u64,
    tree_entries: u64,
    retained_bytes: u64,
    blob_bytes: u64,
    lines: u64,
    work: u64,
    object_bytes: u64,
    line_pairs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            commits: 1_000_000,
            entries: 2_000_000,
            tree_entries: 2_000_000,
            retained_bytes: 512 * 1024 * 1024,
            blob_bytes: 8 * 1024 * 1024 * 1024,
            lines: 20_000_000,
            work: 32 * 1024 * 1024 * 1024,
            object_bytes: OBJECT_ALLOCATION_BYTES,
            line_pairs: 1_000_000_000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{name} budget reached ({limit}); {remedy}")]
struct Exhausted {
    name: &'static str,
    limit: u64,
    remedy: &'static str,
}

fn exhausted(name: &'static str, limit: u64, cumulative: bool) -> anyhow::Error {
    Exhausted {
        name,
        limit,
        remedy: if cumulative {
            "use --allow-truncated-history to play completed commits"
        } else {
            "fixed security ceiling"
        },
    }
    .into()
}

struct Budget {
    limits: Limits,
    blob_bytes: u64,
    lines: u64,
    work: u64,
    changes: u64,
    retained_bytes: u64,
    playback_tip: Option<String>,
    playback_total: usize,
    playback_completed: usize,
    trees: BTreeMap<gix::ObjectId, (u64, usize, usize, u64)>,
}

impl Budget {
    fn new(limits: Limits) -> Self {
        Self {
            limits,
            blob_bytes: 0,
            lines: 0,
            work: 0,
            changes: 0,
            retained_bytes: 0,
            playback_tip: None,
            playback_total: 0,
            playback_completed: 0,
            trees: BTreeMap::new(),
        }
    }

    fn charge(&mut self, amount: u64) -> anyhow::Result<()> {
        self.work = self.work.saturating_add(amount);
        if self.work > self.limits.work {
            return Err(exhausted("cumulative work", self.limits.work, false));
        }
        Ok(())
    }

    fn retain(&mut self, bytes: u64) -> anyhow::Result<()> {
        self.retained_bytes = self.retained_bytes.saturating_add(bytes);
        if self.retained_bytes > self.limits.retained_bytes {
            return Err(exhausted(
                "retained input bytes",
                self.limits.retained_bytes,
                true,
            ));
        }
        Ok(())
    }

    fn change(&mut self, change: &TreeChange<'_, '_, '_>) -> anyhow::Result<()> {
        self.changes += 1;
        self.charge(1)?;
        if self.changes > self.limits.entries {
            return Err(exhausted("changed entries", self.limits.entries, true));
        }
        let source_bytes = match change {
            TreeChange::Rewrite {
                source_location, ..
            } => source_location.len(),
            _ => 0,
        };
        self.retain((change.location().len() + source_bytes + 512) as u64)
    }

    fn object(
        &mut self,
        repo: &gix::Repository,
        id: gix::ObjectId,
    ) -> anyhow::Result<u64> {
        self.charge(1)?;
        let size = repo.find_header(id)?.size();
        if size > self.limits.object_bytes {
            return Err(exhausted("object bytes", self.limits.object_bytes, false));
        }
        self.charge(size)?;
        Ok(size)
    }

    fn tree(
        &mut self,
        repo: &gix::Repository,
        id: gix::ObjectId,
        depth: usize,
    ) -> anyhow::Result<(u64, usize, usize, u64)> {
        self.charge(1)?;
        if depth > 256 {
            return Err(exhausted("tree depth", 256, false));
        }
        if let Some(&summary) = self.trees.get(&id) {
            return Ok(summary);
        }
        let bytes = self.object(repo, id)?;
        self.retain(bytes + 128)?;
        let tree = repo.find_tree(id)?;
        let mut summary = (0u64, 0usize, 0usize, 0u64);
        for entry in tree.iter() {
            self.charge(1)?;
            let entry = entry?;
            let child = if entry.mode().is_tree() {
                self.tree(repo, entry.object_id(), depth + 1)?
            } else {
                (0, 0, 0, 0)
            };
            summary.0 = summary.0.saturating_add(1 + child.0);
            summary.1 = summary.1.max(1 + child.1);
            summary.2 = summary.2.max(entry.filename().len() + 1 + child.2);
            summary.3 = summary.3.saturating_add(child.3).saturating_add(
                (entry.filename().len() as u64 + 1).saturating_mul(1 + child.0),
            );
            for (value, limit, name) in [
                (summary.0, self.limits.tree_entries, "single-tree entries"),
                (summary.1 as u64, 256, "tree depth"),
                (summary.2 as u64, 16_384, "path bytes"),
                (summary.3, 64 * 1024 * 1024, "single-tree path bytes"),
            ] {
                if value > limit {
                    return Err(exhausted(name, limit, false));
                }
            }
        }
        self.trees.insert(id, summary);
        Ok(summary)
    }
}

#[derive(Serialize)]
pub(super) struct Commit {
    pub(super) id: String,
    pub(super) parents: Vec<String>,
    pub(super) author: Identity,
    committer: Identity,
    pub(super) message: Vec<u8>,
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
    Skipped,
}

#[derive(Serialize)]
pub(super) struct Hunk {
    before_start: u32,
    before_end: u32,
    after_start: u32,
    after_end: u32,
}

pub(super) fn resolve(
    path: &Path,
    revision: &str,
) -> Result<(gix::Repository, gix::ObjectId), Error> {
    let mut repo = gix::open_opts(
        path,
        gix::open::Options::default().config_overrides([format!(
            "gitoxide.objects.allocLimit={OBJECT_ALLOCATION_BYTES}"
        )]),
    )
    .map_err(|source| Error::Open {
        path: path.to_owned(),
        source,
    })?;
    repo.object_cache_size(64 * 1024 * 1024);
    let tip = bounded_revision(&repo, revision).map_err(|source| Error::Revision {
        revision: revision.to_owned(),
        source,
    })?;
    Ok((repo, tip))
}

pub(super) fn import_with_options(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    options: Options,
) -> Result<History, Error> {
    load_with_limits(repo, tip, Limits::default())
        .and_then(|history| options.accept(history))
        .map_err(|source| Error::History { tip, source })
}

const MAX_REFERENCE_HOPS: usize = 64;
const OBJECT_ALLOCATION_BYTES: u64 = 16 * 1024 * 1024;

fn reference_id(
    repo: &gix::Repository,
    name: &str,
) -> anyhow::Result<Option<gix::ObjectId>> {
    let Some(mut reference) = repo.try_find_reference(name)? else {
        return Ok(None);
    };
    let mut seen = BTreeSet::new();
    for _ in 0..MAX_REFERENCE_HOPS {
        if !seen.insert(reference.name().to_owned()) {
            bail!("symbolic reference cycle");
        }
        match reference.target() {
            gix::refs::TargetRef::Object(id) => return Ok(Some(id.to_owned())),
            gix::refs::TargetRef::Symbolic(name) => {
                let Some(next) = repo.try_find_reference(name)? else {
                    return Ok(None);
                };
                reference = next;
            }
        }
    }
    bail!("symbolic reference chain exceeds 64 hops")
}

fn peel_commit(
    repo: &gix::Repository,
    mut id: gix::ObjectId,
    budget: &mut Budget,
) -> anyhow::Result<gix::ObjectId> {
    let mut seen = BTreeSet::new();
    for hop in 0..=MAX_REFERENCE_HOPS {
        if !seen.insert(id) {
            bail!("tag object cycle");
        }
        let header = match repo.find_header(id) {
            Ok(header) => header,
            Err(error) if error.is_resource_exhausted() => return Ok(id),
            Err(error) => return Err(error.into()),
        };
        if header.size() > budget.limits.object_bytes {
            return Ok(id);
        }
        budget.object(repo, id)?;
        match header.kind() {
            gix::objs::Kind::Commit => return Ok(id),
            gix::objs::Kind::Tag => {
                if hop == MAX_REFERENCE_HOPS {
                    bail!("tag chain exceeds 64 hops");
                }
                id = repo.find_tag(id)?.target_id()?.detach();
            }
            _ => bail!("revision target is not a commit or annotated tag"),
        }
    }
    unreachable!("the last hop returns an error")
}

fn revision_anchor(
    repo: &gix::Repository,
    name: &str,
) -> anyhow::Result<gix::ObjectId> {
    if name.len() == 40
        && let Ok(id) = gix::ObjectId::from_hex(name.as_bytes())
    {
        return Ok(id);
    }
    if let Some(id) = reference_id(repo, if name == "@" { "HEAD" } else { name })? {
        return Ok(id);
    }
    let prefix = gix::hash::Prefix::from_hex(name)?;
    match repo.objects.lookup_prefix(prefix, None)? {
        Some(Ok(id)) => Ok(id),
        Some(Err(())) => bail!("ambiguous object prefix"),
        None => bail!("revision reference or object does not exist"),
    }
}

fn bounded_revision(
    repo: &gix::Repository,
    revision: &str,
) -> anyhow::Result<gix::ObjectId> {
    if revision.is_empty() || revision.len() > 4096 {
        bail!("revision must contain 1–4096 bytes");
    }
    let split = revision.find(['^', '~']).unwrap_or(revision.len());
    let mut budget = Budget::new(Limits::default());
    let mut id = peel_commit(
        repo,
        revision_anchor(repo, &revision[..split])?,
        &mut budget,
    )?;
    let mut suffix = &revision[split..];
    let mut steps = 0usize;
    while !suffix.is_empty() {
        if let Some(rest) = suffix
            .strip_prefix("^{}")
            .or_else(|| suffix.strip_prefix("^{commit}"))
        {
            suffix = rest;
            continue;
        }
        let operator = suffix.as_bytes()[0];
        if !matches!(operator, b'^' | b'~') {
            bail!("unsupported revision expression; use a ref, object ID, ^N or ~N");
        }
        suffix = &suffix[1..];
        let digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
        let count = if digits == 0 {
            1
        } else {
            suffix[..digits].parse::<usize>()?
        };
        suffix = &suffix[digits..];
        steps = steps.saturating_add(count.max(1));
        if steps > budget.limits.commits {
            bail!("revision traversal exceeds 1000000 steps");
        }
        for _ in 0..if operator == b'~' {
            count
        } else {
            usize::from(count != 0)
        } {
            budget.object(repo, id)?;
            let commit = repo.find_commit(id)?;
            let index = if operator == b'^' { count - 1 } else { 0 };
            let parent = commit
                .parent_ids()
                .nth(index)
                .context("revision parent does not exist")?
                .detach();
            id = peel_commit(repo, parent, &mut budget)?;
        }
    }
    Ok(id)
}

fn load_with_limits(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    limits: Limits,
) -> anyhow::Result<History> {
    let mut budget = Budget::new(limits);
    let mut history = History {
        tip: tip.to_string(),
        merge_policy: "first-parent",
        rename_policy: "exact-content-nonempty-no-copies",
        diff_policy: "raw-blobs-histogram-nul-in-first-8000-bytes",
        commits: Vec::new(),
        truncated: false,
        truncation_reason: None,
    };
    match bounded_load(repo, tip, &mut budget, &mut history.commits) {
        Ok(()) => (),
        Err(error)
            if error.is::<Exhausted>()
                || error.chain().any(|cause| {
                    cause
                        .downcast_ref::<gix::Error>()
                        .is_some_and(gix::Error::is_resource_exhausted)
                }) =>
        {
            history.truncated = true;
            history.truncation_reason = Some(error.downcast_ref::<Exhausted>().map_or_else(
                || format!("object allocation budget reached ({OBJECT_ALLOCATION_BYTES} bytes); fixed security ceiling: {error:#}"),
                ToString::to_string,
            ));
            if let Some(tip) = &budget.playback_tip {
                history.tip.clone_from(tip);
            }
            let boundary = history
                .commits
                .iter()
                .find(|commit| Some(&commit.id) == budget.playback_tip.as_ref())
                .map_or_else(
                    || "no complete first-parent playback commit was read".to_owned(),
                    |commit| {
                        format!(
                            "first-parent history was read through commit {} of {}, {} dated {} UTC, inclusive; commits with no file changes are not shown",
                            budget.playback_completed,
                            budget.playback_total,
                            commit.id,
                            crate::model::date(commit.author.seconds),
                        )
                    },
                );
            if let Some(reason) = &mut history.truncation_reason {
                reason.push_str("; ");
                reason.push_str(&boundary);
            }
        }
        Err(error) => return Err(error),
    }
    Ok(history)
}

fn reachable(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    budget: &mut Budget,
) -> anyhow::Result<BTreeMap<gix::ObjectId, Vec<gix::ObjectId>>> {
    let mut commits = BTreeMap::new();
    let mut pending = vec![tip];
    while let Some(id) = pending.pop() {
        if commits.contains_key(&id) {
            continue;
        }
        if commits.len() >= budget.limits.commits {
            return Err(exhausted(
                "reachable commits",
                budget.limits.commits as u64,
                true,
            ));
        }
        budget.object(repo, id).with_context(|| {
            format!("read commit {id}; complete local history is required")
        })?;
        let commit = repo.find_commit(id).with_context(|| {
            format!("read commit {id}; complete local history is required")
        })?;
        let mut parents = Vec::new();
        for parent in commit.parent_ids() {
            budget.charge(1)?;
            if pending.len() + parents.len() >= budget.limits.commits {
                return Err(exhausted(
                    "pending parents",
                    budget.limits.commits as u64,
                    true,
                ));
            }
            parents.push(parent.detach());
        }
        pending.extend(&parents);
        budget.retain((parents.len() * 64 + 256) as u64)?;
        commits.insert(id, parents);
    }
    Ok(commits)
}

fn first_parent_chain(
    commits: &BTreeMap<gix::ObjectId, Vec<gix::ObjectId>>,
    tip: gix::ObjectId,
    budget: &mut Budget,
) -> anyhow::Result<BTreeSet<gix::ObjectId>> {
    let mut first_parents = BTreeSet::new();
    let mut current = Some(tip);
    while let Some(id) = current {
        budget.charge(1)?;
        if !first_parents.insert(id) {
            bail!("commit graph contains a cycle");
        }
        current = commits[&id].first().copied();
    }
    Ok(first_parents)
}

fn bounded_load(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    budget: &mut Budget,
    ordered: &mut Vec<Commit>,
) -> anyhow::Result<()> {
    let commits = reachable(repo, tip, budget)?;
    let first_parents = first_parent_chain(&commits, tip, budget)?;
    budget.playback_total = first_parents.len();
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
    let mut resources = bounded_resources(repo, budget)?;
    while let Some(id) = ready.pop_first() {
        budget.charge(1)?;
        let commit = repo.find_commit(id)?;
        let parents = &commits[&id];
        let before = if let Some(parent) = parents.first() {
            let before_id = repo.find_commit(*parent)?.tree_id()?.detach();
            budget.tree(repo, before_id, 0)?;
            repo.find_tree(before_id)?
        } else {
            repo.empty_tree()
        };
        let after_id = commit.tree_id()?.detach();
        budget.tree(repo, after_id, 0)?;
        let after = repo.find_tree(after_id)?;
        let mut changes = Vec::new();
        let mut exhaustion = None;
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
                if let Err(error) = budget.change(&change) {
                    exhaustion = Some(error);
                    return Ok(ControlFlow::Break(()));
                }
                changes.extend(records(change).into_iter().flatten());
                Ok(ControlFlow::Continue(()))
            })?;
        if let Some(error) = exhaustion {
            return Err(error);
        }
        resources.clear_resource_cache();
        changes.sort_by(|a, b| {
            (&a.path, &a.source_path, &a.kind).cmp(&(&b.path, &b.source_path, &b.kind))
        });
        for change in &mut changes {
            change.diff =
                bounded_blob_diff(repo, change, budget).with_context(|| {
                    format!("diff path {:?} in commit {id}", change.path)
                })?;
        }
        let decoded = commit.decode()?;
        budget.retain(commit.data.len() as u64 * 3)?;
        ordered.push(Commit {
            id: id.to_string(),
            parents: parents.iter().map(ToString::to_string).collect(),
            author: identity(decoded.author)?,
            committer: identity(decoded.committer)?,
            message: decoded.message.to_vec(),
            changes,
        });
        if first_parents.contains(&id) {
            budget.playback_tip = Some(id.to_string());
            budget.playback_completed += 1;
        }
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
    Ok(())
}

fn bounded_resources(
    repo: &gix::Repository,
    budget: &mut Budget,
) -> anyhow::Result<gix::diff::blob::Platform> {
    let tree = if let Some(head) = reference_id(repo, "HEAD")? {
        let head = peel_commit(repo, head, budget)?;
        budget.object(repo, head)?;
        let tree = repo.find_commit(head)?.tree_id()?.detach();
        budget.tree(repo, tree, 0)?;
        Some(tree)
    } else {
        None
    };
    let index: gix::worktree::IndexPersistedOrInMemory = match repo.try_index()? {
        Some(index) => index.into(),
        None => match tree {
            Some(tree) => repo.index_from_tree(&tree)?.into(),
            None => gix::index::File::from_state(
                gix::index::State::new(repo.object_hash()),
                repo.index_path(),
            )
            .into(),
        },
    };
    Ok(gix::diff::resource_cache(
        repo,
        gix::diff::blob::pipeline::Mode::ToGit,
        repo.attributes_only(
            &index,
            gix::worktree::stack::state::attributes::Source::IdMapping,
        )?
        .detach(),
        gix::diff::blob::pipeline::WorktreeRoots::default(),
    )?)
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

fn load_blobs(
    repo: &gix::Repository,
    change: &Change,
) -> anyhow::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let load = |entry: &Option<Entry>| -> anyhow::Result<Vec<u8>> {
        match entry {
            Some(entry) => {
                let id = gix::ObjectId::from_hex(entry.id.as_bytes())?;
                Ok(repo.find_blob(id)?.data.clone())
            }
            None => Ok(Vec::new()),
        }
    };
    let loaded = load(&change.before)
        .and_then(|before| load(&change.after).map(|after| (before, after)));
    match loaded {
        Ok(blobs) => Ok(Some(blobs)),
        Err(error)
            if error.chain().any(|cause| {
                cause
                    .downcast_ref::<gix::Error>()
                    .is_some_and(gix::Error::is_resource_exhausted)
            }) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn bounded_blob_diff(
    repo: &gix::Repository,
    change: &Change,
    budget: &mut Budget,
) -> anyhow::Result<BlobDiff> {
    if change
        .before
        .iter()
        .chain(&change.after)
        .any(|entry| entry.mode == 0o160_000)
    {
        return Ok(BlobDiff::Gitlink);
    }
    let mut skipped = false;
    for entry in change.before.iter().chain(&change.after) {
        let id = gix::ObjectId::from_hex(entry.id.as_bytes())?;
        budget.charge(1)?;
        let size = match repo.find_header(id) {
            Ok(header) => header.size(),
            Err(error) if error.is_resource_exhausted() => {
                budget.charge(budget.limits.object_bytes)?;
                return Ok(BlobDiff::Skipped);
            }
            Err(error) => return Err(error.into()),
        };
        budget.charge(size)?;
        budget.blob_bytes = budget.blob_bytes.saturating_add(size);
        skipped |= size > budget.limits.object_bytes
            || budget.blob_bytes > budget.limits.blob_bytes;
    }
    if skipped {
        return Ok(BlobDiff::Skipped);
    }
    let Some((before, after)) = load_blobs(repo, change)? else {
        budget.charge(budget.limits.object_bytes)?;
        return Ok(BlobDiff::Skipped);
    };
    if before
        .iter()
        .take(8000)
        .chain(after.iter().take(8000))
        .any(|&byte| byte == 0)
    {
        return Ok(BlobDiff::Binary);
    }
    let lines = |bytes: &[u8]| bytes.split(|&byte| byte == b'\n').count() as u64;
    let before_lines = lines(&before);
    let after_lines = lines(&after);
    budget.lines = budget.lines.saturating_add(before_lines + after_lines);
    budget.charge(before_lines + after_lines)?;
    if budget.lines > budget.limits.lines
        || before_lines.saturating_mul(after_lines) > budget.limits.line_pairs
    {
        return Ok(BlobDiff::Skipped);
    }
    let input =
        gix::diff::blob::InternedInput::new(before.as_slice(), after.as_slice());
    let diff = gix::diff::blob::diff_with_slider_heuristics(
        gix::diff::blob::Algorithm::Histogram,
        &input,
    );
    budget.retain(diff.hunks().count() as u64 * 16 + 512)?;
    let mut samples = Vec::new();
    for hunk in diff.hunks().filter(|_| {
        !crate::diff_samples::sensitive_path(&change.path)
            && !change
                .source_path
                .as_deref()
                .is_some_and(crate::diff_samples::sensitive_path)
    }) {
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

#[cfg(test)]
mod security_tests {
    use super::*;
    use clap::Parser;

    fn import(path: &Path, revision: &str) -> Result<History, Error> {
        let (repo, tip) = resolve(path, revision)?;
        import_with_options(&repo, tip, Options::default())
    }

    #[test]
    fn truncation_opt_in_is_shared_and_budget_option_is_removed() {
        for command in ["inspect", "preview", "export"] {
            let mut args = vec!["codestrand", command];
            if command != "preview" {
                args.push("unused");
            }
            let mut allowed = args.clone();
            allowed.push("--allow-truncated-history");
            assert!(crate::Cli::try_parse_from(allowed).is_ok());
            for value in [
                "1",
                "2",
                "10",
                "100",
                "0",
                "101",
                "none",
                "18446744073709551615",
            ] {
                let mut configured = args.clone();
                configured.extend(["--history-budget", value]);
                assert!(crate::Cli::try_parse_from(configured).is_err());
            }
        }
    }

    #[test]
    fn packed_oversized_blob_keeps_complete_history() {
        let mut bytes = vec![b'x'; 17 * 1024 * 1024];
        let (root, repo, first) = hostile_history(0, &bytes);
        bytes.truncate(15 * 1024 * 1024);
        bytes[0] = b'y';
        let blob = repo.write_blob(&bytes).unwrap().detach();
        let commit = repo.find_commit(first).unwrap();
        let mut commit = commit.decode().unwrap().to_owned().unwrap();
        let tree = repo.find_tree(commit.tree).unwrap();
        let mut tree = tree.decode().unwrap().to_owned();
        tree.entries[0].oid = blob;
        commit.tree = repo.write_object(&tree).unwrap().detach();
        commit.parents = [first].into_iter().collect();
        let tip = repo.write_object(&commit).unwrap().detach();
        std::fs::write(root.path().join(".git/HEAD"), format!("{tip}\n")).unwrap();
        let mut command = std::process::Command::new("git");
        for (key, _) in std::env::vars_os()
            .filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
        {
            command.env_remove(key);
        }
        let packed = command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .arg("-C")
            .arg(root.path())
            .args(["repack", "-ad"])
            .output()
            .unwrap();
        assert!(
            packed.status.success(),
            "{}",
            String::from_utf8_lossy(&packed.stderr)
        );
        let (bounded_repo, _) = resolve(root.path(), "HEAD").unwrap();
        assert_eq!(
            bounded_repo.find_header(blob).unwrap().size(),
            15 * 1024 * 1024
        );
        assert!(
            bounded_repo
                .find_blob(blob)
                .is_err_and(|error| error.is_resource_exhausted())
        );
        let change = Change {
            path: b"file".to_vec(),
            source_path: None,
            kind: ChangeKind::Addition,
            before: None,
            after: Some(Entry {
                id: blob.to_string(),
                mode: 0o100_644,
            }),
            diff: BlobDiff::Gitlink,
        };
        let mut budget = Budget::new(Limits::default());
        assert!(matches!(
            bounded_blob_diff(&bounded_repo, &change, &mut budget).unwrap(),
            BlobDiff::Skipped
        ));
        assert!(budget.work > budget.limits.object_bytes);
        let history = import(root.path(), "HEAD").unwrap();
        assert!(!history.truncated);
        assert_eq!(history.commits.len(), 2);
        for commit in &history.commits {
            assert_eq!(commit.changes.len(), 1);
            assert!(matches!(commit.changes[0].diff, BlobDiff::Skipped));
        }
    }

    #[test]
    fn every_history_refusal_names_its_budget_and_remedy() {
        let (_root, repo, tip) = hostile_history(0, b"first\nsecond\n");
        for (limits, name, remedy) in [
            (
                Limits {
                    commits: 0,
                    ..Limits::default()
                },
                "reachable commits",
                "--allow-truncated-history",
            ),
            (
                Limits {
                    entries: 0,
                    ..Limits::default()
                },
                "changed entries",
                "--allow-truncated-history",
            ),
            (
                Limits {
                    retained_bytes: 0,
                    ..Limits::default()
                },
                "retained input bytes",
                "--allow-truncated-history",
            ),
            (
                Limits {
                    work: 0,
                    ..Limits::default()
                },
                "cumulative work",
                "fixed security ceiling",
            ),
            (
                Limits {
                    object_bytes: 0,
                    ..Limits::default()
                },
                "object bytes",
                "fixed security ceiling",
            ),
            (
                Limits {
                    tree_entries: 0,
                    ..Limits::default()
                },
                "single-tree entries",
                "fixed security ceiling",
            ),
        ] {
            let history = load_with_limits(&repo, tip, limits).unwrap();
            let error = Options::default()
                .accept(history)
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains(name), "{error}");
            assert!(error.contains(remedy), "{error}");
            assert!(error.contains("--allow-truncated-history"), "{error}");
            let history = load_with_limits(&repo, tip, limits).unwrap();
            let accepted = Options {
                allow_truncated_history: true,
            }
            .accept(history)
            .unwrap();
            assert!(accepted.truncated);
        }
    }

    #[test]
    fn single_tree_ceiling_is_fixed() {
        let (_root, repo, tip) = hostile_history(20, b"ok\n");
        let error = import_with_options(&repo, tip, Options::default())
            .err()
            .unwrap();
        let message = format!("{:#}", anyhow::Error::from(error));
        assert!(
            message.contains("single-tree entries budget reached (2000000)"),
            "{message}"
        );
        assert!(message.contains("fixed security ceiling"));
    }

    #[test]
    fn expanded_path_limits_are_fixed() {
        for (levels, filename, reason) in [
            (0, vec![b'x'; 16_384], "path bytes"),
            (16, vec![b'x'; 1024], "single-tree path bytes"),
        ] {
            let (_root, repo, tip) = hostile_path(levels, b"ok\n", &filename);
            let error = import_with_options(&repo, tip, Options::default())
                .err()
                .unwrap();
            let message = format!("{:#}", anyhow::Error::from(error));
            assert!(message.contains(reason), "{message}");
        }
    }

    #[test]
    fn pending_parent_refusal_names_history_option() {
        let (_root, repo, first) = hostile_history(0, b"ok\n");
        let commit = repo.find_commit(first).unwrap();
        let mut commit = commit.decode().unwrap().to_owned().unwrap();
        commit.parents = [first, first].into_iter().collect();
        let tip = repo.write_object(&commit).unwrap().detach();
        let history = load_with_limits(
            &repo,
            tip,
            Limits {
                commits: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        let error = Options::default()
            .accept(history)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("pending parents budget reached (1)"));
        assert!(error.contains("--allow-truncated-history"));
    }

    #[test]
    fn default_budget_imports_complete_small_history() {
        let (_root, repo, tip) = hostile_history(0, b"ok\n");
        assert!(
            !import_with_options(&repo, tip, Options::default())
                .unwrap()
                .truncated
        );
    }

    #[test]
    fn expensive_diff_keeps_changes_and_later_commits() {
        let (_root, repo, first) = hostile_history(0, b"a\nb\nc\nd\n");
        let mut parent = first;
        for content in [b"e\nf\ng\nh\n".as_slice(), b"later\n"] {
            let blob = repo.write_blob(content).unwrap().detach();
            let tree = repo
                .write_object(&gix::objs::Tree {
                    entries: vec![gix::objs::tree::Entry {
                        mode: gix::objs::tree::EntryKind::Blob.into(),
                        filename: "file".into(),
                        oid: blob,
                    }],
                })
                .unwrap()
                .detach();
            let commit = repo.find_commit(parent).unwrap();
            let mut commit = commit.decode().unwrap().to_owned().unwrap();
            commit.tree = tree;
            commit.parents = [parent].into_iter().collect();
            parent = repo.write_object(&commit).unwrap().detach();
        }
        let history = load_with_limits(
            &repo,
            parent,
            Limits {
                line_pairs: 20,
                ..Limits::default()
            },
        )
        .unwrap();
        assert!(!history.truncated);
        assert_eq!(history.commits.len(), 3);
        let change = &history.commits[1].changes[0];
        assert_eq!(change.path, b"file");
        assert!(matches!(change.kind, ChangeKind::Modification));
        assert_eq!(
            serde_json::to_value(&change.diff).unwrap()["kind"],
            "skipped"
        );
        assert!(matches!(
            history.commits[2].changes[0].diff,
            BlobDiff::Text { .. }
        ));
        let events = crate::timeline::from_history(&history);
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].changes[0].insertions, 0);
        assert_eq!(events[1].changes[0].removals, 0);
        assert_eq!(events[1].changes[0].samples.len(), 0);
    }

    fn hostile_history(
        levels: usize,
        blob: &[u8],
    ) -> (tempfile::TempDir, gix::Repository, gix::ObjectId) {
        hostile_path(levels, blob, b"file")
    }

    fn hostile_path(
        levels: usize,
        blob: &[u8],
        filename: &[u8],
    ) -> (tempfile::TempDir, gix::Repository, gix::ObjectId) {
        let root = crate::cache::test_directory().unwrap();
        let repo = gix::init(root.path()).unwrap();
        let blob = repo.write_blob(blob).unwrap().detach();
        let mut tree = repo
            .write_object(&gix::objs::Tree {
                entries: vec![gix::objs::tree::Entry {
                    mode: gix::objs::tree::EntryKind::Blob.into(),
                    filename: filename.into(),
                    oid: blob,
                }],
            })
            .unwrap()
            .detach();
        for _ in 0..levels {
            tree = repo
                .write_object(&gix::objs::Tree {
                    entries: ["a", "b"]
                        .into_iter()
                        .map(|name| gix::objs::tree::Entry {
                            mode: gix::objs::tree::EntryKind::Tree.into(),
                            filename: name.into(),
                            oid: tree,
                        })
                        .collect(),
                })
                .unwrap()
                .detach();
        }
        let signature = gix::actor::Signature {
            name: "Author".into(),
            email: "a@b".into(),
            time: gix::date::Time {
                seconds: 1_700_000_000,
                offset: 0,
            },
        };
        let tip = repo
            .write_object(&gix::objs::Commit {
                tree,
                parents: std::iter::empty().collect(),
                author: signature.clone(),
                committer: signature,
                encoding: None,
                message: "fixture".into(),
                extra_headers: Vec::new(),
            })
            .unwrap()
            .detach();
        (root, repo, tip)
    }

    #[test]
    fn expanded_tree_and_inflated_diff_stop_cleanly() {
        for (levels, blob, truncated) in [
            (12, b"ok\n".to_vec(), true),
            (
                0,
                b"line with repeated compressible content and enough bytes per line\n"
                    .repeat(131_072),
                false,
            ),
        ] {
            let (_root, repo, tip) = hostile_history(levels, &blob);
            let history = load_with_limits(
                &repo,
                tip,
                Limits {
                    tree_entries: 4096,
                    blob_bytes: 4 * 1024 * 1024,
                    ..Limits::default()
                },
            )
            .unwrap();
            assert_eq!(history.truncated, truncated);
            if !truncated {
                assert!(matches!(
                    history.commits[0].changes[0].diff,
                    BlobDiff::Skipped
                ));
            }
        }
    }

    #[test]
    fn truncation_names_the_completed_playback_boundary() {
        let (_root, repo, first) = hostile_history(0, b"first\n");
        let object = repo.find_commit(first).unwrap();
        let mut next = object.decode().unwrap().to_owned().unwrap();
        next.parents = [first].into_iter().collect();
        let blob = repo.write_blob(vec![b'x'; 20_000]).unwrap().detach();
        let mut tree = repo
            .find_tree(next.tree)
            .unwrap()
            .decode()
            .unwrap()
            .to_owned();
        tree.entries[0].oid = blob;
        next.tree = repo.write_object(&tree).unwrap().detach();
        let tip = repo.write_object(&next).unwrap().detach();
        let limits = Limits {
            work: 10_000,
            ..Limits::default()
        };
        let history = load_with_limits(&repo, tip, limits).unwrap();
        assert_eq!(history.tip, first.to_string());
        assert_eq!(history.commits.len(), 1);
        let boundary = format!(
            "first-parent history was read through commit 1 of 2, {first} dated 2023-11-14 UTC, inclusive"
        );
        assert!(
            history
                .truncation_reason
                .as_ref()
                .unwrap()
                .contains(&boundary)
        );
        let refusal = Options::default()
            .accept(history)
            .err()
            .unwrap()
            .to_string();
        assert!(refusal.contains(&boundary), "{refusal}");
        let accepted = Options {
            allow_truncated_history: true,
        }
        .accept(load_with_limits(&repo, tip, limits).unwrap())
        .unwrap();
        assert!(accepted.truncation_reason.unwrap().contains(&boundary));
        let empty = load_with_limits(
            &repo,
            tip,
            Limits {
                commits: 0,
                ..limits
            },
        )
        .unwrap();
        assert!(
            empty
                .truncation_reason
                .unwrap()
                .contains("no complete first-parent playback commit")
        );
    }

    #[test]
    fn each_independent_budget_truncates_instead_of_failing() {
        let (_root, repo, tip) = hostile_history(0, b"first\nsecond\n");
        for limits in [
            Limits {
                commits: 0,
                ..Limits::default()
            },
            Limits {
                entries: 0,
                ..Limits::default()
            },
            Limits {
                work: 0,
                ..Limits::default()
            },
            Limits {
                object_bytes: 0,
                ..Limits::default()
            },
        ] {
            assert!(load_with_limits(&repo, tip, limits).unwrap().truncated);
        }
        assert!(
            !import_with_options(&repo, tip, Options::default())
                .unwrap()
                .truncated
        );
    }

    #[test]
    fn oversized_blob_keeps_addition_rename_and_deletion() {
        let (_root, repo, first) = hostile_history(0, &vec![b'x'; 4096]);
        let commit = repo.find_commit(first).unwrap();
        let mut commit = commit.decode().unwrap().to_owned().unwrap();
        let tree = repo.find_tree(commit.tree).unwrap();
        let mut tree = tree.decode().unwrap().to_owned();
        tree.entries[0].filename = "renamed".into();
        commit.tree = repo.write_object(&tree).unwrap().detach();
        commit.parents = [first].into_iter().collect();
        let renamed = repo.write_object(&commit).unwrap().detach();
        commit.tree = repo.empty_tree().id;
        commit.parents = [renamed].into_iter().collect();
        let deleted = repo.write_object(&commit).unwrap().detach();
        let history = load_with_limits(
            &repo,
            deleted,
            Limits {
                object_bytes: 1024,
                ..Limits::default()
            },
        )
        .unwrap();
        assert!(!history.truncated);
        assert_eq!(history.commits.len(), 3);
        assert!(matches!(
            history.commits[0].changes[0].kind,
            ChangeKind::Addition
        ));
        let rename = &history.commits[1].changes[0];
        assert!(matches!(rename.kind, ChangeKind::RenameCandidate));
        assert_eq!(rename.path, b"renamed");
        assert_eq!(rename.source_path.as_deref(), Some(b"file".as_slice()));
        assert!(matches!(
            history.commits[2].changes[0].kind,
            ChangeKind::Deletion
        ));
        assert!(history.commits.iter().all(|commit| {
            commit
                .changes
                .iter()
                .all(|change| matches!(change.diff, BlobDiff::Skipped))
        }));
    }

    #[test]
    fn skipped_detail_remains_bounded_by_global_work() {
        let (_root, repo, tip) = hostile_history(0, b"first\nsecond\n");
        let history = import_with_options(&repo, tip, Options::default()).unwrap();
        let change = &history.commits[0].changes[0];
        for limits in [
            Limits {
                blob_bytes: 0,
                ..Limits::default()
            },
            Limits {
                lines: 0,
                ..Limits::default()
            },
            Limits {
                object_bytes: 0,
                ..Limits::default()
            },
            Limits {
                line_pairs: 0,
                ..Limits::default()
            },
        ] {
            let mut budget = Budget::new(Limits {
                work: 100,
                ..limits
            });
            assert!(matches!(
                bounded_blob_diff(&repo, change, &mut budget).unwrap(),
                BlobDiff::Skipped
            ));
            assert!(budget.work >= 13);
            for _ in 0..100 {
                if let Err(error) = bounded_blob_diff(&repo, change, &mut budget) {
                    assert!(error.is::<Exhausted>());
                    break;
                }
            }
            assert!(budget.work > budget.limits.work);
        }
        for limits in [
            Limits {
                blob_bytes: 0,
                ..Limits::default()
            },
            Limits {
                lines: 0,
                ..Limits::default()
            },
        ] {
            let history = load_with_limits(&repo, tip, limits).unwrap();
            assert!(!history.truncated);
            assert!(matches!(
                history.commits[0].changes[0].diff,
                BlobDiff::Skipped
            ));
        }
    }

    #[test]
    fn deep_shared_tree_stops_before_gix_expansion() {
        let (_root, repo, tip) = hostile_history(512, b"ok\n");
        let error = import_with_options(&repo, tip, Options::default())
            .err()
            .unwrap();
        assert!(format!("{:#}", anyhow::Error::from(error)).contains("tree depth"));
    }

    #[test]
    fn head_expansion_is_checked_before_diff_cache_construction() {
        let (root, _repo, tip) = hostile_history(12, b"ok\n");
        std::fs::write(root.path().join(".git/HEAD"), format!("{tip}\n")).unwrap();
        let (repo, tip) = resolve(root.path(), "HEAD").unwrap();
        assert!(
            load_with_limits(
                &repo,
                tip,
                Limits {
                    tree_entries: 4096,
                    ..Limits::default()
                }
            )
            .unwrap()
            .truncated
        );
    }

    #[test]
    fn oversized_revision_stops_cleanly() {
        let (root, repo, _) = hostile_history(0, b"ok\n");
        let oversized = repo
            .write_blob(vec![b'x'; 17 * 1024 * 1024])
            .unwrap()
            .detach();
        assert!(import(root.path(), &oversized.to_string()).is_err());
    }

    // Unix index paths permit invalid UTF-8, so only Windows produces this dependency error.
    #[cfg(windows)]
    #[test]
    fn dependency_path_error_is_escaped() {
        let (root, _repo, tip) = hostile_path(0, b"ok\n", b"\x1b[31mfile\xff.txt");
        std::fs::write(root.path().join(".git/HEAD"), format!("{tip}\n")).unwrap();
        let error = import(root.path(), &tip.to_string())
            .err()
            .expect("Windows rejects the index path");
        let error = anyhow::Error::from(error);
        let raw = format!("{error:#}");
        assert!(raw.contains('\u{1b}'));
        assert!(!crate::terminal_escape(&raw).contains('\u{1b}'));
    }
}
