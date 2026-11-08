# Codestrand

![Codestrand logo](https://raw.githubusercontent.com/owenlamont/codestrand/main/assets/codestrand.svg)

A Git history visualiser with persistent file positions, gliding contributors and
animated diff fragments. Preview a local repository or export numbered PNG frames.

**Status: layout study.** Video export uses your installed FFmpeg.
Licensed MIT OR Apache-2.0.

[User documentation](https://codestrand.pages.dev/) covers installation, Git
history, playback, export and contributor avatars.

## Install

```sh
uv tool install codestrand
```

Also available through `cargo install --locked codestrand`,
`npm install --global @owenlamont/codestrand` and
[GitHub releases](https://github.com/owenlamont/codestrand/releases).

## Quick start

```sh
codestrand preview
codestrand preview --repository /path/to/repository
```

The first command plays the built-in demo. Escape closes the window.
See [Contributing](https://github.com/owenlamont/codestrand/blob/main/CONTRIBUTING.md) for development setup and checks.
