#!/usr/bin/env python3
"""Guard the distinction between provider smoke runs and release evidence."""

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = (ROOT / "scripts" / "provider-integration-test.sh").read_text(encoding="utf-8")
SCALE_SCRIPT = (ROOT / "scripts" / "imap-integration-smoke.sh").read_text(encoding="utf-8")
SCALE_WATCHDOG = (ROOT / "scripts" / "scale-docker-wait.sh").read_text(encoding="utf-8")
SCALE_CONTAINER_WAIT = (ROOT / "scripts" / "scale-container-wait.sh").read_text(encoding="utf-8")
STORAGE_FAULT_SCRIPT = (ROOT / "scripts" / "engine-storage-fault-smoke.sh").read_text(encoding="utf-8")
SCALE_WORKFLOW = (ROOT / ".github" / "workflows" / "scale-qualification.yml").read_text(encoding="utf-8")


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
        self.assertIn('timeout --kill-after=15s "$command_timeout" "$binary" "$@"', SCALE_SCRIPT)
        self.assertNotIn('timeout --foreground', SCALE_SCRIPT)
        self.assertIn('extra_options="--timeout=30"', SCALE_SCRIPT)
        self.assertIn('extra_options="--timeout=30 --debug"', SCALE_SCRIPT)

    def test_smoke_timeouts_manage_engine_process_groups(self):
        self.assertIn('timeout --kill-after=15s 300 "$binary" "$@"', STORAGE_FAULT_SCRIPT)
        self.assertNotIn('timeout --foreground', STORAGE_FAULT_SCRIPT)

    def test_timeout_force_kills_a_process_group_that_ignores_term(self):
        if shutil.which("timeout") is None or shutil.which("bash") is None:
            self.skipTest("GNU timeout and bash are required for this process-group test")
        result = subprocess.run(
            [
                "timeout",
                "--kill-after=1s",
                "1s",
                "bash",
                "-c",
                "trap '' TERM; while :; do sleep 1; done",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=5,
        )
        self.assertIn(result.returncode, (-9, 137))

    def test_100k_scale_job_allows_command_timeout_and_diagnostic_upload(self):
        self.assertIn("timeout-minutes: 90", SCALE_WORKFLOW)

    def test_100k_scale_job_bounds_container_wait_and_preserves_stall_evidence(self):
        self.assertIn("scale-docker-wait.sh", SCALE_CONTAINER_WAIT)
        self.assertIn('"$container_name" "$wait_output_file" "$watchdog_marker" 3600', SCALE_WORKFLOW)
        self.assertIn("scripts/scale-container-wait.sh", SCALE_WORKFLOW)
        self.assertIn('"$container_name" "$wait_output_file" "$watchdog_marker" 3600 3800', SCALE_WORKFLOW)
        self.assertIn("timeout --kill-after=10s", SCALE_CONTAINER_WAIT)
        self.assertIn("outer_watchdog_triggered=true", SCALE_CONTAINER_WAIT)
        self.assertIn('timeout --kill-after=5s 20 docker kill "$container_name"', SCALE_CONTAINER_WAIT)
        self.assertIn('provider-scale-evidence/scale-watchdog.txt', SCALE_WORKFLOW)
        self.assertIn('timeout --kill-after=15s "$timeout_seconds" docker wait "$container_name"', SCALE_WATCHDOG)
        self.assertIn('timeout --kill-after=5s 20 docker kill "$container_name"', SCALE_WATCHDOG)
        self.assertIn('printf \'watchdog_triggered=true\\n\' > "$watchdog_marker"', SCALE_WATCHDOG)
        self.assertIn('kill -KILL -- "-$wait_pgid"', SCALE_WATCHDOG)
        self.assertIn("60 docker logs", SCALE_WORKFLOW)
        self.assertIn("20 docker rm", SCALE_WORKFLOW)
        self.assertIn("provider-scale-evidence/scale-progress.txt", SCALE_WORKFLOW)
        self.assertIn("tail -n 100", SCALE_WORKFLOW)
        self.assertIn('printf \'begin epoch=%s operation=%s\\n\'', SCALE_SCRIPT)
        self.assertIn('printf \'end epoch=%s operation=%s status=%s\\n\'', SCALE_SCRIPT)
        self.assertIn("process_snapshot epoch=", SCALE_SCRIPT)

    def test_100k_scale_container_uses_init_to_reap_fixture_children(self):
        self.assertIn('docker run --detach --init --name "$container_name"', SCALE_WORKFLOW)

    def test_scale_fixture_assertions_scan_maildir_ids_once(self):
        dockerfile = (ROOT / "Dockerfile").read_text(encoding="utf-8")
        self.assertIn("required_tools+=(python3)", SCALE_SCRIPT)
        self.assertIn("maildir-fixture-scan.py", dockerfile)
        self.assertIn('python3 "$script_dir/maildir-fixture-scan.py" "$destination_maildir"', SCALE_SCRIPT)
        self.assertIn("operation=fixture-message-id-scan", SCALE_SCRIPT)
        scale_count_function = SCALE_SCRIPT.split("maildir_count_matching() {", 1)[1].split("\n}", 1)[0]
        self.assertIn('grep -F -c -- "$pattern" "$fixture_message_ids_file"', scale_count_function)

    def test_fixture_server_shutdown_is_bounded_and_phase_marked(self):
        stop_server = SCALE_SCRIPT.split("  stop_server() {", 1)[1].split("\n  }", 1)[0]
        self.assertIn("timeout --kill-after=2s 10 doveadm -c", stop_server)
        self.assertIn("operation=dovecot-%s-stop", stop_server)
        self.assertIn("terminating the disposable fixture server", stop_server)

    def test_large_scale_fixture_skips_expensive_recursive_teardown(self):
        cleanup = SCALE_SCRIPT.split("cleanup() {", 1)[1].split("\n}\ntrap cleanup EXIT", 1)[0]
        self.assertIn("elif (( scale_messages > 0 )); then", cleanup)
        self.assertIn("workspace_cleanup=skipped_disposable_scale_container", cleanup)
        self.assertIn("Keeping large scale workspace until disposable container teardown", cleanup)

    def test_maildir_fixture_scanner_streams_headers_and_counts_duplicates(self):
        with tempfile.TemporaryDirectory(prefix="mailswiftsync-maildir-scan-") as directory:
            root = Path(directory)
            cur = root / "cur"
            new = root / "new"
            nested = root / ".Archive" / "cur"
            cur.mkdir()
            new.mkdir()
            nested.mkdir(parents=True)
            (cur / "ordinary").write_bytes(
                b"Message-ID: <ordinary@example.test>\n\nbody mailswiftsync-not-a-header@example.test\n"
            )
            (new / "fixture-1").write_bytes(
                b"From: fixture@example.test\nMessage-ID: <mailswiftsync-duplicate@example.test>\n\nbody\n"
            )
            (new / "fixture-2").write_bytes(
                b"Message-ID: <mailswiftsync-duplicate@example.test>\n\nbody\n"
            )
            (new / "scale-message").write_bytes(
                b"Message-ID: <mailswiftsync-scale-000001@example.test>\n\nbody\n"
            )
            (nested / "fixture-3").write_bytes(
                b"message-id: <mailswiftsync-sparse-100@example.test>\n\nbody\n"
            )
            result = subprocess.run(
                ["python3", str(ROOT / "scripts" / "maildir-fixture-scan.py"), str(root)],
                check=False,
                capture_output=True,
                text=True,
                timeout=10,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            sorted(result.stdout.splitlines()),
            [
                "Message-ID: <mailswiftsync-duplicate@example.test>",
                "Message-ID: <mailswiftsync-duplicate@example.test>",
                "Message-ID: <mailswiftsync-sparse-100@example.test>",
            ],
        )
        self.assertIn("examined 5 message files", result.stderr)


if __name__ == "__main__":
    unittest.main()
