# Playback

Preview loops the selected history and reuses planned windows within the run while
they fit within the replay memory limit. If estimated use exceeds the limit, preview
discards the cached windows, replans as needed, and shows `Replay memory limit reached`
during replanning pauses. Export renders the same launch timeline at explicit frame
times at 1x speed.
Export uses a static overview that fits the scene by default. Use `--camera-path`
for a scripted camera.
New tiles grow over 1.5 seconds before their first commit.

Preview renders at the window's physical resolution, including on HiDPI displays.
Resizing or maximising keeps a 720-logical-pixel-high UI: text scales with height,
and wider windows show more scene. See [Export](export.md) for native output sizes.

Preview opens its window at once and shows planning progress until playback is ready.
Directory packing numbers additional passes when planning retries. After planning,
preview shows `Preparing renderer` until the scene is ready.

If playback is still preparing after a minute, the status shows a warning and
suggests `--start-date YYYY-MM-DD` (inclusive, UTC) when a later start would keep about
the last 1,000 tree-changing commits across the selected repositories. If you already
set `--start-date`, replace it with the suggested date. Preview keeps preparing and
clears the warning when playback starts.

For long histories, the first frame appears while later history is still being
planned. Playback pauses if planning falls behind, then resumes without skipping
history. Camera controls remain active during the pause, and a small label shows
the planning stage and percentage. Rewinds also reuse planned windows.
Export prints the stage and its percentage to standard error at most once a second.

Commit application times are planned ahead for the selected history and pacing.
The caption shows the last applied commit's date; cooling and fades advance in
playback seconds. Seeking or restarting replays the same fixed simulation ticks.
The caption's percentage follows history time, excluding extra walking waits, so it
holds while contributors finish a walk.

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
CLI playback uses the same six fixed columns for single and multiple repositories.

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
| C | Capture or release the mouse while settings are closed |
| Left click in viewport | Capture the mouse while settings are closed |
| Mouse movement (captured) | Aim directly without holding a button |
| Hold left / right button (captured) | Move along world Y up / down; both cancel |
| F10 | Open or close settings; release capture and pause camera controls |
| F11 | Toggle borderless fullscreen on the current monitor |
| F12 | Toggle the preview frame-rate counter (off at startup) |
| `[` | Halve live speed, down to 0.125x |
| `]` | Double live speed, up to 8x |
| `,` / `.` | Previous or next theme |
| W / S | Move forward / backward along camera aim; release contributor follow |
| A / D | Strafe left / right horizontally relative to yaw; release contributor follow |
| Right drag (released) | Orbit with world-Y yaw and pitch clamped to ±90° |
| Scroll wheel | Zoom; scroll the preset list when over the Display tab |
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

F10 settings have two tabs. Playback changes the theme, live speed, caption style and
directory collapsing while playback continues, matching the `,` `.` `[` `]` keys and the
`--theme`, `--caption-style` and `--collapse-directories` options. Export uses only the
command-line options.

The Display tab has a scrollable preset list with a draggable scrollbar. Monitor native
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
aim turns about the eye in free mode. W and S move along the camera's forward and
backward directions, including pitch. At the floor, movement into the plane slides
horizontally along the yaw heading. A and D strafe horizontally at every pitch.
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
`retro-desktop`, `atlas` and `forge`. Choose one for preview or export:

```sh
codestrand preview --theme outrun
codestrand export frames --theme atlas --frames 90 --fps 30
```

Every theme shows each file's type icon. Outrun frames icons in pink tiles and uses
arched beams and a banded sunset fixed on the `-Z` horizon. Blueprint frames icons in
outlined plates and uses construction lines. Retro Desktop draws icons on pages and
uses static black and white dashed beams. Atlas uses paper contours and tinted
top-level territories. Forge cuts icons through iron plates coloured by file heat.
Blueprint and Atlas use ink without additive glow. All seven themes have a sky and a
world-space ground plane beneath the tiles. Neon and Phosphor use subdued grids,
Outrun uses a magenta grid, and Blueprint uses a drafting grid. Tiles cover the grid
within their boundaries. The ground patterns and beam dashes are steady. Switching with
`,` and `.` preserves playback and file positions.

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
and Retro Desktop keep constant ink brightness; touched files still receive
activity outlines.

