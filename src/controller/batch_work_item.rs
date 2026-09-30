//! Per-mailbox batch execution worker.

use super::batch::BatchExecutionMode;
use super::batch_worker::{AdaptiveProviderLimiter, OAuthRefreshLocks, provider_scope_key};
use crate::{
    Event, StreamOutcome,
    bulk_import::BulkJob,
    controller::failure::{
        FailureClass, classified_failure_detail, classify_failure, should_retry_batch_error,
        transient_retry_delay,
    },
    core,
    credentials::CleanupGuard,
    imap_probe::fresh_dual_imaps_authentication,
    process::ProcessLaunchLimiter,
    runner::{
        ResolvedImapsyncIdentity, RunContext, TerminalEvidenceSource, message_verification_enabled,
        persist_engine_identity_before_launch, run_dovecot_destination_preflight,
        run_dovecot_verification, run_imap_message_verification, run_streaming,
        send_reliable_event, terminal_evidence_source, validate_dovecot_checkpoint_context,
    },
    verification::ImapsyncOutputProfile,
};
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

const JOB_FINISHED_ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// Redact one mailbox's secrets from controller-generated text. Errors can
/// carry upstream content (provider responses, OAuth `error_description`),
/// so nothing leaves a batch worker for the journal, the ledger, or stderr
/// without passing through this boundary.
pub(crate) fn redact_child_text(form: &crate::Form, text: &str) -> String {
    crate::ui::redact_secrets(
        text,
        [
            form.source_password.as_str(),
            form.destination_password.as_str(),
        ],
    )
}

/// Deliver a presentation line for one batch child through the redaction
/// boundary.
fn send_run_line(
    tx: &mpsc::SyncSender<Event>,
    form: &crate::Form,
    run_id: &str,
    job_id: &str,
    text: String,
) {
    let _ = tx.send(Event::RunLine {
        run_id: run_id.to_owned(),
        job_id: job_id.to_owned(),
        text: redact_child_text(form, &text),
    });
}

pub(crate) fn send_job_finished(
    tx: &mpsc::SyncSender<Event>,
    job_id: String,
    child_run_id: String,
    state: String,
    detail: String,
    credential_fingerprint: Option<String>,
) -> Result<(), String> {
    let (reply, acknowledgement) = mpsc::sync_channel(1);
    send_reliable_event(
        tx,
        Event::JobFinished {
            job_id,
            child_run_id,
            state,
            detail,
            credential_fingerprint,
            reply,
        },
    )?;
    acknowledgement
        .recv_timeout(JOB_FINISHED_ACK_TIMEOUT)
        .map_err(|error| format!("durable JobFinished acknowledgement was lost: {error}"))?
}

struct BatchTerminalTransition<'a> {
    tx: &'a mpsc::SyncSender<Event>,
    terminal_jobs: &'a Arc<Mutex<HashSet<usize>>>,
    index: usize,
    job_id: String,
    child_run_id: String,
    job_state: &'a str,
    run_state: &'a str,
    detail: String,
    credential_fingerprint: Option<String>,
}

fn persist_batch_terminal_state(transition: BatchTerminalTransition<'_>) {
    let BatchTerminalTransition {
        tx,
        terminal_jobs,
        index,
        job_id,
        child_run_id,
        job_state,
        run_state,
        detail,
        credential_fingerprint,
    } = transition;
    let _ = send_reliable_event(
        tx,
        Event::JobState {
            job_id: job_id.clone(),
            child_run_id: child_run_id.clone(),
            state: job_state.to_owned(),
        },
    );
    if let Err(error) = send_job_finished(
        tx,
        job_id,
        child_run_id,
        run_state.to_owned(),
        detail,
        credential_fingerprint,
    ) {
        eprintln!("durable batch terminal event delivery failed: {error}");
    } else if let Ok(mut terminal) = terminal_jobs.lock() {
        terminal.insert(index);
    }
}

