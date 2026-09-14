//! Execution-controller domain types.

mod batch;
pub(crate) mod batch_admission;
mod batch_completion;
mod batch_worker;
pub(crate) mod failure;
mod orchestrator;
mod preflight;
mod run;
mod single_admission;

pub(crate) use batch::{
    BatchExecutionMode, BatchStartContext, BatchStartDecision, BulkConfirmationSummary,
    BulkQueueSummary, BulkRetryScope, BulkStateSet, batch_mailbox_state, batch_start_decision,
    is_verified_terminal_state, selected_batch_indices, suggested_batch_project_name,
};
pub(crate) use batch_admission::{
    admit_batch_run, durable_batch_profile_config, durable_single_identity_matches,
    prepare_batch_project, prepare_batch_run, prepare_selected_batch_jobs,
};
pub(crate) use batch_completion::{BatchChildCompletion, finish_batch_child};
pub(crate) use batch_worker::spawn_batch_worker;
pub(crate) use orchestrator::{SingleRunWorkerSpec, spawn_single_run_worker};
pub(crate) use preflight::assess_plan;
pub(crate) use run::{ActiveRunContext, LiveAuthProof, RunKind};
pub(crate) use single_admission::{
    SingleRunAdmission, SingleStartContext, SingleStartDecision, admit_single_run,
    single_start_decision,
};
