#!/usr/bin/env bash
set -euo pipefail

# Build an RPM from an already-built release binary, with the same payload as
# the Debian package: MailSwiftSync, operator documentation, the man page, and
# shell completions. It does not install or configure imapsync or Dovecot.
script_dir="$(CDPATH="" cd -- "$(dirname -- "$0")" && pwd)"
project_root="$(CDPATH="" cd -- "$script_dir/.." && pwd)"
cd "$project_root"

if ! command -v rpmbuild >/dev/null 2>&1; then
  echo "package-rpm.sh requires rpmbuild" >&2
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
# RPM versions cannot contain '-'; a tilde sorts prereleases before the final
# release, matching the Debian package's version ordering.
rpm_version="${version/-alpha./~alpha.}"
rpm_version="${rpm_version/-beta./~beta.}"
rpm_version="${rpm_version/-rc./~rc.}"
if [[ "$rpm_version" == *-* ]]; then
  echo "unsupported prerelease form for RPM: $version" >&2
  exit 1
fi
architecture="${MAILSWIFTSYNC_RPM_ARCH:-$(uname -m)}"
case "$architecture" in
  x86_64 | aarch64) ;;
  *) echo "unsupported RPM architecture: $architecture" >&2; exit 1 ;;
esac

if [[ -z "${SOURCE_DATE_EPOCH:-}" ]]; then
  SOURCE_DATE_EPOCH="$(git log -1 --format=%ct HEAD)"
fi
export SOURCE_DATE_EPOCH

work="$(mktemp -d "${TMPDIR:-/tmp}/mailswiftsync-rpm.XXXXXX")"
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT
payload="$work/payload"
install -d -m 0755 "$payload/usr/bin" "$payload/usr/share/doc/mailswiftsync" \
  "$payload/usr/share/man/man1" \
  "$payload/usr/share/bash-completion/completions" \
  "$payload/usr/share/zsh/site-functions" \
  "$payload/usr/share/fish/vendor_completions.d"
install -m 0755 "$binary" "$payload/usr/bin/mailswiftsync"
install -m 0644 README.md LICENSE PRODUCTION_STATUS.md \
  "$payload/usr/share/doc/mailswiftsync/"
install -m 0644 docs/distribution/INSTALL.md \
  "$payload/usr/share/doc/mailswiftsync/INSTALL.md"
install -m 0644 docs/distribution/mailswiftsync.1 \
  "$payload/usr/share/man/man1/mailswiftsync.1"
gzip -n -9 "$payload/usr/share/man/man1/mailswiftsync.1"
# Completions come from the shipped binary so they list its exact commands.
"$binary" completions bash > "$payload/usr/share/bash-completion/completions/mailswiftsync"
"$binary" completions zsh > "$payload/usr/share/zsh/site-functions/_mailswiftsync"
"$binary" completions fish > "$payload/usr/share/fish/vendor_completions.d/mailswiftsync.fish"
chmod 0644 "$payload/usr/share/bash-completion/completions/mailswiftsync" \
  "$payload/usr/share/zsh/site-functions/_mailswiftsync" \
  "$payload/usr/share/fish/vendor_completions.d/mailswiftsync.fish"
find "$payload" -exec touch --date="@${SOURCE_DATE_EPOCH}" {} +

cat > "$work/mailswiftsync.spec" <<EOF
Name:           mailswiftsync
Version:        $rpm_version
Release:        1
Summary:        Local-first mailbox migration workbench
License:        MIT
URL:            https://github.com/cyberducttape/MailSwiftSync
BuildArch:      $architecture
Suggests:       imapsync = 2.314
Suggests:       dovecot

%global debug_package %{nil}
%global __os_install_post %{nil}
%global _build_id_links none

%description
MailSwiftSync plans, executes, verifies, and audits mailbox migrations.
This package does not bundle or configure an IMAP transfer engine. Trusted
verification requires imapsync 2.314; run "mailswiftsync doctor --strict"
to check the installed engine. Removing the package leaves migration
ledgers in the operator's data directory untouched.

%install
cp -a "$payload/." %{buildroot}/

%files
%attr(0755,root,root) /usr/bin/mailswiftsync
%doc /usr/share/doc/mailswiftsync/README.md
%doc /usr/share/doc/mailswiftsync/INSTALL.md
%doc /usr/share/doc/mailswiftsync/PRODUCTION_STATUS.md
%doc /usr/share/doc/mailswiftsync/LICENSE
/usr/share/man/man1/mailswiftsync.1.gz
/usr/share/bash-completion/completions/mailswiftsync
/usr/share/zsh/site-functions/_mailswiftsync
/usr/share/fish/vendor_completions.d/mailswiftsync.fish
EOF

rpmbuild -bb "$work/mailswiftsync.spec" \
  --target "$architecture" \
  --define "_topdir $work/rpmbuild" \
  --define "_buildhost reproducible" \
  --define "use_source_date_epoch_as_buildtime 1" \
  --define "clamp_mtime_to_source_date_epoch 1" \
  >"$work/rpmbuild.log" 2>&1 || { cat "$work/rpmbuild.log" >&2; exit 1; }

built="$(find "$work/rpmbuild/RPMS" -type f -name '*.rpm' -print -quit)"
[[ -n "$built" ]] || { echo "rpmbuild produced no package" >&2; exit 1; }
mkdir -p "$output_dir"
package_name="$(basename -- "$built")"
cp -- "$built" "$output_dir/$package_name"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$output_dir" && sha256sum "$package_name") > "$output_dir/$package_name.sha256"
else
  (cd "$output_dir" && shasum -a 256 "$package_name") > "$output_dir/$package_name.sha256"
fi
printf 'PASS: built %s\n' "$output_dir/$package_name"
