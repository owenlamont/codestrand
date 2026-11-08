# Playback

Preview loops the selected history. Export renders the same launch timeline at
explicit frame times, with a fixed camera and a 1x playback multiplier.

Commit application times are precomputed for the selected history and pacing.
The caption shows the last applied commit's date; cooling and fades advance in
playback seconds. Seeking or restarting replays the same fixed simulation ticks.

## Controls

| Input | Action |
| --- | --- |
| Escape | Close preview |
| `[` | Halve live speed, down to 0.125x |
| `]` | Double live speed, up to 8x |
| `,` / `.` | Previous or next theme |
| WASD | Pan; return from contributor follow to free camera |
| Right mouse drag | Orbit |
| Scroll wheel | Zoom |
| Tab | Follow or cycle visible contributors |
| Space | Restore the fit-to-scene camera over half a second |

The caption shows the live multiplier, theme, camera mode and follow target. Following
returns to free camera when the target fades out. Preview controls do not change
the exported camera or timeline.

## Themes

`--theme` selects the starting theme for preview and export;
the default is `neon`. Switching themes in preview changes styling:
layout, playback position and camera stay put.

The seven themes are `neon`, `phosphor`, `outrun`, `blueprint`,
`bevelled-desktop`, `atlas` and `forge`. Choose one for preview or export:

```sh
codestrand preview --theme outrun
codestrand export frames --theme atlas --frames 90 --fps 30
```

Outrun uses pink tiles, arched beams and a banded sunset. Blueprint uses hatched
plates and construction lines. Bevelled Desktop uses page icons and static
black/white dashed beams. Atlas uses survey points, paper contours and tinted
top-level territories. Forge uses iron plates coloured by file heat.
Blueprint and Atlas use ink without additive glow. All grounds and dash patterns
are steady; switching with `,` and `.` preserves playback and file positions.

## File heat

Each non-deletion touch adds heat based on added plus removed lines. Logarithmic
damping keeps large changes from overwhelming small ones; repeated touches
accumulate up to a cap.
Neon and Phosphor brighten file icons through 32 shared brightness levels while
cold icons remain readable. Heat below 0.001 settles to zero. Activity halos pulse
on every touch and decay independently with a 1.6-second time constant, unaffected
by diff size or the heat half-life.

Forge maps the same 32 heat buckets from dark iron through red and orange to
white heat. Only warm plates contribute to thresholded bloom. Blueprint, Atlas
and Bevelled Desktop keep constant ink brightness; touched files still receive
activity outlines.

`--heat-half-life 3` sets the cooling half-life in canonical playback seconds for
preview and export (default `3`, accepted range `0.01` through `86400`). Live speed
keys speed up cooling along with the rest of playback. Export cooling uses explicit
frame times. Files settled before `--start` begin cold.

```sh
codestrand export frames --heat-half-life 3 --frames 90 --fps 30
```

## Multiple repositories

Repeat `--repository` to play local repositories on one author-time timeline:

```sh
codestrand preview --repository /path/to/first --repository /path/to/second
```

Alternatively, pass `--repositories-file repositories.toml` to preview or export:

```toml
[[repositories]]
path = "/path/to/first"
label = "First project"

[[repositories]]
path = "../second"
```

Paths in the TOML file resolve relative to the file. Labels default to the repository
directory name. Repeated flags and a file can be combined; flag entries come first.
Duplicate repositories and labels are rejected. Each entry is a table so future repository
metadata can be added; archive dates are not supported yet.

Events merge by comparing the next commit from each repository by author timestamp,
breaking ties by repository input order. Each repository retains its first-parent
order, including children authored before their parents. One `--revision` applies
to every repository; `--start` and `--commits`
select from the merged history. Single-repository playback retains first-parent
order. Contributor mappings apply across all repositories, so a matched contributor
uses one avatar that moves between regions.

Live repositories occupy separate regions packed towards the 16:9 export frame.
Regions reserve growth slack and repack when repositories appear or outgrow their
space. Repacking inserts six seconds into playback: file motion pauses while regions
move through separate lanes, then resumes. Region movement uses explicit playback
times, including during export. The camera follows the live regions.

