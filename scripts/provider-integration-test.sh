#!/usr/bin/env bash
set -euo pipefail

# Provider-specific IMAP integration test template.
#
# This script demonstrates how to test MailSwiftSync against a real IMAP
# provider (Gmail, Microsoft 365, Fastmail, etc.) by running a complete
# dry pilot, live migration, recovery, and evidence export cycle.
#
# CREDENTIALS HANDLING:
# This script expects credentials to be supplied as environment variables or
# files, never committed to the repository. See the provider-testing-guide.md
# for setup instructions specific to each provider.
#
# Usage:
#   MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET=/run/secrets/source \
#   MAILSWIFTSYNC_PROVIDER_DEST_SECRET=/run/secrets/dest \
#     scripts/provider-integration-test.sh gmail
#
# Environment variables (required):
#   MAILSWIFTSYNC_PROVIDER_BINARY  — path to packaged MailSwiftSync binary
#   MAILSWIFTSYNC_PROVIDER         — provider name (gmail, m365, fastmail)
#   MAILSWIFTSYNC_PROVIDER_SOURCE_ENDPOINT  — source IMAP server (host:port)
#   MAILSWIFTSYNC_PROVIDER_SOURCE_USER      — source account username
#   MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET    — file containing source password/token
#   MAILSWIFTSYNC_PROVIDER_DEST_ENDPOINT    — destination IMAP server
#   MAILSWIFTSYNC_PROVIDER_DEST_USER        — destination account username
#   MAILSWIFTSYNC_PROVIDER_DEST_SECRET      — file containing dest password/token

provider="${1:-}"
if [[ -z "$provider" ]]; then
  echo "Usage: $0 <provider> (gmail|m365|fastmail)" >&2
  exit 1
fi
source_provider="${MAILSWIFTSYNC_SOURCE_PROVIDER:-$provider}"
destination_provider="${MAILSWIFTSYNC_DESTINATION_PROVIDER:-$provider}"
[[ "$source_provider" == "m365" ]] && source_provider="microsoft365"
[[ "$destination_provider" == "m365" ]] && destination_provider="microsoft365"
source_auth="${MAILSWIFTSYNC_PROVIDER_SOURCE_AUTH:-password}"
destination_auth="${MAILSWIFTSYNC_PROVIDER_DEST_AUTH:-password}"
fixture_id="${MAILSWIFTSYNC_PROVIDER_FIXTURE_ID:-provider-${source_provider}-to-${destination_provider}}"
recovery_dest_endpoint="${MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_ENDPOINT:-}"
recovery_dest_user="${MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_USER:-}"
recovery_dest_secret="${MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_SECRET:-}"
if [[ "$source_auth" != "password" && "$source_auth" != "oauth2" ]]; then
  echo "ERROR: MAILSWIFTSYNC_PROVIDER_SOURCE_AUTH must be password or oauth2" >&2
  exit 1
fi
if [[ "$destination_auth" != "password" && "$destination_auth" != "oauth2" ]]; then
  echo "ERROR: MAILSWIFTSYNC_PROVIDER_DEST_AUTH must be password or oauth2" >&2
  exit 1
fi
if [[ "$destination_provider" == "microsoft365" && "$destination_auth" != "oauth2" ]]; then
  echo "ERROR: Microsoft 365 IMAP qualification requires destination OAuth/XOAUTH2" >&2
  echo "Set MAILSWIFTSYNC_PROVIDER_DEST_AUTH=oauth2 and provide an access token file." >&2
  exit 1
fi
if [[ "$source_provider" == "microsoft365" && "$source_auth" != "oauth2" ]]; then
  echo "ERROR: Microsoft 365 IMAP qualification requires source OAuth/XOAUTH2" >&2
  echo "Set MAILSWIFTSYNC_PROVIDER_SOURCE_AUTH=oauth2 and provide an access token file." >&2
  exit 1
fi
scenario_ids="${MAILSWIFTSYNC_PROVIDER_SCENARIO_IDS:-basic-small,forced-interruption}"
if [[ -z "$recovery_dest_endpoint" || -z "$recovery_dest_user" || -z "$recovery_dest_secret" ]]; then
  echo "ERROR: recovery qualification requires a separate destination endpoint, user, and secret" >&2
  echo "Set MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_ENDPOINT, _USER, and _SECRET." >&2
  exit 1
