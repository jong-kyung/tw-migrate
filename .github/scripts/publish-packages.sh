#!/usr/bin/env bash
set -euo pipefail

version="${1:?Pass the version to release}"
case "$version" in
  *-*) tag=next ;;
  *) tag=latest ;;
esac
# The following GitHub Release step uses the same tag classification.
echo "tag=$tag" >> "$GITHUB_ENV"

# Check after approval and under the workflow's publish concurrency lock so
# releases approved out of order cannot move the tag backward.
if current="$(npm view "tw-migrate@$tag" version 2>"$RUNNER_TEMP/npm-view.err")"; then
  :
elif grep -q 'E404' "$RUNNER_TEMP/npm-view.err"; then
  current=''
else
  cat "$RUNNER_TEMP/npm-view.err"
  echo "::error::Could not reach the npm registry to read the $tag tag"
  exit 1
fi

if [ -z "$current" ]; then
  echo "No $tag tag yet; $version will create it."
else
  newest="$(npx --yes -p semver@7.7.2 semver "$version" "$current" | tail -1)"
  if [ "$version" = "$current" ] || [ "$newest" != "$version" ]; then
    echo "::error::$version does not come after the current $tag ($current); publishing it would move the $tag tag backward"
    exit 1
  fi
  echo "$version comes after the current $tag ($current)."
fi

# Prepare exact optional dependency versions without publishing or creating a
# GitHub Release. Publishing separately lets reruns skip completed packages.
npx --yes --package @napi-rs/cli@3.7.3 napi pre-publish -p npm -t npm --no-gh-release --skip-optional-publish

for dir in npm/*/; do
  name="$(node -p "require('./${dir}package.json').name")"

  if published="$(npm view "$name@$version" version 2>"$RUNNER_TEMP/npm-view.err")"; then
    :
  elif grep -q 'E404' "$RUNNER_TEMP/npm-view.err"; then
    published=''
  else
    cat "$RUNNER_TEMP/npm-view.err"
    echo "::error::Could not reach the npm registry to check $name@$version"
    exit 1
  fi

  if [ -n "$published" ]; then
    echo "$name@$version is already published; skipping."
    continue
  fi

  echo "Publishing $name@$version under the $tag tag"
  (cd "$dir" && npm publish --access public --tag "$tag")
done

echo "Publishing tw-migrate@$version under the $tag tag"
# Root prepare only configures developer hooks and does not affect the tarball.
npm publish --access public --tag "$tag" --ignore-scripts
