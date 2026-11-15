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
codestrand preview --repository /path/to/repository --revision HEAD \
  --start-date 2026-05-01 --end-date 2026-05-31
```

Both UTC date bounds are inclusive: playback includes the whole named end day.
Choose dates present in the repository. Omit either bound to use the beginning or
end of history; earlier commits are settled before playback.

## Next steps

- [Camera controls and pacing](../playback.md).
- [Import and inspect history](../history.md).
- [Export PNG frames](../export.md).
- [Map contributor names and fetch portraits](../contributors.md).

From a source checkout, replace `codestrand` with `cargo run --locked --`.
For smoother rendering, use `cargo run --locked --release --`.
