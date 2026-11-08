# Troubleshooting

## Command not found or missing options

Open a new terminal after installing and confirm the installation directory is on
`PATH`. Use `codestrand --version` and `codestrand --help` to check which binary
runs. The guides track current main; upgrade an older release if an option is
missing. See [installation](getting-started/installation.md) for supported targets.

## Repository or revision cannot be read

Use a local repository path rather than a remote URL. Quote paths with spaces and
check the revision exists locally. Obtain a complete clone: shallow or partial
repositories can lack commits or objects needed for import. Inspect without
starting the renderer:

```sh
codestrand inspect /path/to/repository --revision HEAD
```

For stale or damaged playback cache data, retry with `--no-cache`. Inspection
always reads directly from Git. A small `--commits` window limits playback but
still imports the history; the first load of a large repository can take time.

## Graphics window or export fails

Both preview and export need a working graphics adapter. Preview also needs a
windowing environment. Update graphics drivers and try a local desktop session
for preview. `inspect` works without a GPU.
For slow source-checkout playback, use `cargo run --locked --release -- preview`.

## Source build fails

Install the Rust toolchain and native build prerequisites. Linux release builds
install ALSA, systemd/udev, X11, Xcursor, Xi, Xrandr and xkbcommon development
packages. Package names differ by distribution. Windows source builds need the
MSVC C++ build tools; macOS builds need the Xcode command-line tools.

## Export directory already exists

Choose a new directory for each export. Codestrand rejects an existing directory
rather than overwriting frames. Match FFmpeg's input framerate to the export's
`--fps`; see [export](export.md).

## Contributor configuration or portraits fail

An explicitly selected `--contributors` file must exist. Unknown TOML keys,
malformed TOML and duplicate emails are errors. A missing default file is allowed.
See the [configuration example](contributors.md).

Portraits stay offline unless `--fetch-avatars` is supplied. Network errors and
rate limits leave coloured spheres or previously cached images available.
`--no-cache` disables portrait reuse as well as history caching. Synthetic history
never fetches portraits. Private GitHub repositories need an API token with access.

## Report a problem

Terminal diagnostics escape control characters in paths and dependency error chains.
For example, an escape character is printed as `\u{1b}` rather than changing terminal
colour or interpreting a terminal command.

Include the version, operating system, exact command and error text in a
[GitHub issue](https://github.com/owenlamont/codestrand/issues). Remove private
repository paths, author identities and credentials before sharing logs or JSON.
