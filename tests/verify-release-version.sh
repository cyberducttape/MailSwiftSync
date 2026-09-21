#!/usr/bin/env bash
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/scripts/verify-release-version.sh"

for tag in \
  v0.1.0 \
  v0.1.0-alpha \
  v0.1.0-beta.1 \
  v0.1.0-beta.999 \
  v0.1.0-rc.1
do
  "$script" "$tag" >/dev/null
done

for tag in \
  v0.1.0-garbage \
  v0.1.0-alpha.1 \
  v0.1.0-beta.0 \
  v0.1.0-rc.0 \
  v0.1.0-rc.01 \
  v0.1.0-rc.1.0 \
  v0.1.0-rc.1-garbage \
  0.1.0 \
  v0.2.0
do
  if "$script" "$tag" >/dev/null 2>&1; then
    echo "release-version test unexpectedly accepted $tag" >&2
    exit 1
  fi
done

echo "release-version tests passed"
