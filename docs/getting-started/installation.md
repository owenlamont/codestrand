# Installation

Codestrand ships as a native binary. Choose a channel matching your tools.
The guides describe current `main`. Published releases are v0.1.1, which lacks
`--seconds-per-day`, `--fetch-avatars`, the `[` / `]` playback speed keys and the
avatar cache. Until the next release (0.2.0), build from `main` to use these features:

```sh
cargo install --locked --git https://github.com/owenlamont/codestrand --branch main
```

This requires Rust and the native build prerequisites described below.
Check `codestrand --version` and the command's `--help` after installing.

## PyPI

With [uv](https://docs.astral.sh/uv/getting-started/installation/) and Python 3.11+:

```sh
uv tool install codestrand
```

## npm

With Node.js 18+:

```sh
npm install --global @owenlamont/codestrand
```

The wrapper selects a platform binary through optional dependencies. Keep optional
dependencies enabled when installing.

## crates.io

With [Rust](https://rustup.rs/) and the native build prerequisites:

```sh
cargo install --locked codestrand
```

Building from source compiles Bevy and can take several minutes. A checkout uses
Rust 1.99.1, pinned in `rust-toolchain.toml`.

## GitHub releases

Download a ZIP from [GitHub releases](https://github.com/owenlamont/codestrand/releases),
extract the archive and put `codestrand` (Windows: `codestrand.exe`) on your `PATH`.
Keep the included licences and third-party notices when redistributing the binary.
Compare the archive's SHA-256 digest with the release's `SHA256SUMS` file.

## Platforms

PyPI wheels, npm binaries and release ZIPs target:

- Linux with glibc 2.28+: x86-64 and ARM64.
- macOS: Apple Silicon (ARM64).
- Windows: x86-64 and ARM64.

There is no prebuilt Intel macOS or Linux musl binary. Rendering requires a working
graphics adapter; interactive preview also needs a windowing environment. History
inspection does not start the renderer. See [troubleshooting](../troubleshooting.md)
for build and graphics problems.

## Verify

```sh
codestrand --version
codestrand --help
codestrand preview --help
```

Continue with [first use](quickstart.md).
