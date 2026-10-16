#!/usr/bin/env python3
"""Guard the distinction between provider smoke runs and release evidence."""

import os
import shutil
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = (ROOT / "scripts" / "provider-integration-test.sh").read_text(encoding="utf-8")
SCALE_SCRIPT = (ROOT / "scripts" / "imap-integration-smoke.sh").read_text(encoding="utf-8")
SCALE_WATCHDOG = (ROOT / "scripts" / "scale-docker-wait.sh").read_text(encoding="utf-8")
SCALE_CONTAINER_WAIT = (ROOT / "scripts" / "scale-container-wait.sh").read_text(encoding="utf-8")
SCALE_COMPLETION_WAIT = (ROOT / "scripts" / "scale-container-completion-wait.sh").read_text(encoding="utf-8")
STORAGE_FAULT_SCRIPT = (ROOT / "scripts" / "engine-storage-fault-smoke.sh").read_text(encoding="utf-8")
CONTROLLER_CHAOS_SCRIPT = (ROOT / "scripts" / "controller-chaos-smoke.sh").read_text(encoding="utf-8")
PROCESS_SUPERVISION_SCRIPT = (ROOT / "scripts" / "process-supervision-chaos-smoke.sh").read_text(encoding="utf-8")
SCALE_WORKFLOW = (ROOT / ".github" / "workflows" / "scale-qualification.yml").read_text(encoding="utf-8")
INTEGRATION_WORKFLOW = (ROOT / ".github" / "workflows" / "integration.yml").read_text(encoding="utf-8")
PROVIDER_WORKFLOW = (ROOT / ".github" / "workflows" / "provider-smoke.yml").read_text(encoding="utf-8")
PROVIDER_GUIDE = (ROOT / "docs" / "provider-testing-guide.md").read_text(encoding="utf-8")
OAUTH_IMAPSYNC_SMOKE = (ROOT / "scripts" / "oauth-imapsync-refresh-smoke.py").read_text(encoding="utf-8")


