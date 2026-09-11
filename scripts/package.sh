#!/usr/bin/env bash
set -euo pipefail

# Produces a portable host-native tarball and SHA-256 checksum.
# Sign the checksum with your release key before distribution.
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
project_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
cd "$project_root"

cargo build --release
target_name="sourcecraft-imap-migrator-$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
mkdir -p dist
cp target/release/sourcecraft-imap-migrator dist/sourcecraft-imap-migrator
chmod 0755 dist/sourcecraft-imap-migrator
tar -C dist -czf "dist/${target_name}.tar.gz" sourcecraft-imap-migrator
sha256sum "dist/${target_name}.tar.gz" > "dist/${target_name}.tar.gz.sha256"
rm -f dist/sourcecraft-imap-migrator
