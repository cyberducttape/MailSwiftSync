#!/usr/bin/env bash
set -euo pipefail

# Sign the checksum beside one release archive. The private key is supplied
# only through the CI secret and is never written to the repository.
archive="${1:?usage: sign-linux-release.sh <archive> <base64-gpg-key>}"
encoded_key="${2:?usage: sign-linux-release.sh <archive> <base64-gpg-key>}"
checksum="${archive}.sha256"
[[ -f "$archive" && -f "$checksum" ]] || {
  echo "release archive and checksum are required" >&2
  exit 1
}
key_file="$(mktemp)"
trap 'rm -f "$key_file"' EXIT
printf '%s' "$encoded_key" | base64 --decode >"$key_file"
chmod 0600 "$key_file"
gpg --batch --import "$key_file" >/dev/null
gpg --batch --yes --armor --detach-sign --local-user "${RELEASE_GPG_KEY_ID:?RELEASE_GPG_KEY_ID is required}" "$checksum"
gpg --batch --verify "${checksum}.asc" "$checksum"