fi
if [[ "$recovery_dest_endpoint" == "${MAILSWIFTSYNC_PROVIDER_DEST_ENDPOINT:-}" && "$recovery_dest_user" == "${MAILSWIFTSYNC_PROVIDER_DEST_USER:-}" ]]; then
  echo "ERROR: recovery destination must not be the normal live destination account" >&2
  exit 1
fi

# Check required environment
for var in MAILSWIFTSYNC_PROVIDER_BINARY MAILSWIFTSYNC_PROVIDER_SOURCE_ENDPOINT \
  MAILSWIFTSYNC_PROVIDER_SOURCE_USER MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET \
  MAILSWIFTSYNC_PROVIDER_DEST_ENDPOINT MAILSWIFTSYNC_PROVIDER_DEST_USER \
  MAILSWIFTSYNC_PROVIDER_DEST_SECRET; do
  if [[ -z "${!var:-}" ]]; then
    echo "ERROR: $var is not set" >&2
    exit 1
  fi
done
if [[ ! -f "$recovery_dest_secret" ]]; then
  echo "ERROR: recovery destination secret file $recovery_dest_secret does not exist" >&2
  exit 1
fi

binary="${MAILSWIFTSYNC_PROVIDER_BINARY}"
if [[ ! -x "$binary" ]]; then
  echo "ERROR: $binary is not executable" >&2
  exit 1
fi

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

mailswiftsync_version_output="$($binary --version 2>&1)" || {
  echo "ERROR: could not query the packaged MailSwiftSync binary version" >&2
  exit 1
}
mailswiftsync_version="$(sed -n 's/^MailSwiftSync[[:space:]]\+\([^[:space:]]\+\).*/\1/p' <<<"$mailswiftsync_version_output" | head -1)"
mailswiftsync_commit="$(sed -n 's/.*(git[[:space:]]\+\([^)]\+\)).*/\1/p' <<<"$mailswiftsync_version_output" | head -1)"
if [[ -z "$mailswiftsync_version" ]]; then
  echo "ERROR: packaged MailSwiftSync binary returned an unparseable version: $mailswiftsync_version_output" >&2
  exit 1
fi
[[ -n "$mailswiftsync_commit" ]] || mailswiftsync_commit="unknown"
mailswiftsync_binary_sha256="$(sha256_file "$binary")"

# Parse endpoints (format: host:port)
source_endpoint="${MAILSWIFTSYNC_PROVIDER_SOURCE_ENDPOINT}"
dest_endpoint="${MAILSWIFTSYNC_PROVIDER_DEST_ENDPOINT}"
source_host="${source_endpoint%:*}"
source_port="${source_endpoint##*:}"
dest_host="${dest_endpoint%:*}"
dest_port="${dest_endpoint##*:}"
imapsync_binary="$(command -v imapsync)"
imapsync_version_output="$($imapsync_binary --version 2>&1)" || {
  echo "ERROR: could not query the imapsync binary version" >&2
  exit 1
}
imapsync_version="$(sed -n -e 's/.*imapsync[[:space:]]\+\([0-9][0-9.]*\).*/\1/p' \
  -e 's/^[[:space:]]*v\?\([0-9][0-9.]*\)[[:space:]]*$/\1/p' \
  <<<"$imapsync_version_output" | head -1)"
if [[ -z "$imapsync_version" ]]; then
  echo "ERROR: could not parse imapsync version: $imapsync_version_output" >&2
  exit 1
fi
imapsync_binary_sha256="$(sha256_file "$imapsync_binary")"

# Validate secret files are owner-only
for secret_file in "${MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET}" "${MAILSWIFTSYNC_PROVIDER_DEST_SECRET}"; do
  if [[ ! -f "$secret_file" ]]; then
    echo "ERROR: secret file $secret_file does not exist" >&2
    exit 1
  fi
  perms=$(stat -c %a "$secret_file" 2>/dev/null || stat -f %OLp "$secret_file" 2>/dev/null || echo "unknown")
  if [[ "$perms" != "600" ]]; then
    echo "WARNING: $secret_file permissions are $perms; should be 600 (owner-only)" >&2
  fi
done

workspace="$(mktemp -d "${TMPDIR:-/tmp}/mailswiftsync-provider-test.XXXXXX")"
cleanup() {
  local status=$?
  if [[ "$status" -ne 0 && -f "${state:-}" ]]; then
    echo "--- MailSwiftSync provider test diagnostics (exit $status) ---" >&2
    "$binary" status "$state" --summary >&2 || true
  fi
  rm -rf -- "$workspace"
  return "$status"
}
trap cleanup EXIT