`--heat-half-life 3` sets the cooling half-life in canonical playback seconds for
preview and export (default `3`, accepted range `0.01` through `86400`, inclusive).
Live speed keys speed up cooling along with the rest of playback. Export cooling uses
explicit frame times. Files settled before the inclusive start date begin cold.

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
distinct repositories must be unique. There is no fixed repository-count limit.
Tiles, gutters, avatars and titles have world-space sizes. The field grows to fit the
content, and the overview camera pulls back to keep the whole field in view.
Tile packing time grows steeply with repository count.

`archived = true` ends a repository after the final commit in the selected revision,
once the commit's activity finishes. `ended = "YYYY-MM-DD"` supplies an explicit UTC
ending date and overrides `archived`, for example after deletion. The repository is
shown through the end of that day, inclusive: `ended = "2026-05-01"` includes all of
1 May UTC and begins ending at 00:00 UTC on 2 May. A commit after the ending in
imported history order is an error, even when a rebased commit has an older author
date. The error names the first offending commit and a valid ending date.
Neither field fetches platform metadata. Without either field the repository remains
visible.
An ending before the selected playback interval is already complete when playback starts.

The ending tile greys and fades in place over 2.5 playback seconds. Files and
directories grey, rise and fade over the same interval.
Neighbours start reclaiming the space as soon as the fade completes. The edges close
over four seconds. Commits due during reclaim wait in order until the edges settle.
If two exits start less than four seconds apart before adding any travel waits, both
take five seconds to keep tile edges moving steadily. Exits that start together count
as one for this rule. The extra second does not extend the overlap test to later exits.
Text fades in place.

## Contributor and file lifecycles

The first commit enlarges the contributor avatar, settling over 1.2 playback seconds.
New files and directories descend from the contributor along a visible ray to their
place in the tree. Deleted files grey, rise and fade over 1.2 seconds.

`--contributor-inactivity-days` accepts 1–36,500 calendar days, inclusive. At the
default of 60 days without a commit across the combined history, Codestrand assumes
the contributor has left. The avatar greys and fades over 1.2 playback seconds.
Another commit revives the same contributor.
Departure is an assumption, not platform knowledge. Deadlines follow the compressed,
padded timeline, using the latest author date reached so reversed dates cannot trigger
an immediate departure. Playback does not extrapolate contributor departures beyond
the final history date.

```sh
codestrand preview --repositories-file repositories.toml \
  --contributor-inactivity-days 60
```

Repository manifests must be regular files no larger than 1 MiB.
Repository paths are limited to 4,096 bytes before filesystem resolution.

Events merge by comparing the next commit from each repository by author timestamp,
breaking ties by repository input order. Each repository retains its first-parent
order, including children authored before their parents. One `--revision` applies
to every repository; `--start-date` and `--end-date`
select from the merged history. Single-repository playback retains first-parent
order. Contributor mappings apply across all repositories, so a matched contributor
uses one avatar that moves between regions.

Live repositories share a field that grows with the amount of content. The overview
camera pulls back to keep the field in view. Files, avatars and gutters keep their world
scale as the camera pulls back. Fly in during preview to see smaller details. The top
wordmark and bottom caption reserve their own strips in the overview.
Shared gutters separate neighbouring tiles, with no contributor band. Each tile's weight
comes from its live file and directory count. A reflow starts when the ratio between a
tile's held and live shares exceeds 1.5 in either direction, or when more than 5% of the
total weight would need to shift between tiles. The weights ease over two seconds
towards the counts at the end of the transition. A commit due during a resize can wait
for the edges to settle. Successive births can defer a pending weight change for up to
two seconds, unless archive reclaim takes priority. Empty active repositories weigh
as one item.
The split tree and aspect limits also affect each tile's area.
Each repository keeps file identity,
local layout and one uniform scale, with a fixed quarter-turn chosen for its history.
Content grows to use the space in roomy tiles and scales down as its tile shrinks.
Zoom anticipates tree growth and changes gradually in the direction of tile motion.
Each repository title fills the tile as far as its proportions and padding allow.
The visible letters are centered on the tile surface underneath files and directories.
The title stays aligned with the tile when you tilt or move the camera. Zooming in can
reveal titles whose letters are too small on screen in the overview. Titles use bundled
IBM Plex Mono and replace unsupported characters with a question mark.
Contributor names use a 12-pixel screen font. Directory and touched-file labels use a
10-pixel screen font and can end in an ellipsis when space runs out.

