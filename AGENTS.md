# Codestrand

Git history visualisation experiment, public and licensed MIT OR Apache-2.0.
The README separates the rendering probe from future goals. Keep Git import,
rendering and export work in separately scoped issues.
Reserve `docs/` for user-facing documentation rendered to HTML; keep research and
proof reports on issues.

## Workflow

- Use a feature branch and PR for changes; never merge without Owen's approval.
- Publish a release only with Owen's approval.
- Use the installed engineering, Rust, docs and review skills as applicable.
- Review a PR before requesting merge approval, including the adversarial round.
- Stay near current stable Bevy releases; handle upgrades in feature branches.
- Read the [historical rendering reference](https://github.com/owenlamont/codestrand/issues/12#issuecomment-6628967300)
  and upstream examples matching the current Bevy dependency before changing renderer code.

## Checks

Zensical renders user documentation from `docs/` with `zensical.toml`.
Build: `uv run --locked --only-group docs zensical build --clean --strict`.
Preview: `uv run --locked --only-group docs zensical serve`.
The docs group in `pyproject.toml` and `uv.lock` avoids building the Rust package.
Prek and CI validate the strict build and links with lychee. Keep new pages in
navigation and verify CLI examples against current help and behaviour.
See [Contributing](CONTRIBUTING.md#documentation) for the build and hosting setup.

Keep fixtures small and readable; build inline instead of committing recorded history.

```sh
prek install --install-hooks --overwrite
prek run --all-files
cargo nextest run --locked --all-targets
cargo run --locked -- --help
```

Rust 1.99.1 is pinned with rustfmt and Clippy. Hook commands are shared with CI;
Cargo configuration and toolchain changes must trigger the Rust checks too.
Markdown prose wraps at 88 characters. CPU tests run without a GPU; changes to the
renderer also need a local export and visual inspection.

## Who Codestrand is for, as far as known

Owen is the first user and speaks for himself, not for other users.
He runs Codestrand on work repositories whose code must stay on his machine.
Large histories across many repositories should play without tuning the planner.
Options describe simple, observable properties of the result.
Do not expose controls that trade planning time against memory;
a future optional maximum-memory bound is the exception he has named.
A feature needs a precedent in a comparable tool; Gource is the nearest reference.
Every range bound says whether it is inclusive or exclusive.
Revise this model when real user feedback supplies better evidence.
Check each new option or default against this question: does it help Owen get the
history he wants to watch without exposing planner implementation details?

## Priorities

When qualities conflict, earlier wins; a later one still counts in a lesser form.

1. **Security.** A hostile repository, contributors file, cache or network response
   never executes code, writes outside the cache and the chosen output, follows a
   symlink out of them, or grows memory or time without bound. Network access needs
   an explicit opt-in. Analysis that hits a bound stops and reports less.
2. **Runtime speed.** A speed claim names its baseline, workload and run-to-run
   variance on a release build; added code must buy more speed than it adds.
3. **Low peak memory.** When speed wins, report the memory cost.
4. **Simple, idiomatic code.** Less code and fewer branches wherever the above hold.
5. **Fast CI and local feedback.**

ryl's [priorities](https://github.com/owenlamont/ryl/blob/main/docs/design/decisions.md#priorities)
are the fuller model this follows.

## Design constraints

Preserve file identity and spatial continuity across commits. Treat every commit
as one coordinated event connecting a developer to all affected files. Keep the
scene mostly planar with avatars above it. Drive export from explicit frame times,
not wall-clock rendering speed. Prototype visual readability before expanding scope.
Explore steady neon glow without strobing; visual effects must preserve readability.

### Motion budget

Apply these priorities in order when planning contributor motion.

1. Keep planning bounded and rays local. Every arriving file and directory travels
   on a visible ray for its whole flight. In world coordinates, the contributor is
   beside the target repository and the ray crosses no other tile. Keep the
   contributor stationary and visible until its last arrival lands. Report the
   limiting event if the hard constraints cannot fit within the planning bounds.
2. Keep motion smooth and within the configured speed. Avoid fast reversals,
   repeated seat swaps and jitter. Keep names and titles steady and whole-view
   motion slow and rare.
3. Move avatars to serve commits. Prefer the shortest usable gutter route, progress
   once underway and little added playback delay. An extra walk around a tile or
   a delayed departure can make room for requirements with higher priority.
4. Spread contributors around the repository and avoid sustained overlap between
   seated avatars where space permits. Brief overlap as one avatar passes another
   is acceptable. Avoid covering avatars or labels when that can be done without
   compromising smoother motion.

Spend most motion on rays, heat and file changes. Check new motion in an exported
movie alongside the metrics.