state="$workspace/state.db"
export XDG_CONFIG_HOME="$workspace/config"
mkdir -p "$XDG_CONFIG_HOME/mailswiftsync"
chmod 0700 "$XDG_CONFIG_HOME" "$XDG_CONFIG_HOME/mailswiftsync"

cat > "$XDG_CONFIG_HOME/mailswiftsync/profile.toml" <<PROFILE
name = "Provider test: $provider"
source_host = "$source_host"
source_port = "$source_port"
source_user = "${MAILSWIFTSYNC_PROVIDER_SOURCE_USER}"
source_tls = "implicit"
source_auth = "$source_auth"
source_credential_id = ""
source_ca_bundle = ""
source_certificate_pin_sha256 = ""
allow_insecure_source_transport = false
destination_host = "$dest_host"
destination_port = "$dest_port"
destination_user = "${MAILSWIFTSYNC_PROVIDER_DEST_USER}"
destination_tls = "implicit"
destination_auth = "$destination_auth"
destination_credential_id = ""
destination_ca_bundle = ""
destination_certificate_pin_sha256 = ""
imapsync_path = "$imapsync_binary"
engine = "ImapSync"
doveadm_path = "$(command -v doveadm)"
ssh_path = "ssh"
dovecot_execution = "local"
dovecot_ssh_user = ""
dovecot_config = ""
batch_concurrency = 1
batch_retry_count = 0
max_messages_per_second = 0
max_bytes_per_second = 0
migration_timeout_hours = 1
allow_remote_password_in_argv = false
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
PROFILE

project_name="Provider test: $provider"

# 1. DRY PILOT
echo "=== Starting dry pilot for $provider ==="
"$binary" headless "$state" preflight \
  --source-secret-file "${MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET}" \
  --destination-secret-file "${MAILSWIFTSYNC_PROVIDER_DEST_SECRET}" \
  >"$workspace/preflight.log" 2>&1 || {
  echo "FAIL: Dry pilot preflight failed" >&2
  tail -50 "$workspace/preflight.log" >&2
  exit 1
}
echo "✓ Dry pilot preflight succeeded"
dry_proof="$workspace/${source_provider}-to-${destination_provider}-dry_pilot-proof.json"
"$binary" customer-proof "$state" "$dry_proof" --allow-incomplete \
  --source-provider "$source_provider" --destination-provider "$destination_provider" \
  --source-auth "$source_auth" --destination-auth "$destination_auth" --fixture-id "$fixture_id" \
  --scenario-ids "$scenario_ids" \
  >"$workspace/dry-proof.log" 2>&1 || {
  cat "$workspace/dry-proof.log" >&2
  echo "FAIL: could not export phase-specific dry-pilot proof" >&2
  exit 1
}

# 2. RECOVERY TEST (for recovery_test phase)
# Copy the preflight state, before any live transfer has populated the
# destination. The interruption must therefore occur during the first live
# migration, not after a completed migration has been copied.
echo "=== Starting recovery test for $provider ==="
recovery_state="$workspace/recovery-state.db"
# Snapshot through MailSwiftSync's SQLite online-backup path so WAL pages and
# the durable ledger are copied consistently for the recovery qualification.
"$binary" backup "$state" "$recovery_state" >"$workspace/recovery-backup.log" 2>&1 || {
  cat "$workspace/recovery-backup.log" >&2
  echo "FAIL: could not create a consistent recovery database snapshot" >&2
  exit 1
}
recovery_xdg_config="$workspace/recovery-config"
cp -a "$XDG_CONFIG_HOME" "$recovery_xdg_config"
python3 - "$recovery_xdg_config/mailswiftsync/profile.toml" "$recovery_dest_endpoint" "$recovery_dest_user" <<'PY'
import json
import sys
from pathlib import Path

path, endpoint, user = sys.argv[1:]
lines = Path(path).read_text(encoding="utf-8").splitlines()
replacements = {
    "destination_host": endpoint.rsplit(":", 1)[0],
    "destination_port": endpoint.rsplit(":", 1)[1],
    "destination_user": user,
}
for index, line in enumerate(lines):
    key = line.split(" = ", 1)[0]
    if key in replacements:
        lines[index] = f"{key} = {json.dumps(replacements[key])}"
Path(path).write_text("\n".join(lines) + "\n", encoding="utf-8")
PY
export XDG_CONFIG_HOME="$recovery_xdg_config"