A newcomer splits one existing tile or subtree. Other repositories retain their
relative order. A reflow can change the split tree between repository arrivals and exits.
Tiles and their contents ease together when the new tree improves area shares and
the gutters stay connected throughout the move.
Splits clamp to a ground-aspect range of 1:3.5 to 3.5:1, inclusive. The opening shows
every repository still active from before the selected range.
If none are active, the first repository has a lead-in tile.
Later tiles grow over 1.5 seconds. Growth can overlap when several repositories
arrive close together. A new tile settles before its first commit. Shrinking neighbours
can show commits during the move. A split that would enlarge a neighbour waits for
its earlier writes to finish. Commits retain their order. Each edge eases at both ends
and travels at steady speed in between.
Between reflows, tiles and shared gutters partition the field.
`--gutter` sets spacing using the world distance represented by a pixel in the
1280×720 reference view. The default is `4` and the range is `0`–`20`, inclusive.
Tiles touch at `0`. Crowd scaling adjusts avatars when many are active.
Contributor names prefer clear space outside the file and directory layout, with a
small margin. Names may overlap content at tile edges when space is tight.
Different content shapes can leave space inside a uniformly fitted tile.

In treemap diagnostics, `largest_empty_rectangle` measures the largest axis-aligned gap
outside the tiles, including the margin. The value is `null` when the rectangle search
reaches its work budget.

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
Other edges provide fallback docks. Avatars spread along edges. Names try nearby clear
lanes at a fixed 12-pixel font size before truncating. Names remain visible with their
avatars. Portrait size accounts for perspective.

Avatar travel follows gutter centrelines and the outer margin. Parking slots
sit on the same network. Routes never cross tile interiors, including unused areas;
portraits may overlap tile edges beside a narrow gutter. Dijkstra minimises network
length. Other avatars have no travel cost: walkers never wait or detour for each other.
Parking still spreads avatars when clear slots are available.
A contributor who stays at a repository keeps a valid parking spot when switching
between author and co-author. Co-authors who are already parked can use separate docks.
A group sharing a berth fades in a full-size circle around its members. The circle
can follow contributors who walk together. If the group is still forming when the
lead-in ends, the author casts from their avatar. An established circle holds its
radius until its last ray finishes. Rays prefer the point on the camera-facing rim
nearest the target on screen, subject to the checks for crossing other tiles.

A contributor can depart once its previous commit's last fragment has arrived and the
moving gutter route can meet the speed limit. Travel overlaps other contributors'
animations; commits remain strictly chronological. Routes stop at corners and accelerate
toward `--avatar-max-speed` world units per second (default `31.4`, range `0.1` through
`86400`, inclusive).
Moving junctions follow the gutter network, with every canonical sample checked against
the speed limit, frame bounds and tile guards. A shared parking point creates no trip.
Contributors follow their parked gutter slot until their inferred calendar departure,
then fade over 1.2 playback seconds. Capped playback releases faded lanes and re-enters
at the next service slot after a long idle gap.

