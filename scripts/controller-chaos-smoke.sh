#!/usr/bin/env bash
set -euo pipefail

# Storage-fault chaos lab. This complements controller-recovery-smoke.sh
# (arbitrary-point crash/restart) with two fault classes release-readiness.md
# names as outstanding: a storage limit hit while the ledger is being
# written, and detection/recovery of an already-corrupted ledger file. Both
# use only bash builtins and coreutils (dd, cp, cmp) so they run unmodified
# in the minimal runtime image, which has neither python3 nor a sqlite3 CLI.

for command in mailswiftsync dd cmp; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "SKIP: install mailswiftsync, dd, and cmp to run the controller chaos lab" >&2
    exit 77
  fi
done

workspace="$(mktemp -d "${TMPDIR:-/tmp}/mailswiftsync-chaos-lab.XXXXXX")"
controller_pid=""
cleanup() {
  if [[ -n "$controller_pid" ]]; then
    kill -KILL "$controller_pid" 2>/dev/null || true
    wait "$controller_pid" 2>/dev/null || true
  fi
  if [[ "${MAILSWIFTSYNC_KEEP_LAB:-0}" != "1" ]]; then
    rm -rf -- "$workspace"
  else
    echo "Keeping controller chaos lab workspace: $workspace" >&2
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
printf '%s' 'chaos-source-password' > "$workspace/source.secret"
printf '%s' 'chaos-destination-password' > "$workspace/destination.secret"
chmod 0600 "$workspace/source.secret" "$workspace/destination.secret"

write_profile() {
  cat > "$workspace/config/mailswiftsync/profile.toml" <<EOF
name = "Controller chaos integration"
source_host = "chaos-source.invalid"
source_port = "143"
source_tls = "plain"
source_user = "source@example.test"
source_auth = "password"
source_credential_id = ""
source_ca_bundle = ""
source_certificate_pin_sha256 = ""
allow_insecure_source_transport = true
destination_host = "chaos-destination.invalid"
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
}
write_profile
export XDG_CONFIG_HOME="$workspace/config"

# --- Fault 1: a storage limit is hit while the ledger is first being
# written. `ulimit -f` (RLIMIT_FSIZE) needs no root and no real full disk to
# reproduce a write that fails partway through: the kernel refuses to grow
# any file past the limit, exactly like a full filesystem would refuse an
# SQLite page write. A process that does not handle SIGXFSZ is terminated by
# it; what matters is that the ledger is never left half-written in a way
# that later confuses an unrestricted reader.
storage_fault_state="$workspace/storage-fault.db"
set +e
(
  ulimit -f 40
  mailswiftsync headless "$storage_fault_state" preflight \
    --source-secret-file "$workspace/source.secret" \
    --destination-secret-file "$workspace/destination.secret" \
    >"$workspace/storage-fault.log" 2>&1
)
storage_fault_exit=$?
set -e
if [[ "$storage_fault_exit" -eq 0 ]]; then
  echo "FAIL: hitting the file-size limit did not stop the controller" >&2
  cat "$workspace/storage-fault.log" >&2 || true
  exit 1
fi
echo "PASS: a storage limit hit during ledger creation stopped the controller (exit $storage_fault_exit) instead of completing silently"

# The fault above must not leave a ledger an unrestricted reader cannot open:
# SQLite's WAL is expected to roll back whatever partial write the fault
# interrupted, not hand back a file `status` chokes on.
if ! mailswiftsync status "$storage_fault_state" >"$workspace/storage-fault-status.json" 2>"$workspace/storage-fault-status.err"; then
  echo "FAIL: the ledger was unreadable after the storage-limit fault cleared" >&2
  cat "$workspace/storage-fault-status.err" >&2 || true
  exit 1
fi
if ! grep -q '"schema_version"' "$workspace/storage-fault-status.json"; then
  echo "FAIL: status did not return a well-formed ledger after the storage-limit fault" >&2
  exit 1
fi
echo "PASS: the ledger opened cleanly once the storage limit was lifted, with no partial write visible"

# --- Build a populated, then verified-backed-up, ledger for the corruption
# scenarios below.
corruption_state="$workspace/corruption.db"
rm -f "$workspace/engine.pid"
mailswiftsync headless "$corruption_state" preflight \
  --source-secret-file "$workspace/source.secret" \
  --destination-secret-file "$workspace/destination.secret" \
  >"$workspace/corruption-controller.log" 2>&1 &
controller_pid="$!"

running=0
for _ in {1..100}; do
  if [[ -f "$corruption_state" ]] && mailswiftsync status "$corruption_state" 2>/dev/null | grep -q '"state": "running"'; then
    running=1
    break
  fi
  sleep 0.1
done
if [[ "$running" != 1 ]]; then
  echo "FAIL: chaos fixture never durably recorded a running mailbox" >&2
  tail -40 "$workspace/corruption-controller.log" >&2 || true
  exit 1
fi
kill -KILL "$controller_pid"
wait "$controller_pid" 2>/dev/null || true
controller_pid=""

good_backup="$workspace/good-backup.db"
mailswiftsync backup "$corruption_state" "$good_backup"
echo "PASS: created a verified backup of the populated ledger"

# --- Fault 2: a corrupt or truncated restore source must never touch the
# live ledger. A truncated backup simulates a copy interrupted by a full
# disk or a dropped connection; a block of random bytes simulates unrelated
# or bit-rotted content handed to `restore` by mistake.
before_restore_attempts="$workspace/before-restore-attempts.db"
cp "$corruption_state" "$before_restore_attempts"

truncated_backup="$workspace/truncated-backup.db"
head -c 2000 "$good_backup" > "$truncated_backup"
set +e
mailswiftsync restore "$truncated_backup" "$corruption_state" >"$workspace/restore-truncated.log" 2>&1
truncated_restore_exit=$?
set -e
if [[ "$truncated_restore_exit" -eq 0 ]]; then
  echo "FAIL: restore accepted a truncated backup file" >&2
  exit 1
fi
if ! cmp -s "$corruption_state" "$before_restore_attempts"; then
  echo "FAIL: a rejected truncated restore modified the live ledger" >&2
  exit 1
fi
echo "PASS: restore rejected a truncated backup without touching the live ledger"

garbage_backup="$workspace/garbage-backup.db"
dd if=/dev/urandom of="$garbage_backup" bs=1024 count=4 >/dev/null 2>&1
set +e
mailswiftsync restore "$garbage_backup" "$corruption_state" >"$workspace/restore-garbage.log" 2>&1
garbage_restore_exit=$?
set -e
if [[ "$garbage_restore_exit" -eq 0 ]]; then
  echo "FAIL: restore accepted a file that was never a database" >&2
  exit 1
fi
if ! cmp -s "$corruption_state" "$before_restore_attempts"; then
  echo "FAIL: a rejected garbage restore modified the live ledger" >&2
  exit 1
fi
echo "PASS: restore rejected non-database content without touching the live ledger"

# --- Fault 3: a corrupted ledger must be caught, not silently operated on,
# and must remain recoverable from an earlier verified backup. The backup
# API folds the WAL into one plain file, so corrupting its header is a
# deterministic stand-in for storage media corrupting an already-checkpointed
# ledger: every reader validates the SQLite header before touching any page.
corrupted_ledger="$workspace/corrupted-ledger.db"
cp "$good_backup" "$corrupted_ledger"
dd if=/dev/zero of="$corrupted_ledger" bs=1 count=16 conv=notrunc >/dev/null 2>&1

set +e
mailswiftsync status "$corrupted_ledger" >/dev/null 2>"$workspace/corrupted-status.err"
corrupted_status_exit=$?
mailswiftsync recover "$corrupted_ledger" >/dev/null 2>"$workspace/corrupted-recover.err"
corrupted_recover_exit=$?
set -e
if [[ "$corrupted_status_exit" -eq 0 ]]; then
  echo "FAIL: status silently returned output for a corrupted ledger" >&2
  exit 1
fi
if [[ "$corrupted_recover_exit" -eq 0 ]]; then
  echo "FAIL: recover silently proceeded against a corrupted ledger" >&2
  exit 1
fi
echo "PASS: a corrupted ledger was rejected by status and recover instead of being silently used"

mailswiftsync restore "$good_backup" "$corrupted_ledger" >"$workspace/restore-after-corruption.log" 2>&1
if ! mailswiftsync status "$corrupted_ledger" >/dev/null 2>&1; then
  echo "FAIL: the ledger was not readable after restoring from a verified backup" >&2
  exit 1
fi
if ! mailswiftsync recover "$corrupted_ledger" >/dev/null 2>&1; then
  echo "FAIL: recover failed on a ledger restored from a verified backup" >&2
  exit 1
fi
echo "PASS: restoring the verified backup over the corrupted ledger returned it to normal operation"
