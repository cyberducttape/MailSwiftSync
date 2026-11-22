#!/bin/sh
# MailSwiftSync installer for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/cyberducttape/MailSwiftSync/main/install.sh | sh
#
# It downloads the newest published MailSwiftSync build for this computer,
# checks its SHA-256 checksum, installs it for the current user (no sudo),
# adds a desktop menu entry on Linux, and offers to install the qualified
# imapsync migration engine. When no build is published for this computer it
# builds from source with Cargo instead.
#
# Options (after `sh -s --` when piping):
#   --dir DIR        install the binary into DIR (default: ~/.local/bin)
#   --version TAG    install a specific release tag instead of the newest
#   --from-source    build with Cargo even if a published build exists
#   --no-engine      do not offer to install imapsync
#   --no-desktop     do not add a desktop menu entry (Linux)
#   --yes            answer yes to every question (for unattended installs)
#   -h, --help       show this help
set -eu

REPO="cyberducttape/MailSwiftSync"
# Publisher trust anchors, empty until a release signing identity is
# configured (docs/distribution/RELEASE_SIGNING.md). Once set, a stable
# release (vX.Y.Z) is refused unless its signature verifies against them;
# a signature that is present but wrong is refused on every channel.
RELEASE_GPG_FINGERPRINT=""
MACOS_TEAM_ID=""
install_dir="${MAILSWIFTSYNC_INSTALL_DIR:-$HOME/.local/bin}"
version=""
from_source=0
want_engine=1
want_desktop=1
assume_yes=0

say() { printf '%s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'USAGE'
Install MailSwiftSync for the current user.

Usage: install.sh [options]
   or: curl -fsSL https://raw.githubusercontent.com/cyberducttape/MailSwiftSync/main/install.sh | sh -s -- [options]

Options:
  --dir DIR        install the binary into DIR (default: ~/.local/bin)
  --version TAG    install a specific release tag instead of the newest
  --from-source    build with Cargo even if a published build exists
  --no-engine      do not offer to install imapsync
  --no-desktop     do not add a desktop menu entry (Linux)
  --yes            answer yes to every question (for unattended installs)
  -h, --help       show this help
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir)
      [ "$#" -ge 2 ] || die "--dir needs a directory"
      install_dir="$2"
      shift 2
      ;;
    --dir=*) install_dir="${1#--dir=}"; shift ;;
    --version)
      [ "$#" -ge 2 ] || die "--version needs a release tag"
      version="$2"
      shift 2
      ;;
    --version=*) version="${1#--version=}"; shift ;;
    --from-source) from_source=1; shift ;;
    --no-engine) want_engine=0; shift ;;
    --no-desktop) want_desktop=0; shift ;;
    --yes | -y) assume_yes=1; shift ;;
    -h | --help)
      usage
      exit 0
      ;;
    *) die "unknown option: $1 (try --help)" ;;
  esac
done

# The install directory ends up in shell startup files and messages, so it
# must be one plain line of text.
case "$install_dir" in
  "") die "the install directory must not be empty" ;;
  *[[:cntrl:]]*) die "the install directory must not contain control characters or newlines" ;;
esac

# Quote $1 for a POSIX shell: wrap in single quotes, escaping embedded ones.
sh_quote() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

# Questions read from the terminal even when this script arrives on stdin
# through `curl | sh`.
has_tty() { [ -r /dev/tty ] && (exec </dev/tty) 2>/dev/null; }

confirm() {
  if [ "$assume_yes" -eq 1 ]; then
    return 0
  fi
  if ! has_tty; then
    return 1
  fi
  printf '%s [Y/n] ' "$1" >/dev/tty
  read -r answer </dev/tty || return 1
  case "$answer" in
    "" | y | Y | yes | YES | Yes) return 0 ;;
    *) return 1 ;;
  esac
}

need() {
  command -v "$1" >/dev/null 2>&1
}

# Run the installed program as a command-line tool only. Without a display it
# cannot open its desktop window, so a build that does not know a subcommand
# fails instead of waiting for a window to be closed.
run_cli() {
  if need timeout; then
    env -u DISPLAY -u WAYLAND_DISPLAY timeout 900 "$install_dir/mailswiftsync" "$@"
  else
    env -u DISPLAY -u WAYLAND_DISPLAY "$install_dir/mailswiftsync" "$@"
  fi
}