`--contributor-travel auto` is the default: contributors walk short trips and fade
when a longer route would delay the commit. `--contributor-travel walk` keeps them
walking along the shortest safe gutter route, waiting as long as needed.
`--bot-travel fade` is the default for bots; `--bot-travel walk` gives bots the same
travel policy selected for other contributors. Bot classification uses an explicit
`bot` override
in `contributors.toml`, then a GitHub noreply login ending in `[bot]`, then the displayed
name's `[bot]` suffix. Set `bot = true` for an agent without a bot suffix. Parked bots
fade shortly after service while human contributors follow calendar inactivity.
Travel policies also apply to single repositories.
Walking avatars remain visible during travel until an inferred calendar departure.
With `--contributor-travel walk`, walking trips
also remain routed across long idle gaps instead of disappearing and re-entering at a
different repository.
Trips to a repository's first commit use the selected travel policy. Births follow
presentation time, so the gutter can open before the first commit. In `walk` mode,
a later birth can wait when its moving gutter would outrun an active walk.
Calendar departures and content follow history time, which holds during
travel waits. A required walk that cannot fit within the planning budgets reports
an error.
A plan that cannot converge within 32 replans stops with an error.
Dormant contributors follow their previous repository's lane while waiting, so later
tile growth cannot bury the starting point of their next uncapped trip.
The caption holds the last applied date during waiting; heat, fragments and fades
continue in presentation seconds.

```sh
codestrand export frames --repositories-file repositories.toml \
  --contributor-travel walk --bot-travel walk --avatar-max-speed 31.4 --frames 90 --fps 30
```

Planning tests departure ticks from the contributor's fragment completion onward.
Gaps between that contributor's commits absorb travel before padding is needed.
Padding is revalidated as scene geometry changes. The automatic policy uses at most five
padded rebuilds, including one cleanup of unused service padding.
Uncapped padding grows until every walking trip completes, subject to the planning
memory estimate. Each Dijkstra search is limited to 10,000 graph nodes.

Header and caption remain in the outer frame. Other preview aspect ratios use conservative
framing;
free-camera controls remain independent.

Planning snapshots the machine's available physical memory before importing history.
Swap does not count. An estimate above that snapshot is refused, with both values
reported in bytes and MiB. Total RAM stands in for an available reading above it, and
on macOS for a reading of zero, which means the reading failed; a stderr line says so.
If total RAM cannot be read, planning stops.
The estimate uses the actual selected schedule, repository samples, contributor
first arrivals, imported changes, path bytes and directory samples. Variable allocation
sizes are estimates, so the figure is not a process-memory ceiling. Each padded schedule
is checked before rebuilding; the automatic policy does not reserve hypothetical waits.
The same check applies to single repositories.

Use fewer repositories or a later inclusive `--start-date` to reduce the estimate.
The snapshot is not a memory reservation; other processes can consume RAM afterward.
It is the host's available RAM, without container or job-specific memory limits.
Rendering and video encoding need additional memory beyond the planning estimate.
The check is an estimate made before planning, not a guarantee that the process will
fit in memory. It can be conservative on some inputs and too low on others.
Route searches stop at 10,000 nodes; capped playback can fade across when a search
exceeds that bound, while uncapped walking reports an error.

Export releases consumed travel plans. Preview can retain them for replay within the
same run. The shared layout remains available throughout playback. To play a smaller
interval, use inclusive `--start-date` and `--end-date`.

Playback memory and duration checks use the range selected by `--start-date` and `--end-date`;
the layout estimate also includes the files present when the range starts.

`--repository-boundary glass` adds a faint, coloured ground-plane tile for each live
repository in preview and export. Use `--repository-boundary none` to hide the glass.
Repository titles remain on the ground in either mode.

## Range and pacing

Import refuses a history over budget before planning or rendering. The error names
the exhausted budget, the commit and its UTC author date, and a later `--start-date`
whose range fits, or says that none does.

Import budgets are 1,000,000 commits, 2,000,000 changed entries, 20,000,000 diff input
lines and 512 MiB of retained-input accounting per repository.

Single-object and single-tree limits stay fixed, including 16 MiB objects, 2,000,000
expanded tree entries, depth 256, 16,384-byte paths and 64 MiB of expanded path bytes.
The 32 GiB cumulative work ceiling, 8 GiB blob-detail budget and per-diff work limits
also stay fixed. Oversized or expensive blob diffs keep file changes but omit detail;
exhausting the cumulative line budget also omits detail. Date bounds reduce import
work: diffs outside the range are not read. The caption still numbers commits
against the whole first-parent history.

