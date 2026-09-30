#!/usr/bin/env python3
"""Guard the distinction between provider smoke runs and release evidence."""

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = (ROOT / "scripts" / "provider-integration-test.sh").read_text(encoding="utf-8")


class ProviderIntegrationHarnessTests(unittest.TestCase):
    def test_smoke_harness_does_not_emit_release_qualification_records(self):
        self.assertNotIn("generate-provider-evidence.py", SCRIPT)
        self.assertIn("NOT PROVIDER QUALIFICATION", SCRIPT)
        self.assertIn("mkdir -m 0700 \"$proof_bundle\"", SCRIPT)

    def test_scenario_claims_are_phase_specific_and_match_execution_order(self):
        self.assertIn('dry_scenario_ids="basic-small"', SCRIPT)
        self.assertIn('live_scenario_ids="basic-small"', SCRIPT)
        self.assertIn('recovery_scenario_ids="basic-small,forced-interruption"', SCRIPT)
        self.assertIn('--scenario-ids "$dry_scenario_ids"', SCRIPT)
        self.assertIn('--scenario-ids "$live_scenario_ids"', SCRIPT)
        self.assertIn('--scenario-ids "$recovery_scenario_ids"', SCRIPT)


if __name__ == "__main__":
    unittest.main()
