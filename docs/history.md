# Git history import

## Inspect a repository

```sh
codestrand inspect /path/to/repository --revision HEAD
```

Inspection reads a complete local working or bare repository without modifying
history. The experimental JSON includes parent-before-child commits, raw author
identities and paths, and first-parent file changes. Merge changes compare against
the first parent. Avoid treating the experimental schema as a stable contract.

## Choose history for playback

```sh
codestrand preview --repository /path/to/repository --revision HEAD
```

Preview and export use the selected revision's first-parent timeline. Revision
defaults to `HEAD`. Codestrand does not clone or fetch a remote repository: obtain
a complete local repository first. Inspection includes reachable commits, while
playback follows the first-parent sequence.

## Analysis limits

Import stops at fixed safety limits and prints `analysis truncated: history resource
budget reached; showing completed commits only`. Inspection also sets `truncated` in
JSON. Playback uses the completed first-parent prefix, which can be empty. Partial
imports are never cached; later runs retry analysis.

| Budget | Limit |
| --- | --- |
| Reachable commits and pending parent references | 1,000,000 each |
| Expanded entries per tree and retained changes overall | 2,000,000 each |
| Expanded path bytes per tree | 64 MiB |
| Tree depth and individual path bytes | 256 levels; 16,384 bytes |
| Single inflated object / allocation | 16 MiB |
| Inflated diff blob bytes overall | 8 GiB |
| Diff input lines overall | 20,000,000 |
| Potential line comparisons per diff | 1,000,000,000 |
| Total work charged for objects, bytes and lines | 32 GiB of work units |
| Accounted graph, tree and retained history storage | 512 MiB |
| Import deadline | 300 seconds, checked between bounded operations |

Tree summaries count repeated subtrees at every expanded location before diffing.
Object headers are checked before loading data; lines are counted before interning.
The storage budget accounts for payloads and overhead, rather than measuring process
RSS; dependency buffers and rendering need additional memory. Limits are fixed to keep
hostile inputs bounded; playback selection does not increase limits.

## Cache

Repository playback saves the latest imported timeline, diff counts and bounded
line samples under the platform user cache directory, inside `codestrand/`.
Linked worktrees share a cache
namespace based on the canonical common Git directory. A different resolved tip
replaces the saved timeline after a full import. Changing range or pacing reuses
the imported timeline. Contributor mapping edits take effect on cache hits.

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
