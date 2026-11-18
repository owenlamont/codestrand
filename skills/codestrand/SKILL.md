---
name: codestrand
description: >-
  Animated Git history visualiser. Use when asked to play, preview, record or export a
  video or PNG frames of a local Git repository's history, to show several repositories
  on one timeline, or to inspect a repository's commit history as JSON. Covers preview,
  export, FFmpeg video, date ranges, repository lists, config files, contributor
  mapping, privacy and common failures.
license: MIT OR Apache-2.0
---

# Codestrand

Codestrand plays the committed history of local Git repositories as an animated scene:
files keep fixed positions, contributors fly above them, and each commit sends beams to
the files it changed. `preview` opens a window; `export` writes PNG frames or a video
at explicit frame times, so output does not depend on rendering speed.

Install with `uv tool install codestrand` or `npm install -g @owenlamont/codestrand`;
the [installation guide](https://codestrand.pages.dev/getting-started/installation/)
lists crates.io and release ZIPs too.

## Before running

- Pass the repository path explicitly. Codestrand reads a complete local clone, working
  or bare; it never clones, fetches or takes a URL. Shallow or partial clones can lack
  the commits import needs.
- `preview` and `export` need a working graphics adapter, and `preview` a desktop
  session. `inspect` needs neither, so use it to check a repository reads.
- Only committed history is shown; uncommitted edits are not.

## What leaves the machine

Nothing, by default: import, planning and rendering run locally and nothing is
uploaded. `--fetch-avatars` is the only option that makes network requests, to find
contributor portraits. It can send:

- the repository's GitHub owner and name and a commit hash to `api.github.com`,
  authenticated with `GH_TOKEN` or `GITHUB_TOKEN` when set;
- GitHub account names or numeric IDs to GitHub's avatar servers;
- a SHA-256 hash of a contributor's email address to Gravatar;
- configured GitLab usernames to GitLab, and raw email addresses only when
  `--gitlab-email-avatars` is also given;
- a request to any HTTPS image URL GitLab returns or the contributors file sets.

Codestrand adds no source paths, contents or diffs to these requests; a configured URL
carries whatever its author put in it. Do not add `--fetch-avatars` unless the user
asked for portraits; for a private or work repository, ask first. The history cache in
the user cache directory holds author names, emails, paths and sampled source lines in
plain text; `--no-cache` skips reading and writing it. Video export starts the local
FFmpeg executable.

## Inspect

```sh
codestrand inspect /path/to/repository --revision HEAD
```

Prints commits, authors and file changes as JSON on stdout. The schema is experimental.

## Preview

```sh
codestrand preview --repository /path/to/repository
codestrand preview --repository /path/to/repository --start-date 2026-05-01 --end-date 2026-05-31
codestrand preview --repository /path/to/first --repository /path/to/second --theme outrun
```

Preview loops until the window closes (Escape). Both date bounds are UTC and inclusive:
`--end-date` includes the whole named day. `--seconds-per-day` and `--max-idle-seconds`
set pacing. `codestrand preview --help` lists the themes and every default.

## Export

```sh
codestrand export frames --repository /path/to/repository --frames 60 --fps 30
codestrand export hd-frames --repository /path/to/repository --resolution 1920x1080 --frames 2
codestrand export clip --repository /path/to/repository --video history.mp4 --frames 300 --fps 30
```

- The first argument is a new output directory; an existing directory is refused.
- PNG export writes `frame-00000.png` onwards. Without `--frames` the whole playback is
  exported.
- `--video PATH` streams to an installed FFmpeg instead of writing PNGs. The video path
  is relative to the current directory and must not exist. FFmpeg comes from `PATH`,
  `CODESTRAND_FFMPEG` or `--ffmpeg`; the default encoder is libx264, so an odd
  `--resolution` needs PNGs or `--ffmpeg-arg` options. Codestrand never downloads an
  encoder.
- `--camera-path camera.toml` scripts the export camera; see
  [export](https://codestrand.pages.dev/export/#scripted-camera).

### The frames.csv manifest

Every export directory gets `frames.csv`, one row per frame:

```text
frame,seconds,commit,files,avatars,beams,glyphs,missing_glyphs
```

A nonzero `missing_glyphs` means a contributor name used a script no installed font
covers; install fonts for it and export again.

## Several repositories and config files

Repeat `--repository`, or list repositories in a TOML file:

```toml
# repositories.toml; relative paths resolve beside this file
[[repositories]]
path = "../first"
label = "First project"

[[repositories]]
path = "../second"
archived = true # ends after its last commit
```

```sh
codestrand preview --repositories-file repositories.toml
```

`--config codestrand.toml` reads any preview or export option, keyed by flag name
without dashes, plus a `[[repositories]]` list. Flags on the command line override it,
and any `--repository` flag replaces the file's list.

```toml
# codestrand.toml; relative paths resolve beside this file
theme = "atlas"
seconds-per-day = 2
fps = 60

[[repositories]]
path = "../first"
```

```sh
codestrand export config-frames --config codestrand.toml --start-date 2026-05-10
```

Treat a config file from elsewhere like a script: it can enable avatar fetching or
choose the FFmpeg executable.

`--contributors contributors.toml` merges identities across email addresses and marks
bots; see [contributors](https://codestrand.pages.dev/contributors/).

## Common failures

- **History budget exhausted**: import refuses truncated history. Retry with
  `--allow-truncated-history` to play the completed prefix.
- **Export directory exists**: choose a new directory name.
- **FFmpeg missing or no libx264**: install FFmpeg, or export PNGs.
- **Revision or repository unreadable**: check the path is a local, complete clone and
  the revision exists locally; `codestrand inspect` reproduces it without a GPU.
- **Stale or damaged cache**: retry with `--no-cache`.
- **Date range has no commits**: the error names the range and the history's last
  commit date; pick dates inside the history.
- **Option missing from `--help`**: the installed release is older; upgrade it.

## Full documentation

- Docs: <https://codestrand.pages.dev/>
- Agent guide: <https://codestrand.pages.dev/ai-agents/>
- Whole docs for an LLM: <https://codestrand.pages.dev/llms-full.txt>
