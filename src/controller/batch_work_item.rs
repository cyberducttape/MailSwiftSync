//! Per-mailbox batch execution worker.

use super::batch::BatchExecutionMode;
use crate::{
    Event, StreamOutcome,
    bulk_import::BulkJob,
    controller::failure::{
        FailureClass, classified_failure_detail, classify_failure, is_transient_batch_error,
        transient_retry_delay,
    },
    core,
    credentials::CleanupGuard,
    imap_probe::fresh_dual_imaps_authentication,
    process::ProcessLaunchLimiter,
    runner::{
        RunContext, run_dovecot_destination_preflight, run_dovecot_verification, run_streaming,
    },
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
    } = context;
    let live = mode.is_live();
    while let Ok((index, job_id, child_run_id, checkpoint, job)) = job_rx.recv() {
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(Event::JobState {
                job_id: job_id.clone(),
                child_run_id: child_run_id.clone(),
                state: "Cancelled".into(),
            });
            let _ = tx.send(Event::JobFinished {
                job_id: job_id.clone(),
                child_run_id: child_run_id.clone(),
                state: "cancelled".into(),
                detail: "cancelled before worker claim".into(),
                credential_fingerprint: None,
            });
            if let Ok(mut terminal) = terminal_jobs.lock() {
                terminal.insert(index);
            }
            continue;
        }
        let _ = tx.send(Event::RunLine {
            run_id: child_run_id.clone(),
            job_id: job_id.clone(),
            text: format!("══ Job {}: {} ══", index + 1, job.label),
        });
        let mut completed = false;
        let mut delta_required = false;
        let mut claimed = false;
        for attempt in 0..=retry_count {
            if !launch_limiter.acquire(&cancel) {
                break;
            }
            if live && let Err(error) = fresh_dual_imaps_authentication(&job.form) {
                failed.store(true, Ordering::Relaxed);
                let _ = tx.send(Event::RunLine {
                    run_id: child_run_id.clone(),
                    job_id: job_id.clone(),
                    text: format!(
                        "[{}] fresh live authentication failed before launch: {error}",
                        index + 1
                    ),
                });
                let _ = tx.send(Event::JobState {
                    job_id: job_id.clone(),
                    child_run_id: child_run_id.clone(),
                    state: "Failed".into(),
                });
                let _ = tx.send(Event::JobFinished {
                    job_id: job_id.clone(),
                    child_run_id: child_run_id.clone(),
                    state: "failed".into(),
                    detail: classified_failure_detail(&format!(
                        "fresh live authentication failed before launch: {error}"
                    )),
                    credential_fingerprint: None,
                });
                if let Ok(mut terminal) = terminal_jobs.lock() {
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
                    let _ = tx.send(Event::JobState {
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        state: if cancelled { "Cancelled" } else { "Failed" }.into(),
                    });
                    let _ = tx.send(Event::JobFinished {
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        state: if cancelled { "cancelled" } else { "failed" }.into(),
                        detail: classified_failure_detail(&error),
                        credential_fingerprint: None,
                    });
                    if let Ok(mut terminal) = terminal_jobs.lock() {
                        terminal.insert(index);
                    }
                    break;
                }
                claimed = true;
            }
            let _ = tx.send(Event::JobState {
                job_id: job_id.clone(),
                child_run_id: child_run_id.clone(),
                state: "Running".into(),
            });
            if attempt > 0 {
                let _ = tx.send(Event::RunLine {
                    run_id: child_run_id.clone(),
                    job_id: job_id.clone(),
                    text: format!("[{}] retry attempt {attempt}/{retry_count}", index + 1),
                });
                let _ = tx.send(Event::JobState {
                    job_id: job_id.clone(),
                    child_run_id: child_run_id.clone(),
                    state: "Running".into(),
                });
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
                        prefix: &prefix,
                        cancel: &cancel,
                        secrets: &secrets,
                        timeout: Duration::from_secs(
                            job.form.profile.migration_timeout_hours * 60 * 60,
                        ),
                        dovecot_exit_two_is_delta: job.form.engine() == core::Engine::Dovecot
                            && !job.form.dry_run,
                    })
                    .map(|stream| {
                        if !job.form.dry_run
                            && let Some(evidence) = stream.imapsync_evidence
                        {
                            let _ = tx.send(Event::BatchEvidence {
                                job_id: job_id.clone(),
                                child_run_id: child_run_id.clone(),
                                evidence,
                            });
                        }
                        stream.outcome
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
                            .map(|evidence| {
                                let _ = tx.send(Event::BatchEvidence {
                                    job_id: job_id.clone(),
                                    child_run_id: child_run_id.clone(),
                                    evidence,
                                });
                                outcome
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
                Err(error)
                    if classify_failure(&error) != FailureClass::Cancellation
                        && attempt < retry_count
                        && is_transient_batch_error(&error) =>
                {
                    let _ = tx.send(Event::RunLine {
                        run_id: child_run_id.clone(),
                        job_id: job_id.clone(),
                        text: format!(
                            "[{}] [{}] transient failure; retrying: {error}",
                            index + 1,
                            classify_failure(&error).label()
                        ),
                    });
                    let _ = tx.send(Event::JobState {
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        state: "Retrying".into(),
                    });
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
                    let _ = tx.send(Event::JobState {
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        state: if cancelled { "Cancelled" } else { "Failed" }.into(),
                    });
                    let _ = tx.send(Event::JobFinished {
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        state: if cancelled { "cancelled" } else { "failed" }.into(),
                        detail: classified_failure_detail(&error),
                        credential_fingerprint: None,
                    });
                    if let Ok(mut terminal) = terminal_jobs.lock() {
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
            let _ = tx.send(Event::JobState {
                job_id: job_id.clone(),
                child_run_id: child_run_id.clone(),
                state: if delta_required {
                    "DeltaRequired"
                } else {
                    "Completed"
                }
                .into(),
            });
            let _ = tx.send(Event::JobFinished {
                job_id: job_id.clone(),
                child_run_id: child_run_id.clone(),
                state: terminal_state.into(),
                detail: if delta_required {
                    "Dovecot reports that another delta pass is required".into()
                } else {
                    "process completed".into()
                },
                credential_fingerprint: if job.form.dry_run {
                    Some(job.form.credential_fingerprint())
                } else {
                    None
                },
            });
            if let Ok(mut terminal) = terminal_jobs.lock() {
                terminal.insert(index);
            }
        } else if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(Event::JobState {
                job_id: job_id.clone(),
                child_run_id: child_run_id.clone(),
                state: "Cancelled".into(),
            });
            let _ = tx.send(Event::JobFinished {
                job_id: job_id.clone(),
                child_run_id: child_run_id.clone(),
                state: "cancelled".into(),
                detail: "cancelled by operator".into(),
                credential_fingerprint: None,
            });
            if let Ok(mut terminal) = terminal_jobs.lock() {
                terminal.insert(index);
            }
        }
    }
}
