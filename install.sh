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

# Print the download URL of the newest release asset named exactly $1, or of
# the requested --version. Releases are listed newest first.
find_asset_url() {
  asset="$1"
  if [ -n "$version" ]; then
    api="https://api.github.com/repos/$REPO/releases/tags/$version"
  else
    api="https://api.github.com/repos/$REPO/releases?per_page=30"
  fi
  if [ -n "${GITHUB_TOKEN:-}" ] && need curl; then
    # Optional; avoids the anonymous API rate limit on shared CI runners.
    curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -H "Authorization: Bearer $GITHUB_TOKEN" \
      -o "$work/releases.json" "$api" 2>/dev/null || return 1
  else
    download "$api" "$work/releases.json" 2>/dev/null || return 1
  fi
  grep -o '"browser_download_url"[[:space:]]*:[[:space:]]*"[^"]*"' "$work/releases.json" |
    sed 's/.*"\(https:[^"]*\)"$/\1/' |
    grep "/$asset\$" |
    head -n 1
}

install_binary() {
  # install_binary SOURCE
  mkdir -p "$install_dir"
  tmp_target="$install_dir/.mailswiftsync.new.$$"
  cp "$1" "$tmp_target"
  chmod 0755 "$tmp_target"
  mv -f "$tmp_target" "$install_dir/mailswiftsync"
}

# Set url and archive_name to the newest published build for $1.
find_published() {
  url=""
  for extension in tar.gz zip; do
    url="$(find_asset_url "mailswiftsync-$1.$extension" || true)"
    if [ -n "$url" ]; then
      archive_name="mailswiftsync-$1.$extension"
      return 0
    fi
  done
  return 1
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
  # The qualified engine reports exactly 2.314.
  if ! need imapsync; then
    return 1
  fi
  imapsync --version 2>/dev/null | grep -q '^2\.314'
}

offer_engine() {
  if engine_ready; then
    say "    imapsync 2.314 is already installed."
    return 0
  fi
  case "$(uname -s)" in
    Darwin)
      say "    imapsync is not installed. On macOS it needs Perl modules from CPAN;"
      say "    follow INSTALL.OnMac.txt from https://imapsync.lamiral.info/#install"
      say "    (MailSwiftSync is qualified with imapsync 2.314)."
      return 0
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

# Offer to put the install directory on PATH for future terminals.
add_to_path() {
  case "${SHELL:-}" in
    */zsh) profile="$HOME/.zshrc" ;;
    */bash) profile="$HOME/.bashrc" ;;
    */fish) profile="" ;;
    *) profile="$HOME/.profile" ;;
  esac
  [ -n "$profile" ] || return 1
  line="export PATH=\"$install_dir:\$PATH\""
  if [ -f "$profile" ] && grep -qF "$line" "$profile"; then
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
    say "    export PATH=\"$install_dir:\$PATH\""
  fi
  say "Check your setup at any time with:  mailswiftsync doctor"
}

main
