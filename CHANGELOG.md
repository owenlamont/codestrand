# Changelog

All notable changes to Codestrand are documented here, following
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

Watch the [feature tour video](https://youtu.be/gmueXAHnvAE).

### Added

- Play several local repositories as tiles on one shared timeline, from repeated
  `--repository` flags or `--repositories-file`, with contributors travelling along
  gutters between tiles. Adds `--travel-cap`, `--bot-travel`, `--avatar-scale`,
  `--gutter`, `--caption-style`, `--repository-boundary` and
  `--repository-label-plane`.
  ([#94](https://github.com/owenlamont/codestrand/pull/94))
- Contributors and repositories appear, depart and fade; `--contributor-inactivity-days`
  sets when a quiet contributor is assumed to have left, and repository lists can mark
  repositories `archived` or `ended`.
  ([#98](https://github.com/owenlamont/codestrand/pull/98))
- Pace playback by calendar days with `--seconds-per-day` and `--max-idle-seconds`;
  `[` and `]` change live preview speed from 0.125x to 8x.
  ([#63](https://github.com/owenlamont/codestrand/pull/63),
  [#66](https://github.com/owenlamont/codestrand/pull/66))
- Six more themes, `phosphor`, `outrun`, `blueprint`, `bevelled-desktop`, `atlas` and
  `forge`, chosen with `--theme` and switched live in preview with `,` and `.`.
  ([#71](https://github.com/owenlamont/codestrand/pull/71),
  [#83](https://github.com/owenlamont/codestrand/pull/83),
  [#130](https://github.com/owenlamont/codestrand/pull/130))
- Files heat up on each touch and cool with `--heat-half-life`.
  ([#77](https://github.com/owenlamont/codestrand/pull/77))
- Sampled added and removed source lines travel beside changed files;
  `--no-diff-samples` hides them and `--diff-text-distance` reveals them from farther
  away.
  ([#78](https://github.com/owenlamont/codestrand/pull/78),
  [#135](https://github.com/owenlamont/codestrand/pull/135))
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

- Directory layout is planned over the whole history, so directories never overlap
  and file movement speed is capped.
  ([#66](https://github.com/owenlamont/codestrand/pull/66))
- File glyphs lie flat on the file plane; avatar portraits still face the camera.
  ([#107](https://github.com/owenlamont/codestrand/pull/107))
- Faster preview on large repositories.
  ([#112](https://github.com/owenlamont/codestrand/pull/112))
- New binary-helix logo.
  ([#61](https://github.com/owenlamont/codestrand/pull/61),
  [#69](https://github.com/owenlamont/codestrand/pull/69))

### Fixed

- Close the six security audit findings: caches and local avatars reject links and
  path escapes, history import and configuration files have fixed size budgets, and
  terminal diagnostics escape control characters.
  ([#92](https://github.com/owenlamont/codestrand/pull/92),
  [#95](https://github.com/owenlamont/codestrand/pull/95),
  [#96](https://github.com/owenlamont/codestrand/pull/96))
- Repeated exports of one input no longer differ across many frames; rare
  single-pixel differences remain.
  ([#113](https://github.com/owenlamont/codestrand/pull/113))
- Accented, Cyrillic and other non-Latin contributor names render through bundled
  IBM Plex Mono and installed system fonts.
  ([#122](https://github.com/owenlamont/codestrand/pull/122))

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
