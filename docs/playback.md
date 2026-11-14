# Playback

Preview loops the selected history. Export renders the same launch timeline at
explicit frame times, with a 1x playback multiplier and a fitted or scripted export camera.

Preview renders at the window's physical resolution, including on HiDPI displays.
Resizing or maximising keeps a 720-logical-pixel-high UI: text scales with height,
and wider windows show more scene. See [Export](export.md) for native output sizes.

Commit application times are precomputed for the selected history and pacing.
The caption shows the last applied commit's date; cooling and fades advance in
playback seconds. Seeking or restarting replays the same fixed simulation ticks.

Repository captions also name the applying commit's repository, including single-tile
playback. The default
`--caption-style bar` tints the entire caption bar, matching the repository tile hue
and cross-fading for 0.3 playback
seconds, with at least 0.8 seconds between transitions (at least 0.5 seconds steady).
Rapid changes coalesce to the
latest repository when the hold expires; the caption text always updates immediately.
Both treatments use white text on opaque dark tints with at least 5.52:1 contrast.
`--caption-style chip` colours only the repository name's background instead.

Caption columns have fixed widths for the entire playback, including the lead-in.
Widths use the longest scheduled date, repository and resolved contributor name, plus
the total commit counter's digits. Speed and theme columns reserve their longest
available values so preview controls cannot move separators. Captions always use the
embedded monospace Fira Mono font, even when another theme uses proportional labels.
CLI playback uses the same six fixed columns for single repositories and synthetic
history. The synthetic repository column is named `Synthetic history`.

Text falls back from the selected theme font to bundled IBM Plex Mono for accented
and Cyrillic names, then to installed system fonts for other scripts, including Greek.
Install fonts covering the required scripts on export machines; use the same fonts when
comparing frames across machines.
Linux system-font discovery uses Fontconfig. Linux installations with no system fonts
are outside the supported configuration.
macOS discovers installed fonts through CoreText and Library/Fonts directories.

## Controls

| Input | Action |
| --- | --- |
| Escape | Release captured mouse; otherwise close preview |
| C | Capture or release the mouse while display settings are closed |
| Left click in viewport | Capture the mouse while display settings are closed |
| Mouse movement (captured) | Aim directly without holding a button |
| Hold left / right button (captured) | Move along world Y up / down; both cancel |
| F10 | Open or close display settings; release capture and pause camera controls |
| F11 | Toggle borderless fullscreen on the current monitor |
| `[` | Halve live speed, down to 0.125x |
| `]` | Double live speed, up to 8x |
| `,` / `.` | Previous or next theme |
| WASD | Move horizontally relative to yaw; release contributor follow |
| Right drag (released) | Orbit with world-Y yaw and pitch clamped to ±90° |
| Scroll wheel | Zoom; scroll the preset list when over display settings |
| Tab | Follow or cycle visible contributors |
| Space | Restore the fit-to-scene camera over half a second |

The caption shows the live multiplier, theme, camera mode and follow target. Following
returns to free camera when the target fades out. Preview controls do not change
the exported camera or timeline.

Preview starts with the cursor released. Capture hides and locks the cursor; C or
Escape releases capture, and another Escape closes preview. Opening F10 settings or
losing window focus also releases capture. Closing settings or returning to the window
does not recapture: press C or click the viewport again. Clicks in letterbox bars do
not capture. Capture and release discard that frame's mouse motion to avoid a jump.
Mouse aim uses 0.003 radians per mouse count, independent of frame time.
Held mouse buttons move at the WASD speed along world Y, independent of pitch.
Descending stops at floor clearance. The left click that
captures the mouse cannot move the camera until released and pressed again.

Display settings have a scrollable preset list with a draggable scrollbar. Monitor native
is first; other choices are grouped by aspect ratio, then ascending pixel size.
Ratios use conventional display names: 3440×1440 is labelled 21:9.

