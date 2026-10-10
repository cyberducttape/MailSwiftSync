//! Per-mailbox batch execution worker.

use super::batch::BatchExecutionMode;
use super::batch_worker::{OAuthRefreshLocks, rate_domain_path};
use super::pass_provenance::{record_pass_verification, transfer_pass_intent, verified_folders};
use super::rate_domains::{Admission, DomainKey, RateDomainLimiter, RateDomainPath, failure_sides};
use crate::{
    Event, StreamOutcome,
    bulk_import::BulkJob,
    controller::failure::{
        FailureClass, classified_failure_detail_for_provider, classify_failure_for_provider,
        should_retry_batch_error_for_provider, transient_retry_delay_for_provider,
    },
    core,
    credentials::CleanupGuard,
    imap_probe::{SidedProbeError, fresh_dual_imaps_authentication},
    process::ProcessLaunchLimiter,
    runner::{
        MessageVerificationResult, ResolvedImapsyncIdentity, RunContext, TerminalEvidenceSource,
        message_verification_enabled, persist_engine_identity_before_launch,
        run_dovecot_destination_preflight, run_dovecot_verification, run_imap_message_verification,
        run_streaming, send_reliable_event, terminal_evidence_source,
        validate_dovecot_checkpoint_context,
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
    time::Duration,
};

const JOB_FINISHED_ACK_TIMEOUT: Duration = Duration::from_secs(5);

fn provider_for_error(form: &crate::Form, error: &str) -> &'static str {
    let side = failure_sides(error);
    match side.as_slice() {
        [crate::imap_probe::MailSide::Source] => {
            provider_for_side(form, crate::imap_probe::MailSide::Source)
        }
        [crate::imap_probe::MailSide::Destination] => {
            provider_for_side(form, crate::imap_probe::MailSide::Destination)
        }
        // Do not guess the source provider when an engine diagnostic does
        // not identify which endpoint emitted it. Provider-specific
        // signatures stay disabled until side attribution is explicit.
        _ => crate::core::provider_intelligence::UNATTRIBUTED_PROVIDER,
    }
}

fn provider_for_side(form: &crate::Form, side: crate::imap_probe::MailSide) -> &'static str {
    let host = match side {
        crate::imap_probe::MailSide::Source => &form.profile.source_host,
        crate::imap_probe::MailSide::Destination => &form.profile.destination_host,
    };
    crate::core::provider_intelligence::canonical_provider(host)
}

fn credential_refresh_retryable(
    form: &crate::Form,
    failure: &CredentialRefreshFailure,
    attempt: usize,
    retry_count: usize,
) -> bool {
    should_retry_batch_error_for_provider(
        provider_for_side(form, failure.side),
        &failure.message,
        attempt,
        retry_count,
    )
}

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

/// Tell the operator which rate domains a capacity failure paused.
fn report_cooldowns(
    tx: &mpsc::SyncSender<Event>,
    limiter: &RateDomainLimiter,
    penalized: Vec<(DomainKey, std::time::Instant)>,
) {
    for (domain, until) in penalized {
        let (current_limit, configured_limit, consecutive_failures) =
            limiter.telemetry_snapshot(&domain).unwrap_or((1, 1, 0));
        let _ = tx.try_send(Event::ProviderCooldown {
            domain: domain.label(),
            provider: domain.provider(),
            endpoint: domain.endpoint().map(str::to_owned),
            until,
            current_limit,
            configured_limit,
            consecutive_failures,
        });
    }
}

fn adapt_launch_rate(
    limiter: &ProcessLaunchLimiter,
    penalized: &[(DomainKey, std::time::Instant)],
) {
    let domains = penalized
        .iter()
        .map(|(domain, _)| domain.clone())
        .collect::<Vec<_>>();
    limiter.observe_capacity_failure_for(&domains);
}

/// Shared, immutable inputs for running scheduled mailbox attempts.
pub(crate) struct BatchAttemptRunner {
    pub(crate) concurrency: usize,
    pub(crate) mode: BatchExecutionMode,
    pub(crate) retry_count: usize,
    pub(crate) tx: mpsc::SyncSender<Event>,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) failed: Arc<AtomicBool>,
    pub(crate) terminal_jobs: Arc<Mutex<HashSet<usize>>>,
    pub(crate) launch_limiter: Arc<ProcessLaunchLimiter>,
    pub(crate) provider_limiter: Arc<RateDomainLimiter>,
    pub(crate) batch_project_id: String,
    pub(crate) batch_run_id: String,
    pub(crate) resolved_imapsync: Arc<std::collections::HashMap<String, ResolvedImapsyncIdentity>>,
    pub(crate) oauth_refresh_locks: OAuthRefreshLocks,
    pub(crate) verification_state_path: Option<std::path::PathBuf>,
    pub(crate) diagnostic_logger: Option<Arc<crate::DiagnosticLogger>>,
}

