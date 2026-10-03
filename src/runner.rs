use std::{
    collections::HashSet,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

mod folder_policy;
pub(crate) use folder_policy::{
    apply_explicit_folder_mapping, infer_automap_folder_mapping, validate_destination_folder_policy,
};

const PROCESS_REGISTRATION_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const RELIABLE_EVENT_SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// Minimum spacing of throttled progress snapshots per engine process.
const PROGRESS_EVENT_INTERVAL: Duration = Duration::from_millis(500);
/// Durable progress is less frequent than presentation telemetry so a large
/// mailbox cannot turn the SQLite event log into a per-message transcript.
const DURABLE_PROGRESS_EVENT_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) type OutputObserver = Arc<dyn Fn(&str) + Send + Sync>;

/// Reliable lifecycle events must not wait forever behind lossy diagnostic
/// output. A timeout is deliberately fail-closed: the caller reports the
/// durability failure and cleanup can proceed instead of retaining secrets or
/// a child-process guard indefinitely.
pub(crate) fn send_reliable_event(
    tx: &mpsc::SyncSender<crate::Event>,
    mut event: crate::Event,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + RELIABLE_EVENT_SEND_TIMEOUT;
    loop {
        match tx.try_send(event) {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Full(returned)) => {
                event = returned;
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Err("reliable lifecycle event queue remained full".into());
                }
                thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err("reliable lifecycle event channel disconnected".into());
            }
        }
    }
}

use crate::{
    BoundedLineBuffer, Event, MAX_DIAGNOSTIC_LINE_BYTES, MAX_PROCESS_TAIL_BYTES,
    MAX_PROCESS_TAIL_LINES, StreamOutcome, core,
    credentials::SecretString,
    process::{
        ProcessOutcome, attach_child_supervisor, collect_redacted_lines_with_callback,
        configure_process_group, for_each_lossy_line, process_identity, terminate_process_group,
        wait_with_timeout,
    },
    truncate_utf8, verification,
};

#[derive(Debug)]
pub(crate) struct StreamResult {
    pub(crate) outcome: StreamOutcome,
    pub(crate) imapsync_evidence: Option<core::MailboxEvidence>,
    pub(crate) dovecot_checkpoint: Option<String>,
}

/// Whether this plan carries the metadata needed for exact message-level
/// verification. A disabled capability is a review condition, not a failed
/// transfer.
pub(crate) fn message_verification_enabled(form: &crate::Form) -> bool {
    !form.profile.justfolders
        && !form.profile.addheader
        // imapsync owns --automap semantics. Until its preflight mapping is
        // captured and bound to the run, the verifier must not recreate that
        // decision from SPECIAL-USE/name heuristics after the transfer.
        && !form.profile.automap
        && form.profile.sync_internaldates
        && !form.profile.allowsizemismatch
}

const MAX_BODY_HASH_BYTES_PER_MESSAGE: u64 = 64 * 1024 * 1024;
const MAX_BODY_HASH_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;

pub(crate) fn validate_body_hash_limits(form: &crate::Form) -> Result<(), String> {
    if !form.profile.body_hash_verification {
        return Ok(());
    }
    if form.engine() != crate::core::Engine::ImapSync {
        return Err(
            "body-hash verification is currently available only for encrypted imapsync runs".into(),
        );
    }
    let per_message = form.profile.body_hash_max_bytes;
    let total = form.profile.body_hash_max_total_bytes;
    if per_message == 0 || total == 0 {
        return Err(
            "body-hash verification requires non-zero per-message and total byte bounds".into(),
        );
    }
    if per_message > MAX_BODY_HASH_BYTES_PER_MESSAGE {
        return Err(format!(
            "body-hash per-message bound exceeds the {}-byte safety limit",
            MAX_BODY_HASH_BYTES_PER_MESSAGE
        ));
    }
    if total > MAX_BODY_HASH_TOTAL_BYTES || total < per_message {
        return Err(format!(
            "body-hash total bound must be at least the per-message bound and no more than {} bytes",
            MAX_BODY_HASH_TOTAL_BYTES
        ));
    }
    if !message_verification_enabled(form) {
        return Err(
            "body-hash verification requires the same immutable metadata-preservation plan as independent verification"
                .into(),
        );
    }
    Ok(())
}

