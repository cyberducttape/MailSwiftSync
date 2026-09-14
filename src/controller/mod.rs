//! Execution-controller domain types.

mod batch;
pub(crate) mod batch_admission;
mod batch_completion;
mod batch_worker;
mod events;
pub(crate) mod failure;
mod orchestrator;
mod preflight;
pub(crate) mod probe;
mod run;
mod single_admission;
mod single_start;

pub(crate) use batch::{
    BatchExecutionMode, BatchStartContext, BatchStartDecision, BulkConfirmationSummary,
    BulkQueueSummary, BulkRetryScope, BulkStateSet, batch_mailbox_state, batch_start_decision,
    is_verified_terminal_state,
};
pub(crate) use batch_admission::{
    BatchLaunchRequest, admit_batch_launch, decode_persisted_batch_profile,
    durable_single_identity_matches,
};
pub(crate) use batch_completion::{BatchChildCompletion, finish_batch_child};
pub(crate) use batch_worker::launch_batch_worker;
pub(crate) use events::{
    Event, PendingDbEvent, StreamOutcome, persist_pending_events, process_event_is_current,
    run_line_is_current,
};
pub(crate) use orchestrator::{SingleRunWorkerSpec, spawn_single_run_worker};
pub(crate) use preflight::{
    CapabilityProbeResult, assess_plan, capability_observation_matches,
    capability_probe_result_matches,
};
pub(crate) use probe::{
    CapabilityProbeSpec, ImapProbeEndpoint, LiveAuthProbeSpec, spawn_capability_probe,
    spawn_live_auth_probe,
};
pub(crate) use run::{ActiveRunContext, LiveAuthProof, RunKind};
pub(crate) use single_admission::{
    SingleRunAdmission, SingleStartContext, SingleStartDecision, admit_single_run,
    single_start_decision,
};
