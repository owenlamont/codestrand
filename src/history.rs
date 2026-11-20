use std::{
    collections::{BTreeMap, BTreeSet},
    ops::ControlFlow,
    path::{Path, PathBuf},
};

use gix::object::tree::diff::Change as TreeChange;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("open local repository {}", path.display())]
    Open { path: PathBuf, source: gix::Error },
    #[error("resolve revision {revision:?} to a commit")]
    Revision { revision: String, source: ReadError },
    #[error("import history at {tip}")]
    History {
        tip: gix::ObjectId,
        source: ReadError,
    },
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ReadError {
    #[error("{name} budget reached ({limit})")]
    Exhausted { name: &'static str, limit: u64 },
    #[error(transparent)]
    Git(#[from] gix::Error),
    #[error(transparent)]
    Count(#[from] std::num::ParseIntError),
    #[error("{0}")]
    Invalid(&'static str),
    #[error("read commit {id}; complete local history is required")]
    Incomplete {
        id: gix::ObjectId,
        source: Box<ReadError>,
    },
    #[error("diff path {path:?} in commit {id}")]
    Diff {
        path: gix::bstr::BString,
        id: gix::ObjectId,
        source: Box<ReadError>,
    },
    #[error("commit {id} dated {date} UTC exceeds the import budget; {advice}")]
    Refused {
        id: gix::ObjectId,
        date: String,
        advice: String,
        source: Box<ReadError>,
    },
}

impl ReadError {
    /// A budget or a gix allocation limit stopped the read, so a later start may fit.
    fn is_exhausted(&self) -> bool {
        match self {
            Self::Exhausted { .. } => true,
            Self::Git(error) => error.is_resource_exhausted(),
            Self::Incomplete { source, .. }
            | Self::Diff { source, .. }
            | Self::Refused { source, .. } => source.is_exhausted(),
            Self::Count(_) | Self::Invalid(_) => false,
        }
    }
}

type Result<T, E = ReadError> = std::result::Result<T, E>;

#[derive(Serialize)]
pub(super) struct History {
    pub(super) tip: String,
    merge_policy: &'static str,
    rename_policy: &'static str,
    diff_policy: &'static str,
    /// The commit before the range, its whole tree recorded as additions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) boundary: Option<Commit>,
    /// First-parent commits in the range, oldest first.
    pub(super) commits: Vec<Commit>,
}

#[derive(Clone, Copy)]
struct Limits {
    commits: usize,
    entries: u64,
    tree_entries: u64,
    tree_read_bytes: u64,
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
            tree_read_bytes: 512 * 1024 * 1024,
            retained_bytes: 512 * 1024 * 1024,
            blob_bytes: 8 * 1024 * 1024 * 1024,
            lines: 20_000_000,
            work: 32 * 1024 * 1024 * 1024,
            object_bytes: OBJECT_ALLOCATION_BYTES,
            line_pairs: 1_000_000_000,
        }
    }
}

fn exhausted(name: &'static str, limit: u64) -> ReadError {
    ReadError::Exhausted { name, limit }
}

/// The cumulative charges, recorded after each commit so a shorter range's cost is known.
#[derive(Clone, Copy, Default)]
struct Spent {
    blob_bytes: u64,
    lines: u64,
    work: u64,
    changes: u64,
    retained_bytes: u64,
}

struct Budget {
    limits: Limits,
    spent: Spent,
    trees: BTreeMap<gix::ObjectId, (u64, usize, usize, u64)>,
}

impl Budget {
    fn new(limits: Limits) -> Self {
        Self {
            limits,
            spent: Spent::default(),
            trees: BTreeMap::new(),
        }
    }

    fn charge(&mut self, amount: u64) -> Result<()> {
        self.spent.work = self.spent.work.saturating_add(amount);
        if self.spent.work > self.limits.work {
            return Err(exhausted("cumulative work", self.limits.work));
        }
        Ok(())
    }

    fn retain(&mut self, bytes: u64) -> Result<()> {
        self.spent.retained_bytes = self.spent.retained_bytes.saturating_add(bytes);
        if self.spent.retained_bytes > self.limits.retained_bytes {
            return Err(exhausted(
                "retained input bytes",
                self.limits.retained_bytes,
            ));
        }
        Ok(())
    }

    fn change(&mut self, change: &TreeChange<'_, '_, '_>) -> Result<()> {
        let source_bytes = match change {
            TreeChange::Rewrite {
                source_location, ..
            } => source_location.len(),
            _ => 0,
        };
        self.entry(change.location().len() + source_bytes)
    }

    fn entry(&mut self, path_bytes: usize) -> Result<()> {
        self.spent.changes += 1;
        self.charge(1)?;
        if self.spent.changes > self.limits.entries {
            return Err(exhausted("changed entries", self.limits.entries));
        }
        self.retain((path_bytes + 512) as u64)
    }

    fn object(&mut self, repo: &gix::Repository, id: gix::ObjectId) -> Result<u64> {
        self.charge(1)?;
        let size = repo.find_header(id)?.size();
        if size > self.limits.object_bytes {
            return Err(exhausted("object bytes", self.limits.object_bytes));
        }
        self.charge(size)?;
        Ok(size)
    }

    fn tree(
        &mut self,
        repo: &gix::Repository,
        id: gix::ObjectId,
    ) -> Result<(u64, usize, usize, u64)> {
        self.read_tree(repo, id, 0, self.limits.tree_read_bytes)
    }

    fn read_tree(
        &mut self,
        repo: &gix::Repository,
        id: gix::ObjectId,
        depth: usize,
        remaining_bytes: u64,
    ) -> Result<(u64, usize, usize, u64)> {
        self.charge(1)?;
        if depth > 256 {
            return Err(exhausted("tree depth", 256));
        }
        if let Some(&summary) = self.trees.get(&id) {
            return Ok(summary);
        }
        let bytes = self.object(repo, id)?;
        // Each ancestor tree buffer stays live until its children have been read.
        let remaining_bytes = remaining_bytes.checked_sub(bytes).ok_or_else(|| {
            exhausted("single-tree read bytes", self.limits.tree_read_bytes)
        })?;
        self.retain(128)?;
        let tree = repo.find_tree(id)?;
        let mut summary = (0u64, 0usize, 0usize, 0u64);
        for entry in tree.iter() {
            self.charge(1)?;
            let entry = entry?;
            let child = if entry.mode().is_tree() {
                self.read_tree(repo, entry.object_id(), depth + 1, remaining_bytes)?
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
                    return Err(exhausted(name, limit));
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

/// A first-parent commit whose tree differs from its first parent's, so playback shows
/// it; commits that leave the tree unchanged are not links.
pub(super) struct Link {
    id: gix::ObjectId,
    tree: gix::ObjectId,
    pub(super) seconds: i64,
}

/// Reads only commit headers along the first-parent chain from `tip`, oldest first.
pub(super) fn links(
    repo: &gix::Repository,
    tip: gix::ObjectId,
) -> Result<Vec<Link>, Error> {
    scan(repo, tip, &mut Budget::new(Limits::default()))
        .map_err(|source| Error::History { tip, source })
}

/// Imports the links from `skip` onwards; with `skip > 0`, the tree of the last skipped
/// link becomes the boundary, the only state read from before the range. A budget
/// refusal suggests only start dates that `end_date`, the inclusive last UTC day, allows.
pub(super) fn import(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    links: &[Link],
    skip: usize,
    end_date: Option<i64>,
) -> Result<History, Error> {
    load_with_limits(repo, tip, links, skip, end_date, Limits::default())
        .map_err(|source| Error::History { tip, source })
}

pub(super) fn import_all(
    repo: &gix::Repository,
    tip: gix::ObjectId,
) -> Result<History, Error> {
    import(repo, tip, &links(repo, tip)?, 0, None)
}

const MAX_REFERENCE_HOPS: usize = 64;
const OBJECT_ALLOCATION_BYTES: u64 = 16 * 1024 * 1024;

fn reference_id(repo: &gix::Repository, name: &str) -> Result<Option<gix::ObjectId>> {
    let Some(mut reference) = repo.try_find_reference(name)? else {
        return Ok(None);
    };
    let mut seen = BTreeSet::new();
    for _ in 0..MAX_REFERENCE_HOPS {
        if !seen.insert(reference.name().to_owned()) {
            return Err(ReadError::Invalid("symbolic reference cycle"));
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
    Err(ReadError::Invalid(
        "symbolic reference chain exceeds 64 hops",
    ))
}

fn peel_commit(
    repo: &gix::Repository,
    mut id: gix::ObjectId,
    budget: &mut Budget,
) -> Result<gix::ObjectId> {
    let mut seen = BTreeSet::new();
    for hop in 0..=MAX_REFERENCE_HOPS {
        if !seen.insert(id) {
            return Err(ReadError::Invalid("tag object cycle"));
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
                    return Err(ReadError::Invalid("tag chain exceeds 64 hops"));
                }
                id = repo.find_tag(id)?.target_id()?.detach();
            }
            _ => {
                return Err(ReadError::Invalid(
                    "revision target is not a commit or annotated tag",
                ));
            }
        }
    }
    unreachable!("the last hop returns an error")
}

fn revision_anchor(repo: &gix::Repository, name: &str) -> Result<gix::ObjectId> {
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
        Some(Err(())) => Err(ReadError::Invalid("ambiguous object prefix")),
        None => Err(ReadError::Invalid(
            "revision reference or object does not exist",
        )),
    }
}

fn bounded_revision(repo: &gix::Repository, revision: &str) -> Result<gix::ObjectId> {
    if revision.is_empty() || revision.len() > 4096 {
        return Err(ReadError::Invalid("revision must contain 1–4096 bytes"));
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
            return Err(ReadError::Invalid(
                "unsupported revision expression; use a ref, object ID, ^N or ~N",
            ));
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
            return Err(ReadError::Invalid(
                "revision traversal exceeds 1000000 steps",
            ));
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
                .ok_or(ReadError::Invalid("revision parent does not exist"))?
                .detach();
            id = peel_commit(repo, parent, &mut budget)?;
        }
    }
    Ok(id)
}

/// Reads the range newest first, so a budget refusal knows which later start fits.
fn load_with_limits(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    links: &[Link],
    skip: usize,
    end_date: Option<i64>,
    limits: Limits,
) -> Result<History> {
    let mut budget = Budget::new(limits);
    let mut resources = bounded_resources(repo)?;
    let mut commits = Vec::new();
    let mut spent = Vec::new();
    for (index, link) in links.iter().enumerate().skip(skip).rev() {
        match diffed(repo, link, &mut budget, &mut resources) {
            Ok(commit) => {
                commits.push(commit);
                spent.push(budget.spent);
            }
            Err(error) if error.is_exhausted() => {
                return Err(refusal(
                    repo,
                    links,
                    (index, index),
                    end_date,
                    budget,
                    &spent,
                    error,
                ));
            }
            Err(error) => return Err(error),
        }
    }
    commits.reverse();
    let boundary = match skip.checked_sub(1) {
        None => None,
        Some(last) => match boundary(repo, &links[last], &mut budget) {
            Ok(boundary) => Some(boundary),
            Err(error) if error.is_exhausted() => {
                return Err(refusal(
                    repo,
                    links,
                    (last, skip),
                    end_date,
                    budget,
                    &spent,
                    error,
                ));
            }
            Err(error) => return Err(error),
        },
    };
    Ok(History {
        tip: tip.to_string(),
        merge_policy: "first-parent",
        rename_policy: "exact-content-nonempty-no-copies",
        diff_policy: "raw-blobs-histogram-nul-in-first-8000-bytes",
        boundary,
        commits,
    })
}

/// Names the link whose read failed and a start date whose range lies after `after`.
fn refusal(
    repo: &gix::Repository,
    links: &[Link],
    (failed, after): (usize, usize),
    end_date: Option<i64>,
    budget: Budget,
    spent: &[Spent],
    error: ReadError,
) -> ReadError {
    let advice = later_start(repo, links, after, end_date, budget, spent).map_or_else(
        || "no later --start-date fits".to_owned(),
        |date| format!("--start-date {} fits", crate::model::date(date)),
    );
    let link = &links[failed];
    ReadError::Refused {
        id: link.id,
        date: crate::model::date(link.seconds),
        advice,
        source: Box::new(error),
    }
}

/// The first UTC day whose selection starts after `after` with a range and boundary
/// that fit; `spent` holds the charges after each link, newest first. Boundary trials
/// share one work allowance, and each is itself capped by the import's limits, so the
/// search costs at most twice the import's work limit.
fn later_start(
    repo: &gix::Repository,
    links: &[Link],
    mut after: usize,
    end_date: Option<i64>,
    mut budget: Budget,
    spent: &[Spent],
) -> Option<i64> {
    let mut latest = links.get(..=after)?.iter().map(|link| link.seconds).max()?;
    let mut trials = 0u64;
    loop {
        let date = latest
            .div_euclid(86_400)
            .checked_add(1)?
            .checked_mul(86_400)?;
        let start = after
            + 1
            + links[after + 1..]
                .iter()
                .position(|link| link.seconds >= date)?;
        let before = spent[links.len() - 1 - start];
        // The oldest included diff already charged the boundary tree as its parent.
        budget.spent = before;
        let fits = boundary(repo, &links[start - 1], &mut budget).is_ok();
        trials = budget
            .spent
            .work
            .saturating_sub(before.work)
            .saturating_add(trials);
        if trials > budget.limits.work {
            return None;
        }
        if fits {
            let accepted = crate::start_date(&crate::model::date(date)) == Ok(date)
                && end_date.is_none_or(|end| {
                    links[start..]
                        .iter()
                        .any(|link| (date..end + 86_400).contains(&link.seconds))
                });
            return accepted.then_some(date);
        }
        latest = links[start].seconds;
        after = start;
    }
}

fn scan(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    budget: &mut Budget,
) -> Result<Vec<Link>> {
    let mut links = Vec::new();
    let mut seen = BTreeSet::new();
    let mut child: Option<Link> = None;
    let mut current = Some(tip);
    while let Some(id) = current {
        if seen.len() >= budget.limits.commits {
            return Err(exhausted(
                "first-parent commits",
                budget.limits.commits as u64,
            ));
        }
        if !seen.insert(id) {
            return Err(ReadError::Invalid("commit graph contains a cycle"));
        }
        let incomplete = |source| ReadError::Incomplete {
            id,
            source: Box::new(source),
        };
        budget.object(repo, id).map_err(incomplete)?;
        let commit = repo
            .find_commit(id)
            .map_err(|error| incomplete(error.into()))?;
        budget.retain(128)?;
        let tree = commit.tree_id()?.detach();
        let author = commit.decode()?.author;
        let seconds = gix::actor::SignatureRef::from_bytes(author)?
            .time()?
            .seconds;
        links.extend(child.take().filter(|link| link.tree != tree));
        child = Some(Link { id, tree, seconds });
        current = commit.parent_ids().next().map(gix::Id::detach);
    }
    let empty = gix::ObjectId::empty_tree(repo.object_hash());
    links.extend(child.filter(|root| root.tree != empty));
    links.reverse();
    Ok(links)
}

fn boundary(
    repo: &gix::Repository,
    link: &Link,
    budget: &mut Budget,
) -> Result<Commit> {
    budget.object(repo, link.id)?;
    let commit = repo.find_commit(link.id)?;
    let (entries, ..) = budget.tree(repo, link.tree)?;
    budget.charge(entries)?;
    let mut recorder = gix::traverse::tree::Recorder::default();
    repo.find_tree(link.tree)?
        .traverse()
        .breadthfirst(&mut recorder)?;
    let mut changes = Vec::new();
    for entry in recorder.records {
        if entry.mode.is_tree() {
            continue;
        }
        budget.entry(entry.filepath.len())?;
        changes.push(Change {
            path: entry.filepath.into(),
            source_path: None,
            kind: ChangeKind::Addition,
            before: None,
            after: Some(Entry {
                id: entry.oid.to_string(),
                mode: entry.mode.value(),
            }),
            diff: BlobDiff::Skipped,
        });
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    record_commit(&commit, changes, budget)
}

fn record_commit(
    commit: &gix::Commit<'_>,
    changes: Vec<Change>,
    budget: &mut Budget,
) -> Result<Commit> {
    let decoded = commit.decode()?;
    budget.retain(commit.data.len() as u64 * 3)?;
    Ok(Commit {
        id: commit.id.to_string(),
        parents: commit.parent_ids().map(|id| id.to_string()).collect(),
        author: identity(decoded.author)?,
        committer: identity(decoded.committer)?,
        message: decoded.message.to_vec(),
        changes,
    })
}

fn diffed(
    repo: &gix::Repository,
    link: &Link,
    budget: &mut Budget,
    resources: &mut gix::diff::blob::Platform,
) -> Result<Commit> {
    let id = link.id;
    budget.object(repo, id)?;
    let commit = repo.find_commit(id)?;
    let before = if let Some(parent) = commit.parent_ids().next() {
        budget.object(repo, parent.detach())?;
        let before_id = parent.object()?.into_commit().tree_id()?.detach();
        budget.tree(repo, before_id)?;
        repo.find_tree(before_id)?
    } else {
        repo.empty_tree()
    };
    budget.tree(repo, link.tree)?;
    let after = repo.find_tree(link.tree)?;
    let mut changes = Vec::new();
    let mut exhaustion = None;
    let walked = before
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
        .for_each_to_obtain_tree_with_cache(&after, resources, |change| {
            if let Err(error) = budget.change(&change) {
                exhaustion = Some(error);
                return Ok(ControlFlow::Break(()));
            }
            changes.extend(records(change).into_iter().flatten());
            Ok(ControlFlow::Continue(()))
        });
    // Breaking out of the walk surfaces as gix's `Cancelled`; report the budget instead.
    if let Some(error) = exhaustion {
        return Err(error);
    }
    walked?;
    resources.clear_resource_cache();
    changes.sort_by(|a, b| {
        (&a.path, &a.source_path, &a.kind).cmp(&(&b.path, &b.source_path, &b.kind))
    });
    for change in &mut changes {
        change.diff = bounded_blob_diff(repo, change, budget).map_err(|source| {
            ReadError::Diff {
                path: change.path.clone().into(),
                id,
                source: Box::new(source),
            }
        })?;
    }
    record_commit(&commit, changes, budget)
}

/// A diff cache with no attributes: rename tracking matches exact object ids only and
/// blob diffs read raw blobs, so neither HEAD, the index nor any attribute file is read.
fn bounded_resources(repo: &gix::Repository) -> Result<gix::diff::blob::Platform> {
    let attributes = gix::worktree::stack::state::Attributes::new(
        gix::attrs::Search::default(),
        None,
        gix::worktree::stack::state::attributes::Source::IdMapping,
        gix::attrs::search::MetadataCollection::default(),
    );
    Ok(gix::diff::resource_cache(
        repo,
        gix::diff::blob::pipeline::Mode::ToGit,
        gix::worktree::Stack::new(
            repo.git_dir(),
            gix::worktree::stack::State::AttributesStack(attributes),
            gix::glob::pattern::Case::Sensitive,
            Vec::new(),
            Vec::new(),
        ),
        gix::diff::blob::pipeline::WorktreeRoots::default(),
    )?)
}

fn identity(raw: &[u8]) -> Result<Identity> {
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
) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
    let load = |entry: &Option<Entry>| -> Result<Vec<u8>> {
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
        Err(ReadError::Git(error)) if error.is_resource_exhausted() => Ok(None),
        Err(error) => Err(error),
    }
}

fn bounded_blob_diff(
    repo: &gix::Repository,
    change: &Change,
    budget: &mut Budget,
) -> Result<BlobDiff> {
    if change
        .before
        .iter()
        .chain(&change.after)
        .any(|entry| entry.mode == 0o160_000)
    {
        return Ok(BlobDiff::Gitlink);
    }
    let mut skipped = false;
    let mut read_bytes = 0;
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
        read_bytes += size.min(budget.limits.object_bytes);
        budget.spent.blob_bytes = budget.spent.blob_bytes.saturating_add(size);
        skipped |= size > budget.limits.object_bytes
            || budget.spent.blob_bytes > budget.limits.blob_bytes;
    }
    if skipped {
        return Ok(BlobDiff::Skipped);
    }
    budget.charge(read_bytes)?;
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
    budget.spent.lines = budget
        .spent
        .lines
        .saturating_add(before_lines + after_lines);
    budget.charge(before_lines + after_lines)?;
    if budget.spent.lines > budget.limits.lines
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
        import_all(&repo, tip)
    }

    fn load_all(
        repo: &gix::Repository,
        tip: gix::ObjectId,
        limits: Limits,
    ) -> Result<History> {
        let links = scan(repo, tip, &mut Budget::new(Limits::default()))?;
        load_with_limits(repo, tip, &links, 0, None, limits)
    }

    /// The error and its causes, as the CLI prints them.
    fn message(error: impl Into<anyhow::Error>) -> String {
        format!("{:#}", error.into())
    }

    #[test]
    fn truncation_and_budget_options_are_removed() {
        for command in ["inspect", "preview", "export"] {
            let mut args = vec!["codestrand", command];
            if command != "preview" {
                args.push("unused");
            }
            let mut truncated = args.clone();
            truncated.push("--allow-truncated-history");
            assert!(crate::Cli::try_parse_from(truncated).is_err());
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
        assert!(budget.spent.work > budget.limits.object_bytes);
        let history = import(root.path(), "HEAD").unwrap();
        assert_eq!(history.commits.len(), 2);
        for commit in &history.commits {
            assert_eq!(commit.changes.len(), 1);
            assert!(matches!(commit.changes[0].diff, BlobDiff::Skipped));
        }
    }

    #[test]
    fn every_history_refusal_names_its_budget() {
        let (_root, repo, tip) = hostile_history(0, b"first\nsecond\n");
        for (limits, name) in [
            (
                Limits {
                    entries: 0,
                    ..Limits::default()
                },
                "changed entries",
            ),
            (
                Limits {
                    retained_bytes: 0,
                    ..Limits::default()
                },
                "retained input bytes",
            ),
            (
                Limits {
                    work: 0,
                    ..Limits::default()
                },
                "cumulative work",
            ),
            (
                Limits {
                    object_bytes: 0,
                    ..Limits::default()
                },
                "object bytes",
            ),
            (
                Limits {
                    tree_entries: 0,
                    ..Limits::default()
                },
                "single-tree entries",
            ),
        ] {
            let error = message(load_all(&repo, tip, limits).err().unwrap());
            assert!(error.contains(name), "{error}");
            assert!(error.contains("no later --start-date fits"), "{error}");
        }
    }

    #[test]
    fn a_refusal_with_changes_left_to_diff_names_its_budget() {
        // Tree diffs report modifications as they walk, so a refusal at `a/file`
        // leaves `b/file` undiffed; additions would wait for rename tracking.
        let (_root, repo, first) = hostile_history(1, b"first\n");
        let entry = |kind: gix::objs::tree::EntryKind, name: &str, oid| {
            gix::objs::tree::Entry {
                mode: kind.into(),
                filename: name.into(),
                oid,
            }
        };
        let blob = repo.write_blob(b"second\n").unwrap().detach();
        let file = gix::objs::Tree {
            entries: vec![entry(gix::objs::tree::EntryKind::Blob, "file", blob)],
        };
        let file = repo.write_object(&file).unwrap().detach();
        let tree = gix::objs::Tree {
            entries: ["a", "b"]
                .map(|name| entry(gix::objs::tree::EntryKind::Tree, name, file))
                .into(),
        };
        let commit = repo.find_commit(first).unwrap();
        let mut commit = commit.decode().unwrap().to_owned().unwrap();
        commit.tree = repo.write_object(&tree).unwrap().detach();
        commit.parents = [first].into_iter().collect();
        let tip = repo.write_object(&commit).unwrap().detach();
        let limits = Limits {
            entries: 1,
            ..Limits::default()
        };
        let error = message(load_all(&repo, tip, limits).err().unwrap());
        assert!(error.contains("changed entries budget reached"), "{error}");
    }

    #[test]
    fn single_tree_ceiling_is_fixed() {
        let (_root, repo, tip) = hostile_history(20, b"ok\n");
        let error = import_all(&repo, tip).err().unwrap();
        let message = message(error);
        assert!(
            message.contains("single-tree entries budget reached (2000000)"),
            "{message}"
        );
    }

    #[test]
    fn tree_read_refuses_when_live_buffers_exceed_the_limit() {
        let (_root, repo, tip) = hostile_history(1, b"ok\n");
        let tree = repo.find_commit(tip).unwrap().tree_id().unwrap().detach();
        let root_bytes = repo.find_header(tree).unwrap().size();
        let limits = Limits {
            tree_read_bytes: root_bytes,
            ..Limits::default()
        };
        let error = message(load_all(&repo, tip, limits).err().unwrap());
        assert!(
            error.contains(&format!(
                "single-tree read bytes budget reached ({root_bytes})"
            )),
            "{error}"
        );
    }

    #[test]
    fn expanded_path_limits_are_fixed() {
        for (levels, filename, reason) in [
            (0, vec![b'x'; 16_384], "path bytes"),
            (16, vec![b'x'; 1024], "single-tree path bytes"),
        ] {
            let (_root, repo, tip) = hostile_path(levels, b"ok\n", &filename);
            let error = import_all(&repo, tip).err().unwrap();
            let message = message(error);
            assert!(message.contains(reason), "{message}");
        }
    }

    #[test]
    fn first_parent_scan_stops_at_its_commit_ceiling() {
        let (_root, repo, first) = hostile_history(0, b"ok\n");
        let commit = repo.find_commit(first).unwrap();
        let mut commit = commit.decode().unwrap().to_owned().unwrap();
        commit.parents = [first].into_iter().collect();
        let tip = repo.write_object(&commit).unwrap().detach();
        let limits = Limits {
            commits: 1,
            ..Limits::default()
        };
        let error = scan(&repo, tip, &mut Budget::new(limits)).err().unwrap();
        assert_eq!(error.to_string(), "first-parent commits budget reached (1)");
    }

    #[test]
    fn default_budget_imports_complete_small_history() {
        let (_root, repo, tip) = hostile_history(0, b"ok\n");
        assert!(import_all(&repo, tip).is_ok());
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
        let history = load_all(
            &repo,
            parent,
            Limits {
                line_pairs: 20,
                ..Limits::default()
            },
        )
        .unwrap();
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
        let limits = Limits {
            tree_entries: 4096,
            blob_bytes: 4 * 1024 * 1024,
            ..Limits::default()
        };
        let (_root, repo, tip) = hostile_history(12, b"ok\n");
        assert!(load_all(&repo, tip, limits).is_err());
        let blob =
            b"line with repeated compressible content and enough bytes per line\n";
        let (_root, repo, tip) = hostile_history(0, &blob.repeat(131_072));
        let history = load_all(&repo, tip, limits).unwrap();
        assert!(matches!(
            history.commits[0].changes[0].diff,
            BlobDiff::Skipped
        ));
    }

    #[test]
    fn refusal_names_a_start_date_that_fits() {
        let (_root, repo, first) = hostile_history(0, &vec![b'x'; 20_000]);
        let object = repo.find_commit(first).unwrap();
        let mut next = object.decode().unwrap().to_owned().unwrap();
        let mut tree = repo
            .find_tree(next.tree)
            .unwrap()
            .decode()
            .unwrap()
            .to_owned();
        tree.entries.push(gix::objs::tree::Entry {
            mode: gix::objs::tree::EntryKind::Blob.into(),
            filename: "small".into(),
            oid: repo.write_blob(b"small\n").unwrap().detach(),
        });
        next.tree = repo.write_object(&tree).unwrap().detach();
        next.parents = [first].into_iter().collect();
        next.author.time.seconds += 86_400;
        let tip = repo.write_object(&next).unwrap().detach();
        let limits = Limits {
            work: 10_000,
            ..Limits::default()
        };
        let links = scan(&repo, tip, &mut Budget::new(Limits::default())).unwrap();
        let error = load_with_limits(&repo, tip, &links, 0, None, limits)
            .err()
            .unwrap();
        let expected = format!(
            "commit {first} dated 2023-11-14 UTC exceeds the import budget; --start-date 2023-11-15 fits"
        );
        assert!(format!("{error:#}").contains(&expected), "{error:#}");
        let fitted = load_with_limits(&repo, tip, &links, 1, None, limits).unwrap();
        assert_eq!(fitted.boundary.unwrap().id, first.to_string());
    }

    /// A root commit on 2023-11-14 holding one file, then one commit per `(day, files)`
    /// after it whose tree holds that many files.
    fn daily_history(
        days: &[(i64, usize)],
    ) -> (tempfile::TempDir, gix::Repository, Vec<Link>) {
        let (root, repo, mut parent) = hostile_history(0, b"ok\n");
        let blob = repo.write_blob(b"ok\n").unwrap().detach();
        let mut commit = repo
            .find_commit(parent)
            .unwrap()
            .decode()
            .unwrap()
            .to_owned()
            .unwrap();
        for &(day, files) in days {
            commit.tree = repo
                .write_object(&gix::objs::Tree {
                    entries: (0..files)
                        .map(|file| gix::objs::tree::Entry {
                            mode: gix::objs::tree::EntryKind::Blob.into(),
                            filename: format!("{file:02}").into(),
                            oid: blob,
                        })
                        .collect(),
                })
                .unwrap()
                .detach();
            commit.parents = [parent].into_iter().collect();
            commit.author.time.seconds = 1_700_000_000 + day * 86_400;
            parent = repo.write_object(&commit).unwrap().detach();
        }
        let links = scan(&repo, parent, &mut Budget::new(Limits::default())).unwrap();
        (root, repo, links)
    }

    /// The refusal for `skip`, after checking that the last link alone fits.
    fn refusal_text(repo: &gix::Repository, links: &[Link], skip: usize) -> String {
        let tip = links.last().unwrap().id;
        let limits = Limits {
            entries: 25,
            ..Limits::default()
        };
        assert!(
            load_with_limits(repo, tip, links, links.len() - 1, None, limits).is_ok()
        );
        message(
            load_with_limits(repo, tip, links, skip, None, limits)
                .err()
                .unwrap(),
        )
    }

    #[test]
    fn a_boundary_over_budget_names_a_later_start() {
        let (_root, repo, links) = daily_history(&[(0, 20), (1, 1), (2, 2)]);
        let error = refusal_text(&repo, &links, 2);
        let expected = format!(
            "commit {} dated 2023-11-14 UTC exceeds the import budget; --start-date 2023-11-16 fits",
            links[1].id
        );
        assert!(error.contains(&expected), "{error}");
    }

    #[test]
    fn a_later_start_skips_candidates_whose_boundary_is_over_budget() {
        let (_root, repo, links) = daily_history(&[(1, 20), (2, 1), (3, 2)]);
        let error = refusal_text(&repo, &links, 0);
        let expected = format!(
            "commit {} dated 2023-11-15 UTC exceeds the import budget; --start-date 2023-11-17 fits",
            links[1].id
        );
        assert!(error.contains(&expected), "{error}");
    }

    #[test]
    fn a_later_start_stays_within_the_end_date() {
        let (_root, repo, links) = daily_history(&[(1, 20), (2, 1), (3, 2)]);
        let limits = Limits {
            entries: 25,
            ..Limits::default()
        };
        let end = crate::start_date("2023-11-16").ok();
        let tip = links.last().unwrap().id;
        let error = load_with_limits(&repo, tip, &links, 0, end, limits)
            .err()
            .unwrap();
        assert!(
            format!("{error:#}").contains("no later --start-date fits"),
            "{error:#}"
        );
    }

    #[test]
    fn a_start_date_the_cli_rejects_is_not_suggested() {
        let year_10000 = 2_913_241;
        let (_root, repo, links) =
            daily_history(&[(1, 20), (year_10000, 1), (year_10000 + 1, 2)]);
        let error = refusal_text(&repo, &links, 0);
        assert!(error.contains("no later --start-date fits"), "{error}");
    }

    #[test]
    fn each_independent_budget_refuses() {
        let (_root, repo, tip) = hostile_history(0, b"first\nsecond\n");
        for limits in [
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
            assert!(load_all(&repo, tip, limits).is_err());
        }
        assert!(import_all(&repo, tip).is_ok());
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
        let history = load_all(
            &repo,
            deleted,
            Limits {
                object_bytes: 1024,
                ..Limits::default()
            },
        )
        .unwrap();
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
        let history = import_all(&repo, tip).unwrap();
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
            if limits.blob_bytes == 0 || limits.object_bytes == 0 {
                assert_eq!(budget.spent.work, 1);
            } else {
                assert!(budget.spent.work >= 13);
            }
            for _ in 0..100 {
                if let Err(error) = bounded_blob_diff(&repo, change, &mut budget) {
                    assert!(matches!(error, ReadError::Exhausted { .. }));
                    break;
                }
            }
            assert!(budget.spent.work > budget.limits.work);
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
            let history = load_all(&repo, tip, limits).unwrap();
            assert!(matches!(
                history.commits[0].changes[0].diff,
                BlobDiff::Skipped
            ));
        }
    }

    #[test]
    fn deep_shared_tree_stops_before_gix_expansion() {
        let (_root, repo, tip) = hostile_history(512, b"ok\n");
        let error = import_all(&repo, tip).err().unwrap();
        assert!(message(error).contains("tree depth"));
    }

    #[test]
    fn exhaustion_inside_a_diff_still_offers_a_later_start() {
        let id = gix::ObjectId::null(gix::hash::Kind::Sha1);
        let diff = |source| ReadError::Diff {
            path: "file".into(),
            id,
            source: Box::new(source),
        };
        assert!(diff(exhausted("cumulative work", 0)).is_exhausted());
        assert!(!diff(ReadError::Invalid("tag object cycle")).is_exhausted());
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
}
