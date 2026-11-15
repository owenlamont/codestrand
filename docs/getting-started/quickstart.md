# First use

## Play the current repository

```sh
codestrand preview
```

Run the command inside a Git working tree or one of its subdirectories. Codestrand
searches upward for the repository. Outside a repository, pass `--repository PATH`.
Escape closes the window.

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

`--start` is the inclusive zero-based position after settling earlier commits.
`--commits` gives a count: the start is inclusive and start plus count is exclusive.
The default is the rest of the history.

## Next steps

- [Camera controls and pacing](../playback.md).
- [Import and inspect history](../history.md).
- [Export PNG frames](../export.md).
- [Map contributor names and fetch portraits](../contributors.md).

From a source checkout, replace `codestrand` with `cargo run --locked --`.
For smoother rendering, use `cargo run --locked --release --`.
