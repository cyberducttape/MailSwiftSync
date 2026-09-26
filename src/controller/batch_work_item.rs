//! Per-mailbox batch execution worker.

use super::batch::BatchExecutionMode;
use super::batch_worker::OAuthRefreshLocks;
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
        send_reliable_event, terminal_evidence_source,
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
    pub(crate) batch_project_id: String,
    pub(crate) batch_run_id: String,
    pub(crate) resolved_imapsync: Arc<std::collections::HashMap<String, ResolvedImapsyncIdentity>>,
    pub(crate) oauth_refresh_locks: OAuthRefreshLocks,
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
        batch_project_id,
        batch_run_id,
        resolved_imapsync,
        oauth_refresh_locks,
    } = context;
    let live = mode.is_live();
    while let Ok((index, job_id, child_run_id, checkpoint, mut job)) = job_rx.recv() {
        if cancel.load(Ordering::Relaxed) {
            let _ = send_reliable_event(
                &tx,
                Event::JobState {
                    job_id: job_id.clone(),
                    child_run_id: child_run_id.clone(),
                    state: "Cancelled".into(),
                },
            );
            if let Err(error) = send_job_finished(
                &tx,
                job_id.clone(),
                child_run_id.clone(),
                "cancelled".into(),
                "cancelled before worker claim".into(),
                None,
            ) {
                eprintln!("durable batch terminal event delivery failed: {error}");
            } else if let Ok(mut terminal) = terminal_jobs.lock() {
                terminal.insert(index);
            }
            continue;
        }
        let _ = tx.send(Event::RunLine {
            run_id: child_run_id.clone(),
            job_id: job_id.clone(),
            text: format!("══ Job {}: {} ══", index + 1, job.label),
        });
        let imapsync_output_profile = if job.form.engine() == core::Engine::ImapSync {
            let identity = resolved_imapsync.get(&job.form.profile.imapsync_path);
            let version = identity
                .map(|identity| identity.version.clone())
                .unwrap_or_else(|| "unknown".into());
            let profile = identity
                .map(|identity| identity.output_profile)
                .unwrap_or(ImapsyncOutputProfile::Unknown);
            let identity_persisted = if let Err(error) =
                persist_engine_identity_before_launch(&tx, &child_run_id, &job_id, &version)
            {
                let _ = tx.send(Event::RunLine {
                    run_id: child_run_id.clone(),
                    job_id: job_id.clone(),
                    text: format!(
                        "[{}] [verification] engine identity is not durable; output evidence disabled: {error}",
                        index + 1
                    ),
                });
                false
            } else {
                true
            };
            if identity_persisted && profile == ImapsyncOutputProfile::Unknown {
                let _ = tx.send(Event::RunLine {
                    run_id: child_run_id.clone(),
                    job_id: job_id.clone(),
                    text: format!(
                        "[{}] {}",
                        index + 1,
                        crate::verification::unqualified_imapsync_message(&version)
                    ),
                });
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
        for attempt in 0..=retry_count {
            if !launch_limiter.acquire(&cancel) {
                break;
            }
            if live {
                // A queue-level admission must not mint an access token for
                // every selected mailbox. Refresh immediately before this
                // worker authenticates and launches the engine, so waiting in
                // a large queue cannot consume an expired bearer token.
                if let Err(error) = refresh_live_credentials(&mut job.form, &oauth_refresh_locks) {
                    failed.store(true, Ordering::Relaxed);
                    let _ = tx.send(Event::RunLine {
                        run_id: child_run_id.clone(),
                        job_id: job_id.clone(),
                        text: format!(
                            "[{}] OAuth credential refresh failed before launch: {error}",
                            index + 1
                        ),
                    });
                    let _ = send_reliable_event(
                        &tx,
                        Event::JobState {
                            job_id: job_id.clone(),
                            child_run_id: child_run_id.clone(),
                            state: "Failed".into(),
                        },
                    );
                    if let Err(delivery_error) = send_job_finished(
                        &tx,
                        job_id.clone(),
                        child_run_id.clone(),
                        "failed".into(),
                        classified_failure_detail(&error),
                        None,
                    ) {
                        eprintln!("durable batch terminal event delivery failed: {delivery_error}");
                    } else if let Ok(mut terminal) = terminal_jobs.lock() {
                        terminal.insert(index);
                    }
                    break;
                }
            }
            if live && let Err(error) = fresh_dual_imaps_authentication(&job.form) {
                if should_retry_batch_error(&error, attempt, retry_count) {
                    let _ = tx.send(Event::RunLine {
                        run_id: child_run_id.clone(),
                        job_id: job_id.clone(),
                        text: format!(
                            "[{}] [{}] transient fresh authentication probe failure; retrying: {error}",
                            index + 1,
                            classify_failure(&error).label()
                        ),
                    });
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
                let _ = tx.send(Event::RunLine {
                    run_id: child_run_id.clone(),
                    job_id: job_id.clone(),
                    text: format!(
                        "[{}] fresh live authentication failed before launch: {error}",
                        index + 1
                    ),
                });
                let _ = send_reliable_event(
                    &tx,
                    Event::JobState {
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        state: "Failed".into(),
                    },
                );
                // Classify the raw probe result. Prefixing it with
                // "authentication failed" would incorrectly mask a DNS,
                // TCP, TLS-disconnect, or provider-capacity failure.
                if let Err(delivery_error) = send_job_finished(
                    &tx,
                    job_id.clone(),
                    child_run_id.clone(),
                    "failed".into(),
                    classified_failure_detail(&error),
                    None,
                ) {
                    eprintln!("durable batch terminal event delivery failed: {delivery_error}");
                } else if let Ok(mut terminal) = terminal_jobs.lock() {
                    terminal.insert(index);
                }
                break;
            }
            if !claimed {
                let (claim_tx, claim_rx) = mpsc::sync_channel(1);
                if tx
                    .send(Event::ClaimBatch {
                        project_id: batch_project_id.clone(),
                        job_id: job_id.clone(),
                        parent_run_id: batch_run_id.clone(),
                        child_run_id: child_run_id.clone(),
                        reply: claim_tx,
                    })
                    .is_err()
                {
                    failed.store(true, Ordering::Relaxed);
                    break;
                }
                let claim_result = loop {
                    if cancel.load(Ordering::Relaxed) {
                        break Err("cancelled by operator before durable claim".to_owned());
                    }
                    match claim_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(result) => break result,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            break Err("durable claim response was lost".to_owned());
                        }
                    }
                };
                if let Err(error) = claim_result {
                    let cancelled = classify_failure(&error) == FailureClass::Cancellation;
                    if !cancelled {
                        failed.store(true, Ordering::Relaxed);
                    }
                    let _ = tx.send(Event::RunLine {
                        run_id: child_run_id.clone(),
                        job_id: job_id.clone(),
                        text: format!("[{}] {}", index + 1, error),
                    });
                    let _ = send_reliable_event(
                        &tx,
                        Event::JobState {
                            job_id: job_id.clone(),
                            child_run_id: child_run_id.clone(),
                            state: if cancelled { "Cancelled" } else { "Failed" }.into(),
                        },
                    );
                    if let Err(delivery_error) = send_job_finished(
                        &tx,
                        job_id.clone(),
                        child_run_id.clone(),
                        if cancelled { "cancelled" } else { "failed" }.into(),
                        classified_failure_detail(&error),
                        None,
                    ) {
                        eprintln!("durable batch terminal event delivery failed: {delivery_error}");
                    } else if let Ok(mut terminal) = terminal_jobs.lock() {
                        terminal.insert(index);
                    }
                    break;
                }
                claimed = true;
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
                let _ = tx.send(Event::RunLine {
                    run_id: child_run_id.clone(),
                    job_id: job_id.clone(),
                    text: format!("[{}] retry attempt {attempt}/{retry_count}", index + 1),
                });
                let _ = send_reliable_event(
                    &tx,
                    Event::JobState {
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        state: "Running".into(),
                    },
                );
            }
            let prepared = job
                .form
                .prepared_command_with_throttle_divisor_and_checkpoint(
                    concurrency,
                    checkpoint.as_deref(),
                );
            let result = match prepared {
                Ok(command) => {
                    let cleanup_guard = CleanupGuard::new(command.cleanup.clone());
                    let prefix = format!("[{}] ", index + 1);
                    let secrets = [
                        job.form.source_password.clone(),
                        job.form.destination_password.clone(),
                    ];
                    let result = run_streaming(RunContext {
                        executable: &command.executable,
                        args: &command.args,
                        env: &command.env,
                        tx: &tx,
                        run_id: &child_run_id,
                        job_id: &job_id,
                        project_id: &batch_project_id,
                        prefix: &prefix,
                        cancel: &cancel,
                        secrets: &secrets,
                        timeout: Duration::from_secs(
                            job.form.profile.migration_timeout_hours * 60 * 60,
                        ),
                        dovecot_exit_two_is_delta: job.form.engine() == core::Engine::Dovecot
                            && !job.form.dry_run,
                        imapsync_output_profile,
                        diagnostic_logger: None,
                    })
                    .and_then(|stream| {
                        if !job.form.dry_run
                            && job.form.engine() == core::Engine::ImapSync
                            && message_verification_enabled(&job.form)
                        {
                            match run_imap_message_verification(
                                &job.form,
                                &job_id,
                                &child_run_id,
                                &cancel,
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
                                    let safe = crate::ui::redact_secrets(
                                        &error,
                                        [
                                            job.form.source_password.as_str(),
                                            job.form.destination_password.as_str(),
                                        ],
                                    );
                                    verification_failure = Some(
                                        safe.chars().take(2048).collect::<String>(),
                                    );
                                    let _ = tx.send(Event::RunLine {
                                        run_id: child_run_id.clone(),
                                        job_id: job_id.clone(),
                                        text: format!(
                                            "[{}] message-level verification unavailable; transfer succeeded and requires review: {error}",
                                            index + 1
                                        ),
                                    });
                                }
                            }
                        } else if !job.form.dry_run
                            && terminal_evidence_source(
                                &job.form,
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
                        Ok(stream.outcome)
                    });
                    let result = if result.is_ok()
                        && job.form.dry_run
                        && job.form.engine() == core::Engine::Dovecot
                    {
                        result.and_then(|outcome| {
                            run_dovecot_destination_preflight(
                                &job.form.dovecot_destination_preflight_commands(),
                                &tx,
                                &cancel,
                                Duration::from_secs(
                                    job.form.profile.migration_timeout_hours * 60 * 60,
                                ),
                                &format!("[{}] ", index + 1),
                                &child_run_id,
                                &job_id,
                            )
                            .map(|_| outcome)
                        })
                    } else {
                        result
                    };
                    drop(cleanup_guard);
                    if result.is_ok()
                        && !job.form.dry_run
                        && job.form.engine() == core::Engine::Dovecot
                    {
                        let verification = job.form.dovecot_verification_commands(false);
                        let verification_secret = job.form.source_password.clone();
                        let verification_env = if job.form.local_doveadm() {
                            vec![(
                                "MAILSWIFTSYNC_IMAPC_PASSWORD".into(),
                                verification_secret.clone(),
                            )]
                        } else {
                            Vec::new()
                        };
                        result.and_then(|outcome| {
                            run_dovecot_verification(
                                &verification,
                                &verification_env,
                                std::slice::from_ref(&verification_secret),
                                &tx,
                                &cancel,
                                Duration::from_secs(
                                    job.form.profile.migration_timeout_hours * 60 * 60,
                                ),
                                &format!("[{}] ", index + 1),
                                &child_run_id,
                                &job_id,
                            )
                            .and_then(|evidence| {
                                send_reliable_event(
                                    &tx,
                                    Event::BatchEvidence {
                                        job_id: job_id.clone(),
                                        child_run_id: child_run_id.clone(),
                                        evidence,
                                        mismatches: Vec::new(),
                                    },
                                )
                                .map(|_| outcome)
                                .map_err(|error| {
                                    format!("batch verification evidence delivery failed: {error}")
                                })
                            })
                        })
                    } else {
                        result
                    }
                }
                Err(error) => Err(error),
            };
            match result {
                Ok(outcome) => {
                    if outcome == StreamOutcome::DeltaRequired {
                        let _ = tx.send(Event::RunLine {
                                            run_id: child_run_id.clone(),
                                            job_id: job_id.clone(),
                                            text: format!(
                                                "[{}] Dovecot reports an incomplete synchronization; another delta pass is required",
                                                index + 1
                                            ),
                                        });
                        delta_required = true;
                    }
                    completed = true;
                    break;
                }
                Err(error) if should_retry_batch_error(&error, attempt, retry_count) => {
                    let _ = tx.send(Event::RunLine {
                        run_id: child_run_id.clone(),
                        job_id: job_id.clone(),
                        text: format!(
                            "[{}] [{}] transient failure; retrying: {error}",
                            index + 1,
                            classify_failure(&error).label()
                        ),
                    });
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
                    let _ = tx.send(Event::RunLine {
                        run_id: child_run_id.clone(),
                        job_id: job_id.clone(),
                        text: format!(
                            "[{}] [{}] failed: {error}",
                            index + 1,
                            classify_failure(&error).label()
                        ),
                    });
                    let _ = send_reliable_event(
                        &tx,
                        Event::JobState {
                            job_id: job_id.clone(),
                            child_run_id: child_run_id.clone(),
                            state: if cancelled { "Cancelled" } else { "Failed" }.into(),
                        },
                    );
                    if let Err(delivery_error) = send_job_finished(
                        &tx,
                        job_id.clone(),
                        child_run_id.clone(),
                        if cancelled { "cancelled" } else { "failed" }.into(),
                        classified_failure_detail(&error),
                        None,
                    ) {
                        eprintln!("durable batch terminal event delivery failed: {delivery_error}");
                    } else if let Ok(mut terminal) = terminal_jobs.lock() {
                        terminal.insert(index);
                    }
                    break;
                }
            }
        }
        if completed {
            let terminal_state = if job.form.dry_run {
                "ready"
            } else if delta_required {
                "delta_required"
            } else {
                "completed"
            };
            let _ = send_reliable_event(
                &tx,
                Event::JobState {
                    job_id: job_id.clone(),
                    child_run_id: child_run_id.clone(),
                    state: if delta_required {
                        "DeltaRequired"
                    } else {
                        "Completed"
                    }
                    .into(),
                },
            );
            if let Err(error) = send_job_finished(
                &tx,
                job_id.clone(),
                child_run_id.clone(),
                terminal_state.into(),
                if delta_required {
                    "Dovecot reports that another delta pass is required".into()
                } else {
                    verification_failure.map_or_else(
                        || "process completed".into(),
                        |reason| format!("message-level verification incomplete: {reason}"),
                    )
                },
                if job.form.dry_run {
                    Some(job.form.credential_binding_fingerprint())
                } else {
                    None
                },
            ) {
                eprintln!("durable batch terminal event delivery failed: {error}");
            } else if let Ok(mut terminal) = terminal_jobs.lock() {
                terminal.insert(index);
            }
        } else if cancel.load(Ordering::Relaxed) {
            let _ = send_reliable_event(
                &tx,
                Event::JobState {
                    job_id: job_id.clone(),
                    child_run_id: child_run_id.clone(),
                    state: "Cancelled".into(),
                },
            );
            if let Err(error) = send_job_finished(
                &tx,
                job_id.clone(),
                child_run_id.clone(),
                "cancelled".into(),
                "cancelled by operator".into(),
                None,
            ) {
                eprintln!("durable batch terminal event delivery failed: {error}");
            } else if let Ok(mut terminal) = terminal_jobs.lock() {
                terminal.insert(index);
            }
        }
    }
}
