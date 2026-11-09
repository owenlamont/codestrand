# Contributors and avatars

Preview and export read `contributors.toml` from the user configuration directory:

- Linux: `~/.config/codestrand/` (or `$XDG_CONFIG_HOME/codestrand/` when set).
- macOS: `~/Library/Application Support/codestrand/`.
- Windows: `%APPDATA%\codestrand\`.

Override the file with `--contributors /path/to/contributors.toml`:

```toml
[[contributor]]
name = "Owen Lamont"
emails = ["owen@personal.example", "owen@work.example"]
github = "owenlamont"           # optional GitHub login
gitlab = "owenlamont"           # optional GitLab.com username
gravatar = "owen@avatar.example" # optional Gravatar email
# avatar = "portraits/owen.png"  # optional local path or HTTPS URL; overrides lookups
```

Emails match case-insensitively; duplicate emails are errors. Matching names never merge
people with different nonempty emails. Unlisted emails keep separate identities,
displayed with the name from the last commit in playback order. Commits without an email
are grouped by author name. A missing default file is fine; an explicit missing file,
unknown key or malformed TOML is an error. Edits take effect on cache hits; original
authors and repository history remain unchanged.

Configuration is limited to 1 MiB before parsing, 4,096 contributors, 4,096 total email
aliases, and 4,096 bytes per string. Expanded alias payloads may total at most 16 MiB.
Files exceeding a limit are rejected with a configuration error before alias expansion.

Account fields accept usernames, never profile URLs or whitespace. GitHub logins use
letters, digits and hyphens (bot logins may end in `[bot]`); GitLab usernames start with
a letter or digit and may also contain `_`, `-` and `.`. Invalid fields report the file,
contributor and field. Local avatar paths resolve relative to the TOML file and must stay
inside the TOML file's directory. Absolute paths are accepted only inside that directory.
Parent escapes, symlinks, directory junctions, network paths and device paths are rejected
before opening an image. Rejected paths warn and use the default coloured sphere.
All ancestors must be real directories, including ancestors of the TOML directory.
Local images
must decode as PNG, JPEG or GIF, at most 4 MiB and 2048 pixels per side. Missing or
undecodable local images produce a warning and fall back to account lookups.
Remote overrides require an HTTPS URL without credentials.

## Contributor avatars

Contributors stay above the file plane and move between repository service lanes.
Visible avatars keep their names, including during travel and idle fade-out.
Crowded playback shrinks avatars and names; small groups retain their normal size.
Names first shrink to a minimum of 12 pixels at the reference 1280×720 viewport,
then try nearby free lane positions. Truncation is the last resort when the full name
cannot fit at the minimum size. Names avoid other repositories' tiles and labels.
Name placement uses the default fitted view, including when preview is orbited or zoomed.
Playback time drives crowd scaling and placement, so export speed does not change them.

Preview and export stay offline by default: local overrides and cached portraits are
used, with the coloured sphere as the fallback. Add `--fetch-avatars` to either command
to opt in to network requests before playback starts. Export never fetches during frame
capture. Synthetic history never fetches.

```sh
codestrand preview --repository /path/to/repo --fetch-avatars
codestrand export frames --repository /path/to/repo --fetch-avatars
```

Resolution stops at the first usable portrait, in the following order:

1. Explicit `avatar`: local file or cached/fetched HTTPS image. An unavailable local
   file warns and continues below. An unavailable HTTPS override keeps the default glyph
   (or its previous cached portrait), without account lookups.
2. Explicit `github`, then explicit `gitlab` using the GitLab.com Users API.
3. GitHub noreply login, then the commit author's GitHub account when a remote points
   to github.com (`origin` preferred). An explicit GitHub login replaces inferred
   GitHub lookup. The commit API runs at most once per distinct unresolved email per run.
4. GitLab.com email lookup, only with **both** `--fetch-avatars` and
   `--gitlab-email-avatars`.
5. Gravatar using `gravatar` when provided, otherwise the commit email, hashed with
   SHA-256 after trimming and lowercasing; `d=404` avoids Gravatar placeholders.
6. Default coloured sphere.

`GH_TOKEN` or `GITHUB_TOKEN`, when present, authenticates only GitHub API requests.
Public repositories work without a token; private repositories need a token with
repository access. GitLab lookups are unauthenticated and support GitLab.com only.
Rate limits and network failures leave playback available. HTTPS uses Rustls with ring
and the operating system trust store on Linux, macOS and Windows.

## Privacy and network settings

`--fetch-avatars` alone **never sends raw commit emails**. GitLab email lookup is off by
default, even when fetching is enabled. Opt in explicitly for each invocation:

```sh
codestrand export frames --repository /path/to/repo --fetch-avatars --gitlab-email-avatars
```

- No fetch flags: no requests; local overrides and cache only.
- `--fetch-avatars`, remote `avatar`: the override host receives the configured URL,
  including path and query.
- `--fetch-avatars`, GitHub: GitHub receives the configured or noreply login, or the
  repository and commit SHA. The image host receives the image URL, possibly containing
  a numeric account ID.
- `--fetch-avatars`, `gitlab`: GitLab.com receives the configured username. The returned
  image host receives the avatar URL.
- `--fetch-avatars`, Gravatar: Gravatar receives the normalized email's SHA-256 hash,
  never the email.
- Both flags, GitLab email: GitLab.com receives the trimmed, lowercased commit email
  in a URL query. The returned image host receives the avatar URL.

Identifiers and email hashes are identifying information. GitLab may return an external
avatar URL; fetching follows that URL unless the host is `gravatar.com` or a subdomain.
GitLab Gravatar URLs are treated as misses so the configured Gravatar step decides.
HTTPS redirects can also contact another image host.
An explicit avatar URL can itself contain identifying information supplied by the user.
GitHub credentials are never added to GitLab or image requests. Diagnostics show counts,
not emails or request URLs. Local avatar overrides do not require either fetch flag.

## Cache

Portraits and account resolutions live under `codestrand/avatars-v2/` in the platform
cache directory, keyed by a hash of email, explicit accounts, override, remote and GitLab
email setting. Remote overrides are keyed only by URL. Existing GitHub/Gravatar-only
cache entries remain usable. For account lookups, changing the GitLab email flag selects
a separate cache entry; pass `--gitlab-email-avatars` without
`--fetch-avatars` to reuse an opted-in portrait offline. The email flag alone never
enables network requests.
Portraits are resized to 128-pixel PNG files beside small JSON account records.
Fetching covers only contributors in the selected playback window, with a 30-second
total network budget. Entries refresh after seven days when fetching is enabled;
offline playback can reuse expired images. Failed refreshes retain cached images.
`--no-cache` disables avatar cache reads and writes too; local overrides still work.
Delete the directory to clear saved avatars. Circular portraits retain the contributor
colour as a ring; contributors without images keep the coloured sphere.

Cache reads and writes reject linked directories in every path component. An unusable
cache is treated as a miss; no linked destination is read or written.
