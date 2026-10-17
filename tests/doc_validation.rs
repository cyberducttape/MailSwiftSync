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
fn adoption_qualification_digests_are_well_formed_sha256() {
    let plan = fs::read_to_string("docs/adoption-qualification-plan.md")
        .expect("adoption qualification plan should exist");
    let mut digest_count = 0;
    for line in plan.lines() {
        let mut remaining = line;
        while let Some((_, suffix)) = remaining.split_once("SHA-256 `") {
            let (digest, rest) = suffix
                .split_once('`')
                .expect("SHA-256 digest should end with a backtick");
            assert_eq!(digest.len(), 64, "invalid SHA-256 length in {line}");
            assert!(
                digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "non-hex SHA-256 digest in {line}"
            );
            digest_count += 1;
            remaining = rest;
        }
    }
    assert!(
        digest_count > 0,
        "qualification plan should record SHA-256 digests"
    );
}

#[test]
fn systemd_service_example_uses_a_restricted_worker_sandbox() {
    let service = fs::read_to_string("docs/distribution/mailswiftsync-supervise.service")
        .expect("canonical systemd service example should exist");
    let guide = fs::read_to_string("docs/distribution/SERVICE.md")
        .expect("service deployment guide should exist");
    let rpm_package =
        fs::read_to_string("scripts/package-rpm.sh").expect("RPM package definition should exist");
    let workflow =
        fs::read_to_string(".github/workflows/ci.yml").expect("CI workflow should exist");
    assert!(guide.contains("mailswiftsync-supervise.service"));
    let packaged_service_path =
        "/usr/share/doc/mailswiftsync/docs/distribution/mailswiftsync-supervise.service";
    let packaged_install_guide_path = "/usr/share/doc/mailswiftsync/docs/distribution/INSTALL.md";
    assert!(guide.contains(packaged_service_path));
    assert!(rpm_package.contains(&format!("%doc {packaged_service_path}")));
    assert!(rpm_package.contains(&format!("%doc {packaged_install_guide_path}")));
    assert!(workflow.contains(&format!("test -f {packaged_service_path}")));
    assert!(workflow.contains(&format!("test -f {packaged_install_guide_path}")));
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
fn readme_describes_hardened_engine_execution_as_the_default() {
    let readme = fs::read_to_string("README.md").expect("README should exist");
    let profiles = fs::read_to_string("docs/execution-profiles.md")
        .expect("execution profile guide should exist");
    let implementation =
        fs::read_to_string("src/process.rs").expect("process implementation should exist");
    let cli = fs::read_to_string("src/cli.rs").expect("CLI implementation should exist");

    assert!(readme.contains("Hardened engine execution is the default"));
    assert!(readme.contains("--execution-profile compatibility"));
    assert!(profiles.contains("defaults to the hardened execution profile"));
    assert!(
        implementation.contains("static ENGINE_EXECUTION_PROFILE: AtomicU8 = AtomicU8::new(1)")
    );
    assert!(cli.contains("Use hardened (default) or compatibility engine environment"));
}

#[test]
fn production_status_discloses_plaintext_oauth_rotation_recovery_boundary() {
    let status =
        fs::read_to_string("PRODUCTION_STATUS.md").expect("production status should exist");
    let security = fs::read_to_string("docs/wiki/Security-and-profiles.md")
        .expect("security guide should exist");

    assert!(status.contains("rotation-recovery records may contain plaintext credentials"));
    assert!(status.contains("not encryption"));
    assert!(status.contains("live-provider expiry/rotation qualification remains pending"));
    assert!(security.contains(
        "it contains credentials in plaintext protected only by owner-only filesystem permissions"
    ));
}

#[test]
fn oauth_refresh_docs_disclose_the_windows_callback_boundary() {
    let readme = fs::read_to_string("README.md").expect("README should exist");
    let release = fs::read_to_string("docs/release-readiness.md")
        .expect("release readiness guide should exist");
    let alpha = fs::read_to_string("docs/release-0.1.0-alpha.md")
        .expect("alpha release notes should exist");
    let service =
        fs::read_to_string("docs/distribution/SERVICE.md").expect("service guide should exist");
    let bulk = fs::read_to_string("docs/wiki/Bulk-migrations.md")
        .expect("bulk migration guide should exist");
    let security = fs::read_to_string("docs/wiki/Security-and-profiles.md")
        .expect("security guide should exist");
    let runbook = fs::read_to_string("docs/wiki/Production-runbook.md")
        .expect("production runbook should exist");

    for document in [
        &readme, &release, &alpha, &service, &bulk, &security, &runbook,
    ] {
        assert!(document.contains("Unix"), "platform boundary missing");
        assert!(
            document.contains("Windows") && document.contains("--oauthrefreshcmd1/2"),
            "Windows refresh-callback limitation missing"
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
        readme.contains("signed APT/RPM repository metadata")
            && !readme.contains("packaged installers, and unattended production operation"),
        "README must distinguish shipped packages from repository/support work that remains outstanding"
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
fn cancelled_ci_runs_skip_the_test_inventory_rebuild() {
    let workflow =
        fs::read_to_string(".github/workflows/ci.yml").expect("Could not read CI workflow");
    let inventory_and_upload_steps = workflow
        .split("- name: Publish runnable test inventory")
        .nth(1)
        .expect("CI workflow must publish its runnable test inventory")
        .split("  critical-coverage:")
        .next()
        .expect("CI workflow must retain its coverage job");
    assert!(
        inventory_and_upload_steps
            .matches("if: ${{ always() && !cancelled() }}")
            .count()
            == 2,
        "test inventory and upload should run after ordinary failures, but neither should run after cancellation"
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
        status.contains("Operator-reviewed recovery")
            && status.contains("engine-specific resume behavior")
            && status.contains("still requires independent verification")
            && !status.contains("**Safe recovery** from interrupted migrations"),
        "active status must not imply a universal recovery guarantee"
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
fn crash_recovery_docs_record_current_head_local_fault_drills_with_limits() {
    let docs = fs::read_to_string("docs/crash-recovery-validation.md")
        .expect("crash-recovery validation evidence should exist")
        .replace("\r\n", "\n");
    for evidence in [
        "Current-head local process and ledger fault drills",
        "5a7129642d42f7b21b022d6601a81634a758cc1a",
        "04af4380a9dcbf9721cf44b9e0e7ff228f7a308d20fc38c0ee193ea08283f1d7",
        "RLIMIT_FSIZE` is a bounded write-failure\nsimulation",
        "do not simulate OS restart",
    ] {
        assert!(
            docs.contains(evidence),
            "current-head crash-recovery evidence is missing {evidence:?}"
        );
    }
}

#[test]
fn production_docs_keep_body_proof_and_power_loss_claims_bounded() {
    let verification = fs::read_to_string("docs/verification-envelope.md")
        .expect("verification envelope should exist");
    for claim in [
        "does not establish",
        "exact RFC822 byte sequence",
        "must not be described as fully content-verified",
    ] {
        assert!(
            verification.contains(claim),
            "missing claim boundary: {claim}"
        );
    }

    let durability = fs::read_to_string("docs/durability-and-concurrency-validation.md")
        .expect("durability validation plan should exist");
    for cut_point in [
        "temporary runtime-secret creation",
        "Dovecot checkpoint",
        "verification-page/cursor commit",
        "report signing",
        "explicitly marked for manual intervention",
    ] {
        assert!(
            durability.contains(cut_point),
            "missing power-cut case: {cut_point}"
        );
    }
    assert!(durability.contains("not substitutes for this campaign"));
}

#[test]
fn crash_recovery_docs_bound_engine_storage_fault_claim_to_ci_fixture() {
    let recovery = fs::read_to_string("docs/crash-recovery-validation.md")
        .expect("crash-recovery validation matrix should exist");
    let workflow = fs::read_to_string(".github/workflows/integration.yml")
        .expect("integration workflow should exist");

    assert!(
        recovery.contains("Destination write failure during APPEND (`RLIMIT_FSIZE` simulation)")
            && recovery.contains("not an actually full filesystem")
            && recovery.contains("38515348245")
            && recovery.contains("imapsync 2.314 and Dovecot 2.3.19.1")
            && recovery.contains("a real filesystem-full campaign remains unqualified")
            && workflow.contains("engine-storage-fault-smoke.sh"),
        "engine storage-fault evidence must identify the simulated limit, exact CI run/versions, and unqualified real-filesystem scope"
    );
}

#[test]
fn crash_recovery_docs_cover_terminal_evidence_sqlite_full_rollback() {
    let recovery = fs::read_to_string("docs/crash-recovery-validation.md")
        .expect("crash-recovery validation matrix should exist");
    for evidence in [
        "sqlite_full_rolls_back_terminal_evidence_and_allows_retry",
        "streams 4,000 mismatch rows",
        "no evidence or mismatch rows leak from the rolled-back transaction",
        "not an actually full filesystem or power-loss test",
    ] {
        assert!(
            recovery.contains(evidence),
            "terminal evidence SQLITE_FULL limitation is missing {evidence:?}"
        );
    }
}

#[test]
fn capability_manifest_does_not_overstate_verification_resume() {
    let manifest =
        fs::read_to_string("CAPABILITY_MANIFEST.md").expect("capability manifest should exist");
    let architecture =
        fs::read_to_string("docs/architecture.md").expect("architecture guide should exist");
    let verification_envelope = fs::read_to_string("docs/verification-envelope.md")
        .expect("verification envelope should exist");
    let release_readiness = fs::read_to_string("docs/release-readiness.md")
        .expect("release readiness document should exist");

    assert!(
        manifest.contains(
            "a committed durable metadata prefix can resume only with an identical tracked UIDVALIDITY/UIDNEXT/EXISTS/HIGHESTMODSEQ snapshot"
        ) && !manifest.contains("verification staging resumes from snapshot-bound cursors"),
        "capability claims must limit metadata resume to committed prefixes with an identical tracked snapshot"
    );
    assert!(
        architecture.contains(
            "For an incomplete metadata folder, a committed prefix resumes at the next sequence only after its page/cursor consistency is checked"
        ) && architecture.contains(
            "Snapshot drift, an untracked snapshot, or an invalid prefix restarts the folder from the first page"
        ),
        "the capability claim must remain aligned with the enforced architecture contract"
    );
    assert!(
        verification_envelope.contains(
            "a committed durable metadata prefix may resume only after a fresh SELECT exactly matches UIDVALIDITY, UIDNEXT, EXISTS, and tracked HIGHESTMODSEQ"
        ) && verification_envelope.contains("body-hash pages are always refetched"),
        "the verification envelope must describe the same guarded resume boundary"
    );
    assert!(
        release_readiness.contains(
            "An incomplete folder prefix resumes at the next sequence only when that same tracked snapshot matches"
        ) && release_readiness.contains("Body-hash pages are always refetched")
            && !release_readiness.contains("the folder is fully rescanned whether the snapshot matches or changed"),
        "release readiness must describe snapshot-bound metadata resume without implying body-proof reuse"
    );
}

#[test]
fn capability_manifest_discloses_the_modseq_requirement_for_flag_evidence() {
    let manifest =
        fs::read_to_string("CAPABILITY_MANIFEST.md").expect("capability manifest should exist");
    let envelope = fs::read_to_string("docs/verification-envelope.md")
        .expect("verification envelope should exist");

    for document in [&manifest, &envelope] {
        assert!(
            document.contains("tracked CONDSTORE")
                && document.contains("flag coverage")
                && document.contains("incomplete"),
            "flag-evidence documentation must disclose the no-MODSEQ coverage limit"
        );
    }
    assert!(
        !envelope.contains("The live FETCH already requests `FLAGS`")
            && envelope.contains("selected folder returns a tracked CONDSTORE"),
        "verification envelope must not imply FLAGS are always collected"
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
        .expect("verification envelope should exist")
        .replace("\r\n", "\n");
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
fn verification_envelope_records_current_head_skewed_scale_matrix() {
    let envelope = fs::read_to_string("docs/verification-envelope.md")
        .expect("verification envelope measurements should exist");
    for evidence in [
        "same revision also ran duplicate-ID, missing-ID, and mismatch-heavy",
        "Duplicate IDs | 1,000,000 | 20.725 s | 24.436 s | 24.352 s | 40,923/s",
        "Missing IDs | 1,000,000 | 19.279 s | 20.885 s | 21.482 s | 47,881/s",
        "Mismatch-heavy | 1,000,000 | 20.688 s | 31.787 s | 31.924 s | 31,459/s",
        "1,035,998,416 B / 100,893,904 B",
        "universal disk-sizing formula.",
    ] {
        assert!(
            envelope.contains(evidence),
            "current-head skewed verification evidence is missing {evidence:?}"
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
    assert!(
        release.contains("if: ${{ steps.release-channel.outputs.value == 'stable' }}")
            && release.contains("run: cargo audit --deny yanked"),
        "stable releases must fail closed when the dependency graph contains yanked crates"
    );
    let publish = release
        .split("  publish:")
        .nth(1)
        .expect("release workflow must define publication");
    assert!(
        publish.contains("needs.verify.result == 'success'"),
        "a failed stable dependency gate must prevent release publication"
    );

    let deny = fs::read_to_string("deny.toml").expect("dependency policy should exist");
    let readiness = fs::read_to_string("docs/release-readiness.md")
        .expect("release-readiness document should exist");
    let lockfile = fs::read_to_string("Cargo.lock").expect("Cargo lockfile should exist");
    assert!(!deny.contains("zip@8.6.0"));
    assert!(!lockfile.contains("version = \"8.6.0\""));
    assert!(!lockfile.contains("name = \"calamine\""));
    assert!(readiness.contains("no longer depends on Calamine"));
    assert!(readiness.contains("Calamine security report"));
    assert!(
        readiness.contains("Stable release CI continues to require `cargo audit --deny yanked`")
    );
    assert!(!lockfile.contains("name = \"ttf-parser\""));
    assert!(!readiness.contains("ttf-parser 0.25.1` warning remains tracked"));

    let dependency_audit = fs::read_to_string(".github/workflows/dependency-audit.yml")
        .expect("dependency audit workflow should exist")
        .replace("\r\n", "\n");
    assert!(
        dependency_audit.contains("pip-audit==2.10.1")
            && dependency_audit.contains(
                "run: >-\n          \"$RUNNER_TEMP/pip-audit/bin/pip-audit\""
            )
            && dependency_audit.contains("pip-audit\" -r scripts/requirements-provider-qualification.txt -r tests/provider-evidence/requirements.txt"),
        "scheduled dependency audit must scan all pinned provider-qualification Python requirements"
    );
}

#[test]
fn ci_lints_github_actions_workflows_with_a_pinned_actionlint_binary() {
    let ci = fs::read_to_string(".github/workflows/ci.yml").expect("CI workflow should exist");
    assert!(
        ci.contains("Lint GitHub Actions workflows")
            && ci.contains("version=1.7.12")
            && ci.contains("actionlint_${version}_linux_amd64.tar.gz")
            && ci.contains("ACTIONLINT_SHA256")
            && ci.contains("\"$install_dir/actionlint\" .github/workflows/*.yml"),
        "pull-request CI must syntax- and shell-lint every workflow with the checksum-pinned actionlint binary"
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
fn imap_list_control_octet_restriction_is_documented_as_a_compatibility_limit() {
    let guide = std::fs::read_to_string("docs/imap-fuzzing.md")
        .expect("IMAP parser behavior and fuzzing limits should be documented");
    let guide = guide.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(guide.contains("intentionally rejects ASCII control octets"));
    assert!(guide.contains("compatibility boundary"));
    assert!(guide.contains("must fail closed"));
    assert!(guide.contains("RFC 3501"));
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
fn webhook_contract_documents_signed_identity_and_replay_requirements() {
    let contract =
        fs::read_to_string("docs/psa-event-contract.md").expect("PSA event contract should exist");
    for required in [
        "X-MailSwiftSync-Signature-Version: 2",
        "X-MailSwiftSync-Signature-Timestamp",
        "v2=sha256=",
        "idempotency key, signature timestamp, and exact body bytes",
        "durably deduplicate the event ID",
        "The former body-only",
        "ignore ambient HTTP proxy",
    ] {
        assert!(
            contract.contains(required),
            "missing webhook contract requirement: {required}"
        );
    }
}

#[test]
fn psa_cutover_ready_contract_is_time_bounded_by_source_quiescence() {
    let contract =
        fs::read_to_string("docs/psa-event-contract.md").expect("PSA event contract should exist");
    for required in [
        "source-quiescence attestation expiry",
        "15 minutes",
        "refreshing the attestation during",
        "without rescanning",
        "clears the prior attestation",
        "until the operator records a new, fresh attestation",
        "independent_cursor_revalidated",
        "engine_or_aggregate_only",
        "same verification method and outcome",
        "Missing, stale, or",
        "not an atomic cross-server snapshot",
    ] {
        assert!(
            contract.contains(required),
            "cutover readiness contract is missing {required:?}"
        );
    }
}

#[test]
fn provider_qualification_manifest_covers_every_release_required_route() {
    let capabilities: toml::Value = toml::from_str(
        &fs::read_to_string("capabilities.toml").expect("capabilities.toml should exist"),
    )
    .expect("capabilities.toml should be valid TOML");
    let policy: serde_json::Value = serde_json::from_str(
        &fs::read_to_string("tests/provider-evidence/policy.json")
            .expect("provider evidence policy should exist"),
    )
    .expect("provider evidence policy should be valid JSON");

    let pair_names = [
        ("gmail->microsoft365", "Google Workspace → Microsoft 365"),
        ("microsoft365->gmail", "Microsoft 365 → Google Workspace"),
        (
            "cpanel_dovecot->microsoft365",
            "cPanel/Dovecot → Microsoft 365",
        ),
        ("cpanel_dovecot->gmail", "cPanel/Dovecot → Google Workspace"),
        (
            "cpanel_dovecot->cpanel_dovecot",
            "cPanel/Dovecot → cPanel/Dovecot",
        ),
        ("fastmail->gmail", "Fastmail → Google Workspace"),
        ("fastmail->microsoft365", "Fastmail → Microsoft 365"),
    ];
    let required_pairs = policy["providers"]
        .as_array()
        .expect("provider policy must contain routes")
        .iter()
        .filter(|route| route["release_required"] == true)
        .map(|route| {
            let key = route["pair"]
                .as_str()
                .expect("provider pair key must be a string");
            pair_names
                .iter()
                .find_map(|(known, label)| (*known == key).then_some(*label))
                .unwrap_or_else(|| panic!("provider policy route {key:?} has no manifest label"))
        })
        .collect::<std::collections::BTreeSet<_>>();
    let manifest_pairs = capabilities["provider_qualifications"]
        .as_array()
        .expect("capabilities must list provider qualifications")
        .iter()
        .map(|entry| {
            entry["pair"]
                .as_str()
                .expect("provider qualification pair must be a string")
        })
        .collect::<std::collections::BTreeSet<_>>();

    assert!(
        required_pairs.is_subset(&manifest_pairs),
        "capabilities.toml must list every release-required evidence route; missing: {:?}",
        required_pairs
            .difference(&manifest_pairs)
            .collect::<Vec<_>>()
    );
}

#[test]
fn provider_testing_guide_exists() {
    let guide = fs::read_to_string("docs/provider-testing-guide.md")
        .expect("Provider testing guide should exist");
    let guide = guide.split_whitespace().collect::<Vec<_>>().join(" ");
    let policy: serde_json::Value = serde_json::from_str(
        &fs::read_to_string("tests/provider-evidence/policy.json")
            .expect("Provider qualification policy should exist"),
    )
    .expect("Provider qualification policy should be valid JSON");
    let route_labels = [
        ("gmail", "Gmail / Google Workspace"),
        ("microsoft365", "Microsoft 365"),
        ("cpanel_dovecot", "cPanel/Dovecot"),
        ("fastmail", "Fastmail"),
    ];
    let auth_labels = [
        ("password", "Password/app password"),
        ("oauth2", "OAuth/XOAUTH2"),
    ];
    let routes = policy["providers"]
        .as_array()
        .expect("Provider policy must contain routes");
    assert_eq!(
        routes.len(),
        7,
        "Update this guide when release routes change"
    );
    for route in routes {
        let label = |field: &str, labels: &[(&str, &str)]| -> String {
            let value = route[field]
                .as_str()
                .expect("Provider route fields must be strings");
            labels
                .iter()
                .find_map(|(key, label)| (*key == value).then(|| (*label).to_owned()))
                .unwrap_or_else(|| panic!("Missing human-readable label for {value}"))
        };
        let source_auth = if route["source_provider"] == "fastmail" {
            "App-specific password".to_owned()
        } else {
            label("source_auth_method", &auth_labels)
        };
        let expected = format!(
            "| {} → {} | {} | {} |",
            label("source_provider", &route_labels),
            label("destination_provider", &route_labels),
            source_auth,
            label("destination_auth_method", &auth_labels),
        );
        assert!(
            guide.contains(&expected),
            "Provider guide is missing policy route: {expected}"
        );
    }

    assert!(guide.contains("OAuth/XOAUTH2 preferred"));
    assert!(guide.contains("do not restore the removed Basic Authentication"));
    assert!(guide.contains("Restrict deployment branches to the selected `main` branch"));
    assert!(guide.contains("require at least one reviewer before a job receives its secrets"));
    assert!(
        guide.contains("this guide does not assert a")
            && guide.contains("fixed commands-per-second rate")
    );
    for mailbox_shape in [
        "100,000 messages",
        "1 GiB",
        "10 GiB",
        "20 GiB",
        "50 GiB",
        "100 GiB",
    ] {
        assert!(
            guide.contains(mailbox_shape),
            "Provider testing guide must disclose the {mailbox_shape} qualification scenario"
        );
    }
    assert!(guide.contains("each scenario explicitly references"));
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
