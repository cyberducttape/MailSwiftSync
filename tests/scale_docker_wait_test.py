#!/usr/bin/env python3
"""Exercise the scale container watchdog with a fake Docker CLI."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
WATCHDOG = ROOT / "scripts" / "scale-docker-wait.sh"


class ScaleDockerWaitTests(unittest.TestCase):
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
