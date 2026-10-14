#!/usr/bin/env bash
set -euo pipefail

version="${1:?Pass the released version}"
tag="${2:?Pass the npm dist-tag}"
flags=(--title "v$version" --generate-notes --target "$GITHUB_SHA")
# A version published under next is a prerelease on GitHub too.
if [ "$tag" = 'next' ]; then flags+=(--prerelease); fi
gh release create "v$version" "${flags[@]}"