pub(crate) fn automap_blocks_live_certification(form: &crate::Form) -> bool {
    form.engine() == core::Engine::ImapSync && form.profile.automap
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalEvidenceSource {
    Independent,
    Engine,
    Unavailable,
}

/// Select the only evidence source allowed to determine a terminal state.
/// Live imapsync engine counters are telemetry when the plan cannot support
/// independent metadata reconciliation; they are never certification.
pub(crate) fn terminal_evidence_source(
    form: &crate::Form,
    independent_available: bool,
    engine_available: bool,
) -> TerminalEvidenceSource {
    if !form.dry_run && form.engine() == core::Engine::ImapSync {
        if message_verification_enabled(form) && independent_available {
            TerminalEvidenceSource::Independent
        } else {
            TerminalEvidenceSource::Unavailable
        }
    } else if independent_available {
        TerminalEvidenceSource::Independent
    } else if engine_available {
        TerminalEvidenceSource::Engine
    } else {
        TerminalEvidenceSource::Unavailable
    }
}

/// Perform independent, folder-aware IMAP reconciliation after a successful
/// imapsync transfer. Engine counters remain useful as a fallback, but they
/// cannot prove portable message identity; this adapter fetches Message-ID,
/// INTERNALDATE, and RFC822.SIZE from both accounts and feeds the bounded
/// records into the shared verifier. Full RFC822 bodies are intentionally not
/// downloaded by the default path; forensic body hashing must be opt-in.
pub(crate) fn run_imap_message_verification(
    form: &crate::Form,
    job_id: &str,
    run_id: &str,
    cancel: &AtomicBool,
    durable_stage_path: Option<&Path>,
) -> Result<MessageVerificationResult, String> {
    if !message_verification_enabled(form) {
        return Err(
            "message-level verification is unavailable for this migration plan; refusing to claim exact evidence for automap without an immutable engine mapping, justfolders, addheader, disabled internal-date sync, or allowed size mismatches"
                .into(),
        );
    }
    validate_body_hash_limits(form)?;
    let budget = crate::imap_probe::MessageFetchBudget::new(
        Duration::from_secs(form.profile.migration_timeout_hours * 60 * 60),
        cancel,
    );
    let state_budget = crate::imap_probe::MessageStateBudget::new();
    let body_hash_budget = crate::imap_probe::BodyHashBudget::new(
        usize::try_from(form.profile.body_hash_max_total_bytes)
            .map_err(|_| "body-hash total bound does not fit the platform usize".to_owned())?,
    );
    let body_hash =
        form.profile
            .body_hash_verification
            .then(|| crate::imap_probe::BodyHashOptions {
                max_body_bytes: usize::try_from(form.profile.body_hash_max_bytes)
                    .expect("body-hash per-message bound validated above"),
                budget: &body_hash_budget,
            });
    let source_host = crate::imap_probe::endpoint_for_probe(
        &form.profile.source_host,
        &form.profile.source_port,
    )?;
    let destination_host = crate::imap_probe::endpoint_for_probe(
        &form.profile.destination_host,
        &form.profile.destination_port,
    )?;
    let mut excluded_source_folders = form
        .profile
        .folder_mapping_rules
        .iter()
        .filter(|rule| rule.exclude)
        .map(|rule| rule.source.clone())
        .collect::<HashSet<_>>();
    let mut stage = if let Some(path) = durable_stage_path {
        let identity = crate::plan_identity::fingerprint_digest(&form.plan_fingerprint());
        core::MessageMetadataStage::open_durable(path.to_owned(), &identity)?
    } else {
        core::MessageMetadataStage::open_ephemeral()?
    };
    let source = crate::imap_probe::fetch_tls_account_messages_to_stage_with_body_hashes_excluding(
        &source_host,
        &form.profile.source_user,
        form.source_password.as_str(),
        &form.profile.source_auth,
        &form.profile.source_tls,
        &form.profile.source_ca_bundle,
        &form.profile.source_certificate_pin_sha256,
        &budget,
        &state_budget,
        body_hash.as_ref(),
        &excluded_source_folders,
        &mut stage,
        core::StagedMessageSide::Source,
    )?;
    let destination = crate::imap_probe::fetch_tls_account_messages_to_stage_with_body_hashes(
        &destination_host,
        &form.profile.destination_user,
        form.destination_password.as_str(),
        &form.profile.destination_auth,
        crate::effective_destination_tls(&form.profile.destination_tls),
        &form.profile.destination_ca_bundle,
        &form.profile.destination_certificate_pin_sha256,
        &budget,
        &state_budget,
        body_hash.as_ref(),
        &mut stage,
        core::StagedMessageSide::Destination,
    )?;
    if (source.total_exists > 0
        && stage
            .count(core::StagedMessageSide::Source)
            .map_err(|e| e.to_string())?
            == 0)
        || (destination.total_exists > 0
            && stage
                .count(core::StagedMessageSide::Destination)
                .map_err(|e| e.to_string())?
                == 0)
    {
        return Err(
            "message-level verification refused: IMAP FETCH extraction was empty despite non-empty EXISTS evidence"
                .into(),
        );
    }
    let mut folder_mapping = infer_automap_folder_mapping(
        &source.mailbox_details,
        &destination.mailbox_details,
        form.profile.automap,
    )?;
    apply_explicit_folder_mapping(
        &mut folder_mapping,
        &mut excluded_source_folders,
        &form.profile.folder_mapping_rules,
    );
    let expected_destination_folders = source
        .mailboxes
        .iter()
        .filter(|folder| !excluded_source_folders.contains(*folder))
        .map(|folder| {
            folder_mapping
                .get(folder)
                .cloned()
                .unwrap_or_else(|| folder.clone())
        })
        .collect::<HashSet<_>>();
    validate_destination_folder_policy(
        &expected_destination_folders,
        &destination.mailboxes,
        form.profile.delete2,
    )?;
    let (mismatches, summary) = if form.profile.body_hash_verification {
        for side in [
            core::StagedMessageSide::Source,
            core::StagedMessageSide::Destination,
        ] {
            let count = stage.count(side).map_err(|error| error.to_string())?;
            if count > crate::imap_probe::MAX_BODY_HASH_MESSAGES_PER_ENDPOINT as u64 {
                return Err(format!(
                    "body-hash verification refuses to load more than {} messages per endpoint into the forensic reconciler (requested {count})",
                    crate::imap_probe::MAX_BODY_HASH_MESSAGES_PER_ENDPOINT
                ));
            }
        }
        let source_messages = stage
            .all_messages(core::StagedMessageSide::Source)
            .map_err(|error| {
                format!("could not load source messages for body verification: {error}")
            })?;
        let destination_messages = stage
            .all_messages(core::StagedMessageSide::Destination)
            .map_err(|error| {
                format!("could not load destination messages for body verification: {error}")
            })?;
        let source_fingerprints = stage.content_fingerprints(core::StagedMessageSide::Source);
        let destination_fingerprints =
            stage.content_fingerprints(core::StagedMessageSide::Destination);
        if source_fingerprints.len() != source_messages.len()
            || destination_fingerprints.len() != destination_messages.len()
        {
            return Err("body-hash verification refused incomplete content fingerprints".into());
        }
        core::MessageVerification::detect_mismatches_with_content_fingerprints(
            job_id,
            run_id,
            &source_messages,
            &destination_messages,
            &source_fingerprints,
            &destination_fingerprints,
            &folder_mapping,
        )?
    } else {
        stage.reset_reconciliation()?;
        core::MessageVerification::detect_mismatches_from_stage(
            job_id,
            run_id,
            &stage,
            &folder_mapping,
        )?
    };
    let source_bytes = stage
        .sum_bytes(core::StagedMessageSide::Source)
        .map_err(|e| e.to_string())?;
    let destination_bytes = stage
        .sum_bytes(core::StagedMessageSide::Destination)
        .map_err(|e| e.to_string())?;
    let source_folders = source.mailboxes.len() as u64;
    let destination_folders = destination.mailboxes.len() as u64;
    let verification_method = if form.profile.body_hash_verification {
        core::VerificationMethod::BodyHash
    } else {
        core::VerificationMethod::MetadataReconciliation
    };
    let verification_outcome = match summary.verification_outcome() {
        core::VerificationOutcome::ExactMetadataMatch
            if verification_method == core::VerificationMethod::BodyHash =>
        {
            core::VerificationOutcome::ExactBodyMatch
        }
        outcome => outcome,
    };
    let evidence = core::MailboxEvidence {
        verification_method,
        verification_outcome: Some(verification_outcome),
        source_messages: summary.total_source,
        destination_messages: summary.total_destination,
        source_bytes,
        destination_bytes,
        // `unmatched_messages` is a literal unresolved count. Probable
        // metadata pairings are candidates, not unresolved messages.
        unmatched_messages: Some(
            summary
                .missing_count
                .saturating_add(summary.extra_count)
                .saturating_add(summary.duplicated_count)
                .saturating_add(summary.changed_count),
        ),
        failed_messages: 0,
        source_folders,
        destination_folders,
        authoritative: false,
        missing_messages: summary.missing_count,
        extra_messages: summary.extra_count.saturating_add(summary.duplicated_count),
        modified_messages: summary.changed_count,
        probable_messages: summary.probable_matches,
    };
    // Capture each folder's cursor before a durable stage is removed; the
    // caller records them (by folder digest) as the pass's verified ranges.
    let folders = stage
        .folder_cursors()
        .map_err(|error| format!("could not read verification folder cursors: {error}"))?;
    if durable_stage_path.is_some() {
        stage.finish()?;
    }
    Ok(MessageVerificationResult {
        evidence,
        mismatches,
        folders,
    })
}

/// Outcome of independent message-level verification of one mailbox.
pub(crate) struct MessageVerificationResult {
    pub(crate) evidence: core::MailboxEvidence,
    pub(crate) mismatches: Vec<core::MessageMismatch>,
    /// Per-folder verification cursors; folder names never leave the process.
    pub(crate) folders: Vec<core::FolderCursor>,
}

/// Inputs for one externally executed migration process. Keeping these
/// related values together prevents callers from accidentally pairing a
/// command with the wrong run/job identity or cancellation channel.
pub(crate) struct RunContext<'a> {
    pub(crate) executable: &'a str,
    pub(crate) args: &'a [String],
    pub(crate) env: &'a [(String, SecretString)],
    pub(crate) tx: &'a mpsc::SyncSender<crate::Event>,
    pub(crate) run_id: &'a str,
    pub(crate) job_id: &'a str,
    pub(crate) project_id: &'a str,
    pub(crate) prefix: &'a str,
    pub(crate) cancel: &'a AtomicBool,
    pub(crate) secrets: &'a [SecretString],
    pub(crate) timeout: Duration,
    pub(crate) dovecot_exit_two_is_delta: bool,
    pub(crate) imapsync_output_profile: verification::ImapsyncOutputProfile,
    pub(crate) diagnostic_logger: Option<Arc<crate::DiagnosticLogger>>,
    pub(crate) attempt_number: u32,
    /// Provenance of a live transfer attempt. `Some` exactly when this
    /// process is a live transfer whose attempt boundaries must be durable.
    pub(crate) transfer_pass: Option<&'a core::TransferPassIntent>,
    /// Shared process-start budget. The token is taken immediately before
    /// `spawn`, after every admission, authentication, claim, and preparation
    /// step, so time spent waiting upstream cannot bank tokens into a burst.
    pub(crate) launch_limiter: Option<(
        &'a crate::process::ProcessLaunchLimiter,
        &'a crate::controller::rate_domains::RateDomainPath,
    )>,
}

