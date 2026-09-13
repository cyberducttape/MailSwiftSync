use crate::{
    core, needs_operator_review, plan_snapshot_sha256, project_health_state_counts,
    write_private_atomic,
};
use std::path::Path;

pub(crate) fn export_health(
    store: &core::StateStore,
    project_id: &str,
    path: &Path,
) -> Result<(), String> {
    let project = store
        .project(project_id)
        .map_err(|e| e.to_string())?
        .ok_or("The durable migration project no longer exists.")?;
    let jobs = store.mailboxes(project_id).map_err(|e| e.to_string())?;
    let runs = store
        .recent_runs(project_id, 20)
        .map_err(|e| e.to_string())?;
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
