#!/usr/bin/env python3
"""Guard the distinction between provider smoke runs and release evidence."""

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = (ROOT / "scripts" / "provider-integration-test.sh").read_text(encoding="utf-8")
SCALE_SCRIPT = (ROOT / "scripts" / "imap-integration-smoke.sh").read_text(encoding="utf-8")
STORAGE_FAULT_SCRIPT = (ROOT / "scripts" / "engine-storage-fault-smoke.sh").read_text(encoding="utf-8")


class ProviderIntegrationHarnessTests(unittest.TestCase):
    def test_smoke_harness_does_not_emit_release_qualification_records(self):
        self.assertNotIn("generate-provider-evidence.py", SCRIPT)
        self.assertIn("NOT PROVIDER QUALIFICATION", SCRIPT)
        self.assertIn("mkdir -m 0700 \"$proof_bundle\"", SCRIPT)

    def test_scenario_claims_are_phase_specific_and_match_execution_order(self):
        self.assertIn('dry_scenario_ids="basic-small"', SCRIPT)
        self.assertIn('live_scenario_ids="basic-small"', SCRIPT)
        self.assertIn('recovery_scenario_ids="basic-small,forced-interruption"', SCRIPT)
        self.assertIn('--scenario-ids "$dry_scenario_ids"', SCRIPT)
        self.assertIn('--scenario-ids "$live_scenario_ids"', SCRIPT)
        self.assertIn('--scenario-ids "$recovery_scenario_ids"', SCRIPT)

    def test_missing_engine_is_reported_before_version_probe(self):
        self.assertIn('command -v imapsync >/dev/null 2>&1', SCRIPT)
        self.assertIn("imapsync is required for provider smoke testing", SCRIPT)
        self.assertIn('doveadm_binary="$(command -v doveadm || true)"', SCRIPT)

    def test_secret_permissions_fail_closed(self):
        message = 'provider smoke testing requires 600 or 400 (owner-only)'
        self.assertIn(message, SCRIPT)
        after_message = SCRIPT.split(message, 1)[1].lstrip().splitlines()
        self.assertEqual(after_message[1].strip(), 'exit 1')

    def test_every_secret_is_checked_before_running_the_binary(self):
        start = SCRIPT.index("# Validate all secret files before invoking any executable")
        end = SCRIPT.index('binary="${MAILSWIFTSYNC_PROVIDER_BINARY}"', start)
        validation = SCRIPT[start:end]
        for secret in (
            '"${MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET}"',
            '"${MAILSWIFTSYNC_PROVIDER_DEST_SECRET}"',
            '"$recovery_dest_secret"',
        ):
            self.assertIn(secret, validation)
        self.assertIn('perms=$(stat -c %a "$secret_file"', validation)
        self.assertIn('exit 1', validation)

    def test_scale_lab_uses_a_bounded_longer_timeout_without_debug_noise(self):
        self.assertIn('local command_timeout=180', SCALE_SCRIPT)
        self.assertIn('command_timeout=2400', SCALE_SCRIPT)
        self.assertIn('timeout "$command_timeout" "$binary" "$@"', SCALE_SCRIPT)
        self.assertNotIn('timeout --foreground', SCALE_SCRIPT)
        self.assertIn('extra_options="--timeout=30"', SCALE_SCRIPT)
        self.assertIn('extra_options="--timeout=30 --debug"', SCALE_SCRIPT)

    def test_smoke_timeouts_manage_engine_process_groups(self):
        self.assertIn('timeout 300 "$binary" "$@"', STORAGE_FAULT_SCRIPT)
        self.assertNotIn('timeout --foreground', STORAGE_FAULT_SCRIPT)


if __name__ == "__main__":
    unittest.main()
