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

## Encode a video with FFmpeg

Built-in video encoding is not implemented. With external
[FFmpeg](https://github.com/FFmpeg/FFmpeg) and its libx264 encoder, encode a 30 fps
sequence:

```sh
ffmpeg -framerate 30 -i frames/frame-%05d.png -c:v libx264 -pix_fmt yuv420p history.mp4
```

Match `-framerate` to the `--fps` used for export. Choose a new output filename;
FFmpeg asks before replacing an existing file. Keep PNGs for re-encoding or remove
them manually after checking the video.