pub(crate) fn run_streaming(context: RunContext<'_>) -> Result<StreamResult, String> {
    let RunContext {
        executable,
        args,
        env,
        tx,
        run_id,
        job_id,
        project_id,
        prefix,
        cancel,
        secrets,
        timeout,
        dovecot_exit_two_is_delta,
        imapsync_output_profile,
        diagnostic_logger,
        attempt_number,
        transfer_pass,
        launch_limiter,
    } = context;
    let mut command = execution_command(executable, args, env)?;
    let launch_admitted_at = if let Some((limiter, path)) = launch_limiter {
        Some(limiter.acquire_scoped_at(path, cancel).ok_or_else(|| {
            if cancel.load(Ordering::Relaxed) {
                format!("{executable} launch cancelled before process start")
            } else {
                format!("{executable} launch refused: process-start limiter unavailable")
            }
        })?)
    } else {
        None
    };
    let mut child = spawn_retrying_busy_executable(&mut command)
        .map_err(|error| format!("could not start {executable}: {error}"))?;
    if let (Some((limiter, path)), Some(admitted_at)) = (launch_limiter, launch_admitted_at) {
        limiter.observe_success_for(path, admitted_at);
    }
    let mut release_stdin = child.stdin.take();
    let _child_supervisor = match attach_child_supervisor(&child) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err(format!(
                "could not establish descendant process supervision for {executable}: {error}"
            ));
        }
    };
    let identity = process_identity(child.id());
    let (start_ticks, process_group, session_id) = identity
        .map(|(start, group, session)| (Some(start), Some(group), Some(session)))
        .unwrap_or((None, None, None));
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stdout pipe unavailable; child cancelled before supervision".into());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stderr pipe unavailable; child cancelled before supervision".into());
        }
    };
    let out_tx = tx.clone();
    let out_prefix = prefix.to_owned();
    let out_secrets = secrets.to_vec();
    let tail = Arc::new(Mutex::new(BoundedLineBuffer::new()));
    let evidence = Arc::new(Mutex::new(
        verification::ImapsyncEvidenceAccumulator::default(),
    ));
    let dropped_diagnostics = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let failed_diagnostic_writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let out_tail = Arc::clone(&tail);
    let out_evidence = Arc::clone(&evidence);
    let dovecot_checkpoint = Arc::new(Mutex::new(None::<String>));
    let out_dovecot_checkpoint = Arc::clone(&dovecot_checkpoint);
    let out_dropped_diagnostics = Arc::clone(&dropped_diagnostics);
    let out_run_id = run_id.to_owned();
    let out_job_id = job_id.to_owned();
    let out_project_id = project_id.to_owned();
    let out_logger = diagnostic_logger.clone();
    let out_failed_diagnostic_writes = Arc::clone(&failed_diagnostic_writes);
    let progress = Arc::new(Mutex::new(crate::progress::TransferProgress::default()));
    let out_progress = Arc::clone(&progress);
    let durable_progress = transfer_pass.is_some();
    let mut last_progress_event: Option<std::time::Instant> = None;
    let mut last_durable_progress_event: Option<std::time::Instant> = None;
    let out_thread = thread::spawn(move || {
        for_each_lossy_line(stdout, |line| {
            let safe = crate::process::redact_known_secrets(
                line,
                out_secrets.iter().map(SecretString::as_str),
            );
            record_process_tail(&out_tail, &safe);
            if let Some(logger) = &out_logger
                && logger
                    .write_line(&out_project_id, &out_run_id, &out_job_id, "stdout", &safe)
                    .is_err()
            {
                out_failed_diagnostic_writes.fetch_add(1, Ordering::Relaxed);
            }
            if let Ok(mut evidence) = out_evidence.lock() {
                evidence.observe(&safe);
            }
            // Counters advance on this lossless reader; only the throttled
            // snapshot uses the best-effort presentation channel.
            let snapshot = out_progress.lock().ok().and_then(|mut progress| {
                (progress.observe(&safe)
                    && last_progress_event
                        .is_none_or(|sent| sent.elapsed() >= PROGRESS_EVENT_INTERVAL))
                .then_some(*progress)
            });
            if let Some(snapshot) = snapshot {
                last_progress_event = Some(std::time::Instant::now());
                let _ = out_tx.try_send(Event::Progress {
                    run_id: out_run_id.clone(),
                    job_id: out_job_id.clone(),
                    progress: snapshot,
                });
                if durable_progress
                    && last_durable_progress_event
                        .is_none_or(|sent| sent.elapsed() >= DURABLE_PROGRESS_EVENT_INTERVAL)
                {
                    last_durable_progress_event = Some(std::time::Instant::now());
                    let _ = out_tx.try_send(Event::TransferProgressCheckpoint {
                        run_id: out_run_id.clone(),
                        job_id: out_job_id.clone(),
                        attempt: attempt_number,
                        progress: snapshot,
                    });
                }
            }
            if dovecot_exit_two_is_delta
                && let Some(candidate) = dovecot_state_candidate(&safe)
                && let Ok(mut checkpoint) = out_dovecot_checkpoint.lock()
            {
                *checkpoint = Some(candidate);
            }
            if out_tx
                .try_send(Event::RunLine {
                    run_id: out_run_id.clone(),
                    job_id: out_job_id.clone(),
                    text: format!("{out_prefix}{safe}"),
                })
                .is_err()
            {
                out_dropped_diagnostics.fetch_add(1, Ordering::Relaxed);
            }
        })
    });
    let err_tx = tx.clone();
    let err_prefix = prefix.to_owned();
    let err_secrets = secrets.to_vec();
    let err_tail = Arc::clone(&tail);
    let err_evidence = Arc::clone(&evidence);
    let err_dropped_diagnostics = Arc::clone(&dropped_diagnostics);
    let err_run_id = run_id.to_owned();
    let err_job_id = job_id.to_owned();
    let err_project_id = project_id.to_owned();
    let err_logger = diagnostic_logger.clone();
    let err_failed_diagnostic_writes = Arc::clone(&failed_diagnostic_writes);
    let err_thread = thread::spawn(move || {
        for_each_lossy_line(stderr, |line| {
            let safe = crate::process::redact_known_secrets(
                line,
                err_secrets.iter().map(SecretString::as_str),
            );
            record_process_tail(&err_tail, &safe);
            if let Some(logger) = &err_logger
                && logger
                    .write_line(&err_project_id, &err_run_id, &err_job_id, "stderr", &safe)
                    .is_err()
            {
                err_failed_diagnostic_writes.fetch_add(1, Ordering::Relaxed);
            }
            if let Ok(mut evidence) = err_evidence.lock() {
                evidence.observe(&safe);
            }
            if err_tx
                .try_send(Event::RunLine {
                    run_id: err_run_id.clone(),
                    job_id: err_job_id.clone(),
                    text: format!("{err_prefix}[stderr] {safe}"),
                })
                .is_err()
            {
                err_dropped_diagnostics.fetch_add(1, Ordering::Relaxed);
            }
        })
    });
    // Start both drainers before the reliable lifecycle send. If the
    // bounded event queue is temporarily full, this send may wait, but the
    // child pipes are already being drained and cannot deadlock the engine.
    let (registration_tx, registration_rx) = mpsc::sync_channel(1);
    let registration_deadline = std::time::Instant::now() + PROCESS_REGISTRATION_ACK_TIMEOUT;
    let mut process_started_event = Some(Event::ProcessStarted(
        run_id.to_owned(),
        job_id.to_owned(),
        child.id(),
        start_ticks,
        process_group,
        session_id,
        executable.to_owned(),
        registration_tx,
    ));
    let process_started = loop {
        if cancel.load(Ordering::Relaxed) {
            break false;
        }
        let remaining = registration_deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break false;
        }
        let Some(event) = process_started_event.take() else {
            break false;
        };
        match tx.try_send(event) {
            Ok(()) => break true,
            Err(mpsc::TrySendError::Full(event)) => {
                process_started_event = Some(event);
                thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => break false,
        }
    };
    let mut transfer_attempt_started = false;
    let result = if process_started {
        let registration = loop {
            if cancel.load(Ordering::Relaxed) {
                break Err("cancelled before durable process registration".to_owned());
            }
            let remaining =
                registration_deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break Err("durable process registration acknowledgement timed out".to_owned());
            }
            match registration_rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err("durable process registration response was lost".to_owned());
                }
            }
        };
        if let Err(error) = registration {
            cancel.store(true, Ordering::Relaxed);
            let _ = wait_with_timeout(&mut child, timeout.min(Duration::from_secs(5)), cancel);
            Err(format!(
                "process registration failed; child cancelled: {error}"
            ))
        } else if let Some(intent) = transfer_pass
            && let Err(error) = record_transfer_attempt(
                tx,
                run_id,
                job_id,
                attempt_number,
                crate::controller::TransferAttemptStatus::Started(Box::new(intent.clone())),
            )
        {
            cancel.store(true, Ordering::Relaxed);
            let _ = wait_with_timeout(&mut child, timeout.min(Duration::from_secs(5)), cancel);
            Err(format!(
                "transfer attempt could not be durably recorded: {error}"
            ))
        } else {
            transfer_attempt_started = transfer_pass.is_some();
            if let Err(error) = release_engine(&mut release_stdin) {
                cancel.store(true, Ordering::Relaxed);
                let _ = wait_with_timeout(&mut child, timeout.min(Duration::from_secs(5)), cancel);
                Err(format!("engine release failed; child cancelled: {error}"))
            } else {
                let debug_supervision = process_supervision_debug_enabled();
                if debug_supervision {
                    eprintln!("[process-debug] streaming wait for child {}", child.id());
                }
                match wait_with_timeout(&mut child, timeout, cancel) {
                    Err(error) => Err(error.to_string()),
                    Ok(ProcessOutcome {
                        cancelled: true, ..
                    }) => Err("cancelled by operator".into()),
                    Ok(ProcessOutcome {
                        timed_out: true, ..
                    }) => Err("migration exceeded its configured execution timeout".into()),
                    Ok(ProcessOutcome {
                        exit_code: Some(0), ..
                    }) => Ok(StreamOutcome::Completed),
                    Ok(ProcessOutcome {
                        exit_code: Some(2), ..
                    }) if dovecot_exit_two_is_delta => Ok(StreamOutcome::DeltaRequired),
                    Ok(ProcessOutcome { exit_code, .. }) => {
                        Err(format!("process exited with code {:?}", exit_code))
                    }
                }
            }
        }
    } else {
        // ProcessStarted is the reliable hand-off to the durable controller.
        // Continuing after a disconnected event channel would leave a live
        // child with no possible process registration or cancellation owner.
        cancel.store(true, Ordering::Relaxed);
        let _ = wait_with_timeout(&mut child, timeout.min(Duration::from_secs(5)), cancel);
        Err("process-start event channel disconnected; child cancelled before supervision".into())
    };
    if process_supervision_debug_enabled() {
        eprintln!("[process-debug] streaming wait returned; joining stdout reader");
    }
    let stdout_reader = out_thread
        .join()
        .map_err(|_| "stdout reader thread panicked".to_owned());
    if process_supervision_debug_enabled() {
        eprintln!("[process-debug] stdout reader joined; joining stderr reader");
    }
    let stderr_reader = err_thread
        .join()
        .map_err(|_| "stderr reader thread panicked".to_owned());
    if process_supervision_debug_enabled() {
        eprintln!("[process-debug] stderr reader joined");
    }
    let reader_error = stdout_reader
        .as_ref()
        .err()
        .cloned()
        .or_else(|| stderr_reader.as_ref().err().cloned())
        .or_else(|| {
            stdout_reader
                .as_ref()
                .ok()
                .and_then(|result| result.as_ref().err())
                .map(|error| format!("stdout reader failed: {error}"))
        })
        .or_else(|| {
            stderr_reader
                .as_ref()
                .ok()
                .and_then(|result| result.as_ref().err())
                .map(|error| format!("stderr reader failed: {error}"))
        });
    // The readers have joined: deliver the final counters reliably so the
    // cockpit does not stop at the last throttled snapshot.
    let final_progress = progress.lock().ok().map(|progress| *progress);
    if let Some(final_progress) = final_progress.filter(|progress| progress.has_activity()) {
        let _ = send_reliable_event(
            tx,
            Event::Progress {
                run_id: run_id.to_owned(),
                job_id: job_id.to_owned(),
                progress: final_progress,
            },
        );
        if durable_progress {
            let _ = send_reliable_event(
                tx,
                Event::TransferProgressCheckpoint {
                    run_id: run_id.to_owned(),
                    job_id: job_id.to_owned(),
                    attempt: attempt_number,
                    progress: final_progress,
                },
            );
        }
    }
    let dropped = dropped_diagnostics.load(Ordering::Relaxed);
    if dropped > 0 {
        // The reader threads have joined, so the content-free accounting
        // event can be sent reliably without blocking a child pipe.
        let _ = send_reliable_event(
            tx,
            Event::DiagnosticLinesDropped {
                run_id: run_id.to_owned(),
                job_id: job_id.to_owned(),
                count: dropped as u64,
            },
        );
    }
    let mut failed_diagnostic_writes = failed_diagnostic_writes.load(Ordering::Relaxed);
    // Both drainers have joined. Close this run's transcript off the drainer
    // path; the writer reports lines it accepted but could not persist.
    if let Some(logger) = &diagnostic_logger {
        match logger.finish_run(run_id) {
            Ok(failed) => failed_diagnostic_writes += failed,
            Err(error) => {
                let _ = send_reliable_event(
                    tx,
                    Event::RunLine {
                        run_id: run_id.to_owned(),
                        job_id: job_id.to_owned(),
                        text: format!(
                            "{prefix}[diagnostics] Diagnostic log not finalized: {error}"
                        ),
                    },
                );
            }
        }
    }
    if failed_diagnostic_writes > 0 {
        let _ = send_reliable_event(
            tx,
            Event::RunLine {
                run_id: run_id.to_owned(),
                job_id: job_id.to_owned(),
                text: format!(
                    "{prefix}[diagnostics] Diagnostic log unavailable; {failed_diagnostic_writes} line(s) were not persisted"
                ),
            },
        );
    }
    let attempt_outcome = if let Some(error) = reader_error.as_deref() {
        crate::controller::TransferAttemptOutcome::Failed {
            failure_class: crate::controller::failure::classify_failure(error).label(),
        }
    } else {
        match &result {
            Ok(StreamOutcome::Completed) => crate::controller::TransferAttemptOutcome::Completed,
            Ok(StreamOutcome::DeltaRequired) => {
                crate::controller::TransferAttemptOutcome::DeltaRequired
            }
            Err(error) => crate::controller::TransferAttemptOutcome::Failed {
                failure_class: crate::controller::failure::classify_failure(error).label(),
            },
        }
    };
    let attempt_record_error = if transfer_attempt_started {
        // Counts only: what the engine itself reported, and the digest of
        // any new resume state it emitted. Neither carries mailbox content.
        let completion = core::TransferPassCompletion {
            engine_counters: evidence
                .lock()
                .ok()
                .and_then(|collector| collector.evidence(imapsync_output_profile))
                .map(|evidence| core::EngineCompletionCounters {
                    source_folders: evidence.source_folders,
                    destination_folders: evidence.destination_folders,
                    source_messages: evidence.source_messages,
                    destination_messages: evidence.destination_messages,
                    source_bytes: evidence.source_bytes,
                    destination_bytes: evidence.destination_bytes,
                    unmatched_messages: evidence.unmatched_messages,
                }),
            emitted_state_sha256: dovecot_checkpoint
                .lock()
                .ok()
                .and_then(|state| state.clone())
                .map(|state| core::sha256_hex(state.as_bytes())),
        };
        record_transfer_attempt(
            tx,
            run_id,
            job_id,
            attempt_number,
            crate::controller::TransferAttemptStatus::Finished {
                outcome: attempt_outcome,
                completion,
            },
        )
        .err()
    } else {
        None
    };
    // The process identity is only valid for this attempt. Clear it before
    // returning so a transient retry (or a crash during its backoff) cannot
    // leave an exited PID looking like an active orphan.
    let process_end_error = send_reliable_event(
        tx,
        Event::ProcessEnded {
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
        },
    )
    .err()
    .map(|error| format!("could not clear durable process identity: {error}"));
    let result = match attempt_record_error {
        Some(error) => Err(match result {
            Ok(_) => format!("transfer attempt result was not durably recorded: {error}"),
            Err(prior) => {
                format!("{prior}; transfer attempt result was not durably recorded: {error}")
            }
        }),
        None => result,
    };
    match result {
        Ok(outcome) if reader_error.is_none() => {
            if let Some(error) = process_end_error {
                return Err(error);
            }
            let imapsync_evidence = evidence
                .lock()
                .map_err(|_| "imapsync evidence collector was poisoned".to_owned())?;
            let imapsync_evidence = imapsync_evidence.evidence(imapsync_output_profile);
            let checkpoint = dovecot_checkpoint
                .lock()
                .map_err(|_| "Dovecot checkpoint collector was poisoned".to_owned())?
                .clone();
            Ok(StreamResult {
                outcome,
                imapsync_evidence,
                dovecot_checkpoint: checkpoint,
            })
        }
        result => {
            let reader_error = reader_error.unwrap_or_default();
            let process_error = match result {
                Ok(_) => String::new(),
                Err(error) => error,
            };
            let error = [
                process_error,
                reader_error,
                process_end_error.unwrap_or_default(),
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("; ");
            let recent = process_tail_text(&tail);
            if recent.is_empty() {
                Err(error)
            } else {
                Err(format!("{error}; recent output: {recent}"))
            }
        }
    }
}

