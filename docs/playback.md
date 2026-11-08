# Playback

Preview loops the selected history. Export renders the same launch timeline at
explicit frame times, with a fixed camera and a 1x playback multiplier.

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

`--theme neon|phosphor` selects the starting theme for preview and export;
the default is `neon`. Switching themes in preview changes only colours and icons:
layout, playback position and camera stay put.

## File heat

Each non-deletion touch adds heat based on added plus removed lines. Logarithmic
damping keeps large changes from overwhelming small ones; repeated touches
accumulate up to a cap.
Neon and Phosphor brighten file icons through 32 shared brightness levels while
cold icons remain readable. Heat below 0.001 settles to zero. Activity halos pulse
on every touch and decay independently with a 1.6-second time constant, unaffected
by diff size or the heat half-life.

`--heat-half-life 3` sets the cooling half-life in canonical playback seconds for
preview and export (default `3`, accepted range `0.01` through `86400`). Live speed
keys speed up cooling along with the rest of playback. Export cooling uses explicit
frame times. Files settled before `--start` begin cold.

```sh
codestrand export frames --heat-half-life 3 --frames 90 --fps 30
```

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
flicker near the boundary. Recent activity expands the affected subtree and
its ancestors until the activity cools. Files and folders scale between the
two views over 0.3 playback seconds.

Aggregation changes rendering only. Every directory and file keeps the same
simulation and position at every zoom, including export. Top-level directory
labels get priority over other directory landmarks; labels that would overlap
are omitted to keep the overview readable.
