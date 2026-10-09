//! Execution events exchanged between controller workers and the shell.

use super::run::ActiveRunContext;
use crate::core;
use std::{collections::HashSet, sync::mpsc};

/// A structured durable event waiting for the next controller commit. Named
/// fields make the project/run/detail relationship explicit while poll()
/// batches events around terminal state transitions.
#[derive(Clone, Debug)]
pub(crate) struct PendingDbEvent {
    pub(crate) run_id: String,
    pub(crate) kind: String,
    pub(crate) detail: String,
}

impl PendingDbEvent {
    pub(crate) fn new(run_id: String, kind: String, detail: String) -> Self {
        Self {
            run_id,
            kind,
            detail,
        }
    }
}

/// Persist structured execution events without exposing SQLite's tuple-shaped
/// write API to the event reducer. Raw engine output is never accepted here;
/// callers enqueue only classified, bounded durable details.
pub(crate) fn persist_pending_events(
    store: &core::StateStore,
    events: &[PendingDbEvent],
) -> rusqlite::Result<()> {
    let batch = events
        .iter()
        .map(|event| {
            (
                event.run_id.as_str(),
                event.kind.as_str(),
                event.detail.as_str(),
            )
        })
        .collect::<Vec<_>>();
    store.record_events_for_runs_batch(&batch)
}

/// Presentation output is accepted only from the currently owned process
/// and only until that process has emitted its terminal event.
pub(crate) fn run_line_is_current(
    active_run: Option<&ActiveRunContext>,
    ended_processes: &HashSet<(String, String)>,
    run_id: &str,
    job_id: &str,
) -> bool {
    active_run.is_some_and(|run| run.owns_line(run_id, job_id))
        && !ended_processes.contains(&(run_id.to_owned(), job_id.to_owned()))
}

/// Process lifecycle metadata is accepted only for a process owned by the
/// active single run or one of its batch children.
pub(crate) fn process_event_is_current(
    active_run: Option<&ActiveRunContext>,
    run_id: &str,
    job_id: &str,
) -> bool {
    active_run.is_some_and(|run| run.owns_process(run_id, job_id))
}

