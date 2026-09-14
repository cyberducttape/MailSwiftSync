//! Execution events exchanged between controller workers and the shell.

use crate::core;
use std::sync::mpsc;

pub(crate) enum Event {
    Line(String),
    RunLine {
        run_id: String,
        job_id: String,
        text: String,
    },
    EngineVersion {
        run_id: String,
        job_id: String,
        version: String,
    },
    ProcessStarted(
        String,
        String,
        u32,
        Option<u64>,
        Option<u32>,
        Option<u32>,
        String,
        mpsc::SyncSender<Result<(), String>>,
    ),
    ProcessEnded {
        run_id: String,
        job_id: String,
    },
    ClaimBatch {
        project_id: String,
        job_id: String,
        parent_run_id: String,
        child_run_id: String,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    JobState {
        job_id: String,
        child_run_id: String,
        state: String,
    },
    JobFinished {
        job_id: String,
        child_run_id: String,
        state: String,
        detail: String,
        credential_fingerprint: Option<String>,
    },
    BatchEvidence {
        job_id: String,
        child_run_id: String,
        evidence: core::MailboxEvidence,
    },
    Checkpoint {
        run_id: String,
        job_id: String,
        value: String,
    },
    Evidence(core::MailboxEvidence),
    VerificationFailed(String),
    Finished(Result<StreamOutcome, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamOutcome {
    Completed,
    DeltaRequired,
}
