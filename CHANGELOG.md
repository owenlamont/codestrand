# Changelog

All notable changes to Codestrand are documented here, following
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

### Changed

- Planning is faster: on thirty repositories, about 35% less time per window and 40%
  less before the first frame.
  ([#176](https://github.com/owenlamont/codestrand/pull/176))
- Repository titles choose their corner from contributor discs, beams and tiles, no
  longer from contributor names, so some titles sit in a different corner and switch
  less often. ([#176](https://github.com/owenlamont/codestrand/pull/176))
- Repository sets too large for full-size tiles play with tiles, gutters, avatars and
  titles shrunk together, instead of packing refusing with "use fewer repositories".
  ([#180](https://github.com/owenlamont/codestrand/pull/180))
- Avatars no longer shrink as repository count rises, so many-repository playback
  shows them up to four times larger.
  ([#180](https://github.com/owenlamont/codestrand/pull/180))
- Repository titles share one size across tiles, never below 12 pixels, and end in an
  ellipsis when too wide. ([#180](https://github.com/owenlamont/codestrand/pull/180))
- Directory and touched-file labels shrink with their tile's files, to no less than 8
  pixels, and end in an ellipsis when wider than their tile.
  ([#180](https://github.com/owenlamont/codestrand/pull/180))

## v0.2.0 - 2026-11-16

Watch the [feature tour video](https://youtu.be/gmueXAHnvAE).

### Added

- Play full histories across many repositories with windowed planning streamed to
  the renderer and planning progress in preview and export.
  ([#153](https://github.com/owenlamont/codestrand/pull/153))
- Select an inclusive UTC date range with `--start-date` and `--end-date`, including
  the whole end day; earlier commits settle before playback.
  ([#152](https://github.com/owenlamont/codestrand/pull/152),
  [#153](https://github.com/owenlamont/codestrand/pull/153))
- Security documentation covering local inputs, network opt-ins and resource limits.
  ([#153](https://github.com/owenlamont/codestrand/pull/153))
- Native Wayland preview windows on Linux alongside X11 support.
  ([#144](https://github.com/owenlamont/codestrand/pull/144))
- Play several local repositories as tiles on one shared timeline, from repeated
  `--repository` flags or `--repositories-file`, with contributors travelling along
  gutters between tiles. Adds `--contributor-travel`, `--bot-travel`, `--avatar-scale`,
  `--gutter`, `--caption-style`, `--repository-boundary` and
  `--repository-label-plane`.
  ([#94](https://github.com/owenlamont/codestrand/pull/94),
  [#153](https://github.com/owenlamont/codestrand/pull/153))
- Contributors and repositories appear, depart and fade; `--contributor-inactivity-days`
  sets when a quiet contributor is assumed to have left, and repository lists can mark
  repositories `archived` or `ended`.
  ([#98](https://github.com/owenlamont/codestrand/pull/98))
- Pace playback by calendar days with `--seconds-per-day` and `--max-idle-seconds`;
  `[` and `]` change live preview speed from 0.125x to 8x, inclusive.
  ([#63](https://github.com/owenlamont/codestrand/pull/63),
  [#66](https://github.com/owenlamont/codestrand/pull/66))
- Six more themes, `phosphor`, `outrun`, `blueprint`, `bevelled-desktop`, `atlas` and
  `forge`, chosen with `--theme` and switched live in preview with `,` and `.`.
  ([#71](https://github.com/owenlamont/codestrand/pull/71),
  [#83](https://github.com/owenlamont/codestrand/pull/83))
- Files heat up on each touch and cool with `--heat-half-life`.
  ([#77](https://github.com/owenlamont/codestrand/pull/77))
- Sampled added and removed source lines appear beside changed files at close zoom;
  `--no-diff-samples` hides them.
  ([#78](https://github.com/owenlamont/codestrand/pull/78),
  [#135](https://github.com/owenlamont/codestrand/pull/135),
  [#153](https://github.com/owenlamont/codestrand/pull/153))
- Small directory subtrees collapse into folder glyphs at overview zoom;
  `--collapse-directories off` keeps every file visible.
  ([#79](https://github.com/owenlamont/codestrand/pull/79),
  [#114](https://github.com/owenlamont/codestrand/pull/114))
- Contributor portraits from GitHub, GitLab.com, Gravatar or a configured image,
  fetched only with `--fetch-avatars` and cached for offline playback.
  ([#65](https://github.com/owenlamont/codestrand/pull/65),
  [#80](https://github.com/owenlamont/codestrand/pull/80))
- Contributor names stay readable in crowded histories, move slowly around their
  avatars, and file and directory names stay visible at close zoom.
  ([#91](https://github.com/owenlamont/codestrand/pull/91),
  [#104](https://github.com/owenlamont/codestrand/pull/104))
- Stream exports to video through an installed FFmpeg with `--video`.
  ([#82](https://github.com/owenlamont/codestrand/pull/82))
- Export at any native size with `--resolution`, and drive the export camera from a
  TOML keyframe file with `--camera-path`.
  ([#127](https://github.com/owenlamont/codestrand/pull/127),
  [#108](https://github.com/owenlamont/codestrand/pull/108))
- Preview starts from an overhead camera, aims first-person with a captured mouse,
  toggles borderless fullscreen with F11 and opens a resolution settings panel with
  F10.
  ([#102](https://github.com/owenlamont/codestrand/pull/102),
  [#136](https://github.com/owenlamont/codestrand/pull/136),
  [#129](https://github.com/owenlamont/codestrand/pull/129))
- User documentation at <https://codestrand.pages.dev/>, with a feature montage on
  the README.
  ([#70](https://github.com/owenlamont/codestrand/pull/70),
  [#72](https://github.com/owenlamont/codestrand/pull/72),
  [#131](https://github.com/owenlamont/codestrand/pull/131))
- Third-party licence notices in every release artifact.
  ([#67](https://github.com/owenlamont/codestrand/pull/67))

### Changed

- Preview and export play the current repository by default.
  ([#153](https://github.com/owenlamont/codestrand/pull/153))
- Exact timeline time preserves frame timing throughout long histories.
  ([#153](https://github.com/owenlamont/codestrand/pull/153))
- Truncated imports are refused unless `--allow-truncated-history` is set; planning
  is refused when its memory estimate exceeds available memory.
  ([#153](https://github.com/owenlamont/codestrand/pull/153))
- `--contributor-travel auto` walks short trips and fades longer ones;
  `--contributor-travel walk` always walks, alongside the `--bot-travel` choice.
  ([#153](https://github.com/owenlamont/codestrand/pull/153))
- Repository `ended` dates include the whole named UTC day.
  ([#153](https://github.com/owenlamont/codestrand/pull/153))
- Installed release wheels must render two frames before publication.
  ([#153](https://github.com/owenlamont/codestrand/pull/153))
- Directory layout is planned over the whole history, so directories never overlap
  and file movement speed is capped.
  ([#66](https://github.com/owenlamont/codestrand/pull/66))
- File glyphs lie flat on the file plane; avatar portraits still face the camera.
  ([#107](https://github.com/owenlamont/codestrand/pull/107))
- Faster preview on large repositories.
  ([#112](https://github.com/owenlamont/codestrand/pull/112))
- Faster export, with frames unchanged: PNG frames are encoded off the render thread
  and each frame needs fewer render passes. Preview no longer redoes file and name
  work on frames where playback has not advanced.
  ([#142](https://github.com/owenlamont/codestrand/pull/142))
- Export renders each frame once instead of twice.
  ([performance issue 143](https://github.com/owenlamont/codestrand/issues/143))
- Faster preview in crowded histories: placing contributor names costs about a
  third as much, with frames unchanged.
  ([performance issue 143](https://github.com/owenlamont/codestrand/issues/143))
- New binary-helix logo.
  ([#61](https://github.com/owenlamont/codestrand/pull/61),
  [#69](https://github.com/owenlamont/codestrand/pull/69))

### Fixed

- Duplicate repository paths now report an error naming both input spellings.
  ([#152](https://github.com/owenlamont/codestrand/pull/152))
- Moving translucent surfaces sort at their current positions, fixing stale overlap
  order after movement and material changes.
  ([performance issue 143](https://github.com/owenlamont/codestrand/issues/143))
- Close the six security audit findings: caches and local avatars reject links and
  path escapes, history import and configuration files have fixed size budgets, and
  terminal diagnostics escape control characters.
  ([#92](https://github.com/owenlamont/codestrand/pull/92),
  [#95](https://github.com/owenlamont/codestrand/pull/95),
  [#96](https://github.com/owenlamont/codestrand/pull/96))
- Repeated PNG exports are byte-identical on the same machine, graphics driver,
  backend and build with unchanged history, options and portrait assets, including
  directory intersections that previously varied by single pixels.
  ([#113](https://github.com/owenlamont/codestrand/pull/113),
  [performance issue 143](https://github.com/owenlamont/codestrand/issues/143))
- Diff fragments and arriving files follow each theme's beam curves.
  ([#130](https://github.com/owenlamont/codestrand/pull/130))
- Contributor names with accented, Cyrillic or other non-ASCII characters render
  through bundled IBM Plex Mono and installed system fonts.
  ([#122](https://github.com/owenlamont/codestrand/pull/122))

### Removed

- The built-in demo user mode and positional commit-range controls; use a local
  repository and inclusive date bounds instead.
  ([#153](https://github.com/owenlamont/codestrand/pull/153))

## v0.1.1

### Added

- npm platform packages and trusted publishing for `@owenlamont/codestrand`.
  ([#56](https://github.com/owenlamont/codestrand/pull/56))

### Changed

- Smaller release binaries using ryl's release profile.
  ([#58](https://github.com/owenlamont/codestrand/pull/58))

### Fixed

- Keep overlapping contributor names visible.
  ([#57](https://github.com/owenlamont/codestrand/pull/57))

## v0.1.0

### Added

- Project logo for the README.
  ([#10](https://github.com/owenlamont/codestrand/pull/10))
- Bevy rendering feasibility probe with neon file and contributor visuals.
  ([#4](https://github.com/owenlamont/codestrand/pull/4))
- Import local Git history and inspect events as JSON.
  ([#15](https://github.com/owenlamont/codestrand/pull/15))
- Play history with continuous layouts, compact directories, file icons and
  contributor avatars; merge contributor identities by email.
  ([#25](https://github.com/owenlamont/codestrand/pull/25),
  [#40](https://github.com/owenlamont/codestrand/pull/40),
  [#44](https://github.com/owenlamont/codestrand/pull/44),
  [#45](https://github.com/owenlamont/codestrand/pull/45))
- Colour directories by top-level subtree and cache resolved playback timelines.
  ([#35](https://github.com/owenlamont/codestrand/pull/35),
  [#39](https://github.com/owenlamont/codestrand/pull/39))
- Interactive preview camera modes and offline video export proof.
  ([#43](https://github.com/owenlamont/codestrand/pull/43),
  [#16](https://github.com/owenlamont/codestrand/pull/16))
- Standalone binaries, Python wheels and crates.io packaging for Codestrand,
  licensed under MIT OR Apache-2.0.
  ([#50](https://github.com/owenlamont/codestrand/pull/50),
  [#53](https://github.com/owenlamont/codestrand/pull/53),
  [#54](https://github.com/owenlamont/codestrand/pull/54),
  [#55](https://github.com/owenlamont/codestrand/pull/55))

### Changed

- Speed up directory disc separation with a spatial grid.
  ([#51](https://github.com/owenlamont/codestrand/pull/51))

### Fixed

- Keep contributor avatars above projected file tops.
  ([#41](https://github.com/owenlamont/codestrand/pull/41))