recovery_timeout="${MAILSWIFTSYNC_PROVIDER_RECOVERY_TIMEOUT_SECONDS:-10}"
if ! [[ "$recovery_timeout" =~ ^[1-9][0-9]*$ ]]; then
  echo "FAIL: MAILSWIFTSYNC_PROVIDER_RECOVERY_TIMEOUT_SECONDS must be a positive integer" >&2
  exit 1
fi
echo "Starting live migration and waiting for durable running state (deadline ${recovery_timeout}s)..."
set +e
setsid "$binary" headless "$recovery_state" live \
  --source-secret-file "${MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET}" \
  --destination-secret-file "$recovery_dest_secret" \
  >"$workspace/recovery-interrupted.log" 2>&1
recovery_pid=$!
recovery_started=0
recovery_deadline=$((SECONDS + recovery_timeout))
while kill -0 "$recovery_pid" 2>/dev/null; do
  recovery_status_probe=$("$binary" status "$recovery_state" 2>/dev/null || true)
  if [[ "$recovery_status_probe" == *'"state": "running"'* ]]; then
    recovery_started=1
    break
  fi
  if (( SECONDS >= recovery_deadline )); then
    break
  fi
  sleep 1
done
if [[ "$recovery_started" -ne 1 ]]; then
  kill -KILL -- "-$recovery_pid" 2>/dev/null || true
  wait "$recovery_pid" 2>/dev/null || true
  set -e
  echo "FAIL: recovery run did not reach observable running state before deadline" >&2
  tail -50 "$workspace/recovery-interrupted.log" >&2 || true
  exit 1
fi
echo "✓ Durable running state observed; interrupting process group"
kill -KILL -- "-$recovery_pid"
wait "$recovery_pid"
recovery_exit=$?
set -e
if [[ "$recovery_exit" -ne 137 ]]; then
  echo "FAIL: recovery run was not interrupted by SIGKILL (exit $recovery_exit)" >&2
  tail -50 "$workspace/recovery-interrupted.log" >&2 || true
  exit 1
fi

# A killed controller must leave durable work requiring recovery. Use the
# supported detailed status command; --no-headers is not a MailSwiftSync CLI
# option and must not be swallowed into an "unknown" result.
recovery_status=$("$binary" status "$recovery_state" 2>"$workspace/recovery-status.log") || {
  cat "$workspace/recovery-status.log" >&2
  echo "FAIL: could not inspect interrupted recovery state" >&2
  exit 1
}
if [[ "$recovery_status" != *'"state": "running"'* && "$recovery_status" != *'"state": "attention"'* && "$recovery_status" != *'"state": "delta_required"'* ]]; then
  echo "FAIL: interrupted run did not leave a recoverable durable state" >&2
  printf '%s\n' "$recovery_status" >&2
  exit 1
fi
echo "✓ Interruption simulated, state recorded for recovery"

"$binary" recover "$recovery_state" >"$workspace/recover.log" 2>&1 || {
  cat "$workspace/recover.log" >&2
  echo "FAIL: durable recovery command failed" >&2
  exit 1
}
recovery_status=$("$binary" status "$recovery_state" 2>/dev/null) || {
  echo "FAIL: could not inspect state after recovery" >&2
  exit 1
}
if [[ "$recovery_status" != *'"state": "attention"'* && "$recovery_status" != *'"state": "delta_required"'* ]]; then
  echo "FAIL: recover did not produce an operator-reviewable continuation state" >&2
  printf '%s\n' "$recovery_status" >&2
  exit 1
fi
echo "✓ Durable recovery state observed"

# Run live again to complete the delta (recovery)
echo "Running live migration again to complete recovery..."
"$binary" headless "$recovery_state" live \
  --source-secret-file "${MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET}" \
  --destination-secret-file "$recovery_dest_secret" \
  >"$workspace/recovery-continued.log" 2>&1 || {
  echo "FAIL: Recovery continuation failed" >&2
  tail -50 "$workspace/recovery-continued.log" >&2
  exit 1
}
echo "✓ Recovery continuation succeeded"
recovery_proof="$workspace/${source_provider}-to-${destination_provider}-recovery_test-proof.json"
"$binary" customer-proof "$recovery_state" "$recovery_proof" \
  --source-provider "$source_provider" --destination-provider "$destination_provider" \
  --source-auth "$source_auth" --destination-auth "$destination_auth" --fixture-id "$fixture_id" \
  --scenario-ids "$scenario_ids" \
  >"$workspace/recovery-proof.log" 2>&1 || {
  cat "$workspace/recovery-proof.log" >&2
  echo "FAIL: could not export phase-specific recovery proof" >&2
  exit 1
}

