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

    def test_manifest_marked_experimental_terms_cannot_be_in_stable_components(self):
        manifest = {"documentation": {"experimental_only_terms": ["native dovecot execution"]}}
        heading = "Stable components (implementation status; not a production-support claim):"
        valid = f"{heading}\n- imapsync\n\nExperimental or planned:\n- Native Dovecot execution\n"
        invalid = f"{heading}\n- Native Dovecot execution\n\nExperimental or planned:\n- Native Dovecot execution\n"
        self.assertEqual(CHECKER.find_status_section_violations(valid, manifest), [])
        self.assertEqual(len(CHECKER.find_status_section_violations(invalid, manifest)), 1)

    MANIFEST = {
        "capabilities": {
            "provider_oauth_authorization": {
                "code": "implemented",
                "controller": "wired",
                "ui": "wired",
                "gmail_live": False,
                "m365_live": False,
                "production_supported": False,
                "manifest_rows": ["OAuth consent (authorization code + PKCE)"],
            },
            "provider_oauth_refresh": {
                "code": "implemented",
                "controller": "wired",
                "ui": "partial",
                "gmail_live": False,
                "m365_live": False,
                "production_supported": False,
                "manifest_rows": ["Gmail authentication (OAuth)"],
            },
        }
    }

    @staticmethod
    def manifest_doc(oauth_wired="yes", refresh_wired="partial", live="no", table=None):
        table = CHECKER.render_status_table(CapabilityClaimTests.MANIFEST) if table is None else table
        return (
            f"{CHECKER.TABLE_BEGIN}\n{table}\n{CHECKER.TABLE_END}\n\n"
            "| Capability | Code | Wired | Tested | Live Provider | Notes |\n"
            "|---|---|---|---|---|---|\n"
            f"| **OAuth consent (authorization code + PKCE)** | yes | {oauth_wired} | unit | {live} | x |\n"
            f"| **Gmail authentication (OAuth)** | yes | {refresh_wired} | unit | no | x |\n"
        )

    def test_consistent_manifest_rows_pass(self):
        self.assertEqual(CHECKER.find_manifest_drift(self.manifest_doc(), self.MANIFEST), [])

    def test_gui_wired_capability_cannot_be_documented_as_cli_only(self):
        violations = CHECKER.find_manifest_drift(
            self.manifest_doc(oauth_wired="CLI only"), self.MANIFEST
        )
        self.assertEqual(len(violations), 1)
        self.assertIn("OAuth consent", violations[0])

    def test_partially_wired_capability_cannot_be_documented_as_unwired(self):
        violations = CHECKER.find_manifest_drift(
            self.manifest_doc(refresh_wired="no"), self.MANIFEST
        )
        self.assertEqual(len(violations), 1)

    def test_live_provider_claim_requires_recorded_live_validation(self):
        violations = CHECKER.find_manifest_drift(self.manifest_doc(live="yes"), self.MANIFEST)
        self.assertEqual(len(violations), 1)
        self.assertIn("Live Provider", violations[0])

    def test_stale_generated_table_and_missing_rows_are_reported(self):
        stale = self.manifest_doc(table="| stale |")
        self.assertTrue(any("stale" in v for v in CHECKER.find_manifest_drift(stale, self.MANIFEST)))
        missing = self.manifest_doc().replace("Gmail authentication (OAuth)", "Gmail auth")
        self.assertTrue(
            any("not found" in v for v in CHECKER.find_manifest_drift(missing, self.MANIFEST))
        )

    def test_repository_manifest_matches_capabilities_toml(self):
        import tomllib

        with CHECKER.MANIFEST.open("rb") as stream:
            manifest = tomllib.load(stream)
        content = CHECKER.CAPABILITY_MANIFEST_MD.read_text(encoding="utf-8")
        self.assertEqual(CHECKER.find_manifest_drift(content, manifest), [])


if __name__ == "__main__":
    unittest.main()
