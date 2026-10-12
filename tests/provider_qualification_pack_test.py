import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "scripts" / "build-provider-qualification-pack.py"
SPEC = importlib.util.spec_from_file_location("provider_qualification_pack", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


PHASES = {
    "dry_pilot": ["dry"],
    "live_pilot": ["live"],
    "recovery_test": ["recovery"],
}


def policy():
    return {
        "providers": [
            {
                "pair": "source->destination",
                "engine": "imapsync",
                "engine_version": "2.314",
                "source_auth_method": "password",
                "destination_auth_method": "oauth2",
                "minimum_mailboxes": 1,
                "minimum_messages": 2,
                "minimum_folders": 3,
                "minimum_bytes": 100,
                "required_phases": list(PHASES),
                "required_scenarios_by_phase": PHASES,
            }
        ]
    }


def evidence(phase):
    dry = phase == "dry_pilot"
    summary = {
        "mailboxes_tested": 1,
        "messages_total": 0 if dry else 2,
        "bytes_total": 0 if dry else 100,
        "destination_messages_total": 0 if dry else 2,
        "destination_bytes_total": 0 if dry else 100,
        "folders_total": 0 if dry else 3,
        "destination_folders_total": 0 if dry else 3,
        "maximum_message_bytes": 0 if dry else 50,
        "unicode_folders_observed": True,
    }
    return {
        "source_provider": "source",
        "destination_provider": "destination",
        "testing_phase": phase,
        "tested_at": f"2026-10-12T00:0{list(PHASES).index(phase)}:00Z",
        "run_id": phase,
        "selected_run_id": phase,
        "proof_digest": "a" * 64,
        "proof_verification": "canonical_digest_verified",
        "test_dataset_digest": "b" * 64,
        "qualification_bundle_id": "qualification-1",
        "mailswiftsync_version": "0.1.0-alpha.1",
        "mailswiftsync_commit": "c" * 40,
        "mailswiftsync_binary_sha256": "d" * 64,
        "imapsync_binary_sha256": "e" * 64,
        "engine": "imapsync",
        "engine_version": "2.314",
        "source_auth_method": "password",
        "destination_auth_method": "oauth2",
        "source_version": "provider-source-1",
        "destination_version": "provider-destination-1",
        "fixture_id": "fixture-1",
        "scenario_ids": PHASES[phase],
        "test_summary": summary,
        "results": {
            "overall_result": "pass",
            "verification_confidence": "not_applicable" if dry else "high",
            "messages_verified": 0 if dry else 2,
            "messages_covered_by_aggregate_evidence": 0,
            "known_issues": [{"issue": "private-mailbox@example.test"}],
        },
    }


class ProviderQualificationPackTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary.name)
        self.policy_path = self.directory / "policy.json"
        self.policy_path.write_text(json.dumps(policy()), encoding="utf-8")
        self.evidence_dir = self.directory / "evidence"
        self.evidence_dir.mkdir()
        self.write_phases()

    def tearDown(self):
        self.temporary.cleanup()

    def write_phases(self):
        for phase in PHASES:
            (self.evidence_dir / f"{phase}.json").write_text(
                json.dumps(evidence(phase)), encoding="utf-8"
            )

    def read_phase(self, phase):
        path = self.evidence_dir / f"{phase}.json"
        return path, json.loads(path.read_text(encoding="utf-8"))

    def write_phase(self, phase, value):
        (self.evidence_dir / f"{phase}.json").write_text(
            json.dumps(value), encoding="utf-8"
        )

    def build(self):
        return MODULE.build_pack(
            self.evidence_dir, "source", "destination", self.policy_path
        )

    def test_pack_is_candidate_and_redacts_free_text_issue_details(self):
        pack = self.build()
        self.assertEqual(pack["status"], "qualification_candidate")
        self.assertTrue(pack["release_gate_required"])
        serialized = json.dumps(pack)
        self.assertNotIn("private-mailbox@example.test", serialized)
        self.assertEqual(pack["phases"][0]["results"]["overall_result"], "pass")

    def test_exceptions_and_aggregate_only_evidence_cannot_be_called_qualified(self):
        for result, confidence in (
            ("pass_with_exceptions", "high"),
            ("pass", "aggregate_only"),
        ):
            with self.subTest(result=result, confidence=confidence):
                path, value = self.read_phase("live_pilot")
                value["results"]["overall_result"] = result
                value["results"]["verification_confidence"] = confidence
                self.write_phase("live_pilot", value)
                with self.assertRaises(ValueError):
                    self.build()
                path.write_text(json.dumps(evidence("live_pilot")), encoding="utf-8")

    def test_phases_must_share_dataset_fixture_and_versions(self):
        for field, value in (
            ("test_dataset_digest", "f" * 64),
            ("fixture_id", "different-fixture"),
            ("source_version", "different-version"),
        ):
            with self.subTest(field=field):
                _, changed = self.read_phase("recovery_test")
                changed[field] = value
                self.write_phase("recovery_test", changed)
                with self.assertRaisesRegex(ValueError, "phase identity mismatch"):
                    self.build()
                self.write_phase("recovery_test", evidence("recovery_test"))

    def test_policy_counts_and_source_destination_totals_are_enforced(self):
        _, value = self.read_phase("live_pilot")
        value["test_summary"]["destination_messages_total"] = 1
        self.write_phase("live_pilot", value)
        with self.assertRaisesRegex(ValueError, "totals do not match"):
            self.build()

    def test_evidence_symlinks_are_rejected(self):
        source = self.evidence_dir / "live_pilot.json"
        target = self.evidence_dir / "elsewhere.json"
        target.write_text(source.read_text(encoding="utf-8"), encoding="utf-8")
        source.unlink()
        source.symlink_to(target)
        with self.assertRaisesRegex(ValueError, "must not be symlinks"):
            self.build()

    def test_cli_output_is_private_and_never_overwritten(self):
        output = self.directory / "candidate.json"
        original_argv = MODULE.sys.argv
        try:
            MODULE.sys.argv = [
                str(SCRIPT), str(self.evidence_dir), "source", "destination",
                str(output), "--policy", str(self.policy_path),
            ]
            self.assertEqual(MODULE.main(), 0)
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)
            first_contents = output.read_bytes()
            self.assertEqual(MODULE.main(), 1)
            self.assertEqual(output.read_bytes(), first_contents)
        finally:
            MODULE.sys.argv = original_argv


if __name__ == "__main__":
    unittest.main()
