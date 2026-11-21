# Export

## Numbered PNG frames

```sh
codestrand export frames --repository /path/to/repository --start-date 2026-05-01 --end-date 2026-05-31
```

The output directory (`frames` here) must not already exist. Without a repository
argument or manifest, export discovers the repository from the current directory,
searching upward. Export writes `frame-00000.png` onwards and a
`frames.csv` logical-state manifest. Frames default to 1280×720.
The final `glyphs` and `missing_glyphs` columns count visible text-layout glyphs
used for each captured frame. A nonzero missing count indicates unavailable glyphs;
install fonts covering the contributor names' scripts before exporting again.

Select a native render size with `--resolution WIDTHxHEIGHT`:

```sh
codestrand export hd-frames --resolution 1920x1080 --frames 2
codestrand export uhd-frames --resolution 3840x2160 --frames 2
```

Each side accepts 64–8192 pixels, inclusive, with at most 33,554,432 pixels per frame
(including 7680×4320). The graphics adapter must support the selected texture size
and have enough memory. Both PNG and video exports render at the selected size
from the start. No upscaling is used. The default H.264 encoder needs even dimensions;
use PNGs or a suitable custom encoder for odd dimensions.

Export and preview share a 720-logical-pixel-high UI. Text, captions, labels and
diff snippets scale with frame height; wider aspect ratios reveal more scene
without stretching it. Preview renders at the window's physical resolution,
including after resize, maximise or fullscreen and with display scaling enabled.
Display scaling does not multiply the height-based text size a second time.

By default the whole playback is exported at 30 fps. To make a short capture:

```sh
codestrand export short-frames --frames 60 --fps 30
```

`--frames` accepts 2 through 4,294,967,295, inclusive, matching the supported frame
index range; `--fps` accepts 1 through 240, inclusive. An explicit
frame count stops capture after that many frames rather than changing the timeline
speed.

Export accepts the same repository, revision, range, pacing, contributor and cache
options as preview. By default, the camera uses the fit-to-scene view. Explicit frame
times keep export independent of rendering speed. Each capture is confirmed
written before export completes. Export needs a working graphics adapter.

## Progress

Export prints one line to stderr every 10 seconds:

```text
Exported 4510 frames; history 100.6/153.5s (65.6%); video 2:30 of ~6:09 (~11091 frames); 0:50 elapsed, ETA ~1:12
```

The percentage is history rendered so far. Until planning reaches the end of history,
`~` marks a projected video length and ETA: the waits planned so far, scaled to every
commit.
Waits grow as more contributors arrive, so the projection and ETA usually rise later.
When rendering has waited 10 seconds or more for planning, export prints a `Waiting for
planning` line with how long it has waited.
Set `RUST_LOG=info` to see graphics adapter details.

Diff snippets appear in close views, including scripted camera paths. Their text stays
14–20 logical pixels high (inclusive); the twelve-sample limit and collapsed-directory
hiding still apply. Snippets may overlap.

Moving translucent surfaces overlap using their current positions. Halos and glyphs
can look different from older builds.

Repeated PNG exports produce byte-identical frames and `frames.csv` manifests on the
same machine, graphics driver and build, with the same graphics backend, repository
history, options and portrait assets. Overlapping solid shapes of different colours
keep a stable draw order.
Use cached or local portraits offline: leave `--fetch-avatars` off and keep the avatar
cache unchanged between exports. Live fetching can change portraits with network
availability, deadlines and cache expiry. Fetch portraits before making repeatable captures.

## Stream a video to FFmpeg

```sh
codestrand export clip-state --video history.mp4 --frames 60 --fps 30
```

`--video` streams RGB frames directly to your installed FFmpeg instead of writing
PNGs. The required new directory (`clip-state`) still receives `frames.csv`.
The video path is relative to the current working directory. Choose a new video
filename; existing videos are refused. Codestrand never downloads, bundles or
links an encoder.

FFmpeg is found on PATH. `CODESTRAND_FFMPEG` selects another executable;
`--ffmpeg /path/to/ffmpeg` takes precedence. Paths containing spaces are supported
when quoted. Install FFmpeg with `winget install Gyan.FFmpeg` on Windows,
`brew install ffmpeg` on macOS, or your distribution's `ffmpeg` package on Linux.
PNG export works without FFmpeg.

The default encode uses `-c:v libx264 -pix_fmt yuv420p -crf 18 -movflags +faststart`
for an H.264 MP4 suitable for GitHub, X, Bluesky and QuickTime. Before planning,
Codestrand runs FFmpeg and encodes one blank frame at the chosen resolution with the
default options; custom options are not tried until the export. Install a
libx264-enabled build if the default encoder is unavailable, or replace the encoding
options:

```sh
codestrand export lossless-state --video history.mkv --frames 60 \
  --ffmpeg-arg=-c:v --ffmpeg-arg=ffv1
```

