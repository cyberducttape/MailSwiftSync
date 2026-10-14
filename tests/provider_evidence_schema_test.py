import json
import unittest
from pathlib import Path


SCHEMA_PATH = Path(__file__).parent / "provider-evidence" / "schema.json"
PACK_SCHEMA_PATH = Path(__file__).parent / "provider-evidence" / "pack-schema.json"
POLICY_PATH = Path(__file__).parent / "provider-evidence" / "policy.json"


def reject_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise AssertionError(f"duplicate schema key: {key}")
        result[key] = value
    return result


class ProviderEvidenceSchemaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.schema = json.loads(
            SCHEMA_PATH.read_text(encoding="utf-8"),
            object_pairs_hook=reject_duplicate_keys,
        )
        cls.pack_schema = json.loads(
            PACK_SCHEMA_PATH.read_text(encoding="utf-8"),
            object_pairs_hook=reject_duplicate_keys,
        )
        cls.policy = json.loads(
            POLICY_PATH.read_text(encoding="utf-8"),
            object_pairs_hook=reject_duplicate_keys,
        )

    def test_provider_policy_identities_are_accepted_by_the_evidence_schema(self):
        for field in ("source_provider", "destination_provider"):
            with self.subTest(field=field):
                allowed = set(self.schema["properties"][field]["enum"])
                for entry in self.policy["providers"]:
                    self.assertIn(entry[field], allowed, f"{entry['pair']} uses unsupported {field}")

    def test_schema_has_no_duplicate_keys_or_open_boundaries(self):
        self.assertFalse(self.schema.get("additionalProperties"))
        for name in ("test_summary", "results", "tester"):
            self.assertFalse(self.schema["properties"][name].get("additionalProperties"))
        self.assertFalse(
            self.schema["properties"]["results"]["properties"]["mismatches"].get(
                "additionalProperties"
            )
        )

    def test_required_provenance_and_integrity_fields_are_schema_fields(self):
        required = set(self.schema["required"])
        self.assertTrue(
            {
                "proof_digest",
                "proof_verification",
                "run_id",
                "selected_run_id",
                "run_type",
                "run_started_at",
                "run_finished_at",
                "fixture_id",
                "mailswiftsync_binary_sha256",
                "imapsync_binary_sha256",
            }.issubset(required)
        )

    def test_large_mailbox_observations_are_bound_to_a_specific_mailbox(self):
        observations = self.schema["properties"]["scenario_observations"]["properties"]
        for scenario, metric in (
            ("large_mailbox_100k", "messages"),
            ("large_mailbox_1gb", "bytes"),
            ("large_mailbox_10gb", "bytes"),
            ("large_mailbox_20gb", "bytes"),
            ("large_mailbox_50gb", "bytes"),
            ("large_mailbox_100gb", "bytes"),
        ):
            with self.subTest(scenario=scenario):
                self.assertIn("mailbox_job_id", observations[scenario]["required"])
                self.assertIn("mailbox_job_id", observations[scenario]["properties"])
                self.assertIn(metric, observations[scenario]["properties"])

    def test_large_mailbox_observations_can_carry_strict_direct_engine_comparisons(self):
        scenario_properties = self.schema["properties"]["scenario_observations"]["properties"]
        comparison = self.schema["$defs"]["direct_imapsync_comparison"]
        self.assertTrue(comparison["additionalProperties"] is False)
        self.assertIn("baseline_artifact_sha256", comparison["required"])
        self.assertIn("managed_artifact_sha256", comparison["required"])
        for key in ("large_mailbox_1gb", "large_mailbox_10gb", "large_mailbox_20gb", "large_mailbox_50gb", "large_mailbox_100gb"):
            with self.subTest(scenario=key):
                self.assertEqual(
                    scenario_properties[key]["properties"]["direct_imapsync_comparison"],
                    {"$ref": "#/$defs/direct_imapsync_comparison"},
                )

    def test_every_policy_size_scenario_has_a_schema_observation(self):
        observations = self.schema["properties"]["scenario_observations"]
        for entry in self.policy["providers"]:
            for scenario in entry["scenario_requirements"]:
                if not scenario.startswith("large-mailbox-"):
                    continue
                observation = scenario.replace("-", "_")
                with self.subTest(pair=entry["pair"], scenario=scenario):
                    self.assertIn(observation, observations["required"])
                    self.assertIn(observation, observations["properties"])

    def test_size_scenario_observations_are_accepted_by_json_schema(self):
        try:
            import jsonschema
        except ImportError:
            self.skipTest("jsonschema is installed by CI/release requirements")
        schema = self.schema["properties"]["scenario_observations"]
        instance = {
            "large_mailbox_100k": {"mailbox_job_id": "job", "messages": 100000},
            "large_mailbox_1gb": {"mailbox_job_id": "job", "bytes": 1024**3},
            "large_mailbox_10gb": {"mailbox_job_id": "job", "bytes": 10 * 1024**3},
            "large_mailbox_20gb": {"mailbox_job_id": "job", "bytes": 20 * 1024**3},
            "large_mailbox_50gb": {"mailbox_job_id": "job", "bytes": 50 * 1024**3},
            "large_mailbox_100gb": {"mailbox_job_id": "job", "bytes": 100 * 1024**3},
            "large_messages": {"maximum_message_bytes": 1},
            "unicode_folders": {"observed": False},
            "special_use_folders": {"observed": False},
            "gmail_labels": {"labels_observed": 0, "mapped": False},
            "duplicate_message_id": {"planted": False, "preserved": False},
            "source_changed_during_seed": {"source_change_injected": False, "caught_up": False},
            "destination_active_final_delta": {"destination_change_injected": False, "preserved": False},
            "throttling_recovery": {"throttling_observed": False, "recovered": False},
            "quota_folder_edge_cases": {"quota_limit_classified": False, "folder_limit_classified": False},
            "mismatch_detection": {"planted": False, "detected": False},
        }
        jsonschema.validate(instance, schema)

    def test_sha256_fields_reject_non_digest_shapes_when_validator_available(self):
        try:
            import jsonschema
        except ImportError:
            self.skipTest("jsonschema is installed by CI/release requirements")
        instance = {
            "mailswiftsync_binary_sha256": "not-a-digest",
            "imapsync_binary_sha256": "0" * 64,
        }
        with self.assertRaises(jsonschema.ValidationError):
            jsonschema.validate(instance, self.schema)

    def test_qualification_pack_schema_cannot_claim_release_qualification(self):
        properties = self.pack_schema["properties"]
        self.assertEqual(properties["schema_version"].get("const"), 2)
        self.assertEqual(properties["status"].get("const"), "qualification_candidate")
        self.assertEqual(properties["release_gate_required"].get("const"), True)
        phase_properties = properties["phases"]["items"]["properties"]
        self.assertFalse(phase_properties["test_summary"].get("additionalProperties", True))
        self.assertFalse(phase_properties["results"].get("additionalProperties", True))
        self.assertNotIn("known_issues", phase_properties["results"].get("properties", {}))
        tested_properties = properties["tested"]["properties"]
        self.assertTrue(
            {"fixture_id", "source_version", "destination_version"}.issubset(
                properties["tested"]["required"]
            )
        )
        self.assertTrue(
            {"fixture_id", "source_version", "destination_version"}.issubset(
                properties["phases"]["items"]["required"]
            )
        )
        self.assertIn("fixture_id", tested_properties)


if __name__ == "__main__":
    unittest.main()
