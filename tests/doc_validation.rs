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
        "verification-envelope.md",
        "history/message-level-verification-design.md",
        "history/scheduler-design.md",
        "msp-operations-runbook.md",
        "provider-testing-guide.md",
    ];

    for file in expected_files {
        let path = Path::new(doc_dir).join(file);
        assert!(path.exists(), "Expected {} to exist", file);
    }
}

#[test]
fn systemd_service_example_uses_a_restricted_worker_sandbox() {
    let service = fs::read_to_string("docs/distribution/mailswiftsync-supervise.service")
        .expect("canonical systemd service example should exist");
    let guide = fs::read_to_string("docs/distribution/SERVICE.md")
        .expect("service deployment guide should exist");
    assert!(guide.contains("mailswiftsync-supervise.service"));
    assert!(guide.contains("/usr/share/doc/mailswiftsync/mailswiftsync-supervise.service"));
    assert!(guide.contains("it is not a D-Bus session bus"));
    assert!(guide.contains("DBUS_SESSION_BUS_ADDRESS"));
    assert!(guide.contains("Do not copy a desktop operator's bus address"));
    for directive in [
        "User=mailswiftsync",
        "NoNewPrivileges=true",
        "CapabilityBoundingSet=",
        "AmbientCapabilities=",
        "PrivateDevices=true",
        "ProtectSystem=strict",
        "ProtectHome=true",
        "ProtectKernelTunables=true",
        "ProtectKernelModules=true",
        "ProtectControlGroups=true",
        "ProtectClock=true",
        "ProtectKernelLogs=true",
        "ProtectHostname=true",
        "RestrictNamespaces=true",
        "SystemCallArchitectures=native",
        "RestrictSUIDSGID=true",
        "RestrictRealtime=true",
        "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
        "LockPersonality=true",
        "ReadWritePaths=/var/lib/mailswiftsync",
    ] {
        assert!(
            service.contains(directive),
            "systemd service example is missing sandbox directive {directive:?}"
        );
    }
}

