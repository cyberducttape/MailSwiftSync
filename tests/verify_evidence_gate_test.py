#!/usr/bin/env python3
"""Exercise the populated-evidence path of the shell release gate."""

import hashlib
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
    def test_release_mode_requires_pinned_provider_proof_signer(self):
        environment = os.environ.copy()
        environment.pop("MAILSWIFTSYNC_PROVIDER_EVIDENCE_PUBLIC_KEY", None)
        result = subprocess.run(
            [str(GATE), "--release"],
            cwd=ROOT,
            env=environment,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires MAILSWIFTSYNC_PROVIDER_EVIDENCE_PUBLIC_KEY", result.stderr)

    def test_release_policy_covers_customer_migration_directions_and_auth_modes(self):
        policy = json.loads((SOURCE_EVIDENCE / "policy.json").read_text(encoding="utf-8"))
        routes = {
            entry["pair"]: (entry["source_auth_method"], entry["destination_auth_method"])
            for entry in policy["providers"]
        }
        self.assertEqual(
            routes,
            {
                "gmail->microsoft365": ("oauth2", "oauth2"),
                "microsoft365->gmail": ("oauth2", "oauth2"),
                "cpanel_dovecot->microsoft365": ("password", "oauth2"),
                "cpanel_dovecot->gmail": ("password", "oauth2"),
                "cpanel_dovecot->cpanel_dovecot": ("password", "password"),
                "fastmail->gmail": ("password", "oauth2"),
                "fastmail->microsoft365": ("password", "oauth2"),
            },
        )
        qualification_plan = (ROOT / "docs" / "compatibility-matrix.md").read_text(
            encoding="utf-8"
        )
        for planned_route in [
            "Google Workspace → Microsoft 365",
            "Microsoft 365 → Google Workspace",
            "cPanel/Dovecot → Microsoft 365",
            "cPanel/Dovecot → Google Workspace",
            "cPanel/Dovecot → cPanel/Dovecot",
            "Fastmail → Google Workspace or Microsoft 365",
        ]:
            self.assertIn(
                f"| {planned_route} |",
                qualification_plan,
                f"qualification plan is missing policy route {planned_route}",
            )
        for entry in policy["providers"]:
            self.assertEqual(
                entry["scenario_requirements"]["large-mailbox-1gb"]["minimum_bytes"],
                1024**3,
            )
            self.assertEqual(
                entry["scenario_requirements"]["large-mailbox-10gb"]["minimum_bytes"],
                10 * 1024**3,
            )
            for scenario in ["large-mailbox-1gb", "large-mailbox-10gb"]:
                self.assertIn(scenario, entry["required_scenarios_by_phase"]["live_pilot"])
            self.assertEqual(
                entry["scenario_requirements"]["large-mailbox-50gb"]["minimum_bytes"],
                50 * 1024**3,
            )
            self.assertIn(
                "large-mailbox-50gb",
                entry["required_scenarios_by_phase"]["live_pilot"],
            )
            self.assertEqual(
                entry["scenario_requirements"]["large-mailbox-100gb"]["minimum_bytes"],
                100 * 1024**3,
            )
            self.assertIn(
                "large-mailbox-100gb",
                entry["required_scenarios_by_phase"]["live_pilot"],
            )
            self.assertEqual(
                set(entry["required_scenarios_by_phase"]),
                set(entry["required_phases"]),
            )
            self.assertEqual(
                entry["required_scenarios_by_phase"]["dry_pilot"],
                ["basic-small", "quota-folder-edge-cases"],
            )

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
                "source_auth_method": entry["source_auth_method"],
                "destination_auth_method": entry["destination_auth_method"],
                "tested_at": "2026-09-20T00:00:00Z",
                "testing_phase": "live_pilot",
                "source_version": "fixture",
                "destination_version": "fixture",
                "engine": entry["engine"],
                "engine_version": entry["engine_version"],
                "test_summary": {
                    "mailboxes_tested": 1,
                    "messages_total": 100000,
                    "bytes_total": 20 * 1024**3,
                    "destination_messages_total": 100000,
                    "destination_bytes_total": 20 * 1024**3,
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
                    "messages_covered_by_aggregate_evidence": 100000,
                    "messages_verified": 100000,
                    "verification_confidence": "high",
                },
                "proof_digest": "0" * 64,
                "proof_verification": "canonical_digest_verified",
                "proof_file": "proof.json",
                "run_id": "run",
                "selected_run_id": "run",
                "run_type": "live",
                "run_started_at": "2026-09-20T00:00:00Z",
                "run_finished_at": "2026-09-20T00:01:00Z",
                "fixture_id": "fixture",
                "scenario_ids": entry["required_scenarios_by_phase"]["live_pilot"],
                "scenario_observations": {
                    "large_mailbox_100k": {"mailbox_job_id": "job-a", "messages": 100000},
                    "large_mailbox_1gb": {"mailbox_job_id": "job-a", "bytes": 10 * 1024**3},
                    "large_mailbox_10gb": {"mailbox_job_id": "job-a", "bytes": 10 * 1024**3},
                    "large_mailbox_20gb": {"mailbox_job_id": "job-a", "bytes": 20 * 1024**3},
                    "large_mailbox_50gb": {"mailbox_job_id": "job-a", "bytes": 50 * 1024**3},
                    "large_mailbox_100gb": {"mailbox_job_id": "job-a", "bytes": 100 * 1024**3},
                    "large_messages": {"maximum_message_bytes": 10485760},
                    "unicode_folders": {"observed": True},
                    "special_use_folders": {"observed": True},
                    "gmail_labels": {"labels_observed": 3, "mapped": True},
                    "duplicate_message_id": {"planted": True, "preserved": True},
                    "source_changed_during_seed": {"source_change_injected": True, "caught_up": True},
                    "destination_active_final_delta": {"destination_change_injected": True, "preserved": True},
                    "throttling_recovery": {"throttling_observed": True, "recovered": True},
                    "quota_folder_edge_cases": {"quota_limit_classified": True, "folder_limit_classified": True},
                    "mismatch_detection": {"planted": True, "detected": True},
                },
                "test_dataset_digest": "d" * 64,
                "qualification_bundle_id": "bundle-1",
                "mailswiftsync_version": "0.1.0-alpha.1",
                "mailswiftsync_commit": "abc123",
                "mailswiftsync_binary_sha256": "a" * 64,
                "imapsync_binary_sha256": "b" * 64,
            }
            for key in (
                "large_mailbox_1gb", "large_mailbox_10gb", "large_mailbox_20gb",
                "large_mailbox_50gb", "large_mailbox_100gb",
            ):
                observed_bytes = evidence["scenario_observations"][key]["bytes"]
                metrics = {
                    "wall_seconds": 60,
                    "messages": 100000,
                    "bytes": observed_bytes,
                    "peak_rss_bytes": 1024**3,
                    "cpu_seconds": 45,
                    "disk_growth_bytes": observed_bytes,
                    "retries": 0,
                    "throttled_seconds": 0,
                }
                evidence["scenario_observations"][key]["direct_imapsync_comparison"] = {
                    "baseline_run_id": f"direct-{key}",
                    "dataset_digest": "d" * 64,
                    "engine_binary_sha256": "b" * 64,
                    "engine_settings_sha256": "c" * 64,
                    "source_snapshot_sha256": "e" * 64,
                    "destination_baseline_sha256": "f" * 64,
                    "baseline_artifact_file": f"{key}-direct.txt",
                    "baseline_artifact_sha256": hashlib.sha256(b"direct baseline").hexdigest(),
                    "managed_artifact_file": f"{key}-managed.txt",
                    "managed_artifact_sha256": hashlib.sha256(b"managed run").hexdigest(),
                    "baseline_metrics": metrics,
                    "managed_metrics": dict(metrics),
                }
                (evidence_dir / f"{key}-direct.txt").write_bytes(b"direct baseline")
                (evidence_dir / f"{key}-managed.txt").write_bytes(b"managed run")
            proof = {
                "format": "mailswiftsync-customer-proof",
                "project": {"project_id": "project", "dataset_digest": "d" * 64},
                "provider_identity": {
                    "source_provider": entry["source_provider"],
                    "destination_provider": entry["destination_provider"],
                    "source_auth_method": entry["source_auth_method"],
                    "destination_auth_method": entry["destination_auth_method"],
                    "fixture_id": "fixture",
                },
                "runs": [{
                    "run_id": "run", "project_id": "project", "job_id": None,
                    "status": "completed", "engine": "imapsync",
                    "engine_version": "2.314", "started_at": "2026-09-20T00:00:00Z",
                    "finished_at": "2026-09-20T00:01:00Z",
                }],
                "mailboxes": [
                    {"job_id": job_id, "state": "verified", "evidence": {
                        "source_messages": 50000, "destination_messages": 50000,
                        "source_bytes": 10 * 1024**3, "destination_bytes": 10 * 1024**3,
                    }}
                    for job_id in ("job-a", "job-b")
                ],
            }
            canonical_proof = json.dumps(
                proof, separators=(",", ":"), ensure_ascii=False, sort_keys=True
            )
            proof_digest = hashlib.sha256(canonical_proof.encode()).hexdigest()
            proof["proof_digest"] = proof_digest
            evidence["proof_digest"] = proof_digest
            (evidence_dir / "proof.json").write_text(json.dumps(proof), encoding="utf-8")
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
            self.assertNotIn("source_auth_method must be", output)
            self.assertNotIn("destination_auth_method must be", output)
            self.assertNotIn("A/B baseline_run_id must be a SHA-256 digest", output)
            self.assertIn(
                "large-mailbox-100k per-mailbox observation does not match verified customer-proof evidence",
                output,
            )
            self.assertIn(
                "large-mailbox-20gb per-mailbox observation does not match verified customer-proof evidence",
                output,
            )
            self.assertIn(
                "large-mailbox-50gb per-mailbox observation does not match verified customer-proof evidence",
                output,
            )
            self.assertIn(
                "large-mailbox-100gb per-mailbox observation does not match verified customer-proof evidence",
                output,
            )
            self.assertIn(
                "large-mailbox-100gb does not contain at least 100 GiB",
                output,
            )
            self.assertIn(
                "large-mailbox-50gb does not contain at least 50 GiB",
                output,
            )

            missing_baseline = json.loads(json.dumps(evidence))
            missing_baseline["scenario_observations"]["large_mailbox_10gb"].pop(
                "direct_imapsync_comparison"
            )
            (evidence_dir / "synthetic.json").write_text(
                json.dumps(missing_baseline), encoding="utf-8"
            )
            missing_result = subprocess.run(
                [str(GATE)],
                cwd=ROOT,
                env={**os.environ, "MAILSWIFTSYNC_EVIDENCE_DIR": str(evidence_dir)},
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertIn(
                "large-mailbox-10gb lacks a direct-imapsync A/B comparison",
                missing_result.stdout + missing_result.stderr,
            )

            # Dry preflight evidence describes a no-transfer phase. It must
            # not be rejected for lacking live transfer totals, while the
            # live phase keeps its stricter large-dataset requirements.
            dry = json.loads(json.dumps(evidence))
            dry["provider"] = entry["pair"]
            dry["testing_phase"] = "dry_pilot"
            dry["scenario_ids"] = entry["required_scenarios_by_phase"]["dry_pilot"]
            dry["run_type"] = "preflight"
            dry["test_summary"].update({
                "messages_total": 0,
                "bytes_total": 0,
                "destination_messages_total": 0,
                "destination_bytes_total": 0,
                "folders_total": 0,
                "destination_folders_total": 0,
            })
            dry["results"]["verification_confidence"] = "not_applicable"
            dry["results"]["overall_result"] = "pass"
            (evidence_dir / "dry-pilot.json").write_text(json.dumps(dry), encoding="utf-8")
            rerun = subprocess.run(
                [str(GATE)],
                cwd=ROOT,
                env={**os.environ, "MAILSWIFTSYNC_EVIDENCE_DIR": str(evidence_dir)},
                text=True,
                capture_output=True,
                check=False,
            )
            rerun_output = rerun.stdout + rerun.stderr
            self.assertNotIn("dry-pilot.json: dry_pilot must not report transfer totals", rerun_output)
            self.assertNotIn("dry-pilot.json: large-mailbox-100k", rerun_output)
            self.assertNotIn("dry-pilot.json: large-mailbox-1gb", rerun_output)
            self.assertNotIn("dry-pilot.json: large-mailbox-10gb", rerun_output)
            self.assertNotIn("dry-pilot.json: large-mailbox-20gb", rerun_output)
            self.assertNotIn("dry-pilot.json: large-mailbox-50gb", rerun_output)
            self.assertNotIn("dry-pilot.json: large-mailbox-100gb", rerun_output)

            # A basename check alone does not keep a qualification bundle
            # self-contained: a symlink can still resolve outside the bundle.
            # Release evidence must be reproducible from the submitted files.
            external_proof = Path(directory) / "external-proof.json"
            external_proof.write_text(json.dumps(proof), encoding="utf-8")
            (evidence_dir / "proof.json").unlink()
            try:
                (evidence_dir / "proof.json").symlink_to(external_proof)
            except OSError as error:
                self.skipTest(f"symlink test unavailable: {error}")
            symlinked = subprocess.run(
                [str(GATE)],
                cwd=ROOT,
                env={**os.environ, "MAILSWIFTSYNC_EVIDENCE_DIR": str(evidence_dir)},
                text=True,
                capture_output=True,
                check=False,
            )
            symlinked_output = symlinked.stdout + symlinked.stderr
            self.assertIn("proof_file must not be a symlink", symlinked_output)


if __name__ == "__main__":
    unittest.main()
