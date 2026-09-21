#!/usr/bin/env bash
set -euo pipefail

tag="${1:-${GITHUB_REF_NAME:-}}"
if [[ -z "$tag" ]]; then
  echo "release tag is required" >&2
  exit 2
fi
cargo_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
if [[ -z "$cargo_version" ]]; then
  echo "could not read package version from Cargo.toml" >&2
  exit 1
fi
if [[ "$tag" != "v${cargo_version}" && "$tag" != "v${cargo_version}-"* ]]; then
  echo "release tag $tag does not match Cargo package version $cargo_version" >&2
  exit 1
fi
if [[ "$tag" == v*-[.-] || "$tag" == *..* || "$tag" == *--* ]]; then
  echo "release tag $tag has an invalid prerelease suffix" >&2
  exit 1
fi
echo "release version accepted: $tag (Cargo $cargo_version)"
