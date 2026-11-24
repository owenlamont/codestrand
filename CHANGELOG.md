# Changelog

All notable changes to Codestrand are documented here, following
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

## v0.3.1 - 2026-11-24

### Fixed

- Keep contributor rays beside their target repository and hold contributors in place
  until file and directory arrivals land. New directories stay hidden until their
  arrival rays launch. ([PR #233](https://github.com/owenlamont/codestrand/pull/233))
- Fade idle contributors shortly after their last visible activity while retaining
  their berth until inferred departure.
  ([PR #233](https://github.com/owenlamont/codestrand/pull/233))
- Keep co-author rings facing the camera and preserve the selected theme and diff
  colours across preview loops. ([PR #233](https://github.com/owenlamont/codestrand/pull/233))
- Draw the Outrun sun sharply at the world horizon.
  ([PR #235](https://github.com/owenlamont/codestrand/pull/235))

## v0.3.0 - 2026-11-23

### Added

- An agent skill, installed with
  `gh skill install owenlamont/codestrand codestrand`, and `llms.txt` and
  `llms-full.txt` on the docs site, so coding agents can run Codestrand.
  ([#158](https://github.com/owenlamont/codestrand/issues/158))
- Load preview and export options from a TOML file with `--config`. Command-line
  flags override the file. ([#182](https://github.com/owenlamont/codestrand/pull/182))
- Adjust diff fragment travel speed with `--fragment-speed`.
  ([#190](https://github.com/owenlamont/codestrand/pull/190))
- Export reports history progress, projected video length, ETA and waits for planning.
  ([#192](https://github.com/owenlamont/codestrand/pull/192))
- Co-authors share a ring with the commit author and keep their own contributor
  identities, names and portraits. Contributors who stay at a repository keep a
  valid parking spot when switching between author and co-author.
  ([#191](https://github.com/owenlamont/codestrand/pull/191))
- Change the theme, speed, caption style and directory collapsing in F10 settings.
  ([#198](https://github.com/owenlamont/codestrand/pull/198))
- Preview warns after a minute of startup and suggests a later `--start-date` when
  it would shorten the selected history to about 1,000 tree-changing commits.
  Preview shows `Preparing renderer` while the renderer starts, and planning
  progress numbers directory packing retries.
  ([#223](https://github.com/owenlamont/codestrand/pull/223))
- Toggle a frame-rate counter in preview with F12.
  ([#216](https://github.com/owenlamont/codestrand/pull/216))

### Changed

- Planning does less work before the first frame and between playback windows, and
  retains fewer copies of imported events.
  ([#176](https://github.com/owenlamont/codestrand/pull/176),
  [#206](https://github.com/owenlamont/codestrand/pull/206),
  [#209](https://github.com/owenlamont/codestrand/pull/209))
- Repository tiles share a world-space field that grows with their contents. The
  overhead view fits the field, and export uses that fixed overview by default.
  Neighbouring tiles start reclaiming space as soon as an ended tile fades out.
  Avatar travel speed is set in world units per second.
  ([#214](https://github.com/owenlamont/codestrand/pull/214))
- Tiles reflow as live file and directory counts shift. New tiles can grow together
  just before their first commits. Trees scale to use the available tile space while
  keeping their quarter-turn. Commits can wait in history order for tile motion.
  New files and directories descend from the contributor along visible rays.
  ([#197](https://github.com/owenlamont/codestrand/pull/197),
  [#208](https://github.com/owenlamont/codestrand/pull/208),
  [#216](https://github.com/owenlamont/codestrand/pull/216))
- Repository titles sit on their tiles beneath the files. Contributor names use a
  12-pixel font, and directory and touched-file labels use a 10-pixel font.
  ([#214](https://github.com/owenlamont/codestrand/pull/214),
  [#216](https://github.com/owenlamont/codestrand/pull/216))
- Preview reuses planned windows when looping or rewinding while they fit in memory.
  When estimated use exceeds the limit, preview releases the windows and replans as
  needed.
  ([#221](https://github.com/owenlamont/codestrand/pull/221))
- The desktop theme is now named Retro Desktop (`retro-desktop`).
  ([#185](https://github.com/owenlamont/codestrand/pull/185))
- All seven themes have perspective ground planes and distinct skies.
  ([#230](https://github.com/owenlamont/codestrand/pull/230))
- Export checks its output directory, camera path syntax and video encoder before
  planning, so a typo fails at once rather than after planning; the default `--video`
  encoder is tried on one frame at the chosen resolution first.
  ([#193](https://github.com/owenlamont/codestrand/pull/193))
- Every theme shows file type icons: Outrun, Blueprint and Retro Desktop frame them,
  Atlas shows them bare and Forge sets them in their own colours on its plates.
  Neon, Phosphor and Atlas icons are smaller, so they sit inside their round rings.
  ([#195](https://github.com/owenlamont/codestrand/pull/195),
  [#204](https://github.com/owenlamont/codestrand/pull/204))
- Avatar fetching uses GitHub accounts and configured HTTPS images. Commit lookups
  can group multiple emails for one GitHub account, and portrait data uses a new
  cache. ([#202](https://github.com/owenlamont/codestrand/pull/202))
- A late `--start-date` imports only the requested range: on a full Cargo clone, the
  last seven weeks imported in 0.5 s instead of about 17 s. Playback opens on the tree
  of the commit before the range, so first frames differ.
  ([#194](https://github.com/owenlamont/codestrand/pull/194))
- Import and `inspect` read first-parent commits that change the tree; side-branch
  commits are no longer read. ([#194](https://github.com/owenlamont/codestrand/pull/194))
- History over an import budget is refused with a later `--start-date` when one fits.
  Budget accounting includes retained history and tree summaries.
  ([#194](https://github.com/owenlamont/codestrand/pull/194),
  [#213](https://github.com/owenlamont/codestrand/pull/213))
- `--end-date` stops the import at the range's last commit: on a full Cargo clone, a
  two-frame export ending in 2016 took about 4.5 s instead of 75 s on a release build
  without LTO. Overview framing uses peak content across the selected interval.
  Moving the end date earlier can leave the framing unchanged if the same peak remains.
  ([#199](https://github.com/owenlamont/codestrand/pull/199))

### Fixed

- Walking routes converge more reliably with overlapping co-author rings and moving
  tiles. Export reports the same end tick for eager and streamed playback.
  ([#197](https://github.com/owenlamont/codestrand/pull/197),
  [#200](https://github.com/owenlamont/codestrand/pull/200),
  [#210](https://github.com/owenlamont/codestrand/pull/210))
- Planning handles unavailable physical-memory readings on macOS and explains
  when it uses total RAM. ([#200](https://github.com/owenlamont/codestrand/pull/200))
- A date range numbers commits against the whole history again instead of from 1.
  ([#199](https://github.com/owenlamont/codestrand/pull/199))
- Diff setup no longer resolves `HEAD` or reads the index or `.gitattributes`, so a
  cyclic `HEAD` no longer stops importing another `--revision`.
  ([#199](https://github.com/owenlamont/codestrand/pull/199))
- `codestrand preview` outside a repository exits with "pass --repository PATH"
  before opening a window.
  ([#187](https://github.com/owenlamont/codestrand/pull/187))
- Camera path files are limited to 1 MiB like the other TOML inputs, and every TOML
  input names a FIFO or device as not a regular file instead of waiting on it.
  ([#184](https://github.com/owenlamont/codestrand/pull/184))
- Many repositories starting on the same day no longer stop playback with
  "presence-scaled tiles remain feasible" while their tiles ease in.
  ([#180](https://github.com/owenlamont/codestrand/pull/180))
- A contributor name no longer vanishes for a moment under the top caption bar when its
  avatar jumps to another repository without travelling.
  ([#180](https://github.com/owenlamont/codestrand/pull/180))

### Removed

- `--repository-label-plane`; titles now sit on the tile surface.
  ([#216](https://github.com/owenlamont/codestrand/pull/216))
- GitLab and Gravatar portrait discovery, including `--gitlab-email-avatars`.
  Existing contributor files with `gitlab` or `gravatar` keys now fail at startup.
  ([#202](https://github.com/owenlamont/codestrand/pull/202))
- `--allow-truncated-history`; choose a shorter date range when an import exceeds
  a budget. ([#194](https://github.com/owenlamont/codestrand/pull/194))

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
