//! Durable admission for a single mailbox run.

use super::run::{ActiveRunContext, RunKind};
use crate::core;

pub(crate) struct SingleRunAdmission {
    pub(crate) project_id: String,
    pub(crate) job_id: String,
    pub(crate) run_id: String,
    pub(crate) engine: core::Engine,
    pub(crate) dry_run: bool,
    pub(crate) plan_fingerprint: String,
    pub(crate) credential_fingerprint: String,
    pub(crate) plan_snapshot: String,
}

/// Commit a single run snapshot and construct the matching ownership context.
/// The UI may still prepare engine resources, but it does not decide how a
/// durable run and its process-event identity fit together.
pub(crate) fn admit_single_run(
    store: &core::StateStore,
    admission: SingleRunAdmission,
) -> Result<ActiveRunContext, String> {
    store
        .begin_run_with_snapshot(
            &admission.project_id,
            &admission.job_id,
            &admission.run_id,
            admission.engine.label(),
            &admission.plan_snapshot,
        )
        .map_err(|error| format!("Could not record durable run; nothing was started: {error}"))?;
    Ok(ActiveRunContext {
        run_id: admission.run_id,
        project_id: admission.project_id,
        job_id: Some(admission.job_id),
        batch_job_ids: Vec::new(),
        batch_child_run_ids: Vec::new(),
        batch_child_indices: std::collections::HashMap::new(),
        batch_plan_fingerprints: Vec::new(),
        kind: RunKind::Single,
        dry_run: admission.dry_run,
        engine: admission.engine,
        plan_fingerprint: admission.plan_fingerprint,
        credential_fingerprint: admission.credential_fingerprint,
    })
}