/// One mailbox job and the progress it carries between scheduled attempts.
/// The scheduler owns it while it waits; a worker owns it only while an
/// admitted attempt runs.
pub(crate) struct MailboxTask {
    pub(crate) index: usize,
    job_id: String,
    child_run_id: String,
    checkpoint: Option<String>,
    label: String,
    form: crate::Form,
    pub(crate) rate_path: RateDomainPath,
    /// Next attempt number, starting at zero.
    attempt: usize,
    /// The job banner and engine identity have been emitted.
    introduced: bool,
    imapsync_output_profile: ImapsyncOutputProfile,
    claimed: bool,
    transfer_attempt_number: u32,
    verification_failure: Option<String>,
    /// Why the job could not be prepared from its durable plan.
    preparation_error: Option<String>,
}

impl MailboxTask {
    pub(crate) fn new(
        index: usize,
        job_id: String,
        child_run_id: String,
        checkpoint: Option<String>,
        job: &BulkJob,
    ) -> Self {
        let form = job.form();
        let rate_path = rate_domain_path(&form);
        Self {
            index,
            job_id,
            child_run_id,
            checkpoint,
            label: job.label.clone(),
            form,
            rate_path,
            attempt: 0,
            introduced: false,
            imapsync_output_profile: ImapsyncOutputProfile::Unknown,
            claimed: false,
            transfer_attempt_number: 0,
            verification_failure: None,
            preparation_error: None,
        }
    }

    /// A job whose plan or credentials could not be prepared. Its first
    /// attempt settles it as failed without contacting any server.
    pub(crate) fn unpreparable(
        index: usize,
        job_id: String,
        child_run_id: String,
        error: String,
    ) -> Self {
        let form = crate::Form::default();
        let rate_path = rate_domain_path(&form);
        Self {
            index,
            job_id,
            child_run_id,
            checkpoint: None,
            label: String::new(),
            form,
            rate_path,
            attempt: 0,
            introduced: false,
            imapsync_output_profile: ImapsyncOutputProfile::Unknown,
            claimed: false,
            transfer_attempt_number: 0,
            verification_failure: None,
            preparation_error: Some(error),
        }
    }
}

/// What one admitted attempt left for the scheduler to do.
pub(crate) enum AttemptOutcome {
    /// The job reached a terminal state, or its controller is gone.
    Finished,
    /// A transient failure: park the task until `delay` passes, then seek
    /// admission again. The attempt's rate-domain slots are already released.
    Retry {
        task: Box<MailboxTask>,
        delay: Duration,
    },
}

#[derive(Debug)]
struct CredentialRefreshFailure {
    side: crate::imap_probe::MailSide,
    message: String,
}