# Whether the installed build has a given subcommand.
supports() {
  run_cli --help </dev/null 2>/dev/null | grep -q "^  $1"
}

download() {
  # download URL FILE
  if need curl; then
    curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$2" "$1"
  elif need wget; then
    wget --https-only -q -O "$2" "$1"
  else
    die "curl or wget is required"
  fi
}

sha256_of() {
  if need sha256sum; then
    sha256sum "$1" | cut -d ' ' -f 1
  elif need shasum; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  else
    die "sha256sum or shasum is required to verify the download"
  fi
}

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$arch" in
    x86_64 | amd64) arch="x86_64" ;;
    aarch64 | arm64) arch="aarch64" ;;
    *) return 1 ;;
  esac
  case "$os" in
    Linux) printf '%s-unknown-linux-gnu\n' "$arch" ;;
    Darwin) printf '%s-apple-darwin\n' "$arch" ;;
    *) return 1 ;;
  esac
}

# Print the download URL of the newest published release's archive named
# $1.tar.gz or $1.zip, or of the requested --version. Releases are listed
# newest first.
find_asset_url() {
  asset="$1"
  if [ -n "$version" ]; then
    api="https://api.github.com/repos/$REPO/releases/tags/$version"
  else
    api="https://api.github.com/repos/$REPO/releases?per_page=30"
  fi
  if github_token_usable && need curl; then
    # Optional; avoids the anonymous API rate limit on shared CI runners.
    # The header goes to curl as a config on stdin (printf is a shell
    # builtin), so the token never appears in any process's arguments.
    printf 'header = "Authorization: Bearer %s"\n' "$GITHUB_TOKEN" |
      curl --proto '=https' --tlsv1.2 -fsSL --retry 3 --config - \
        -o "$work/releases.json" "$api" 2>/dev/null || return 1
  else
    download "$api" "$work/releases.json" 2>/dev/null || return 1
  fi
  published_asset_urls <"$work/releases.json" | select_archive "$asset"
}

# Print the first URL (newest release first) whose file is $1.tar.gz or
# $1.zip, matched literally.
select_archive() {
  while IFS= read -r candidate; do
    case "${candidate##*/}" in
      "$1.tar.gz" | "$1.zip")
        printf '%s\n' "$candidate"
        return 0
        ;;
    esac
  done
  return 1
}

# A token is used only if it is a plain token string, so it cannot break out
# of the quoted curl config line.
github_token_usable() {
  [ -n "${GITHUB_TOKEN:-}" ] || return 1
  case "$GITHUB_TOKEN" in
    *[!A-Za-z0-9_.-]*) warn "ignoring GITHUB_TOKEN: unexpected characters"; return 1 ;;
  esac
  return 0
}

# Print the asset download URLs of published (non-draft) releases, in order,
# from a GitHub releases API response. Each release object carries one
# "draft" field before its "assets"; only assets after "draft": false are
# printed, so a draft (or a response without the field) is never selected.
# Prereleases stay eligible: preview builds are published as prereleases.
published_asset_urls() {
  awk '{
      gsub(/"draft"[[:space:]]*:/, "\n&")
      gsub(/"browser_download_url"[[:space:]]*:/, "\n&")
      print
    }' |
    awk '
      /^"draft"[[:space:]]*:/ {
        published = ($0 ~ /^"draft"[[:space:]]*:[[:space:]]*false/)
        next
      }
      /^"browser_download_url"[[:space:]]*:/ {
        if (published && match($0, /"https:[^"]*"/)) {
          print substr($0, RSTART + 1, RLENGTH - 2)
        }
      }'
}

install_binary() {
  # install_binary SOURCE
  final="$install_dir/mailswiftsync"
  mkdir -p "$install_dir" || die "could not create $install_dir"
  [ -d "$install_dir" ] || die "$install_dir is not a directory"
  # `mv` onto a directory (or a link to one) would move the program inside
  # it and still succeed, so refuse that instead of reporting success.
  if [ -d "$final" ] && [ ! -L "$final" ]; then
    die "$final is a directory; move it away or choose another --dir"
  fi
  tmp_target="$install_dir/.mailswiftsync.new.$$"
  rm -f "$tmp_target"
  if ! cp "$1" "$tmp_target" || ! chmod 0755 "$tmp_target"; then
    rm -f "$tmp_target"
    die "could not write $tmp_target"
  fi
  # A symlink at the destination is replaced by the program itself rather
  # than followed.
  if [ -L "$final" ]; then
    rm -f "$final" || { rm -f "$tmp_target"; die "could not replace the link $final"; }
  fi
  if ! mv -f "$tmp_target" "$final"; then
    rm -f "$tmp_target"
    die "could not install $final"
  fi
  if [ ! -f "$final" ] || [ -L "$final" ] || [ ! -x "$final" ]; then
    die "$final is not an executable file after installation"
  fi
}

