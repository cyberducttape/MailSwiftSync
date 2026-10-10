use crate::atomic_artifact::write_private_atomic;
use crate::credentials::SecretString;
use crate::maintenance_window::MaintenanceWindow;
use crate::{
    App, BatchExecutionMode, BulkRetryScope, cleanup_stale_secret_directories, core,
    plan_fingerprint_digest, recorded_process_is_gone, recorded_process_matches,
    secret_runtime_base, terminate_recorded_process_group,
};
use serde::Serialize;
use std::{collections::HashSet, sync::atomic::Ordering, thread, time::Duration};

const SUPPORT_MAILBOX_SAMPLE_LIMIT: u32 = 1_000;
const MAX_SUPPORT_MAILBOX_ROWS_TOTAL: usize = 100_000;
const MAX_HEADLESS_DETAIL_MAILBOXES: u32 = 10_000;
const MAX_HEADLESS_DETAIL_MAILBOXES_TOTAL: usize = 100_000;
const MAX_HEADLESS_ACTIVE_PROCESSES: u32 = 1_000;

/// Versioned automation output contracts. `format_version` changes only for
/// breaking changes (removed, renamed, or retyped fields); additive fields keep
/// the version. `schema_version` remains the SQLite ledger schema and is not a
/// compatibility signal for callers. See docs/automation-contract.md.
pub(crate) const STATUS_FORMAT: &str = "mailswiftsync-status";
pub(crate) const STATUS_SUMMARY_FORMAT: &str = "mailswiftsync-status-summary";
pub(crate) const FLEET_STATUS_FORMAT: &str = "mailswiftsync-fleet-status";
pub(crate) const AUTOMATION_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub(crate) struct HeadlessStatus {
    pub(crate) format: &'static str,
    pub(crate) format_version: u32,
    pub(crate) schema_version: i64,
    pub(crate) active_processes: Vec<core::ActiveProcess>,
    pub(crate) active_processes_truncated: bool,
    pub(crate) returned_projects: usize,
    pub(crate) total_projects: usize,
    pub(crate) projects_truncated: bool,
    pub(crate) projects: Vec<HeadlessProjectStatus>,
}

#[derive(Debug, Serialize)]
pub(crate) struct HeadlessProjectStatus {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) batch: bool,
    pub(crate) source_endpoint: String,
    pub(crate) destination_endpoint: String,
    pub(crate) phase: String,
    pub(crate) mailboxes_truncated: bool,
    pub(crate) mailboxes: Vec<HeadlessMailboxStatus>,
}

#[derive(Debug, Serialize)]
pub(crate) struct HeadlessMailboxStatus {
    pub(crate) id: String,
    pub(crate) source_mailbox: String,
    pub(crate) destination_mailbox: String,
    pub(crate) state: String,
    pub(crate) attention_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct HeadlessStatusSummary {
    pub(crate) format: &'static str,
    pub(crate) format_version: u32,
    pub(crate) schema_version: i64,
    pub(crate) active_processes: Vec<core::ActiveProcess>,
    pub(crate) active_processes_truncated: bool,
    pub(crate) returned_projects: usize,
    pub(crate) total_projects: usize,
    pub(crate) projects_truncated: bool,
    pub(crate) aggregate_mailbox_state_counts: core::MailboxStateCounts,
    pub(crate) projects: Vec<HeadlessProjectSummary>,
}

#[derive(Debug, Serialize)]
pub(crate) struct HeadlessProjectSummary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) batch: bool,
    pub(crate) source_endpoint: String,
    pub(crate) destination_endpoint: String,
    pub(crate) phase: String,
    pub(crate) mailbox_state_counts: core::MailboxStateCounts,
    pub(crate) attention_reason_counts: std::collections::BTreeMap<String, usize>,
}

pub(crate) struct HeadlessCredentials {
    pub(crate) source: SecretString,
    pub(crate) destination: SecretString,
}

/// Build a support artifact from durable state without including anything
/// that could disclose credentials, mailbox content, or internal topology.
pub(crate) fn export_support_bundle(
    state_path: &std::path::Path,
    output_path: &std::path::Path,
) -> Result<(), String> {
    export_support_bundle_with_sample_limit(state_path, output_path, SUPPORT_MAILBOX_SAMPLE_LIMIT)
}