pub(crate) enum Event {
    Line(String),
    DiagnosticLinesDropped {
        run_id: String,
        job_id: String,
        count: u64,
    },
    TransferAttempt {
        run_id: String,
        job_id: String,
        attempt: u32,
        status: TransferAttemptStatus,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    RunLine {
        run_id: String,
        job_id: String,
        text: String,
    },
    EngineVersion {
        run_id: String,
        job_id: String,
        version: String,
        reply: mpsc::SyncSender<Result<(), String>>,
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
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    BatchEvidence {
        job_id: String,
        child_run_id: String,
        evidence: core::MailboxEvidence,
        mismatches: Vec<core::MessageMismatch>,
    },
    Checkpoint {
        run_id: String,
        job_id: String,
        value: String,
    },
    Evidence(core::MailboxEvidence),
    MessageMismatches {
        run_id: String,
        job_id: String,
        mismatches: Vec<core::MessageMismatch>,
    },
    VerificationFailed(String),
    /// Cumulative, content-free engine progress for one process. Snapshots
    /// are throttled and may be dropped; each one supersedes the last.
    Progress {
        run_id: String,
        job_id: String,
        progress: crate::progress::TransferProgress,
    },
    /// Content-free progress checkpoint retained in the durable event log.
    /// This is a bounded observation of engine output, not per-message proof.
    TransferProgressCheckpoint {
        run_id: String,
        job_id: String,
        attempt: u32,
        progress: crate::progress::TransferProgress,
    },
    /// A batch mailbox will be retried after `delay`.
    RetryScheduled {
        job_id: String,
        attempt: u32,
        max_retries: u32,
        delay: std::time::Duration,
        failure_class: &'static str,
    },
    /// The rate-domain limiter is holding launches for one domain (a
    /// mailbox, credential, tenant, provider, or all providers).
    ProviderCooldown {
        domain: String,
        /// Canonical provider (`global` when every provider is paused).
        provider: &'static str,
        /// `host:port` of the paused domain; `None` for the global domain.
        endpoint: Option<String>,
        until: std::time::Instant,
        /// Adaptive concurrency after this capacity event.
        current_limit: usize,
        /// Organization/batch ceiling for this domain.
        configured_limit: usize,
        /// Consecutive capacity failures in the current escalation episode.
        consecutive_failures: u8,
    },
    /// Verification results for the transfer attempt they examined.
    TransferPassVerified {
        run_id: String,
        job_id: String,
        attempt: u32,
        method: String,
        outcome: String,
        folders: Vec<core::TransferPassFolder>,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    Finished(Result<StreamOutcome, String>),
}

/// A durable transfer-attempt boundary, with the provenance that belongs to
/// it: what the engine was asked to do at start, what it reported at finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TransferAttemptStatus {
    Started(Box<core::TransferPassIntent>),
    Finished {
        outcome: TransferAttemptOutcome,
        completion: core::TransferPassCompletion,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransferAttemptOutcome {
    Completed,
    DeltaRequired,
    Failed { failure_class: &'static str },
}

impl TransferAttemptOutcome {
    pub(crate) fn durable_outcome(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::DeltaRequired => "delta_required",
            Self::Failed { failure_class } => match failure_class {
                "authentication" => "failed:authentication",
                "capacity" => "failed:capacity",
                "transport" => "failed:transport",
                "quota" => "failed:quota",
                "message" => "failed:message",
                "verification" => "failed:verification",
                "cancelled" => "failed:cancelled",
                "configuration" => "failed:configuration",
                "unknown" => "failed:unknown",
                _ => "failed:other",
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamOutcome {
    Completed,
    DeltaRequired,
}

#[cfg(test)]
mod tests {
    use super::{TransferAttemptOutcome, process_event_is_current, run_line_is_current};
    use crate::controller::run::{ActiveRunContext, RunKind};
    use std::collections::{HashMap, HashSet};

    fn single_context() -> ActiveRunContext {
        ActiveRunContext {
            run_id: "run-a".into(),
            project_id: "project-a".into(),
            job_id: Some("job-a".into()),
            batch_job_ids: Vec::new(),
            batch_child_run_ids: Vec::new(),
            batch_child_indices: HashMap::new(),
            batch_plan_fingerprints: Vec::new(),
            kind: RunKind::Single,
            dry_run: true,
            plan_fingerprint: "plan-a".into(),
            credential_fingerprint: "credential-a".into(),
            source_provider: "generic".into(),
            destination_provider: "generic".into(),
        }
    }

    #[test]
    fn run_line_policy_rejects_foreign_and_ended_processes() {
        let context = single_context();
        let ended = HashSet::new();
        assert!(run_line_is_current(
            Some(&context),
            &ended,
            "run-a",
            "job-a"
        ));
        assert!(!run_line_is_current(
            Some(&context),
            &ended,
            "run-other",
            "job-a"
        ));

        let ended = HashSet::from([("run-a".into(), "job-a".into())]);
        assert!(!run_line_is_current(
            Some(&context),
            &ended,
            "run-a",
            "job-a"
        ));
        assert!(!run_line_is_current(None, &ended, "run-a", "job-a"));
        assert!(process_event_is_current(Some(&context), "run-a", "job-a"));
        assert!(!process_event_is_current(
            Some(&context),
            "run-other",
            "job-a"
        ));
    }

    #[test]
    fn transfer_attempt_status_is_bounded_and_excludes_engine_output() {
        assert_eq!(
            TransferAttemptOutcome::Failed {
                failure_class: "capacity",
            }
            .durable_outcome(),
            "failed:capacity"
        );
        assert_eq!(
            TransferAttemptOutcome::Failed {
                failure_class: "arbitrary raw engine output",
            }
            .durable_outcome(),
            "failed:other"
        );
    }
}