# Set url and archive_name to the newest published build for $1.
# The newest release wins whichever archive format it uses: macOS builds
# moved from .tar.gz to .zip, so searching all releases for one format first
# would pick an older release.
find_published() {
  url="$(find_asset_url "mailswiftsync-$1" || true)"
  [ -n "$url" ] || return 1
  archive_name="${url##*/}"
}

# The armored public key whose fingerprint is RELEASE_GPG_FINGERPRINT.
release_public_key() {
  cat <<'KEY'
KEY
}

# Whether a release tag is a stable release rather than a preview.
is_stable_tag() {
  printf '%s\n' "$1" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'
}

# verify_checksum_signature TAG CHECKSUM SIGNATURE|""
# Check the Linux release checksum's detached GPG signature against the
# embedded key. Returns normally when the release may be installed.
verify_checksum_signature() {
  if [ -z "$RELEASE_GPG_FINGERPRINT" ]; then
    if is_stable_tag "$1"; then
      warn "this installer has no release signing key yet, so the publisher signature was not checked"
    fi
    return 0
  fi
  if [ -z "$3" ]; then
    is_stable_tag "$1" && die "stable release $1 has no checksum signature; refusing to install it"
    say "    Preview build without a publisher signature (checksum verified only)."
    return 0
  fi
  if ! need gpg; then
    is_stable_tag "$1" && die "gpg is required to check the publisher signature of $1"
    warn "gpg is not installed, so the publisher signature was not checked"
    return 0
  fi
  keyring="$work/gnupg"
  rm -rf "$keyring"
  mkdir -m 700 "$keyring"
  release_public_key | gpg --homedir "$keyring" --batch --quiet --import 2>/dev/null ||
    die "could not load the embedded release signing key"
  status="$(gpg --homedir "$keyring" --batch --status-fd 1 --verify "$3" "$2" 2>/dev/null || true)"
  if ! printf '%s\n' "$status" | grep -q "^\[GNUPG:\] VALIDSIG .*$RELEASE_GPG_FINGERPRINT"; then
    die "the checksum signature of $1 does not verify against the MailSwiftSync release key; nothing was installed"
  fi
  say "    Publisher signature verified (key $RELEASE_GPG_FINGERPRINT)."
}

# verify_macos_signature TAG BINARY
# Check the macOS program's code signature and team. Returns normally when
# the release may be installed.
verify_macos_signature() {
  if [ -z "$MACOS_TEAM_ID" ]; then
    if is_stable_tag "$1"; then
      warn "this installer has no expected macOS signing team yet, so the code signature was not checked"
    fi
    return 0
  fi
  details="$(codesign -dv --verbose=2 "$2" 2>&1 || true)"
  case "$details" in
    *"not signed at all"*)
      is_stable_tag "$1" && die "stable release $1 is not code-signed; refusing to install it"
      say "    Preview build without a code signature (checksum verified only)."
      return 0
      ;;
  esac
  codesign --verify --strict "$2" 2>/dev/null ||
    die "the code signature of $1 is invalid; nothing was installed"
  printf '%s\n' "$details" | grep -qx "TeamIdentifier=$MACOS_TEAM_ID" ||
    die "$1 is signed by a different team than MailSwiftSync's ($MACOS_TEAM_ID); nothing was installed"
  say "    Code signature verified (team $MACOS_TEAM_ID)."
}

