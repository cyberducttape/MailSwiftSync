#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "usage: $0 RELEASE_ARCHIVE" >&2
  exit 2
fi

archive="$1"
if [[ ! -f "$archive" ]]; then
  echo "FAIL: release archive does not exist: $archive" >&2
  exit 1
fi

workspace="$(mktemp -d "${TMPDIR:-/tmp}/mailswiftsync-release-check.XXXXXX")"
cleanup() {
  rm -rf -- "$workspace"
}
trap cleanup EXIT

case "$archive" in
  *.tar.gz)
    tar -xzf "$archive" -C "$workspace"
    ;;
  *.zip)
    if ! command -v unzip >/dev/null 2>&1; then
      echo "FAIL: unzip is required to validate $archive" >&2
      exit 1
    fi
    unzip -q "$archive" -d "$workspace"
    ;;
  *)
    echo "FAIL: unsupported release archive: $archive" >&2
    exit 1
    ;;
esac

required_documents=(
  README.md
  LICENSE
  PROVIDER_SETUP.md
  OAUTH_SETUP.md
  CAPABILITY_MANIFEST.md
  PRODUCTION_STATUS.md
  SECURITY.md
  CHANGELOG.md
  CONTRIBUTING.md
  capabilities.toml
  scripts/provider-integration-test.sh
)

for document in "${required_documents[@]}"; do
  if [[ ! -f "$workspace/$document" ]]; then
    echo "FAIL: release archive omits $document" >&2
    exit 1
  fi
done
if [[ ! -d "$workspace/docs" ]]; then
  echo "FAIL: release archive omits docs/" >&2
  exit 1
fi

binary_count=$(find "$workspace" -mindepth 1 -maxdepth 1 -type f \
  \( -name 'mailswiftsync-*' -o -name 'mailswiftsync-*.exe' \) -print | wc -l | tr -d ' ')
if [[ "$binary_count" -ne 1 ]]; then
  echo "FAIL: release archive must contain exactly one root-level mailswiftsync binary" >&2
  exit 1
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
python3 "$script_dir/verify-markdown-links.py" "$workspace"

echo "PASS: release archive contains all bundled documentation references"
