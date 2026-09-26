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
)

for document in "${root_documents[@]}"; do
  if [[ ! -f "$document" ]]; then
    echo "FAIL: release document is missing from the repository: $document" >&2
    exit 1
  fi
  cp "$document" "$bundle_dir/"
done

for linked_artifact in capabilities.toml; do
  if [[ ! -f "$linked_artifact" ]]; then
    echo "FAIL: release-linked artifact is missing from the repository: $linked_artifact" >&2
    exit 1
  fi
  mkdir -p "$bundle_dir/$(dirname "$linked_artifact")"
  cp "$linked_artifact" "$bundle_dir/$linked_artifact"
done

distribution_documents=(
  docs/architecture.md
  docs/compatibility-matrix.md
  docs/container.md
  docs/provider-facts.md
  docs/release-0.1.0-alpha.md
  docs/release-readiness.md
  docs/bulk-migrations-template.csv
  docs/distribution/INSTALL.md
  docs/distribution/SERVICE.md
)
for document in "${distribution_documents[@]}"; do
  if [[ ! -f "$document" ]]; then
    echo "FAIL: distribution document is missing from the repository: $document" >&2
    exit 1
  fi
  destination="$bundle_dir/$document"
  mkdir -p "$(dirname "$destination")"
  cp "$document" "$destination"
done

if [[ ! -d docs/wiki ]]; then
  echo "FAIL: distribution wiki is missing from the repository: docs/wiki" >&2
  exit 1
fi
mkdir -p "$bundle_dir/docs/wiki"
cp -R docs/wiki/. "$bundle_dir/docs/wiki/"
