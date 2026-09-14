//! Durable completion policy for batch child runs.

use super::batch::{batch_mailbox_state, batch_run_status};
use super::run::ActiveRunContext;
use crate::core;

pub(crate) struct BatchChildCompletion<'a> {
    pub(crate) job_id: &'a str,
    pub(crate) child_run_id: &'a str,
    pub(crate) state: &'a str,
    pub(crate) detail: &'a str,
    pub(crate) evidence: Option<&'a core::MailboxEvidence>,
    pub(crate) checkpoint: Option<&'a str>,
}

/// Commit one batch child's terminal state, evidence, preflight result, and
/// checkpoint through the durable-core transaction boundary.
pub(crate) fn finish_batch_child(
    store: &core::StateStore,
    run: &ActiveRunContext,
    completion: BatchChildCompletion<'_>,
) -> rusqlite::Result<()> {
    let Some(index) = run.batch_child_index(completion.job_id, completion.child_run_id) else {
        return Err(rusqlite::Error::InvalidQuery);
    };
    let run_status = batch_run_status(completion.state);
    let final_state = batch_mailbox_state(completion.state, completion.evidence);
    let preflight_plan = if run.dry_run && completion.state == "ready" {
        run.batch_plan_fingerprints.get(index).map(String::as_str)
    } else {
        None
    };
    let checkpoint = completion.checkpoint.filter(|_| run_status == "completed");
    if let Some(evidence) = completion.evidence {
        store.finish_run_for_mailbox_with_evidence_and_preflight_plan_and_checkpoint(
            &run.project_id,
            completion.job_id,
            completion.child_run_id,
            run_status,
            &final_state,
            completion.detail,
            evidence,
            preflight_plan,
            checkpoint,
        )
    } else {
        store.finish_run_for_mailbox_with_preflight_plan_and_checkpoint(
            &run.project_id,
            completion.job_id,
            completion.child_run_id,
            run_status,
            &final_state,
            completion.detail,
            preflight_plan,
            checkpoint,
        )
    }
}
