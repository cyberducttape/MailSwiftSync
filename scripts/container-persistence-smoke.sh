#!/usr/bin/env bash
set -euo pipefail

# Verify the distributed image's intended Docker persistence boundary:
# files in the persistent state mount survive container replacement, while
# files in XDG_RUNTIME_DIR do not. This checks Docker volume behavior, not
# host power-loss, backup, or restoration guarantees for SQLite.
image="${1:-mailswiftsync:ci}"
volume="mailswiftsync-persistence-smoke-${GITHUB_RUN_ID:-local}-$$"
container="${volume}-first"
next_container="${volume}-second"
cleanup() {
  docker rm -f "$container" "$next_container" >/dev/null 2>&1 || true
  docker volume rm "$volume" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker volume create "$volume" >/dev/null

docker run --name "$container" --volume "$volume:/var/lib/mailswiftsync" \
  --entrypoint /bin/bash "$image" -euc '
    printf "persistent-volume-marker\n" > /var/lib/mailswiftsync/persistence-smoke-marker
    test -s /var/lib/mailswiftsync/persistence-smoke-marker
    printf "ephemeral-runtime-marker\n" > "$XDG_RUNTIME_DIR/persistence-smoke-marker"
    test -s "$XDG_RUNTIME_DIR/persistence-smoke-marker"
  '
docker rm "$container" >/dev/null

docker run --name "$next_container" --volume "$volume:/var/lib/mailswiftsync" \
  --entrypoint /bin/bash "$image" -euc '
    test -s /var/lib/mailswiftsync/persistence-smoke-marker
    test ! -e "$XDG_RUNTIME_DIR/persistence-smoke-marker"
    /usr/local/bin/mailswiftsync --version >/dev/null
  '

echo "PASS: state volume persisted across container replacement; runtime directory was ephemeral"
