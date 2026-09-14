use serde::Serialize;

use super::Phase;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub source_endpoint: String,
    pub destination_endpoint: String,
    pub phase: Phase,
}

/// Compact project row for workspace selection. It intentionally omits
/// mailbox configuration and plan snapshots so switching customers never
/// loads secrets or large historical plans into the UI shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectListItem {
    pub id: String,
    pub name: String,
    pub source_endpoint: String,
    pub destination_endpoint: String,
    pub phase: Phase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxJob {
    pub id: String,
    pub source_mailbox: String,
    pub destination_mailbox: String,
    pub state: String,
    /// Secret-free serialized configuration, if the importer supplied one.
    pub config: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MailboxStateCounts {
    pub total: usize,
    pub ready: usize,
    pub running: usize,
    pub verified: usize,
    pub needs_review: usize,
}

/// Immutable durable facts needed to admit a selected batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchAdmissionState {
    pub job_id: String,
    pub state: String,
    pub preflight_plan: Option<String>,
    pub checkpoint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub id: String,
    pub job_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub engine: String,
    pub phase_at_start: String,
    pub plan_snapshot: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub detail: String,
}

/// Lightweight run row for activity views and health summaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunListItem {
    pub id: String,
    pub job_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub source_mailbox: Option<String>,
    pub destination_mailbox: Option<String>,
    pub engine: String,
    pub phase_at_start: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub detail: String,
}

/// Immutable per-mailbox metadata supplied when a batch wave is admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchChildPlan {
    pub engine: String,
    pub plan_snapshot: String,
    pub engine_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActiveProcess {
    pub run_id: String,
    pub job_id: String,
    pub pid: u32,
    pub start_ticks: Option<u64>,
    pub process_group: Option<u32>,
    pub session_id: Option<u32>,
    pub executable: String,
}
