use crate::{core, evidence_digest, with_proof_digest, write_private_atomic};
use std::path::Path;

/// Write customer-safe evidence for one durable project. This function has no
/// application/controller state and performs only the read needed to build
/// the artifact plus the atomic output write.
pub(crate) fn export_from_store(
    store: &core::StateStore,
    project_id: &str,
    path: &Path,
) -> Result<(), String> {
    let snapshot = store
        .project_report_snapshot(project_id)
        .map_err(|e| e.to_string())?
        .ok_or("The durable migration project no longer exists.")?;
    ensure_exportable(&snapshot)?;
    let project = snapshot.project;
    let mailboxes = snapshot
        .mailboxes
        .into_iter()
        .map(|mailbox| {
            let job = mailbox.job;
            let evidence = match mailbox.evidence {
                Some((run_id, value, plan_snapshot)) => {
                    let plan_snapshot = plan_snapshot
                        .ok_or("The customer proof refers to a missing evidence run.")?;
                    Some(serde_json::json!({
                        "run_id": run_id,
                        "scope": value.evidence_scope().label(),
                        "evidence_level": value.evidence_level(),
                        "evidence_digest": evidence_digest(&run_id, &plan_snapshot, &value),
                        "source_folders": value.source_folders,
                        "destination_folders": value.destination_folders,
                        "source_messages": value.source_messages,
                        "destination_messages": value.destination_messages,
                        "source_bytes": value.source_bytes,
                        "destination_bytes": value.destination_bytes,
                        "unmatched_messages": value.unmatched_messages,
                        "failed_messages": value.failed_messages,
                    }))
                }
                None => None,
            };
            Ok(serde_json::json!({
                "source_mailbox": job.source_mailbox,
                "destination_mailbox": job.destination_mailbox,
                "state": job.state,
                "verification_acceptance": mailbox.acceptance.map(|value| serde_json::json!({
                    "run_id": value.run_id,
                    "operator": value.operator,
                    "reason": value.reason,
                    "accepted_at": value.accepted_at,
                })),
                "evidence": evidence,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let run_manifest = snapshot
        .runs
        .into_iter()
        .map(|run| {
            let value = run.run;
            Ok(serde_json::json!({
                "run_id": value.id,
                "engine": value.engine,
                "engine_version": run.engine_version,
                "phase_at_start": value.phase_at_start,
                "status": value.status,
                "started_at": value.started_at,
                "finished_at": value.finished_at,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let value = with_proof_digest(serde_json::json!({
        "format": "mailswiftsync-customer-proof",
        "format_version": 1,
        "application_version": env!("CARGO_PKG_VERSION"),
        "artifact_role": "customer_evidence",
        "completion_claim": {
            "durable_project_phase": "Complete",
            "independent_certificate": false,
            "note": "This artifact records the completed state of the MailSwiftSync durable ledger at export time. Digest or signature validation proves artifact integrity or signer authenticity; it does not independently certify message-level completion."
        },
        "project": {
            "name": project.name,
            "phase": format!("{:?}", project.phase),
        },
        "mailboxes": mailboxes,
        "runs": run_manifest,
        "note": "This customer proof contains no passwords, credential references, endpoints, plan snapshots, executable paths, or diagnostic details. Aggregate and engine-confirmed evidence are not independent message-level reconciliation. Verify the proof digest, and add an Ed25519 signature before treating it as an authenticated artifact."
    }))?;
    let report = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    write_private_atomic(path, &report).map_err(|e| e.to_string())
}

/// Customer proof is a deliverable for a durably completed migration, not a
/// progress snapshot. Keep this gate next to the report builder so both the
/// GUI and the read-only CLI enforce the same completion contract.
fn ensure_exportable(snapshot: &core::ProjectReportSnapshot) -> Result<(), String> {
    if snapshot.project.phase != core::Phase::Complete {
        return Err(
            "Customer proof is available only after the project reaches durable Complete state."
                .into(),
        );
    }
    if snapshot.mailboxes.is_empty() {
        return Err("The project has no mailbox jobs to report.".into());
    }
    if let Some(mailbox) = snapshot.mailboxes.iter().find(|mailbox| {
        !matches!(
            mailbox.job.state.as_str(),
            "verified" | "verified_with_exceptions"
        ) || mailbox.evidence.is_none()
    }) {
        return Err(format!(
            "Customer proof is blocked: mailbox {} does not have a verified durable result.",
            mailbox.job.id
        ));
    }
    if snapshot
        .runs
        .iter()
        .any(|run| matches!(run.run.status.as_str(), "queued" | "running"))
    {
        return Err("Customer proof is blocked while migration work is still running.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
                            source_messages: 1,
                            destination_messages: 1,
                            source_bytes: 1,
                            destination_bytes: 1,
                            unmatched_messages: 0,
                            failed_messages: 0,
                            source_folders: 1,
                            destination_folders: 1,
                            authoritative: true,
                        },
                        Some("plan".into()),
                    )
                }),
            }],
            runs: Vec::new(),
        }
    }

    #[test]
    fn customer_proof_requires_durable_completion_and_evidence() {
        assert!(ensure_exportable(&snapshot(core::Phase::Verification, "verified", true)).is_err());
        assert!(ensure_exportable(&snapshot(core::Phase::Complete, "verified", false)).is_err());
        assert!(ensure_exportable(&snapshot(core::Phase::Complete, "verified", true)).is_ok());
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
            .is_ok()
        );
    }
}
