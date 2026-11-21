#!/usr/bin/env bash
set -euo pipefail

# Regression tests for install.sh's file installation, PATH line quoting,
# release selection, and token handling. The installer's functions are
# loaded without running it (MAILSWIFTSYNC_INSTALLER_TEST=1); each case runs
# in a subshell because `die` exits.
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }

load() {
  # shellcheck disable=SC1091
  HOME="$work/home" MAILSWIFTSYNC_INSTALLER_TEST=1 . "$root/install.sh" "$@"
}

printf '#!/bin/sh\necho new\n' >"$work/program"

# --- install_binary ------------------------------------------------------
case_dir() { rm -rf "$work/dir"; mkdir -p "$work/dir"; }

case_dir
( load --dir "$work/dir"; install_binary "$work/program" ) 2>/dev/null ||
  fail "fresh install failed"
[ -f "$work/dir/mailswiftsync" ] && [ -x "$work/dir/mailswiftsync" ] ||
  fail "fresh install did not produce an executable file"

printf 'old\n' >"$work/dir/mailswiftsync"
( load --dir "$work/dir"; install_binary "$work/program" ) 2>/dev/null ||
  fail "replacing an existing file failed"
grep -q new "$work/dir/mailswiftsync" || fail "existing file was not replaced"

( load --dir "$work/dir"; install_binary "$work/program" ) 2>/dev/null ||
  fail "repeated install failed"

case_dir
mkdir "$work/dir/mailswiftsync"
if ( load --dir "$work/dir"; install_binary "$work/program" ) 2>/dev/null; then
  fail "install reported success over an existing directory"
fi
[ -z "$(ls -A "$work/dir/mailswiftsync")" ] ||
  fail "the program was moved inside the existing directory"

case_dir
mkdir "$work/elsewhere"
ln -s "$work/elsewhere" "$work/dir/mailswiftsync"
( load --dir "$work/dir"; install_binary "$work/program" ) 2>/dev/null ||
  fail "replacing a symlink to a directory failed"
[ -f "$work/dir/mailswiftsync" ] && [ ! -L "$work/dir/mailswiftsync" ] ||
  fail "a symlink to a directory was followed instead of replaced"
[ -z "$(ls -A "$work/elsewhere")" ] || fail "the link target directory was written to"

case_dir
printf 'target\n' >"$work/linked-file"
ln -s "$work/linked-file" "$work/dir/mailswiftsync"
( load --dir "$work/dir"; install_binary "$work/program" ) 2>/dev/null ||
  fail "replacing a symlink to a file failed"
grep -q target "$work/linked-file" || fail "the symlink's target file was overwritten"

# An interrupted earlier install leaves a stale staging file; it must not
# break the next install or be left behind by it.
case_dir
printf 'partial' >"$work/dir/.mailswiftsync.new.12345"
( load --dir "$work/dir"; install_binary "$work/program" ) 2>/dev/null ||
  fail "install failed with a stale staging file present"
for leftover in "$work/dir"/.mailswiftsync.new.*; do
  case "$leftover" in */.mailswiftsync.new.12345) ;; *) fail "install left $leftover behind" ;; esac
done

# A missing source fails without touching the installed program.
case_dir
printf 'old\n' >"$work/dir/mailswiftsync"
if ( load --dir "$work/dir"; install_binary "$work/missing" ) 2>/dev/null; then
  fail "install reported success for a missing source"
fi
grep -q old "$work/dir/mailswiftsync" || fail "a failed install changed the installed program"
ls "$work/dir"/.mailswiftsync.new.* >/dev/null 2>&1 && fail "a failed install left a staging file"

# --- PATH line quoting -----------------------------------------------------
marker="$work/injected"
# shellcheck disable=SC2016
for dir in "$work/plain dir" "$work/q'uote" "$work/\$(touch $marker)" "$work/\`touch $marker\`" "$work/back\\slash" "$work/\$HOME"; do
  line="$( load --dir "$dir"; path_line )"
  (
    PATH=/usr/bin:/bin
    eval "$line"
    case "$PATH" in
      "$dir:/usr/bin:/bin") ;;
      *) echo "FAIL: PATH line for '$dir' produced '$PATH'" >&2; exit 1 ;;
    esac
  ) || failures=$((failures + 1))
