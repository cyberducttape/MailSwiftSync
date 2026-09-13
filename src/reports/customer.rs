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
    if snapshot.mailboxes.is_empty() {
        return Err("The project has no mailbox jobs to report.".into());
    }
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
