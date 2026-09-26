#!/usr/bin/env bash
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/scripts/release-channel.sh"

for tag in v0.1.0-alpha.1 v1.2.3-alpha.10 v1.2.3-beta.2 v1.2.3-rc.1; do
  result="$(bash "$script" "$tag")"
  if [[ "$result" != preview ]]; then
    echo "release-channel test expected $tag to be preview, got $result" >&2
    exit 1
  fi
done

for tag in v1.2.3 v1.2.3-alpha v1.2.3-alpha.foo v1.2.3-preview.1; do
  result="$(bash "$script" "$tag")"
  if [[ "$result" != stable ]]; then
    echo "release-channel test expected $tag to be stable, got $result" >&2
    exit 1
  fi
done

echo "release-channel tests passed"
