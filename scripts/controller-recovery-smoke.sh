#!/usr/bin/env bash
set -euo pipefail

# Product-level durability lab. This intentionally uses a deterministic
# blocking engine so the controller, ledger, process identity, crash, and
# recovery paths can be tested without depending on an IMAP server.

for command in mailswiftsync timeout; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "SKIP: install mailswiftsync and timeout to run the controller recovery lab" >&2
    exit 77
  fi
done

workspace="$(mktemp -d "${TMPDIR:-/tmp}/mailswiftsync-recovery-lab.XXXXXX")"
controller_pid=""
engine_pid=""
cleanup() {
  if [[ -n "$controller_pid" ]]; then
    kill -KILL "$controller_pid" 2>/dev/null || true
    wait "$controller_pid" 2>/dev/null || true
  fi
  if [[ -n "$engine_pid" ]]; then
    kill -KILL "$engine_pid" 2>/dev/null || true
  fi
  if [[ "${MAILSWIFTSYNC_KEEP_LAB:-0}" != "1" ]]; then
    rm -rf -- "$workspace"
  else
    echo "Keeping controller recovery lab workspace: $workspace" >&2
  fi
}
trap cleanup EXIT

mkdir -p "$workspace/config/mailswiftsync"
chmod 0700 "$workspace" "$workspace/config" "$workspace/config/mailswiftsync"
cat > "$workspace/fake-engine" <<EOF
#!/usr/bin/env bash
if [[ "\${1:-}" == "--version" ]]; then
  printf '%s\n' 'fake-engine 1.0'
  exit 0
fi
printf '%s' "\$\$" > "$workspace/engine.pid"
trap '' TERM INT HUP
sleep 300
EOF
chmod 0700 "$workspace/fake-engine"
printf '%s' 'recovery-source-password' > "$workspace/source.secret"
printf '%s' 'recovery-destination-password' > "$workspace/destination.secret"
chmod 0600 "$workspace/source.secret" "$workspace/destination.secret"

cat > "$workspace/config/mailswiftsync/profile.toml" <<EOF
name = "Controller recovery integration"
source_host = "recovery-source.invalid"
source_port = "143"
source_tls = "plain"
source_user = "source@example.test"
source_auth = "password"
source_credential_id = ""
source_ca_bundle = ""
source_certificate_pin_sha256 = ""
allow_insecure_source_transport = true
destination_host = "recovery-destination.invalid"
destination_user = "destination@example.test"
destination_auth = "password"
destination_credential_id = ""
destination_port = "143"
destination_tls = "starttls"
destination_ca_bundle = ""
destination_certificate_pin_sha256 = ""
imapsync_path = "$workspace/fake-engine"
engine = "ImapSync"
doveadm_path = "doveadm"
dovecot_config = ""
batch_concurrency = 1
batch_retry_count = 0
max_messages_per_second = 0
max_bytes_per_second = 0
migration_timeout_hours = 1
automap = true
addheader = false
justfolders = false
sync_internaldates = true
useuid = true
usecache = true
fastio1 = false
fastio2 = false
allowsizemismatch = false
delete2 = false
extra_options = ""
EOF

export XDG_CONFIG_HOME="$workspace/config"
state="$workspace/state.db"
mailswiftsync headless "$state" preflight \
  --source-secret-file "$workspace/source.secret" \
  --destination-secret-file "$workspace/destination.secret" \
  >"$workspace/controller.log" 2>&1 &
controller_pid="$!"

running=0
for _ in {1..100}; do
  if [[ -f "$state" ]] && mailswiftsync status "$state" 2>/dev/null | grep -q '"state": "running"'; then
    running=1
    break
  fi
  sleep 0.1
done
if [[ "$running" != 1 ]]; then
  echo "FAIL: controller never durably recorded a running mailbox" >&2
  tail -40 "$workspace/controller.log" >&2 || true
  exit 1
fi
echo "PASS: controller durably recorded active work"

# SIGKILL simulates a host/process crash: no graceful cancellation or final
# event is allowed to make the ledger look clean.
kill -KILL "$controller_pid"
wait "$controller_pid" 2>/dev/null || true
controller_pid=""

if ! mailswiftsync status "$state" | grep -q '"state": "running"'; then
  echo "FAIL: crash simulation did not leave durable interrupted work" >&2
  exit 1
fi
echo "PASS: crash simulation left the durable run for recovery"

mailswiftsync recover "$state"
if mailswiftsync status "$state" | grep -q '"state": "running"'; then
  echo "FAIL: recovery left a mailbox running" >&2
  exit 1
fi
if ! mailswiftsync status "$state" | grep -q '"state": "attention"'; then
  echo "FAIL: recovery did not classify interrupted work as attention" >&2
  exit 1
fi
if mailswiftsync status "$state" | grep -q '"active_processes": \[[^]]'; then
  echo "FAIL: recovery left an active process identity" >&2
  exit 1
fi
echo "PASS: recovery cleared process ownership and classified work for review"

# Interrupt the engine while leaving the controller alive. This proves that
# an engine failure is durable and reviewable separately from controller loss.
engine_state="$workspace/engine-interruption.db"
rm -f "$workspace/engine.pid"
mailswiftsync headless "$engine_state" preflight \
  --source-secret-file "$workspace/source.secret" \
  --destination-secret-file "$workspace/destination.secret" \
  >"$workspace/engine-interruption.log" 2>&1 &
controller_pid="$!"

running=0
for _ in {1..100}; do
  if [[ -f "$engine_state" ]] && [[ -s "$workspace/engine.pid" ]] && \
    mailswiftsync status "$engine_state" 2>/dev/null | grep -q '"state": "running"'; then
    running=1
    break
  fi
  sleep 0.1
done
if [[ "$running" != 1 ]]; then
  echo "FAIL: engine interruption fixture never recorded active work" >&2
  tail -40 "$workspace/engine-interruption.log" >&2 || true
  exit 1
fi
engine_pid="$(<"$workspace/engine.pid")"
if [[ ! "$engine_pid" =~ ^[0-9]+$ ]]; then
  echo "FAIL: engine interruption fixture recorded an invalid engine PID" >&2
  exit 1
fi
# The production runner records the engine itself as the process-group leader
# after the internal launcher execs it. Interrupt the whole group to exercise
# the same ownership boundary used by startup recovery.
kill -KILL -- "-$engine_pid"
engine_pid=""

failed=0
for _ in {1..100}; do
  if mailswiftsync status "$engine_state" 2>/dev/null | grep -q '"state": "failed"'; then
    failed=1
    break
  fi
  sleep 0.1
done
if [[ "$failed" != 1 ]]; then
  echo "FAIL: interrupted engine was not classified as a durable failure" >&2
  tail -40 "$workspace/engine-interruption.log" >&2 || true
  exit 1
fi
if ! mailswiftsync status "$engine_state" 2>/dev/null | grep -q '"phase": "attention"'; then
  echo "FAIL: interrupted engine did not move the project into operator review" >&2
  exit 1
fi
wait "$controller_pid" 2>/dev/null || true
controller_pid=""
echo "PASS: engine interruption remained durable and was classified for review"
