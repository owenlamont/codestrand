# First use

## Play the demo

```sh
codestrand preview
```

The built-in synthetic history includes moves, deletions and a crowded 65-file
commit. No repository or network access is needed. Escape closes the window.

## Play a local repository

```sh
codestrand preview --repository /path/to/repository
```

Replace the path with a local working or bare repository. Quote paths containing
spaces. Codestrand reads committed history without changing the repository;
uncommitted working-tree edits are not shown.

To choose a revision and a smaller playback window:

```sh
codestrand preview --repository /path/to/repository --revision HEAD --start 0 --commits 20
```

`--start` counts commits to lay out and settle before playback. `--commits` limits
commits played after the start. The default is the rest of the history.

## Next steps

- [Camera controls and pacing](../playback.md).
- [Import and inspect history](../history.md).
- [Export PNG frames](../export.md).
- [Map contributor names and fetch portraits](../contributors.md).

From a source checkout, replace `codestrand` with `cargo run --locked --`.
For smoother rendering, use `cargo run --locked --release --`.
