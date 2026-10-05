#!/usr/bin/env python3
"""Exercise the scale container watchdog with a fake Docker CLI."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
WATCHDOG = ROOT / "scripts" / "scale-docker-wait.sh"
CONTAINER_WAIT = ROOT / "scripts" / "scale-container-wait.sh"
COMPLETION_WAIT = ROOT / "scripts" / "scale-container-completion-wait.sh"
INTEGRATION_SMOKE = ROOT / "scripts" / "imap-integration-smoke.sh"
SCALE_WORKFLOW = ROOT / ".github" / "workflows" / "scale-qualification.yml"


class ScaleDockerWaitTests(unittest.TestCase):
    def test_disposable_scale_teardown_is_explicit_and_has_a_completion_marker(self):
        script = INTEGRATION_SMOKE.read_text(encoding="utf-8")
        workflow = SCALE_WORKFLOW.read_text(encoding="utf-8")
        self.assertIn('disposable_scale="${MAILSWIFTSYNC_DISPOSABLE_SCALE:-0}"', script)
        self.assertIn('if [[ "$disposable_scale" == 1 ]]; then', script)
        self.assertIn("dovecot_shutdown=delegated_to_disposable_container_teardown", script)
        self.assertIn("harness_cleanup_complete=true exit_status=%s", script)
        self.assertIn("MAILSWIFTSYNC_DISPOSABLE_SCALE=1", workflow)
        self.assertIn("scale-container-completion-wait.sh", workflow)

    def test_completion_marker_releases_container_but_preserves_harness_status(self):
        with tempfile.TemporaryDirectory(prefix="scale-completion-wait-test-") as temporary:
            directory = Path(temporary)
            binary_dir = directory / "bin"
            binary_dir.mkdir()
            fake_docker = binary_dir / "docker"
            fake_docker.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "case \"$1\" in\n"
                "  kill) touch \"$FAKE_DOCKER_KILL_MARKER\" ;;\n"
                "  wait) printf '137\\n' ;;\n"
                "  *) exit 2 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            fake_docker.chmod(0o700)
            progress = directory / "scale-progress.txt"
            progress.write_text("harness_cleanup_complete=true exit_status=0\n", encoding="utf-8")
            kill_marker = directory / "container-killed"
            watchdog_marker = directory / "watchdog-marker.txt"
            environment = os.environ.copy()
            environment["PATH"] = f"{binary_dir}{os.pathsep}{environment['PATH']}"
            environment["FAKE_DOCKER_KILL_MARKER"] = str(kill_marker)
            result = subprocess.run(
                ["bash", str(COMPLETION_WAIT), str(progress), str(watchdog_marker), "5"],
                check=False, capture_output=True, text=True, env=environment, timeout=8,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "0")
            self.assertIn("container_teardown=delegated_to_ephemeral_runner", result.stderr)
            self.assertFalse(kill_marker.exists())

    def test_missing_completion_marker_kills_container_and_preserves_watchdog_marker(self):
        with tempfile.TemporaryDirectory(prefix="scale-missing-completion-test-") as temporary:
            directory = Path(temporary)
            binary_dir = directory / "bin"
            binary_dir.mkdir()
            kill_marker = directory / "container-killed"
            fake_docker = binary_dir / "docker"
            fake_docker.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "case \"$1\" in\n"
                "  kill) touch \"$FAKE_DOCKER_KILL_MARKER\" ;;\n"
                "  *) exit 2 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            fake_docker.chmod(0o700)
            progress = directory / "scale-progress.txt"
            progress.write_text("begin epoch=1 operation=live\\n", encoding="utf-8")
            watchdog_marker = directory / "watchdog-marker.txt"
            environment = os.environ.copy()
            environment["PATH"] = f"{binary_dir}{os.pathsep}{environment['PATH']}"
            environment["FAKE_DOCKER_KILL_MARKER"] = str(kill_marker)
            result = subprocess.run(
                ["bash", str(COMPLETION_WAIT), str(progress), str(watchdog_marker), "1"],
                check=False, capture_output=True, text=True, env=environment, timeout=8,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "124")
            self.assertFalse(kill_marker.exists())
            self.assertEqual(
                watchdog_marker.read_text(encoding="utf-8").strip(),
                "completion_watchdog_triggered=true",
            )

    @unittest.skipIf(os.geteuid() == 0, "root can traverse any directory")
    def test_unreadable_progress_directory_fails_fast(self):
        with tempfile.TemporaryDirectory(prefix="scale-unreadable-progress-test-") as temporary:
            directory = Path(temporary)
            private = directory / "evidence"
            private.mkdir()
            progress = private / "scale-progress.txt"
            progress.write_text("harness_cleanup_complete=true exit_status=0\n", encoding="utf-8")
            private.chmod(0o000)
            watchdog_marker = directory / "watchdog-marker.txt"
            try:
                result = subprocess.run(
                    ["bash", str(COMPLETION_WAIT), str(progress), str(watchdog_marker), "60"],
                    check=False, capture_output=True, text=True, timeout=8,
                )
            finally:
                private.chmod(0o700)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "125")
            self.assertIn("Cannot read the scale progress directory", result.stderr)
            self.assertEqual(
                watchdog_marker.read_text(encoding="utf-8").strip(),
                "completion_watchdog_unreadable_progress=true",
            )

    def test_disposable_scale_mode_requires_scale_fixture(self):
        environment = os.environ.copy()
        environment["MAILSWIFTSYNC_SCALE_MESSAGES"] = "0"
        environment["MAILSWIFTSYNC_DISPOSABLE_SCALE"] = "1"
        result = subprocess.run(
            ["bash", str(INTEGRATION_SMOKE)],
            check=False,
            capture_output=True,
            text=True,
            env=environment,
            timeout=5,
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("requires a scale message fixture", result.stderr)

    def test_outer_deadline_kills_a_stuck_helper_and_preserves_its_marker(self):
        with tempfile.TemporaryDirectory(prefix="scale-outer-watchdog-test-") as temporary:
            directory = Path(temporary)
            binary_dir = directory / "bin"
            binary_dir.mkdir()
            kill_marker = directory / "container-killed"
            fake_docker = binary_dir / "docker"
            fake_docker.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "case \"$1\" in\n"
                "  wait) while :; do sleep 0.02; done ;;\n"
                "  kill) touch \"$FAKE_DOCKER_KILL_MARKER\" ;;\n"
                "  *) exit 2 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            fake_docker.chmod(0o700)
            wait_output = directory / "wait-output.txt"
            watchdog_marker = directory / "watchdog-marker.txt"
            environment = os.environ.copy()
            environment["PATH"] = f"{binary_dir}{os.pathsep}{environment['PATH']}"
            environment["FAKE_DOCKER_KILL_MARKER"] = str(kill_marker)
            result = subprocess.run(
                [
                    "bash",
                    str(CONTAINER_WAIT),
                    "test-container",
                    str(wait_output),
                    str(watchdog_marker),
                    "60",
                    "1",
                ],
                check=False,
                capture_output=True,
                text=True,
                env=environment,
                timeout=8,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "124")
            self.assertTrue(kill_marker.exists())
            self.assertEqual(watchdog_marker.read_text().strip(), "outer_watchdog_triggered=true")

    def run_watchdog(
        self, *, complete: bool, fail_kill: bool = False
    ) -> tuple[subprocess.CompletedProcess, Path]:
        temporary = tempfile.TemporaryDirectory(prefix="scale-watchdog-test-")
        self.addCleanup(temporary.cleanup)
        directory = Path(temporary.name)
        binary_dir = directory / "bin"
        binary_dir.mkdir()
        kill_marker = directory / "container-killed"
        fake_docker = binary_dir / "docker"
        fake_docker.write_text(
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            "case \"$1\" in\n"
            "  wait)\n"
            + ("    printf '0\\n'\n" if complete else
               "    while [[ ! -f \"$FAKE_DOCKER_KILL_MARKER\" ]]; do sleep 0.02; done\n"
               "    printf '137\\n'\n")
            + "    ;;\n"
            + ("  kill) exit 1 ;;\n" if fail_kill else
               "  kill) touch \"$FAKE_DOCKER_KILL_MARKER\" ;;\n")
            + "  *) exit 2 ;;\n"
            "esac\n",
            encoding="utf-8",
        )
        fake_docker.chmod(0o700)
        wait_output = directory / "wait-output.txt"
        watchdog_marker = directory / "watchdog-marker.txt"
        environment = os.environ.copy()
        environment["PATH"] = f"{binary_dir}{os.pathsep}{environment['PATH']}"
        environment["FAKE_DOCKER_KILL_MARKER"] = str(kill_marker)
        result = subprocess.run(
            [
                "bash",
                str(WATCHDOG),
                "test-container",
                str(wait_output),
                str(watchdog_marker),
                "10" if complete else "1",
                "1",
            ],
            check=False,
            capture_output=True,
            text=True,
            env=environment,
            timeout=8,
        )
        return result, watchdog_marker

    def test_completed_container_returns_its_status_without_triggering_watchdog(self):
        result, watchdog_marker = self.run_watchdog(complete=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "0")
        self.assertFalse(watchdog_marker.exists())

    def test_watchdog_kills_container_and_returns_timeout_status(self):
        result, watchdog_marker = self.run_watchdog(complete=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "124")
        self.assertTrue(watchdog_marker.exists())
        self.assertIn("host watchdog", result.stderr)

    def test_watchdog_bounds_a_wait_client_when_docker_kill_fails(self):
        result, watchdog_marker = self.run_watchdog(complete=False, fail_kill=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "124")
        self.assertTrue(watchdog_marker.exists())


if __name__ == "__main__":
    unittest.main()