# Not called from a condition, so `set -e` stops on any failure here.
install_published() {
  target="$1"
  step "Downloading $archive_name"
  say "    $url"
  download "$url" "$work/$archive_name" || die "download failed"
  download "$url.sha256" "$work/$archive_name.sha256" ||
    die "the release has no checksum for $archive_name; refusing to install it"
  expected="$(cut -d ' ' -f 1 <"$work/$archive_name.sha256" | tr -d '\r\n')"
  actual="$(sha256_of "$work/$archive_name")"
  if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    die "checksum mismatch for $archive_name (expected $expected, got $actual); nothing was installed"
  fi
  say "    SHA-256 verified: $actual"
  release_tag="${url%/*}"
  release_tag="${release_tag##*/}"
  case "$archive_name" in
    *.tar.gz)
      signature=""
      if download "$url.sha256.asc" "$work/$archive_name.sha256.asc" 2>/dev/null; then
        signature="$work/$archive_name.sha256.asc"
      fi
      verify_checksum_signature "$release_tag" "$work/$archive_name.sha256" "$signature"
      ;;
  esac

  mkdir -p "$work/bundle"
  case "$archive_name" in
    *.tar.gz) tar -xzf "$work/$archive_name" -C "$work/bundle" || die "could not unpack $archive_name" ;;
    *.zip)
      need unzip || die "unzip is required to unpack $archive_name"
      unzip -q "$work/$archive_name" -d "$work/bundle" || die "could not unpack $archive_name"
      ;;
  esac
  binary=""
  for candidate in "$work/bundle/mailswiftsync-$target" "$work/bundle/mailswiftsync"; do
    if [ -f "$candidate" ]; then
      binary="$candidate"
      break
    fi
  done
  [ -n "$binary" ] || die "the archive does not contain the mailswiftsync program"
  case "$archive_name" in
    *.zip) [ "$(uname -s)" = Darwin ] && verify_macos_signature "$release_tag" "$binary" ;;
  esac
  install_binary "$binary"
  installed_label="MailSwiftSync $(printf '%s\n' "$url" | sed 's|.*/releases/download/\([^/]*\)/.*|\1|')"
}

install_from_source() {
  if ! need cargo; then
    say ""
    say "No published MailSwiftSync build matches this computer, and Rust is not"
    say "installed, so it cannot be built from source yet. Install Rust with:"
    say ""
    say "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    say ""
    say "then run this installer again with --from-source."
    exit 1
  fi
  step "Building MailSwiftSync from source with Cargo (this takes a few minutes)"
  if [ -n "$version" ]; then
    cargo install --locked --root "$work/cargo" --git "https://github.com/$REPO" --tag "$version" mailswiftsync
  else
    cargo install --locked --root "$work/cargo" --git "https://github.com/$REPO" mailswiftsync
  fi
  install_binary "$work/cargo/bin/mailswiftsync"
}

write_icon() {
  cat >"$1" <<'SVG'
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
  <rect width="64" height="64" rx="14" fill="#13233a"/>
  <rect x="10" y="18" width="34" height="24" rx="3" fill="none" stroke="#6cb6ff" stroke-width="3"/>
  <path d="M11 20l16 12 16-12" fill="none" stroke="#6cb6ff" stroke-width="3" stroke-linejoin="round"/>
  <path d="M34 48h18m-6-6l6 6-6 6" fill="none" stroke="#4fd1a5" stroke-width="3.5" stroke-linecap="round" stroke-linejoin="round"/>
</svg>
SVG
}

install_desktop_entry() {
  data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
  icon_dir="$data_home/icons/hicolor/scalable/apps"
  apps_dir="$data_home/applications"
  mkdir -p "$icon_dir" "$apps_dir"
  write_icon "$icon_dir/mailswiftsync.svg"
  cat >"$apps_dir/mailswiftsync.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=MailSwiftSync
GenericName=Mailbox migration
Comment=Plan, run, and verify IMAP mailbox migrations
Exec="$install_dir/mailswiftsync"
Icon=mailswiftsync
StartupWMClass=mailswiftsync
Terminal=false
Categories=Network;Email;
Keywords=imap;imapsync;email;migration;mailbox;
EOF
  if need update-desktop-database; then
    update-desktop-database "$apps_dir" >/dev/null 2>&1 || true
  fi
  say "    Added MailSwiftSync to your applications menu."
}

engine_ready() {
  # Ask the installed build first: `doctor` resolves the engine exactly as
  # the app does (the saved engine path, such as a private install from
  # `install-engine`, then PATH) and checks for the qualified 2.314.
  if run_cli doctor --json </dev/null 2>/dev/null | tr -d '\n' |
    grep -Eq '"name":[[:space:]]*"imapsync executable",[[:space:]]*"status":[[:space:]]*"qualified"'; then
    return 0
  fi
  # Older builds without `doctor --json`: imapsync 2.314 on PATH.
  need imapsync && imapsync --version 2>/dev/null | grep -q '^2\.314'
}