#[test]
fn live_provider_smoke_does_not_claim_mid_transfer_oauth_refresh_qualification() {
    let workflow = fs::read_to_string(".github/workflows/provider-smoke.yml")
        .expect("live provider smoke workflow should exist");
    let guide = fs::read_to_string("docs/provider-testing-guide.md")
        .expect("provider testing guide should exist");
    let recovery = fs::read_to_string("docs/crash-recovery-validation.md")
        .expect("crash recovery matrix should exist");

    assert!(workflow.contains("does NOT prove mid-transfer token renewal"));
    assert!(workflow.contains("does not configure imapsync's per-reconnect refresh"));
    assert!(guide.contains("scripted IMAP peer to force a socket drop"));
    assert!(guide.contains("invokes both configured refresh callbacks"));
    assert!(guide.contains("does not qualify real-provider"));
    assert!(guide.contains("Those remain open until a short-lived-token"));
    assert!(recovery.contains("does not exercise provider token expiry during transfer"));
    assert!(recovery.contains("requires a short-lived-token expiry test"));
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
fn readme_front_matter_identifies_technical_preview_without_implying_ga() {
    let readme = fs::read_to_string("README.md").expect("Could not read README.md");
    let preview = readme
        .find("Current status: Technical Preview for experienced mail administrators")
        .expect("README front matter must label the product as a technical preview");
    let component_status = readme
        .find("Stable components (implementation status; not a production-support claim)")
        .expect("README must distinguish stable implementation from product support");
    assert!(
        preview < component_status,
        "status must precede the feature list"
    );
    assert!(
        readme[..component_status]
            .contains("not a generally supported production migration service"),
        "README front matter must state the production-support boundary explicitly"
    );
}

#[test]
fn packaging_claims_match_the_release_workflow() {
    let readme = fs::read_to_string("README.md").expect("Could not read README.md");
    let readiness = fs::read_to_string("docs/release-readiness.md")
        .expect("Could not read release-readiness.md");
    let workflow = fs::read_to_string(".github/workflows/release.yml")
        .expect("Could not read release workflow");

    assert!(
        readme.contains("signed-checksum Linux Debian and RPM packages"),
        "README must describe both Linux package formats produced by release CI"
    );
    assert!(
        readiness.contains("deterministic signed Debian and RPM packages"),
        "release-readiness must describe both Linux package formats"
    );
    assert!(
        workflow.contains("scripts/package-deb.sh") && workflow.contains("scripts/package-rpm.sh"),
        "release CI must build both package formats named by the documentation"
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
    assert!(
        !status.contains("dormant prototype")
            && status.contains(
                "observed-signal classification wired; live provider qualification pending"
            )
            && status.contains("per-message transfer checkpoints remain outstanding"),
        "active status must distinguish wired provider intelligence from outstanding live qualification and checkpoint work"
    );

    let release_readiness = fs::read_to_string("docs/release-readiness.md")
        .expect("release-readiness criteria should exist");
    assert!(
        !release_readiness.contains("does not claim durable retry/dead-letter delivery")
            && release_readiness.contains("persists a credential-free outbox record")
            && release_readiness.contains("dead-letter state after repeated failure"),
        "release-readiness must not retain the pre-outbox webhook limitation"
    );

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
fn crash_recovery_docs_match_the_restored_ledger_integration_drill() {
    let harness = fs::read_to_string("scripts/imap-integration-smoke.sh")
        .expect("IMAP integration harness should exist");
    let recovery = fs::read_to_string("docs/crash-recovery-validation.md")
        .expect("crash-recovery validation matrix should exist");

    assert!(
        harness.contains("run_interruption_drill defaults false strict restore")
            && harness.contains("run_product backup \"$drill_state\" \"$drill_backup\"")
            && harness.contains("run_product restore \"$drill_backup\" \"$drill_state\""),
        "the default real-engine interruption scenario must back up and restore the ledger before resuming"
    );
    assert!(
        recovery
            .contains("Proven in CI (integration workflow) for the default header-matching plan")
            && recovery.contains(
                "does not establish per-message checkpoints or host power-loss durability"
            )
            && !recovery
                .contains("no lab resumes a partially transferred mailbox from a restored ledger"),
        "recovery documentation must reflect the tested scope and retain its limitations"
    );
}

#[test]
fn integration_harness_contains_a_throttled_mid_append_interruption_case() {
    let harness = fs::read_to_string("scripts/imap-integration-smoke.sh")
        .expect("IMAP integration harness should exist");
    let recovery = fs::read_to_string("docs/crash-recovery-validation.md")
        .expect("crash-recovery validation matrix should exist");

    assert!(
        harness
            .contains("run_interruption_drill large-append false strict \"\" 1 67108864 1048576")
            && harness
                .contains("The configured byte rate is an average run-level target, not in-flight")
            && harness.contains("partial APPEND was observed"),
        "the integration drill must wait for an incomplete large-message APPEND before killing the controller"
    );
    assert!(
        recovery.contains("CI at `e86971b1b3e2838fd0fb39fbbfa800a9a823818e` killed the controller after observing 3,145,263 of 67,109,132 bytes")
            && recovery.contains("resumed to exactly one independently verified message")
            && recovery.contains("reproduced failure at `6f714fb`")
            && recovery.contains("Proven in CI for this packaged fixture"),
        "the crash-recovery matrix must record both the reproduced failure and its fixture-scoped post-fix qualification"
    );
}

#[test]
fn documented_scale_workflow_timeout_matches_release_readiness() {
    let workflow = fs::read_to_string(".github/workflows/scale-qualification.yml")
        .expect("scale qualification workflow should exist");
    let readiness = fs::read_to_string("docs/release-readiness.md")
        .expect("release-readiness criteria should exist");
    let timeout = workflow
        .lines()
        .find_map(|line| line.trim().strip_prefix("timeout-minutes:"))
        .expect("scale workflow should declare a job timeout")
        .trim();
    assert!(
        readiness.contains(&format!("within a {timeout}-minute workflow limit")),
        "release-readiness must state the scale workflow's configured timeout ({timeout} minutes)"
    );
}

#[test]
fn current_scale_observation_keeps_qualification_limits_explicit() {
    let adoption = fs::read_to_string("docs/adoption-qualification-plan.md")
        .expect("adoption qualification evidence should exist");
    let status =
        fs::read_to_string("PRODUCTION_STATUS.md").expect("active production status should exist");
    for evidence in [
        "38215860881",
        "55242c879118610b7d54043743c549e8197201ef",
        "11733295124",
        "c7ecfa4c0497e41a0dac3bc84ceb412b5d0e1bafdc332c3f493e73192dcff2ff",
        "Level 2 metadata only, bodies not compared",
        "This is unsigned CI evidence from a generic disposable fixture",
        "no direct-imapsync A/B baseline",
        "38218024314",
        "4a75c0682a7aa3907d3a7a597c7923a1dde510a9",
        "11732703115",
        "9f3e736685cdd0d98dcfbdd9623afb2203870133cf1f6b11c485dc5f4355a2ab",
        "10,000 generated messages with 1,073,741,824 bytes aggregate generated body payload",
        "not proof that message bodies were hashed",
    ] {
        assert!(
            adoption.contains(evidence),
            "scale qualification evidence is missing {evidence:?}"
        );
    }
    assert!(
        status.contains("latest packaged-engine scale runs on 2026-10-12")
            && status.contains("not body-integrity proofs, a direct-imapsync A/B"),
        "production status must report the latest run without overstating its qualification"
    );
}

#[test]
fn verification_disk_sizing_uses_measured_ranges_not_a_fixed_multiplier() {
    let readiness = fs::read_to_string("docs/release-readiness.md")
        .expect("release-readiness scale evidence should exist");
    let envelope = fs::read_to_string("docs/verification-envelope.md")
        .expect("verification envelope measurements should exist");
    for evidence in [
        "727–732 MB after",
        "1,095–1,154 MB",
        "+51–58%",
        "no safe general",
        "16% of the post-staging size",
    ] {
        assert!(
            readiness.contains(evidence),
            "release-readiness disk estimate is missing {evidence:?}"
        );
    }
    for evidence in [
        "730,079,232 B after staging",
        "peaked at 1,151,854,544 B",
        "727,031,808 B",
        "1,094,510,640 B",
    ] {
        assert!(
            envelope.contains(evidence),
            "verification benchmark evidence is missing {evidence:?}"
        );
    }
    assert!(
        !readiness.contains("475 bytes per message per side, plus about 50%"),
        "measured workload-specific data must not be summarized as a fixed multiplier"
    );
}

#[test]
fn verification_envelope_records_current_head_adversarial_scale_observations() {
    let envelope = fs::read_to_string("docs/verification-envelope.md")
        .expect("verification envelope measurements should exist");
    for evidence in [
        "0a4a5ea50bce9001d29961f046575150fd2fb175",
        "targeted 100,000-message-per-endpoint repeat",
        "47,281/s",
        "31,046/s",
        "40,016/s",
        "111,049,240 B / 9,722,392 B",
        "105,626,136 B / 9,722,392 B",
        "111,356,464 B / 9,779,760 B",
        "do not establish a universal per-message disk multiplier",
    ] {
        assert!(
            envelope.contains(evidence),
            "current-head verification observation is missing {evidence:?}"
        );
    }
}

#[test]
fn verification_envelope_discloses_typed_mapping_equals_limit() {
    let envelope = fs::read_to_string("docs/verification-envelope.md")
        .expect("verification envelope should exist");
    let validation = fs::read_to_string("src/migration_plan/validation.rs")
        .expect("typed mapping validation should exist");
    assert!(
        envelope.contains("A source folder name containing `=` cannot\ncurrently be represented")
            && envelope.contains(
                "destination names may contain `=`, and typed exclusions can include `=`"
            ),
        "verification envelope must distinguish source mapping limits from destination and exclusion support"
    );
    assert!(
        validation.contains("if !rule.exclude && source.contains('=')")
            && validation.contains("Source folder names cannot contain '='"),
        "the documented compatibility limit must remain enforced by plan validation"
    );
}

#[test]
fn verification_envelope_records_current_head_maximum_stress_observations() {
    let envelope = fs::read_to_string("docs/verification-envelope.md")
        .expect("verification envelope measurements should exist");
    for evidence in [
        "930439da071f2d2dcf3d38a83ae727ac5175f8d6",
        "maximum-envelope stress repeat",
        "31,360/s",
        "40,247/s",
        "1,094,514,640 B / 100,894,672 B",
        "1,153,976,072 B / 101,443,336 B",
        "801,488,896 B during reconciliation",
        "713,072,640 B during reconciliation",
        "do not establish a safe disk ceiling or GA",
    ] {
        assert!(
            envelope.contains(evidence),
            "maximum-envelope verification observation is missing {evidence:?}"
        );
    }
}

#[test]
fn production_status_describes_runtime_eta_boundary() {
    let status =
        fs::read_to_string("PRODUCTION_STATUS.md").expect("active production status should exist");
    let manifest =
        fs::read_to_string("CAPABILITY_MANIFEST.md").expect("capability manifest should exist");
    assert!(
        status.contains("Time-to-completion estimates | ⚠️ Estimate for imapsync only")
            && status.contains("Activity exposes an explicitly labelled imapsync")
            && status.contains("native Dovecot runs remain progress- and ETA-free"),
        "production status must describe the conservative runtime ETA boundary"
    );
    assert!(
        manifest.contains("**Live throughput, progress, and ETA** | yes | yes"),
        "the capability manifest must agree that runtime telemetry is wired"
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
fn release_and_pull_request_ci_enforce_the_dependency_policy() {
    for workflow in [
        ".github/workflows/ci.yml",
        ".github/workflows/release.yml",
        ".github/workflows/dependency-audit.yml",
    ] {
        let contents = fs::read_to_string(workflow).expect("workflow should be readable");
        assert!(
            contents.contains("cargo install cargo-deny --version 0.20.2 --locked"),
            "{workflow} must install the pinned cargo-deny version"
        );
        assert!(
            contents.contains("cargo deny check advisories bans licenses sources"),
            "{workflow} must enforce all committed cargo-deny policy categories"
        );
    }

    let ci = fs::read_to_string(".github/workflows/ci.yml").expect("CI workflow should exist");
    assert!(
        ci.contains("pull_request:"),
        "dependency policy must run in pull-request CI"
    );
    assert!(
        ci.contains("shellcheck scripts/*.sh"),
        "pull-request CI must enforce shell-script linting"
    );
    let release =
        fs::read_to_string(".github/workflows/release.yml").expect("release workflow should exist");
    assert!(
        release.contains("shellcheck scripts/*.sh"),
        "tagged releases must enforce shell-script linting"
    );
}

#[test]
fn native_runtime_failures_retain_logs_for_every_platform() {
    let ci = fs::read_to_string(".github/workflows/ci.yml").expect("CI workflow should exist");
    let upload_step = ci
        .split("- name: Upload native test log")
        .nth(1)
        .expect("native-test log upload step should exist")
        .split("\n      - name:")
        .next()
        .expect("native-test log upload step should be delimited");
    assert!(
        upload_step.contains("if: failure()")
            && upload_step.contains("uses: actions/upload-artifact@"),
        "failed native-test jobs must upload diagnostics on every matrix platform"
    );
    assert!(
        ci.contains("name: native-test-log-${{ matrix.os }}"),
        "native-test artifact names must identify their runner platform"
    );
    assert!(
        ci.contains("tail -n 160 native-test.log"),
        "failed native-test jobs must summarize the retained log"
    );
}

#[test]
fn stable_release_requires_privileged_self_hosted_windows_qualification() {
    let release = fs::read_to_string(".github/workflows/release.yml")
        .expect("release workflow should be readable");
    let qualification = release
        .split("  windows-privileged-qualification:")
        .nth(1)
        .and_then(|section| section.split("\n  publish:").next())
        .expect("stable release must define the privileged Windows job");

    assert!(qualification.contains("needs.verify.outputs.release-channel == 'stable'"));
    assert!(qualification.contains("[self-hosted, windows, x64, mailswiftsync-release]"));
    assert!(qualification.contains("RUNNER_ENVIRONMENT -ne 'self-hosted'"));
    assert!(qualification.contains("cargo test --locked --all-targets --all-features"));
    let publish = release
        .split("  publish:")
        .nth(1)
        .expect("release workflow must define publication");
    assert!(publish.contains("windows-privileged-qualification"));
    assert!(publish.contains("needs.windows-privileged-qualification.result == 'success'"));
    assert!(publish.contains("needs.verify.outputs.release-channel == 'preview'"));
}

#[test]
fn release_jobs_use_least_privilege_permissions() {
    let release = fs::read_to_string(".github/workflows/release.yml")
        .expect("release workflow should be readable");
    assert!(
        release.contains("permissions: {}"),
        "release workflow must deny permissions by default"
    );
    let job_lines = |job: &str| {
        let marker = format!("  {job}:");
        let mut found = false;
        let mut lines = Vec::new();
        for line in release.lines() {
            if line == marker {
                found = true;
                continue;
            }
            if found
                && line.starts_with("  ")
                && !line.starts_with("    ")
                && !line.trim().is_empty()
            {
                break;
            }
            if found {
                lines.push(line);
            }
        }
        lines.join("\n")
    };
    for job in [
        "verify",
        "packaged-integration",
        "native-runtime-tests",
        "windows-privileged-qualification",
    ] {
        let section = job_lines(job);
        assert!(
            section.contains("permissions:") && section.contains("contents: read"),
            "{job} must explicitly use read-only contents permissions"
        );
        assert!(
            !section.contains("id-token: write") && !section.contains("attestations: write"),
            "{job} must not receive release attestation permissions"
        );
    }
    assert_eq!(
        release.matches("contents: write").count(),
        1,
        "only the publish job may write repository contents"
    );
    let publish = job_lines("publish");
    assert!(publish.contains("contents: write"));
    assert!(publish.contains("id-token: write"));
    assert!(publish.contains("attestations: write"));
}

#[test]
fn imap_fuzz_workflow_runs_targets_with_nightly() {
    let workflow = std::fs::read_to_string(".github/workflows/imap-fuzz.yml")
        .expect("IMAP parser fuzz workflow must be checked in");
    assert!(
        workflow.contains("rustup toolchain install nightly"),
        "the workflow must install the unstable compiler required by libFuzzer"
    );
    assert!(
        workflow.contains("cargo +nightly fuzz run"),
        "the fuzz build must select nightly explicitly instead of the repository's stable override"
    );
    assert!(
        !workflow.contains("run: cargo fuzz run"),
        "a plain cargo fuzz command inherits the stable repository toolchain"
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
        Some("implemented")
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
        manifest.contains("Native IMAP message transfer (Dovecot) | yes | yes | integration"),
        "Capability manifest must record the passing native Dovecot integration fixture"
    );
    assert!(
        manifest.contains("Gmail-specific throttling presets")
            && manifest.contains("Microsoft 365-specific throttling presets")
            && manifest.contains("Fastmail-specific throttling presets")
            && manifest.matches("| no | no |").count() >= 3,
        "Capability manifest should not claim provider-specific throttle presets"
    );
    assert!(
        manifest.contains("explicit headless `post-report` export is available")
            && manifest.contains(
                "Automatic post-migration exception-report generation after each live run"
            ),
        "Capability manifest must distinguish an explicit report export from automatic report generation"
    );
    assert!(
        manifest.contains("Activity recovery guidance is advisory; it does not schedule, resume, or autonomously retry migrations")
            && !manifest.contains("Automated migrations (recovery guidance not surfaced)"),
        "Capability manifest must not say recovery guidance is absent while documenting it in Activity"
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
        "| Gmail / Workspace | Microsoft 365 |",
        "| Microsoft 365 | Gmail / Workspace |",
        "| Fastmail | Microsoft 365 |",
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
