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
The multilingual export regression also needs Devanagari and CJK fonts. On Ubuntu,
install `fonts-noto-core` and `fonts-noto-cjk`; the CI jobs install both packages.

Before tagging, update `CHANGELOG.md` and regenerate third-party notices after
dependency changes. The release workflow checks notice freshness and extracts the
matching changelog section:

```sh
uv run --script scripts/licenses.py
uv run --script scripts/changelog.py extract v0.1.1
```

Dependabot groups Cargo requirement and GitHub Actions updates into one weekly PR.
The weekly update workflow separately refreshes Cargo.lock, third-party notices and
prek hook pins.

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

### README montage

`docs/assets/montage.webp` appears in the README and on the docs home page. It is
excluded from the crate, PyPI and npm packages. Regenerate it with a release build,
FFmpeg with libwebp, and complete clones at the commits pinned in the script:

```sh
uv run --script scripts/readme_montage.py target/release/codestrand /path/to/clones /path/to/new-output --fetch-avatars
cp /path/to/new-output/montage.webp docs/assets/montage.webp
```

Shots, themes, pacing and trims are in `scripts/readme_montage.py`; camera paths and
the contributor manifest are in `scripts/readme_montage/`. Portraits are fetched live
from GitHub and are not committed, so faces can differ between runs. Inspect the
exported frames before replacing the image.