pub(crate) fn export_support_bundle_with_sample_limit(
    state_path: &std::path::Path,
    output_path: &std::path::Path,
    sample_limit: u32,
) -> Result<(), String> {
    let store = core::StateStore::open_readonly(state_path).map_err(|error| error.to_string())?;
    let sample_limit = sample_limit.min(SUPPORT_MAILBOX_SAMPLE_LIMIT);
    let projects = store
        .recent_projects(1_000)
        .map_err(|error| error.to_string())?;
    let mut project_values = Vec::with_capacity(projects.len());
    let mut remaining_mailbox_rows = MAX_SUPPORT_MAILBOX_ROWS_TOTAL;
    for project in projects {
        let counts = store
            .mailbox_state_counts(&project.id)
            .map_err(|error| error.to_string())?;
        let jobs = store
            .mailbox_status_page(
                &project.id,
                None,
                sample_limit.min(remaining_mailbox_rows.try_into().unwrap_or(u32::MAX)),
            )
            .map_err(|error| error.to_string())?;
        remaining_mailbox_rows = remaining_mailbox_rows.saturating_sub(jobs.rows.len());
        let sampled_mailbox_count = jobs.rows.len();
        let mailbox_values = jobs
            .rows
            .iter()
            .map(|(job, attention_reason)| {
                Ok(serde_json::json!({
                    "id": job.id,
                    "state": job.state,
                    "attention_reason": attention_reason.as_ref().map(|reason| reason.as_str()),
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let runs = store
            .recent_run_list(&project.id, 100)
            .map_err(|error| error.to_string())?;
        let run_values = runs
            .into_iter()
            .map(|run| {
                Ok(serde_json::json!({
                    "run_id": run.id,
                    "job_id": run.job_id,
                    "parent_run_id": run.parent_run_id,
                    "engine": run.engine,
                    "engine_version": store.engine_version(&run.id)
                        .map_err(|error| error.to_string())?,
                    "phase_at_start": run.phase_at_start,
                    "status": run.status,
                    "started_at": run.started_at,
                    "finished_at": run.finished_at,
                    "has_detail": !run.detail.is_empty(),
                    // Digests and classifications only: the launched
                    // command names endpoints and users, so it stays out.
                    "transfer_passes": store
                        .transfer_passes(&run.id)
                        .map_err(|error| error.to_string())?
                        .into_iter()
                        .map(|pass| serde_json::json!({
                            "attempt": pass.attempt,
                            "pass_sequence": pass.pass_sequence,
                            "pass_kind": pass.pass_kind,
                            "command_sha256": pass.command_sha256,
                            "folder_scope": pass.folder_scope,
                            "source_range": pass.source_range,
                            "outcome": pass.outcome,
                            "delta_required": pass.delta_required,
                            "verification_method": pass.verification_method,
                            "verification_outcome": pass.verification_outcome,
                            "verified_folders": pass.folders.len(),
                            "incomplete_folders": pass.folders.iter().filter(|folder| !folder.complete).count(),
                        }))
                        .collect::<Vec<_>>(),
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
        project_values.push(serde_json::json!({
            "project_id": project.id,
            "phase": project.phase.as_str(),
            "mailbox_count": counts.total,
            "mailbox_sample_limit": sample_limit,
            "mailboxes_truncated": counts.total > sampled_mailbox_count,
            "mailbox_state_counts": {
                "ready": counts.ready,
                "running": counts.running,
                "verified": counts.verified,
                "needs_review": counts.needs_review,
            },
            "mailboxes": mailbox_values,
            "recent_runs": run_values,
        }));
    }
    let (processes, active_processes_truncated) = store
        .active_processes_page(MAX_HEADLESS_ACTIVE_PROCESSES)
        .map_err(|error| error.to_string())?;
    let active_processes = processes
        .into_iter()
        .map(|process| {
            serde_json::json!({
                "run_id": process.run_id,
                "job_id": process.job_id,
                "pid": process.pid,
            })
        })
        .collect::<Vec<_>>();
    let value = serde_json::json!({
        "format": "mailswiftsync-support-bundle",
        "format_version": 1,
        "application_version": env!("CARGO_PKG_VERSION"),
        "schema_version": core::CURRENT_SCHEMA_VERSION,
        "platform": {
            "os": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
        },
        "active_processes": active_processes,
        "active_processes_truncated": active_processes_truncated,
        "projects": project_values,
        "redaction": {
            "endpoints": "excluded",
            "credentials": "excluded",
            "project_names": "excluded",
            "plan_snapshots": "excluded",
            "command_paths": "excluded",
            "transfer_commands": "digest_only",
            "mail_content": "excluded",
            "diagnostic_text": "excluded",
        },
        "note": "This bundle is intended for support and incident triage. It contains durable state classifications and run metadata, not a forensic log or migration proof."
    });
    let report = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    write_private_atomic(output_path, &report).map_err(|error| error.to_string())
}

#[derive(Debug, Serialize)]
pub(crate) struct HeadlessRecoveryResult {
    pub(crate) recovered_jobs: usize,
    pub(crate) preserved_processes: usize,
}

pub(crate) fn headless_status(
    state_path: &std::path::Path,
    selected_project_id: Option<&str>,
) -> Result<HeadlessStatus, String> {
    let store = core::StateStore::open_readonly(state_path).map_err(|error| error.to_string())?;
    let total_projects = if let Some(project_id) = selected_project_id {
        usize::from(
            store
                .project(project_id)
                .map_err(|error| error.to_string())?
                .is_some(),
        )
    } else {
        store.project_count().map_err(|error| error.to_string())?
    };
    let projects = if let Some(project_id) = selected_project_id {
        store
            .project(project_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        store
            .recent_projects(1_000)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|project| core::Project {
                id: project.id,
                name: project.name,
                source_endpoint: project.source_endpoint,
                destination_endpoint: project.destination_endpoint,
                phase: project.phase,
            })
            .collect::<Vec<_>>()
    };
    let mut result = Vec::with_capacity(projects.len());
    let mut remaining_mailbox_rows = MAX_HEADLESS_DETAIL_MAILBOXES_TOTAL;
    for project in projects {
        let mailbox_count = store
            .mailbox_state_counts(&project.id)
            .map_err(|error| error.to_string())?
            .total;
        let batch = store
            .project_has_mailbox_configs(&project.id)
            .map_err(|error| error.to_string())?;
        let mut mailboxes = Vec::new();
        let mailbox_limit = remaining_mailbox_rows
            .min(MAX_HEADLESS_DETAIL_MAILBOXES as usize)
            .try_into()
            .unwrap_or(0);
        for (mailbox, attention_reason) in store
            .mailbox_status_page(&project.id, None, mailbox_limit)
            .map_err(|error| error.to_string())?
            .rows
        {
            mailboxes.push(HeadlessMailboxStatus {
                attention_reason: attention_reason.map(|reason| reason.as_str().to_owned()),
                id: mailbox.id,
                source_mailbox: mailbox.source_mailbox,
                destination_mailbox: mailbox.destination_mailbox,
                state: mailbox.state,
            });
        }
        remaining_mailbox_rows = remaining_mailbox_rows.saturating_sub(mailboxes.len());
        result.push(HeadlessProjectStatus {
            id: project.id,
            name: project.name,
            batch,
            source_endpoint: project.source_endpoint,
            destination_endpoint: project.destination_endpoint,
            phase: project.phase.as_str().to_owned(),
            mailboxes_truncated: mailbox_count > mailbox_limit as usize,
            mailboxes,
        });
    }
    let (active_processes, active_processes_truncated) = store
        .active_processes_page(MAX_HEADLESS_ACTIVE_PROCESSES)
        .map_err(|error| error.to_string())?;
    Ok(HeadlessStatus {
        format: STATUS_FORMAT,
        format_version: AUTOMATION_FORMAT_VERSION,
        schema_version: core::CURRENT_SCHEMA_VERSION,
        active_processes,
        active_processes_truncated,
        returned_projects: result.len(),
        total_projects,
        projects_truncated: result.len() < total_projects,
        projects: result,
    })
}

/// Emit a bounded status projection for large ledgers. Unlike detailed
/// `status`, this path never loads individual mailbox rows; state counts are
/// computed by SQLite and remain exact.
pub(crate) fn headless_status_summary(
    state_path: &std::path::Path,
    selected_project_id: Option<&str>,
) -> Result<HeadlessStatusSummary, String> {
    let store = core::StateStore::open_readonly(state_path).map_err(|error| error.to_string())?;
    let total_projects = if let Some(project_id) = selected_project_id {
        usize::from(
            store
                .project(project_id)
                .map_err(|error| error.to_string())?
                .is_some(),
        )
    } else {
        store.project_count().map_err(|error| error.to_string())?
    };
    let aggregate_mailbox_state_counts = if let Some(project_id) = selected_project_id {
        store.mailbox_state_counts(project_id)
    } else {
        store.all_mailbox_state_counts()
    }
    .map_err(|error| error.to_string())?;
    let projects = if let Some(project_id) = selected_project_id {
        store
            .project(project_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        store
            .recent_projects(1_000)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|project| core::Project {
                id: project.id,
                name: project.name,
                source_endpoint: project.source_endpoint,
                destination_endpoint: project.destination_endpoint,
                phase: project.phase,
            })
            .collect::<Vec<_>>()
    };
    let mut summaries = Vec::with_capacity(projects.len());
    for project in projects {
        let batch = store
            .project_has_mailbox_configs(&project.id)
            .map_err(|error| error.to_string())?;
        let mailbox_state_counts = store
            .mailbox_state_counts(&project.id)
            .map_err(|error| error.to_string())?;
        let attention_reason_counts = store
            .mailbox_attention_reason_counts(&project.id)
            .map_err(|error| error.to_string())?;
        summaries.push(HeadlessProjectSummary {
            id: project.id,
            name: project.name,
            batch,
            source_endpoint: project.source_endpoint,
            destination_endpoint: project.destination_endpoint,
            phase: project.phase.as_str().to_owned(),
            mailbox_state_counts,
            attention_reason_counts,
        });
    }
    let (active_processes, active_processes_truncated) = store
        .active_processes_page(MAX_HEADLESS_ACTIVE_PROCESSES)
        .map_err(|error| error.to_string())?;
    Ok(HeadlessStatusSummary {
        format: STATUS_SUMMARY_FORMAT,
        format_version: AUTOMATION_FORMAT_VERSION,
        schema_version: core::CURRENT_SCHEMA_VERSION,
        active_processes,
        active_processes_truncated,
        returned_projects: summaries.len(),
        total_projects,
        projects_truncated: summaries.len() < total_projects,
        aggregate_mailbox_state_counts,
        projects: summaries,
    })
}

const FLEET_MAX_LEDGER_CANDIDATES: usize = 10_000;
const FLEET_MAX_SCAN_DEPTH: usize = 8;

#[derive(Debug, Serialize)]
pub(crate) struct FleetStatus {
    pub(crate) format: &'static str,
    pub(crate) format_version: u32,
    pub(crate) root: String,
    pub(crate) ledger_count: usize,
    pub(crate) totals: core::MailboxStateCounts,
    pub(crate) ledgers: Vec<FleetLedgerSummary>,
    pub(crate) unreadable: Vec<FleetUnreadableLedger>,
}

#[derive(Debug, Serialize)]
pub(crate) struct FleetLedgerSummary {
    pub(crate) path: String,
    pub(crate) summary: HeadlessStatusSummary,
}

#[derive(Debug, Serialize)]
pub(crate) struct FleetUnreadableLedger {
    pub(crate) path: String,
    pub(crate) error: String,
}

/// Find candidate MailSwiftSync ledger files under `root`. A file only
/// qualifies by its `.db` extension; anything that is not actually a
/// MailSwiftSync ledger (an unrelated SQLite file, a stray backup with a
/// different extension) fails to summarize later and is reported in
/// `unreadable` rather than silently skipped, so a misconfigured scan root
/// is visible instead of quietly under-counting. Symlinks are not followed
/// (avoids directory-cycle loops); depth and candidate count are bounded so
/// an unexpectedly large or cyclical tree fails closed instead of running
/// unbounded.
fn discover_ledger_candidates(root: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0_usize)];
    while let Some((dir, depth)) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|error| format!("could not read {}: {error}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!("could not read an entry under {}: {error}", dir.display())
            })?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
            if file_type.is_dir() {
                if depth < FLEET_MAX_SCAN_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if path.extension().is_some_and(|extension| extension == "db") {
                found.push(path);
                if found.len() > FLEET_MAX_LEDGER_CANDIDATES {
                    return Err(format!(
                        "more than {FLEET_MAX_LEDGER_CANDIDATES} candidate ledger files found under {}; narrow the scan directory",
                        root.display()
                    ));
                }
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Aggregate secret-free status across every MailSwiftSync ledger found
/// under `root`, for operators sharding a large migration across multiple
/// instances (see the "Scaling large migrations" wiki page). This is
/// read-only and never takes an instance lock, so it is safe to run
/// alongside live shards; it reuses the exact same summary `status
/// --summary` produces for each ledger it finds.
pub(crate) fn fleet_status(root: &std::path::Path) -> Result<FleetStatus, String> {
    let candidates = discover_ledger_candidates(root)?;
    let mut ledgers = Vec::new();
    let mut unreadable = Vec::new();
    let mut totals = core::MailboxStateCounts::default();
    for path in candidates {
        match headless_status_summary(&path, None) {
            Ok(summary) => {
                let aggregate = summary.aggregate_mailbox_state_counts;
                totals.total += aggregate.total;
                totals.ready += aggregate.ready;
                totals.running += aggregate.running;
                totals.verified += aggregate.verified;
                totals.needs_review += aggregate.needs_review;
                ledgers.push(FleetLedgerSummary {
                    path: path.display().to_string(),
                    summary,
                });
            }
            Err(error) => unreadable.push(FleetUnreadableLedger {
                path: path.display().to_string(),
                error,
            }),
        }
    }
    Ok(FleetStatus {
        format: FLEET_STATUS_FORMAT,
        format_version: AUTOMATION_FORMAT_VERSION,
        root: root.display().to_string(),
        ledger_count: ledgers.len(),
        totals,
        ledgers,
        unreadable,
    })
}

pub(crate) fn headless_recover(
    state_path: &std::path::Path,
) -> Result<HeadlessRecoveryResult, String> {
    let store = core::StateStore::open(state_path).map_err(|error| error.to_string())?;
    let processes = store
        .active_processes()
        .map_err(|error| error.to_string())?;
    let mut unverified = Vec::new();
    for process in &processes {
        if process.pid > 0 && recorded_process_matches(process) {
            terminate_recorded_process_group(process);
            if recorded_process_matches(process) {
                unverified.push(process.clone());
            }
        } else if recorded_process_is_gone(process) {
            // The owned process already exited (for example after the
            // Linux parent-death signal). There is nothing left to signal,
            // and retaining the stale PID would wedge recovery forever.
        } else {
            unverified.push(process.clone());
        }
    }
    let recovered_jobs = store
        .recover_abandoned_jobs_preserving(&unverified)
        .map_err(|error| error.to_string())?;
    if unverified.is_empty() {
        crate::cleanup_reconciled_secret_directories(&secret_runtime_base());
    } else {
        cleanup_stale_secret_directories(&secret_runtime_base());
    }
    Ok(HeadlessRecoveryResult {
        recovered_jobs,
        preserved_processes: unverified.len(),
    })
}

/// A failed headless run and the process exit status that reports it.
/// Automation distinguishes a pass that needs another delta (3), a
/// verification difference (4), and operator attention (5) from other
/// failures (1).
#[derive(Debug)]
pub(crate) struct HeadlessFailure {
    pub(crate) code: i32,
    pub(crate) message: String,
}

impl From<String> for HeadlessFailure {
    fn from(message: String) -> Self {
        Self { code: 1, message }
    }
}

impl From<&str> for HeadlessFailure {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

/// Exit status and description for a live run that ended unverified.
fn unverified_exit_status(state: Option<&str>) -> (i32, &'static str) {
    match state {
        Some("delta_required") => (3, "delta required (exit status 3)"),
        Some("verification_difference") => (4, "verification difference (exit status 4)"),
        Some("attention") => (5, "operator attention (exit status 5)"),
        _ => (1, "unresolved (exit status 1)"),
    }
}

/// Run the existing durable controller without constructing an egui window.
/// A live invocation always performs a fresh dry preflight first, so this
/// path cannot promote credentials or a plan left over from another process.
pub(crate) fn headless_execute_with_credentials(
    state_path: &std::path::Path,
    live: bool,
    credentials: Option<HeadlessCredentials>,
    diagnostic_log: Option<&std::path::Path>,
    reopen_reason: Option<&str>,
    acknowledge_destination_loss: bool,
) -> Result<String, HeadlessFailure> {
    // Use the same startup recovery as the GUI, but pass the ledger path as
    // data instead of mutating process-global environment state.
    let mut app =
        App::try_from_state_path(Some(state_path)).map_err(|failure| failure.to_string())?;
    if let Some(directory) = diagnostic_log {
        app.diagnostic_logger = Some(std::sync::Arc::new(crate::DiagnosticLogger::create(
            directory,
        )?));
        eprintln!(
            "WARNING: opt-in diagnostic logs are owner-only and bounded, but are plaintext and may retain provider, mailbox, and folder metadata. Review and securely remove them after troubleshooting."
        );
    }
    if let Some(credentials) = credentials {
        app.form.source_password = credentials.source;
        app.form.destination_password = credentials.destination;
    }
    if !app.persistence_available {
        return Err("durable SQLite state is unavailable; execution is blocked".into());
    }
    if !app.profile_available {
        return Err("saved migration profile is unavailable; repair it before execution".into());
    }
    if app.process_review_required {
        return Err(
            "recorded process ownership could not be verified; run `recover` and review the host before retrying"
                .into(),
        );
    }
    if !app.queue.is_empty() {
        return Err(
            "headless single-mailbox execution refuses a restored batch queue; use the GUI for batch admission or an explicit batch supervisor"
                .into(),
        );
    }

    if live {
        // The command line is the confirmation for unattended runs; a plan
        // that may remove destination state needs its own explicit flag,
        // checked before any preflight work.
        let policy = app.form.profile.destination_mutation_policy();
        eprintln!("Destination mutation policy: {}", policy.warning());
        if policy.may_remove_destination_state() && !acknowledge_destination_loss {
            return Err(HeadlessFailure::from(format!(
                "live run refused: {} Re-run with --acknowledge-destination-loss to proceed.",
                policy.warning()
            )));
        }
    }
    if live && crate::runner::automap_blocks_live_certification(&app.form) {
        return Err("headless live requires independent message-verification evidence; imapsync automapping is not replayable from an immutable mapping snapshot. Disable automap and retry".into());
    }

    if let Some(project_id) = app.project_id.clone() {
        let project = app
            .store
            .project(&project_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "headless execution project no longer exists".to_owned())?;
        if project.phase == core::Phase::Complete {
            if !live {
                return Err(
                    "completed projects are immutable; request an explicit live reopen with --reopen-reason"
                        .into(),
                );
            }
            let mailbox_count = app
                .store
                .mailbox_state_counts(&project_id)
                .map_err(|error| error.to_string())?
                .total;
            if mailbox_count != 1 {
                return Err(HeadlessFailure::from(format!(
                    "headless incremental reopen requires exactly one mailbox; durable project contains {mailbox_count}"
                )));
            }
            let reason = reopen_reason.ok_or_else(|| {
                "completed projects require --reopen-reason before another headless live pass"
                    .to_owned()
            })?;
            app.store
                .reopen_project(&project_id, reason)
                .map_err(|error| {
                    format!("could not reopen completed project for incremental sync: {error}")
                })?;
            app.refresh_ui_snapshot_now();
        } else if reopen_reason.is_some() {
            return Err("--reopen-reason is only valid when reopening a completed project".into());
        }
    } else if reopen_reason.is_some() {
        return Err("--reopen-reason requires an existing completed project".into());
    }

    app.form.dry_run = true;
    app.start();
    wait_for_headless_controller(&mut app)?;
    let project_id = app
        .project_id
        .as_deref()
        .ok_or_else(|| {
            format!(
                "preflight did not create a durable mailbox: {}",
                app.status.text
            )
        })?
        .to_owned();
    let job_id = app
        .job_id
        .as_deref()
        .ok_or_else(|| {
            format!(
                "preflight did not create a durable mailbox: {}",
                app.status.text
            )
        })?
        .to_owned();
    let mailbox_count = app
        .store
        .mailbox_state_counts(&project_id)
        .map_err(|error| error.to_string())?
        .total;
    if mailbox_count != 1 {
        return Err(HeadlessFailure::from(format!(
            "headless execution requires exactly one mailbox; durable project contains {mailbox_count}"
        )));
    }
    let preflight_state = app
        .store
        .mailbox_state(&job_id)
        .map_err(|error| error.to_string())?;
    if preflight_state.as_deref() != Some("ready") {
        return Err(HeadlessFailure::from(format!(
            "preflight did not produce a runnable mailbox (state={:?}, status={})",
            preflight_state, app.status.text
        )));
    }
    if !live {
        return Ok(format!(
            "Headless preflight completed for project {project_id}, mailbox {job_id}."
        ));
    }

    // This is the explicit command-line live confirmation. The plan and
    // credential fingerprint were captured by the just-completed preflight;
    // start() still revalidates both before launching an engine.
    app.form.dry_run = false;
    app.live_confirmed = true;
    app.live_confirmation_plan = Some(plan_fingerprint_digest(&app.form.plan_fingerprint()));
    app.start();
    wait_for_headless_controller(&mut app)?;
    let final_state = app
        .store
        .mailbox_state(&job_id)
        .map_err(|error| error.to_string())?;
    if !matches!(
        final_state.as_deref(),
        Some("verified") | Some("verified_with_exceptions")
    ) {
        let (code, exit_hint) = unverified_exit_status(final_state.as_deref());
        return Err(HeadlessFailure {
            code,
            message: format!(
                "live migration did not reach a verified terminal state: {exit_hint}; state={:?}, status={}",
                final_state, app.status.text
            ),
        });
    }
    Ok(format!(
        "Headless live migration completed for project {project_id}, mailbox {job_id}; state={}.",
        final_state.unwrap_or_default()
    ))
}

/// Drive the existing durable batch worker without a GUI. Only queues already
/// restored from the ledger are accepted; importing or silently reconstructing
/// a partial batch in automation would make the scope ambiguous.
pub(crate) fn headless_batch_execute(
    state_path: &std::path::Path,
    live: bool,
    acknowledge_destination_loss: bool,
) -> Result<String, String> {
    headless_batch_execute_selected(state_path, live, None, acknowledge_destination_loss, None)
}

/// Execute only the requested durable batch jobs. This is the automation
/// boundary for selective remediation: unknown IDs are rejected rather than
/// silently broadening a repair run to the whole queue.
pub(crate) fn headless_batch_execute_selected(
    state_path: &std::path::Path,
    live: bool,
    requested_ids: Option<&HashSet<String>>,
    acknowledge_destination_loss: bool,
    diagnostic_log: Option<&std::path::Path>,
) -> Result<String, String> {
    let mut app =
        App::try_from_state_path(Some(state_path)).map_err(|failure| failure.to_string())?;
    if let Some(directory) = diagnostic_log {
        app.diagnostic_logger = Some(std::sync::Arc::new(crate::DiagnosticLogger::create(
            directory,
        )?));
        eprintln!(
            "WARNING: opt-in batch diagnostic logs are owner-only and bounded, but are plaintext and may retain provider, mailbox, and folder metadata. Review and securely remove them after troubleshooting."
        );
    }
    if !app.persistence_available {
        return Err("durable SQLite state is unavailable; batch execution is blocked".into());
    }
    if app.process_review_required {
        return Err(
            "recorded process ownership could not be verified; run `recover` and review the host before retrying"
                .into(),
        );
    }
    let Some(project_id) = app.queue.project_id().map(str::to_owned) else {
        return Err(
            "no durable batch queue was restored; import and validate the batch in the GUI before using headless batch execution"
                .into(),
        );
    };
    if requested_ids.is_some_and(HashSet::is_empty) {
        return Err("selective remediation requested zero mailbox IDs; refusing success".into());
    }
    let mut queue_rows = Vec::new();
    app.store
        .queue_durable_scan(&project_id, |row| queue_rows.push(row))
        .map_err(|error| error.to_string())?;
    if let Some(requested_ids) = requested_ids {
        let known = queue_rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<HashSet<_>>();
        let unknown = requested_ids
            .iter()
            .filter(|job_id| !known.contains(job_id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            return Err(format!(
                "selective remediation includes mailbox ID(s) outside the durable batch: {}",
                unknown.join(", ")
            ));
        }
    }
    app.bulk_retry_scope = BulkRetryScope::Automation;
    let attention_reasons = app
        .store
        .mailbox_attention_reasons(&project_id)
        .map_err(|error| error.to_string())?;
    let eligible_ids: HashSet<String> = queue_rows
        .iter()
        .filter_map(|row| {
            if requested_ids.is_some_and(|ids| !ids.contains(&row.id)) {
                return None;
            }
            let reason = attention_reasons.get(&row.id).copied();
            app.bulk_retry_scope
                .includes_automation(&row.durable_state, reason)
                .then_some(row.id.clone())
        })
        .collect();
    if let Some(requested_ids) = requested_ids {
        let ineligible = requested_ids
            .iter()
            .filter(|job_id| !eligible_ids.contains(*job_id))
            .cloned()
            .collect::<Vec<_>>();
        if !ineligible.is_empty() {
            return Err(format!(
                "selective remediation includes mailbox ID(s) that are not automation-safe in the current retry scope: {}",
                ineligible.join(", ")
            ));
        }
    }
    app.bulk_selected_ids = eligible_ids;
    if live {
        let removing = queue_rows
            .iter()
            .filter(|row| app.bulk_selected_ids.contains(&row.id) && row.destructive)
            .count();
        if removing > 0 {
            eprintln!(
                "Destination mutation policy: {removing} selected mailbox(es) use destination mirror or --delete2; destination-only messages/mailboxes may be removed or replaced."
            );
            if !acknowledge_destination_loss {
                return Err(format!(
                    "batch live run refused: {removing} selected mailbox(es) may remove destination-only mail. Re-run with --acknowledge-destination-loss to proceed."
                ));
            }
        }
    }
    if app.bulk_selected_ids.is_empty() {
        return Err(
            "no automation-safe batch work is queued; operator-review and verification-difference rows were not retried"
                .into(),
        );
    }
    app.form.dry_run = true;
    app.bulk_mode = BatchExecutionMode::Preflight;
    app.start_bulk();
    if app.receiver.is_none() {
        return Err(format!(
            "batch preflight was not admitted; refusing success: {}",
            app.bulk_message
        ));
    }
    wait_for_headless_controller(&mut app)?;
    if app.queue.project_id() != Some(project_id.as_str()) {
        return Err(format!(
            "batch preflight lost its durable project: {}",
            app.bulk_message
        ));
    }
    let job_ids = queue_rows
        .iter()
        .filter(|row| app.bulk_selected_ids.contains(&row.id))
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    if job_ids.is_empty() {
        return Err(
            "batch preflight resolved zero selected durable mailbox IDs; refusing success".into(),
        );
    }
    let states = app
        .store
        .batch_admission_states(&project_id, &job_ids)
        .map_err(|error| error.to_string())?;
    if states.iter().any(|state| state.state != "ready") {
        return Err(format!(
            "batch preflight did not make every mailbox runnable (states: {})",
            states
                .iter()
                .map(|state| format!("{}={}", state.job_id, state.state))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !live {
        return Ok(format!(
            "Headless batch preflight completed for project {project_id}; {} mailbox(es) are ready.",
            job_ids.len()
        ));
    }

    app.form.dry_run = false;
    app.bulk_mode = BatchExecutionMode::Live;
    app.bulk_live_confirmed = true;
    app.start_bulk();
    if app.receiver.is_none() {
        return Err(format!(
            "batch live execution was not admitted; refusing success: {}",
            app.bulk_message
        ));
    }
    let live_project_id = app.queue.project_id().ok_or_else(|| {
        "batch live execution lost its durable project; refusing success".to_owned()
    })?;
    if live_project_id != project_id {
        return Err(
            "batch live promotion changed the durable project identity; refusing success".into(),
        );
    }
    wait_for_headless_controller(&mut app)?;
    let final_selected = app
        .store
        .batch_admission_states(&project_id, &job_ids)
        .map_err(|error| error.to_string())?;
    if final_selected.is_empty() {
        return Err(
            "headless batch live run resolved zero selected durable mailbox states; refusing success"
                .into(),
        );
    }
    if final_selected.len() != job_ids.len() {
        return Err(format!(
            "headless batch live run returned {} mailbox state(s) for {} selected mailbox(es); refusing success",
            final_selected.len(),
            job_ids.len()
        ));
    }
    let unresolved = final_selected
        .iter()
        .filter(|mailbox| !mailbox.verified_terminal)
        .map(|mailbox| format!("{}={}", mailbox.job_id, mailbox.state))
        .collect::<Vec<_>>();
    if !unresolved.is_empty() {
        return Err(format!(
            "headless batch live run left unresolved mailbox(es): {}",
            unresolved.join(", ")
        ));
    }
    Ok(format!(
        "Headless batch live migration completed for project {project_id}; {} mailbox(es) reached verified terminal states.",
        job_ids.len()
    ))
}

/// Foreground supervisor for an already admitted durable batch. The loop is
/// intentionally conservative: it only selects automation-safe rows and
/// leaves Attention/verification-difference rows for an operator. A zero
/// idle-poll limit keeps watching for work; a non-zero limit makes a one-shot
/// invocation terminate after the requested quiet period. An optional
/// `maintenance_window` additionally confines new batch passes to a
/// time-of-day (and optionally day-of-week) range: outside the window the
/// loop only waits and re-checks the clock rather than admitting new work, so
/// a batch already admitted before the window closes is not abandoned
/// mid-run. Being outside the window counts toward the same idle-poll limit
/// as having no actionable work, so a `max_idle_polls`-bounded invocation
/// (for example one launched by an external scheduler at the start of each
/// window) still terminates instead of running through every future window;
/// a continuous (`max_idle_polls == 0`) invocation instead just waits for the
/// window to reopen.
pub(crate) fn headless_supervise(
    state_path: &std::path::Path,
    poll_interval: Duration,
    max_idle_polls: usize,
    maintenance_window: Option<MaintenanceWindow>,
    acknowledge_destination_loss: bool,
) -> Result<String, String> {
    let mut idle_polls = 0_usize;
    let mut completed_passes = 0_usize;
    loop {
        // Checked before `headless_status` so a closed window costs nothing
        // beyond the wall-clock check itself.
        if let Some(window) = &maintenance_window
            && !window.contains_now()
        {
            idle_polls = idle_polls.saturating_add(1);
            if max_idle_polls != 0 && idle_polls >= max_idle_polls {
                return Ok(format!(
                    "Migration supervisor stopped after {idle_polls} poll(s) outside the configured maintenance window; completed {completed_passes} batch pass(es). Operator-review rows were left untouched."
                ));
            }
            thread::sleep(poll_interval);
            continue;
        }
        let store =
            core::StateStore::open_readonly(state_path).map_err(|error| error.to_string())?;
        let actionable = store
            .latest_project()
            .map_err(|error| error.to_string())?
            .filter(|project| !project.name.trim().is_empty())
            .map(|project| {
                store
                    .project_has_automation_safe_batch_work(&project.id)
                    .map_err(|error| error.to_string())
            })
            .transpose()?
            .unwrap_or(false);
        if !actionable {
            idle_polls = idle_polls.saturating_add(1);
            if max_idle_polls != 0 && idle_polls >= max_idle_polls {
                return Ok(format!(
                    "Migration supervisor stopped after {idle_polls} idle poll(s); completed {completed_passes} batch pass(es). Operator-review rows were left untouched."
                ));
            }
            thread::sleep(poll_interval);
            continue;
        }
        idle_polls = 0;
        match headless_batch_execute(state_path, true, acknowledge_destination_loss) {
            Ok(message) => {
                completed_passes = completed_passes.saturating_add(1);
                eprintln!("{message}");
            }
            Err(error)
                if error.contains("Another MailSwiftSync instance holds the project database")
                    || error.contains("durable SQLite state is unavailable") =>
            {
                eprintln!("Migration supervisor waiting for durable controller ownership: {error}");
                thread::sleep(poll_interval);
            }
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn wait_for_headless_controller(app: &mut App) -> Result<(), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(7 * 24 * 60 * 60);
    let mut last_debug = std::time::Instant::now();
    // Include every background channel, notably asynchronous keyring
    // loading: `start()` returns while credentials load, and exiting then
    // would report a failed preflight that never ran.
    while app.background_work_pending() {
        app.poll();
        if crate::runner::process_supervision_debug_enabled()
            && last_debug.elapsed() >= Duration::from_secs(15)
        {
            eprintln!(
                "[process-debug] headless poll still waiting (running={}, output_lines={}, status={})",
                app.running(),
                app.output.len(),
                app.status.text
            );
            last_debug = std::time::Instant::now();
        }
        if std::time::Instant::now() >= deadline {
            if let Some(cancel) = &app.cancel_requested {
                cancel.store(true, Ordering::Relaxed);
            }
            return Err("headless controller wait exceeded seven days".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
    if app.durability_error || app.durability_recovery_pending {
        let verification_detail = last_verification_detail(&app.output)
            .map(|detail| {
                format!(
                    "; verification detail: {}",
                    crate::truncate_utf8(detail, 2048)
                )
            })
            .unwrap_or_default();
        let durability_detail = last_durability_detail(&app.output)
            .map(|detail| {
                format!(
                    "; durability detail: {}",
                    crate::truncate_utf8(detail, 2048)
                )
            })
            .unwrap_or_default();
        return Err(format!(
            "durable terminal state was not confirmed: {}{verification_detail}{durability_detail}",
            app.status.text
        ));
    }
    Ok(())
}

fn last_verification_detail(output: &crate::BoundedLineBuffer) -> Option<&str> {
    output
        .iter()
        .rev()
        .find_map(|line| line.strip_prefix("[verification] "))
}

fn last_durability_detail(output: &crate::BoundedLineBuffer) -> Option<&str> {
    output
        .iter()
        .rev()
        .find_map(|line| line.strip_prefix("[durability] "))
}

#[cfg(test)]
mod tests {
    use super::{last_durability_detail, last_verification_detail};
    use crate::BoundedLineBuffer;

    #[test]
    fn unverified_live_states_map_to_documented_exit_codes() {
        for (state, code) in [
            (Some("delta_required"), 3),
            (Some("verification_difference"), 4),
            (Some("attention"), 5),
            (Some("failed"), 1),
            (None, 1),
        ] {
            let (actual, hint) = super::unverified_exit_status(state);
            assert_eq!(actual, code);
            assert!(hint.contains(&format!("exit status {code}")));
        }
        let failure = super::HeadlessFailure::from("plain error");
        assert_eq!(failure.code, 1);
    }

    /// `start()` returns while keyring credentials load on another thread.
    /// The headless wait must keep polling until that result is applied.
    #[test]
    fn headless_wait_covers_asynchronous_credential_loading() {
        let state_path = std::env::temp_dir().join(format!(
            "mailswiftsync-headless-wait-{}.db",
            uuid::Uuid::new_v4()
        ));
        let mut app = crate::App::from_state_path(Some(&state_path));
        let (sender, receiver) = std::sync::mpsc::channel();
        app.start_credentials_receiver = Some(receiver);
        let loader = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let _ = sender.send(Err("keyring unavailable".to_owned()));
        });
        let _ = super::wait_for_headless_controller(&mut app);
        loader.join().unwrap();
        assert!(
            app.start_credentials_receiver.is_none(),
            "the wait returned before the credential result was applied"
        );
        assert_eq!(app.status.text, "keyring unavailable");
        drop(app);
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(state_path.with_extension("lock"));
    }

    #[test]
    fn headless_failure_keeps_the_latest_verification_detail() {
        let mut output = BoundedLineBuffer::new();
        output.push_bounded("[verification] earlier issue".into(), 10, 1024);
        output.push_bounded("engine output".into(), 10, 1024);
        output.push_bounded("[verification] latest issue".into(), 10, 1024);
        assert_eq!(last_verification_detail(&output), Some("latest issue"));
    }

    #[test]
    fn headless_failure_keeps_the_latest_durability_detail() {
        let mut output = BoundedLineBuffer::new();
        output.push_bounded("[durability] earlier issue".into(), 10, 1024);
        output.push_bounded("engine output".into(), 10, 1024);
        output.push_bounded("[durability] latest issue".into(), 10, 1024);

        assert_eq!(last_durability_detail(&output), Some("latest issue"));
    }
}