fn record_transfer_attempt(
    tx: &mpsc::SyncSender<crate::Event>,
    run_id: &str,
    job_id: &str,
    attempt: u32,
    status: crate::controller::TransferAttemptStatus,
) -> Result<(), String> {
    if attempt == 0 {
        return Err("transfer attempt number must be positive".into());
    }
    let (reply, acknowledgement) = mpsc::sync_channel(1);
    send_reliable_event(
        tx,
        crate::Event::TransferAttempt {
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
            attempt,
            status,
            reply,
        },
    )?;
    acknowledgement
        .recv_timeout(PROCESS_REGISTRATION_ACK_TIMEOUT)
        .map_err(|_| "durable transfer-attempt acknowledgement timed out".to_owned())?
}

/// Dovecot emits the stateful-sync resume value as a compact, single-line
/// standard-base64 token. Both padded and unpadded forms are accepted; the
/// durable store applies the same validation before persistence.
pub(crate) fn dovecot_state_candidate(line: &str) -> Option<String> {
    if core::valid_dovecot_checkpoint(line) {
        Some(line.to_owned())
    } else {
        None
    }
}

pub(crate) fn record_process_tail(tail: &Mutex<BoundedLineBuffer>, line: &str) {
    if let Ok(mut tail) = tail.lock() {
        let line = truncate_utf8(line, MAX_DIAGNOSTIC_LINE_BYTES);
        tail.push_bounded(line, MAX_PROCESS_TAIL_LINES, MAX_PROCESS_TAIL_BYTES);
    }
}