pub(crate) struct BatchWorkerContext {
    pub(crate) concurrency: usize,
    pub(crate) mode: BatchExecutionMode,
    pub(crate) retry_count: usize,
    pub(crate) job_rx:
        crossbeam_channel::Receiver<(usize, String, String, Option<String>, BulkJob)>,
    pub(crate) tx: mpsc::SyncSender<Event>,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) failed: Arc<AtomicBool>,
    pub(crate) terminal_jobs: Arc<Mutex<HashSet<usize>>>,
    pub(crate) launch_limiter: Arc<ProcessLaunchLimiter>,
    pub(crate) provider_limiter: Arc<AdaptiveProviderLimiter>,
    pub(crate) batch_project_id: String,
    pub(crate) batch_run_id: String,
    pub(crate) resolved_imapsync: Arc<std::collections::HashMap<String, ResolvedImapsyncIdentity>>,
    pub(crate) oauth_refresh_locks: OAuthRefreshLocks,
    pub(crate) verification_state_path: Option<std::path::PathBuf>,
}

fn refresh_live_credentials(
    form: &mut crate::migration_plan::Form,
    locks: &OAuthRefreshLocks,
) -> Result<(), String> {
    form.load_static_configured_keyring_credentials()?;
    let refreshes = [
        (
            true,
            form.profile.source_auth.clone(),
            form.profile.source_oauth_refresh_credential_id.clone(),
        ),
        (
            false,
            form.profile.destination_auth.clone(),
            form.profile.destination_oauth_refresh_credential_id.clone(),
        ),
    ];
    for (source, auth, refresh_id) in refreshes {
        if auth != "oauth2" || refresh_id.trim().is_empty() {
            continue;
        }
        let refresh_lock = {
            let mut lock_map = locks
                .lock()
                .map_err(|_| "OAuth refresh lock registry was poisoned".to_owned())?;
            lock_map
                .entry(refresh_id.trim().to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = refresh_lock
            .lock()
            .map_err(|_| "OAuth refresh lock was poisoned".to_owned())?;
        form.refresh_oauth_access_token(source)?;
    }
    Ok(())
}

enum BatchClaimError {
    Rejected(String),
    ControllerUnavailable,
}

/// Wait for the reducer's durable claim decision before starting a process.
/// A disconnected event channel is distinct from an operator rejection: the
/// worker can no longer report a terminal transition in that case.
fn claim_batch_job(
    tx: &mpsc::SyncSender<Event>,
    project_id: &str,
    job_id: &str,
    parent_run_id: &str,
    child_run_id: &str,
    cancel: &AtomicBool,
) -> Result<(), BatchClaimError> {
    let (reply, response) = mpsc::sync_channel(1);
    tx.send(Event::ClaimBatch {
        project_id: project_id.to_owned(),
        job_id: job_id.to_owned(),
        parent_run_id: parent_run_id.to_owned(),
        child_run_id: child_run_id.to_owned(),
        reply,
    })
    .map_err(|_| BatchClaimError::ControllerUnavailable)?;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(BatchClaimError::Rejected(
                "cancelled by operator before durable claim".to_owned(),
            ));
        }
        match response.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) => return Err(BatchClaimError::Rejected(error)),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(BatchClaimError::Rejected(
                    "durable claim response was lost".to_owned(),
                ));
            }
        }
    }
}

struct BatchAttemptContext<'a> {
    form: &'a crate::Form,
    checkpoint: Option<String>,
    concurrency: usize,
    live: bool,
    index: usize,
    job_id: String,
    child_run_id: String,
    project_id: String,
    tx: mpsc::SyncSender<Event>,
    cancel: Arc<AtomicBool>,
    imapsync_output_profile: ImapsyncOutputProfile,
    verification_state_path: Option<std::path::PathBuf>,
    transfer_attempt_number: &'a mut u32,
    verification_failure: &'a mut Option<String>,
}

