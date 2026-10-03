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

    def test_release_documents_are_generated_from_machine_readable_status(self):
        import tomllib

        with CHECKER.MANIFEST.open("rb") as stream:
            manifest = tomllib.load(stream)
        capability = CHECKER.CAPABILITY_MANIFEST_MD.read_text(encoding="utf-8")
        production = CHECKER.PRODUCTION_STATUS_MD.read_text(encoding="utf-8")
        self.assertEqual(
            CHECKER.replace_block(
                capability,
                CHECKER.PROVIDER_BEGIN,
                CHECKER.PROVIDER_END,
                CHECKER.render_provider_qualification_table(manifest),
            ),
            capability,
        )
        self.assertEqual(
            CHECKER.replace_block(
                production,
                CHECKER.RELEASE_BEGIN,
                CHECKER.RELEASE_END,
                CHECKER.render_release_metadata(
                    manifest, CHECKER.get_schema_version(), CHECKER.get_package_version()
                ),
            ),
            production,
        )
        self.assertEqual(
            CHECKER.replace_block(
                production,
                CHECKER.FEATURES_BEGIN,
                CHECKER.FEATURES_END,
                CHECKER.render_production_feature_table(manifest),
            ),
            production,
        )
        self.assertEqual(
            CHECKER.replace_block(
                production,
                CHECKER.PROVIDER_BEGIN,
                CHECKER.PROVIDER_END,
                CHECKER.render_provider_qualification_table(manifest),
            ),
            production,
        )


    @staticmethod
    def repository_manifest():
        import tomllib

        with CHECKER.MANIFEST.open("rb") as stream:
            return tomllib.load(stream)

    def test_undefined_evidence_tier_is_rejected(self):
        manifest = self.repository_manifest()
        # The exact stale manifest wording that the vocabulary check missed.
        stale = (
            "explicit encrypted-imapsync body-hash runs emit a distinct "
            "Level 4-style bounded body-proof outcome after complete coverage"
        )
        violations = CHECKER.find_level_prose_violations(stale, manifest)
        self.assertEqual(len(violations), 1)
        self.assertIn("Level 4", violations[0])

    def test_renamed_tier_and_wrong_tier_count_are_rejected(self):
        manifest = self.repository_manifest()
        self.assertEqual(
            CHECKER.find_level_prose_violations(
                "MailSwiftSync presents three verification levels. "
                "**Level 3 — Bounded content fingerprints** hashes bodies.",
                manifest,
            ),
            [],
        )
        renamed = CHECKER.find_level_prose_violations(
            "**Level 3 — Full body proof** compares every byte.", manifest
        )
        self.assertEqual(len(renamed), 1)
        self.assertIn("Bounded content fingerprints", renamed[0])
        miscounted = CHECKER.find_level_prose_violations("We present four evidence levels.", manifest)
        self.assertEqual(len(miscounted), 1)

    def test_rust_labels_must_use_canonical_name_and_limitation(self):
        manifest = self.repository_manifest()
        good = '"Level 2 — Per-message metadata reconciliation — bodies not compared"'
        self.assertEqual(CHECKER.find_rust_level_violations(good, manifest), [])
        weakened = '"Level 3 — Bounded content fingerprints — complete proof"'
        self.assertEqual(len(CHECKER.find_rust_level_violations(weakened, manifest)), 1)
        self.assertEqual(
            len(CHECKER.find_rust_level_violations('"Level 4 — Body proof"', manifest)), 1
        )

    def test_level_method_mapping_must_match_declared_methods(self):
        manifest = self.repository_manifest()
        source = CHECKER.EVIDENCE_RS.read_text(encoding="utf-8")
        self.assertEqual(CHECKER.find_level_method_violations(source, manifest), [])
        promoted = source.replace(
            "VerificationMethod::AggregateEngine | VerificationMethod::NativeDovecot => {\n"
            '                    "Level 1',
            "VerificationMethod::AggregateEngine => {\n"
            '                    "Level 1',
        ).replace(
            "VerificationMethod::BodyHash => {",
            "VerificationMethod::BodyHash | VerificationMethod::NativeDovecot => {",
        )
        self.assertNotEqual(promoted, source)
        self.assertEqual(len(CHECKER.find_level_method_violations(promoted, manifest)), 1)

    def test_repository_level_table_is_generated_from_capabilities_toml(self):
        manifest = self.repository_manifest()
        readme = CHECKER.README_MD.read_text(encoding="utf-8")
        self.assertEqual(
            CHECKER.replace_block(
                readme,
                CHECKER.LEVELS_BEGIN,
                CHECKER.LEVELS_END,
                CHECKER.render_level_table(manifest),
            ),
            readme,
        )
        for path in sorted((CHECKER.ROOT / "locales").glob("*.toml")):
            with self.subTest(locale=path.name):
                self.assertEqual(CHECKER.find_locale_level_violations(path, manifest), [])

    def test_locale_level_check_reads_the_message_catalog(self):
        import tempfile

        manifest = self.repository_manifest()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "en.toml"
            path.write_text(
                "[messages]\n"
                '"ui.verification-level-2" = "Level 2 — Message metadata reconciliation"\n'
                '"ui.verification-level-4" = "Level 4 — Full proof"\n'
                '"ui.verification-level-3" = "Level 2 — Bounded content fingerprints"\n',
                encoding="utf-8",
            )
            violations = CHECKER.find_locale_level_violations(path, manifest)
        self.assertTrue(any("ui.verification-level-4" in v for v in violations), violations)
        self.assertTrue(any("ui.verification-level-3" in v for v in violations), violations)
        self.assertFalse(any("ui.verification-level-2:" in v for v in violations), violations)


    def test_docs_cannot_deny_implemented_engine_install(self):
        claims = CHECKER.contradicting_claims(self.repository_manifest())
        # The stale INSTALL.md wording that contradicted install-engine.
        stale = (
            "6. Verify the engine in a terminal (`imapsync --version` or\n"
            "   `doveadm --version`). MailSwiftSync does not download, update, or configure\n"
            "   either engine for you.\n"
        )
        paragraphs = CHECKER.markdown_paragraphs(stale)
        self.assertEqual(len(paragraphs), 1)
        self.assertEqual(
            len(CHECKER.find_contradicted_capability_claims(paragraphs[0][1], claims)), 1
        )
        for accurate in (
            "It does not bundle imapsync or Dovecot; follow the engine steps below.",
            "MailSwiftSync never installs, updates, or configures Dovecot.",
            "`mailswiftsync install-engine` downloads imapsync 2.314.",
        ):
            with self.subTest(line=accurate):
                self.assertEqual(CHECKER.find_contradicted_capability_claims(accurate, claims), [])
        unimplemented = {"capabilities": {"x": {"code": "planned", "contradicted_by": ["anything"]}}}
        self.assertEqual(CHECKER.contradicting_claims(unimplemented), [])


if __name__ == "__main__":
    unittest.main()
