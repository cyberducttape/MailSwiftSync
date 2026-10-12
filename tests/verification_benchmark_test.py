"""Keep synthetic disk benchmarks off tmpfs by default and report their path."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).parents[1]
SCRIPT = ROOT / "scripts" / "benchmark-verification-scale.sh"


class VerificationBenchmarkStorageTests(unittest.TestCase):
    def run_benchmark(self, workdir: Path, stage_parent: Path | None = None) -> str:
        fake_bin = workdir / "fake-bin"
        fake_bin.mkdir()
        cargo = fake_bin / "cargo"
        cargo.write_text(
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            'if [[ " $* " == *" --no-run "* ]]; then exit 0; fi\n'
            'printf "verification-scale shape=%s messages_per_side=%s outcome=ok '
            'peak_rss_mib=1 reconcile_messages_per_sec=30000\\n" '
            '"$MAILSWIFTSYNC_RECONCILIATION_BENCH_SHAPE" '
            '"$MAILSWIFTSYNC_RECONCILIATION_BENCH_MESSAGES"\n',
            encoding="utf-8",
        )
        cargo.chmod(0o755)
        environment = os.environ.copy()
        environment.update(
            {
                "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
                "MAILSWIFTSYNC_VERIFY_SIZES": "1",
                "MAILSWIFTSYNC_VERIFY_SHAPES": "typical",
                "MAILSWIFTSYNC_VERIFY_PEAK_RSS_BUDGET_MIB": "2",
                "MAILSWIFTSYNC_VERIFY_MIN_RECONCILE_RATE": "1",
            }
        )
        if stage_parent is not None:
            environment["MAILSWIFTSYNC_RECONCILIATION_BENCH_DIR"] = str(stage_parent)
        else:
            environment.pop("MAILSWIFTSYNC_RECONCILIATION_BENCH_DIR", None)
        result = subprocess.run(
            ["bash", str(SCRIPT)],
            cwd=workdir,
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Filesystem", result.stdout)
        self.assertIn("verification-scale shape=typical messages_per_side=1", result.stdout)
        return result.stdout

    def test_default_stage_is_under_workspace_target_and_filesystem_is_reported(self):
        with tempfile.TemporaryDirectory(prefix="verification-benchmark-default-") as temporary:
            workspace = Path(temporary)
            output = self.run_benchmark(workspace)
            self.assertIn(
                f"verification_stage_parent={workspace / 'target/verification-scale'}",
                output,
            )

    def test_explicit_stage_parent_is_preserved_and_reported(self):
        with tempfile.TemporaryDirectory(prefix="verification-benchmark-explicit-") as temporary:
            workspace = Path(temporary)
            selected = workspace / "selected-storage"
            output = self.run_benchmark(workspace, selected)
            self.assertIn(f"verification_stage_parent={selected}", output)
            self.assertTrue(selected.is_dir())


if __name__ == "__main__":
    unittest.main()
