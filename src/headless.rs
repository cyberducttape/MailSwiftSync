use crate::atomic_artifact::write_private_atomic;
use crate::credentials::SecretString;
use crate::maintenance_window::MaintenanceWindow;
use crate::{
    App, BatchExecutionMode, BulkRetryScope, cleanup_stale_secret_directories, core,
    is_verified_terminal_state, plan_fingerprint_digest, recorded_process_matches,
    secret_runtime_base, terminate_recorded_process_group,
};
use serde::Serialize;
use std::{sync::atomic::Ordering, thread, time::Duration};

const SUPPORT_MAILBOX_SAMPLE_LIMIT: u32 = 1_000;

#[derive(Debug, Serialize)]
pub(crate) struct HeadlessStatus {
    pub(crate) schema_version: i64,
    pub(crate) active_processes: Vec<core::ActiveProcess>,
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
    pub(crate) schema_version: i64,
    pub(crate) active_processes: Vec<core::ActiveProcess>,
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
    let projects = store
        .recent_projects(1_000)
        .map_err(|error| error.to_string())?;
    let mut project_values = Vec::with_capacity(projects.len());
    for project in projects {
        let counts = store
            .mailbox_state_counts(&project.id)
            .map_err(|error| error.to_string())?;
        let jobs = store
            .mailbox_status_page(&project.id, 0, sample_limit)
            .map_err(|error| error.to_string())?;
        let mailbox_values = jobs
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
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
        project_values.push(serde_json::json!({
            "project_id": project.id,
            "project_name": project.name,
            "phase": project.phase.as_str(),
            "mailbox_count": counts.total,
            "mailbox_sample_limit": sample_limit,
            "mailboxes_truncated": counts.total > jobs.len(),
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
    let active_processes = store
        .active_processes()
        .map_err(|error| error.to_string())?
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
        "projects": project_values,
        "redaction": {
            "endpoints": "excluded",
            "credentials": "excluded",
            "plan_snapshots": "excluded",
            "command_paths": "excluded",
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
    for project in projects {
        let batch = store
            .project_has_mailbox_configs(&project.id)
            .map_err(|error| error.to_string())?;
        let attention_reasons = store
            .mailbox_attention_reasons(&project.id)
            .map_err(|error| error.to_string())?;
        let mut mailboxes = Vec::new();
        for mailbox in store
            .mailboxes(&project.id)
            .map_err(|error| error.to_string())?
        {
            mailboxes.push(HeadlessMailboxStatus {
                attention_reason: attention_reasons
                    .get(&mailbox.id)
                    .map(|reason| reason.as_str().to_owned()),
                id: mailbox.id,
                source_mailbox: mailbox.source_mailbox,
                destination_mailbox: mailbox.destination_mailbox,
                state: mailbox.state,
            });
        }
        result.push(HeadlessProjectStatus {
            id: project.id,
            name: project.name,
            batch,
            source_endpoint: project.source_endpoint,
            destination_endpoint: project.destination_endpoint,
            phase: project.phase.as_str().to_owned(),
            mailboxes,
        });
    }
    let active_processes = store
        .active_processes()
        .map_err(|error| error.to_string())?;
    Ok(HeadlessStatus {
        schema_version: core::CURRENT_SCHEMA_VERSION,
        active_processes,
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
        summaries.push(HeadlessProjectSummary {
            id: project.id,
            name: project.name,
            batch,
            source_endpoint: project.source_endpoint,
            destination_endpoint: project.destination_endpoint,
            phase: project.phase.as_str().to_owned(),
            mailbox_state_counts,
        });
    }
    Ok(HeadlessStatusSummary {
        schema_version: core::CURRENT_SCHEMA_VERSION,
        active_processes: store
            .active_processes()
            .map_err(|error| error.to_string())?,
        projects: summaries,
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
        } else {
            unverified.push(process.clone());
        }
    }
    let recovered_jobs = store
        .recover_abandoned_jobs_preserving(&unverified)
        .map_err(|error| error.to_string())?;
    if unverified.is_empty() {
        cleanup_stale_secret_directories(&secret_runtime_base());
    }
    Ok(HeadlessRecoveryResult {
        recovered_jobs,
        preserved_processes: unverified.len(),
    })
}

/// Run the existing durable controller without constructing an egui window.
/// A live invocation always performs a fresh dry preflight first, so this
/// path cannot promote credentials or a plan left over from another process.
pub(crate) fn headless_execute_with_credentials(
    state_path: &std::path::Path,
    live: bool,
    credentials: Option<HeadlessCredentials>,
) -> Result<String, String> {
    // Use the same startup recovery as the GUI, but pass the ledger path as
    // data instead of mutating process-global environment state.
    let mut app = App::from_state_path(Some(state_path));
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
    if !app.bulk_job_ids.is_empty() {
        return Err(
            "headless single-mailbox execution refuses a restored batch queue; use the GUI for batch admission or an explicit batch supervisor"
                .into(),
        );
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
        .mailboxes(&project_id)
        .map_err(|error| error.to_string())?
        .len();
    if mailbox_count != 1 {
        return Err(format!(
            "headless execution requires exactly one mailbox; durable project contains {mailbox_count}"
        ));
    }
    let preflight_state = app
        .store
        .mailbox_state(&job_id)
        .map_err(|error| error.to_string())?;
    if preflight_state.as_deref() != Some("ready") {
        return Err(format!(
            "preflight did not produce a runnable mailbox (state={:?}, status={})",
            preflight_state, app.status.text
        ));
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
        let exit_hint = match final_state.as_deref() {
            Some("delta_required") => "delta required (exit status 3)",
            Some("verification_difference") => "verification difference (exit status 4)",
            Some("attention") => "operator attention (exit status 5)",
            _ => "unresolved (exit status 1)",
        };
        return Err(format!(
            "live migration did not reach a verified terminal state: {exit_hint}; state={:?}, status={}",
            final_state, app.status.text
        ));
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
) -> Result<String, String> {
    let mut app = App::from_state_path(Some(state_path));
    if !app.persistence_available {
        return Err("durable SQLite state is unavailable; batch execution is blocked".into());
    }
    if app.process_review_required {
        return Err(
            "recorded process ownership could not be verified; run `recover` and review the host before retrying"
                .into(),
        );
    }
    if app.bulk_jobs.is_empty() {
        return Err(
            "no durable batch queue was restored; import and validate the batch in the GUI before using headless batch execution"
                .into(),
        );
    }
    if app.bulk_job_ids.len() != app.bulk_jobs.len() || app.bulk_project_id.is_none() {
        return Err(
            "the restored batch queue has no complete durable identity; refusing ambiguous headless admission"
                .into(),
        );
    }
    app.bulk_retry_scope = BulkRetryScope::Automation;
    app.bulk_selected_ids = app
        .bulk_job_ids
        .iter()
        .filter_map(|job_id| {
            let state = app.store.mailbox_state(job_id).ok().flatten()?;
            let reason = app.store.mailbox_attention_reason(job_id).ok().flatten();
            app.bulk_retry_scope
                .includes_automation(&state, reason)
                .then_some(job_id.clone())
        })
        .collect();
    if app.bulk_selected_ids.is_empty() {
        return Err(
            "no automation-safe batch work is queued; operator-review and verification-difference rows were not retried"
                .into(),
        );
    }
    app.form.dry_run = true;
    app.bulk_mode = BatchExecutionMode::Preflight;
    app.start_bulk();
    wait_for_headless_controller(&mut app)?;
    let project_id = app
        .bulk_project_id
        .as_deref()
        .ok_or_else(|| {
            format!(
                "batch preflight lost its durable project: {}",
                app.bulk_message
            )
        })?
        .to_owned();
    let job_ids = app
        .bulk_job_ids
        .iter()
        .filter(|job_id| app.bulk_selected_ids.contains(*job_id))
        .cloned()
        .collect::<Vec<_>>();
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
    wait_for_headless_controller(&mut app)?;
    let final_mailboxes = app
        .store
        .mailboxes(&project_id)
        .map_err(|error| error.to_string())?;
    let final_selected = final_mailboxes
        .iter()
        .filter(|mailbox| app.bulk_selected_ids.contains(&mailbox.id))
        .collect::<Vec<_>>();
    if final_selected.len() != job_ids.len() {
        return Err(format!(
            "headless batch live run returned {} mailbox state(s) for {} selected mailbox(es); refusing success",
            final_selected.len(),
            job_ids.len()
        ));
    }
    let unresolved = final_selected
        .iter()
        .filter(|mailbox| !is_verified_terminal_state(&mailbox.state))
        .map(|mailbox| format!("{}={}", mailbox.id, mailbox.state))
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
        let status = headless_status(state_path, None)?;
        let actionable = status
            .projects
            .iter()
            .filter(|project| project.batch)
            .flat_map(|project| project.mailboxes.iter())
            .any(|mailbox| {
                BulkRetryScope::Automation.includes_automation(
                    &mailbox.state,
                    mailbox
                        .attention_reason
                        .as_deref()
                        .and_then(core::AttentionReason::parse),
                )
            });
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
        match headless_batch_execute(state_path, true) {
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
    while app.running() || app.capability_receiver.is_some() || app.live_auth_receiver.is_some() {
        app.poll();
        if std::time::Instant::now() >= deadline {
            if let Some(cancel) = &app.cancel_requested {
                cancel.store(true, Ordering::Relaxed);
            }
            return Err("headless controller wait exceeded seven days".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
    if app.durability_error || app.durability_recovery_pending {
        return Err(format!(
            "durable terminal state was not confirmed: {}",
            app.status.text
        ));
    }
    Ok(())
}