fn process_tail_text(tail: &Mutex<BoundedLineBuffer>) -> String {
    tail.lock()
        .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" | "))
        .unwrap_or_default()
}

fn register_process(
    tx: &mpsc::SyncSender<crate::Event>,
    run_id: &str,
    job_id: &str,
    executable: &str,
    child: &std::process::Child,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let (start_ticks, process_group, session_id) = process_identity(child.id())
        .map(|(start, group, session)| (Some(start), Some(group), Some(session)))
        .unwrap_or((None, None, None));
    let (ack_tx, ack_rx) = mpsc::sync_channel(1);
    send_reliable_event(
        tx,
        crate::Event::ProcessStarted(
            run_id.to_owned(),
            job_id.to_owned(),
            child.id(),
            start_ticks,
            process_group,
            session_id,
            executable.to_owned(),
            ack_tx,
        ),
    )?;
    let deadline = std::time::Instant::now() + PROCESS_REGISTRATION_ACK_TIMEOUT;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled before durable process registration".to_owned());
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err("durable process registration acknowledgement timed out".to_owned());
        }
        match ack_rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) => return Err(format!("process registration failed: {error}")),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("durable process registration response was lost".to_owned());
            }
        }
    }
}

/// Start MailSwiftSync's internal gatekeeper instead of the external engine.
/// The gatekeeper inherits the configured environment, waits for the durable
/// ProcessStarted acknowledgement, and only then launches the engine. This
/// closes the crash window between OS process creation and durable ownership.
#[cfg(not(test))]
fn execution_command(
    executable: &str,
    args: &[String],
    env: &[(String, SecretString)],
) -> Result<Command, String> {
    let current_executable = std::env::current_exe()
        .map_err(|error| format!("could not locate MailSwiftSync launcher: {error}"))?;
    let mut command = Command::new(current_executable);
    // The launcher becomes the engine, so it starts from the same minimal
    // environment the engine may see.
    crate::process::apply_engine_environment(&mut command);
    command
        .arg("--internal-launcher")
        .arg(executable)
        .arg("--")
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    Ok(command)
}

/// Unit-test binaries do not dispatch through the application CLI, so retain
/// direct execution there while production binaries always use the gate.
#[cfg(test)]
fn execution_command(
    executable: &str,
    args: &[String],
    env: &[(String, SecretString)],
) -> Result<Command, String> {
    let mut command = Command::new(executable);
    crate::process::apply_engine_environment(&mut command);
    command
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    Ok(command)
}

fn release_engine(release_stdin: &mut Option<std::process::ChildStdin>) -> Result<(), String> {
    if let Some(mut stdin) = release_stdin.take() {
        stdin
            .write_all(b"GO\n")
            .map_err(|error| format!("could not signal the internal launcher: {error}"))?;
    }
    Ok(())
}

/// Entry point used only by the production binary's internal launcher mode.
/// Arguments are passed as OS strings so executable paths and engine options
/// do not undergo shell parsing or lossy Unicode conversion.
pub(crate) fn run_internal_launcher(arguments: Vec<std::ffi::OsString>) -> i32 {
    let Some((executable, remainder)) = arguments.split_first() else {
        return 125;
    };
    let Some(separator) = remainder.iter().position(|argument| argument == "--") else {
        return 125;
    };
    let mut release = [0_u8; 3];
    if std::io::stdin().read_exact(&mut release).is_err() || release != *b"GO\n" {
        return 125;
    }
    let mut command = Command::new(executable);
    // Re-apply the allowlist even though the controller already cleared the
    // launcher's environment, so the engine never depends on the caller.
    crate::process::apply_engine_environment(&mut command);
    command
        .args(&remainder[separator + 1..])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // On Unix, replace the gatekeeper image instead of creating a second
    // process. The durable ProcessStarted record therefore continues to
    // identify the actual migration engine, while the session/process group
    // and PR_SET_PDEATHSIG containment survive the exec. Spawning the engine
    // here leaves a dead launcher PID in the ledger after a controller crash,
    // allowing the real child to outlive its recorded ownership.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        eprintln!("could not exec migration engine: {error}");
        125
    }
    #[cfg(not(unix))]
    {
        match command.spawn().and_then(|mut child| child.wait()) {
            Ok(status) => status.code().unwrap_or(1),
            Err(_) => 125,
        }
    }
}

struct ProcessRegistrationGuard<'a> {
    tx: &'a mpsc::SyncSender<crate::Event>,
    run_id: String,
    job_id: String,
}

impl Drop for ProcessRegistrationGuard<'_> {
    fn drop(&mut self) {
        if let Err(error) = send_reliable_event(
            self.tx,
            crate::Event::ProcessEnded {
                run_id: self.run_id.clone(),
                job_id: self.job_id.clone(),
            },
        ) {
            eprintln!("reliable process-end event delivery failed: {error}");
        }
    }
}

