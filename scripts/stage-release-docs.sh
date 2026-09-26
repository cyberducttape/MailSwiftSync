#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "usage: $0 BUNDLE_DIRECTORY" >&2
  exit 2
fi

bundle_dir="$1"
mkdir -p "$bundle_dir/docs"

root_documents=(
  README.md
  LICENSE
  PROVIDER_SETUP.md
  OAUTH_SETUP.md
  CAPABILITY_MANIFEST.md
  PRODUCTION_STATUS.md
  SECURITY.md
  CHANGELOG.md
  CONTRIBUTING.md
)

for document in "${root_documents[@]}"; do
  if [[ ! -f "$document" ]]; then
    echo "FAIL: release document is missing from the repository: $document" >&2
    exit 1
  fi
  cp "$document" "$bundle_dir/"
done

for linked_artifact in capabilities.toml scripts/provider-integration-test.sh; do
  if [[ ! -f "$linked_artifact" ]]; then
    echo "FAIL: release-linked artifact is missing from the repository: $linked_artifact" >&2
    exit 1
  fi
  mkdir -p "$bundle_dir/$(dirname "$linked_artifact")"
  cp "$linked_artifact" "$bundle_dir/$linked_artifact"
done

cp -R docs/. "$bundle_dir/docs/"
