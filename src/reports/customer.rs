use crate::branding::OperatorBranding;
use crate::{
    core,
    reports::integrity::{evidence_coverage_fields, evidence_digest, write_streamed_proof},
};
use std::collections::BTreeSet;
use std::path::Path;

fn execution_executable_sha256(plan_snapshot: &str) -> Option<String> {
    let snapshot: toml::Value = toml::from_str(plan_snapshot).ok()?;
    let identity = snapshot
        .get("profile")?
        .get("execution_executable_sha256")?
        .as_str()?;
    let digest = identity.strip_prefix("sha256:")?;
    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| digest.to_ascii_lowercase())
}

fn execution_profile(plan_snapshot: &str) -> &'static str {
    let value = toml::from_str::<toml::Value>(plan_snapshot)
        .ok()
        .and_then(|snapshot| {
            snapshot
                .get("profile")?
                .get("execution_profile")?
                .as_str()
                .map(str::to_owned)
        });
    match value.as_deref() {
        Some(value) if value.eq_ignore_ascii_case("hardened") => "hardened",
        Some(value) if value.eq_ignore_ascii_case("compatibility") => "compatibility",
        _ => "unknown",
    }
}

fn engine_identity(engine: &str, version: Option<&str>) -> String {
    version
        .map(str::trim)
        .filter(|version| !version.is_empty() && !version.eq_ignore_ascii_case("unknown"))
        .map(str::to_owned)
        .unwrap_or_else(|| engine.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderIdentity {
    pub(crate) source_provider: String,
    pub(crate) destination_provider: String,
    pub(crate) source_auth_method: String,
    pub(crate) destination_auth_method: String,
    pub(crate) fixture_id: String,
    pub(crate) scenario_ids: Vec<String>,
}

/// Write customer-safe evidence for one durable project. This function has no
/// application/controller state and performs only the read needed to build
/// the artifact plus the atomic output write.
pub(crate) fn export_from_store(
    store: &core::StateStore,
    project_id: &str,
    path: &Path,
    branding: &OperatorBranding,
) -> Result<(), String> {
    export_from_store_with_options_and_identity(store, project_id, path, false, branding, None)
}

#[cfg(test)]
pub(crate) fn export_from_store_with_options(
    store: &core::StateStore,
    project_id: &str,
    path: &Path,
    allow_incomplete: bool,
    branding: &OperatorBranding,
) -> Result<(), String> {
    export_from_store_with_options_and_identity(
        store,
        project_id,
        path,
        allow_incomplete,
        branding,
        None,
    )
}

pub(crate) fn export_from_store_with_options_and_identity(
    store: &core::StateStore,
    project_id: &str,
    path: &Path,
    allow_incomplete: bool,
    branding: &OperatorBranding,
    provider_identity: Option<&ProviderIdentity>,
) -> Result<(), String> {
    let reader = store
        .project_export_reader(project_id)
        .map_err(|e| e.to_string())?
        .ok_or("The durable migration project no longer exists.")?;
    // One aggregate pass; the proof's mailbox and run arrays are then
    // streamed again from the same pinned snapshot.
    let summary = ExportSummary::collect(&reader)?;
    if !allow_incomplete {
        summary.ensure_exportable(reader.project().phase, reader.has_active_runs())?;
    } else if summary.total_mailboxes == 0 {
        return Err("The project has no mailbox jobs to report.".into());
    }
    let durably_complete = summary.is_durably_complete(&reader);
    let total_mailboxes = summary.total_mailboxes;
    let exact_mailboxes = summary.exact_mailboxes;
    let mailbox_exceptions = total_mailboxes.saturating_sub(exact_mailboxes);
    let mut message_counts = serde_json::json!({
        "source": summary.messages.source,
        "destination": summary.messages.destination,
        "missing": summary.messages.missing,
        "extra": summary.messages.extra,
        "modified": summary.messages.modified,
        "unmatched": summary.messages.unmatched,
        "failed": summary.messages.failed,
    });
    if summary.unmatched_count_unknown {
        message_counts["unmatched"] = serde_json::Value::Null;
    }
    let only = |set: BTreeSet<String>| (set.len() == 1).then(|| set.into_iter().next().unwrap());
    let plan_sha256 = only(summary.plan_hashes);
    let engine = only(summary.engine_identities);
    let engine_sha256 = only(summary.engine_hashes);
    let verification_level = (summary.verification_levels.len() == 1)
        .then(|| summary.verification_levels.into_iter().next().unwrap());
    let mailboxes_transferred = summary.mailboxes_transferred;
    let mailboxes_verified = summary.mailboxes_verified;
    let project = reader.project().clone();
    // Each mailbox and run entry is generated on demand and serialized
    // immediately; the proof is never materialized as one JSON tree.
    let mailbox_entry =
        |mailbox: &core::ReportMailboxSnapshot| -> Result<serde_json::Value, String> {
            let job = &mailbox.job;
            let evidence = match &mailbox.evidence {
                Some((run_id, value, plan_snapshot)) => {
                    let plan_snapshot = plan_snapshot
                        .as_deref()
                        .ok_or("The customer proof refers to a missing evidence run.")?;
                    let mut evidence = serde_json::json!({
                        "run_id": run_id,
                        "migration_plan_sha256": crate::plan_identity::snapshot_sha256(plan_snapshot),
                        "engine_binary_sha256": execution_executable_sha256(plan_snapshot),
                        "execution_profile": execution_profile(plan_snapshot),
                        "scope": value.evidence_scope().label(),
                        "verification_method": value.verification_method().as_str(),
                        "verification_outcome": value.verification_outcome().as_str(),
                        "verification_outcome_label": value.verification_outcome().display_label(),
                        "verification_status": value.verification_status(),
                        "verification_level": value.verification_level(),
                        "evidence_level": value.evidence_level(),
                        "reason": value.verification_reason(),
                        "evidence_digest": evidence_digest(run_id, plan_snapshot, value),
                        "source_folders": value.source_folders,
                        "destination_folders": value.destination_folders,
                        "source_messages": value.source_messages,
                        "destination_messages": value.destination_messages,
                        "source_bytes": value.source_bytes,
                        "destination_bytes": value.destination_bytes,
                        "unmatched_messages": value.unresolved_count(),
                        "unresolved_count": value.unresolved_count(),
                        "missing_count": value.missing_count(),
                        "extra_count": value.extra_count(),
                        "modified_count": value.modified_count(),
                        "probable_count": value.probable_count(),
                        "metadata_matched_count": value.metadata_matched_count(),
                        "failed_messages": value.failed_messages,
                        "missing_messages": value.missing_messages,
                        "extra_messages": value.extra_messages,
                        "modified_messages": value.modified_messages,
                        "certificate_status": if value.is_exact_match() { "exact" } else { "exception" },
                    });
                    evidence
                        .as_object_mut()
                        .expect("evidence is a JSON object")
                        .extend(evidence_coverage_fields(value));
                    Some(evidence)
                }
                None => None,
            };
            Ok(serde_json::json!({
                "job_id": job.id,
                "source_mailbox": job.source_mailbox,
                "destination_mailbox": job.destination_mailbox,
                "state": job.state,
                "verification_acceptance": mailbox.acceptance.as_ref().map(|value| serde_json::json!({
                    "run_id": value.run_id,
                    "operator": value.operator,
                    "reason": value.reason,
                    "accepted_at": value.accepted_at,
                })),
                "evidence": evidence,
            }))
        };
    let run_entry = |run: &core::ReportRunSnapshot| -> serde_json::Value {
        let value = &run.run;
        serde_json::json!({
            "run_id": value.id,
            "project_id": project.id.clone(),
            "job_id": value.job_id,
            "engine": value.engine,
            "execution_profile": execution_profile(&value.plan_snapshot),
            "engine_version": run.engine_version,
            "phase_at_start": value.phase_at_start,
            "status": value.status,
            "started_at": value.started_at,
            "finished_at": value.finished_at,
            "diagnostic_stream_complete": run.diagnostic_lines_dropped.map(|count| count == 0),
            "dropped_presentation_lines": run.diagnostic_lines_dropped,
            "transfer_attempt_count": run.transfer_attempt_count,
            "unfinished_transfer_attempt_count": run.unfinished_transfer_attempt_count,
        })
    };
    let serde_json::Value::Object(fields) = serde_json::json!({
        "format": "mailswiftsync-customer-proof",
        "format_version": 2,
        "application_version": env!("CARGO_PKG_VERSION"),
        "artifact_role": "customer_evidence",
        "migration_id": project.id.clone(),
        "customer": project.name.clone(),
        "engine": engine,
        "engine_sha256": engine_sha256,
        "plan_sha256": plan_sha256,
        "source_provider": provider_identity.map(|identity| identity.source_provider.clone()),
        "destination_provider": provider_identity
            .map(|identity| identity.destination_provider.clone()),
        "certificate_summary": {
            "mailboxes_planned": total_mailboxes,
            "mailboxes_transferred": mailboxes_transferred,
            "mailboxes_verified": mailboxes_verified,
            "source_messages": message_counts["source"],
            "destination_messages": message_counts["destination"],
            "missing": message_counts["missing"],
            "extra": message_counts["extra"],
            "modified": message_counts["modified"],
            "exceptions": mailbox_exceptions,
            "verification_level": verification_level,
        },
        "completion_claim": {
            "status": if durably_complete { "durably_complete" } else { "incomplete" },
            "durable_project_phase": format!("{:?}", project.phase),
            "independent_certificate": false,
            "note": if durably_complete {
                "This artifact records the completed state of the MailSwiftSync durable ledger at export time. Digest or signature validation proves artifact integrity or signer authenticity; it does not independently certify message-level completion."
            } else {
                "This is an explicitly requested incomplete progress artifact. It is not a completion certificate; digest or signature validation proves artifact integrity or signer authenticity only."
            }
        },
        "verification_summary": {
            "mailboxes_total": total_mailboxes,
            "mailboxes_exact": exact_mailboxes,
            "mailboxes_with_exceptions": mailbox_exceptions,
            "messages": message_counts,
        },
        "project": {
            "project_id": project.id,
            "name": project.name,
            "phase": format!("{:?}", project.phase),
        },
        "provider_identity": provider_identity.map(|identity| serde_json::json!({
            "source_provider": identity.source_provider.clone(),
            "destination_provider": identity.destination_provider.clone(),
            "source_auth_method": identity.source_auth_method.clone(),
            "destination_auth_method": identity.destination_auth_method.clone(),
            "fixture_id": identity.fixture_id.clone(),
            "scenario_ids": identity.scenario_ids.clone(),
        })),
        "issued_by": if branding.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!({
                "name": branding.name.trim(),
                "contact": branding.contact.trim(),
            })
        },
        "note": "This customer proof contains no passwords, credential references, endpoints, plan snapshots, executable paths, or diagnostic details. Aggregate and engine-confirmed evidence are not independent message-level reconciliation. Verify the proof digest, and add an Ed25519 signature before treating it as an authenticated artifact."
    }) else {
        unreachable!("a JSON object literal is an object")
    };
    write_streamed_proof(
        path,
        fields,
        &[
            ("mailboxes", &|emit| {
                reader.for_each_mailbox(|mailbox| emit(mailbox_entry(&mailbox)?))
            }),
            ("runs", &|emit| {
                reader.for_each_run(|run| emit(run_entry(&run)))
            }),
        ],
    )
}

#[derive(Default)]
struct MessageTotals {
    source: u64,
    destination: u64,
    missing: u64,
    extra: u64,
    modified: u64,
    unmatched: u64,
    failed: u64,
}

/// Everything the proof header needs, accumulated in one streamed pass.
#[derive(Default)]
struct ExportSummary {
    total_mailboxes: usize,
    exact_mailboxes: usize,
    mailboxes_transferred: usize,
    mailboxes_verified: usize,
    messages: MessageTotals,
    unmatched_count_unknown: bool,
    plan_hashes: BTreeSet<String>,
    engine_hashes: BTreeSet<String>,
    engine_identities: BTreeSet<String>,
    verification_levels: BTreeSet<&'static str>,
    /// The first mailbox that blocks a completion claim, with the reason.
    first_blocker: Option<(String, &'static str)>,
}

impl ExportSummary {
    fn collect(reader: &core::ProjectExportReader<'_>) -> Result<Self, String> {
        let mut summary = Self::default();
        reader.for_each_mailbox(|mailbox| {
            summary.add_mailbox(&mailbox);
            Ok(())
        })?;
        reader.for_each_run(|run| {
            summary.engine_identities.insert(engine_identity(
                &run.run.engine,
                run.engine_version.as_deref(),
            ));
            Ok(())
        })?;
        Ok(summary)
    }

    fn add_mailbox(&mut self, mailbox: &core::ReportMailboxSnapshot) {
        self.total_mailboxes += 1;
        let state = mailbox.job.state.as_str();
        if matches!(
            state,
            "completed"
                | "verified"
                | "verified_with_exceptions"
                | "delta_required"
                | "verification_difference"
        ) {
            self.mailboxes_transferred += 1;
        }
        let accepted_exception =
            state == "verified_with_exceptions" && mailbox.acceptance.is_some();
        if state == "verified" || accepted_exception {
            self.mailboxes_verified += 1;
        }
        if self.first_blocker.is_none() {
            if state == "verified_with_exceptions" && mailbox.acceptance.is_none() {
                self.first_blocker = Some((
                    mailbox.job.id.clone(),
                    "does not have a durable operator acceptance for its verification exceptions",
                ));
            } else if !matches!(state, "verified" | "verified_with_exceptions")
                || mailbox.evidence.is_none()
            {
                self.first_blocker = Some((
                    mailbox.job.id.clone(),
                    "does not have a verified durable result",
                ));
            }
        }
        let Some((_, value, plan)) = &mailbox.evidence else {
            return;
        };
        if value.is_exact_match() {
            self.exact_mailboxes += 1;
        }
        match value.unresolved_count() {
            Some(count) => self.messages.unmatched = self.messages.unmatched.saturating_add(count),
            None => self.unmatched_count_unknown = true,
        }
        let totals = &mut self.messages;
        totals.source = totals.source.saturating_add(value.source_messages);
        totals.destination = totals
            .destination
            .saturating_add(value.destination_messages);
        totals.missing = totals.missing.saturating_add(value.missing_messages);
        totals.extra = totals.extra.saturating_add(value.extra_messages);
        totals.modified = totals.modified.saturating_add(value.modified_messages);
        totals.failed = totals.failed.saturating_add(value.failed_messages);
        self.verification_levels
            .insert(match value.verification_method {
                core::VerificationMethod::MetadataReconciliation => {
                    "Level 2 — Per-message metadata reconciliation"
                }
                core::VerificationMethod::BodyHash => "Level 3 — Bounded content fingerprints",
                core::VerificationMethod::AggregateEngine
                | core::VerificationMethod::NativeDovecot => "Level 1 — Aggregate evidence",
            });
        if let Some(plan) = plan.as_deref() {
            self.plan_hashes
                .insert(crate::plan_identity::snapshot_sha256(plan));
            if let Some(hash) = execution_executable_sha256(plan) {
                self.engine_hashes.insert(hash);
            }
        }
    }

    /// Customer proof is a deliverable for a durably completed migration, not
    /// a progress snapshot. Keep this gate next to the report builder so both
    /// the GUI and the read-only CLI enforce the same completion contract.
    fn ensure_exportable(&self, phase: core::Phase, has_active_runs: bool) -> Result<(), String> {
        if phase != core::Phase::Complete {
            return Err(
                "Customer proof is available only after the project reaches durable Complete state; use the CLI --allow-incomplete flag only for an explicitly labeled progress artifact."
                    .into(),
            );
        }
        if self.total_mailboxes == 0 {
            return Err("Customer proof is blocked: the project has no mailbox results.".into());
        }
        if let Some((job_id, reason)) = &self.first_blocker {
            return Err(format!(
                "Customer proof is blocked: mailbox {job_id} {reason}."
            ));
        }
        if has_active_runs {
            return Err("Customer proof is blocked while migration work is still running.".into());
        }
        Ok(())
    }

    fn is_durably_complete(&self, reader: &core::ProjectExportReader<'_>) -> bool {
        reader.project().phase == core::Phase::Complete
            && self.total_mailboxes > 0
            && self.first_blocker.is_none()
            && !reader.has_active_runs()
    }
}

/// The completion gate applied to an in-memory snapshot, for tests that build
/// one directly.
#[cfg(test)]
fn ensure_exportable(snapshot: &core::ProjectReportSnapshot) -> Result<(), String> {
    let mut summary = ExportSummary::default();
    for mailbox in &snapshot.mailboxes {
        summary.add_mailbox(mailbox);
    }
    summary.ensure_exportable(snapshot.project.phase, snapshot.has_active_runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    fn test_directory(prefix: &str) -> std::path::PathBuf {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-{prefix}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&directory).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&directory, permissions).unwrap();
        }
        directory
    }

    #[test]
    fn execution_binary_hash_is_extracted_only_from_a_valid_run_snapshot() {
        let directory = test_directory("customer-proof-hash");
        let cleanup = crate::credentials::CleanupGuard::new(vec![directory.clone()]);
        let executable = directory.join("mock-engine");
        std::fs::write(&executable, b"mock engine binary").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&executable, permissions).unwrap();
        }
        let mut form = crate::Form::default();
        form.profile.imapsync_path = executable.to_string_lossy().into_owned();
        let snapshot = form.plan_snapshot();
        let expected =
            crate::plan_identity::executable_content_identity(&executable.to_string_lossy());
        assert_eq!(
            execution_executable_sha256(&snapshot),
            expected.strip_prefix("sha256:").map(str::to_owned)
        );
        drop(cleanup);
        assert_eq!(execution_executable_sha256("not a run snapshot"), None);
        assert_eq!(
            execution_executable_sha256(
                "dry_run = false\n\n[profile]\nexecution_executable_sha256 = \"unresolved\"\n"
            ),
            None
        );
        assert_eq!(
            execution_executable_sha256(
                "dry_run = false\n\n[profile]\nexecution_executable_sha256 = \"sha256:abcd\"\n"
            ),
            None
        );
    }

    #[test]
    fn customer_proof_exposes_plan_and_engine_hashes_without_exporting_plan() {
        let directory = test_directory("customer-proof");
        let cleanup = crate::credentials::CleanupGuard::new(vec![directory.clone()]);
        let executable = directory.join("mock-engine");
        std::fs::write(&executable, b"mock engine binary").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&executable, permissions).unwrap();
        }
        let mut form = crate::Form::default();
        form.profile.imapsync_path = executable.to_string_lossy().into_owned();
        let plan_snapshot = form.plan_snapshot();

        let store = core::StateStore::in_memory().unwrap();
        let project = store
            .create_project("proof", "source", "destination")
            .unwrap();
        let job = store
            .add_mailbox(&project.id, "source@example.com", "destination@example.com")
            .unwrap();
        store
            .begin_run_with_snapshot(&project.id, &job, "run-proof", "imapsync", &plan_snapshot)
            .unwrap();
        store
            .record_engine_version("run-proof", "imapsync 2.314")
            .unwrap();
        store
            .finish_run_for_mailbox_with_evidence(
                &project.id,
                &job,
                "run-proof",
                "completed",
                "verified",
                "verified",
                &core::MailboxEvidence {
                    verification_method: core::VerificationMethod::AggregateEngine,
                    verification_outcome: None,
                    source_messages: 1,
                    destination_messages: 1,
                    source_bytes: 100,
                    destination_bytes: 100,
                    unmatched_messages: Some(0),
                    failed_messages: 0,
                    source_folders: 1,
                    destination_folders: 1,
                    authoritative: true,
                    missing_messages: 0,
                    extra_messages: 0,
                    modified_messages: 0,
                    probable_messages: 0,
                    rfc_message_id_matches: 0,
                    synthetic_identity_matches: 0,
                    flag_verification: None,
                },
            )
            .unwrap();
        store
            .transition(&project.id, core::Phase::Preflight)
            .unwrap();
        store
            .transition(&project.id, core::Phase::Verification)
            .unwrap();
        store
            .transition(&project.id, core::Phase::Complete)
            .unwrap();

        let output = directory.join("customer-proof.json");
        export_from_store(&store, &project.id, &output, &OperatorBranding::default()).unwrap();
        let signing_key =
            ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                .unwrap();
        let signing_key_path = directory.join("signing-key.pk8");
        let mut key_file = crate::credentials::open_secret_file(&signing_key_path).unwrap();
        std::io::Write::write_all(&mut key_file, signing_key.as_ref()).unwrap();
        drop(key_file);
        let signed_output = directory.join("certificate.json");
        crate::reports::signing::sign_file_to(
            &output,
            &signed_output,
            &signing_key_path,
            "test-key",
            true,
        )
        .unwrap();
        let signing_key_pair =
            ring::signature::Ed25519KeyPair::from_pkcs8(signing_key.as_ref()).unwrap();
        let trusted_public_key =
            crate::reports::integrity::hex_encode(signing_key_pair.public_key().as_ref());
        crate::reports::signing::verify_file(&signed_output, Some(&trusted_public_key)).unwrap();
        let certificate: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&signed_output).unwrap()).unwrap();
        assert_eq!(certificate["engine"], "imapsync 2.314");
        assert_eq!(certificate["artifact_role"], "signed_migration_certificate");
        assert_eq!(certificate["format_version"], 2);
        assert_eq!(certificate["proof_signature"]["algorithm"], "Ed25519");
        let evidence = &certificate["mailboxes"][0]["evidence"];
        assert_eq!(
            evidence["migration_plan_sha256"],
            crate::plan_identity::snapshot_sha256(&plan_snapshot)
        );
        let binary_identity =
            crate::plan_identity::executable_content_identity(&executable.to_string_lossy());
        assert_eq!(
            evidence["engine_binary_sha256"],
            binary_identity.strip_prefix("sha256:").unwrap()
        );
        // Aggregate evidence names its tier, checks no individual message,
        // and reports flags as unverified rather than omitting them.
        assert_eq!(
            evidence["verification_tier"],
            "Level 1 — Aggregate evidence — individual messages not compared"
        );
        assert_eq!(
            evidence["verification_level"],
            "Level 1 — Aggregate evidence — individual messages not compared"
        );
        assert_eq!(
            evidence["verification_outcome_label"],
            "Exact metadata match — message bodies not compared"
        );
        assert_eq!(
            evidence["evidence_level"],
            "Engine-confirmed exact match — not message-body proof"
        );
        assert_eq!(evidence["message_coverage"]["checked_messages"], 0);
        assert_eq!(evidence["message_coverage"]["complete"], false);
        assert_eq!(
            evidence["identity_match_counts"]["rfc_message_id_matches"],
            0
        );
        assert_eq!(
            evidence["identity_match_counts"]["synthetic_identity_matches"],
            0
        );
        assert!(evidence["flag_verification"].is_null());
        assert!(!certificate.to_string().contains(&plan_snapshot));
        assert!(
            !certificate
                .to_string()
                .contains(&executable.to_string_lossy().to_string())
        );
        drop(cleanup);
    }

    #[test]
    fn engine_identity_falls_back_when_version_is_unknown_or_blank() {
        assert_eq!(
            engine_identity("Dovecot native", Some("unknown")),
            "Dovecot native"
        );
        assert_eq!(engine_identity("imapsync", None), "imapsync");
        assert_eq!(engine_identity("imapsync", Some("  ")), "imapsync");
        assert_eq!(
            engine_identity("imapsync", Some("imapsync 2.314")),
            "imapsync 2.314"
        );
    }

    fn snapshot(phase: core::Phase, state: &str, evidence: bool) -> core::ProjectReportSnapshot {
        core::ProjectReportSnapshot {
            project: core::Project {
                id: "project".into(),
                name: "Project".into(),
                source_endpoint: "source".into(),
                destination_endpoint: "destination".into(),
                phase,
            },
            mailboxes: vec![core::ReportMailboxSnapshot {
                job: core::MailboxJob {
                    id: "job".into(),
                    source_mailbox: "source@example.com".into(),
                    destination_mailbox: "destination@example.com".into(),
                    state: state.into(),
                    config: None,
                },
                attention_reason: None,
                acceptance: None,
                evidence: evidence.then(|| {
                    (
                        "run".into(),
                        core::MailboxEvidence {
                            verification_method: core::VerificationMethod::AggregateEngine,
                            verification_outcome: None,
                            source_messages: 1,
                            destination_messages: 1,
                            source_bytes: 1,
                            destination_bytes: 1,
                            unmatched_messages: Some(0),
                            failed_messages: 0,
                            source_folders: 1,
                            destination_folders: 1,
                            authoritative: true,
                            missing_messages: 0,
                            extra_messages: 0,
                            modified_messages: 0,
                            probable_messages: 0,
                            rfc_message_id_matches: 0,
                            synthetic_identity_matches: 0,
                            flag_verification: None,
                        },
                        Some("plan".into()),
                    )
                }),
            }],
            runs: Vec::new(),
            has_active_runs: false,
        }
    }

    #[test]
    fn customer_proof_requires_durable_completion_and_evidence() {
        assert!(ensure_exportable(&snapshot(core::Phase::Verification, "verified", true)).is_err());
        assert!(ensure_exportable(&snapshot(core::Phase::Complete, "verified", false)).is_err());
        assert!(ensure_exportable(&snapshot(core::Phase::Complete, "verified", true)).is_ok());
        let mut active = snapshot(core::Phase::Complete, "verified", true);
        active.has_active_runs = true;
        assert!(ensure_exportable(&active).is_err());
        let mut empty = snapshot(core::Phase::Complete, "verified", true);
        empty.mailboxes.clear();
        assert!(ensure_exportable(&empty).is_err());
    }

    #[test]
    fn customer_proof_rejects_unverified_mailboxes() {
        assert!(ensure_exportable(&snapshot(core::Phase::Complete, "ready", true)).is_err());
        assert!(
            ensure_exportable(&snapshot(
                core::Phase::Complete,
                "verified_with_exceptions",
                true
            ))
            .is_err()
        );
        let mut accepted = snapshot(core::Phase::Complete, "verified_with_exceptions", true);
        accepted.mailboxes[0].acceptance = Some(core::VerificationAcceptance {
            job_id: "job".into(),
            run_id: "run".into(),
            operator: "operator".into(),
            reason: "Reviewed bounded flag coverage".into(),
            accepted_at: "now".into(),
        });
        assert!(ensure_exportable(&accepted).is_ok());
    }

    #[test]
    fn incomplete_customer_proof_requires_explicit_opt_in_and_is_labeled() {
        let store = core::StateStore::in_memory().unwrap();
        let project = store
            .create_project("progress", "source", "destination")
            .unwrap();
        store
            .add_mailbox(&project.id, "source@example.com", "destination@example.com")
            .unwrap();
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "mailswiftsync-incomplete-proof-{}",
                uuid::Uuid::new_v4()
            ));
        crate::credentials::ensure_private_directory(&directory).unwrap();
        let path = directory.join("proof.json");

        let branding = OperatorBranding::default();
        assert!(export_from_store(&store, &project.id, &path, &branding).is_err());
        export_from_store_with_options(&store, &project.id, &path, true, &branding).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["completion_claim"]["status"], "incomplete");
        assert_eq!(value["completion_claim"]["independent_certificate"], false);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn customer_proof_reports_durable_diagnostic_drop_count_without_content() {
        let store = core::StateStore::in_memory().unwrap();
        let project = store
            .create_project("diagnostics", "source", "destination")
            .unwrap();
        let job = store
            .add_mailbox(&project.id, "source@example.com", "destination@example.com")
            .unwrap();
        store
            .insert_run_for_test(&project.id, Some(&job), "diagnostic-run", "imapsync")
            .unwrap();
        store
            .record_events_for_runs_batch(&[("diagnostic-run", "diagnostic_lines_dropped", "3284")])
            .unwrap();
        store
            .record_transfer_attempt_started(
                "diagnostic-run",
                1,
                &crate::core::TransferPassIntent::for_test(),
            )
            .unwrap();
        store
            .finish_run("diagnostic-run", "completed", "ok")
            .unwrap();

        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "mailswiftsync-diagnostic-proof-{}",
                uuid::Uuid::new_v4()
            ));
        crate::credentials::ensure_private_directory(&directory).unwrap();
        let path = directory.join("proof.json");
        export_from_store_with_options(
            &store,
            &project.id,
            &path,
            true,
            &OperatorBranding::default(),
        )
        .unwrap();
        let proof: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(proof["runs"][0]["diagnostic_stream_complete"], false);
        assert_eq!(proof["runs"][0]["dropped_presentation_lines"], 3284);
        assert_eq!(proof["runs"][0]["transfer_attempt_count"], 1);
        assert_eq!(proof["runs"][0]["unfinished_transfer_attempt_count"], 1);
        assert!(!proof.to_string().contains("raw engine output"));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn customer_proof_omits_issued_by_without_branding() {
        let store = core::StateStore::in_memory().unwrap();
        let project = store
            .create_project("unbranded", "source", "destination")
            .unwrap();
        store
            .add_mailbox(&project.id, "source@example.com", "destination@example.com")
            .unwrap();
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "mailswiftsync-unbranded-proof-{}",
                uuid::Uuid::new_v4()
            ));
        crate::credentials::ensure_private_directory(&directory).unwrap();
        let path = directory.join("proof.json");

        export_from_store_with_options(
            &store,
            &project.id,
            &path,
            true,
            &OperatorBranding::default(),
        )
        .unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(value["issued_by"].is_null());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn customer_proof_includes_trimmed_issued_by_with_branding() {
        let store = core::StateStore::in_memory().unwrap();
        let project = store
            .create_project("branded", "source", "destination")
            .unwrap();
        store
            .add_mailbox(&project.id, "source@example.com", "destination@example.com")
            .unwrap();
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "mailswiftsync-branded-proof-{}",
                uuid::Uuid::new_v4()
            ));
        crate::credentials::ensure_private_directory(&directory).unwrap();
        let path = directory.join("proof.json");
        let branding = OperatorBranding {
            name: "  Acme Managed Services  ".into(),
            contact: "  support@acme.example  ".into(),
        };

        export_from_store_with_options(&store, &project.id, &path, true, &branding).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["issued_by"]["name"], "Acme Managed Services");
        assert_eq!(value["issued_by"]["contact"], "support@acme.example");
        let _ = std::fs::remove_dir_all(directory);
    }

    /// Peak memory of a customer-proof export over 100,000 verified mailboxes,
    /// each with a run carrying a realistic ~2 KB plan snapshot. Run with
    /// `cargo test --release customer_proof_export_memory_benchmark -- --ignored --nocapture`.
    #[test]
    #[ignore = "opt-in release memory benchmark"]
    fn customer_proof_export_memory_benchmark() {
        // Building the fixture dominates the runtime (about 28 minutes for
        // 100,000 mailboxes); set MAILSWIFTSYNC_EXPORT_BENCH_MAILBOXES lower
        // for a quicker, proportionally scaled measurement.
        let mailboxes: usize = std::env::var("MAILSWIFTSYNC_EXPORT_BENCH_MAILBOXES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(100_000);
        fn rss_kib(field: &str) -> u64 {
            std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|status| {
                    status
                        .lines()
                        .find(|line| line.starts_with(field))?
                        .split_whitespace()
                        .nth(1)?
                        .parse()
                        .ok()
                })
                .unwrap_or(0)
        }
        let runs_per_mailbox: usize = std::env::var("MAILSWIFTSYNC_EXPORT_BENCH_RUNS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(3);
        let directory = test_directory("customer-proof-scale");
        // A file-backed ledger, as in production; an in-memory one would
        // count the whole database toward the export's resident memory.
        let store = core::StateStore::open(directory.join("state.db")).unwrap();
        let project = store
            .create_project("export-benchmark", "source.example", "destination.example")
            .unwrap();
        let fixture_started = std::time::Instant::now();
        store
            .insert_export_benchmark_fixture_for_test(&project.id, mailboxes, runs_per_mailbox)
            .unwrap();
        eprintln!(
            "customer-proof-export fixture mailboxes={mailboxes} runs={} elapsed_ms={}",
            mailboxes * runs_per_mailbox,
            fixture_started.elapsed().as_millis()
        );
        let path = directory.join("customer-proof.json");
        let _ = std::fs::write("/proc/self/clear_refs", "5");
        let before = rss_kib("VmRSS:");
        let started = std::time::Instant::now();
        export_from_store_with_options(
            &store,
            &project.id,
            &path,
            true,
            &OperatorBranding::default(),
        )
        .unwrap();
        let elapsed = started.elapsed().as_millis();
        let peak = rss_kib("VmHWM:");
        let bytes = std::fs::metadata(&path).unwrap().len();
        eprintln!(
            "customer-proof-export mailboxes={mailboxes} file_mib={} elapsed_ms={elapsed} rss_before_mib={} peak_mib={} growth_mib={}",
            bytes / (1024 * 1024),
            before / 1024,
            peak / 1024,
            peak.saturating_sub(before) / 1024
        );
        let export_growth = peak.saturating_sub(before) / 1024;

        let _ = std::fs::write("/proc/self/clear_refs", "5");
        let before = rss_kib("VmRSS:");
        let started = std::time::Instant::now();
        crate::reports::signing::verify_file(&path, None).unwrap();
        let elapsed = started.elapsed().as_millis();
        let peak = rss_kib("VmHWM:");
        let verify_growth = peak.saturating_sub(before) / 1024;
        eprintln!(
            "customer-proof-verify mailboxes={mailboxes} elapsed_ms={elapsed} peak_mib={} growth_mib={verify_growth}",
            peak / 1024,
        );
        let _ = std::fs::remove_dir_all(directory);
        // Both passes stream; memory must not grow with the project.
        assert!(export_growth < 256, "export grew {export_growth} MiB");
        assert!(verify_growth < 256, "verify grew {verify_growth} MiB");
    }
}
