# Privacy and local processing

Codestrand makes no application network requests by default. Git import, planning
and rendering run locally; repositories are not uploaded. Avatar fetching is opt-in.

## Avatar requests

`--fetch-avatars` enables HTTPS requests for selected contributors. Depending on the
available portrait and account information, requests can disclose:

- A GitHub repository's owner and name and a commit hash to `api.github.com`, to find
  the matching account. API authentication uses `GH_TOKEN`, or `GITHUB_TOKEN` when
  `GH_TOKEN` is unset or not valid Unicode. An empty `GH_TOKEN` disables authentication
  without falling back to `GITHUB_TOKEN`; empty tokens are ignored. GitHub avatar
  servers receive the account name or numeric ID.
- A configured remote avatar URL to its HTTPS server, and any redirect destination.

No email address, email hash or author name is sent, and GitHub is the only service
contacted unless a configured avatar URL names another.

Codestrand does not add source file paths, source contents or diff text to these
requests. Servers also receive ordinary connection information such as the source
IP address and the Codestrand user agent. Configured URLs can contain whatever
information their author put in them.

## What CI checks

- Only Codestrand depends on `ureq`, `rustls` is reached only through `ureq`, and
  neither `bevy_asset` nor `gix-transport` enables an HTTP feature.
- A Linux PNG export of a small local repository, with the history cache on, runs
  under `strace` in an empty network namespace and fails on any IP socket or Unix
  socket connection outside an allow-list.

The trace covers Linux and the code paths that export exercises. Preview, video
encoding, avatar fetching, macOS and Windows are not traced.

## Local files and processes

The history cache contains commit IDs and dates, contributor names and email
addresses, file paths, change counts and sampled source lines in plain text.
Portrait images and account lookup results are cached locally too. These files are
not encrypted by Codestrand. `--no-cache` disables reading and writing its caches.

The cache is under `codestrand` in the operating system's user cache directory:
`%LOCALAPPDATA%` on Windows, `~/Library/Caches` on macOS, and `$XDG_CACHE_HOME` or
`~/.cache` on Linux. History snapshots use per-repository subdirectories;
portrait data uses `avatars-v3`.

Video export starts FFmpeg and sends it rendered frames through standard input.
Codestrand's production import and renderer do not launch Git, repository hooks,
external diff drivers or other subprocesses.

Before planning, Codestrand compares a planning-memory estimate with available physical
memory. The estimate excludes the renderer and video encoder; it can be conservative
on some inputs and too low on others. It is not a process-memory limit or reservation.
Windowed planning bounds retained travel windows and checkpoints, not total process
memory. The available-memory snapshot excludes swap and container or job-specific limits.

This describes Codestrand's own behavior. It cannot promise what a user-selected
encoder executable does, or keep an operating system from accessing a network-mounted
repository, cache, avatar file or output path. HTTPS avatar requests use environment
proxy settings and the system certificate verifier. Operating-system services and other
software are outside this guarantee.
