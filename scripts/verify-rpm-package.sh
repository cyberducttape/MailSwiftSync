#!/usr/bin/env bash
set -euo pipefail

package="${1:?usage: verify-rpm-package.sh PACKAGE.rpm}"
command -v rpm >/dev/null 2>&1 || { echo "rpm is required" >&2; exit 2; }
[[ -f "$package" ]] || { echo "package not found: $package" >&2; exit 1; }

mapfile -t files < <(rpm -qlp "$package")
for required in \
  "/usr/bin/mailswiftsync" \
  "/usr/share/doc/mailswiftsync/README.md" \
  "/usr/share/doc/mailswiftsync/INSTALL.md" \
  "/usr/share/doc/mailswiftsync/LICENSE" \
  "/usr/share/man/man1/mailswiftsync.1.gz" \
  "/usr/share/bash-completion/completions/mailswiftsync" \
  "/usr/share/zsh/site-functions/_mailswiftsync" \
  "/usr/share/fish/vendor_completions.d/mailswiftsync.fish"; do
  printf '%s\n' "${files[@]}" | grep -Fxq "$required" || {
    echo "package is missing required file: $required" >&2
    exit 1
  }
done
if printf '%s\n' "${files[@]}" | grep -Eq '(^|/)\.git(/|$)'; then
  echo "package contains repository history" >&2
  exit 1
fi
[[ "$(rpm -qp --queryformat '%{NAME}' "$package")" == mailswiftsync ]] || {
  echo "package name is not mailswiftsync" >&2
  exit 1
}
# The package must not hard-require an engine; imapsync is a weak dependency.
if rpm -qpR "$package" | grep -Eq '^(imapsync|dovecot)'; then
  echo "package hard-requires an IMAP engine" >&2
  exit 1
fi
mode="$(rpm -qp --queryformat '[%{FILENAMES} %{FILEMODES:octal}\n]' "$package" |
  awk '$1 == "/usr/bin/mailswiftsync" {print $2}')"
[[ "$mode" == 100755 ]] || {
  echo "binary has unexpected mode: $mode" >&2
  exit 1
}
printf 'PASS: RPM package verified: %s\n' "$package"
