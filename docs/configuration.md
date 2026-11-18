# Config file

`--config FILE` reads `preview` and `export` options from a TOML file, so a run can be
kept and repeated as one file. Each key is a flag name without the leading dashes:

```toml
seconds-per-day = 2
start-date = "2025-01-01"
gutter = 8
caption-style = "chip"
no-diff-samples = true
fps = 60
video = "history.mp4"

[[repositories]]
path = "../first"
label = "First project"

[[repositories]]
path = "../second"
```

```sh
codestrand preview --config codestrand.toml
codestrand export frames --config codestrand.toml --start-date 2025-06-01
```

- Flags on the command line override the file. A repeated flag such as `--repository`
  replaces the file's list rather than adding to it, and any `--repository` or
  `--repositories-file` flag replaces the whole repository list, and the file's list
  is then not read.
- One file serves both commands: an export option such as `fps` is checked but ignored
  by `preview`.
- Relative paths resolve beside the file, as repository paths do. `ffmpeg` is the
  exception, because a bare name is looked up on `PATH`.
- Switches such as `no-cache` take `true` or `false`. A switch set to `true` in the file
  cannot be turned off from the command line.
- Repeatable flags take a list, such as `repository = ["../first", "../second"]`.
- The `[[repositories]]` tables use the
  [repositories file](playback.md#multiple-repositories) format. A file holding them
  cannot also set `repositories-file`.
- Unknown keys and invalid values are errors that name the key.
- The export directory is a positional argument and stays on the command line.

Codestrand reads a config file only when `--config` names it. A file can enable avatar
fetching or choose the FFmpeg executable, so treat one from elsewhere like a script.
