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
github = "owenlamont"         # optional GitHub login
bot = false                   # optional bot classification override
# avatar = "portraits/owen.png"  # optional local path or HTTPS URL; overrides lookups
```

Emails match case-insensitively; duplicate emails are errors. Matching names never merge
people with different nonempty emails. Unlisted emails of one GitHub account become one
contributor (see [GitHub accounts](#github-accounts)); other unlisted emails keep
separate identities. Each is displayed with the name from its last commit in playback
order. Commits without an email
are grouped by author name. A missing default file is fine; an explicit missing file,
unknown key or malformed TOML is an error. Edits take effect on cache hits; original
authors and repository history remain unchanged.

Each `Co-authored-by: Name <email>` trailer in a commit message's final trailer block
makes that person a contributor of the commit, as on GitHub. Co-authors match by email
like authors, so a co-author shares the avatar of their own commits. A commit's
contributors pack, slightly smaller, inside one ring in the author's colour, with each
name outside the ring nearest its member, and every beam leaves from the ring's edge. A
co-author stays in the ring while the author keeps its place, then walks to its own
next commit; when the author moves on, a co-author fades out with the ring unless it
commits again within `--contributor-inactivity-days`. The author's colour marks the
files and the caption names the author.
Codestrand reads at most 8 co-authors per commit, none from a message over 64 KiB, and
ignores a trailer without an email or with a name or email over 4,096 bytes.

`bot = true` marks automation and `bot = false` marks a human, overriding automatic
detection even after a display-name change. Without an override, a parsed GitHub noreply
login ending in `[bot]` identifies a bot; a displayed name ending in `[bot]` is the
fallback. `[bot]` elsewhere in a name does not match. Bot avatars fade between
repositories by default; use `--bot-travel walk` to route them like humans.

Configuration is limited to 1 MiB before parsing, 4,096 contributors, 4,096 total email
aliases, and 4,096 bytes per string. Expanded alias payloads may total at most 16 MiB.
Files exceeding a limit are rejected with a configuration error before alias expansion.

`github` accepts a login, never a profile URL or whitespace: letters, digits and
hyphens, and bot logins may end in `[bot]`. Invalid fields report the file, contributor
and field. Local avatar paths resolve relative to the TOML file and must stay
inside the TOML file's directory. Absolute paths are accepted only inside that directory.
Parent escapes, symlinks, directory junctions, network paths and device paths are rejected
before opening an image. Rejected paths warn and use the default coloured sphere.
All ancestors must be real directories, including ancestors of the TOML directory.
Local images
must decode as PNG, JPEG or GIF, at most 4 MiB and 2048 pixels per side. Missing or
undecodable local images produce a warning and fall back to account lookups.
Remote overrides require an HTTPS URL without credentials.

## Coding agents

Coding agents sign commits with fixed emails that GitHub links to their own accounts.
Codestrand does not build these in, so map the ones you use; a `github` login gives the
agent its portrait under `--fetch-avatars`. Agents that commit as a GitHub noreply
address, such as Copilot, Jules and Devin, need no entry.

| Agent | Email | `github` | Source |
| --- | --- | --- | --- |
| Claude Code | `noreply@anthropic.com` | `claude` | [Settings](https://docs.anthropic.com/en/docs/claude-code/settings) |
| Codex | `noreply@openai.com`, `codex@openai.com` | `codex` | [Source](https://github.com/openai/codex/blob/0e8c7683c4a85f00667bd762265358a407124f2e/codex-rs/ext/git-attribution/src/world_state.rs) |
| Cursor | `cursoragent@cursor.com` | `cursoragent` | [Agents](https://docs.cursor.com/background-agent) |
| Amp | `amp@ampcode.com` | `ampagent` | [Manual](https://ampcode.com/manual) |
| OpenHands | `openhands@all-hands.dev` | `openhands-agent` | [Source](https://github.com/OpenHands/OpenHands/blob/428b3dc2a0e8d3d0f689a909e2607e33d973da76/src/services/settings.ts) |
| Aider | `noreply@aider.chat` | none | [Git](https://aider.chat/docs/git.html) |

```toml
[[contributor]]
name = "Claude"
emails = ["noreply@anthropic.com"]
github = "claude"

[[contributor]]
name = "Codex"
emails = ["noreply@openai.com", "codex@openai.com"]
github = "codex"

[[contributor]]
name = "Cursor"
emails = ["cursoragent@cursor.com"]
github = "cursoragent"

[[contributor]]
name = "Amp"
emails = ["amp@ampcode.com"]
github = "ampagent"

[[contributor]]
name = "OpenHands"
emails = ["openhands@all-hands.dev"]
github = "openhands-agent"

