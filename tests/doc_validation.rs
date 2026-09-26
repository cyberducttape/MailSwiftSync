/// Basic documentation smoke tests.
/// These verify required files and selected safety-critical wording only. They
/// do not prove live provider evidence, controller wiring, or semantic parity
/// between documentation and implementation.
use std::fs;
use std::path::Path;

#[test]
fn doc_files_exist() {
    let doc_dir = "docs";
    assert!(Path::new(doc_dir).is_dir(), "docs directory must exist");

    let expected_files = vec![
        "architecture.md",
        "history/message-level-verification-design.md",
        "history/scheduler-design.md",
        "provider-testing-guide.md",
    ];

    for file in expected_files {
        let path = Path::new(doc_dir).join(file);
        assert!(path.exists(), "Expected {} to exist", file);
    }
}

#[test]
fn readme_mentions_production_status() {
    let readme = fs::read_to_string("README.md").expect("Could not read README.md");
    assert!(
        readme.contains("docs/compatibility-matrix.md") && readme.contains("source repository"),
        "README should identify the active compatibility and source-only qualification documents"
    );
}

#[test]
fn production_readiness_surfaces_have_one_conservative_source() {
    assert!(
        !Path::new("docs/PRODUCTION_READINESS_REPORT_2026_09_25.md").exists(),
        "dated readiness snapshots must not remain in active documentation"
    );

    let status =
        fs::read_to_string("PRODUCTION_STATUS.md").expect("active production status should exist");
    assert!(
        status.contains("controlled technical-preview deployments"),
        "active status must retain the technical-preview boundary"
    );

    let release_readiness = fs::read_to_string("docs/release-readiness.md")
        .expect("release-readiness criteria should exist");
    assert!(
        release_readiness.contains("Required before calling it production-ready"),
        "release-readiness must retain explicit production gates"
    );

    let archived = fs::read_to_string("docs/history/PRODUCTION_READINESS_REPORT-2026-09-25.md")
        .expect("dated readiness report should remain available as history");
    assert!(
        archived.contains("Historical snapshot — not current status"),
        "archived readiness reports must be visibly labeled"
    );
    for stale_fact in ["502/502", "0 clippy warnings", "28MB optimized"] {
        assert!(
            !archived.contains(stale_fact),
            "archived readiness report must not hardcode volatile fact: {stale_fact}"
        );
    }
}

#[test]
fn security_md_exists_and_complete() {
    let security = fs::read_to_string("SECURITY.md").expect("Could not read SECURITY.md");

    assert!(
        security.contains("Reporting a vulnerability"),
        "SECURITY.md must explain how to report"
    );
    assert!(
        security.contains("Credential"),
        "SECURITY.md must discuss credentials"
    );
    assert!(
        security.contains("Transport"),
        "SECURITY.md must discuss transport security"
    );
}

#[test]
fn capability_manifest_is_current() {
    let capabilities = fs::read_to_string("capabilities.toml")
        .expect("machine-readable capability status should exist");
    let capabilities: toml::Value = toml::from_str(&capabilities)
        .expect("machine-readable capability status should be valid TOML");
    assert_eq!(capabilities["schema_version"].as_integer(), Some(1));
    assert_eq!(
        capabilities["capabilities"]["message_level_metadata_reconciliation"]["controller"]
            .as_str(),
        Some("wired")
    );
    assert_eq!(
        capabilities["capabilities"]["uidvalidity_delta_checkpoints"]["code"].as_str(),
        Some("planned")
    );

    let manifest = fs::read_to_string("CAPABILITY_MANIFEST.md")
        .expect("Could not read CAPABILITY_MANIFEST.md");

    assert!(
        manifest.contains("capabilities.toml"),
        "Capability manifest should identify its machine-readable status source"
    );
    assert!(
        manifest.contains("Message-level mismatch detection"),
        "Capability manifest should document message verification"
    );
    assert!(
        manifest.contains("Message extraction (imapsync) | yes | yes (TLS live path)")
            && manifest.contains("Message extraction (Dovecot) | yes | partial"),
        "Capability manifest should distinguish the wired imapsync path from native Dovecot scope"
    );
    assert!(
        manifest.contains("Native IMAP message transfer (Dovecot) | yes | yes | no"),
        "Capability manifest must not call the native Dovecot path integration-tested"
    );
    assert!(
        manifest.contains("Gmail-specific throttling presets")
            && manifest.contains("Microsoft 365-specific throttling presets")
            && manifest.contains("Fastmail-specific throttling presets")
            && manifest.matches("| no | no |").count() >= 3,
        "Capability manifest should not claim provider-specific throttle presets"
    );
}

#[test]
fn microsoft_oauth_docs_match_raw_refresh_token_model() {
    let guide = fs::read_to_string("OAUTH_SETUP.md").expect("OAuth setup guide should exist");

    assert!(guide.contains("MailSwiftSync currently consumes a raw OAuth refresh token"));
    assert!(guide.contains("MSAL token cache or broker session"));
    assert!(guide.contains("grant_type=authorization_code"));
    assert!(guide.contains("IMAP.AccessAsUser.All%20offline_access"));
    assert!(!guide.contains("result.get(\"refresh_token\")"));
    assert!(!guide.contains("\"offline_access\","));
}

