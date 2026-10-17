import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class RuntimeImageScanTests(unittest.TestCase):
    def test_scan_preserves_full_report_and_propagates_fixable_cve_gate(self):
        with tempfile.TemporaryDirectory(prefix="mss-image-scan-test-") as temporary:
            root = Path(temporary)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            calls = root / "trivy-calls.jsonl"
            fake_trivy = fake_bin / "trivy"
            fake_trivy.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, pathlib, sys\n"
                "args = sys.argv[1:]\n"
                "with open(os.environ['TRIVY_TEST_CALLS'], 'a', encoding='utf-8') as log:\n"
                "    log.write(json.dumps(args) + '\\n')\n"
                "if args == ['image', '--download-db-only']:\n"
                "    raise SystemExit(0)\n"
                "if args == ['--version']:\n"
                "    print('Version: 0.76.0\\nVulnerability DB: test-fixture')\n"
                "    raise SystemExit(0)\n"
                "if '--output' in args:\n"
                "    output = pathlib.Path(args[args.index('--output') + 1])\n"
                "    output.write_text('{\\\"Results\\\": []}', encoding='utf-8')\n"
                "    raise SystemExit(0)\n"
                "if '--ignore-unfixed' in args:\n"
                "    raise SystemExit(int(os.environ['TRIVY_TEST_GATE_EXIT']))\n"
                "raise SystemExit(90)\n",
                encoding="utf-8",
            )
            fake_trivy.chmod(0o755)

            report = root / "reports" / "image.json"
            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
                    "TRIVY_TEST_CALLS": str(calls),
                    "TRIVY_TEST_GATE_EXIT": "1",
                }
            )
            result = subprocess.run(
                ["bash", str(ROOT / "scripts/scan-runtime-image.sh"), "fixture:image", str(report)],
                check=False,
                capture_output=True,
                text=True,
                env=environment,
            )

            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(json.loads(report.read_text(encoding="utf-8")), {"Results": []})
            metadata = report.with_name("image.scanner.txt").read_text(encoding="utf-8")
            self.assertIn("Version: 0.76.0", metadata)
            commands = [json.loads(line) for line in calls.read_text(encoding="utf-8").splitlines()]
            self.assertEqual(commands[:2], [["image", "--download-db-only"], ["--version"]])
            self.assertIn("--ignore-unfixed", commands[-1])
            self.assertIn("--severity", commands[-1])
            self.assertIn("HIGH,CRITICAL", commands[-1])

    def test_trivy_installer_refuses_to_reuse_an_existing_directory(self):
        with tempfile.TemporaryDirectory(prefix="mss-trivy-install-test-") as temporary:
            target = Path(temporary) / "existing"
            target.mkdir()
            result = subprocess.run(
                ["bash", str(ROOT / "scripts/install-pinned-trivy.sh"), str(target)],
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 2)
            self.assertIn("refusing to install Trivy into an existing path", result.stderr)


if __name__ == "__main__":
    unittest.main()
