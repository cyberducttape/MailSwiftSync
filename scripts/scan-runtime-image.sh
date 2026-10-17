#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <image-reference> <full-json-report-path>" >&2
  exit 2
fi

image_reference="$1"
report_path="$2"
scanner_metadata_path="${report_path%.json}.scanner.txt"
if ! command -v trivy >/dev/null 2>&1; then
  echo "pinned Trivy executable is not available in PATH" >&2
  exit 127
fi

report_directory="$(dirname -- "$report_path")"
mkdir -p -- "$report_directory"

# Download the vulnerability database once, then capture the exact Trivy and
# database version/timestamps used by both the full report and policy gate.
trivy image --download-db-only
trivy --version >"$scanner_metadata_path"

# Retain all findings for review, including unfixed issues. The separate gate
# fails only on HIGH/CRITICAL findings for which the package vendor provides a
# fixed version; unfixed findings remain visible in the full report artifact.
trivy image \
  --scanners vuln \
  --exit-code 0 \
  --format json \
  --output "$report_path" \
  "$image_reference"

trivy image \
  --scanners vuln \
  --severity HIGH,CRITICAL \
  --ignore-unfixed \
  --exit-code 1 \
  "$image_reference"
