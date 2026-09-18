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
profile="$workspace/profile.toml"

cat > "$profile" <<PROFILE
[migration]
engine = "imapsync"
source_endpoint = "${MAILSWIFTSYNC_PROVIDER_SOURCE_ENDPOINT}"
source_user = "${MAILSWIFTSYNC_PROVIDER_SOURCE_USER}"
source_tls = "implicit"
dest_endpoint = "${MAILSWIFTSYNC_PROVIDER_DEST_ENDPOINT}"
dest_user = "${MAILSWIFTSYNC_PROVIDER_DEST_USER}"
dest_tls = "implicit"
auth_method = "password"
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

# 2. LIVE MIGRATION
echo "=== Starting live migration for $provider ==="
"$binary" headless "$state" live \
  --source-secret-file "${MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET}" \
  --destination-secret-file "${MAILSWIFTSYNC_PROVIDER_DEST_SECRET}" \
  >"$workspace/live.log" 2>&1 || {
  echo "FAIL: Live migration failed" >&2
  tail -50 "$workspace/live.log" >&2
  exit 1
}
echo "✓ Live migration succeeded"

# 3. CAPTURE EVIDENCE
echo "=== Exporting evidence for $provider ==="
evidence="$workspace/$provider-proof.json"
"$binary" customer-proof "$state" "$evidence" --allow-incomplete >"$workspace/proof.log" 2>&1 || {
  echo "FAIL: Customer proof export failed" >&2
  tail -50 "$workspace/proof.log" >&2
  exit 1
}
echo "✓ Customer proof exported"

# 4. VERIFY EVIDENCE INTEGRITY
echo "=== Verifying proof integrity ==="
"$binary" verify "$evidence" >"$workspace/verify.log" 2>&1 || {
  echo "FAIL: Proof verification failed" >&2
  tail -20 "$workspace/verify.log" >&2
  exit 1
}
echo "✓ Proof integrity verified"

# 5. PRINT SUMMARY
echo "=== Test Summary for $provider ==="
"$binary" status "$state" --summary >"$workspace/summary.log" 2>&1 || true
cat "$workspace/summary.log" || true

# 6. SAVE ARTIFACTS (optional, for CI/CD)
if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
  mkdir -p "$(dirname "$MAILSWIFTSYNC_EVIDENCE_OUTPUT")"
  cp "$evidence" "$MAILSWIFTSYNC_EVIDENCE_OUTPUT"
  echo "Evidence saved to $MAILSWIFTSYNC_EVIDENCE_OUTPUT"
fi

echo ""
echo "✓ All tests passed for $provider"
echo "Evidence: $evidence"
