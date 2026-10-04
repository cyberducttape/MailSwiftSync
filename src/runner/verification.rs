use super::*;

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
        // The reconciler refuses incomplete or orphaned fingerprint coverage.
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
