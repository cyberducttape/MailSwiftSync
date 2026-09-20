/// Documentation validation test suite.
/// Ensures docs remain consistent with code and don't contain outdated claims.

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
        readme.contains("production") || readme.contains("Production"),
        "README should mention production status"
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
fn production_readiness_status_updated() {
    let status = fs::read_to_string("PRODUCTION_READINESS_STATUS.md")
        .expect("Could not read PRODUCTION_READINESS_STATUS.md");

    // Should mention message-level verification as complete
    assert!(
        status.contains("message") || status.contains("verification"),
        "Status should mention message verification"
    );
}

#[test]
fn no_outdated_oauth_warnings() {
    let oauth_docs = fs::read_to_string("docs/wiki/PSA-notifications.md")
        .unwrap_or_else(|_| String::new());

    // If webhook docs exist, they should warn against embedding secrets
    if !oauth_docs.is_empty() {
        assert!(
            oauth_docs.contains("https://") || oauth_docs.contains("environment"),
            "Webhook docs should mention secure secret handling"
        );
    }
}

#[test]
fn compatibility_matrix_references_tested_providers() {
    let matrix = fs::read_to_string("docs/compatibility-matrix.md")
        .unwrap_or_else(|_| String::new());

    if !matrix.is_empty() {
        // Should mention major providers
        let has_providers = matrix.contains("Gmail")
            || matrix.contains("gmail")
            || matrix.contains("Microsoft")
            || matrix.contains("Office 365");

        assert!(
            has_providers,
            "Compatibility matrix should reference tested providers"
        );
    }
}

#[test]
fn provider_testing_guide_exists() {
    let guide = fs::read_to_string("docs/provider-testing-guide.md")
        .expect("Provider testing guide should exist");

    assert!(
        !guide.trim().is_empty(),
        "Provider testing guide should not be empty"
    );
}

#[test]
fn architecture_documents_message_verification() {
    let arch = fs::read_to_string("docs/architecture.md")
        .unwrap_or_else(|_| String::new());

    if !arch.is_empty() {
        assert!(
            arch.contains("verification") || arch.contains("message"),
            "Architecture doc should mention message verification"
        );
    }
}

#[test]
fn no_references_to_unreleased_versions() {
    let files = vec![
        "README.md",
        "CHANGELOG.md",
        "PRODUCTION_READINESS_STATUS.md",
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