/// Spawn, retrying briefly while the kernel reports the executable busy
/// (ETXTBSY). A file that was just written can still be open for writing
/// in a child another thread forked but has not yet exec'd, for example an
/// engine installed moments ago. The window is microseconds; a few short
/// retries are enough and every other error is returned at once.
#[cfg(unix)]
fn spawn_retrying_busy_executable(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    const ATTEMPTS: u32 = 20;
    const ETXTBSY: i32 = 26;
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Err(error) if error.raw_os_error() == Some(ETXTBSY) && attempt + 1 < ATTEMPTS => {
                attempt += 1;
                thread::sleep(Duration::from_millis(5 * u64::from(attempt)));
            }
            result => return result,
        }
    }
}

/// Windows has no ETXTBSY race of this kind; spawn directly.
#[cfg(not(unix))]
fn spawn_retrying_busy_executable(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    command.spawn()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_capture_lines(
    executable: &str,
    args: &[String],
    env: &[(String, SecretString)],
    cancel: &AtomicBool,
    secrets: &[SecretString],
    timeout: Duration,
    observer: Option<OutputObserver>,
    durable: Option<(&mpsc::SyncSender<crate::Event>, &str, &str)>,
) -> Result<(ProcessOutcome, Vec<String>, bool), String> {
    let mut command = execution_command(executable, args, env)?;
    let mut child = spawn_retrying_busy_executable(&mut command)
        .map_err(|error| format!("could not start {executable}: {error}"))?;
    let mut release_stdin = child.stdin.take();
    let _child_supervisor = match attach_child_supervisor(&child) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err(format!(
                "could not establish descendant process supervision for {executable}: {error}"
            ));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stdout pipe unavailable; child cancelled before supervision".into());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stderr pipe unavailable; child cancelled before supervision".into());
        }
    };
    let out_secrets = secrets.to_vec();
    let out_observer = observer.clone();
    let out_thread = thread::spawn(move || {
        collect_redacted_lines_with_callback(stdout, &out_secrets, |line| {
            if let Some(observer) = &out_observer {
                observer(line);
            }
        })
    });
    let err_secrets = secrets.to_vec();
    let err_observer = observer;
    let err_thread = thread::spawn(move || {
        collect_redacted_lines_with_callback(stderr, &err_secrets, |line| {
            if let Some(observer) = &err_observer {
                observer(line);
            }
        })
    });
    let registration = durable.map(|(tx, run_id, job_id)| {
        register_process(tx, run_id, job_id, executable, &child, cancel)
    });
    let mut registration_error = registration
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .cloned();
    if registration_error.is_none()
        && let Err(error) = release_engine(&mut release_stdin)
    {
        registration_error = Some(format!("engine release failed; child cancelled: {error}"));
    }
    if registration_error.is_some() {
        cancel.store(true, Ordering::Relaxed);
        terminate_process_group(&mut child);
    }
    let _registration_guard = match (durable, registration_error.is_none()) {
        (Some((tx, run_id, job_id)), true) => Some(ProcessRegistrationGuard {
            tx,
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
        }),
        _ => None,
    };
    let debug_supervision = process_supervision_debug_enabled();
    if debug_supervision {
        eprintln!("[process-debug] capture waiting for child {}", child.id());
    }
    let status = wait_with_timeout(&mut child, timeout, cancel).map_err(|error| error.to_string());
    if debug_supervision {
        eprintln!("[process-debug] capture wait returned; joining stdout reader");
    }
    let stdout_lines = out_thread
        .join()
        .map_err(|_| "stdout reader thread panicked".to_owned())?
        .map_err(|error| format!("stdout reader failed: {error}"));
    if debug_supervision {
        eprintln!("[process-debug] stdout reader joined; joining stderr reader");
    }
    let stderr_lines = err_thread
        .join()
        .map_err(|_| "stderr reader thread panicked".to_owned())?
        .map_err(|error| format!("stderr reader failed: {error}"));
    if debug_supervision {
        eprintln!("[process-debug] stderr reader joined");
    }
    let stdout_output = stdout_lines?;
    let stderr_output = stderr_lines?;
    let capture_truncated = stdout_output.truncated || stderr_output.truncated;
    let mut lines = stdout_output.lines;
    lines.extend(
        stderr_output
            .lines
            .into_iter()
            .map(|line| format!("[stderr] {line}")),
    );
    let status = status?;
    if let Some(error) = registration_error {
        return Err(error);
    }
    if capture_truncated {
        lines.push(
            "[diagnostics truncated; semantic verification continued from the full stream]".into(),
        );
    }
    Ok((status, lines, capture_truncated))
}

pub(crate) fn process_supervision_debug_enabled() -> bool {
    std::env::var_os("MAILSWIFTSYNC_DEBUG_PROCESS_WAIT").is_some_and(|value| value == "1")
}

/// Ask an engine for its version without passing credentials or mailbox
/// arguments. This is best-effort metadata: an old wrapper may not implement
/// `--version`, in which case the run explicitly remains unversioned.
pub(crate) fn probe_engine_version(executable: &str) -> Option<String> {
    let cancel = AtomicBool::new(false);
    let mut candidates = vec![(executable.to_owned(), vec!["--version".into()])];
    if std::path::Path::new(executable)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("doveadm"))
    {
        let sibling = std::path::Path::new(executable)
            .parent()
            .map(|parent| parent.join("dovecot"))
            .unwrap_or_else(|| std::path::PathBuf::from("dovecot"));
        candidates.push((
            sibling.to_string_lossy().into_owned(),
            vec!["--version".into()],
        ));
    }
    for (candidate, args) in candidates {
        let Ok((status, lines, _)) = run_capture_lines(
            &candidate,
            &args,
            &[],
            &cancel,
            &[],
            Duration::from_secs(5),
            None,
            None,
        ) else {
            continue;
        };
        if status.exit_code == Some(0)
            && let Some(line) = lines
                .into_iter()
                .map(|line| line.trim().to_owned())
                .find(|line| !line.is_empty() && line.len() <= 512)
        {
            return Some(line);
        }
    }
    None
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedImapsyncIdentity {
    pub(crate) version: String,
    pub(crate) output_profile: verification::ImapsyncOutputProfile,
}

/// Resolve the executable identity before launch. An unreadable version is
/// recorded explicitly and maps to an unknown parser profile, so it can never
/// produce trusted verification evidence.
pub(crate) fn resolve_imapsync_identity(executable: &str) -> ResolvedImapsyncIdentity {
    let probed = probe_engine_version(executable);
    ResolvedImapsyncIdentity {
        output_profile: verification::imapsync_output_profile(probed.as_deref()),
        version: probed.unwrap_or_else(|| "unknown".into()),
    }
}

/// Persist the resolved engine identity before process registration. The
/// acknowledgment makes version/profile selection part of the durable launch
/// boundary instead of lossy telemetry.
pub(crate) fn persist_engine_identity_before_launch(
    tx: &mpsc::SyncSender<crate::Event>,
    run_id: &str,
    job_id: &str,
    version: &str,
) -> Result<(), String> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    send_reliable_event(
        tx,
        crate::Event::EngineVersion {
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
            version: version.to_owned(),
            reply: reply_tx,
        },
    )?;
    reply_rx
        .recv_timeout(PROCESS_REGISTRATION_ACK_TIMEOUT)
        .map_err(|_| "engine identity was not durably acknowledged before launch".to_owned())?
}

