#!/usr/bin/env python3
"""Regression tests for Markdown-aware capability-claim detection."""

import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "verify_capability_claims", ROOT / "scripts" / "verify-capability-claims.py"
)
CHECKER = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(CHECKER)


class CapabilityClaimTests(unittest.TestCase):
    def test_markdown_and_html_formatting_cannot_hide_claims(self):
        fixtures = [
            "**Status:** ✅ PRODUCTION-READY",
            "*Status:* production ready",
            "## ✅ PRODUCTION-READY",
            "| Status | production-ready |",
            "> **Status:** production-ready",
            "<strong>Status:</strong> <em>production-ready</em>",
        ]
        for fixture in fixtures:
            with self.subTest(fixture=fixture):
                self.assertTrue(CHECKER.contains_affirmative_claim(fixture))

    def test_affirmative_wording_variants_are_detected(self):
        fixtures = [
            "MailSwiftSync is production-ready.",
            "The service is fully production ready.",
            "This build is ready for production.",
            "The release is GA-ready.",
            "IMAP transfer is production supported.",
        ]
        for fixture in fixtures:
            with self.subTest(fixture=fixture):
                self.assertTrue(CHECKER.contains_affirmative_claim(fixture))

    def test_gate_language_and_negative_claims_are_not_affirmative(self):
        fixtures = [
            "Required before calling it production-ready",
            "Production readiness remains outstanding.",
            "The feature is not production-ready.",
            "Production support is not yet available.",
        ]
        for fixture in fixtures:
            with self.subTest(fixture=fixture):
                self.assertFalse(CHECKER.contains_affirmative_claim(fixture))

    def test_manifest_marked_experimental_terms_cannot_be_in_stable_status(self):
        manifest = {"documentation": {"experimental_only_terms": ["native dovecot execution"]}}
        valid = "Stable today:\n- imapsync\n\nExperimental or planned:\n- Native Dovecot execution\n"
        invalid = "Stable today:\n- Native Dovecot execution\n\nExperimental or planned:\n- Native Dovecot execution\n"
        self.assertEqual(CHECKER.find_status_section_violations(valid, manifest), [])
        self.assertEqual(len(CHECKER.find_status_section_violations(invalid, manifest)), 1)


if __name__ == "__main__":
    unittest.main()
