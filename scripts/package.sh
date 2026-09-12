#!/usr/bin/env bash
set -euo pipefail

# Produces a portable host-native tarball and SHA-256 checksum.
# Sign the checksum with your release key before distribution.
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
project_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
cd "$project_root"

cargo build --release
target_name="mailswiftsync-$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
mkdir -p dist
cp target/release/mailswiftsync dist/mailswiftsync
chmod 0755 dist/mailswiftsync
tar -C dist -czf "dist/${target_name}.tar.gz" mailswiftsync
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "dist/${target_name}.tar.gz" > "dist/${target_name}.tar.gz.sha256"
else
  shasum -a 256 "dist/${target_name}.tar.gz" > "dist/${target_name}.tar.gz.sha256"
fi
rm -f dist/mailswiftsync