#[test]
fn oauth_lifecycle_docs_link_current_provider_guidance_without_fixed_folklore() {
    let oauth = fs::read_to_string("OAUTH_SETUP.md").expect("OAuth setup guide should exist");
    let lifecycle = oauth
        .split("## OAuth Token Lifecycle")
        .nth(1)
        .expect("OAuth lifecycle section should exist");
    assert!(lifecycle.contains("developers.google.com/identity/protocols/oauth2#expiration"));
    assert!(lifecycle.contains("learn.microsoft.com/en-us/entra/identity-platform/refresh-tokens"));
    assert!(!lifecycle.contains("6 months of inactivity"));
    assert!(!lifecycle.contains("1-2 years"));
    assert!(lifecycle.contains("not a guarantee"));
}

#[test]
fn gmail_oauth_docs_match_raw_refresh_token_model() {
    let guide = fs::read_to_string("OAUTH_SETUP.md").expect("OAuth setup guide should exist");

    assert!(guide.contains("Installed application flow (recommended)"));
    assert!(guide.contains("scopes=['https://mail.google.com/']"));
    assert!(guide.contains("Application Default Credentials are not a supported"));
    assert!(!guide.contains("gcloud auth application-default login\n"));
}

#[test]
fn no_outdated_oauth_warnings() {
    let oauth_docs =
        fs::read_to_string("docs/wiki/PSA-notifications.md").unwrap_or_else(|_| String::new());

    // If webhook docs exist, require concrete secret-handling wording.
    if !oauth_docs.is_empty() {
        assert!(
            oauth_docs.contains("environment") && oauth_docs.contains("secret"),
            "Webhook docs should mention secure secret handling"
        );
    }
}

#[test]
fn compatibility_matrix_references_tested_providers() {
    let matrix =
        fs::read_to_string("docs/compatibility-matrix.md").unwrap_or_else(|_| String::new());

    assert!(
        !matrix.trim().is_empty(),
        "Compatibility matrix must not be empty"
    );
    for provider in [
        "| Gmail | Gmail |",
        "| Microsoft 365 | Microsoft 365 |",
        "| Fastmail | Fastmail |",
    ] {
        assert!(
            matrix.contains(provider),
            "Compatibility matrix is missing {provider}"
        );
    }
    assert!(
        matrix.contains("Aggregate verification is wired for all engines; encrypted imapsync runs additionally perform bounded metadata-level reconciliation"),
        "Compatibility matrix must distinguish aggregate evidence from metadata and content proof"
    );
}

#[test]
fn provider_testing_guide_exists() {
    let guide = fs::read_to_string("docs/provider-testing-guide.md")
        .expect("Provider testing guide should exist");

    assert!(guide.contains("OAuth/XOAUTH2 preferred"));
    assert!(guide.contains("do not restore the removed Basic Authentication"));
    assert!(
        guide.contains("this guide does not assert a")
            && guide.contains("fixed commands-per-second rate")
    );
}

#[test]
fn gmail_provider_setup_uses_valid_account_and_authentication_guidance() {
    let guide = fs::read_to_string("docs/provider-tests/GMAIL_SETUP.md")
        .expect("Gmail provider setup should exist");
    let normalized = guide.to_ascii_lowercase();

    assert!(!normalized.contains("gcloud identity users create"));
    assert!(guide.contains("Google Admin console"));
    assert!(guide.contains("Optional app-password test (eligible Google accounts)"));
    assert!(guide.contains("OAuth/XOAUTH2 for Google Workspace IMAP qualification"));
    assert!(!normalized.contains("workspace password"));
    assert!(!normalized.contains("gcloud identity users delete"));
}

#[test]
fn canonical_provider_facts_are_reflected_in_primary_surfaces() {
    let facts = fs::read_to_string("docs/provider-facts.md")
        .expect("canonical provider facts should exist");
    assert!(facts.contains("## Google Workspace") && facts.contains("## Personal Gmail"));
    assert!(facts.contains("Metadata reconciled — message bodies not compared"));
    assert!(facts.contains("no universal 50 GB or 100 GB threshold"));

    let provider_setup = fs::read_to_string("PROVIDER_SETUP.md")
        .unwrap()
        .replace("\r\n", "\n");
    let readme = fs::read_to_string("README.md").unwrap();
    assert!(provider_setup.contains("canonical\nprovider facts"));
    assert!(provider_setup.contains("OAuth 2.0 / XOAUTH2 is the preferred and default"));
    assert!(provider_setup.contains("no universal 50 GB or 100 GB threshold"));
    assert!(readme.contains("Aggregate match — not message-body proof"));
    assert!(readme.contains("Metadata reconciled — message bodies not compared"));
}

#[test]
fn architecture_documents_message_verification() {
    let arch = fs::read_to_string("docs/architecture.md").unwrap_or_else(|_| String::new());

    assert!(arch.contains("The ledger records project lifecycle, structured run events"));
    assert!(arch.contains("verbose engine transcripts remain bounded process-local diagnostics"));
}

#[test]
fn no_references_to_unreleased_versions() {
    let files = vec![
        "README.md",
        "CHANGELOG.md",
        "CAPABILITY_MANIFEST.md",
        "capabilities.toml",
    ];

    for file in files {
        let content = fs::read_to_string(file).unwrap_or_else(|_| String::new());

        // Check for reasonable version pattern; 0.1.x is current dev line
        if content.contains("version") {
            // Just ensure it doesn't claim things like "0.2" or "1.0" exist yet
            assert!(
                !content.contains("v0.2.0") && !content.contains("v1.0.0-release"),
                "{} should not claim unreleased versions are ready",
                file
            );
        }
    }
}