To choose visual treatments, use
`--repository-boundary none|glass` and `--repository-label-plane files|raised`.
Defaults are `glass` and `raised`. Glass is a faint, coloured rectangular prism around
each live region; raised labels stay at a constant height of 1.6 scene units.
Both options apply to preview and export and affect multiple repositories only.

## Range and pacing

```sh
codestrand preview --repository /path/to/repository --start 0 --commits 20 \
  --seconds-per-day 2 --max-idle-seconds 1
```

- `--start`: commits laid out and settled before playback; default `0`.
- `--commits`: commits to play after the start; default all remaining commits.
- `--seconds-per-day`: timeline seconds per calendar day; default `0.5`.
- `--max-idle-seconds`: maximum timeline gap between commits; default `1`.

Pacing preserves within-day timestamp spacing. Coincident, reversed or very close
timestamps get a 0.05-second minimum gap, limited by the idle cap, preserving commit
order. Seconds per day accepts `0.01` through `86400`; the idle cap must also be at
least one simulation tick (1/60 second).

Live speed keys move through the canonical timeline faster or slower without
changing commit times, layout, pulses, beams, avatar motion, settling or fades in
timeline seconds. Changing live speed preserves the current position.

Commits affecting more than 24 files aim beams at directories to reduce clutter.

## Overview detail

Small subtrees containing multiple files collapse into folder glyphs below a 32-pixel screen
footprint. Zooming in expands the subtree above 40 pixels; the gap prevents
flicker near the boundary. Each touch expands the affected subtree and
its ancestors for 2.5 playback seconds, independently of heat. Files and folders scale
between the two views over 0.3 real seconds in preview and 0.3 playback seconds in export.
Footprints use logical pixels and a bounding circle to stay stable during orbit.

Aggregation changes rendering only. Every directory and file keeps the same
simulation and position at every zoom, including export. Directory landmarks rank by
subtree size, with at most four extra top-level labels beyond the usual twenty.
Labels inside fully collapsed subtrees and overlapping directory labels are omitted
to keep the overview readable.

In repository playback, sampled added lines descend from the contributor; removed
lines rise from the file and smoothly fade to transparent. The camera-facing snippet
is the fragment, following the same path, phase and scale envelope above beams, with
a text shadow for contrast. Text and plain prisms crossfade; fully visible snippets
replace their prisms. Extra fragments stay plain. Every theme supplies added/removed
colours; Atlas uses dark ink on its light ground. Snippets use alpha blending and the
theme's monospace font, with IBM Plex Mono for Atlas code beside its serif labels.
Theme switching restyles active snippets without restarting their motion or fade.

Snippet text keeps the font's natural proportions with equal X and Y scaling.
Lines are cut at 88 characters; the fixed uniform size fits 88 characters plus the
direction prefix within eight beam diameters, before the fragment scale envelope. Text
never grows to a minimum screen size: samples smoothly appear between 24 and 40
projected beam-diameter pixels, requiring the camera to be very close to the fragment.
Overlapping snippets may overlap freely without collision or viewport-edge hiding.
Files inside collapsed subtrees show no sample text; LOD keeps its fragment behaviour.

Samples come from actual changed lines: up to three per file and twelve per commit,
preferring lines of 16–60 bytes. Lines over 72 bytes before trimming, non-ASCII text,
controls and trivial lines are omitted. Binary files and submodules have no samples.
Selection is deterministic; crowded commits use path order to spend the budget.
At most twelve samples appear at once.

**Source lines can contain secrets or personal information.** Review exports before
sharing. Sensitive paths (`.env*`, `*.pem`, `*.key`, `id_rsa*`) and common credential
patterns are skipped, but filtering cannot guarantee redaction. Use
`--no-diff-samples` for preview or export to hide all sampled source lines. The
opt-out controls playback; bounded samples may still exist in the history cache.

The history cache stores these bounded samples. Older cache versions are rebuilt
automatically from local Git history.
