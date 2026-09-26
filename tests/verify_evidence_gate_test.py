#!/usr/bin/env python3
"""Exercise the populated-evidence path of the shell release gate."""

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).parents[1]
GATE = ROOT / "scripts" / "verify-evidence-gate.sh"
SOURCE_EVIDENCE = ROOT / "tests" / "provider-evidence"


class EvidenceGateTests(unittest.TestCase):
    def test_populated_evidence_path_reports_validation_errors_not_tracebacks(self):
        try:
            import jsonschema  # noqa: F401
        except ImportError:
            self.skipTest("jsonschema is installed by CI/release requirements")

        with tempfile.TemporaryDirectory() as directory:
            evidence_dir = Path(directory) / "provider-evidence"
            shutil.copytree(SOURCE_EVIDENCE, evidence_dir)
            policy = json.loads((evidence_dir / "policy.json").read_text(encoding="utf-8"))
            entry = policy["providers"][0]
            evidence = {
                "provider": entry["pair"],
                "source_provider": entry["source_provider"],
                "destination_provider": entry["destination_provider"],
                "source_auth_method": "password",
                "destination_auth_method": "password",
                "tested_at": "2026-09-20T00:00:00Z",
                "testing_phase": "live_pilot",
                "source_version": "fixture",
                "destination_version": "fixture",
                "engine": entry["engine"],
                "engine_version": entry["engine_version"],
                "test_summary": {
                    "mailboxes_tested": 1,
                    "messages_total": entry["minimum_messages"],
                    "bytes_total": entry["minimum_bytes"],
                    "destination_messages_total": entry["minimum_messages"],
                    "destination_bytes_total": entry["minimum_bytes"],
                    "folders_total": entry["minimum_folders"],
                    "destination_folders_total": entry["minimum_folders"],
                    "maximum_message_bytes": 10485760,
                    "unicode_folders_observed": True,
                    "special_use_folders_observed": True,
                    "mismatch_planted": True,
                    "mismatch_detected": True,
                },
                "results": {
                    "overall_result": "pass_with_exceptions",
                    "messages_covered_by_aggregate_evidence": entry["minimum_messages"],
                    "verification_confidence": "aggregate_only",
                },
                "proof_digest": "0" * 64,
                "proof_verification": "canonical_digest_verified",
                "proof_file": "missing-proof.json",
                "run_id": "run",
                "selected_run_id": "run",
                "run_type": "live",
                "run_started_at": "2026-09-20T00:00:00Z",
                "run_finished_at": "2026-09-20T00:01:00Z",
                "fixture_id": "fixture",
                "scenario_ids": entry["required_scenarios"],
                "scenario_observations": {
                    "large_mailbox_10k": {"messages": 10000},
                    "large_messages": {"maximum_message_bytes": 10485760},
                    "unicode_folders": {"observed": True},
                    "special_use_folders": {"observed": True},
                    "mismatch_detection": {"planted": True, "detected": True},
                },
                "test_dataset_digest": "d" * 64,
                "qualification_bundle_id": "bundle-1",
                "mailswiftsync_version": "0.1.0-alpha.1",
                "mailswiftsync_commit": "abc123",
                "mailswiftsync_binary_sha256": "a" * 64,
                "imapsync_binary_sha256": "b" * 64,
            }
            (evidence_dir / "synthetic.json").write_text(
                json.dumps(evidence), encoding="utf-8"
            )
            result = subprocess.run(
                [str(GATE)],
                cwd=ROOT,
                env={**os.environ, "MAILSWIFTSYNC_EVIDENCE_DIR": str(evidence_dir)},
                text=True,
                capture_output=True,
                check=False,
            )
            output = result.stdout + result.stderr
            self.assertNotIn("NameError", output)
            self.assertNotIn("Traceback", output)
            self.assertIn("referenced customer proof is missing", output)


if __name__ == "__main__":
    unittest.main()