| Aspect | Presets (physical pixels) |
| --- | --- |
| 16:9 | 1280×720, 1600×900, 1920×1080, 2560×1440, 3840×2160 |
| 16:10 | 1280×800, 1920×1200, 2560×1600 |
| 21:9 | 2560×1080, 3440×1440, 3840×1600 |
| 32:9 | 3840×1080, 5120×1440 |

Choices exceeding either dimension of the current monitor are greyed out with a reason
and cannot be selected. Windowed choices resize the window after restoring normal
window placement; the desktop may further constrain the actual window size.

In borderless fullscreen, presets keep fullscreen and render at exactly the selected
pixel size, centred with black bars around the unused monitor area. No preset is
upscaled. Monitor native fills the display. Captions and other UI stay inside the
rendered viewport, scaled by its height rather than the full monitor height.
F11 restores the previous windowed size and position. Playback continues while
settings are open; mouse wheel input over the panel scrolls instead of zooming.

The default camera looks straight down at the centre of the repository tile area.
Repository tiles are rectangles in world coordinates. Preview stores yaw and pitch
explicitly, with no roll. Yaw rotates about world Y; pitch rotates about the camera's
local right axis and clamps to −90° through +90°, inclusive. The camera can look
exactly straight down or up, and yaw still rotates the view at either pole.
Camera up never opposes world up. The eye stays at least 1.2 world units above the plane,
above the glass and file geometry, and can look up at contributors at height 3.2.
Vertical mouse motion changes pitch; horizontal motion changes yaw. Captured mouse
aim turns about the eye in free mode. WASD uses forward and right from yaw alone,
stays horizontal and remains well-defined when looking straight down or up.
Right-drag orbit remains available with the cursor released.
Orbit, captured aim and zoom retain the followed
contributor as the aim; WASD or button elevation releases follow. Released right-drag
and follow orbit use the same yaw and pitch limits. At the floor, free orbit turns about
the eye instead of crossing the plane; zoom slides along the floor.
Follow shortens its orbit radius at the floor while keeping its aim on the contributor.
Space smoothly raises the eye and restores the
overhead view with yaw 0°, pitch −90° and camera up pointing toward −Z.

