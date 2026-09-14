use crate::{
    core, evidence_digest, markdown_escape, needs_operator_review, plan_snapshot_sha256,
    project_health_state_counts, with_proof_digest, write_private_atomic,
};
use std::path::Path;

pub(crate) fn build_project_report(
    store: &core::StateStore,
    project_id: &str,
) -> Result<String, String> {
    let snapshot = store
        .project_report_snapshot(project_id)
        .map_err(|e| e.to_string())?
        .ok_or("The durable migration project no longer exists.")?;
    if snapshot.mailboxes.is_empty() {
        return Err("The project has no mailbox jobs to report.".into());
    }
    let project = snapshot.project;
    let verified = snapshot
        .mailboxes
        .iter()
        .filter(|mailbox| {
            matches!(
                mailbox.job.state.as_str(),
                "verified" | "verified_with_exceptions"
            )
        })
        .count();
    let attention = snapshot
        .mailboxes
        .iter()
        .filter(|mailbox| needs_operator_review(&mailbox.job.state))
        .count();
    let mut report = format!(
        "# MailSwiftSync project report\n\n- Project: {}\n- Project ID: `{}`\n- Source endpoint: {}\n- Destination endpoint: {}\n- Phase: `{:?}`\n- Mailboxes: {}\n- Verified: {}\n- Attention required: {}\n\n## Mailbox results\n\n| Source mailbox | Destination mailbox | State | Attention reason | Recommended action | Exception acceptance | Evidence run | Evidence | Evidence digest | Source messages | Destination messages | Unmatched | Failed |\n|---|---|---|---|---|---|---|---|---|---:|---:|---:|---:|\n",
        markdown_escape(&project.name),
        project.id,
        markdown_escape(&project.source_endpoint),
        markdown_escape(&project.destination_endpoint),
        project.phase,
        snapshot.mailboxes.len(),
        verified,
        attention,
    );
    for mailbox in snapshot.mailboxes {
        let job = mailbox.job;
        let attention_reason = mailbox.attention_reason;
        let acceptance = mailbox.acceptance;
        let acceptance_summary = acceptance
            .as_ref()
            .map(|value| format!("{}: {}", value.operator, value.reason))
            .unwrap_or_else(|| "—".into());
        let reason_label = attention_reason.map(|reason| reason.label()).unwrap_or("—");
        let recommended_action = attention_reason
            .map(|reason| reason.recommended_action())
            .unwrap_or("—");
        if let Some((evidence_run_id, evidence, plan_snapshot)) = mailbox.evidence {
            let plan_snapshot = plan_snapshot.ok_or("The evidence run no longer exists.")?;
            report.push_str(&format!(
                "| {} | {} | `{}` | {} | {} | {} | `{}` | {} | `{}` | {} | {} | {} | {} |\n",
                markdown_escape(&job.source_mailbox),
                markdown_escape(&job.destination_mailbox),
                job.state,
                markdown_escape(reason_label),
                markdown_escape(recommended_action),
                markdown_escape(&acceptance_summary),
                evidence_run_id,
                evidence.evidence_level(),
                evidence_digest(&evidence_run_id, &plan_snapshot, &evidence),
                evidence.source_messages,
                evidence.destination_messages,
                evidence.unmatched_messages,
                evidence.failed_messages,
            ));
        } else {
            report.push_str(&format!(
                "| {} | {} | `{}` | {} | {} | {} | — | missing | — | — | — | — | — |\n",
                markdown_escape(&job.source_mailbox),
                markdown_escape(&job.destination_mailbox),
                job.state,
                markdown_escape(reason_label),
                markdown_escape(recommended_action),
                markdown_escape(&acceptance_summary),
            ));
        }
    }
    report.push_str("\n## Recent runs\n\n| Run | Engine | Engine version | Phase at start | Status | Plan reference | Started | Finished | Detail |\n|---|---|---|---|---|---|---|---|---|\n");
    for report_run in snapshot.runs.iter().rev().take(20) {
        let run = &report_run.run;
        let engine_version = report_run
            .engine_version
            .clone()
            .unwrap_or_else(|| "unavailable".into());
        report.push_str(&format!(
            "| `{}` | {} | {} | `{}` | `{}` | `{}` | {} | {} | {} |\n",
            run.id,
            markdown_escape(&run.engine),
            markdown_escape(&engine_version),
            markdown_escape(&run.phase_at_start),
            run.status,
            plan_snapshot_sha256(&run.plan_snapshot),
            run.started_at,
            run.finished_at
                .clone()
                .unwrap_or_else(|| "in progress".into()),
            markdown_escape(if run.detail.is_empty() {
                "—"
            } else {
                &run.detail
            }),
        ));
    }
    report.push_str("\nEvidence levels describe what was actually established. Engine-confirmed output is not independent message-level reconciliation, and aggregate totals are not proof of message identity. Missing evidence or any state other than `verified` or `verified_with_exceptions` requires operator review before declaring the project complete.\n");
    Ok(report)
}

