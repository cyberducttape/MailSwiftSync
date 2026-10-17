#!/usr/bin/env bash
set -euo pipefail

readonly TRIVY_VERSION="0.76.0"
readonly TRIVY_ARCHIVE_SHA256="e24b1c51293c1b15e4ba8d45bbc7891ba053f90278ec2698f3562c993e145fe7"

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <new-install-directory>" >&2
  exit 2
fi

install_dir="$1"
if [[ -e "$install_dir" || -L "$install_dir" ]]; then
  echo "refusing to install Trivy into an existing path: $install_dir" >&2
  exit 2
fi
mkdir -m 0700 -- "$install_dir"

archive="$(mktemp "${TMPDIR:-/tmp}/mailswiftsync-trivy.XXXXXX.tar.gz")"
trap 'rm -f -- "$archive"' EXIT

curl --fail --location --proto '=https' --tlsv1.2 \
  "https://github.com/aquasecurity/trivy/releases/download/v${TRIVY_VERSION}/trivy_${TRIVY_VERSION}_Linux-64bit.tar.gz" \
  --output "$archive"
printf '%s  %s\n' "$TRIVY_ARCHIVE_SHA256" "$archive" | sha256sum --check --status
tar -xzf "$archive" -C "$install_dir" trivy

version_output="$("$install_dir/trivy" --version)"
if ! grep -Fqx "Version: ${TRIVY_VERSION}" <<<"$version_output"; then
  echo "downloaded Trivy binary did not report the pinned version" >&2
  exit 1
fi
printf '%s\n' "$install_dir"
