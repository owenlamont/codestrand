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
options as preview. The camera uses the default fit-to-scene view. Explicit frame
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
