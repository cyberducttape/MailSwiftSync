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
        "message-level-verification-design.md",
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
        readme.contains("docs/compatibility-matrix.md")
            && readme.contains("docs/provider-testing-guide.md"),
        "README should link the active compatibility and provider-testing documents"
    );
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
    let manifest = fs::read_to_string("CAPABILITY_MANIFEST.md")
        .expect("Could not read CAPABILITY_MANIFEST.md");

    assert!(
        manifest.contains("Manually maintained from source call-site review"),
        "Capability manifest should identify its maintenance basis"
    );
    assert!(
        manifest.contains("Message-level mismatch detection"),
        "Capability manifest should document message verification"
    );
    assert!(
        manifest.contains("Message extraction (imapsync) | yes | no")
            && manifest.contains("Message extraction (Dovecot) | yes | no"),
        "Capability manifest should not claim unwired extractors are operational"
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
        "Generic IMAP (Gmail/Workspace)",
        "Generic IMAP (Microsoft 365)",
        "Generic IMAP (Fastmail)",
    ] {
        assert!(
            matrix.contains(provider),
            "Compatibility matrix is missing {provider}"
        );
    }
    assert!(
        matrix.contains(
            "Aggregate verification is wired; message-level reconciliation is a prototype"
        ),
        "Compatibility matrix must distinguish aggregate evidence from message-level proof"
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
fn architecture_documents_message_verification() {
    let arch = fs::read_to_string("docs/architecture.md").unwrap_or_else(|_| String::new());

    assert!(arch.contains("The ledger records project lifecycle, structured run events"));
    assert!(arch.contains("verbose engine transcripts remain bounded process-local diagnostics"));
}

#[test]
fn no_references_to_unreleased_versions() {
    let files = vec!["README.md", "CHANGELOG.md", "CAPABILITY_MANIFEST.md"];

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
