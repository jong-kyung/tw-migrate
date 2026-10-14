#!/usr/bin/env bash
set -euo pipefail

before="${1:-}"
version="$(node -p "require('./package.json').version")"
echo "version=$version" >> "$GITHUB_OUTPUT"

# Compare against the previous push tip, not HEAD^, because one push can
# contain multiple commits that already carry the new version.
previous=''
if [ -n "$before" ] && [ "$before" != '0000000000000000000000000000000000000000' ] \
  && git fetch --no-tags --depth=1 origin "$before" 2>/dev/null; then
  previous="$(git show "$before:package.json" | node -pe 'JSON.parse(require("node:fs").readFileSync(0, "utf8")).version')"
fi

if [ -z "$previous" ]; then
  echo "::warning::Could not read the version main held before this push; relying on the registry check alone"
elif [ "$version" = "$previous" ]; then
  echo "package.json changed but the version is still $version; nothing to release."
  echo 'changed=false' >> "$GITHUB_OUTPUT"
  exit 0
fi

# E404 or empty output means unpublished. Other failures must stop the release.
if published="$(npm view "tw-migrate@$version" version 2>"$RUNNER_TEMP/npm-view.err")"; then
  :
elif grep -q 'E404' "$RUNNER_TEMP/npm-view.err"; then
  published=''
else
  cat "$RUNNER_TEMP/npm-view.err"
  echo "::error::Could not reach the npm registry to check tw-migrate@$version"
  exit 1
fi

if [ -n "$published" ]; then
  echo "tw-migrate@$version is already published; stopping."
  echo 'changed=false' >> "$GITHUB_OUTPUT"
else
  echo "tw-migrate@$version is not on npm; releasing it."
  echo 'changed=true' >> "$GITHUB_OUTPUT"
fi
