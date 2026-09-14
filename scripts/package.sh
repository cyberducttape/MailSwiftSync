#!/usr/bin/env bash
set -euo pipefail

# Produces a deterministic portable host-native tarball and SHA-256 checksum.
# Sign the checksum with the release key or CI signing service before distribution.
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
project_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
cd "$project_root"

CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" cargo build --locked --release
target_name="mailswiftsync-$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
mkdir -p dist/package
cp target/release/mailswiftsync dist/package/mailswiftsync
cp README.md LICENSE docs/distribution/INSTALL.md docs/distribution/SERVICE.md docs/container.md docs/release-readiness.md docs/wiki/Install-and-first-launch.md dist/package/
chmod 0755 dist/package/mailswiftsync
archive="dist/${target_name}.tar.gz"
if [[ -z "${SOURCE_DATE_EPOCH:-}" ]]; then
  SOURCE_DATE_EPOCH="$(git log -1 --format=%ct HEAD)"
fi
export SOURCE_DATE_EPOCH
tar --sort=name --mtime="@${SOURCE_DATE_EPOCH}" --owner=0 --group=0 --numeric-owner \
  -C dist/package -czf "$archive" .
if command -v sha256sum >/dev/null 2>&1; then
  (cd dist && sha256sum "$(basename "$archive")") > "${archive}.sha256"
else
  (cd dist && shasum -a 256 "$(basename "$archive")") > "${archive}.sha256"
fi
rm -rf dist/package
