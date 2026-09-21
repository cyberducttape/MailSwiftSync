#!/usr/bin/env bash
set -euo pipefail

# Sign the checksum beside one release archive. The private key is supplied
# only through the CI environment, never through argv or the runner's default
# keyring.
archive="${1:?usage: sign-linux-release.sh <archive>}"
encoded_key="${RELEASE_GPG_PRIVATE_KEY_BASE64:?RELEASE_GPG_PRIVATE_KEY_BASE64 is required}"
checksum="${archive}.sha256"
[[ -f "$archive" && -f "$checksum" ]] || {
  echo "release archive and checksum are required" >&2
  exit 1
}
gnupg_home="$(mktemp -d)"
key_file="$gnupg_home/release-key.asc"
chmod 0700 "$gnupg_home"
trap 'rm -rf "$gnupg_home"' EXIT
printf '%s' "$encoded_key" | base64 --decode >"$key_file"
chmod 0600 "$key_file"
gpg_args=(--homedir "$gnupg_home" --batch --no-tty)
gpg "${gpg_args[@]}" --import "$key_file" >/dev/null
gpg "${gpg_args[@]}" --yes --armor --detach-sign --local-user "${RELEASE_GPG_KEY_ID:?RELEASE_GPG_KEY_ID is required}" "$checksum"
gpg "${gpg_args[@]}" --verify "${checksum}.asc" "$checksum"
