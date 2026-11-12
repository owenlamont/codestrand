# Contributing

Use a feature branch and pull request. Releases and merges require Owen's approval.
Keep Git import, rendering and export changes separately scoped. Research and proof
reports belong on issues; `docs/` holds user-facing documentation.

## Development

Install [rustup](https://rustup.rs/),
[uv](https://docs.astral.sh/uv/getting-started/installation/),
[prek](https://prek.j178.dev/) and [pixi](https://pixi.sh/).
Rust 1.99.1, rustfmt and Clippy are pinned in `rust-toolchain.toml`.

```sh
prek install --install-hooks --overwrite
cargo run --locked -- --help
prek run --all-files
cargo test --locked --all-targets
```

CPU tests do not create a renderer. Rendering changes also need a local graphics
export and visual inspection:

```sh
cargo test --locked --test render_export -- --ignored
```

CI runs the CPU tests on Linux, macOS and Windows, and a separate Ubuntu job runs the
ignored `render_export` tests on Mesa's lavapipe software Vulkan adapter.

Before tagging, update `CHANGELOG.md` and regenerate third-party notices after
dependency changes. The release workflow checks notice freshness and extracts the
matching changelog section:

```sh
uv run --script scripts/licenses.py
uv run --script scripts/changelog.py extract v0.1.1
```

## Documentation

Zensical renders `docs/` using `zensical.toml`. Follow the locked `docs` dependency
group without building the Rust/PyPI package:

```sh
uv run --locked --only-group docs zensical build --clean --strict
uv run --locked --only-group docs zensical serve
```

Open the local address printed by the server. Check navigation and changed pages.
Generated `site/` and `.venv/` directories are ignored. Update the navigation when
adding pages, wrap Markdown prose at 88 characters and verify CLI examples against
the current binary's help and behaviour. Stage new files before running prek.
Prek runs the strict build and lychee link checks; CI runs the same hooks.

Cloudflare Pages deploys `main` to <https://codestrand.pages.dev/> through its Git
integration, so the repository holds no deployment credentials or workflow. The
Cloudflare build installs the latest Zensical rather than the locked version:
`pip install zensical && zensical build --clean --strict`, output directory `site`.