done
[ ! -e "$marker" ] || fail "a PATH line executed text from the install directory"

newline_dir="$work/new
line"
if ( load --dir "$newline_dir" ) 2>/dev/null; then
  fail "an install directory containing a newline was accepted"
fi

# --- release selection -----------------------------------------------------
cat >"$work/releases.json" <<'JSON'
[{"tag_name":"v9.9.9","draft":true,"prerelease":false,"assets":[{"name":"mailswiftsync-x86_64-unknown-linux-gnu.tar.gz","browser_download_url":"https://github.com/o/r/releases/download/untagged-1/mailswiftsync-x86_64-unknown-linux-gnu.tar.gz"}]},
 {"tag_name":"v1.0.0-alpha.3","draft":false,"prerelease":true,"assets":[{"name":"mailswiftsync-x86_64-unknown-linux-gnu.tar.gz","browser_download_url":"https://github.com/o/r/releases/download/v1.0.0-alpha.3/mailswiftsync-x86_64-unknown-linux-gnu.tar.gz"}]}]
JSON
selected="$( load; published_asset_urls <"$work/releases.json" | head -n 1 )"
[ "$selected" = "https://github.com/o/r/releases/download/v1.0.0-alpha.3/mailswiftsync-x86_64-unknown-linux-gnu.tar.gz" ] ||
  fail "release selection picked '$selected' instead of the published prerelease"
# Pretty-printed responses select the same way.
python3 -c 'import json,sys; print(json.dumps(json.load(open(sys.argv[1])), indent=2))' "$work/releases.json" >"$work/pretty.json"
selected="$( load; published_asset_urls <"$work/pretty.json" | head -n 1 )"
case "$selected" in */v1.0.0-alpha.3/*) ;; *) fail "pretty-printed selection picked '$selected'" ;; esac
# Without a draft field nothing is trusted.
printf '[{"assets":[{"browser_download_url":"https://x/y.tar.gz"}]}]' >"$work/nodraft.json"
[ -z "$( load; published_asset_urls <"$work/nodraft.json" )" ] ||
  fail "an asset without a draft field was selected"

# The newest release wins regardless of archive format (macOS moved from
# .tar.gz to .zip), and names are matched exactly.
cat >"$work/formats.json" <<'JSON'
[{"draft":false,"assets":[{"browser_download_url":"https://github.com/o/r/releases/download/v3/mailswiftsync-aarch64-apple-darwin.zip"},{"browser_download_url":"https://github.com/o/r/releases/download/v3/mailswiftsync-aarch64-apple-darwin.zip.sha256"}]},
 {"draft":false,"assets":[{"browser_download_url":"https://github.com/o/r/releases/download/v1/mailswiftsync-aarch64-apple-darwin.tar.gz"}]}]
JSON
selected="$( load; published_asset_urls <"$work/formats.json" | select_archive mailswiftsync-aarch64-apple-darwin )"
[ "$selected" = "https://github.com/o/r/releases/download/v3/mailswiftsync-aarch64-apple-darwin.zip" ] ||
  fail "archive selection picked '$selected' instead of the newest release's .zip"
if ( load; printf 'https://x/v1/mailswiftsync-x86_64-apple-darwinXzip\n' | select_archive mailswiftsync-x86_64-apple-darwin ) >/dev/null; then
  fail "archive names are not matched literally"
fi

# --- token handling --------------------------------------------------------
# shellcheck disable=SC2034 # read by github_token_usable
( load; GITHUB_TOKEN='ghs_abc.DEF-123_x'; github_token_usable ) 2>/dev/null ||
  fail "a normal token was rejected"
# shellcheck disable=SC2034 # read by github_token_usable
if ( load; GITHUB_TOKEN='abc" -o /tmp/x'; github_token_usable ) 2>/dev/null; then
  fail "a token with quotes was accepted"
fi
# shellcheck disable=SC2016 # matching the literal source text
grep -q 'Authorization: Bearer \$GITHUB_TOKEN" *\\$' "$root/install.sh" &&
  fail "the token is still passed on curl's command line"

if [ "$failures" -ne 0 ]; then
  echo "install.sh tests: $failures failure(s)" >&2
  exit 1
fi
echo "install.sh tests passed"
