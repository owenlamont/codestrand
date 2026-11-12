# Export

## Numbered PNG frames

```sh
codestrand export frames --repository /path/to/repository --commits 20
```

The output directory (`frames` here) must not already exist. Without `--repository`,
export uses the synthetic demo. Export writes `frame-00000.png` onwards and a
`frames.csv` logical-state manifest. Frames are 1280×720.

By default the whole playback is exported at 30 fps. To make a short capture:

```sh
codestrand export short-frames --frames 60 --fps 30
```

`--frames` accepts 2 through 1,000,000; `--fps` accepts 1 through 240. An explicit
frame count stops capture after that many frames rather than changing the timeline
speed. `--frame-delay-ms` adds a wall-clock delay between captures (default `0`);
the delay does not change simulated frame times.

Export accepts the same repository, revision, range, pacing, contributor and cache
options as preview. By default, the camera uses the fit-to-scene view. Explicit frame
times keep export independent of rendering speed. Each capture is confirmed
written before export completes. Export needs a working graphics adapter.

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
for an H.264 MP4 suitable for GitHub, X, Bluesky and QuickTime. Codestrand checks
FFmpeg and the default encoder before rendering. Install a libx264-enabled build
if the default encoder is unavailable, or replace the encoding options:

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
include FFmpeg's diagnostic tail. Frame writes and encoder shutdown each have a
30-second timeout; failed exports may leave a partial video and manifest.

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

## Scripted camera

`--camera-path` replaces the fitted export camera with TOML keys. Download the
[demo path](examples/demo-camera.toml), then export the synthetic history:

```sh
codestrand export demo-state --camera-path docs/examples/demo-camera.toml \
  --frames 241 --fps 30 --video demo.mkv --ffmpeg-arg=-c:v --ffmpeg-arg=ffv1
```

The file requires `fps` (1–240) and one or more `[[key]]` tables. Each key requires
an unsigned `frame`, `eye` and `focus`. Frames must increase strictly. Key seconds
are `frame / file fps`; export `--fps` samples the same path at its own rate.
A path never changes playback speed or export duration. Frame zero follows the
selected `--start`; before the first key and after the last, the endpoint key
continues resolving its targets.

```toml
fps = 30
[[key]]
frame = 0
eye = { avatar = "Ada", point = [0.0, 3.2, 0.0], offset = [0.0, 6.0, 8.0] }
focus = { avatar = "Ada", point = [0.0, 3.2, 0.0] }
up = [0.0, 0.0, -1.0]
```

Each anchor selects `point`, `avatar`, `file` or `directory`. Coordinates and
optional `offset` vectors use world units: +Y is up, with files on the XZ plane.
Offsets remain world-aligned. Semantic anchors can also supply a fallback `point`
(default origin). Contributor names match resolved display names exactly; ambiguous
names fail. File and directory paths match repository-relative layout strings,
including case. An empty directory path selects the repository root. A `repository`
label can qualify a file or directory; without the label, an ambiguous path reports
candidate repositories. Paths are matched, never opened by the camera evaluator.

Targets resolve from the simulation's current world positions on every frame,
including arrival/deletion effects. Avatars retain their simulation positions after
leaving. Before an avatar has a position, or while a file path is absent, the anchor
uses its fallback point plus offset. Files match the authored path, so a rename away
from that path uses the fallback; the camera does not follow rename identity.
Directories use their planned positions even before appearing. There is no follow
filter: target motion retains the simulation's fixed 60 Hz ticks, which can show
steps at high output rates. Smoothstep blends the two keys' current eye/focus
positions; a repeated target follows throughout the segment without easing to a stop.

`up` inherits from the preceding key and defaults to `[0, 0, -1]`, suitable for
straight-down views. Normalised up vectors blend with smoothstep; orientation is
constructed from the resolved eye, focus and up, without Euler interpolation.
Opposite consecutive ups fail. Unknown fields, non-finite coordinates, invalid
ordering and missing targets fail before the GPU, encoder or output directory starts.
Each output frame also rejects an eye below the 1.2-unit camera floor, coincident
eye/focus, or up parallel to the view. A later invalid pose can leave partial output.
Right/forward turns over 45° per output frame fail; add an intermediate key.

The same inputs, including fps, produce the same logical camera poses regardless
of render delays. Pixel equality is limited to the same graphics backend and driver.
Camera paths affect export only; preview controls remain independent. Close-zoom
names still enter at 48 pixels and leave below 40 pixels during camera moves.
