#!/usr/bin/env bash
set -euo pipefail

package="${1:?usage: verify-deb-package.sh PACKAGE.deb}"
command -v dpkg-deb >/dev/null 2>&1 || { echo "dpkg-deb is required" >&2; exit 2; }
[[ -f "$package" ]] || { echo "package not found: $package" >&2; exit 1; }

mapfile -t files < <(dpkg-deb --contents "$package" | awk '{print $NF}')
for required in \
  "./usr/bin/mailswiftsync" \
  "./usr/share/doc/mailswiftsync/README.md" \
  "./usr/share/doc/mailswiftsync/INSTALL.md" \
  "./usr/share/man/man1/mailswiftsync.1.gz" \
  "./usr/share/bash-completion/completions/mailswiftsync" \
  "./usr/share/zsh/vendor-completions/_mailswiftsync" \
  "./usr/share/fish/vendor_completions.d/mailswiftsync.fish"; do
  printf '%s\n' "${files[@]}" | grep -Fxq "$required" || {
    echo "package is missing required file: $required" >&2
    exit 1
  }
done
if printf '%s\n' "${files[@]}" | grep -Eq '(^|/)\.git(/|$)'; then
  echo "package contains repository history" >&2
  exit 1
fi
dpkg-deb --info "$package" | grep -Fq 'Package: mailswiftsync'
printf 'PASS: Debian package verified: %s\n' "$package"
