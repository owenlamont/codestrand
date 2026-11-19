# Using Codestrand with AI agents

Coding agents can run Codestrand for you: inspect a repository, open a preview or export
frames and video. This page lists the context Codestrand publishes for them; the rest of
these docs cover the CLI itself.

## The Codestrand agent skill

The agent skill, `skills/codestrand/SKILL.md` in the
[repository](https://github.com/owenlamont/codestrand), teaches an agent the commands,
the export manifest, what leaves the machine and the common failures. Install it for
your user with the GitHub CLI:

```sh
gh skill install owenlamont/codestrand codestrand --agent claude-code --scope user
```

Swap `--agent` for `codex`, `cursor`, `github-copilot` or another host listed by
`gh skill install --help`. The skill installs from the latest release, so it is
available from the first release after 0.2.0. A test checks that every option the skill
names is in `--help` and that every command it shows parses.

## Prompting in AGENTS.md

A line in a repository's `AGENTS.md` or `CLAUDE.md` points agents at the tool:

> Visualise this repository's history with `codestrand preview --repository .`, or
> export it with `codestrand export <new directory> --repository .`. Do not pass
> `--fetch-avatars` without asking: it sends contributor lookups to GitHub and Gravatar.

## Feeding the docs to an LLM

The docs site publishes [llmstxt.org](https://llmstxt.org) files for agents that read
documentation directly:

- [llms.txt](llms.txt): an index of every page with a one-line summary.
- [llms-full.txt](llms-full.txt): every page's Markdown in one file.

Both are generated from these pages by `scripts/gen_llms_txt.py`.
