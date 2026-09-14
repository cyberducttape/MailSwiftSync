use crate::*;

impl App {
    pub(crate) fn poll(&mut self) {
        let was_workspace_stale = self.ui_snapshot.is_stale();
        self.refresh_ui_snapshot();
        if !was_workspace_stale && self.ui_snapshot.is_stale() && !self.running() {
            self.set_status(
                "Durable state view is stale; execution is disabled until SQLite refresh succeeds.",
                StatusSeverity::Error,
            );
        }
        if self.invalidate_stale_capability_observation() {
            self.set_status(
                "Readiness observations expired because the migration plan changed; run discovery again.",
                StatusSeverity::Warning,
            );
        }
        if let Some(receiver) = &self.bulk_import_receiver
            && let Ok(result) = receiver.try_recv()
        {
            self.bulk_import_receiver = None;
            self.apply_bulk_import_result(result);
        }
        let start_credentials_result = self
            .start_credentials_receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        if let Some(result) = start_credentials_result {
            self.start_credentials_receiver = None;
            let probed_plan = self.start_credentials_plan.take();
            let current_plan = self.start_credentials_plan_marker();
            match result {
                Ok(form) if probed_plan.as_deref() == Some(current_plan.as_str()) => {
                    self.form = form;
                    self.credentials_ready_for_start = true;
                    self.start();
                }
                Ok(_) => {
                    self.credentials_ready_for_start = false;
                    self.set_status(
                        "The migration plan changed while credentials were loading; review it and start again.",
                        StatusSeverity::Warning,
                    );
                }
                Err(error) => {
                    self.credentials_ready_for_start = false;
                    self.set_status(error, StatusSeverity::Error);
                }
            }
        }
        let live_auth_result = self
            .live_auth_receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        if let Some(result) = live_auth_result {
            self.live_auth_receiver = None;
            match result {
                Ok(proof) => {
                    self.live_auth_proof = Some(proof);
                    self.set_status(
                        "Fresh IMAPS authentication passed; continuing live admission…",
                        StatusSeverity::Success,
                    );
                    self.start();
                }
                Err(error) => {
                    self.live_auth_proof = None;
                    self.set_status(
                        format!("Live authentication failed; migration was not started: {error}"),
                        StatusSeverity::Error,
                    );
                }
            }
        }
        let capability_result = self
            .capability_receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        if let Some(result) = capability_result {
            let current_fingerprint = plan_fingerprint_digest(&self.form.plan_fingerprint());
            let matches = self
                .capability_probe_request_id
                .as_deref()
                .zip(self.capability_probe_fingerprint.as_deref())
                .is_some_and(|(request_id, fingerprint)| {
                    capability_probe_result_matches(
                        &result,
                        request_id,
                        fingerprint,
                        &current_fingerprint,
                    )
                });
            self.capability_receiver = None;
            self.capability_probe_request_id = None;
            self.capability_probe_fingerprint = None;
            if matches {
                match result.result {
                    Ok((source, destination)) => {
                        self.source_capabilities = Some(source);
                        self.destination_capabilities = Some(destination);
                        self.capability_observation_fingerprint = Some(result.plan_fingerprint);
                        self.set_status("Capability discovery complete", StatusSeverity::Success);
                        self.assess_plan();
                    }
                    Err(error) => self.set_status(
                        format!("Preflight discovery failed: {error}"),
                        StatusSeverity::Error,
                    ),
                }
            }
        }
        let mut done = None;
        let mut pending_db_events = std::mem::take(&mut self.pending_db_events);
        let mut deferred_events = std::mem::take(&mut self.deferred_events);
        let mut durability_errors = Vec::new();
        let mut recovered_durability = false;
        let mut bulk_state_changed = false;
        let active_run = self.active_run.clone();
        let mut ended_processes = HashSet::new();
        if let Some(rx) = &self.receiver {
            let mut processed_events = 0;
            while processed_events < MAX_EVENTS_PER_FRAME {
                let event = if let Some(event) = deferred_events.pop_front() {
                    event
                } else {
                    match rx.try_recv() {
                        Ok(event) => event,
                        Err(_) => break,
                    }
                };
                processed_events += 1;
                match event {
                    Event::ClaimBatch {
                        project_id,
                        job_id,
                        parent_run_id,
                        child_run_id,
                        reply,
                    } => {
                        let result = if active_run.as_ref().is_some_and(|run| {
                            run.project_id == project_id
                                && run.owns_batch_child(&parent_run_id, &child_run_id, &job_id)
                        }) {
                            self.store
                                .claim_batch_mailbox_for_child(
                                    &project_id,
                                    &job_id,
                                    &parent_run_id,
                                    &child_run_id,
                                )
                                .map_err(|error| error.to_string())
                        } else {
                            Err(
                                "batch claim event does not belong to the active run context"
                                    .to_owned(),
                            )
                        };
                        if let Err(error) = &result {
                            durability_errors.push(format!(
                                "durable claim for child run {child_run_id} failed: {error}"
                            ));
                        }
                        let _ = reply.send(result);
                    }
                    Event::ProcessStarted(
                        process_run_id,
                        job_id,
                        pid,
                        start_ticks,
                        process_group,
                        session_id,
                        executable,
                        reply,
                    ) => {
                        let checkpoint_run_id = process_run_id.clone();
                        let result = if active_run
                            .as_ref()
                            .is_some_and(|run| run.owns_process(&process_run_id, &job_id))
                        {
                            self.store
                                .register_process(&core::ActiveProcess {
                                    run_id: process_run_id,
                                    job_id,
                                    pid,
                                    start_ticks,
                                    process_group,
                                    session_id,
                                    executable,
                                })
                                .map_err(|error| error.to_string())
                        } else {
                            Err(
                                "process-start event does not belong to the active run context"
                                    .to_owned(),
                            )
                        };
                        if result.is_ok()
                            && active_run
                                .as_ref()
                                .is_some_and(|run| matches!(run.kind, RunKind::Batch))
                        {
                            // A new attempt has a new process and therefore
                            // must not inherit a checkpoint candidate emitted
                            // by a failed earlier attempt of the same child.
                            self.pending_batch_checkpoints.remove(&checkpoint_run_id);
                        }
                        let _ = reply.send(result.clone());
                        if let Err(error) = result {
                            durability_errors.push(format!(
                                "persist process identity failed; cancellation requested before an untracked engine can continue: {error}"
                            ));
                            // A migration must not continue when its process
                            // identity could not be durably registered. The
                            // runner will terminate the child through its
                            // normal cancellation path, while the terminal
                            // event records the durability failure.
                            if let Some(cancel) = &self.cancel_requested {
                                cancel.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                    Event::RunLine {
                        run_id,
                        job_id,
                        text,
                    } => {
                        let owns_line = run_line_is_current(
                            active_run.as_ref(),
                            &ended_processes,
                            &run_id,
                            &job_id,
                        );
                        if !owns_line {
                            // RunLine is an asynchronous presentation event;
                            // never let a delayed or foreign worker append
                            // output to the active migration's journal.
                            durability_errors.push(format!(
                                "ignored run-line event for unknown run {run_id} and job {job_id}"
                            ));
                        } else {
                            // Engine output is presentation-only. It may
                            // contain subjects, folder metadata, or other
                            // message-derived text, so retain it only in the
                            // bounded process-local journal.
                            push_visible_output(&mut self.output, text);
                        }
                    }
                    Event::EngineVersion {
                        run_id,
                        job_id,
                        version,
                    } => {
                        let owns_version =
                            process_event_is_current(active_run.as_ref(), &run_id, &job_id);
                        if owns_version {
                            // Version probing is advisory metadata. A
                            // storage failure must not turn a successful
                            // migration into a false execution failure.
                            if let Err(error) = self.store.record_engine_version(&run_id, &version)
                            {
                                durability_errors.push(format!(
                                    "could not persist engine version metadata for {run_id}: {error}"
                                ));
                            }
                        }
                    }
                    Event::ProcessEnded { run_id, job_id } => {
                        if process_event_is_current(active_run.as_ref(), &run_id, &job_id) {
                            ended_processes.insert((run_id.clone(), job_id.clone()));
                            if let Err(error) = self.store.clear_processes(&run_id) {
                                durability_errors.push(format!(
                                    "clear completed process identity for {run_id} failed: {error}"
                                ));
                            }
                        } else {
                            durability_errors.push(format!(
                                "ignored process-ended event for unknown run {run_id}"
                            ));
                        }
                    }
                    Event::Line(s) => {
                        // Runner threads redact secrets before publishing events.
                        // Do not re-read mutable form fields here: the operator
                        // may have edited the next plan while this run was active.
                        let safe = s;
                        // Do not persist raw engine output. The bounded UI
                        // journal is intentionally the only destination for
                        // these lines; durable events contain classifications
                        // and evidence summaries instead.
                        push_visible_output(&mut self.output, safe);
                    }
                    Event::JobState {
                        job_id,
                        child_run_id,
                        state,
                    } => {
                        if let Some(run) = active_run.as_ref()
                            && matches!(run.kind, RunKind::Batch)
                            && let Some(index) = run.batch_child_index(&job_id, &child_run_id)
                        {
                            let bulk_index = self
                                .bulk_job_index_by_id
                                .get(&job_id)
                                .copied()
                                .unwrap_or(index);
                            if let Some(job) = self.bulk_jobs.get_mut(bulk_index) {
                                job.state = state.clone();
                            }
                            bulk_state_changed = true;
                            // JobState is deliberately presentation-only. The
                            // worker has already received an acknowledged
                            // ClaimBatch response before it can launch the
                            // process, and terminal state, evidence,
                            // checkpoint, and preflight data must commit
                            // together in the following JobFinished event.
                            // Persisting a terminal JobState here would leave
                            // a mailbox terminal while its child run remained
                            // running if that transaction later failed.
                        } else {
                            durability_errors.push(format!(
                                "ignored batch state event for unknown child run {child_run_id}"
                            ));
                        }
                    }
                    Event::JobFinished {
                        job_id,
                        child_run_id,
                        state,
                        detail,
                        credential_fingerprint,
                    } => {
                        // Diagnostics are accepted only while a child is
                        // active/queued. Flush them before the terminal
                        // transaction so a fast worker cannot deliver
                        // RunLine(s) and JobFinished in one poll cycle and
                        // lose the child log to the terminal-state guard.
                        if !pending_db_events.is_empty() {
                            match persist_pending_events(&self.store, &pending_db_events) {
                                Ok(()) => {
                                    pending_db_events.clear();
                                    if self.durability_recovery_pending {
                                        recovered_durability = true;
                                    }
                                }
                                Err(error) => {
                                    self.durability_recovery_pending = true;
                                    durability_errors.push(format!(
                                        "persist child diagnostics before completion failed: {error}"
                                    ));
                                    // Leave the child running in the ledger,
                                    // retain both the diagnostics and the
                                    // completion event, and stop consuming
                                    // later events until the durable
                                    // predecessor can be committed.
                                    deferred_events.push_front(Event::JobFinished {
                                        job_id,
                                        child_run_id,
                                        state,
                                        detail,
                                        credential_fingerprint,
                                    });
                                    break;
                                }
                            }
                        }
                        if let Some(run) = active_run.as_ref()
                            && matches!(run.kind, RunKind::Batch)
                            && let Some(index) = run.batch_child_index(&job_id, &child_run_id)
                        {
                            let evidence = self.pending_batch_evidence.get(&child_run_id);
                            let final_state = batch_mailbox_state(&state, evidence);
                            let checkpoint = self
                                .pending_batch_checkpoints
                                .get(&child_run_id)
                                .map(String::as_str);
                            let result = finish_batch_child(
                                &self.store,
                                run,
                                controller::BatchChildCompletion {
                                    job_id: &job_id,
                                    child_run_id: &child_run_id,
                                    state: &state,
                                    detail: &detail,
                                    evidence,
                                    checkpoint,
                                },
                            );
                            let completion_persisted = result.is_ok();
                            // Durable state is authoritative. Do not show a
                            // terminal child state in the editable queue until
                            // the run/mailbox transaction has committed.
                            if completion_persisted
                                && let Some(bulk_index) =
                                    self.bulk_job_index_by_id.get(&job_id).copied()
                                && let Some(job) = self.bulk_jobs.get_mut(bulk_index)
                            {
                                job.state = display_job_state(&final_state).into();
                                bulk_state_changed = true;
                            }
                            if let Err(error) = result {
                                self.durability_recovery_pending = true;
                                durability_errors.push(format!(
                                    "persist child run {} completion failed: {error}",
                                    index + 1
                                ));
                                // Keep the completion event and its evidence
                                // inputs together until the terminal
                                // transaction succeeds. The next poll will
                                // retry this event after any pending
                                // diagnostics have been committed.
                                deferred_events.push_front(Event::JobFinished {
                                    job_id,
                                    child_run_id,
                                    state,
                                    detail,
                                    credential_fingerprint,
                                });
                                break;
                            } else {
                                if self.durability_recovery_pending {
                                    recovered_durability = true;
                                }
                                self.pending_batch_evidence.remove(&child_run_id);
                                self.pending_batch_checkpoints.remove(&child_run_id);
                            }
                            if completion_persisted
                                && !self.bulk_live_run
                                && state == "ready"
                                && let Some(fingerprint) = credential_fingerprint
                                && let Some(bulk_index) =
                                    self.bulk_job_index_by_id.get(&job_id).copied()
                                && let Some(saved) = self
                                    .bulk_preflight_credential_fingerprints
                                    .get_mut(bulk_index)
                            {
                                *saved = Some(fingerprint);
                            }
                        } else {
                            durability_errors.push(format!(
                                "ignored batch completion event for unknown child run {child_run_id}"
                            ));
                        }
                    }
                    Event::BatchEvidence {
                        job_id,
                        child_run_id,
                        evidence,
                    } => {
                        if let Some(run) = active_run.as_ref()
                            && matches!(run.kind, RunKind::Batch)
                            && run.batch_child_index(&job_id, &child_run_id).is_some()
                        {
                            self.pending_batch_evidence.insert(child_run_id, evidence);
                        } else {
                            durability_errors.push(format!(
                                "ignored evidence event for unknown child run {child_run_id}"
                            ));
                        }
                    }
                    Event::Checkpoint {
                        run_id,
                        job_id,
                        value,
                    } => {
                        if let Some(run) = active_run.as_ref()
                            && run.owns_process(&run_id, &job_id)
                        {
                            if matches!(run.kind, RunKind::Batch) {
                                self.pending_batch_checkpoints.insert(run_id, value);
                            } else if run.run_id == run_id {
                                self.pending_checkpoint = Some(value);
                            }
                        } else {
                            durability_errors.push(format!(
                                "ignored Dovecot checkpoint for unknown run {run_id}"
                            ));
                        }
                    }
                    Event::Evidence(evidence) => {
                        let evidence_level = evidence.evidence_level();
                        // Hold evidence until Finished so its history, run
                        // status, mailbox state, and terminal event commit
                        // together. In particular, this permits the
                        // evidence-backed running -> verified transition
                        // without weakening ordinary state transitions.
                        self.pending_evidence = Some(evidence);
                        if active_run.is_some()
                            && let Some(run_id) = active_run.as_ref().map(|run| run.run_id.as_str())
                        {
                            pending_db_events.push(PendingDbEvent::new(
                                run_id.to_owned(),
                                "verification_evidence".into(),
                                format!("evidence level: {evidence_level}"),
                            ));
                        }
                    }
                    Event::VerificationFailed(detail) => {
                        let safe = ui::redact_secrets(
                            &detail,
                            [
                                self.form.source_password.as_str(),
                                self.form.destination_password.as_str(),
                            ],
                        );
                        push_visible_output(&mut self.output, format!("[verification] {safe}"));
                        if active_run.is_some()
                            && let Some(run_id) = active_run.as_ref().map(|run| run.run_id.as_str())
                        {
                            pending_db_events.push(PendingDbEvent::new(
                                run_id.to_owned(),
                                "verification_pending".into(),
                                safe,
                            ));
                        }
                    }
                    Event::Finished(r) => done = Some(r),
                }
            }
        }
        if bulk_state_changed {
            self.mark_bulk_state_changed();
        }
        if !pending_db_events.is_empty() {
            let result = active_run.as_ref().map_or_else(
                || Err(rusqlite::Error::InvalidQuery),
                |_| persist_pending_events(&self.store, &pending_db_events),
            );
            if let Err(error) = result {
                self.durability_recovery_pending = true;
                durability_errors.push(format!(
                    "record execution events failed; diagnostics remain queued for retry: {error}"
                ));
                self.pending_db_events = pending_db_events.clone();
            } else {
                pending_db_events.clear();
                if self.durability_recovery_pending {
                    recovered_durability = true;
                }
            }
        }
        let cycle_had_durability_errors = !durability_errors.is_empty();
        for error in durability_errors {
            self.report_store_error("batch event persistence", Err(error));
        }
        self.deferred_events = deferred_events;
        if recovered_durability
            && self.durability_recovery_pending
            && !cycle_had_durability_errors
            && pending_db_events.is_empty()
            && self.deferred_events.is_empty()
        {
            self.durability_error = false;
            self.durability_recovery_pending = false;
        }
        if !pending_db_events.is_empty() || !self.deferred_events.is_empty() {
            // A parent Finished event must not finalize the batch while a
            // child completion or its audit trail is waiting on durable
            // storage. The retained events will be retried on the next poll.
            done = None;
        }
        if let Some(r) = done {
            let Some(run_context) = active_run else {
                self.set_status(
                    "Execution completed without a durable run context",
                    StatusSeverity::Error,
                );
                self.receiver = None;
                return;
            };
            let succeeded = r.is_ok();
            let delta_required = r
                .as_ref()
                .is_ok_and(|outcome| *outcome == StreamOutcome::DeltaRequired);
            let was_bulk_run = matches!(run_context.kind, RunKind::Batch);
            let mut direct_final_state = None;
            let terminal_evidence = if succeeded && !run_context.dry_run {
                self.pending_evidence.clone().or_else(|| {
                    (run_context.engine == core::Engine::ImapSync)
                        .then(|| {
                            let output = self.output.iter().cloned().collect::<Vec<_>>();
                            let engine_version = self
                                .store
                                .engine_version(&run_context.run_id)
                                .ok()
                                .flatten();
                            verification::parse_imapsync_evidence_for_version(
                                &output,
                                engine_version.as_deref(),
                            )
                        })
                        .flatten()
                })
            } else {
                self.pending_evidence.clone()
            };
            let terminal_checkpoint = if !was_bulk_run && succeeded {
                self.pending_checkpoint.clone()
            } else {
                None
            };
            if succeeded && !run_context.dry_run && terminal_evidence.is_none() {
                let result = self.store.record_event(
                    &run_context.project_id,
                    "verification_pending",
                    "completed transfer did not provide complete verification evidence",
                );
                self.report_store_error("record incomplete verification", result);
            }
            if !was_bulk_run && let Some(job) = &run_context.job_id {
                if succeeded && run_context.dry_run {
                    let result = self
                        .store
                        .set_preflight_plan(job, &run_context.plan_fingerprint);
                    self.report_store_error("record preflight plan", result);
                }
                let final_state =
                    if succeeded && run_context.dry_run {
                        "ready"
                    } else if !succeeded {
                        if r.as_ref().err().is_some_and(|error| {
                            classify_failure(error) == FailureClass::Verification
                        }) {
                            "attention"
                        } else if r.as_ref().err().is_some_and(|error| {
                            classify_failure(error) == FailureClass::Cancellation
                        }) {
                            "cancelled"
                        } else {
                            "failed"
                        }
                    } else if let Some(evidence) = terminal_evidence.as_ref() {
                        if evidence.is_exact_match() && !delta_required {
                            "verified"
                        } else if delta_required {
                            "delta_required"
                        } else {
                            "verification_difference"
                        }
                    } else if !run_context.dry_run {
                        "attention"
                    } else {
                        "completed"
                    };
                direct_final_state = Some(final_state);
            }
            let mut retry_terminal_commit = false;
            {
                let project = &run_context.project_id;
                let run_id = &run_context.run_id;
                let run_status = if succeeded {
                    "completed"
                } else if r
                    .as_ref()
                    .err()
                    .is_some_and(|error| classify_failure(error) == FailureClass::Verification)
                {
                    "verification_failed"
                } else if r
                    .as_ref()
                    .err()
                    .is_some_and(|error| classify_failure(error) == FailureClass::Cancellation)
                {
                    "cancelled"
                } else {
                    "failed"
                };
                let detail = r
                    .as_ref()
                    .err()
                    .map(|error| classified_failure_detail(error))
                    .unwrap_or_default();
                let terminal_write = if !was_bulk_run
                    && let (Some(job), Some(state)) = (&run_context.job_id, direct_final_state)
                {
                    if run_status == "completed" {
                        if let Some(evidence) = terminal_evidence.as_ref() {
                            self.store
                                .finish_run_for_mailbox_with_evidence_and_checkpoint(
                                    project,
                                    job,
                                    run_id,
                                    run_status,
                                    state,
                                    &detail,
                                    evidence,
                                    terminal_checkpoint.as_deref(),
                                )
                        } else {
                            self.store.finish_run_for_mailbox_with_checkpoint(
                                project,
                                job,
                                run_id,
                                run_status,
                                state,
                                &detail,
                                terminal_checkpoint.as_deref(),
                            )
                        }
                    } else {
                        self.store.finish_run_for_mailbox_with_checkpoint(
                            project,
                            job,
                            run_id,
                            run_status,
                            state,
                            &detail,
                            terminal_checkpoint.as_deref(),
                        )
                    }
                } else {
                    self.store.finish_run(run_id, run_status, &detail)
                };
                let terminal_write_ok = match terminal_write {
                    Ok(()) => true,
                    Err(error) => {
                        self.durability_error = true;
                        self.durability_recovery_pending = true;
                        push_visible_output(
                            &mut self.output,
                            format!("[durability] Could not persist terminal state: {error}"),
                        );
                        retry_terminal_commit = true;
                        false
                    }
                };
                if terminal_write_ok && !was_bulk_run && run_context.dry_run {
                    self.preflight_credential_fingerprint =
                        Some(run_context.credential_fingerprint.clone());
                }
                if terminal_write_ok {
                    if self.durability_recovery_pending && !cycle_had_durability_errors {
                        self.durability_error = false;
                        self.durability_recovery_pending = false;
                    }
                    // These inputs belong to this terminal commit. Do not
                    // consume them before SQLite acknowledges the commit, or
                    // a retry would be unable to reproduce the same durable
                    // result.
                    self.pending_evidence = None;
                    self.pending_checkpoint = None;
                }
                // A terminal run commit is the boundary between an external
                // process result and durable control-plane state.  If that
                // commit failed, the database still owns a running run (or a
                // queued child), so do not append a terminal success/failure
                // event or advance the project phase.  Startup recovery must
                // reconcile it after the operator has restored the database.
                if was_bulk_run && terminal_write_ok {
                    let result = self.store.record_event(
                        project,
                        "run_finished",
                        if succeeded { "success" } else { "failure" },
                    );
                    self.report_store_error("record batch run completion", result);
                }
                // A successful engine result is not enough to advance the
                // lifecycle. Any earlier persistence failure in this event
                // cycle (for example, the preflight digest or evidence
                // event) leaves the durable result incomplete and must keep
                // the project in its current phase for recovery/review.
                if terminal_phase_advance_allowed(
                    succeeded,
                    terminal_write_ok,
                    self.durability_error,
                ) {
                    if run_context.dry_run {
                        let result = self.store.transition(project, core::Phase::Preflight);
                        self.report_store_error("advance project phase", result);
                    } else {
                        let fully_verified = self.store.all_mailboxes_verified(project);
                        match fully_verified {
                            Ok(true) => {
                                let verification =
                                    self.store.transition(project, core::Phase::Verification);
                                if let Err(error) = verification {
                                    self.report_store_error(
                                        "advance project to Verification",
                                        Err(error),
                                    );
                                } else {
                                    let complete =
                                        self.store.transition(project, core::Phase::Complete);
                                    self.report_store_error(
                                        "complete fully verified project",
                                        complete,
                                    );
                                }
                            }
                            Ok(false) => {
                                let result =
                                    self.store.transition(project, core::Phase::Verification);
                                self.report_store_error("advance project phase", result);
                            }
                            Err(error) => self.report_store_error(
                                "check project verification before completion",
                                Err(error),
                            ),
                        }
                    }
                } else if terminal_write_ok && !succeeded && !self.durability_error {
                    let result = self.store.transition(project, core::Phase::Attention);
                    self.report_store_error("move project to Attention", result);
                }
            }
            if retry_terminal_commit {
                // The external process is already gone, but the durable run
                // is still active. Keep ownership and retry the exact
                // terminal event on the next poll instead of forcing startup
                // recovery for a transient SQLite failure.
                self.deferred_events.push_front(Event::Finished(r));
                self.set_status(
                    "Migration result requires durable storage; retrying terminal commit",
                    StatusSeverity::Error,
                );
                return;
            }
            let (completion_status, completion_severity) = if self.durability_error {
                (
                    "Migration result requires durability review".to_owned(),
                    StatusSeverity::Error,
                )
            } else {
                match r {
                    Ok(_) => (
                        successful_run_status(
                            run_context.dry_run,
                            was_bulk_run,
                            direct_final_state,
                        )
                        .to_owned(),
                        successful_run_severity(
                            run_context.dry_run,
                            was_bulk_run,
                            direct_final_state,
                        ),
                    ),
                    Err(e) => (format!("Failed: {e}"), StatusSeverity::Error),
                }
            };
            self.set_status(completion_status, completion_severity);
            self.receiver = None;
            self.cancel_requested = None;
            self.run_started_at = None;
            self.run_id = None;
            self.active_run = None;
            self.locked_profile = None;
            self.pending_batch_evidence.clear();
            // Keep the durable queue after completion so a validated batch
            // can be promoted to live execution, and failed/live jobs can be
            // deliberately retried or run through another delta pass.
            if !was_bulk_run {
                if self.selected_project_id == self.bulk_project_id {
                    self.selected_project_id = None;
                }
                self.bulk_project_id = None;
                self.bulk_job_ids.clear();
                self.bulk_job_index_by_id.clear();
            }
            self.bulk_live_run = false;
            self.live_confirmed = false;
        }
    }
}
