#!/usr/bin/env bash
set -euo pipefail

if ! cmp -s assets/codestrand.svg docs/assets/codestrand.svg; then
  echo 'Docs logo differs: copy assets/codestrand.svg to docs/assets/codestrand.svg.' >&2
  exit 1
fi
