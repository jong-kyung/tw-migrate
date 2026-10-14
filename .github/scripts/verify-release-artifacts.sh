#!/usr/bin/env bash
set -euo pipefail

test -f dist/bin.js
test -f dist/index.js
found="$(find npm -name '*.node' | sort)"
count="$(printf '%s\n' "$found" | grep -c '\.node$' || true)"
printf '%s\n' "$found"
if [ "$count" -ne 5 ]; then
  echo "::error::Expected 5 native addons, found $count"
  exit 1
fi
