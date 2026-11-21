#!/usr/bin/env bash
set -euo pipefail

# scripts/verify-release.sh must accept LF and CRLF (Windows) checksum
# manifests and reject a manifest whose file is missing or altered.
script="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/scripts/verify-release.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

printf 'payload\n' > "$work/artifact.zip"
digest="$(sha256sum "$work/artifact.zip" | cut -d ' ' -f 1)"

printf '%s  artifact.zip\n' "$digest" > "$work/artifact.zip.sha256"
"$script" "$work" >/dev/null

printf '%s  artifact.zip\r\n' "$digest" > "$work/artifact.zip.sha256"
"$script" "$work" >/dev/null

printf 'altered\n' > "$work/artifact.zip"
if "$script" "$work" >/dev/null 2>&1; then
  echo "verify-release accepted an altered artifact" >&2
  exit 1
fi

rm "$work/artifact.zip"
if "$script" "$work" >/dev/null 2>&1; then
  echo "verify-release accepted a missing artifact" >&2
  exit 1
fi
echo "verify-release tests passed"