class ProviderIntegrationHarnessTests(unittest.TestCase):
    def test_harness_termination_kills_and_reaps_detached_recovery_group(self):
        if not shutil.which("setsid"):
            self.skipTest("setsid is required to exercise detached process-group cleanup")
        start = SCRIPT.index("terminate_recovery_group() {")
        end = SCRIPT.index("\ntrap cleanup EXIT", start)
        cleanup_functions = SCRIPT[start:end]
        self.assertIn("trap 'exit 130' INT", SCRIPT)
        self.assertIn("trap 'exit 143' TERM", SCRIPT)
        with tempfile.TemporaryDirectory(prefix="provider-process-cleanup-") as temp:
            workspace = Path(temp) / "workspace"
            workspace.mkdir()
            pid_file = Path(temp) / "recovery-pid"
            command = "\n".join(
                (
                    cleanup_functions,
                    'workspace="$1"',
                    'state=""',
                    "trap cleanup EXIT",
                    "setsid sleep 60 &",
                    "recovery_pid=$!",
                    'printf "%s" "$recovery_pid" > "$2"',
                    "exit 0",
                )
            )
            result = subprocess.run(
                ["bash", "-c", command, "provider-cleanup-test", str(workspace), str(pid_file)],
                text=True,
                capture_output=True,
                check=False,
                timeout=5,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(workspace.exists(), "cleanup must remove its private workspace")
            process_group = int(pid_file.read_text(encoding="ascii"))
            with self.assertRaises(ProcessLookupError):
                os.killpg(process_group, 0)

    def test_usage_lists_the_implemented_cpanel_smoke_route(self):
        result = subprocess.run(
            ["bash", str(ROOT / "scripts" / "provider-integration-test.sh")],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "gmail|m365|microsoft365|fastmail|cpanel_dovecot", result.stderr
        )

    def test_unqualified_imapsync_version_is_refused_before_migration(self):
        with tempfile.TemporaryDirectory(prefix="provider-engine-version-") as temp:
            root = Path(temp)
            tools = root / "tools"
            tools.mkdir()
            invoked = root / "migration-invoked"
            binary = tools / "mailswiftsync"
            binary.write_text(
                '#!/bin/sh\nif [ "$1" = "--version" ]; then '
                'echo "MailSwiftSync 0.1.0-alpha.1 (git test)"; exit 0; fi\n'
                f'printf invoked > "{invoked}"\nexit 0\n',
                encoding="utf-8",
            )
            binary.chmod(0o700)
            imapsync = tools / "imapsync"
            imapsync.write_text('#!/bin/sh\necho "imapsync 2.229"\n', encoding="utf-8")
            imapsync.chmod(0o700)
            secrets = []
            for name in ("source", "destination", "recovery"):
                secret = root / f"{name}.secret"
                secret.write_text("test-only", encoding="utf-8")
                secret.chmod(0o600)
                secrets.append(secret)
            env = {
                "PATH": f"{tools}:/usr/bin:/bin",
                "MAILSWIFTSYNC_PROVIDER_BINARY": str(binary),
                "MAILSWIFTSYNC_PROVIDER_SOURCE_ENDPOINT": "source.example.test:993",
                "MAILSWIFTSYNC_PROVIDER_SOURCE_USER": "source@example.test",
                "MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET": str(secrets[0]),
                "MAILSWIFTSYNC_PROVIDER_DEST_ENDPOINT": "destination.example.test:993",
                "MAILSWIFTSYNC_PROVIDER_DEST_USER": "destination@example.test",
                "MAILSWIFTSYNC_PROVIDER_DEST_SECRET": str(secrets[1]),
                "MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_ENDPOINT": "recovery.example.test:993",
                "MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_USER": "recovery@example.test",
                "MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_SECRET": str(secrets[2]),
                "TMPDIR": str(root),
            }
            result = subprocess.run(
                ["bash", str(ROOT / "scripts" / "provider-integration-test.sh"), "cpanel_dovecot"],
                cwd=ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("requires qualified imapsync 2.314; found 2.229", result.stderr)
            self.assertFalse(invoked.exists(), "the harness must stop before starting a migration")

    def test_recovery_profile_rewrite_requires_each_destination_field_once(self):
        command = (
            'python3 - "$recovery_xdg_config/mailswiftsync/profile.toml" '
            '"$recovery_dest_endpoint" "$recovery_dest_user" <<\'PY\'\n'
        )
        start = SCRIPT.index(command) + len(command)
        end = SCRIPT.index('\nPY\nexport XDG_CONFIG_HOME="$recovery_xdg_config"', start)
        rewrite = SCRIPT[start:end]
        complete_profile = (
            'source_host = "source.example.test"\n'
            'source_user = "source@example.test"\n'
            'destination_host = "destination.example.test"\n'
            'destination_port = "993"\n'
            'destination_user = "destination@example.test"\n'
        )
        with tempfile.TemporaryDirectory(prefix="provider-recovery-profile-") as temp:
            profile = Path(temp) / "profile.toml"
            profile.write_text(complete_profile, encoding="utf-8")
            result = subprocess.run(
                [
                    "python3",
                    "-c",
                    rewrite,
                    str(profile),
                    "recovery.example.test:2143",
                    "recovery@example.test",
                ],
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            parsed = tomllib.loads(profile.read_text(encoding="utf-8"))
            self.assertEqual(parsed["source_host"], "source.example.test")
            self.assertEqual(parsed["source_user"], "source@example.test")
            self.assertEqual(parsed["destination_host"], "recovery.example.test")
            self.assertEqual(parsed["destination_port"], "2143")
            self.assertEqual(parsed["destination_user"], "recovery@example.test")

            for invalid_profile in (
                complete_profile.replace('destination_user = "destination@example.test"\n', ""),
                complete_profile + 'destination_user = "duplicate@example.test"\n',
            ):
                with self.subTest(invalid_profile=invalid_profile):
                    profile.write_text(invalid_profile, encoding="utf-8")
                    original = profile.read_bytes()
                    failed = subprocess.run(
                        [
                            "python3",
                            "-c",
                            rewrite,
                            str(profile),
                            "recovery.example.test:2143",
                            "recovery@example.test",
                        ],
                        text=True,
                        capture_output=True,
                        check=False,
                    )
                    self.assertNotEqual(failed.returncode, 0)
                    self.assertIn("exactly once", failed.stderr)
                    self.assertEqual(profile.read_bytes(), original)

    def test_cpanel_dovecot_smoke_route_uses_isolated_password_files(self):
        self.assertIn("cpanel-dovecot-to-cpanel-dovecot", PROVIDER_WORKFLOW)
        self.assertIn("CPANEL_SOURCE_PASSWORD", PROVIDER_WORKFLOW)
        self.assertIn("CPANEL_DESTINATION_PASSWORD", PROVIDER_WORKFLOW)
        self.assertIn("CPANEL_RECOVERY_PASSWORD", PROVIDER_WORKFLOW)
        self.assertIn('printf \'%s\' "${!password_var}" > "$secrets/${role}-password"', PROVIDER_WORKFLOW)
        self.assertIn("MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET=/secrets/source-password", PROVIDER_WORKFLOW)
        self.assertIn("MAILSWIFTSYNC_PROVIDER_DEST_SECRET=/secrets/destination-password", PROVIDER_WORKFLOW)
        self.assertIn("MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_SECRET=/secrets/recovery-password", PROVIDER_WORKFLOW)
        self.assertIn("does not test cPanel provisioning", PROVIDER_GUIDE)
        self.assertIn("not release qualification", PROVIDER_WORKFLOW.lower())

    def test_packaged_engine_oauth_callback_contract_is_exercised_without_provider_secrets(self):
        dockerfile = (ROOT / "Dockerfile").read_text(encoding="utf-8")
        self.assertIn("oauth-imapsync-refresh-smoke.py", dockerfile)
        self.assertIn("Exercise packaged imapsync OAuth refresh callbacks", INTEGRATION_WORKFLOW)
        self.assertIn("oauth-imapsync-refresh-smoke.py", INTEGRATION_WORKFLOW)
        self.assertIn('EXPECTED_VERSION = "2.314"', OAUTH_IMAPSYNC_SMOKE)
        self.assertIn('"--oauthrefreshcmd1", source_command', OAUTH_IMAPSYNC_SMOKE)
        self.assertIn('"--oauthrefreshcmd2", destination_command', OAUTH_IMAPSYNC_SMOKE)
        self.assertIn("self.authenticated.add(user)", OAUTH_IMAPSYNC_SMOKE)
        self.assertIn("self.expected_users[user]", OAUTH_IMAPSYNC_SMOKE)
        self.assertIn("after a forced IMAP socket drop", OAUTH_IMAPSYNC_SMOKE)
        self.assertIn("any(count < 2 for count in peer.auth_counts.values())", OAUTH_IMAPSYNC_SMOKE)
        self.assertIn("callback-written token", OAUTH_IMAPSYNC_SMOKE)
        self.assertIn("does not qualify token expiry", OAUTH_IMAPSYNC_SMOKE)

    def test_smoke_harness_does_not_emit_release_qualification_records(self):
        self.assertNotIn("generate-provider-evidence.py", SCRIPT)
        self.assertIn("NOT PROVIDER QUALIFICATION", SCRIPT)
        self.assertIn("mkdir -m 0700 \"$proof_bundle\"", SCRIPT)

    def test_smoke_harness_refuses_same_source_and_destination_identity(self):
        self.assertIn("source and destination must be different mailbox identities", SCRIPT)
        self.assertIn('"$source_identity" == "$destination_identity"', SCRIPT)
        env = {
            "PATH": str(Path(shutil.which("bash")).parent) + ":/usr/bin:/bin",
            "MAILSWIFTSYNC_PROVIDER_BINARY": "/unused",
            "MAILSWIFTSYNC_PROVIDER_SOURCE_ENDPOINT": "imap.example.test:0993",
            "MAILSWIFTSYNC_PROVIDER_SOURCE_USER": "LAB@example.test",
            "MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET": "/unused/source",
            "MAILSWIFTSYNC_PROVIDER_DEST_ENDPOINT": "destination.example.test:993",
            "MAILSWIFTSYNC_PROVIDER_DEST_USER": "destination@example.test",
            "MAILSWIFTSYNC_PROVIDER_DEST_SECRET": "/unused/destination",
            "MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_ENDPOINT": "recovery.example.test:993",
            "MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_USER": "recovery@example.test",
            "MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_SECRET": "/unused/recovery",
        }
        collision_cases = (
            ("destination", "IMAP.EXAMPLE.TEST.:993", "lab@example.test", "source and destination must be different", "imap.example.test:0993"),
            ("recovery", "IMAP.EXAMPLE.TEST.:993", "LAB@example.test", "recovery destination must not be the source", "imap.example.test:0993"),
            ("recovery", "DESTINATION.EXAMPLE.TEST.:0993", "DESTINATION@example.test", "recovery destination must not be the normal live destination", "imap.example.test:0993"),
            ("destination", "[::1]:0993", "lab@example.test", "source and destination must be different", "[0:0:0:0:0:0:0:1]:993"),
        )
        for role, endpoint, user, expected_error, source_endpoint in collision_cases:
            with self.subTest(role=role, endpoint=endpoint, user=user):
                case_env = env.copy()
                case_env["MAILSWIFTSYNC_PROVIDER_SOURCE_ENDPOINT"] = source_endpoint
                prefix = "MAILSWIFTSYNC_PROVIDER_DEST" if role == "destination" else "MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST"
                case_env[f"{prefix}_ENDPOINT"] = endpoint
                case_env[f"{prefix}_USER"] = user
                result = subprocess.run(
                    ["bash", "-c", SCRIPT, "provider-smoke-test", "cpanel_dovecot"],
                    cwd=ROOT,
                    env=case_env,
                    text=True,
                    capture_output=True,
                    check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected_error, result.stderr)
                self.assertNotIn("ERROR: /unused is not executable", result.stderr)

        invalid_endpoint_env = env.copy()
        invalid_endpoint_env["MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_ENDPOINT"] = "bad\nhost:993"
        result = subprocess.run(
            ["bash", "-c", SCRIPT, "provider-smoke-test", "cpanel_dovecot"],
            cwd=ROOT,
            env=invalid_endpoint_env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("IMAP endpoints must be host:port values", result.stderr)
        self.assertNotIn("ERROR: /unused is not executable", result.stderr)

        for invalid_endpoint, expected_error in (
            ("bad..host:993", "invalid DNS hostname"),
            ("-bad.example.test:993", "invalid DNS hostname"),
            ("host.example.test..:993", "invalid DNS hostname"),
            ("127.000.000.001:993", "ambiguous numeric IPv4"),
            ("2130706433:993", "ambiguous numeric IPv4"),
            ("[abcd:1234]:993", "invalid IPv6 literal"),
            ("[127.0.0.1]:993", "must contain an IPv6 address"),
        ):
            with self.subTest(endpoint=invalid_endpoint):
                invalid_host_env = env.copy()
                invalid_host_env["MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_ENDPOINT"] = invalid_endpoint
                result = subprocess.run(
                    ["bash", "-c", SCRIPT, "provider-smoke-test", "cpanel_dovecot"],
                    cwd=ROOT,
                    env=invalid_host_env,
                    text=True,
                    capture_output=True,
                    check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected_error, result.stderr)
                self.assertNotIn("ERROR: /unused is not executable", result.stderr)

    def test_artifact_path_labels_are_sanitized_without_changing_provider_identity(self):
        self.assertIn('source_provider_path="$(safe_path_component "$source_provider")"', SCRIPT)
        self.assertIn('destination_provider_path="$(safe_path_component "$destination_provider")"', SCRIPT)
        self.assertIn('--source-provider "$source_provider" --destination-provider "$destination_provider"', SCRIPT)
        start = SCRIPT.index("safe_path_component() {")
        end = SCRIPT.index("\n}", start) + 2
        helper = SCRIPT[start:end]
        result = subprocess.run(
            ["bash", "-c", helper + '; safe_path_component "$1"', "test", "../../outside/name"],
            text=True,
            capture_output=True,
            check=True,
        )
        self.assertEqual(result.stdout, ".._.._outside_name")

    def test_profile_toml_escapes_usernames_and_engine_paths(self):
        with tempfile.TemporaryDirectory(prefix="provider-smoke-") as temp:
            root = Path(temp)
            tool_dir = root / 'tools "with spaces"'
            tool_dir.mkdir()
            binary = tool_dir / "mailswiftsync"
            binary.write_text(
                '#!/bin/sh\nif [ "$1" = "--version" ]; then echo "MailSwiftSync 0.1.0"; exit 0; fi\nif [ "$1" = "headless" ]; then cp "$XDG_CONFIG_HOME/mailswiftsync/profile.toml" "$PROFILE_CAPTURE"; fi\nexit 1\n',
                encoding="utf-8",
            )
            imapsync = tool_dir / "imapsync"
            imapsync.write_text(
                '#!/bin/sh\necho "imapsync 2.314"\n', encoding="utf-8"
            )
            binary.chmod(0o700)
            imapsync.chmod(0o700)
            secrets = {}
            for name in ("source", "destination", "recovery"):
                secret = root / f"{name}.secret"
                secret.write_text("test-only", encoding="utf-8")
                secret.chmod(0o600)
                secrets[name] = secret

            config_dir = root / "config"
            env = {
                "PATH": f"{tool_dir}:/usr/bin:/bin",
                "XDG_CONFIG_HOME": str(config_dir),
                "TMPDIR": str(root),
                "PROFILE_CAPTURE": str(root / "profile-captured.toml"),
                "MAILSWIFTSYNC_PROVIDER_BINARY": str(binary),
                "MAILSWIFTSYNC_PROVIDER_SOURCE_ENDPOINT": "source.example.test:993",
                "MAILSWIFTSYNC_PROVIDER_SOURCE_USER": 'src "quoted"\\name@example.test',
                "MAILSWIFTSYNC_PROVIDER_SOURCE_SECRET": str(secrets["source"]),
                "MAILSWIFTSYNC_PROVIDER_DEST_ENDPOINT": "destination.example.test:993",
                "MAILSWIFTSYNC_PROVIDER_DEST_USER": 'dest "quoted"\\name@example.test',
                "MAILSWIFTSYNC_PROVIDER_DEST_SECRET": str(secrets["destination"]),
                "MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_ENDPOINT": "recovery.example.test:993",
                "MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_USER": "recovery@example.test",
                "MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_SECRET": str(secrets["recovery"]),
            }
            result = subprocess.run(
                ["bash", str(ROOT / "scripts" / "provider-integration-test.sh"), "cpanel_dovecot"],
                cwd=ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)  # fake product stops at preflight
            self.assertIn("Dry pilot preflight failed", result.stderr, result.stderr)
            profile = root / "profile-captured.toml"
            parsed = tomllib.loads(profile.read_text(encoding="utf-8"))
            self.assertEqual(parsed["source_user"], env["MAILSWIFTSYNC_PROVIDER_SOURCE_USER"])
            self.assertEqual(parsed["destination_user"], env["MAILSWIFTSYNC_PROVIDER_DEST_USER"])
            self.assertEqual(parsed["imapsync_path"], str(imapsync))

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
        start = SCRIPT.index("# Validate all secret files before invoking the packaged product or engine")
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
        self.assertIn('private side channel', SCALE_SCRIPT)
        self.assertIn('command_status=$?', SCALE_SCRIPT)
        self.assertIn('local pipeline_status=125', SCALE_SCRIPT)
        self.assertIn('pipeline_status="$command_status"', SCALE_SCRIPT)
        self.assertNotIn('timeout --foreground', SCALE_SCRIPT)
        self.assertIn('extra_options="--timeout=30"', SCALE_SCRIPT)
        self.assertNotIn('extra_options="--timeout=30 --debug"', SCALE_SCRIPT)

    def test_smoke_timeouts_manage_engine_process_groups(self):
        self.assertIn('timeout --kill-after=15s 300 "$binary" "$@"', STORAGE_FAULT_SCRIPT)
        self.assertNotIn('timeout --foreground', STORAGE_FAULT_SCRIPT)

    def test_file_size_fault_fixtures_disable_core_dump_persistence(self):
        self.assertIn("ulimit -c 0\n  ulimit -f 40", CONTROLLER_CHAOS_SCRIPT)
        self.assertIn("ulimit -c 0\nulimit -f $fsize_limit", STORAGE_FAULT_SCRIPT)

    def test_process_supervision_lab_requires_exact_release_handshake(self):
        self.assertIn("without durable release", PROCESS_SUPERVISION_SCRIPT)
        self.assertIn("printf 'NO\\n'", PROCESS_SUPERVISION_SCRIPT)
        self.assertIn("printf 'GO\\n'", PROCESS_SUPERVISION_SCRIPT)
        self.assertIn("assert_not_started", PROCESS_SUPERVISION_SCRIPT)

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
        self.assertIn("scripts/scale-container-completion-wait.sh", SCALE_WORKFLOW)
        self.assertIn('"$PWD/provider-scale-evidence/scale-progress.txt"', SCALE_WORKFLOW)
        self.assertIn('"$watchdog_marker" 3800)', SCALE_WORKFLOW)
        self.assertNotIn("scripts/scale-container-wait.sh", SCALE_WORKFLOW)
        self.assertIn("timeout --kill-after=10s", SCALE_CONTAINER_WAIT)
        self.assertIn("outer_watchdog_triggered=true", SCALE_CONTAINER_WAIT)
        self.assertIn('timeout --kill-after=5s 20 docker kill "$container_name"', SCALE_CONTAINER_WAIT)
        self.assertIn("harness_cleanup_complete=true exit_status=", SCALE_COMPLETION_WAIT)
        self.assertNotIn("docker kill", SCALE_COMPLETION_WAIT)
        self.assertNotIn("docker wait", SCALE_COMPLETION_WAIT)
        self.assertIn("container_teardown=delegated_to_ephemeral_runner", SCALE_COMPLETION_WAIT)
        self.assertIn("harness_exit_status=%s", SCALE_WORKFLOW)
        self.assertIn("container_teardown=delegated_to_ephemeral_runner", SCALE_WORKFLOW)
        self.assertIn('provider-scale-evidence/scale-watchdog.txt', SCALE_WORKFLOW)
        self.assertIn('timeout --kill-after=15s "$timeout_seconds" docker wait "$container_name"', SCALE_WATCHDOG)
        self.assertIn('timeout --kill-after=5s 20 docker kill "$container_name"', SCALE_WATCHDOG)
        self.assertIn('printf \'watchdog_triggered=true\\n\' > "$watchdog_marker"', SCALE_WATCHDOG)
        self.assertIn('kill -KILL -- "-$wait_pgid"', SCALE_WATCHDOG)
        self.assertNotIn("docker logs", SCALE_WORKFLOW)
        self.assertNotIn("docker rm", SCALE_WORKFLOW)
        self.assertIn("provider-scale-evidence/scale-progress.txt", SCALE_WORKFLOW)
        self.assertIn('printf \'begin epoch=%s operation=%s\\n\'', SCALE_SCRIPT)
        self.assertIn('printf \'end epoch=%s operation=%s status=%s\\n\'', SCALE_SCRIPT)
        self.assertIn("process_snapshot epoch=", SCALE_SCRIPT)

    def test_100k_scale_container_uses_init_to_reap_fixture_children(self):
        self.assertIn('docker run --detach --init --name "$container_name"', SCALE_WORKFLOW)

    def test_scale_fixture_assertions_scan_maildir_ids_once(self):
        dockerfile = (ROOT / "Dockerfile").read_text(encoding="utf-8")
        self.assertIn("COPY assets ./assets", dockerfile)
        self.assertIn("required_tools+=(python3)", SCALE_SCRIPT)
        self.assertIn("generate-scale-maildir.py", SCALE_SCRIPT)
        self.assertIn("MAILSWIFTSYNC_SCALE_TOTAL_BODY_BYTES", SCALE_SCRIPT)
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
