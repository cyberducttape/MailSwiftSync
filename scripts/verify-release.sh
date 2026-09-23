#!/usr/bin/env bash
set -euo pipefail

# Verify every checksum file in a release directory.
release_dir="${1:-dist}"
shopt -s nullglob
checksums=("$release_dir"/*.sha256)
if ((${#checksums[@]} == 0)); then
  echo "No SHA-256 manifests found in $release_dir" >&2
  exit 1
fi
for manifest in "${checksums[@]}"; do
  while read -r expected file extra; do
    [[ -n "$expected" && -n "$file" && -z "${extra:-}" ]] || {
      echo "Malformed checksum entry in $manifest" >&2
      exit 1
    }
    [[ "$expected" =~ ^[[:xdigit:]]{64}$ ]] || {
      echo "Invalid SHA-256 digest in $manifest" >&2
      exit 1
    }
    case "$file" in
      "$release_dir"/*) file="${file#"$release_dir"/}" ;;
      ./*) file="${file#./}" ;;
    esac
    case "$file" in
      ""|/*|..|../*|*/../*|*/..)
        echo "Unsafe release path in $manifest: $file" >&2
        exit 1
        ;;
    esac
    path="$release_dir/$file"
    test -f "$path" || { echo "Missing release file: $path" >&2; exit 1; }
    if command -v sha256sum >/dev/null 2>&1; then
      actual="$(sha256sum "$path" | awk '{print $1}')"
    else
      actual="$(shasum -a 256 "$path" | awk '{print $1}')"
    fi
    test "$expected" = "$actual" || {
      echo "Checksum mismatch: $path" >&2
      exit 1
    }
  done < "$manifest"
done