pub(crate) fn build_project_json(
    store: &core::StateStore,
    project_id: &str,
) -> Result<String, String> {
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
            let attention_reason = mailbox.attention_reason;
            match mailbox.evidence {
                Some((evidence_run_id, evidence, plan_snapshot)) => {
                    let plan_snapshot = plan_snapshot.ok_or("The evidence run no longer exists.")?;
                    let digest = evidence_digest(&evidence_run_id, &plan_snapshot, &evidence);
                    Ok(serde_json::json!({
                        "id": job.id,
                        "source_mailbox": job.source_mailbox,
                        "destination_mailbox": job.destination_mailbox,
                        "state": job.state,
                        "attention_reason": attention_reason.map(|reason| reason.as_str()),
                        "recommended_action": attention_reason.map(|reason| reason.recommended_action()),
                        "verification_acceptance": mailbox.acceptance,
                        "evidence": {
                            "run_id": evidence_run_id,
                            "scope": evidence.evidence_scope().label(),
                            "evidence_level": evidence.evidence_level(),
                            "authoritative": evidence.authoritative,
                            "evidence_digest": digest,
                            "source_folders": evidence.source_folders,
                            "destination_folders": evidence.destination_folders,
                            "source_messages": evidence.source_messages,
                            "destination_messages": evidence.destination_messages,
                            "source_bytes": evidence.source_bytes,
                            "destination_bytes": evidence.destination_bytes,
                            "unmatched_messages": evidence.unmatched_messages,
                            "failed_messages": evidence.failed_messages,
                        }
                    }))
                }
                None => Ok(serde_json::json!({
                    "id": job.id,
                    "source_mailbox": job.source_mailbox,
                    "destination_mailbox": job.destination_mailbox,
                    "state": job.state,
                    "attention_reason": attention_reason.map(|reason| reason.as_str()),
                    "recommended_action": attention_reason.map(|reason| reason.recommended_action()),
                    "verification_acceptance": mailbox.acceptance,
                    "evidence": null
                })),
            }
        })
        .collect::<Result<Vec<_>, String>>()?;
    let runs = snapshot
        .runs
        .into_iter()
        .map(|report_run| {
            let run = report_run.run;
            Ok(serde_json::json!({
                "id": run.id,
                "job_id": run.job_id,
                "parent_run_id": run.parent_run_id,
                "engine": run.engine,
                "engine_version": report_run.engine_version,
                "phase_at_start": run.phase_at_start,
                "plan_snapshot_sha256": plan_snapshot_sha256(&run.plan_snapshot),
                "status": run.status,
                "started_at": run.started_at,
                "finished_at": run.finished_at,
                "detail": run.detail,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let value = serde_json::json!({
        "format": "mailswiftsync-project-report",
        "format_version": 2,
        "application_version": env!("CARGO_PKG_VERSION"),
        "run_manifest_complete": true,
        "project": {
            "id": project.id,
            "name": project.name,
            "source_endpoint": project.source_endpoint,
            "destination_endpoint": project.destination_endpoint,
            "phase": format!("{:?}", project.phase),
        },
        "mailboxes": mailboxes,
        "runs": runs,
        "note": "The run manifest contains every durable run for this project. Aggregate evidence is not message-level reconciliation; unresolved or missing evidence requires operator review."
    });
    serde_json::to_string_pretty(&with_proof_digest(value)?).map_err(|e| e.to_string())
}

pub(crate) fn export_health(
    store: &core::StateStore,
    project_id: &str,
    path: &Path,
) -> Result<(), String> {
    let snapshot = store
        .project_report_snapshot(project_id)
        .map_err(|e| e.to_string())?;
    let snapshot = snapshot.ok_or("The durable migration project no longer exists.")?;
    let project = snapshot.project;
    let jobs = snapshot
        .mailboxes
        .iter()
        .map(|mailbox| mailbox.job.clone())
        .collect::<Vec<_>>();
    let runs = snapshot
        .runs
        .into_iter()
        .rev()
        .take(20)
        .map(|snapshot| snapshot.run)
        .collect::<Vec<_>>();
    let attention = jobs
        .iter()
        .filter(|job| needs_operator_review(&job.state))
        .map(|job| {
            serde_json::json!({
                "id": job.id,
                "source_mailbox": job.source_mailbox,
                "destination_mailbox": job.destination_mailbox,
                "state": job.state,
            })
        })
        .collect::<Vec<_>>();
    let recent_runs = runs
        .iter()
        .map(|run| {
            serde_json::json!({
                "id": run.id,
                "job_id": run.job_id,
                "parent_run_id": run.parent_run_id,
                "engine": run.engine,
                "phase_at_start": run.phase_at_start,
                "plan_snapshot_sha256": plan_snapshot_sha256(&run.plan_snapshot),
                "status": run.status,
                "started_at": run.started_at,
                "finished_at": run.finished_at,
                "detail": run.detail,
            })
        })
        .collect::<Vec<_>>();
    let value = serde_json::json!({
        "format": "mailswiftsync-project-health",
        "version": 1,
        "project": {
            "id": project.id,
            "name": project.name,
            "phase": format!("{:?}", project.phase),
            "source_endpoint": project.source_endpoint,
            "destination_endpoint": project.destination_endpoint,
        },
        "mailboxes": {
            "total": jobs.len(),
            "by_state": project_health_state_counts(&jobs),
            "attention": attention,
        },
        "recent_runs": recent_runs,
    });
    let report = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    write_private_atomic(path, &report).map_err(|e| e.to_string())
}
