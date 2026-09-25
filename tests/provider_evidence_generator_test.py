import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "scripts" / "generate-provider-evidence.py"
SPEC = importlib.util.spec_from_file_location("provider_evidence", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


def proof(status="verified", messages=2, claim_status="durably_complete", run=None,
          source_provider=None, destination_provider=None):
    run = run or {
        "run_id": "run",
        "project_id": "project",
        "job_id": "job",
        "status": "completed",
        "phase_at_start": "live",
        "engine": "imapsync",
        "engine_version": "2.314",
        "started_at": "2026-09-20T00:00:00Z",
        "finished_at": "2026-09-20T00:01:00Z",
    }
    run.setdefault("engine", "imapsync")
    run.setdefault("engine_version", "2.314")
    project = {
        "project_id": "project",
        "name": "fixture",
        "phase": "Complete",
        "fixture_id": "fixture-1",
        "dataset_digest": "d" * 64,
        "scenario_observations": {
            "large_mailbox_10k": {"messages": 10000},
            "large_messages": {"maximum_message_bytes": 10 * 1024 * 1024},
            "unicode_folders": {"observed": True},
            "special_use_folders": {"observed": True},
            "mismatch_detection": {"planted": True, "detected": True},
        },
    }
    if source_provider is not None:
        project["source_provider"] = source_provider
    if destination_provider is not None:
        project["destination_provider"] = destination_provider
    identity_source = source_provider or "gmail"
    identity_destination = destination_provider or "microsoft365"
    value = {
        "format": "mailswiftsync-customer-proof",
        "format_version": 1,
        "application_version": "0.1.0",
        "artifact_role": "customer_evidence",
        "completion_claim": {"status": claim_status},
        "project": project,
        "provider_identity": {
            "source_provider": identity_source,
            "destination_provider": identity_destination,
            "source_auth_method": "password",
            "destination_auth_method": "oauth2",
            "fixture_id": "fixture-1",
            "scenario_ids": ["basic-small", "forced-interruption"],
        },
        "mailboxes": [{
            "job_id": "job",
            "source_mailbox": "source",
            "destination_mailbox": "destination",
            "state": status,
            "evidence": {
                "scope": "engine-confirmed",
                "evidence_level": "Engine-confirmed exact match — not message-body proof",
                "source_messages": messages,
                "destination_messages": messages,
                "source_bytes": 100,
                "destination_bytes": 100,
                "source_folders": 3,
                "destination_folders": 3,
                "unmatched_messages": 0,
                "failed_messages": 0,
            },
        }],
        "runs": [run],
    }
    value["proof_digest"] = MODULE.canonical_proof_digest(value)
    return value


class ProviderEvidenceGeneratorTests(unittest.TestCase):
    IDENTITY = {
        "engine_version": "2.314",
        "mailswiftsync_version": "0.1.0",
        "mailswiftsync_commit": "abc123",
        "mailswiftsync_binary_sha256": "a" * 64,
        "imapsync_binary_sha256": "b" * 64,
        "qualification_bundle_id": "bundle-1",
    }

    def generate(self, value, source="gmail", destination="microsoft365", phase="live_pilot",
                 run_id="run", **overrides):
        arguments = {**self.IDENTITY, **overrides}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "proof.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            return MODULE.generate_evidence(
                str(path), source, destination, phase, run_id=run_id,
                qualification_bundle_id=arguments.pop("qualification_bundle_id"), **arguments
            )

    def test_actual_customer_proof_schema_generates_strict_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "proof.json"
            path.write_text(json.dumps(proof()), encoding="utf-8")
            evidence = self.generate(proof())
        self.assertEqual(evidence["results"]["overall_result"], "pass_with_exceptions")
        self.assertEqual(evidence["test_summary"]["messages_total"], 2)
        self.assertEqual(evidence["results"]["messages_covered_by_aggregate_evidence"], 2)
        self.assertNotIn("messages_verified", evidence["results"])
        self.assertEqual(evidence["results"]["verification_confidence"], "aggregate_only")
        self.assertEqual(evidence["proof_verification"], "canonical_digest_verified")

    def test_failed_or_tampered_proof_cannot_generate_pass_evidence(self):
        for value in (proof(status="attention"), proof(messages=0)):
            with self.subTest(value=value):
                with tempfile.TemporaryDirectory() as directory:
                    path = Path(directory) / "proof.json"
                    path.write_text(json.dumps(value), encoding="utf-8")
                    with self.assertRaises(ValueError):
                        self.generate(value, destination="gmail")

    def test_missing_claim_and_corrupt_digest_are_rejected(self):
        value = proof()
        value.pop("completion_claim")
        with self.assertRaises(ValueError):
            self.generate(value)
        value = proof()
        value["proof_digest"] = "0" * 64
        with self.assertRaises(ValueError):
            self.generate(value)

    def test_wrong_run_id_and_unrecovered_interruption_are_rejected(self):
        with self.assertRaises(ValueError):
            self.generate(proof(), run_id="wrong")
        interrupted = proof(run={
            "run_id": "run", "project_id": "project", "job_id": "job",
            "status": "running", "phase_at_start": "live",
            "started_at": "2026-09-20T00:00:00Z", "finished_at": None,
        })
        with self.assertRaises(ValueError):
            self.generate(interrupted)

    def test_recovery_requires_a_completed_run_and_produces_recovery_evidence(self):
        recovery = proof(run={
            "run_id": "recovery-run", "project_id": "project", "job_id": "job",
            "status": "completed", "phase_at_start": "attention",
            "started_at": "2026-09-20T00:02:00Z", "finished_at": "2026-09-20T00:03:00Z",
        })
        recovery["runs"].insert(0, {
            "run_id": "abandoned-run", "project_id": "project", "job_id": "job",
            "status": "abandoned", "phase_at_start": "live",
            "started_at": "2026-09-20T00:00:00Z", "finished_at": "2026-09-20T00:00:10Z",
        })
        recovery["proof_digest"] = MODULE.canonical_proof_digest(recovery)
        evidence = self.generate(recovery, phase="recovery_test", run_id="recovery-run")
        self.assertEqual(evidence["run_type"], "recovery")
        self.assertEqual(evidence["selected_run_id"], "recovery-run")

    def test_live_qualification_allows_a_resolved_historical_failure(self):
        value = proof()
        value["runs"].insert(0, {
            "run_id": "failed-attempt", "project_id": "project", "job_id": "job",
            "status": "failed", "phase_at_start": "live",
            "started_at": "2026-09-19T23:59:00Z", "finished_at": "2026-09-19T23:59:10Z",
        })
        value["proof_digest"] = MODULE.canonical_proof_digest(value)
        evidence = self.generate(value)
        self.assertEqual(evidence["selected_run_id"], "run")

    def test_recovery_rejects_an_abandoned_run_from_another_job(self):
        value = proof()
        value["runs"].insert(0, {
            "run_id": "wrong-job", "project_id": "project", "job_id": "other-job",
            "status": "abandoned", "phase_at_start": "live",
            "started_at": "2026-09-19T23:59:00Z", "finished_at": "2026-09-19T23:59:10Z",
        })
        value["proof_digest"] = MODULE.canonical_proof_digest(value)
        with self.assertRaises(ValueError):
            self.generate(value, phase="recovery_test")

    def test_pair_engine_and_confidence_failures_are_rejected(self):
        with self.assertRaises(ValueError):
            self.generate(proof(source_provider="gmail", destination_provider="gmail"))
        with self.assertRaises(ValueError):
            self.generate(proof(), engine_version="2.315")
        mismatched_run = proof()
        mismatched_run["runs"][0]["engine"] = "doveadm"
        mismatched_run["runs"][0]["engine_version"] = "9.9.9"
        mismatched_run["proof_digest"] = MODULE.canonical_proof_digest(mismatched_run)
        with self.assertRaises(ValueError):
            self.generate(mismatched_run)
        low_confidence = proof()
        low_confidence["mailboxes"][0]["evidence"]["evidence_level"] = "Probable metadata match"
        low_confidence["proof_digest"] = MODULE.canonical_proof_digest(low_confidence)
        with self.assertRaises(ValueError):
            self.generate(low_confidence)

    def test_zero_message_and_attention_fixtures_are_rejected(self):
        for value in (proof(messages=0), proof(status="attention")):
            with self.assertRaises(ValueError):
                self.generate(value)

    def test_dry_pilot_is_phase_specific_and_does_not_claim_transfer(self):
        dry = proof(claim_status="incomplete", run={
            "run_id": "preflight-run", "project_id": "project", "job_id": "job",
            "status": "completed", "phase_at_start": "preflight",
            "started_at": "2026-09-20T00:00:00Z", "finished_at": "2026-09-20T00:01:00Z",
        })
        evidence = self.generate(dry, phase="dry_pilot", run_id="preflight-run")
        self.assertEqual(evidence["run_type"], "preflight")
        self.assertEqual(evidence["test_summary"]["messages_total"], 0)


if __name__ == "__main__":
    unittest.main()
