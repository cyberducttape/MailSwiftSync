#!/usr/bin/env bash
# Fetch the externally retained provider qualification bundle for a release.
# The URL and digest are repository-level release configuration, not source
# files, because the bundle certifies the immutable commit being released.
set -euo pipefail

output_dir="${1:-}"
url="${MAILSWIFTSYNC_RELEASE_EVIDENCE_URL:-}"
expected_sha256="${MAILSWIFTSYNC_RELEASE_EVIDENCE_SHA256:-}"

if [[ -z "$output_dir" || -z "$url" || -z "$expected_sha256" ]]; then
  echo "FAIL: release evidence requires an output directory, MAILSWIFTSYNC_RELEASE_EVIDENCE_URL, and MAILSWIFTSYNC_RELEASE_EVIDENCE_SHA256" >&2
  exit 2
fi
if [[ "$url" != https://* ]]; then
  echo "FAIL: release evidence URL must use HTTPS" >&2
  exit 2
fi
if [[ ! "$expected_sha256" =~ ^[0-9a-fA-F]{64}$ ]]; then
  echo "FAIL: MAILSWIFTSYNC_RELEASE_EVIDENCE_SHA256 must be a SHA-256 digest" >&2
  exit 2
fi

archive="$(mktemp "${TMPDIR:-/tmp}/mailswiftsync-release-evidence.XXXXXX.tar.gz")"
cleanup() { rm -f "$archive"; }
trap cleanup EXIT
mkdir -p -- "$output_dir"

if command -v curl >/dev/null 2>&1; then
  curl --fail --location --proto '=https' --tlsv1.2 --silent --show-error \
    --output "$archive" "$url"
elif command -v wget >/dev/null 2>&1; then
  wget --https-only --output-document="$archive" "$url"
else
  echo "FAIL: curl or wget is required to fetch release evidence" >&2
  exit 1
fi

actual_sha256="$(sha256sum "$archive" 2>/dev/null | awk '{print $1}' || shasum -a 256 "$archive" | awk '{print $1}')"
if [[ "${actual_sha256,,}" != "${expected_sha256,,}" ]]; then
  echo "FAIL: release evidence archive SHA-256 does not match the pinned digest" >&2
  exit 1
fi

# Reject absolute paths and parent traversal before extraction.
if tar -tzf "$archive" | awk '
  /^// || /(^|\/)\.\.($|\/)/ { bad=1 }
  END { exit bad ? 0 : 1 }
'; then
  echo "FAIL: release evidence archive contains an unsafe path" >&2
  exit 1
fi
tar -xzf "$archive" -C "$output_dir"
if ! find "$output_dir" -maxdepth 1 -type f -name '*.json' ! -name 'policy.json' ! -name 'schema.json' | grep -q .; then
  echo "FAIL: release evidence archive contains no evidence JSON files" >&2
  exit 1
fi
echo "Fetched and verified external release evidence bundle into $output_dir"