/// Prepare and execute a single engine attempt, then collect engine-specific
/// evidence. Retry policy and durable job transitions deliberately stay with
/// the outer mailbox coordinator.
fn run_prepared_batch_attempt(context: BatchAttemptContext<'_>) -> Result<StreamOutcome, String> {
    let BatchAttemptContext {
        form,
        checkpoint,
        concurrency,
        live,
        index,
        job_id,
        child_run_id,
        project_id,
        tx,
        cancel,
        imapsync_output_profile,
        verification_state_path,
        transfer_attempt_number,
        verification_failure,
    } = context;
    let prepared = form
        .prepared_command_with_throttle_divisor_and_checkpoint(concurrency, checkpoint.as_deref());
    match prepared {
        Ok(command) => {
            let cleanup_guard = CleanupGuard::new(command.cleanup.clone());
            let verification = command.verification.clone();
            let prefix = format!("[{}] ", index + 1);
            let secrets = [
                form.source_password.clone(),
                form.destination_password.clone(),
            ];
            let checkpoint_validation = if !form.dry_run
                && form.engine() == core::Engine::Dovecot
                && let Some(saved_checkpoint) = checkpoint.as_deref()
            {
                match core::dovecot_checkpoint_context(saved_checkpoint) {
                    Some(context) => validate_dovecot_checkpoint_context(
                        &verification,
                        &[],
                        &form.source_password,
                        context,
                        &tx,
                        &cancel,
                        Duration::from_secs(form.profile.migration_timeout_hours * 60 * 60),
                        &prefix,
                        &child_run_id,
                        &job_id,
                    ),
                    None => Err(
                        "saved Dovecot checkpoint has no UIDVALIDITY context; refusing resume"
                            .into(),
                    ),
                }
            } else {
                Ok(())
            };
            let result = checkpoint_validation.and_then(|()| {
                *transfer_attempt_number = transfer_attempt_number.saturating_add(1);
                run_streaming(RunContext {
                    executable: &command.executable,
                    args: &command.args,
                    env: &command.env,
                    tx: &tx,
                    run_id: &child_run_id,
                    job_id: &job_id,
                    project_id: &project_id,
                    prefix: &prefix,
                    cancel: &cancel,
                    secrets: &secrets,
                    timeout: Duration::from_secs(
                        form.profile.migration_timeout_hours * 60 * 60,
                    ),
                    dovecot_exit_two_is_delta: form.engine() == core::Engine::Dovecot
                        && !form.dry_run,
                    imapsync_output_profile,
                    diagnostic_logger: None,
                    attempt_number: *transfer_attempt_number,
                    live_transfer: live,
                })
            })
            .and_then(|stream| {
                if !form.dry_run
                    && form.engine() == core::Engine::ImapSync
                    && message_verification_enabled(form)
                {
                    match run_imap_message_verification(
                        form,
                        &job_id,
                        &child_run_id,
                        &cancel,
                        verification_state_path.as_deref().map(|path| {
                            core::durable_stage_path(path, &job_id)
                        }).as_deref(),
                    ) {
                        Ok((evidence, mismatches)) => {
                            send_reliable_event(&tx, Event::BatchEvidence {
                                job_id: job_id.clone(),
                                child_run_id: child_run_id.clone(),
                                evidence,
                                mismatches,
                            })
                            .map_err(|error| format!("batch evidence delivery failed: {error}"))?;
                        }
                        Err(error) => {
                            let safe = redact_child_text(form, &error);
                            eprintln!(
                                "[{}] message-level IMAP verification failed: {safe}",
                                index + 1
                            );
                            *verification_failure = Some(
                                safe.chars().take(2048).collect::<String>(),
                            );
                            send_run_line(&tx, form, &child_run_id, &job_id, format!(
                                    "[{}] message-level verification unavailable; transfer succeeded and requires review: {safe}",
                                    index + 1
                                ));
                        }
                    }
                } else if !form.dry_run
                    && terminal_evidence_source(
                        form,
                        false,
                        stream.imapsync_evidence.is_some(),
                    ) == TerminalEvidenceSource::Engine
                    && let Some(evidence) = stream.imapsync_evidence
                {
                    send_reliable_event(&tx, Event::BatchEvidence {
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        evidence,
                        mismatches: Vec::new(),
                    })
                    .map_err(|error| format!("batch evidence delivery failed: {error}"))?;
                }
                if !form.dry_run && form.engine() == core::Engine::Dovecot {
                    let verification_secret = form.source_password.clone();
                    let verification_env = Vec::new();
                    let verification_result = run_dovecot_verification(
                        &verification,
                        &verification_env,
                        std::slice::from_ref(&verification_secret),
                        &tx,
                        &cancel,
                        Duration::from_secs(form.profile.migration_timeout_hours * 60 * 60),
                        &prefix,
                        &child_run_id,
                        &job_id,
                    )?;
                    if let (Some(state), Some(context)) = (
                        stream.dovecot_checkpoint.as_deref(),
                        verification_result.checkpoint_context.as_deref(),
                    ) {
                        let value = core::encode_dovecot_checkpoint(state, context)
                            .ok_or_else(|| {
                                "Dovecot produced an invalid checkpoint context".to_owned()
                            })?;
                        send_reliable_event(
                            &tx,
                            Event::Checkpoint {
                                run_id: child_run_id.clone(),
                                job_id: job_id.clone(),
                                value,
                            },
                        )?;
                    }
                    send_reliable_event(
                        &tx,
                        Event::BatchEvidence {
                            job_id: job_id.clone(),
                            child_run_id: child_run_id.clone(),
                            evidence: verification_result.evidence,
                            mismatches: Vec::new(),
                        },
                    )?;
                }
                Ok(stream.outcome)
            });
            let result = if result.is_ok() && form.dry_run && form.engine() == core::Engine::Dovecot
            {
                result.and_then(|stream| {
                    run_dovecot_destination_preflight(
                        &form.dovecot_destination_preflight_commands(),
                        &tx,
                        &cancel,
                        Duration::from_secs(form.profile.migration_timeout_hours * 60 * 60),
                        &format!("[{}] ", index + 1),
                        &child_run_id,
                        &job_id,
                    )
                    .map(|_| stream)
                })
            } else {
                result
            };
            drop(cleanup_guard);
            result
        }
        Err(error) => Err(error),
    }
}

