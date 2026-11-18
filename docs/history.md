# Git history import

## Inspect a repository

```sh
codestrand inspect /path/to/repository --revision HEAD
```

Inspection reads a complete local working or bare repository without modifying
history. The experimental JSON lists the first-parent commits that change the tree,
oldest first, with raw author identities and paths and their file changes. Merge
changes compare against the first parent. Avoid treating the experimental schema as a
stable contract.

## Choose history for playback

```sh
codestrand preview --repository /path/to/repository --revision HEAD
```

Preview and export use the selected revision's first-parent timeline. Revision
defaults to `HEAD`. Codestrand does not clone or fetch a remote repository: obtain
a complete local repository first.

With `--start-date`, import scans commit headers to find the range, then reads one
tree, the commit before the range, and the diffs inside the range. Earlier diffs are
not read, and import budgets apply to the range read.

Revisions accept reference names, full or unambiguous abbreviated object IDs, and
`HEAD` (also `@`). Parent selectors `^N` and first-parent ancestor selectors `~N` can
be combined; `^{}` and `^{commit}` are accepted. Other Git revision expressions,
including reflog and message searches, are rejected rather than delegated to unbounded
reference traversal. Revision strings are limited to 4,096 bytes and navigation to
1,000,000 steps, with the existing object-size and work budgets applied before loading.
Annotated tags and symbolic references have a 64-hop limit and cycle detection.
Cyclic or excessive chains produce an error, including when encountered through HEAD
during diff setup. Import uses no wall-clock deadline.

## Analysis limits

Import stops at fixed safety limits and refuses incomplete history by default.
`--allow-truncated-history` accepts completed commits and prints the exhausted budget
and the inclusive last complete first-parent commit, when known. Inspection sets
`truncated` in JSON. Playback uses that first-parent prefix, which can be empty;
commits with no file changes are not shown. Partial imports are never cached;
later runs retry analysis.

Blob size, diff input and per-diff comparison limits skip line detail only. Inspection
records the path, change kind and rename with diff kind `skipped`, without line counts
or samples. Playback retains the file event with zero line counts and no samples;
touch heat still applies. Skipped blobs still consume global work units. Once the
aggregate byte or line allowance is spent, later diffs also omit line detail.

| Budget | Limit |
| --- | --- |
| First-parent commits | 1,000,000 |
| Expanded entries per tree and retained changes overall | 2,000,000 each |
| Expanded path bytes per tree | 64 MiB |
| Tree depth and individual path bytes | 256 levels; 16,384 bytes |
| Single inflated object / allocation | 16 MiB |
| Inflated diff blob bytes overall | 8 GiB |
| Diff input lines overall | 20,000,000 |
| Potential line comparisons per diff | 1,000,000,000 |
| Total work charged for objects, bytes and lines | 32 GiB of work units |
| Accounted graph, tree and retained history storage | 512 MiB |

Tree summaries count repeated subtrees at every expanded location before diffing.
Object headers are checked before loading data; lines are counted before interning.
The storage budget accounts for payloads and overhead, rather than measuring process
RSS; dependency buffers and rendering need additional memory. Limits are fixed to keep
hostile inputs bounded; a later start reduces the history they are charged against.

## Cache

Repository playback saves the latest imported timeline, diff counts and bounded
line samples under the platform user cache directory, inside `codestrand/`.
Linked worktrees share a cache
namespace based on the canonical common Git directory. A different resolved tip or
start commit replaces the saved timeline after a complete import. Changing the end
date or pacing reuses the imported timeline. Contributor mapping edits take effect on
cache hits.

Cache errors regenerate data without blocking playback. To bypass history and
avatar cache reads and writes:

```sh
codestrand preview --repository /path/to/repository --no-cache
```

Inspection and repositories with Git replacement objects always import directly.
Messages, signatures, blob IDs and hunks are not cached. See
[contributors](contributors.md) for separately cached portraits.

Cache files are read only through real directories, with a 64 MiB byte ceiling before
JSON parsing. Version, selected tip, commit identifiers, event/change counts, paths,
author lengths, timestamps, diff counts and sample limits are checked before reuse.
Only the canonical snapshot encoding is accepted: padded, malformed or invalid snapshots
are misses and are rebuilt. Cache validation does not authenticate who wrote a snapshot.
