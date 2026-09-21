import json
import unittest
from pathlib import Path


SCHEMA_PATH = Path(__file__).parent / "provider-evidence" / "schema.json"


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


if __name__ == "__main__":
    unittest.main()