# 3. LIVE MIGRATION (for live_pilot phase). This runs the normal state after
# the independent interruption/recovery qualification has completed.
export XDG_CONFIG_HOME="$workspace/config"
echo "=== Starting live migration for $provider (live_pilot) ==="
"$binary" headless "$state" live \
  --source-secret-file "${MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET}" \
  --destination-secret-file "${MAILSWIFTSYNC_PROVIDER_DEST_SECRET}" \
  >"$workspace/live.log" 2>&1 || {
  echo "FAIL: Live migration failed" >&2
  tail -50 "$workspace/live.log" >&2
  exit 1
}
echo "✓ Live migration succeeded"
live_proof="$workspace/${source_provider}-to-${destination_provider}-live_pilot-proof.json"
"$binary" customer-proof "$state" "$live_proof" \
  --source-provider "$source_provider" --destination-provider "$destination_provider" \
  --source-auth "$source_auth" --destination-auth "$destination_auth" --fixture-id "$fixture_id" \
  --scenario-ids "$scenario_ids" \
  >"$workspace/live-proof.log" 2>&1 || {
  cat "$workspace/live-proof.log" >&2
  echo "FAIL: could not export phase-specific live proof" >&2
  exit 1
}

# 4. GENERATE PROVIDER EVIDENCE FOR EACH TESTING PHASE
echo "=== Generating provider evidence records ==="

evidence_dir="${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-.}"
mkdir -p "$evidence_dir"

# Helper function to export and convert to evidence format
export_phase_evidence() {
  local proof="$1"
  local phase="$2"
  local run_name="${source_provider}-to-${destination_provider}-${phase}"
  local run_id
  run_id="$(python3 - "$proof" "$phase" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    proof = json.load(handle)
phase = sys.argv[2]
runs = [
    run for run in proof.get("runs", [])
    if isinstance(run, dict)
    and run.get("status") == "completed"
    and run.get("started_at")
    and (phase != "dry_pilot" or run.get("phase_at_start") == "preflight")
]
if not runs:
    raise SystemExit("proof contains no completed run")
selected = max(runs, key=lambda run: (run["started_at"], run["run_id"]))
print(selected["run_id"])
PY
)" || {
    echo "FAIL: could not select a completed run for $phase" >&2
    return 1
  }

  # Generate provider evidence record
  local evidence="$evidence_dir/${run_name}.json"
  cp -- "$proof" "$evidence_dir/$(basename "$proof")"
  python3 "$(dirname "$0")/generate-provider-evidence.py" \
    "$proof" "$source_provider" "$destination_provider" "$phase" \
    --run-id "$run_id" --engine-version "$imapsync_version" \
    --mailswiftsync-version "$mailswiftsync_version" \
    --mailswiftsync-commit "$mailswiftsync_commit" \
    --mailswiftsync-binary-sha256 "$mailswiftsync_binary_sha256" \
    --imapsync-binary-sha256 "$imapsync_binary_sha256" \
    --output "$evidence" || {
    echo "FAIL: Could not generate phase-specific evidence record for $phase" >&2
    return 1
  }

  echo "✓ Generated evidence for $phase: $evidence"
  echo "$evidence"
}

# Generate evidence for each phase
echo "Exporting dry_pilot evidence..."
export_phase_evidence "$dry_proof" "dry_pilot"

echo "Exporting live_pilot evidence..."
export_phase_evidence "$live_proof" "live_pilot"

echo "Exporting recovery_test evidence..."
export_phase_evidence "$recovery_proof" "recovery_test"

# 5. VERIFY EVIDENCE INTEGRITY (for live_pilot phase)
if [[ -f "$live_proof" ]]; then
  echo "=== Verifying proof integrity ==="
  "$binary" verify "$live_proof" >"$workspace/verify.log" 2>&1 || {
    cat "$workspace/verify.log" >&2
    echo "FAIL: customer-proof integrity verification failed" >&2
    exit 1
  }
  echo "✓ Customer-proof integrity verified"
fi

# 6. PRINT SUMMARY
echo "=== Test Summary for $provider ==="
"$binary" status "$state" --summary >"$workspace/summary.log" 2>&1 || true
cat "$workspace/summary.log" || true

echo ""
echo "✓ All tests passed for $provider"
echo "Evidence records saved to: $evidence_dir"
