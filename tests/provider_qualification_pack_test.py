import importlib.util
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

try:
    import jsonschema
except ImportError:  # Installed by the provider-evidence CI requirements.
    jsonschema = None


SCRIPT = Path(__file__).parents[1] / "scripts" / "build-provider-qualification-pack.py"
VERIFY_SCRIPT = (
    Path(__file__).parents[1] / "scripts" / "verify-provider-qualification-pack.py"
)
SPEC = importlib.util.spec_from_file_location("provider_qualification_pack", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)
VERIFY_SPEC = importlib.util.spec_from_file_location(
    "verify_provider_qualification_pack", VERIFY_SCRIPT
)
VERIFY_MODULE = importlib.util.module_from_spec(VERIFY_SPEC)
assert VERIFY_SPEC.loader is not None
VERIFY_SPEC.loader.exec_module(VERIFY_MODULE)


PHASES = {
    "dry_pilot": ["dry"],
    "live_pilot": ["live", "large-mailbox-1gb", "large-mailbox-10gb", "large-mailbox-20gb", "large-mailbox-50gb", "large-mailbox-100gb"],
    "recovery_test": ["recovery"],
}


def policy():
    result = {
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
    return result


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
    result = {
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
        "scenario_observations": {},
        "test_summary": summary,
        "results": {
            "overall_result": "pass",
            "verification_confidence": "not_applicable" if dry else "high",
            "messages_verified": 0 if dry else 2,
            "messages_covered_by_aggregate_evidence": 0,
            "known_issues": [{"issue": "private-mailbox@example.test"}],
        },
    }
    if not dry:
        for scenario in PHASES[phase]:
            if not scenario.startswith("large-mailbox-"):
                continue
            size = scenario.removeprefix("large-mailbox-")
            byte_count = {
                "1gb": 1024**3, "10gb": 10 * 1024**3, "20gb": 20 * 1024**3,
                "50gb": 50 * 1024**3, "100gb": 100 * 1024**3,
            }[size]
            metrics = {
                "wall_seconds": 1,
                "messages": 2,
                "bytes": byte_count,
                "peak_rss_bytes": 10,
                "cpu_seconds": 1,
                "disk_growth_bytes": byte_count,
                "retries": 0,
                "throttled_seconds": 0,
            }
            comparison = {
                "baseline_run_id": f"direct-{scenario}",
                "dataset_digest": "b" * 64,
                "engine_binary_sha256": "e" * 64,
                "engine_settings_sha256": "f" * 64,
                "source_snapshot_file": "pending-source.json",
                "source_snapshot_sha256": "0" * 64,
                "destination_baseline_file": "pending-destination.json",
                "destination_baseline_sha256": "0" * 64,
                "baseline_artifact_file": "pending-baseline.txt",
                "baseline_artifact_sha256": "0" * 64,
                "managed_artifact_file": "pending-managed.txt",
                "managed_artifact_sha256": "0" * 64,
                "baseline_metrics": metrics,
                "managed_metrics": dict(metrics),
            }
            result["scenario_observations"][scenario.replace("-", "_")] = {
                "mailbox_job_id": "mailbox-1",
                "bytes": byte_count,
                "direct_imapsync_comparison": comparison,
            }
    return result


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
            value = evidence(phase)
            for index, observation in enumerate(value["scenario_observations"].values()):
                comparison = observation.get("direct_imapsync_comparison")
                if not comparison:
                    continue
                for suffix, contents in (
                    ("source_snapshot", b"source snapshot"),
                    ("destination_baseline", b"destination baseline"),
                    ("baseline", b"direct baseline log"),
                    ("managed", b"managed run log"),
                ):
                    name = f"{phase}-{index}-{suffix}.txt"
                    (self.evidence_dir / name).write_bytes(contents)
                    file_key = f"{suffix}_file" if suffix in {"source_snapshot", "destination_baseline"} else f"{suffix}_artifact_file"
                    digest_key = f"{suffix}_sha256" if suffix in {"source_snapshot", "destination_baseline"} else f"{suffix}_artifact_sha256"
                    comparison[file_key] = name
                    comparison[digest_key] = hashlib.sha256(contents).hexdigest()
            (self.evidence_dir / f"{phase}.json").write_text(json.dumps(value), encoding="utf-8")

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
        self.assertEqual(pack["schema_version"], 2)
        self.assertEqual(pack["status"], "qualification_candidate")
        self.assertTrue(pack["release_gate_required"])
        serialized = json.dumps(pack)
        self.assertNotIn("private-mailbox@example.test", serialized)
        self.assertEqual(pack["phases"][0]["results"]["overall_result"], "pass")
        self.assertEqual(pack["tested"]["fixture_id"], "fixture-1")
        self.assertEqual(pack["tested"]["source_version"], "provider-source-1")
        self.assertEqual(
            pack["phases"][0]["destination_version"], "provider-destination-1"
        )
        digest = pack["evidence_digest"]
        digest_payload = {
            key: value for key, value in pack.items() if key != "evidence_digest"
        }
        self.assertEqual(digest, MODULE.sha256(digest_payload))
        digest_payload["tested"]["fixture_id"] = "tampered-fixture"
        self.assertNotEqual(digest, MODULE.sha256(digest_payload))
        if jsonschema is not None:
            schema_path = (
                SCRIPT.parent.parent
                / "tests"
                / "provider-evidence"
                / "pack-schema.json"
            )
            jsonschema.validate(pack, json.loads(schema_path.read_text(encoding="utf-8")))

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

    def test_unknown_or_unavailable_fixture_and_provider_versions_are_rejected(self):
        for field, value in (
            ("source_version", "unknown"),
            ("destination_version", "N/A"),
            ("fixture_id", "unspecified"),
        ):
            with self.subTest(field=field):
                _, changed = self.read_phase("live_pilot")
                changed[field] = value
                self.write_phase("live_pilot", changed)
                with self.assertRaisesRegex(ValueError, f"{field} must identify"):
                    self.build()
                self.write_phase("live_pilot", evidence("live_pilot"))

    def test_policy_counts_and_source_destination_totals_are_enforced(self):
        _, value = self.read_phase("live_pilot")
        value["test_summary"]["destination_messages_total"] = 1
        self.write_phase("live_pilot", value)
        with self.assertRaisesRegex(ValueError, "totals do not match"):
            self.build()

    def test_live_pack_requires_direct_engine_comparison_for_each_large_mailbox_case(self):
        _, value = self.read_phase("live_pilot")
        value["scenario_observations"]["large_mailbox_50gb"].pop(
            "direct_imapsync_comparison"
        )
        self.write_phase("live_pilot", value)
        with self.assertRaisesRegex(ValueError, "large-mailbox-50gb lacks"):
            self.build()

    def test_direct_baseline_must_match_qualified_engine_identity(self):
        _, value = self.read_phase("live_pilot")
        value["scenario_observations"]["large_mailbox_10gb"][
            "direct_imapsync_comparison"
        ]["engine_binary_sha256"] = "9" * 64
        self.write_phase("live_pilot", value)
        with self.assertRaisesRegex(ValueError, "does not use the qualified imapsync binary"):
            self.build()

    def test_direct_run_artifact_bytes_must_match_declared_digest(self):
        _, value = self.read_phase("live_pilot")
        comparison = value["scenario_observations"]["large_mailbox_1gb"][
            "direct_imapsync_comparison"
        ]
        (self.evidence_dir / comparison["baseline_artifact_file"]).write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "artifact digest mismatch"):
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
            if jsonschema is not None:
                completed = subprocess.run(
                    [sys.executable, str(VERIFY_SCRIPT), str(output)],
                    check=False,
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(completed.returncode, 0, completed.stderr)
                self.assertIn("not release-qualified", completed.stdout)
        finally:
            MODULE.sys.argv = original_argv

    def test_pack_verifier_rejects_tampering_and_duplicate_json_keys(self):
        pack = self.build()
        path = self.directory / "pack.json"
        path.write_text(json.dumps(pack), encoding="utf-8")
        changed = json.loads(path.read_text(encoding="utf-8"))
        changed["tested"]["fixture_id"] = "tampered"
        path.write_text(json.dumps(changed), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "digest does not match"):
            VERIFY_MODULE.verify_pack(path)
        path.write_text('{"status":"a","status":"b"}', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "duplicate JSON key"):
            VERIFY_MODULE.parse_pack(path)

    def test_pack_verifier_rejects_symlinks_and_oversized_inputs(self):
        target = self.directory / "target.json"
        target.write_text("{}", encoding="utf-8")
        link = self.directory / "link.json"
        link.symlink_to(target)
        with self.assertRaisesRegex(ValueError, "must not be a symlink"):
            VERIFY_MODULE.parse_pack(link)

        oversized = self.directory / "oversized.json"
        with oversized.open("wb") as handle:
            handle.truncate(VERIFY_MODULE.MAX_PACK_BYTES + 1)
        with self.assertRaisesRegex(ValueError, "validation limit"):
            VERIFY_MODULE.parse_pack(oversized)


if __name__ == "__main__":
    unittest.main()
