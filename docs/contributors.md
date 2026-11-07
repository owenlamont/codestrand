# Contributors and avatars

Preview and export read `contributors.toml` from the user configuration directory:

- Linux: `~/.config/codestrand/` (or `$XDG_CONFIG_HOME/codestrand/` when set).
- macOS: `~/Library/Application Support/codestrand/`.
- Windows: `%APPDATA%\codestrand\`.

Override the file with `--contributors /path/to/contributors.toml`:

```toml
[[contributor]]
name = "Owen Lamont"   # display name
emails = ["owen@personal.example", "owen@work.example"]
```

Emails match case-insensitively; duplicate emails are errors. Matching names never merge
people with different nonempty emails. Unlisted emails keep separate identities,
displayed with the name from the last commit in playback order. Commits without an email
are grouped by author name. A missing default file is fine; an explicit missing file,
unknown key or malformed TOML is an error. Edits take effect on cache hits; original
authors and repository history remain unchanged.

## Contributor avatars

Preview and export stay offline by default: only cached portraits are used, with the
coloured sphere as the fallback. Add `--fetch-avatars` to either command to opt in to
network requests before playback starts. Export never fetches during frame capture.

```sh
codestrand preview --repository /path/to/repo --fetch-avatars
codestrand export frames --repository /path/to/repo --fetch-avatars
```

Optional fields in each `[[contributor]]` entry select accounts by the existing email
aliases:

```toml
[[contributor]]
name = "Owen Lamont"
emails = ["owen@personal.example", "owen@work.example"]
github = "owenlamont"
gravatar = "owen@avatar.example"  # optional fallback email; defaults to commit email
```

Resolution tries explicit `github`, a GitHub noreply login, then the commit author's
GitHub account when a remote points to github.com (`origin` preferred). The commit API
is queried at most once per distinct unresolved email per run; results, including
misses, are cached.
`GH_TOKEN` or `GITHUB_TOKEN`, when present, authenticates only GitHub API requests.
Public repositories work without a token (GitHub's unauthenticated limit is 60 requests
per hour); private repositories need a token with repository access. Rate limits and
network failures leave playback available. HTTPS uses Rustls with ring and the operating
system trust store; no bundled certificate roots are used on Linux, macOS or Windows.

If GitHub supplies no image, Gravatar is tried with a SHA-256 hash of the trimmed, lowercase
email and `d=404`, so a missing Gravatar never becomes a generic placeholder. Email
hashes are identifying information: opting in discloses the hash to Gravatar, account
names to GitHub, and the repository/commit to GitHub's API. Raw emails are never sent.

Portraits and account resolutions live under `codestrand/avatars-v2/` in the existing
platform cache directory, keyed by a hash of email, explicit accounts and remote.
Portraits are resized to 128-pixel PNG files beside small JSON account records.
Fetching covers only contributors in the selected playback window, with a 30-second
total network budget and count-only diagnostics. Synthetic history never fetches.
Entries refresh after seven days when fetching is enabled; offline playback can reuse
expired images. Failed refreshes retain cached images. `--no-cache` disables avatar
cache reads and writes too. Delete that directory to clear saved avatars. Circular
portraits retain the contributor colour as a ring; contributors without images keep
the coloured sphere.
