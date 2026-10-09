#!/usr/bin/env bash
set -euo pipefail

# Gatekeeper/process-ownership qualification lab. This deliberately exercises
# the production binary's internal launcher, not the test-only direct spawn
# path. The launcher must not execute an engine until the controller writes the
# exact durable-release token. A missing token models a controller crash or a
# launcher killed before durable registration; an invalid token models a
# broken release path. The controller-recovery and storage-fault labs cover
# the ledger and restart halves of this matrix.

for command in mailswiftsync timeout; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "SKIP: install mailswiftsync and timeout to run the process supervision lab" >&2
    exit 77
  fi
done

workspace="$(mktemp -d "${TMPDIR:-/tmp}/mailswiftsync-process-lab.XXXXXX")"
launcher_pid=""
cleanup() {
  if [[ -n "$launcher_pid" ]]; then
    kill -KILL "$launcher_pid" 2>/dev/null || true
    wait "$launcher_pid" 2>/dev/null || true
  fi
  if [[ "${MAILSWIFTSYNC_KEEP_LAB:-0}" != "1" ]]; then
    rm -rf -- "$workspace"
  else
    echo "Keeping process supervision lab workspace: $workspace" >&2
  fi
}
trap cleanup EXIT

cat > "$workspace/fake-engine" <<EOF
#!/usr/bin/env bash
if [[ "\${1:-}" == "--version" ]]; then
  printf '%s\\n' 'fake-engine 1.0'
  exit 0
fi
printf '%s\n' "\$\$" > "$workspace/started.pid"
sleep 1
EOF
chmod 0700 "$workspace/fake-engine"

assert_not_started() {
  if [[ -e "$workspace/started.pid" ]]; then
    echo "FAIL: gatekeeper started the engine before durable release" >&2
    exit 1
  fi
}

# Closing stdin without writing GO is the exact crash-before-release path.
set +e
timeout --kill-after=2s 3s mailswiftsync --internal-launcher "$workspace/fake-engine" -- \
  >"$workspace/no-release.out" 2>"$workspace/no-release.err"
no_release_status=$?
set -e
if [[ "$no_release_status" -eq 0 ]]; then
  echo "FAIL: launcher succeeded without a durable release token" >&2
  exit 1
fi
assert_not_started
echo "PASS: launcher refused to start the engine without durable release"

# An invalid release token must be treated like a broken controller/launcher
# hand-off and must not fall through to the engine.
set +e
printf 'NO\n' | timeout --kill-after=2s 3s mailswiftsync \
  --internal-launcher "$workspace/fake-engine" -- \
  >"$workspace/invalid-release.out" 2>"$workspace/invalid-release.err"
invalid_release_status=$?
set -e
if [[ "$invalid_release_status" -eq 0 ]]; then
  echo "FAIL: launcher accepted an invalid durable release token" >&2
  exit 1
fi
assert_not_started
echo "PASS: launcher rejected an invalid durable release token"

# A valid token is the only path that may execute the engine. Keep this as a
# bounded smoke check so the fixture also detects an accidental token/parser
# regression without leaving a process behind.
set +e
printf 'GO\n' | timeout --kill-after=1s 2s mailswiftsync \
  --internal-launcher "$workspace/fake-engine" -- \
  >"$workspace/release.out" 2>"$workspace/release.err"
release_status=$?
set -e
if [[ ! -s "$workspace/started.pid" ]]; then
  echo "FAIL: launcher did not execute the engine after durable release" >&2
  exit 1
fi
if [[ "$release_status" -ne 0 ]]; then
  echo "FAIL: released engine did not complete normally" >&2
  exit 1
fi
echo "PASS: launcher executed the engine only after durable release"