[[contributor]]
name = "Aider"
emails = ["noreply@aider.chat"]
```

These identities were checked in November 2026 and can change. To check one, open a
commit the agent co-authored on GitHub: the avatars beside the commit link to the
account GitHub matched to each email.

## Contributor avatars

Contributors stay above the file plane and move between repository service lanes.
`--avatar-scale` controls disc diameter relative to the original size: default `2`,
range `0.5`–`2`, inclusive. The default doubles portraits in both single- and
multi-repository playback; single-repository pixels therefore change too. Use
`--avatar-scale 1` for the original small-group disc diameter. Repository count
shrinks avatars only through the packing detail scale in [Playback](playback.md).
Name fonts retain their existing readability policy. `--gutter` independently controls
visible tile spacing (default `4` reference pixels at full detail, `0` allowed); the
outer margin is 24 pixels.
Visible avatars keep their names, including during travel and idle fade-out.
Crowded playback shrinks avatars and names; small groups retain their normal size.
Names first shrink to a minimum of 12 pixels at the reference 1280×720 viewport,
then try nearby free lane positions. Truncation is the last resort when the full name
cannot fit at the minimum size. Names prefer to avoid every repository’s content
rectangle and labels, with edge overlap permitted when no clear lane fits.
Name placement uses the default fitted view, including when preview is orbited or zoomed.
Playback time drives crowd scaling and placement, so export speed does not change them.

Preview and export stay offline by default: local overrides and cached portraits are
used, with the coloured sphere as the fallback. Add `--fetch-avatars` to either command
to opt in to requests to GitHub before playback starts. Export never fetches during
frame capture.

```sh
codestrand preview --repository /path/to/repo --fetch-avatars
codestrand export frames --repository /path/to/repo --fetch-avatars
```

Resolution stops at the first usable portrait, in the following order:

1. Explicit `avatar`: local file or cached/fetched HTTPS image. An unavailable local
   file warns and continues below. An unavailable HTTPS override keeps the default glyph
   (or its previous cached portrait), without GitHub lookups.
2. The contributor's GitHub account: explicit `github`, else the account found for one
   of its emails, preferring one with a numeric account ID.
3. Default coloured sphere.

## GitHub accounts

Codestrand finds the GitHub account behind a commit email, without a contributors file:

- A GitHub noreply address, `<id>+<login>@users.noreply.github.com` or
  `<login>@users.noreply.github.com`, names its account. No request is needed, so this
  works offline.
- With `--fetch-avatars`, when a remote points to github.com (`origin` preferred),
  GitHub's commit API names the author account of one commit by that email. Busiest
  emails go first, and each email is looked up at most once per run. An email seen only
  as a co-author is not looked up, since a commit names only its author's account.

Unlisted emails of one account become one contributor, identified by the numeric
account ID where known, so a renamed login stays one person. A contributors entry whose
`github` names any login of that account takes those emails, unless two entries do; the
entry may list no emails, to name an account's contributor alone. An email the
file lists always keeps its entry. A commit email can claim any noreply address, so a
forged one merges with that account, just as GitHub attributes it.

`GH_TOKEN` or `GITHUB_TOKEN`, when present, authenticates only GitHub API requests.
Public repositories work without a token; private repositories need a token with
repository access. Unauthenticated, GitHub allows about 60 API requests an hour, so a
history with many authors needs a token, or several runs, to find every account; the
summary line reports `unresolved_emails`. Rate limits and network failures leave
playback available. HTTPS uses Rustls with ring and the operating system trust store on
Linux, macOS and Windows.

## Privacy and network settings

Codestrand never sends a commit email, an email hash or an author name. GitHub is the
only service it contacts on its own, and only with `--fetch-avatars`:

- No flag: no requests; local overrides and cache only.
- GitHub API: receives the repository owner and name and a commit SHA.
- GitHub image hosts: receive a login or numeric account ID in the image URL.
- Remote `avatar`: the override host receives the configured URL, including path and
  query, which can carry whatever its author put in it.

HTTPS redirects can contact another image host. GitHub credentials are never added to
image requests. Diagnostics show counts, not emails or request URLs. Local avatar
overrides need no flag.

## Cache

Portraits and accounts live under `codestrand/avatars-v3/` in the platform cache
directory. An account found for an email is reused for every repository; an email with
no account is rechecked for a different remote, and a commit GitHub does not have, such
as an unpushed one, is rechecked next run. Images are keyed by URL. Earlier
releases used `avatars-v2`, which Codestrand leaves on disk but no longer reads; after
upgrading, run once with `--fetch-avatars` to get portraits back.
Portraits are resized to 128-pixel PNG files beside small JSON account records.
Fetching covers only contributors in the selected playback window, with a 30-second
total network budget; account lookups stop with 15 seconds left so portraits still
load. Entries refresh after seven days when fetching is enabled;
offline playback can reuse expired images. Failed refreshes retain cached images.
`--no-cache` disables avatar cache reads and writes too; local overrides still work.
Delete the directory to clear saved avatars. Circular portraits retain the contributor
colour as a ring; contributors without images keep the coloured sphere.

Cache reads and writes reject linked directories in every path component. An unusable
cache is treated as a miss; no linked destination is read or written.