/// Execute mailbox work items for one batch worker. All output is emitted as
/// typed controller events; durable state remains owned by the poll reducer.
pub(crate) fn process_batch_work_items(context: BatchWorkerContext) {
    let BatchWorkerContext {
        concurrency,
        mode,
        retry_count,
        job_rx,
        tx,
        cancel,
        failed,
        terminal_jobs,
        launch_limiter,
        provider_limiter,
        batch_project_id,
        batch_run_id,
        resolved_imapsync,
        oauth_refresh_locks,
        verification_state_path,
    } = context;
    let live = mode.is_live();
    while let Ok((index, job_id, child_run_id, checkpoint, job)) = job_rx.recv() {
        let mut form = job.form();
        if cancel.load(Ordering::Relaxed) {
            persist_batch_terminal_state(BatchTerminalTransition {
                tx: &tx,
                terminal_jobs: &terminal_jobs,
                index,
                job_id: job_id.clone(),
                child_run_id: child_run_id.clone(),
                job_state: "Cancelled",
                run_state: "cancelled",
                detail: "cancelled before worker claim".into(),
                credential_fingerprint: None,
            });
            continue;
        }
        send_run_line(
            &tx,
            &form,
            &child_run_id,
            &job_id,
            format!("══ Job {}: {} ══", index + 1, job.label),
        );
        let imapsync_output_profile = if form.engine() == core::Engine::ImapSync {
            let identity = resolved_imapsync.get(&form.profile.imapsync_path);
            let version = identity
                .map(|identity| identity.version.clone())
                .unwrap_or_else(|| "unknown".into());
            let profile = identity
                .map(|identity| identity.output_profile)
                .unwrap_or(ImapsyncOutputProfile::Unknown);
            let identity_persisted = if let Err(error) =
                persist_engine_identity_before_launch(&tx, &child_run_id, &job_id, &version)
            {
                send_run_line(
                    &tx,
                    &form,
                    &child_run_id,
                    &job_id,
                    format!(
                        "[{}] [verification] engine identity is not durable; output evidence disabled: {error}",
                        index + 1
                    ),
                );
                false
            } else {
                true
            };
            if identity_persisted && profile == ImapsyncOutputProfile::Unknown {
                send_run_line(
                    &tx,
                    &form,
                    &child_run_id,
                    &job_id,
                    format!(
                        "[{}] {}",
                        index + 1,
                        crate::verification::unqualified_imapsync_message(&version)
                    ),
                );
            }
            if identity_persisted {
                profile
            } else {
                ImapsyncOutputProfile::Unknown
            }
        } else {
            ImapsyncOutputProfile::Unknown
        };
        let mut completed = false;
        let mut delta_required = false;
        let mut claimed = false;
        let mut verification_failure: Option<String> = None;
        let mut transfer_attempt_number = 0_u32;
        let provider_key = provider_scope_key(&form);
        for attempt in 0..=retry_count {
            if !launch_limiter.acquire(&cancel) {
                break;
            }
            if !provider_limiter.wait(&provider_key, &cancel) {
                break;
            }
            if live {
                // A queue-level admission must not mint an access token for
                // every selected mailbox. Refresh immediately before this
                // worker authenticates and launches the engine, so waiting in
                // a large queue cannot consume an expired bearer token.
                if let Err(error) = refresh_live_credentials(&mut form, &oauth_refresh_locks) {
                    failed.store(true, Ordering::Relaxed);
                    send_run_line(
                        &tx,
                        &form,
                        &child_run_id,
                        &job_id,
                        format!(
                            "[{}] OAuth credential refresh failed before launch: {error}",
                            index + 1
                        ),
                    );
                    persist_batch_terminal_state(BatchTerminalTransition {
                        tx: &tx,
                        terminal_jobs: &terminal_jobs,
                        index,
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        job_state: "Failed",
                        run_state: "failed",
                        detail: classified_failure_detail(&redact_child_text(&form, &error)),
                        credential_fingerprint: None,
                    });
                    break;
                }
            }
            if live && let Err(error) = fresh_dual_imaps_authentication(&form) {
                if should_retry_batch_error(&error, attempt, retry_count) {
                    provider_limiter.observe_failure(&provider_key, &error);
                    send_run_line(
                        &tx,
                        &form,
                        &child_run_id,
                        &job_id,
                        format!(
                            "[{}] [{}] transient fresh authentication probe failure; retrying: {error}",
                            index + 1,
                            classify_failure(&error).label()
                        ),
                    );
                    let _ = send_reliable_event(
                        &tx,
                        Event::JobState {
                            job_id: job_id.clone(),
                            child_run_id: child_run_id.clone(),
                            state: "Retrying".into(),
                        },
                    );
                    let delay = transient_retry_delay(&error, attempt);
                    let started = std::time::Instant::now();
                    while started.elapsed() < delay {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    continue;
                }
                failed.store(true, Ordering::Relaxed);
                send_run_line(
                    &tx,
                    &form,
                    &child_run_id,
                    &job_id,
                    format!(
                        "[{}] fresh live authentication failed before launch: {error}",
                        index + 1
                    ),
                );
                // Classify the raw probe result. Prefixing it with
                // "authentication failed" would incorrectly mask a DNS,
                // TCP, TLS-disconnect, or provider-capacity failure.
                persist_batch_terminal_state(BatchTerminalTransition {
                    tx: &tx,
                    terminal_jobs: &terminal_jobs,
                    index,
                    job_id: job_id.clone(),
                    child_run_id: child_run_id.clone(),
                    job_state: "Failed",
                    run_state: "failed",
                    detail: classified_failure_detail(&redact_child_text(&form, &error)),
                    credential_fingerprint: None,
                });
                break;
            }
            if !claimed {
                match claim_batch_job(
                    &tx,
                    &batch_project_id,
                    &job_id,
                    &batch_run_id,
                    &child_run_id,
                    &cancel,
                ) {
                    Ok(()) => claimed = true,
                    Err(BatchClaimError::ControllerUnavailable) => {
                        failed.store(true, Ordering::Relaxed);
                        break;
                    }
                    Err(BatchClaimError::Rejected(error)) => {
                        let cancelled = classify_failure(&error) == FailureClass::Cancellation;
                        if !cancelled {
                            failed.store(true, Ordering::Relaxed);
                        }
                        send_run_line(
                            &tx,
                            &form,
                            &child_run_id,
                            &job_id,
                            format!("[{}] {}", index + 1, error),
                        );
                        persist_batch_terminal_state(BatchTerminalTransition {
                            tx: &tx,
                            terminal_jobs: &terminal_jobs,
                            index,
                            job_id: job_id.clone(),
                            child_run_id: child_run_id.clone(),
                            job_state: if cancelled { "Cancelled" } else { "Failed" },
                            run_state: if cancelled { "cancelled" } else { "failed" },
                            detail: classified_failure_detail(&redact_child_text(&form, &error)),
                            credential_fingerprint: None,
                        });
                        break;
                    }
                }
            }
            let _ = send_reliable_event(
                &tx,
                Event::JobState {
                    job_id: job_id.clone(),
                    child_run_id: child_run_id.clone(),
                    state: "Running".into(),
                },
            );
            if attempt > 0 {
                send_run_line(
                    &tx,
                    &form,
                    &child_run_id,
                    &job_id,
                    format!("[{}] retry attempt {attempt}/{retry_count}", index + 1),
                );
                let _ = send_reliable_event(
                    &tx,
                    Event::JobState {
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        state: "Running".into(),
                    },
                );
            }
            let result = run_prepared_batch_attempt(BatchAttemptContext {
                form: &form,
                checkpoint: checkpoint.clone(),
                concurrency,
                live,
                index,
                job_id: job_id.clone(),
                child_run_id: child_run_id.clone(),
                project_id: batch_project_id.clone(),
                tx: tx.clone(),
                cancel: cancel.clone(),
                imapsync_output_profile,
                verification_state_path: verification_state_path.clone(),
                transfer_attempt_number: &mut transfer_attempt_number,
                verification_failure: &mut verification_failure,
            });
            match result {
                Ok(outcome) => {
                    if outcome == StreamOutcome::DeltaRequired {
                        send_run_line(
                            &tx,
                            &form,
                            &child_run_id,
                            &job_id,
                            format!(
                                "[{}] Dovecot reports an incomplete synchronization; another delta pass is required",
                                index + 1
                            ),
                        );
                        delta_required = true;
                    }
                    completed = true;
                    break;
                }
                Err(error) if should_retry_batch_error(&error, attempt, retry_count) => {
                    provider_limiter.observe_failure(&provider_key, &error);
                    send_run_line(
                        &tx,
                        &form,
                        &child_run_id,
                        &job_id,
                        format!(
                            "[{}] [{}] transient failure; retrying: {error}",
                            index + 1,
                            classify_failure(&error).label()
                        ),
                    );
                    let _ = send_reliable_event(
                        &tx,
                        Event::JobState {
                            job_id: job_id.clone(),
                            child_run_id: child_run_id.clone(),
                            state: "Retrying".into(),
                        },
                    );
                    let delay = transient_retry_delay(&error, attempt);
                    let started = std::time::Instant::now();
                    while started.elapsed() < delay {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                }
                Err(error) => {
                    let cancelled = classify_failure(&error) == FailureClass::Cancellation;
                    if !cancelled {
                        failed.store(true, Ordering::Relaxed);
                    }
                    send_run_line(
                        &tx,
                        &form,
                        &child_run_id,
                        &job_id,
                        format!(
                            "[{}] [{}] failed: {error}",
                            index + 1,
                            classify_failure(&error).label()
                        ),
                    );
                    persist_batch_terminal_state(BatchTerminalTransition {
                        tx: &tx,
                        terminal_jobs: &terminal_jobs,
                        index,
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        job_state: if cancelled { "Cancelled" } else { "Failed" },
                        run_state: if cancelled { "cancelled" } else { "failed" },
                        detail: classified_failure_detail(&redact_child_text(&form, &error)),
                        credential_fingerprint: None,
                    });
                    break;
                }
            }
        }
        if completed {
            let terminal_state = if form.dry_run {
                "ready"
            } else if delta_required {
                "delta_required"
            } else {
                "completed"
            };
            persist_batch_terminal_state(BatchTerminalTransition {
                tx: &tx,
                terminal_jobs: &terminal_jobs,
                index,
                job_id: job_id.clone(),
                child_run_id: child_run_id.clone(),
                job_state: if delta_required {
                    "DeltaRequired"
                } else {
                    "Completed"
                },
                run_state: terminal_state,
                detail: if delta_required {
                    "Dovecot reports that another delta pass is required".into()
                } else {
                    verification_failure.map_or_else(
                        || "process completed".into(),
                        |reason| {
                            format!(
                                "[attention_reason=verification_incomplete] message-level verification incomplete: {reason}"
                            )
                        },
                    )
                },
                credential_fingerprint: if form.dry_run {
                    Some(form.credential_binding_fingerprint())
                } else {
                    None
                },
            });
        } else if cancel.load(Ordering::Relaxed) {
            persist_batch_terminal_state(BatchTerminalTransition {
                tx: &tx,
                terminal_jobs: &terminal_jobs,
                index,
                job_id: job_id.clone(),
                child_run_id: child_run_id.clone(),
                job_state: "Cancelled",
                run_state: "cancelled",
                detail: "cancelled by operator".into(),
                credential_fingerprint: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_generated_output_is_redacted_before_it_leaves_the_worker() {
        let form = crate::Form {
            source_password: String::from("source-secret-value").into(),
            destination_password: String::from("destination-secret-value").into(),
            ..Default::default()
        };
        let (tx, rx) = mpsc::sync_channel(1);
        // An upstream error (for example an OAuth error_description) that
        // echoes a credential must not reach the journal verbatim.
        let error = "invalid_grant: token source-secret-value rejected";
        send_run_line(&tx, &form, "run", "job", format!("[1] failed: {error}"));
        let Ok(Event::RunLine { text, .. }) = rx.recv() else {
            panic!("expected a RunLine event");
        };
        assert!(!text.contains("source-secret-value"), "{text}");
        assert!(text.contains("[REDACTED]"), "{text}");

        let detail = classified_failure_detail(&redact_child_text(
            &form,
            "destination-secret-value was refused",
        ));
        assert!(!detail.contains("destination-secret-value"), "{detail}");
    }

    #[test]
    fn durable_batch_claim_waits_for_reducer_acknowledgement() {
        let (tx, rx) = mpsc::sync_channel(1);
        let cancel = AtomicBool::new(false);
        let worker = thread::spawn(move || {
            claim_batch_job(&tx, "project", "job", "parent", "child", &cancel)
        });
        let Ok(Event::ClaimBatch {
            project_id,
            job_id,
            parent_run_id,
            child_run_id,
            reply,
        }) = rx.recv()
        else {
            panic!("expected a durable batch claim request");
        };
        assert_eq!(project_id, "project");
        assert_eq!(job_id, "job");
        assert_eq!(parent_run_id, "parent");
        assert_eq!(child_run_id, "child");
        reply.send(Ok(())).unwrap();
        assert!(worker.join().unwrap().is_ok());
    }

    #[test]
    fn durable_batch_claim_preserves_reducer_rejection() {
        let (tx, rx) = mpsc::sync_channel(1);
        let cancel = AtomicBool::new(false);
        let worker = thread::spawn(move || {
            claim_batch_job(&tx, "project", "job", "parent", "child", &cancel)
        });
        let Ok(Event::ClaimBatch { reply, .. }) = rx.recv() else {
            panic!("expected a durable batch claim request");
        };
        reply.send(Err("stale plan".to_owned())).unwrap();
        assert!(matches!(
            worker.join().unwrap(),
            Err(BatchClaimError::Rejected(error)) if error == "stale plan"
        ));
    }

    #[test]
    fn batch_job_becomes_terminal_only_after_durable_acknowledgement() {
        let (tx, rx) = mpsc::sync_channel(2);
        let terminal_jobs = Arc::new(Mutex::new(HashSet::new()));
        let worker_terminal_jobs = Arc::clone(&terminal_jobs);
        let worker = thread::spawn(move || {
            persist_batch_terminal_state(BatchTerminalTransition {
                tx: &tx,
                terminal_jobs: &worker_terminal_jobs,
                index: 7,
                job_id: "job".to_owned(),
                child_run_id: "run".to_owned(),
                job_state: "Completed",
                run_state: "completed",
                detail: "done".to_owned(),
                credential_fingerprint: None,
            });
        });
        assert!(matches!(rx.recv(), Ok(Event::JobState { state, .. }) if state == "Completed"));
        let Ok(Event::JobFinished { reply, state, .. }) = rx.recv() else {
            panic!("expected a durable JobFinished event");
        };
        assert_eq!(state, "completed");
        assert!(!terminal_jobs.lock().unwrap().contains(&7));
        reply.send(Ok(())).unwrap();
        worker.join().unwrap();
        assert!(terminal_jobs.lock().unwrap().contains(&7));
    }
}
