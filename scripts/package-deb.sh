#!/usr/bin/env bash
set -euo pipefail

# Build a deterministic Debian package from an already-built release binary.
# The package contains only MailSwiftSync and operator documentation; it does
# not silently install imapsync or Dovecot.
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
project_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
cd "$project_root"

if ! command -v dpkg-deb >/dev/null 2>&1; then
  echo "package-deb.sh requires dpkg-deb" >&2
  exit 2
fi

output_dir="${1:-dist}"
binary="${MAILSWIFTSYNC_PACKAGE_BINARY:-target/release/mailswiftsync}"
if [[ ! -x "$binary" ]]; then
  cargo build --locked --release
fi
[[ -x "$binary" ]] || { echo "release binary is not executable: $binary" >&2; exit 1; }

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
[[ -n "$version" ]] || { echo "could not read package version" >&2; exit 1; }
# Debian's tilde sorts prereleases before the final release and is valid in the
# upstream-version component, unlike blindly treating the hyphen as a revision.
debian_version="${version/-alpha./~alpha.}"
debian_version="${debian_version/-beta./~beta.}"
debian_version="${debian_version/-rc./~rc.}"
architecture="$(dpkg --print-architecture)"
package_name="mailswiftsync_${debian_version}_${architecture}.deb"
stage="$(mktemp -d "${TMPDIR:-/tmp}/mailswiftsync-deb.XXXXXX")"
cleanup() { rm -rf -- "$stage"; }
trap cleanup EXIT
chmod 0755 "$stage"
if [[ -z "${SOURCE_DATE_EPOCH:-}" ]]; then
  SOURCE_DATE_EPOCH="$(git log -1 --format=%ct HEAD)"
fi
export SOURCE_DATE_EPOCH

install -d -m 0755 "$stage/usr/bin" "$stage/usr/share/doc/mailswiftsync" \
  "$stage/usr/share/man/man1"
install -m 0755 "$binary" "$stage/usr/bin/mailswiftsync"
install -m 0644 README.md LICENSE PRODUCTION_STATUS.md \
  "$stage/usr/share/doc/mailswiftsync/"
install -m 0644 docs/distribution/INSTALL.md \
  "$stage/usr/share/doc/mailswiftsync/INSTALL.md"
install -m 0644 docs/distribution/mailswiftsync.1 \
  "$stage/usr/share/man/man1/mailswiftsync.1"
gzip -n -9 "$stage/usr/share/man/man1/mailswiftsync.1"

# Normalize every staged timestamp so rebuilding the same source and binary
# produces the same archive bytes.
find "$stage" -exec touch --date="@${SOURCE_DATE_EPOCH}" {} +

install -d -m 0755 "$stage/DEBIAN"
cat > "$stage/DEBIAN/control" <<EOF
Package: mailswiftsync
Version: $debian_version
Section: mail
Priority: optional
Architecture: $architecture
Maintainer: Stephan Loesevitz <stephan.loesevitz@gmail.com>
Description: local-first mailbox migration workbench
 MailSwiftSync plans, executes, verifies, and audits mailbox migrations.
 This package does not bundle or configure an IMAP transfer engine.
EOF

mkdir -p "$output_dir"
output_path="$output_dir/$package_name"
dpkg-deb --build --root-owner-group "$stage" "$output_path" >/dev/null
dpkg-deb --info "$output_path" >/dev/null
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$output_dir" && sha256sum "$package_name") > "$output_path.sha256"
else
  (cd "$output_dir" && shasum -a 256 "$package_name") > "$output_path.sha256"
fi
printf 'PASS: built %s\n' "$output_path"