```sh
codestrand preview --repository /path/to/repository --start-date 2026-05-01 --end-date 2026-05-31 \
  --seconds-per-day 2 --max-idle-seconds 1
```

- `--start-date YYYY-MM-DD`: first commit in merged order authored on or after
  00:00 UTC that day, inclusive; playback opens on the tree of the commit before it.
- `--end-date YYYY-MM-DD`: last commit in merged order authored on or before that
  UTC day, inclusive of the whole day. Omitted bounds use the beginning or end of
  history. A reversed range or one containing no commits reports both bounds.

Selection is a contiguous span of merged history. Rebased commits with older author
dates remain in that span; date bounds do not reorder or remove interior commits.

- `--seconds-per-day`: timeline seconds per calendar day; default `0.5`.
- `--max-idle-seconds`: maximum timeline gap between commits; default `1`.

```sh
codestrand preview --repository /path/to/repository --start-date 2026-05-01 --end-date 2026-05-31
```

Pacing preserves within-day timestamp spacing. Coincident, reversed or very close
timestamps get a 0.05-second minimum gap, limited by the idle cap, preserving commit
order. Seconds per day accepts `0.01` through `86400`, inclusive. The idle cap
accepts one simulation tick (1/60 second) through `86400` seconds, inclusive.

Live speed keys move through the canonical timeline faster or slower without
changing commit times, layout, pulses, beams, avatar motion, settling or fades in
timeline seconds. Changing live speed preserves the current position.

Commits that would draw more than 24 beams group them by parent directory. If there are
still more than 24 targets, the deepest groups move up to their parents until the
beam count fits. New files stream down each shared beam, then settle into their own
positions. The spacing tightens for large groups to keep the stream within the
arrival animation. When a commit creates more than 24 directories, each new directory
keeps its own arrival beam. Commit timing stays the same.

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

`--fragment-speed` scales how fast fragments cross their beams in preview and export
(default `1`, about 0.9 playback seconds per crossing; accepted range `0.25` through
`2`, inclusive). `0.5` doubles the crossing time, giving snippets longer to read.
A walking contributor stays until its last fragment lands before leaving for its
next repository, and a slower setting also delays a calendar departure until then.
Tile transitions and walking trips can delay commits, which stay in history order.
Contributors who lead successive commits in the same repository keep their parking
spot while it fits. If the spot no longer fits, they take the shortest gutter path
to another spot. `--contributor-travel auto` may fade a contributor crossing between
repositories.

```sh
codestrand export frames --fragment-speed 0.5 --frames 90 --fps 30
```

Snippet text keeps the font's natural proportions with equal X and Y scaling.
Lines are cut at 88 characters. Samples are hidden at 6 projected beam-diameter
pixels or less and fully visible at 14 or more, with a smooth transition between.
Visible text stays between 14 and 20 logical pixels high, inclusive, making close
oblique shots readable while the overview keeps plain fragments.
Text holds full contrast through the first three quarters of the fragment's alpha
fade, then fades out with the backing.
Overlapping snippets may overlap freely without collision or viewport-edge hiding.
Files inside collapsed subtrees show no sample text; LOD keeps its fragment behaviour.

Samples come from actual changed lines: up to three per file and twelve per commit,
preferring lines of 16–60 bytes, inclusive. Lines over 72 bytes before trimming,
non-ASCII text, controls and trivial lines are omitted. Binary files and submodules
have no samples.
Selection is deterministic; crowded commits use path order to spend the budget.
At most twelve samples appear at once.

**Source lines can contain secrets or personal information.** Review exports before
sharing. Sensitive paths (`.env*`, `*.pem`, `*.key`, `id_rsa*`) and common credential
patterns are skipped, but filtering cannot guarantee redaction. Use
`--no-diff-samples` for preview or export to hide all sampled source lines. The
opt-out controls playback; bounded samples may still exist in the history cache.

The history cache stores these bounded samples. Older cache versions are rebuilt
automatically from local Git history.
