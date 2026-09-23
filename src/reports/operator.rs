use crate::atomic_artifact::write_private_atomic;
use crate::migration_plan::decode_report_run_snapshot;
use crate::{
    core, evidence_digest, markdown_escape, needs_operator_review, plan_snapshot_sha256,
    project_health_state_counts, with_proof_digest,
};
use std::path::Path;

fn optional_count(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".into(), |count| count.to_string())
}

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
        markdown_escape(&project.id),
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
                markdown_escape(&job.state),
                markdown_escape(reason_label),
                markdown_escape(recommended_action),
                markdown_escape(&acceptance_summary),
                markdown_escape(&evidence_run_id),
                evidence.evidence_level(),
                markdown_escape(&evidence_digest(
                    &evidence_run_id,
                    &plan_snapshot,
                    &evidence
                )),
                evidence.source_messages,
                evidence.destination_messages,
                optional_count(evidence.unmatched_messages),
                evidence.failed_messages,
            ));
        } else {
            report.push_str(&format!(
                "| {} | {} | `{}` | {} | {} | {} | — | missing | — | — | — | — | — |\n",
                markdown_escape(&job.source_mailbox),
                markdown_escape(&job.destination_mailbox),
                markdown_escape(&job.state),
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
            markdown_escape(&run.id),
            markdown_escape(&run.engine),
            markdown_escape(&engine_version),
            markdown_escape(&run.phase_at_start),
            markdown_escape(&run.status),
            markdown_escape(&plan_snapshot_sha256(&run.plan_snapshot)),
            markdown_escape(&run.started_at),
            markdown_escape(run.finished_at.as_deref().unwrap_or("in progress")),
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
                            "verification_level": evidence.verification_level(),
                            "evidence_level": evidence.evidence_level(),
                            "reason": evidence.verification_reason(),
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

pub(crate) fn build_verification_report(
    store: &core::StateStore,
    project_id: &str,
    job_id: &str,
) -> Result<String, String> {
    let snapshot = store
        .project_report_snapshot(project_id)
        .map_err(|e| e.to_string())?
        .ok_or("The mailbox project no longer exists.")?;
    let mailbox = snapshot
        .mailboxes
        .iter()
        .find(|mailbox| mailbox.job.id == job_id)
        .ok_or("The mailbox job no longer exists.")?;
    let (evidence_run_id, evidence, plan_snapshot) = mailbox
        .evidence
        .clone()
        .ok_or("No mailbox evidence is available yet.")?;
    let plan_snapshot = plan_snapshot.ok_or("The evidence run no longer exists.")?;
    let run = snapshot
        .runs
        .iter()
        .find(|run| run.run.id == evidence_run_id)
        .map(|run| run.run.clone())
        .ok_or("The evidence refers to a run that is no longer available.")?;
    let snapshot_profile = decode_report_run_snapshot(&run.plan_snapshot)?.map(|run| run.profile);
    let source_endpoint = snapshot_profile
        .as_ref()
        .map(|profile| profile.source_host.as_str())
        .unwrap_or(snapshot.project.source_endpoint.as_str());
    let destination_endpoint = snapshot_profile
        .as_ref()
        .map(|profile| profile.destination_host.as_str())
        .unwrap_or(snapshot.project.destination_endpoint.as_str());
    let source_identity = snapshot_profile
        .as_ref()
        .map(|profile| profile.source_user.as_str())
        .unwrap_or(mailbox.job.source_mailbox.as_str());
    let destination_identity = snapshot_profile
        .as_ref()
        .map(|profile| profile.destination_user.as_str())
        .unwrap_or(mailbox.job.destination_mailbox.as_str());
    let identity_note = if snapshot_profile.is_some() {
        "run snapshot"
    } else {
        "durable project/mailbox fallback (legacy run snapshot unavailable)"
    };
    let evidence_reference = evidence_digest(&run.id, &plan_snapshot, &evidence);
    let mut report = format!(
        "# MailSwiftSync verification report\n\n- Project: {}\n- Source endpoint: {}\n- Destination endpoint: {}\n- Source mailbox: {}\n- Destination mailbox: {}\n- Identity source: {}\n- Engine: {}\n- Run ID: `{}`\n- Run status: `{}`\n- Started: `{}`\n- Finished: `{}`\n- Mailbox state: `{}`\n- Evidence level: `{}`\n- Evidence source: `{}`\n- Evidence digest: `{}`\n\n## Execution plan snapshot\n\nThe snapshot excludes session passwords and raw extra-option values. It retains an SHA-256 digest for expert-option identity without copying those values into the ledger or report.\n\n```toml\n{}\n```\n\n| Metric | Source | Destination |\n|---|---:|---:|\n| Folders | {} | {} |\n| Messages | {} | {} |\n| Virtual size | {} | {} |\n| Unmatched messages | {} | — |\n| Failed messages | {} | — |\n\nThis report distinguishes engine-confirmed output from aggregate reconciliation. Neither is independent message-level proof; provider-specific warnings and deeper verification require additional review.",
        markdown_escape(&snapshot.project.name),
        markdown_escape(source_endpoint),
        markdown_escape(destination_endpoint),
        markdown_escape(source_identity),
        markdown_escape(destination_identity),
        identity_note,
        markdown_escape(&run.engine),
        markdown_escape(&run.id),
        markdown_escape(&run.status),
        markdown_escape(&run.started_at),
        markdown_escape(run.finished_at.as_deref().unwrap_or("in progress")),
        markdown_escape(&mailbox.job.state),
        evidence.evidence_level(),
        match evidence.evidence_scope() {
            core::EvidenceScope::EngineConfirmed => "engine-confirmed summary",
            core::EvidenceScope::AggregateReconciled => "aggregate mailbox totals",
        },
        markdown_escape(&evidence_reference),
        run.plan_snapshot.replace("```", "` ``"),
        evidence.source_folders,
        evidence.destination_folders,
        evidence.source_messages,
        evidence.destination_messages,
        evidence.source_bytes,
        evidence.destination_bytes,
        optional_count(evidence.unmatched_messages),
        evidence.failed_messages,
    );
    if let Some(index) = report.find("- Run ID:") {
        report.insert_str(
            index,
            &format!(
                "- Project phase at run start: `{}`\n",
                markdown_escape(&run.phase_at_start)
            ),
        );
    }
    Ok(report)
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