offer_engine() {
  if engine_ready; then
    say "    imapsync 2.314 is already installed."
    return 0
  fi
  case "$(uname -s)" in
    Darwin)
      if [ ! -x /opt/homebrew/bin/brew ] && [ ! -x /usr/local/bin/brew ]; then
        say "    imapsync is not installed. On macOS, MailSwiftSync installs it with"
        say "    Homebrew: install Homebrew from https://brew.sh, then run"
        say "    mailswiftsync install-engine (qualified version: imapsync 2.314)."
        return 0
      fi
      ;;
  esac
  if ! confirm "imapsync (the engine that copies the mail) is not installed. Install the qualified version now?"; then
    say "    Skipped. Install it later with: mailswiftsync install-engine"
    return 0
  fi
  if ! supports install-engine; then
    warn "this MailSwiftSync build cannot install the engine itself; follow the"
    warn "imapsync steps in docs/distribution/INSTALL.md (version 2.314)."
    return 0
  fi
  if run_cli install-engine --yes </dev/null; then
    return 0
  fi
  warn "the engine was not installed. You can retry with: mailswiftsync install-engine"
  warn "or follow the manual steps in docs/distribution/INSTALL.md."
}

# The PATH line for shell startup files, with the directory shell-quoted so
# no character in it is ever executed.
path_line() {
  # shellcheck disable=SC2016 # $PATH is meant literally, for the profile.
  printf 'export PATH=%s:"$PATH"\n' "$(sh_quote "$install_dir")"
}

# Offer to put the install directory on PATH for future terminals.
add_to_path() {
  case "${SHELL:-}" in
    */zsh) profile="$HOME/.zshrc" ;;
    */bash) profile="$HOME/.bashrc" ;;
    */fish) profile="" ;;
    *) profile="$HOME/.profile" ;;
  esac
  [ -n "$profile" ] || return 1
  line="$(path_line)"
  # Also recognize the unquoted line older installers wrote.
  legacy="export PATH=\"$install_dir:\$PATH\""
  if [ -f "$profile" ] && { grep -qxF "$line" "$profile" || grep -qxF "$legacy" "$profile"; }; then
    return 0
  fi
  confirm "Add $install_dir to your PATH in $profile so 'mailswiftsync' works in new terminals?" ||
    return 1
  printf '\n# Added by the MailSwiftSync installer\n%s\n' "$line" >>"$profile"
  say "    Updated $profile."
}

main() {
  work="$(mktemp -d "${TMPDIR:-/tmp}/mailswiftsync-install.XXXXXX")"
  trap 'rm -rf "$work"' EXIT INT TERM

  say "MailSwiftSync installer"
  target="$(detect_target || true)"
  if [ "$from_source" -eq 0 ] && [ -n "$target" ] && find_published "$target"; then
    install_published "$target"
  else
    if [ "$from_source" -eq 0 ]; then
      if [ -n "$target" ]; then
        say "No published build for $target was found."
      else
        say "No published build exists for $(uname -s) $(uname -m)."
      fi
    fi
    install_from_source
  fi
  step "Installed ${installed_label:-MailSwiftSync} to $install_dir"

  start_hint="Run"
  if [ "$want_desktop" -eq 1 ] && [ "$(uname -s)" = "Linux" ]; then
    install_desktop_entry
    start_hint="Start MailSwiftSync from your applications menu, or run"
  fi

  if [ "$want_engine" -eq 1 ]; then
    step "Checking the migration engine"
    offer_engine
  fi

  on_path=0
  case ":$PATH:" in
    *":$install_dir:"*) on_path=1 ;;
  esac
  if [ "$on_path" -eq 0 ] && add_to_path; then
    on_path=2
  fi
  step "Done"
  if [ "$on_path" -eq 2 ]; then
    say "$start_hint this in a new terminal:"
    say "    mailswiftsync"
  elif [ "$on_path" -eq 1 ]; then
    say "$start_hint:  mailswiftsync"
  else
    say "$start_hint:"
    say "    $install_dir/mailswiftsync"
    say ""
    say "To run it as just 'mailswiftsync', add this line to your shell profile"
    say "(~/.bashrc, ~/.zshrc, or ~/.profile) and open a new terminal:"
    say "    $(path_line)"
  fi
  say "Check your setup at any time with:  mailswiftsync doctor"
}

# Tests source the functions without running the installer.
[ "${MAILSWIFTSYNC_INSTALLER_TEST:-0}" = 1 ] || main
