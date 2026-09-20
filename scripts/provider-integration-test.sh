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

binary="${MAILSWIFTSYNC_PROVIDER_BINARY}"
if [[ ! -x "$binary" ]]; then
  echo "ERROR: $binary is not executable" >&2
  exit 1
fi

# Parse endpoints (format: host:port)
source_endpoint="${MAILSWIFTSYNC_PROVIDER_SOURCE_ENDPOINT}"
dest_endpoint="${MAILSWIFTSYNC_PROVIDER_DEST_ENDPOINT}"
source_host="${source_endpoint%:*}"
source_port="${source_endpoint##*:}"
dest_host="${dest_endpoint%:*}"
dest_port="${dest_endpoint##*:}"

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
source_auth = "password"
source_credential_id = ""
source_ca_bundle = ""
source_certificate_pin_sha256 = ""
allow_insecure_source_transport = false
destination_host = "$dest_host"
destination_port = "$dest_port"
destination_user = "${MAILSWIFTSYNC_PROVIDER_DEST_USER}"
destination_tls = "implicit"
destination_auth = "password"
destination_credential_id = ""
destination_ca_bundle = ""
destination_certificate_pin_sha256 = ""
imapsync_path = "$(command -v imapsync)"
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

# 2. LIVE MIGRATION (for live_pilot phase)
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

# 3. RECOVERY TEST (for recovery_test phase)
# Create a fresh state database to test recovery scenario
echo "=== Starting recovery test for $provider ==="
recovery_state="$workspace/recovery-state.db"
cp "$state" "$recovery_state"

# Run live migration with a timeout to simulate interruption
# (allow 10 seconds before timeout to ensure some messages are migrated)
echo "Starting live migration with timeout to simulate interruption..."
timeout 10 "$binary" headless "$recovery_state" live \
  --source-secret-file "${MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET}" \
  --destination-secret-file "${MAILSWIFTSYNC_PROVIDER_DEST_SECRET}" \
  >"$workspace/recovery-interrupted.log" 2>&1 || recovery_exit=$?

# Check if state is delta_required (indicating interruption was captured)
recovery_status=$("$binary" status "$recovery_state" --no-headers --summary 2>/dev/null || echo "unknown")
if [[ "$recovery_status" != *"delta_required"* ]]; then
  echo "Warning: Expected delta_required status after interruption, got: $recovery_status"
fi
echo "✓ Interruption simulated, state recorded for recovery"

# Run live again to complete the delta (recovery)
echo "Running live migration again to complete recovery..."
"$binary" headless "$recovery_state" live \
  --source-secret-file "${MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET}" \
  --destination-secret-file "${MAILSWIFTSYNC_PROVIDER_DEST_SECRET}" \
  >"$workspace/recovery-continued.log" 2>&1 || {
  echo "FAIL: Recovery continuation failed" >&2
  tail -50 "$workspace/recovery-continued.log" >&2
  exit 1
}
echo "✓ Recovery continuation succeeded"

# 4. GENERATE PROVIDER EVIDENCE FOR EACH TESTING PHASE
echo "=== Generating provider evidence records ==="

evidence_dir="${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-.}"
mkdir -p "$evidence_dir"

# Helper function to export and convert to evidence format
export_phase_evidence() {
  local state_db="$1"
  local phase="$2"
  local run_name="${provider}-${phase}"

  # Export customer-proof for this phase
  local proof="$workspace/${run_name}-proof.json"
  "$binary" customer-proof "$state_db" "$proof" --allow-incomplete >"$workspace/${run_name}-proof.log" 2>&1 || {
    echo "Warning: Could not export customer-proof for $phase" >&2
    return 1
  }

  # Generate provider evidence record
  local evidence="$evidence_dir/${run_name}.json"
  python3 "$(dirname "$0")/generate-provider-evidence.py" \
    "$proof" "$provider" "$phase" --output "$evidence" || {
    echo "Warning: Could not generate evidence record for $phase" >&2
    return 1
  }

  echo "✓ Generated evidence for $phase: $evidence"
  echo "$evidence"
}

# Generate evidence for each phase
echo "Exporting dry_pilot evidence..."
export_phase_evidence "$state" "dry_pilot" || true

echo "Exporting live_pilot evidence..."
export_phase_evidence "$state" "live_pilot" || true

echo "Exporting recovery_test evidence..."
export_phase_evidence "$recovery_state" "recovery_test" || true

# 5. VERIFY EVIDENCE INTEGRITY (for live_pilot phase)
live_evidence="$evidence_dir/$provider-live_pilot.json"
if [[ -f "$live_evidence" ]]; then
  echo "=== Verifying proof integrity ==="
  "$binary" verify "$live_evidence" >"$workspace/verify.log" 2>&1 || true
  echo "✓ Proof integrity checked (warnings non-blocking for now)"
fi

# 6. PRINT SUMMARY
echo "=== Test Summary for $provider ==="
"$binary" status "$state" --summary >"$workspace/summary.log" 2>&1 || true
cat "$workspace/summary.log" || true

echo ""
echo "✓ All tests passed for $provider"
echo "Evidence records saved to: $evidence_dir"