Export defaults to the overhead perspective camera at explicit frame times;
[scripted paths](export.md#scripted-camera) can override the view. Default
framing reserves space for elevated avatars and labels; clearance guarantees apply to
the fitted view. Free orbit and zoom can crop content. File glyphs lie flat with upright
artwork in the overhead view. Directory structures, membership rings and activity halos
remain on the file plane.
Avatar portraits face the camera.
The plane cannot be viewed from underneath; glass and file surfaces use backface culling.

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
archived = true

[[repositories]]
path = "../second"
ended = "2026-05-01"
```

Paths in the TOML file resolve relative to the file. Labels default to the repository
directory name. Repeated flags and a file can be combined; flag entries come first.
Paths resolving to the same location are rejected as duplicate repositories. Labels for
distinct repositories must be unique. The combined list is limited to 30
unique repositories before any history is imported or avatars are resolved.

`archived = true` ends a repository after the final commit in the selected revision,
once the commit's activity finishes. `ended = "YYYY-MM-DD"` supplies an explicit UTC
ending date and overrides `archived`, for example after deletion. Neither field fetches
platform metadata. Without either field the repository remains visible.
An ending before the selected playback interval is already complete when playback starts.

The ending tile, files and directories grey, rise and fade over 2.5 playback seconds.
Neighbours then reclaim the space over four seconds. Text fades in place.

## Contributor and file lifecycles

The first commit enlarges the contributor avatar, settling over 1.2 playback seconds.
Whole-file additions descend from the contributor along the beam to the file layout;
deletions grey, rise and fade over 1.2 seconds. Line fragments remain separate.

`--contributor-inactivity-days 60` assumes a contributor has left after 60 calendar
days without a commit across the combined history. The avatar greys and fades over
1.2 playback seconds; another commit revives the same contributor. The two-month default
flags Owen's observed 88-day pause in the combined first-parent histories.
Departure is an assumption, not platform knowledge. Deadlines follow the compressed,
padded timeline, using the latest author date reached so reversed dates cannot trigger
an immediate departure. Playback does not extrapolate contributor departures beyond
the final history date.

```sh
codestrand preview --repositories-file repositories.toml \
  --contributor-inactivity-days 60
```

Repository manifests must be regular files no larger than 1 MiB, with at most 4,096
entries. Repository paths are limited to 4,096 bytes before filesystem resolution.

Events merge by comparing the next commit from each repository by author timestamp,
breaking ties by repository input order. Each repository retains its first-parent
order, including children authored before their parents. One `--revision` applies
to every repository; `--start` and `--commits`
select from the merged history. Single-repository playback retains first-parent
order. Contributor mappings apply across all repositories, so a matched contributor
uses one avatar that moves between regions.

Live repositories share the inset viewport inside a 24-pixel frame margin. The
same frame-scaled avatar sizing and gutter placement apply to single repositories and
synthetic playback. The top wordmark has a separate 26-pixel reserved strip, including
a four-pixel buffer;
avatars, gutter routes and name labels stay below the strip, matching caption clearance.
The default 16:9 export camera stays fixed. Spacing and size floors use a fixed world scale
calibrated in reference pixels; apparent ground spacing is slightly smaller to reserve
room for elevated portraits. Preview orbit changes the projection, not the packing.
Shared gutters separate neighbouring tiles, with no
contributor band. Tile weights follow eased square roots of live file counts;
empty active repositories retain weight one. Each repository keeps file identity,
local layout and one uniform scale, with a fixed quarter-turn chosen for its history.
Content zoom anticipates growth with a precomputed safe-fit envelope and half-second
easing. The log-scale limit is 0.12 per second when zooming out and 0.08 when zooming in
(about 11.3% shrink or 8.3% growth). A larger future contraction can start the
adjustment earlier; the chosen scale never exceeds the tile's current safe fit.
The usable content area leaves an eight-pixel edge inset; corner titles retain their
existing placement rules. Invisible archived content cannot constrain earlier zoom.

A newcomer splits one existing tile or subtree. Other repositories retain their
relative order. Splits clamp to a ground-aspect range of 1:2.5 to 2.5:1 and a
144-by-64 reference-pixel floor. Births ease for up to 0.75 playback seconds
before their first commit; births near the lead-in use the available lead-in time.
Aspect and size clamps scale with presence during the transition. Tiles and shared
gutters partition the whole inset field. `--gutter` sets the visible spacing in
1280×720 reference pixels: default `4`, range `0`–`20`, including touching tiles at
`0`. Avatars remain doubled through five repositories, then taper with a smoothstep
curve to the original diameter at thirty. Crowd scaling also remains active.
Names prefer clear content rectangles, including repository labels
and a small margin, but may overlap content near tile edges when space is tight.
Different content shapes can leave space inside a uniformly fitted tile.

Contributor names orbit at no more than 30 degrees per playback second, with a
three-second minimum between direction reversals. A new position must reduce the
currently occluded text area by at least a quarter for 0.75 playback seconds before
the name moves. Brief overlap is preferable to fast or oscillating text.

Close zoom keeps inactive file and directory names visible. Names appear when the
smaller dimension of the object's projected footprint reaches 48 logical pixels and
remain until the footprint drops below 40 pixels. These names bypass the activity and
landmark limits. Names use fixed world anchors and upright text; long names can overlap.
The same rules apply to preview and export, using each mode's camera.

Contributors first appear in a gutter slot above their repository, aligned with the
projected mean of touched files. Switching repositories chooses the nearest valid edge
instead of requiring a detour to the top. A slot survives consecutive commits while
its beam and frame checks remain valid.
Placement checks the touched files throughout service, including
additions and deletions, for an open half-plane and beams crossing another repository
or its label.
Other edges provide fallback docks. Avatars spread along edges; names shrink to a readable
minimum, then move to free nearby lanes before truncating as a last resort. Names remain
visible with their avatars. Portrait size accounts for perspective.

Avatar travel follows gutter centrelines and the outer margin. Parking slots
sit on the same network. Routes never cross tile interiors, including unused areas;
portraits may overlap tile edges beside a narrow gutter. Dijkstra minimises network
length. Other avatars have no travel cost: walkers never wait or detour for each other.
Parking still spreads avatars when clear slots are available.

A contributor can depart once its previous commit's last fragment has arrived and the
moving gutter route can meet the speed limit. Travel overlaps other contributors'
animations; commits remain strictly chronological. Routes stop at corners and accelerate
toward `--avatar-max-speed` reference pixels/second (default `480`, minimum `1`).
Moving junctions follow the gutter network, with every canonical sample checked against
the speed limit, frame bounds and tile guards. A shared parking point creates no trip.
Contributors follow their parked gutter slot until their inferred calendar departure,
then fade over 1.2 playback seconds. Capped playback releases faded lanes and re-enters
at the next service slot after a long idle gap.

`--travel-cap` limits extra waiting per walking trip (default `0.4` seconds; range `0`
through `3600`, or `none` to wait as long as the route needs). Available gaps between
commits also count toward travel. If a capped trip
cannot finish in time, the avatar moves along the safe route, fades out at the deadline
and fades in at the destination. Blocked capped routes fade across. Zero disables extra
waiting. `--bot-travel fade` is the default; `--bot-travel walk` gives bots the same
routing and waiting policy as humans. Bot classification uses an explicit `bot` override
in `contributors.toml`, then a GitHub noreply login ending in `[bot]`, then the displayed
name's `[bot]` suffix. Travel policies also apply to single repositories and synthetic
playback.
Walking avatars remain visible during travel until an inferred calendar departure.
With `--travel-cap none`, walking trips
also remain routed across long idle gaps instead of disappearing and re-entering at a
different repository. After the last service, the normal inactivity fade still applies.
Uncapped walking services trips in commit order and adds another second of waiting
when tile motion or insufficient travel time blocks a route. New repository tiles get
a full 0.75-second birth window inside their added wait. Packing weights start
approaching the upcoming service footprint during that wait. An uncapped plan
that cannot find safe routes within the planning budgets reports an error instead of
silently accepting a travel fade.
A plan that cannot converge within 32 replans stops with an error.
Dormant contributors follow their previous repository's lane while waiting, so later
tile growth cannot bury the starting point of their next uncapped trip.
The caption holds the last applied date during waiting; heat, fragments and fades
continue in presentation seconds.

```sh
codestrand export frames --repositories-file repositories.toml \
  --travel-cap none --bot-travel walk --avatar-max-speed 480 --frames 90 --fps 30
```

Planning tests departure ticks from the contributor's fragment completion onward.
Gaps between that contributor's commits absorb travel before padding is needed.
Padding is revalidated as scene geometry changes. Numeric caps use at most five
padded rebuilds, including one cleanup of unused service padding.
Uncapped padding grows until every walking trip completes, subject to the existing
216,000-tick, 2,000,000-region-sample and 4,000,000-pose capture budgets. Each Dijkstra
search is limited to 10,000 graph nodes; thirty repositories need at most 3,844.
Numeric caps apply to accumulated extra waiting per commit.

Header and caption remain in the outer frame. Other preview aspect ratios use conservative
framing;
free-camera controls remain independent.

Repository planning supports up to 30 repositories, 216,000 simulation ticks,
2,000,000 region samples and 4,000,000 temporary avatar poses. The preflight includes
worst-case travel padding for numeric caps. With `none`, each actual padded schedule is
checked before rebuilding; exceeding a budget reports an error rather than allocating
an unbounded plan. Route searches stop at 10,000 nodes; capped playback can fade across
when a search exceeds that bound, while uncapped walking reports an error. Use fewer
repositories, shorten the imported history or reduce playback times if an input limit
is exceeded. The same planning limits apply to single repositories and synthetic playback.

Planning happens before the first frame. Large multi-repository histories can take
several seconds to plan. To inspect a smaller interval, select the playback with
`--start` and `--commits`.

Tick and region-sample budgets apply to the playback selected by `--start` and `--commits`.

To choose visual treatments, use
`--repository-boundary none|glass` and `--repository-label-plane files|raised`.
Defaults are `glass` and `raised`. Glass is a faint, coloured ground-plane tile for
each live repository; raised labels stay at a constant height of 1.6 scene units.
Both options apply to preview and export, including single repositories and synthetic
playback.

## Range and pacing

```sh
codestrand preview --repository /path/to/repository --start 0 --commits 20 \
  --seconds-per-day 2 --max-idle-seconds 1
```

- `--start`: commits laid out and settled before playback; default `0`.
- `--start-date YYYY-MM-DD`: begin at the first commit in merged history order authored
  on or after 00:00 UTC that day, settling preceding commits as with `--start`.
  Conflicts with `--start`; dates after the history report the last commit date.
- `--commits`: commits to play after the start; default all remaining commits.
- `--seconds-per-day`: timeline seconds per calendar day; default `0.5`.
- `--max-idle-seconds`: maximum timeline gap between commits; default `1`.

```sh
codestrand preview --repository /path/to/repository --start-date 2026-05-01 --commits 20
```

Pacing preserves within-day timestamp spacing. Coincident, reversed or very close
timestamps get a 0.05-second minimum gap, limited by the idle cap, preserving commit
order. Seconds per day accepts `0.01` through `86400`; the idle cap must also be at
least one simulation tick (1/60 second).

Live speed keys move through the canonical timeline faster or slower without
changing commit times, layout, pulses, beams, avatar motion, settling or fades in
timeline seconds. Changing live speed preserves the current position.

Commits affecting more than 24 files aim beams at directories to reduce clutter.

## Overview detail

Directory collapsing is on by default. Use `--collapse-directories off` in preview
or export to keep every file and directory visible at every zoom. Drawing every file
can reduce frame rate in large repositories. `--collapse-directories on` restores
aggregation. The option affects rendering only; file identity, layout and playback
remain unchanged.

Small subtrees containing multiple files collapse into folder glyphs below a 32-pixel screen
footprint. Zooming in expands the subtree above 40 pixels; the gap prevents
flicker near the boundary. Each touch expands the affected subtree and
its ancestors for 2.5 playback seconds, independently of heat. Files and folders scale
between the two views over 0.3 real seconds in preview and 0.3 playback seconds in export.
Footprints use logical pixels and a bounding circle to stay stable during orbit.

Collapsed folders have the theme's file-glyph footprint at the directory's scale,
centred on the directory and raised clear of file surfaces. Folders shrink with the
repository instead of growing to a minimum screen size.

Aggregation changes rendering only. Every directory and file keeps the same
simulation and position at every zoom, including export. Directory landmarks rank by
subtree size, with at most four extra top-level labels beyond the usual twenty.
Labels inside fully collapsed subtrees and overlapping directory labels are omitted
to keep the overview readable.

In repository playback, sampled added lines descend from the contributor; removed
lines rise from the file and smoothly fade to transparent. The camera-facing snippet
is the fragment, following the same path and phase above beams, with
a text shadow and theme backing for contrast. Text and plain prisms crossfade; fully
visible snippets replace their prisms. Extra fragments stay plain. Every theme supplies
added/removed colours; Atlas uses dark ink on its light ground. Snippets use alpha
blending and the theme's monospace font, with IBM Plex Mono for Atlas code beside its
serif labels.
Theme switching restyles active snippets without restarting their motion or fade.

Snippet text keeps the font's natural proportions with equal X and Y scaling.
Lines are cut at 88 characters. Samples smoothly appear between 6 and 14 projected
beam-diameter pixels by default. `--diff-text-distance 4` reveals samples at four times
the camera range: the same fade spans 1.5–3.5 pixels. Preview and export accept
`--diff-text-distance` from `0.1` through `100`, default `1`; values below `1` require
a closer view. The multiplier affects only the reveal, including scripted export
cameras. Visible text stays between 14 and 20 logical pixels high, making
close oblique shots readable while the overview keeps plain fragments.
Text holds full contrast through the first three quarters of the fragment's alpha
fade, then fades out with the backing.
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
