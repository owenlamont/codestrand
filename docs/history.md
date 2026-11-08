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
