#!/usr/bin/env bash
set -euo pipefail

if [[ "${GITHUB_EVENT_NAME:-}" != pull_request ]]; then
  echo true
  exit 0
fi

status=0
git diff --quiet --no-ext-diff --no-renames HEAD^1 HEAD -- . \
  ':(top,exclude)README.md' \
  ':(top,exclude)AGENTS.md' \
  ':(top,exclude)CONTRIBUTING.md' \
  ':(top,exclude)zensical.toml' \
  ':(top,exclude)lychee.toml' \
  ':(top,glob,exclude)docs/assets/**' \
  ':(top,exclude)scripts/check_docs_logo.sh' \
  ':(top,glob,exclude)docs/**/*.md' \
  ':(top,exclude)assets/codestrand.svg' || status=$?
case "$status" in
  0) echo false ;;
  1) echo true ;;
  *) exit "$status" ;;
esac
