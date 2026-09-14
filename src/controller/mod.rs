//! Execution-controller domain types.

mod batch;
pub(crate) mod batch_admission;
mod batch_worker;
pub(crate) mod failure;
mod orchestrator;
mod preflight;
mod run;

pub(crate) use batch::{
    BatchExecutionMode, BulkConfirmationSummary, BulkQueueSummary, BulkRetryScope, BulkStateSet,
    is_verified_terminal_state, selected_batch_indices, suggested_batch_project_name,
};
pub(crate) use batch_admission::{durable_single_identity_matches, prepare_batch_project};
pub(crate) use batch_worker::spawn_batch_worker;
pub(crate) use orchestrator::{SingleRunWorkerSpec, spawn_single_run_worker};
pub(crate) use preflight::assess_plan;
pub(crate) use run::{ActiveRunContext, LiveAuthProof, RunKind};