fn refresh_live_credentials(
    form: &mut crate::migration_plan::Form,
    locks: &OAuthRefreshLocks,
) -> Result<(), CredentialRefreshFailure> {
    if form.source_password.is_empty() && !form.profile.source_credential_id.trim().is_empty() {
        form.load_keyring_password(true)
            .map_err(|message| CredentialRefreshFailure {
                side: crate::imap_probe::MailSide::Source,
                message,
            })?;
    }
    if form.destination_password.is_empty()
        && !form.profile.destination_credential_id.trim().is_empty()
    {
        form.load_keyring_password(false)
            .map_err(|message| CredentialRefreshFailure {
                side: crate::imap_probe::MailSide::Destination,
                message,
            })?;
    }
    let refreshes = [
        (
            crate::imap_probe::MailSide::Source,
            true,
            form.profile.source_auth.clone(),
            form.profile.source_oauth_refresh_credential_id.clone(),
        ),
        (
            crate::imap_probe::MailSide::Destination,
            false,
            form.profile.destination_auth.clone(),
            form.profile.destination_oauth_refresh_credential_id.clone(),
        ),
    ];
    for (side, source, auth, refresh_id) in refreshes {
        if auth != "oauth2" || refresh_id.trim().is_empty() {
            continue;
        }
        let refresh_lock = {
            let mut lock_map = locks.lock().map_err(|_| CredentialRefreshFailure {
                side,
                message: "OAuth refresh lock registry was poisoned".to_owned(),
            })?;
            lock_map
                .entry(refresh_id.trim().to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = refresh_lock.lock().map_err(|_| CredentialRefreshFailure {
            side,
            message: "OAuth refresh lock was poisoned".to_owned(),
        })?;
        form.refresh_oauth_access_token(source)
            .map_err(|message| CredentialRefreshFailure { side, message })?;
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
    diagnostic_logger: Option<Arc<crate::DiagnosticLogger>>,
    oauth_refresh_locks: &'a OAuthRefreshLocks,
    transfer_attempt_number: &'a mut u32,
    verification_failure: &'a mut Option<String>,
    launch_limiter: &'a ProcessLaunchLimiter,
    launch_path: &'a RateDomainPath,
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
        diagnostic_logger,
        oauth_refresh_locks,
        transfer_attempt_number,
        verification_failure,
        launch_limiter,
        launch_path,
    } = context;
    let prepared = form.validated_plan().and_then(|plan| {
        plan.prepared_command_with_throttle_divisor_and_checkpoint(
            concurrency,
            checkpoint.as_deref(),
        )
    });
    match prepared {
        Ok(command) => {
            let transfer_pass = live.then(|| {
                transfer_pass_intent(
                    form,
                    &command.executable,
                    &command.args,
                    &command.cleanup,
                    checkpoint.as_deref(),
                )
            });
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
                    diagnostic_logger: diagnostic_logger.clone(),
                    attempt_number: *transfer_attempt_number,
                    transfer_pass: transfer_pass.as_ref(),
                    launch_limiter: Some((launch_limiter, launch_path)),
                })
            })
            .and_then(|stream| {
                if !form.dry_run
                    && form.engine() == core::Engine::ImapSync
                    && message_verification_enabled(form)
                {
                    let mut verification_form = form.clone();
                    let refresh_result = refresh_live_credentials(
                        &mut verification_form,
                        oauth_refresh_locks,
                    )
                    .map_err(|failure| failure.message);
                    let verification_result = refresh_result.and_then(|()| {
                        run_imap_message_verification(
                            &verification_form,
                            &job_id,
                            &child_run_id,
                            &cancel,
                            verification_state_path
                                .as_deref()
                                .map(|path| core::durable_stage_path(path, &job_id))
                                .as_deref(),
                        )
                    });
                    match verification_result {
                        Ok(MessageVerificationResult {
                            evidence,
                            mismatches,
                            folders,
                        }) => {
                            record_pass_verification(
                                &tx,
                                &child_run_id,
                                &job_id,
                                *transfer_attempt_number,
                                &evidence,
                                verified_folders(&project_id, &folders),
                            )
                            .map_err(|error| format!("pass verification record failed: {error}"))?;
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
                                crate::controller::failure::bounded_verification_error(&safe, 2048),
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
                    record_pass_verification(
                        &tx,
                        &child_run_id,
                        &job_id,
                        *transfer_attempt_number,
                        &evidence,
                        Vec::new(),
                    )
                    .map_err(|error| format!("pass verification record failed: {error}"))?;
                    send_reliable_event(&tx, Event::BatchEvidence {
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        evidence,
                        mismatches: core::MismatchSet::default(),
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
                    record_pass_verification(
                        &tx,
                        &child_run_id,
                        &job_id,
                        *transfer_attempt_number,
                        &verification_result.evidence,
                        Vec::new(),
                    )
                    .map_err(|error| format!("pass verification record failed: {error}"))?;
                    send_reliable_event(
                        &tx,
                        Event::BatchEvidence {
                            job_id: job_id.clone(),
                            child_run_id: child_run_id.clone(),
                            evidence: verification_result.evidence,
                            mismatches: core::MismatchSet::default(),
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

impl BatchAttemptRunner {
    fn line(&self, task: &MailboxTask, text: String) {
        send_run_line(&self.tx, &task.form, &task.child_run_id, &task.job_id, text);
    }

    fn terminal(
        &self,
        task: &MailboxTask,
        job_state: &str,
        run_state: &str,
        detail: String,
        credential_fingerprint: Option<String>,
    ) {
        persist_batch_terminal_state(BatchTerminalTransition {
            tx: &self.tx,
            terminal_jobs: &self.terminal_jobs,
            index: task.index,
            job_id: task.job_id.clone(),
            child_run_id: task.child_run_id.clone(),
            job_state,
            run_state,
            detail,
            credential_fingerprint,
        });
    }

    fn fail(&self, task: &MailboxTask, error: &str) {
        let provider = provider_for_error(&task.form, error);
        let cancelled =
            classify_failure_for_provider(provider, error) == FailureClass::Cancellation;
        if !cancelled {
            self.failed.store(true, Ordering::Relaxed);
        }
        self.terminal(
            task,
            if cancelled { "Cancelled" } else { "Failed" },
            if cancelled { "cancelled" } else { "failed" },
            classified_failure_detail_for_provider(provider, &redact_child_text(&task.form, error)),
            None,
        );
    }

    fn finish_transcript(&self, task: &MailboxTask) {
        if let Some(logger) = &self.diagnostic_logger
            && let Err(error) = logger.finish_run(&task.child_run_id)
        {
            eprintln!("could not finalize mailbox diagnostic transcript: {error}");
        }
    }

    /// Settle a task the scheduler will not run again because the batch was
    /// cancelled while it waited.
    pub(crate) fn cancel_waiting(&self, task: MailboxTask) {
        let detail = if task.introduced {
            "cancelled by operator"
        } else {
            "cancelled before worker claim"
        };
        self.terminal(&task, "Cancelled", "cancelled", detail.into(), None);
        if task.introduced {
            self.finish_transcript(&task);
        }
    }

    /// Settle a task whose rate domains can no longer be evaluated.
    pub(crate) fn fail_unschedulable(&self, task: MailboxTask, reason: &str) {
        self.line(&task, format!("[{}] {reason}", task.index + 1));
        self.fail(&task, reason);
        self.finish_transcript(&task);
    }

    /// Emit the job banner and persist the engine identity once per job.
    fn introduce(&self, task: &mut MailboxTask) {
        task.introduced = true;
        self.line(
            task,
            format!("══ Job {}: {} ══", task.index + 1, task.label),
        );
        if task.form.engine() != core::Engine::ImapSync {
            return;
        }
        let identity = self.resolved_imapsync.get(&task.form.profile.imapsync_path);
        let version = identity
            .map(|identity| identity.version.clone())
            .unwrap_or_else(|| "unknown".into());
        let profile = identity
            .map(|identity| identity.output_profile)
            .unwrap_or(ImapsyncOutputProfile::Unknown);
        if let Err(error) = persist_engine_identity_before_launch(
            &self.tx,
            &task.child_run_id,
            &task.job_id,
            &version,
        ) {
            self.line(
                task,
                format!(
                    "[{}] [verification] engine identity is not durable; output evidence disabled: {error}",
                    task.index + 1
                ),
            );
            return;
        }
        if profile == ImapsyncOutputProfile::Unknown {
            self.line(
                task,
                format!(
                    "[{}] {}",
                    task.index + 1,
                    crate::verification::unqualified_imapsync_message(&version)
                ),
            );
        }
        task.imapsync_output_profile = profile;
    }

    /// Report a transient failure and hand the task back for a timed retry.
    /// The caller must already have released the attempt's admission.
    fn retry_later(
        &self,
        mut task: MailboxTask,
        provider: &str,
        error: &str,
        what: &str,
    ) -> AttemptOutcome {
        self.line(
            &task,
            format!(
                "[{}] [{}] {what}; retrying: {error}",
                task.index + 1,
                classify_failure_for_provider(provider, error).label()
            ),
        );
        let _ = send_reliable_event(
            &self.tx,
            Event::JobState {
                job_id: task.job_id.clone(),
                child_run_id: task.child_run_id.clone(),
                state: "Retrying".into(),
            },
        );
        let delay = transient_retry_delay_for_provider(provider, error, task.attempt);
        let _ = self.tx.try_send(Event::RetryScheduled {
            job_id: task.job_id.clone(),
            attempt: u32::try_from(task.attempt + 2).unwrap_or(u32::MAX),
            max_retries: u32::try_from(self.retry_count).unwrap_or(u32::MAX),
            delay,
            failure_class: classify_failure_for_provider(provider, error).label(),
        });
        task.attempt += 1;
        AttemptOutcome::Retry {
            task: Box::new(task),
            delay,
        }
    }

    /// Run one admitted attempt of `task`. The admission is held for exactly
    /// this attempt and is released before the outcome is returned, so the
    /// scheduler can admit other work the moment this call ends.
    pub(crate) fn run_attempt(
        &self,
        mut task: MailboxTask,
        admission: Admission,
    ) -> AttemptOutcome {
        if self.cancel.load(Ordering::Relaxed) {
            drop(admission);
            self.cancel_waiting(task);
            return AttemptOutcome::Finished;
        }
        if let Some(error) = task.preparation_error.take() {
            drop(admission);
            self.failed.store(true, Ordering::Relaxed);
            self.line(&task, format!("[{}] {error}", task.index + 1));
            self.terminal(
                &task,
                "Failed",
                "failed",
                classified_failure_detail_for_provider(
                    provider_for_error(&task.form, &error),
                    &redact_child_text(&task.form, &error),
                ),
                None,
            );
            return AttemptOutcome::Finished;
        }
        if !task.introduced {
            self.introduce(&mut task);
        }
        let live = self.mode.is_live();
        let attempt = task.attempt;
        if live {
            // A queue-level admission must not mint an access token for
            // every selected mailbox. Refresh immediately before this
            // worker authenticates and launches the engine, so waiting in
            // a large queue cannot consume an expired bearer token.
            if let Err(error) = refresh_live_credentials(&mut task.form, &self.oauth_refresh_locks)
            {
                let provider = provider_for_side(&task.form, error.side);
                if credential_refresh_retryable(&task.form, &error, attempt, self.retry_count) {
                    let penalized = self.provider_limiter.observe_failure_on(
                        &admission,
                        &error.message,
                        vec![error.side],
                    );
                    adapt_launch_rate(&self.launch_limiter, &penalized);
                    report_cooldowns(&self.tx, &self.provider_limiter, penalized);
                    drop(admission);
                    return self.retry_later(
                        task,
                        provider,
                        &error.message,
                        "transient OAuth credential refresh failure",
                    );
                }
                drop(admission);
                self.failed.store(true, Ordering::Relaxed);
                self.line(
                    &task,
                    format!(
                        "[{}] OAuth credential refresh failed before launch: {}",
                        task.index + 1,
                        error.message
                    ),
                );
                self.terminal(
                    &task,
                    "Failed",
                    "failed",
                    classified_failure_detail_for_provider(
                        provider,
                        &redact_child_text(&task.form, &error.message),
                    ),
                    None,
                );
                self.finish_transcript(&task);
                return AttemptOutcome::Finished;
            }
            if let Err(SidedProbeError {
                side,
                message: error,
            }) = fresh_dual_imaps_authentication(&task.form)
            {
                let provider = provider_for_side(&task.form, side);
                if should_retry_batch_error_for_provider(
                    provider,
                    &error,
                    attempt,
                    self.retry_count,
                ) {
                    let penalized =
                        self.provider_limiter
                            .observe_failure_on(&admission, &error, vec![side]);
                    adapt_launch_rate(&self.launch_limiter, &penalized);
                    report_cooldowns(&self.tx, &self.provider_limiter, penalized);
                    drop(admission);
                    return self.retry_later(
                        task,
                        provider,
                        &error,
                        "transient fresh authentication probe failure",
                    );
                }
                drop(admission);
                self.failed.store(true, Ordering::Relaxed);
                self.line(
                    &task,
                    format!(
                        "[{}] fresh live authentication failed before launch: {error}",
                        task.index + 1
                    ),
                );
                // Classify the raw probe result. Prefixing it with
                // "authentication failed" would incorrectly mask a DNS,
                // TCP, TLS-disconnect, or provider-capacity failure.
                self.terminal(
                    &task,
                    "Failed",
                    "failed",
                    classified_failure_detail_for_provider(
                        provider,
                        &redact_child_text(&task.form, &error),
                    ),
                    None,
                );
                self.finish_transcript(&task);
                return AttemptOutcome::Finished;
            }
        }
        if !task.claimed {
            match claim_batch_job(
                &self.tx,
                &self.batch_project_id,
                &task.job_id,
                &self.batch_run_id,
                &task.child_run_id,
                &self.cancel,
            ) {
                Ok(()) => task.claimed = true,
                Err(BatchClaimError::ControllerUnavailable) => {
                    drop(admission);
                    self.failed.store(true, Ordering::Relaxed);
                    self.finish_transcript(&task);
                    return AttemptOutcome::Finished;
                }
                Err(BatchClaimError::Rejected(error)) => {
                    drop(admission);
                    self.line(&task, format!("[{}] {}", task.index + 1, error));
                    self.fail(&task, &error);
                    self.finish_transcript(&task);
                    return AttemptOutcome::Finished;
                }
            }
        }
        let _ = send_reliable_event(
            &self.tx,
            Event::JobState {
                job_id: task.job_id.clone(),
                child_run_id: task.child_run_id.clone(),
                state: "Running".into(),
            },
        );
        if attempt > 0 {
            self.line(
                &task,
                format!(
                    "[{}] retry attempt {attempt}/{}",
                    task.index + 1,
                    self.retry_count
                ),
            );
        }
        let mut transfer_attempt_number = task.transfer_attempt_number;
        let mut verification_failure = task.verification_failure.take();
        let result = run_prepared_batch_attempt(BatchAttemptContext {
            form: &task.form,
            checkpoint: task.checkpoint.clone(),
            concurrency: self.concurrency,
            live,
            index: task.index,
            job_id: task.job_id.clone(),
            child_run_id: task.child_run_id.clone(),
            project_id: self.batch_project_id.clone(),
            tx: self.tx.clone(),
            cancel: self.cancel.clone(),
            imapsync_output_profile: task.imapsync_output_profile,
            verification_state_path: self.verification_state_path.clone(),
            diagnostic_logger: self.diagnostic_logger.clone(),
            oauth_refresh_locks: &self.oauth_refresh_locks,
            transfer_attempt_number: &mut transfer_attempt_number,
            verification_failure: &mut verification_failure,
            launch_limiter: &self.launch_limiter,
            launch_path: &task.rate_path,
        });
        task.transfer_attempt_number = transfer_attempt_number;
        task.verification_failure = verification_failure;
        match result {
            Ok(outcome) => {
                self.provider_limiter.observe_success(&admission);
                drop(admission);
                let delta_required = outcome == StreamOutcome::DeltaRequired;
                if delta_required {
                    self.line(
                        &task,
                        format!(
                            "[{}] Dovecot reports an incomplete synchronization; another delta pass is required",
                            task.index + 1
                        ),
                    );
                }
                let run_state = if task.form.dry_run {
                    "ready"
                } else if delta_required {
                    "delta_required"
                } else {
                    "completed"
                };
                let detail = if delta_required {
                    "Dovecot reports that another delta pass is required".into()
                } else {
                    task.verification_failure.as_ref().map_or_else(
                        || "process completed".into(),
                        |reason| crate::controller::failure::incomplete_verification_detail(reason),
                    )
                };
                let fingerprint = task
                    .form
                    .dry_run
                    .then(|| task.form.credential_binding_fingerprint());
                self.terminal(
                    &task,
                    if delta_required {
                        "DeltaRequired"
                    } else {
                        "Completed"
                    },
                    run_state,
                    detail,
                    fingerprint,
                );
                self.finish_transcript(&task);
                AttemptOutcome::Finished
            }
            Err(error)
                if should_retry_batch_error_for_provider(
                    provider_for_error(&task.form, &error),
                    &error,
                    attempt,
                    self.retry_count,
                ) =>
            {
                let penalized = self.provider_limiter.observe_failure(&admission, &error);
                adapt_launch_rate(&self.launch_limiter, &penalized);
                report_cooldowns(&self.tx, &self.provider_limiter, penalized);
                drop(admission);
                let provider = provider_for_error(&task.form, &error);
                self.retry_later(task, provider, &error, "transient failure")
            }
            Err(error) => {
                drop(admission);
                let provider = provider_for_error(&task.form, &error);
                let failure_class = classify_failure_for_provider(provider, &error);
                let exhausted_retry_budget = attempt >= self.retry_count
                    && should_retry_batch_error_for_provider(provider, &error, attempt, usize::MAX);
                let detail = if exhausted_retry_budget {
                    format!(
                        "retry budget exhausted after {} retries: {error}",
                        self.retry_count
                    )
                } else {
                    error.clone()
                };
                self.line(
                    &task,
                    format!(
                        "[{}] [{}] failed: {detail}",
                        task.index + 1,
                        failure_class.label()
                    ),
                );
                self.fail(&task, &detail);
                self.finish_transcript(&task);
                AttemptOutcome::Finished
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn test_runner(tx: mpsc::SyncSender<Event>) -> BatchAttemptRunner {
        BatchAttemptRunner {
            concurrency: 2,
            mode: BatchExecutionMode::Preflight,
            retry_count: 2,
            tx,
            cancel: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
            terminal_jobs: Arc::new(Mutex::new(HashSet::new())),
            launch_limiter: Arc::new(ProcessLaunchLimiter::new(2)),
            provider_limiter: Arc::new(RateDomainLimiter::new(2)),
            batch_project_id: "project".into(),
            batch_run_id: "parent".into(),
            resolved_imapsync: Arc::new(std::collections::HashMap::new()),
            oauth_refresh_locks: Arc::new(Mutex::new(std::collections::HashMap::new())),
            verification_state_path: None,
            diagnostic_logger: None,
        }
    }

    fn test_unpreparable_task(index: usize) -> MailboxTask {
        MailboxTask::unpreparable(
            index,
            format!("job-{index}"),
            format!("run-{index}"),
            "durable plan could not be loaded".into(),
        )
    }

    fn test_refresh_task(index: usize) -> MailboxTask {
        let mut form = crate::Form::default();
        form.profile.engine = core::Engine::Dovecot;
        form.profile.source_host = "imap.gmail.com".into();
        form.profile.source_user = "operator@example.test".into();
        form.profile.source_auth = "oauth2".into();
        form.profile.source_oauth_refresh_credential_id = "source-refresh".into();
        let job = BulkJob::from_form(format!("refresh-{index}"), form, "queued".into());
        MailboxTask::new(
            index,
            format!("refresh-job-{index}"),
            format!("refresh-run-{index}"),
            None,
            &job,
        )
    }

    fn test_admission(runner: &BatchAttemptRunner, task: &MailboxTask) -> Admission {
        Arc::clone(&runner.provider_limiter)
            .try_admit(&task.rate_path)
            .unwrap()
    }

    #[test]
    fn ambiguous_engine_errors_do_not_inherit_the_source_provider() {
        let mut form = crate::Form::default();
        form.profile.source_host = "imap.gmail.com".into();
        form.profile.destination_host = "outlook.office365.com".into();

        assert_eq!(provider_for_error(&form, "Host1: rate limited"), "gmail");
        assert_eq!(
            provider_for_error(&form, "Host2: rate limited"),
            "microsoft365"
        );
        assert_eq!(
            provider_for_error(&form, "rate limited"),
            crate::core::provider_intelligence::UNATTRIBUTED_PROVIDER
        );
    }

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

        let detail = crate::controller::failure::classified_failure_detail(&redact_child_text(
            &form,
            "destination-secret-value was refused",
        ));
        assert!(!detail.contains("destination-secret-value"), "{detail}");
    }

    #[test]
    fn unpreparable_task_retains_only_the_failure_needed_for_terminal_reporting() {
        let task = test_unpreparable_task(3);
        assert_eq!(task.index, 3);
        assert_eq!(task.job_id, "job-3");
        assert_eq!(task.child_run_id, "run-3");
        assert_eq!(
            task.preparation_error.as_deref(),
            Some("durable plan could not be loaded")
        );
        assert!(!task.claimed);
        assert_eq!(task.transfer_attempt_number, 0);
    }

    #[test]
    fn refresh_live_credentials_is_a_noop_without_configured_refresh_tokens() {
        let mut form = crate::Form::default();
        let locks = Arc::new(Mutex::new(std::collections::HashMap::new()));
        refresh_live_credentials(&mut form, &locks).unwrap();
        assert!(locks.lock().unwrap().is_empty());
    }

    #[test]
    fn live_credential_refresh_failure_retains_the_endpoint_side() {
        let locks = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let poisoned = Arc::clone(&locks);
        assert!(
            thread::spawn(move || {
                let _guard = poisoned.lock().unwrap();
                panic!("poison refresh lock registry for test");
            })
            .join()
            .is_err()
        );

        let mut form = crate::Form::default();
        form.profile.source_auth = "oauth2".into();
        form.profile.source_oauth_refresh_credential_id = "source-refresh".into();
        let error = refresh_live_credentials(&mut form, &locks).unwrap_err();
        assert_eq!(error.side, crate::imap_probe::MailSide::Source);
        assert!(error.message.contains("lock registry was poisoned"));
    }

    #[test]
    fn live_refresh_failure_is_classified_on_its_side_and_settled_before_claim() {
        let locks = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let poisoned = Arc::clone(&locks);
        assert!(
            thread::spawn(move || {
                let _guard = poisoned.lock().unwrap();
                panic!("poison refresh lock registry for test");
            })
            .join()
            .is_err()
        );

        let (tx, rx) = mpsc::sync_channel(5);
        let mut runner = test_runner(tx);
        runner.mode = BatchExecutionMode::Live;
        runner.oauth_refresh_locks = locks;
        let failed = Arc::clone(&runner.failed);
        let terminal_jobs = Arc::clone(&runner.terminal_jobs);
        let task = test_refresh_task(9);
        let admission = test_admission(&runner, &task);
        let worker = thread::spawn(move || runner.run_attempt(task, admission));

        assert!(
            matches!(rx.recv().unwrap(), Event::RunLine { text, .. } if text.contains("Job 10"))
        );
        assert!(
            matches!(rx.recv().unwrap(), Event::RunLine { text, .. } if text.contains("OAuth credential refresh failed before launch"))
        );
        assert!(matches!(
            rx.recv().unwrap(),
            Event::JobState { state, .. } if state == "Failed"
        ));
        let Event::JobFinished {
            state,
            reply,
            detail,
            ..
        } = rx.recv().unwrap()
        else {
            panic!("expected durable OAuth refresh failure");
        };
        assert_eq!(state, "failed");
        assert!(detail.contains("authentication") || detail.contains("unknown"));
        assert!(!terminal_jobs.lock().unwrap().contains(&9));
        reply.send(Ok(())).unwrap();
        assert!(matches!(worker.join().unwrap(), AttemptOutcome::Finished));
        assert!(failed.load(Ordering::Relaxed));
        assert!(terminal_jobs.lock().unwrap().contains(&9));
        assert!(
            rx.try_recv().is_err(),
            "refresh failure must not claim or launch"
        );
    }

    #[test]
    fn transient_refresh_errors_retry_by_provider_but_auth_failures_do_not() {
        let mut form = crate::Form::default();
        form.profile.destination_host = "outlook.office365.com".into();
        let throttled = CredentialRefreshFailure {
            side: crate::imap_probe::MailSide::Destination,
            message: "Request is throttled. Suggested Backoff Time: 12000 milliseconds".into(),
        };
        assert!(credential_refresh_retryable(&form, &throttled, 0, 2));
        assert!(!credential_refresh_retryable(&form, &throttled, 2, 2));

        let rejected = CredentialRefreshFailure {
            side: crate::imap_probe::MailSide::Destination,
            message: "invalid_grant: refresh token expired".into(),
        };
        assert!(!credential_refresh_retryable(&form, &rejected, 0, 2));
    }

    #[test]
    fn retry_transition_increments_attempt_and_reports_the_failure_class() {
        let (tx, rx) = mpsc::sync_channel(4);
        let runner = test_runner(tx);
        let task = test_unpreparable_task(5);
        let worker = thread::spawn(move || {
            runner.retry_later(
                task,
                "generic",
                "Host2: NO [UNAVAILABLE] server busy",
                "transient failure",
            )
        });

        assert!(
            matches!(rx.recv().unwrap(), Event::RunLine { text, .. } if text.contains("transient failure"))
        );
        assert!(matches!(
            rx.recv().unwrap(),
            Event::JobState { state, .. } if state == "Retrying"
        ));
        let Event::RetryScheduled {
            attempt,
            max_retries,
            failure_class,
            delay,
            ..
        } = rx.recv().unwrap()
        else {
            panic!("expected a durable retry schedule");
        };
        assert_eq!(attempt, 2);
        assert_eq!(max_retries, 2);
        assert_eq!(failure_class, "capacity");
        assert!(!delay.is_zero());
        let AttemptOutcome::Retry { task, .. } = worker.join().unwrap() else {
            panic!("transient failure must return the task to the scheduler");
        };
        assert_eq!(task.attempt, 1);
    }

    #[test]
    fn introduction_of_non_imapsync_job_emits_banner_without_external_engine_lookup() {
        let (tx, rx) = mpsc::sync_channel(1);
        let runner = test_runner(tx);
        let mut task = test_unpreparable_task(6);
        task.label = "Dovecot mailbox".into();
        task.form.profile.engine = core::Engine::Dovecot;

        runner.introduce(&mut task);

        assert!(task.introduced);
        assert!(
            matches!(rx.recv().unwrap(), Event::RunLine { text, .. } if text.contains("Dovecot mailbox"))
        );
    }

    #[test]
    fn waiting_cancellation_is_durably_settled_without_marking_batch_failed() {
        let (tx, rx) = mpsc::sync_channel(4);
        let runner = test_runner(tx);
        let failed = Arc::clone(&runner.failed);
        let worker = thread::spawn(move || runner.cancel_waiting(test_unpreparable_task(0)));

        assert!(matches!(
            rx.recv().unwrap(),
            Event::JobState { state, .. } if state == "Cancelled"
        ));
        let Event::JobFinished { state, reply, .. } = rx.recv().unwrap() else {
            panic!("expected the durable cancellation transition");
        };
        assert_eq!(state, "cancelled");
        reply.send(Ok(())).unwrap();
        worker.join().unwrap();
        assert!(!failed.load(Ordering::Relaxed));
    }

    #[test]
    fn unschedulable_task_is_failed_only_after_durable_terminal_ack() {
        let (tx, rx) = mpsc::sync_channel(4);
        let runner = test_runner(tx);
        let failed = Arc::clone(&runner.failed);
        let terminal_jobs = Arc::clone(&runner.terminal_jobs);
        let worker = thread::spawn(move || {
            runner.fail_unschedulable(test_unpreparable_task(4), "rate-domain path unavailable")
        });

        assert!(
            matches!(rx.recv().unwrap(), Event::RunLine { text, .. } if text.contains("rate-domain path unavailable"))
        );
        assert!(matches!(
            rx.recv().unwrap(),
            Event::JobState { state, .. } if state == "Failed"
        ));
        let Event::JobFinished {
            state,
            reply,
            detail,
            ..
        } = rx.recv().unwrap()
        else {
            panic!("expected the durable failure transition");
        };
        assert_eq!(state, "failed");
        assert!(detail.contains("rate-domain path unavailable"));
        assert!(!terminal_jobs.lock().unwrap().contains(&4));
        reply.send(Ok(())).unwrap();
        worker.join().unwrap();
        assert!(failed.load(Ordering::Relaxed));
        assert!(terminal_jobs.lock().unwrap().contains(&4));
    }

    #[test]
    fn run_attempt_cancellation_settles_without_claiming_or_failing_batch() {
        let (tx, rx) = mpsc::sync_channel(4);
        let runner = test_runner(tx);
        let cancelled = Arc::clone(&runner.cancel);
        cancelled.store(true, Ordering::Relaxed);
        let failed = Arc::clone(&runner.failed);
        let task = test_unpreparable_task(7);
        let admission = test_admission(&runner, &task);
        let worker = thread::spawn(move || runner.run_attempt(task, admission));

        assert!(matches!(
            rx.recv().unwrap(),
            Event::JobState { state, .. } if state == "Cancelled"
        ));
        let Event::JobFinished { state, reply, .. } = rx.recv().unwrap() else {
            panic!("expected durable cancellation before claim");
        };
        assert_eq!(state, "cancelled");
        reply.send(Ok(())).unwrap();
        assert!(matches!(worker.join().unwrap(), AttemptOutcome::Finished));
        assert!(!failed.load(Ordering::Relaxed));
        assert!(rx.try_recv().is_err(), "cancelled work must not be claimed");
    }

    #[test]
    fn run_attempt_preparation_failure_is_terminal_without_claiming() {
        let (tx, rx) = mpsc::sync_channel(4);
        let runner = test_runner(tx);
        let failed = Arc::clone(&runner.failed);
        let terminal_jobs = Arc::clone(&runner.terminal_jobs);
        let task = test_unpreparable_task(8);
        let admission = test_admission(&runner, &task);
        let worker = thread::spawn(move || runner.run_attempt(task, admission));

        assert!(
            matches!(rx.recv().unwrap(), Event::RunLine { text, .. } if text.contains("durable plan could not be loaded"))
        );
        assert!(matches!(
            rx.recv().unwrap(),
            Event::JobState { state, .. } if state == "Failed"
        ));
        let Event::JobFinished { state, reply, .. } = rx.recv().unwrap() else {
            panic!("expected durable preparation failure");
        };
        assert_eq!(state, "failed");
        assert!(!terminal_jobs.lock().unwrap().contains(&8));
        reply.send(Ok(())).unwrap();
        assert!(matches!(worker.join().unwrap(), AttemptOutcome::Finished));
        assert!(failed.load(Ordering::Relaxed));
        assert!(terminal_jobs.lock().unwrap().contains(&8));
        assert!(
            rx.try_recv().is_err(),
            "unpreparable work must not be claimed"
        );
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