Repeat `--ffmpeg-arg=VALUE` for every argument, including option values. Supplying
any arguments replaces **all** default encoding options. Arguments are passed
directly without shell expansion, after the fixed raw RGB input and before the
video filename. Select codecs, filters, pixel format and container options there;
input dimensions and frame rate come from Codestrand. Frame times stay explicit,
independent of encoding speed. Stderr is drained concurrently, and failures
include FFmpeg's diagnostic tail. Frame writes have a 30-second timeout. After the
last frame, Codestrand waits while the output file's size or modification time
changes; 30 seconds without either changing fails the export. Finalizing a large
video can therefore take longer than 30 seconds. Failed exports may leave a
partial video and manifest.

## Encode PNG frames manually

With external
[FFmpeg](https://github.com/FFmpeg/FFmpeg) and its libx264 encoder, encode a 30 fps
sequence:

```sh
ffmpeg -framerate 30 -i frames/frame-%05d.png -c:v libx264 -pix_fmt yuv420p history.mp4
```

Match `-framerate` to the `--fps` used for export. Choose a new output filename;
FFmpeg asks before replacing an existing file. Keep PNGs for re-encoding or remove
them manually after checking the video.

## Export framing

Export uses a static overview that fits the whole field. Add `--follow-activity` to
ease towards the repository of the most recent commit and show more file detail.

```sh
codestrand export activity-frames --follow-activity --frames 90 --fps 30
```

## Scripted camera

`--camera-path` replaces the export overview with TOML keys. Choose either a camera path
or `--follow-activity`. Save the
[example path](examples/camera-path.toml) as `camera-path.toml` in a Git working
tree, then run there:

```sh
codestrand export camera-frames --camera-path camera-path.toml \
  --frames 241 --fps 30 --video camera.mkv --ffmpeg-arg=-c:v --ffmpeg-arg=ffv1
```

The file must be a regular file of at most 1 MiB. It requires `fps` (1–240, inclusive)
and one or more `[[key]]` tables. Each key requires an unsigned `frame`, `eye` and
`focus`. Frames must increase strictly. Key seconds are `frame / file fps`; export
`--fps` samples the same path at its own rate.
A path never changes playback speed or export duration. Frame zero follows the
selected inclusive start date; before the first key and after the last, the endpoint key
continues resolving its targets.

```toml
fps = 30
[[key]]
frame = 30
eye = { point = [0.0, 24.0, 16.0] }
focus = { point = [0.0, 0.0, 0.0] }
[[key]]
frame = 60
eye = { avatar = "Ada", offset = [0.0, 6.0, 8.0] }
focus = { avatar = "Ada" }
```

Each anchor selects exactly one of `point`, `avatar`, `file` or `directory`.
Coordinates and optional `offset` vectors use world units: +Y is up, with files
on the XZ plane. Offsets remain world-aligned. Contributor names match resolved
display names exactly; ambiguous names fail.
File and directory paths match repository-relative layout strings, including case.
An empty directory path selects the repository root. A `repository`
label can qualify a file or directory; without the label, an ambiguous path reports
candidate repositories. Paths are matched, never opened by the camera evaluator.

Targets resolve from the simulation's current world positions on every frame,
including arrival/deletion effects. Avatars retain their simulation positions after
leaving, but have none before first creation. A referenced target without a position
fails with the output frame, key and target name; there is no origin fallback.
The first key must use points or targets already present at playback start. Hold a point
view until a target exists before blending toward that target's key.

A file target follows the same file through renames. A name used by multiple file
identities is ambiguous; choose another historical name. Re-adding after a deletion's
fade completes creates a new identity, so pacing can affect ambiguity.
Directories bind directly to the loaded layout, including planned positions before
appearance and outside the selected playback interval. There is no follow
filter: target motion retains the simulation's fixed 60 Hz ticks, which can show
steps at high output rates. Smoothstep blends the two keys' current eye/focus
positions; a repeated target follows throughout the segment without easing to a stop.

`up` inherits from the preceding key and defaults to `[0, 0, -1]`, suitable for
straight-down views. Normalised up vectors blend with smoothstep; orientation is
constructed from the resolved eye, focus and up, without Euler interpolation.
Opposite consecutive ups fail. Unknown fields, non-finite coordinates, invalid
ordering and unknown targets fail before the GPU, encoder or output directory starts.
Each output frame also rejects an eye below the 1.2-unit camera floor, coincident
eye/focus, or up parallel to the view. A later invalid pose can leave partial output.
Right/forward turns >90°/frame fail; add a key, raise fps, or lengthen the move.

The same inputs, including fps, produce the same logical camera poses regardless
of render delays. Rendered pixels can differ across graphics adapters, backends and
drivers, and slightly between runs. Camera paths affect export only; preview controls
remain independent.
Close-zoom names still enter at 48 pixels and leave below 40 pixels during camera moves.