pub(crate) fn run_dovecot_destination_preflight(
    commands: &[(String, Vec<String>)],
    tx: &mpsc::SyncSender<crate::Event>,
    cancel: &AtomicBool,
    timeout: Duration,
    prefix: &str,
    run_id: &str,
    job_id: &str,
) -> Result<(), String> {
    for (index, (executable, args)) in commands.iter().enumerate() {
        let (status, lines, _) =
            run_capture_lines(executable, args, &[], cancel, &[], timeout, None, None)?;
        for line in lines {
            let _ = tx.send(Event::RunLine {
                run_id: run_id.to_owned(),
                job_id: job_id.to_owned(),
                text: format!("{prefix}[destination preflight/{}] {line}", index + 1),
            });
        }
        if status.exit_code != Some(0) {
            return Err(format!(
                "Dovecot destination preflight command {} exited with code {:?}",
                index + 1,
                status.exit_code
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_dovecot_verification(
    commands: &[(String, Vec<String>)],
    verification_env: &[(String, SecretString)],
    secrets: &[SecretString],
    tx: &mpsc::SyncSender<crate::Event>,
    cancel: &AtomicBool,
    timeout: Duration,
    prefix: &str,
    run_id: &str,
    job_id: &str,
) -> Result<DovecotVerificationResult, String> {
    let mut reports = Vec::with_capacity(commands.len());
    for (index, (verify_exe, verify_args)) in commands.iter().enumerate() {
        let accumulator = Arc::new(Mutex::new(verification::DovecotStatusAccumulator::default()));
        let observer_accumulator = Arc::clone(&accumulator);
        let diagnostic_tail = Arc::new(Mutex::new(BoundedLineBuffer::new()));
        let observer_tail = Arc::clone(&diagnostic_tail);
        let observer: OutputObserver = Arc::new(move |line| {
            if let Ok(mut accumulator) = observer_accumulator.lock() {
                accumulator.observe(line);
            }
            record_process_tail(&observer_tail, line);
        });
        let (status, report, truncated) = run_capture_lines(
            verify_exe,
            verify_args,
            if index == 0 { verification_env } else { &[] },
            cancel,
            secrets,
            timeout,
            Some(observer),
            Some((tx, run_id, job_id)),
        )?;
        for line in &report {
            let _ = tx.send(Event::RunLine {
                run_id: run_id.to_owned(),
                job_id: job_id.to_owned(),
                text: format!("{prefix}[verification/{}] {line}", index + 1),
            });
        }
        if status.exit_code != Some(0) {
            let diagnostic = truncate_utf8(&process_tail_text(&diagnostic_tail), 4096);
            return Err(format!(
                "Dovecot verification command {} exited with code {:?}{}",
                index + 1,
                status.exit_code,
                if diagnostic.is_empty() {
                    String::new()
                } else {
                    format!("; recent output: {diagnostic}")
                }
            ));
        }
        if truncated {
            let _ = tx.send(Event::RunLine {
                run_id: run_id.to_owned(),
                job_id: job_id.to_owned(),
                text: format!(
                    "{prefix}[verification/{}] diagnostic transcript truncated; semantic accumulator processed the complete stream",
                    index + 1
                ),
            });
        }
        let status = accumulator
            .lock()
            .map_err(|_| "Dovecot verification accumulator was poisoned".to_owned())?
            .clone();
        reports.push((report, status));
    }
    if reports.len() < 2 {
        return Err("Dovecot verification returned incomplete reports".into());
    }
    let evidence = verification::dovecot_evidence_from_accumulators(&reports[0].1, &reports[1].1)
        .ok_or_else(|| {
        let summarize = |status: &verification::DovecotStatusAccumulator| {
            format!(
                "{} folders, {} messages, {} bytes, {} malformed status lines{}",
                status.folders,
                status.messages,
                status.bytes,
                status.malformed_lines,
                if status.overflowed {
                    ", counters overflowed"
                } else {
                    ""
                }
            )
        };
        format!(
            "Dovecot status output was incomplete (source: {}; destination: {})",
            summarize(&reports[0].1),
            summarize(&reports[1].1)
        )
    })?;
    Ok(DovecotVerificationResult {
        evidence,
        checkpoint_context: verification::dovecot_checkpoint_context_digest(
            &reports[0].1,
            &reports[1].1,
        ),
    })
}

pub(crate) struct DovecotVerificationResult {
    pub(crate) evidence: core::MailboxEvidence,
    pub(crate) checkpoint_context: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_dovecot_checkpoint_context(
    commands: &[(String, Vec<String>)],
    verification_env: &[(String, SecretString)],
    secret: &SecretString,
    expected_context: &str,
    tx: &mpsc::SyncSender<crate::Event>,
    cancel: &AtomicBool,
    timeout: Duration,
    prefix: &str,
    run_id: &str,
    job_id: &str,
) -> Result<(), String> {
    let result = run_dovecot_verification(
        commands,
        verification_env,
        std::slice::from_ref(secret),
        tx,
        cancel,
        timeout,
        prefix,
        run_id,
        job_id,
    )?;
    match result.checkpoint_context.as_deref() {
        Some(actual) if actual == expected_context => Ok(()),
        Some(actual) => Err(format!(
            "Dovecot checkpoint UIDVALIDITY context changed (saved {expected_context}, current {actual}); refusing resume"
        )),
        None => Err(
            "Dovecot checkpoint could not be validated because UIDVALIDITY was not present for every mailbox".into(),
        ),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::folder_policy::automap_folder_kind;
    use super::{
        RunContext, TerminalEvidenceSource, automap_blocks_live_certification,
        infer_automap_folder_mapping, message_verification_enabled,
        persist_engine_identity_before_launch, process_tail_text, record_process_tail,
        resolve_imapsync_identity, run_streaming, terminal_evidence_source,
        validate_body_hash_limits, validate_destination_folder_policy,
    };
    use crate::{
        BoundedLineBuffer, Event, MAX_DIAGNOSTIC_LINE_BYTES, MAX_PROCESS_TAIL_BYTES, StreamOutcome,
        imap_probe::MailboxDescriptor, verification::ImapsyncOutputProfile,
    };
    use std::{collections::HashSet, fs, os::unix::fs::PermissionsExt, sync::mpsc, thread};

    fn unsuitable_live_form() -> crate::Form {
        let mut form = crate::Form {
            dry_run: false,
            ..Default::default()
        };
        form.profile.engine = crate::core::Engine::ImapSync;
        form
    }

    #[test]
    fn body_hash_verification_requires_bounded_stable_imap_plan() {
        let mut form = unsuitable_live_form();
        form.profile.body_hash_verification = true;
        assert!(validate_body_hash_limits(&form).is_ok());

        form.profile.body_hash_max_bytes = 0;
        assert!(validate_body_hash_limits(&form).is_err());
        form.profile.body_hash_max_bytes = 8 * 1024 * 1024;
        form.profile.body_hash_max_total_bytes = 4 * 1024 * 1024;
        assert!(validate_body_hash_limits(&form).is_err());

        form.profile.body_hash_max_total_bytes = 512 * 1024 * 1024;
        form.profile.engine = crate::core::Engine::Dovecot;
        assert!(validate_body_hash_limits(&form).is_err());
    }

    #[test]
    fn body_hash_setting_is_bound_to_plan_fingerprint() {
        let mut form = unsuitable_live_form();
        let original = form.plan_fingerprint();
        form.profile.body_hash_verification = true;
        assert_ne!(original, form.plan_fingerprint());
        let original = form.plan_fingerprint();
        form.profile.body_hash_max_total_bytes += 1;
        assert_ne!(original, form.plan_fingerprint());
    }

    #[test]
    fn reliable_event_delivery_reports_disconnected_controller() {
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        let result = super::send_reliable_event(&tx, Event::Finished(Ok(StreamOutcome::Completed)));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("disconnected"));
    }

    #[test]
    fn process_diagnostic_tail_retains_latest_output_within_its_bound() {
        let tail = std::sync::Mutex::new(BoundedLineBuffer::new());
        record_process_tail(&tail, "early status output");
        for _ in 0..80 {
            record_process_tail(&tail, &"x".repeat(MAX_DIAGNOSTIC_LINE_BYTES));
        }
        record_process_tail(&tail, "final Dovecot error");

        let diagnostic = process_tail_text(&tail);
        assert!(diagnostic.contains("final Dovecot error"));
        assert!(!diagnostic.contains("early status output"));
        assert!(diagnostic.len() <= MAX_PROCESS_TAIL_BYTES);
    }

    #[test]
    fn single_justfolders_never_uses_engine_evidence() {
        let mut form = unsuitable_live_form();
        form.profile.justfolders = true;
        assert_eq!(
            terminal_evidence_source(&form, false, true),
            TerminalEvidenceSource::Unavailable
        );
    }

    #[test]
    fn single_addheader_never_uses_engine_evidence() {
        let mut form = unsuitable_live_form();
        form.profile.addheader = true;
        assert_eq!(
            terminal_evidence_source(&form, false, true),
            TerminalEvidenceSource::Unavailable
        );
    }

    #[test]
    fn single_internal_date_disabled_never_uses_engine_evidence() {
        let mut form = unsuitable_live_form();
        form.profile.sync_internaldates = false;
        assert_eq!(
            terminal_evidence_source(&form, false, true),
            TerminalEvidenceSource::Unavailable
        );
    }

    #[test]
    fn single_size_mismatch_allowed_never_uses_engine_evidence() {
        let mut form = unsuitable_live_form();
        form.profile.allowsizemismatch = true;
        assert_eq!(
            terminal_evidence_source(&form, false, true),
            TerminalEvidenceSource::Unavailable
        );
    }

    #[test]
    fn automap_never_claims_independent_message_verification_without_engine_mapping() {
        let mut form = unsuitable_live_form();
        form.profile.automap = true;
        assert!(form.profile.automap);
        assert!(!super::message_verification_enabled(&form));
        assert_eq!(
            terminal_evidence_source(&form, true, false),
            TerminalEvidenceSource::Unavailable
        );
    }

    #[test]
    fn default_profile_can_provide_headless_live_terminal_evidence() {
        let form = crate::Form::default();
        assert!(message_verification_enabled(&form));
        assert!(!automap_blocks_live_certification(&form));
        assert_eq!(
            terminal_evidence_source(&form, true, true),
            TerminalEvidenceSource::Independent
        );
    }

    #[test]
    fn imapsync_automap_is_rejected_before_live_certification() {
        let mut form = crate::Form::default();
        form.profile.automap = true;
        assert!(automap_blocks_live_certification(&form));
        assert!(!message_verification_enabled(&form));
    }

    #[test]
    fn imapsync_identity_is_resolved_before_execution() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-version-probe-{}-{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, "#!/bin/sh\nprintf 'imapsync 2.314\\n'\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let identity = resolve_imapsync_identity(&path.to_string_lossy());
        assert_eq!(identity.version, "imapsync 2.314");
        assert_eq!(identity.output_profile, ImapsyncOutputProfile::Packaged2314);
        fs::remove_file(path).unwrap();
    }

    /// Progress is counted on the lossless reader, so a saturated controller
    /// that drops throttled snapshots still receives exact final totals.
    #[test]
    fn final_progress_is_exact_even_when_snapshots_are_dropped() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-progress-engine-{}-{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::write(
            &path,
            "#!/bin/sh\n\
             printf 'Host1 Nb messages: 300 messages\\nHost1 Total size: 300000 bytes\\n'\n\
             i=1\n\
             while [ $i -le 300 ]; do\n\
             printf 'msg INBOX/%d {1000} copied to INBOX/%d  1 msgs/s  1 KiB/s  ETA: x  0 s  %d/300 msgs left\\n' $i $i $((300 - i))\n\
             i=$((i + 1))\n\
             done\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let (tx, rx) = mpsc::sync_channel(2);
        let controller = thread::spawn(move || {
            let mut snapshots = Vec::new();
            while let Ok(event) = rx.recv() {
                match event {
                    Event::ProcessStarted(.., reply) => {
                        let _ = reply.send(Ok(()));
                    }
                    Event::Progress { progress, .. } => snapshots.push(progress),
                    _ => {}
                }
                // A slow controller keeps the bounded channel saturated.
                thread::sleep(std::time::Duration::from_millis(2));
            }
            snapshots
        });
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let result = run_streaming(RunContext {
            executable: &path.to_string_lossy(),
            args: &[],
            env: &[],
            tx: &tx,
            run_id: "run",
            job_id: "job",
            project_id: "project",
            prefix: "",
            cancel: &cancel,
            secrets: &[],
            timeout: std::time::Duration::from_secs(30),
            dovecot_exit_two_is_delta: false,
            imapsync_output_profile: ImapsyncOutputProfile::Unknown,
            diagnostic_logger: None,
            attempt_number: 1,
            transfer_pass: None,
            launch_limiter: None,
        });
        drop(tx);
        let snapshots = controller.join().unwrap();
        fs::remove_file(path).unwrap();
        assert!(result.is_ok(), "{result:?}");
        let last = snapshots
            .last()
            .expect("final progress snapshot is delivered");
        assert_eq!(last.messages_copied, 300);
        assert_eq!(last.bytes_copied, 300_000);
        assert_eq!(last.source_messages, Some(300));
        assert_eq!(last.messages_left, Some(0));
        assert!(snapshots.len() < 300, "snapshots must be throttled");
    }

    #[test]
    fn unreadable_imapsync_identity_is_unknown() {
        let identity = resolve_imapsync_identity("/path/that/does/not/exist/imapsync");
        assert_eq!(identity.version, "unknown");
        assert_eq!(identity.output_profile, ImapsyncOutputProfile::Unknown);
    }

    #[test]
    fn automap_rejects_ambiguous_special_use_candidates() {
        let source = vec![MailboxDescriptor {
            wire_name: "Sent".into(),
            delimiter: Some("/".into()),
            special_use: vec!["sent".into()],
            selectable: true,
        }];
        let destination = vec![
            MailboxDescriptor {
                wire_name: "Sent".into(),
                delimiter: Some("/".into()),
                special_use: vec!["sent".into()],
                selectable: true,
            },
            MailboxDescriptor {
                wire_name: "Sent Items".into(),
                delimiter: Some("/".into()),
                special_use: vec!["sent".into()],
                selectable: true,
            },
        ];
        let error = infer_automap_folder_mapping(&source, &destination, true).unwrap_err();
        assert!(error.contains("ambiguous automap"));
        assert!(error.contains("Sent Items"));
    }

    #[test]
    fn engine_identity_requires_a_durable_acknowledgment() {
        let (tx, rx) = mpsc::sync_channel(1);
        let receiver = thread::spawn(move || {
            let Event::EngineVersion {
                run_id,
                job_id,
                version,
                reply,
            } = rx.recv().unwrap()
            else {
                panic!("expected engine identity event");
            };
            assert_eq!(run_id, "run");
            assert_eq!(job_id, "job");
            assert_eq!(version, "imapsync 2.314");
            reply.send(Ok(())).unwrap();
        });

        persist_engine_identity_before_launch(&tx, "run", "job", "imapsync 2.314").unwrap();
        receiver.join().unwrap();
    }

    #[test]
    fn automap_does_not_classify_unrelated_folder_names_by_substring() {
        assert_eq!(automap_folder_kind("Cabinet"), None);
        assert_eq!(automap_folder_kind("Sent Items"), Some("sent"));
        assert_eq!(automap_folder_kind("[Gmail]/Trash"), Some("trash"));
    }

    #[test]
    fn merge_folder_policy_allows_destination_only_folders() {
        let required = HashSet::from(["INBOX".to_owned(), "Sent".to_owned()]);
        let actual = HashSet::from([
            "INBOX".to_owned(),
            "Sent".to_owned(),
            "Provider/System".to_owned(),
        ]);

        assert!(validate_destination_folder_policy(&required, &actual, false).is_ok());
    }

    #[test]
    fn folder_policy_rejects_missing_required_folders() {
        let required = HashSet::from(["INBOX".to_owned(), "Sent".to_owned()]);
        let actual = HashSet::from(["INBOX".to_owned()]);

        let error = validate_destination_folder_policy(&required, &actual, false).unwrap_err();
        assert!(error.contains("Sent"));
    }

    #[test]
    fn strict_folder_policy_rejects_destination_only_folders() {
        let required = HashSet::from(["INBOX".to_owned()]);
        let actual = HashSet::from(["INBOX".to_owned(), "Provider/System".to_owned()]);

        let error = validate_destination_folder_policy(&required, &actual, true).unwrap_err();
        assert!(error.contains("unexpected selectable folders"));
        assert!(error.contains("Provider/System"));
    }
}
