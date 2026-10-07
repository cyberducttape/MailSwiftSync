//! Event consumption for the execution controller.

use crate::*;

/// Mutable state shared across one bounded event-reduction pass.
///
/// Keeping this as a single value makes the poll-cycle contract explicit and
/// gives the reducer a natural seam for later extraction from `App` without
/// hiding durability ordering in a long positional argument list.
pub(crate) struct PollEventState {
    pub(crate) active_run: Option<ActiveRunContext>,
    pub(crate) pending_db_events: Vec<PendingDbEvent>,
    pub(crate) deferred_events: VecDeque<Event>,
    pub(crate) durability_errors: Vec<String>,
    pub(crate) recovered_durability: bool,
    pub(crate) bulk_state_changed: bool,
    pub(crate) ended_processes: HashSet<(String, String)>,
}

impl App {
    /// Consume at most the configured number of worker events for one UI
    /// cycle. This keeps process/event reduction separate from refresh and
    /// terminal completion bookkeeping in the outer poll loop.
    pub(crate) fn process_poll_events(
        &mut self,
        cycle: &mut PollEventState,
    ) -> Option<Result<StreamOutcome, String>> {
        let PollEventState {
            active_run,
            pending_db_events,
            deferred_events,
            durability_errors,
            recovered_durability,
            bulk_state_changed,
            ended_processes,
        } = cycle;
        let mut done = None;
        if let Some(rx) = &self.receiver {
            let mut processed_events = 0;
            while processed_events < MAX_EVENTS_PER_FRAME {
                let event = if let Some(event) = deferred_events.pop_front() {
                    event
                } else {
                    match rx.try_recv() {
                        Ok(event) => event,
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            // Workers always send `Finished` last, including
                            // from their panic guards. A closed channel with
                            // no terminal event means the worker vanished;
                            // finalize as a failure needing review instead of
                            // leaving the UI "running" forever.
                            if done.is_none() {
                                done = Some(Err(format!(
                                    "execution {}; migration requires operator review",
                                    crate::controller::poll::WORKER_STOPPED_UNEXPECTEDLY
                                )));
                            }
                            break;
                        }
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
                            ended_processes,
                            &run_id,
                            &job_id,
                        );
                        if !owns_line {
                            // RunLine is an asynchronous presentation event;
                            // never let a delayed or foreign worker append
                            // output to the active migration's journal. Since
                            // it is not durable state, rejecting a stale line
                            // must not poison the migration's durability result.
                        } else {
                            // Engine output is presentation-only. It may
                            // contain subjects, folder metadata, or other
                            // message-derived text, so retain it only in the
                            // bounded process-local journal. Producers redact
                            // their own per-mailbox secrets; redact again
                            // against the active plan so a producer that
                            // interpolates an unsanitized error cannot reach
                            // the journal.
                            let safe = ui::redact_secrets(
                                &text,
                                [
                                    self.form.source_password.as_str(),
                                    self.form.destination_password.as_str(),
                                ],
                            );
                            push_visible_output(&mut self.output, safe);
                        }
                    }
                    Event::DiagnosticLinesDropped {
                        run_id,
                        job_id,
                        count,
                    } => {
                        if count == 0
                            || !process_event_is_current(active_run.as_ref(), &run_id, &job_id)
                        {
                            durability_errors.push(format!(
                                "ignored diagnostic-drop accounting for unknown process {run_id}"
                            ));
                        } else {
                            push_visible_output(
                                &mut self.output,
                                format!(
                                    "[diagnostics] {count} output line(s) omitted because the operator event queue was full"
                                ),
                            );
                            // Persist only the bounded count, never engine text.
                            pending_db_events.push(PendingDbEvent::new(
                                run_id,
                                "diagnostic_lines_dropped".into(),
                                count.to_string(),
                            ));
                        }
                    }
                    Event::TransferAttempt {
                        run_id,
                        job_id,
                        attempt,
                        status,
                        reply,
                    } => {
                        let result = if attempt == 0
                            || !process_event_is_current(active_run.as_ref(), &run_id, &job_id)
                        {
                            Err(format!(
                                "ignored transfer-attempt record for unknown process {run_id}"
                            ))
                        } else {
                            match &status {
                                controller::TransferAttemptStatus::Finished {
                                    outcome,
                                    completion,
                                } => self.store.record_transfer_attempt_finished(
                                    &run_id,
                                    attempt,
                                    outcome.durable_outcome(),
                                    completion,
                                ),
                                controller::TransferAttemptStatus::Started(intent) => self
                                    .store
                                    .record_transfer_attempt_started(&run_id, attempt, intent),
                            }
                            .map_err(|error| {
                                format!("could not persist transfer attempt {attempt}: {error}")
                            })
                        };
                        if let Err(error) = &result {
                            durability_errors.push(error.clone());
                        }
                        let _ = reply.send(result);
                    }
                    Event::TransferPassVerified {
                        run_id,
                        job_id,
                        attempt,
                        method,
                        outcome,
                        folders,
                        reply,
                    } => {
                        let result = if attempt == 0
                            || !process_event_is_current(active_run.as_ref(), &run_id, &job_id)
                        {
                            Err(format!(
                                "ignored transfer-pass verification for unknown run {run_id}"
                            ))
                        } else {
                            self.store
                                .record_transfer_pass_verified(
                                    &run_id, attempt, &method, &outcome, &folders,
                                )
                                .map_err(|error| {
                                    format!(
                                        "could not persist verification of transfer attempt {attempt}: {error}"
                                    )
                                })
                        };
                        if let Err(error) = &result {
                            durability_errors.push(error.clone());
                        }
                        let _ = reply.send(result);
                    }
                    Event::EngineVersion {
                        run_id,
                        job_id,
                        version,
                        reply,
                    } => {
                        let owns_version =
                            process_event_is_current(active_run.as_ref(), &run_id, &job_id);
                        let result = if owns_version {
                            // Engine identity is resolved before launch and
                            // is part of the verification trust boundary.
                            // Preserve any persistence failure for terminal
                            // durability review.
                            self.store
                                .record_engine_version(&run_id, &version)
                                .map_err(|error| {
                                    format!(
                                        "could not persist engine version metadata for {run_id}: {error}"
                                    )
                                })
                        } else {
                            Err(format!(
                                "ignored engine version for unknown run {run_id} and job {job_id}"
                            ))
                        };
                        if let Err(error) = &result {
                            durability_errors.push(error.clone());
                        }
                        let _ = reply.send(result);
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
                            let _ = index;
                            // Claims and terminal states are durable; only a
                            // retry wait is presentation-only state.
                            if crate::ui::display_state_key(&state) == "retrying" {
                                self.queue.set_transient(&job_id, "retrying");
                            } else {
                                self.queue.clear_transient(&job_id);
                            }
                            *bulk_state_changed = true;
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
                        reply,
                    } => {
                        // Diagnostics are accepted only while a child is
                        // active/queued. Flush them before the terminal
                        // transaction so a fast worker cannot deliver
                        // RunLine(s) and JobFinished in one poll cycle and
                        // lose the child log to the terminal-state guard.
                        if !pending_db_events.is_empty() {
                            match persist_pending_events(&self.store, pending_db_events) {
                                Ok(()) => {
                                    pending_db_events.clear();
                                    if self.durability_recovery_pending {
                                        *recovered_durability = true;
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
                                        reply,
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
                            let mismatches = self.pending_batch_mismatches.get(&child_run_id);
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
                                    mismatches,
                                    checkpoint,
                                },
                            );
                            let completion_persisted = result.is_ok();
                            let reply_result = result
                                .as_ref()
                                .map(|_| ())
                                .map_err(|error| error.to_string());
                            // Durable state is authoritative. Do not show a
                            // terminal child state in the editable queue until
                            // the run/mailbox transaction has committed.
                            if completion_persisted {
                                // The durable row now carries the terminal
                                // state; drop the in-run presentation state.
                                self.queue.clear_transient(&job_id);
                                *bulk_state_changed = true;
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
                                    reply,
                                });
                                break;
                            } else {
                                let _ = reply.send(reply_result);
                                self.run_telemetry.record_job_finished(
                                    &job_id,
                                    &final_state,
                                    &detail,
                                    std::time::Instant::now(),
                                );
                                if self.durability_recovery_pending {
                                    *recovered_durability = true;
                                }
                                self.pending_batch_evidence.remove(&child_run_id);
                                self.pending_batch_mismatches.remove(&child_run_id);
                                self.pending_batch_checkpoints.remove(&child_run_id);
                            }
                            if completion_persisted
                                && !self.bulk_live_run
                                && state == "ready"
                                && let Some(fingerprint) = credential_fingerprint
                            {
                                self.queue
                                    .preflight_credentials
                                    .insert(job_id.clone(), fingerprint);
                            }
                        } else {
                            let _ = reply
                                .send(Err("ignored batch completion event for unknown child run"
                                    .to_owned()));
                            durability_errors.push(format!(
                                "ignored batch completion event for unknown child run {child_run_id}"
                            ));
                        }
                    }
                    Event::BatchEvidence {
                        job_id,
                        child_run_id,
                        evidence,
                        mismatches,
                    } => {
                        if let Some(run) = active_run.as_ref()
                            && matches!(run.kind, RunKind::Batch)
                            && run.batch_child_index(&job_id, &child_run_id).is_some()
                        {
                            self.pending_batch_evidence
                                .insert(child_run_id.clone(), evidence);
                            self.pending_batch_mismatches
                                .insert(child_run_id, mismatches);
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
                        let evidence_level = evidence.verification_outcome().as_str();
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
                    Event::MessageMismatches {
                        run_id,
                        job_id,
                        mismatches,
                    } => {
                        if active_run
                            .as_ref()
                            .is_some_and(|run| run.owns_process(&run_id, &job_id))
                        {
                            self.pending_mismatches = mismatches;
                        } else {
                            durability_errors.push(format!(
                                "ignored message mismatch event for unknown run {run_id}"
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
                            self.pending_verification_failure = Some(safe.clone());
                            pending_db_events.push(PendingDbEvent::new(
                                run_id.to_owned(),
                                "verification_pending".into(),
                                safe,
                            ));
                        }
                    }
                    Event::Progress {
                        run_id,
                        job_id,
                        progress,
                    } => {
                        // Presentation-only and content-free; stale or
                        // foreign processes are ignored like engine lines.
                        if process_event_is_current(active_run.as_ref(), &run_id, &job_id) {
                            self.run_telemetry.record_progress(
                                &job_id,
                                progress,
                                std::time::Instant::now(),
                            );
                        }
                    }
                    Event::TransferProgressCheckpoint {
                        run_id,
                        job_id,
                        attempt,
                        progress,
                    } => {
                        if attempt == 0
                            || !process_event_is_current(active_run.as_ref(), &run_id, &job_id)
                        {
                            durability_errors.push(format!(
                                "ignored transfer-progress checkpoint for unknown process {run_id}"
                            ));
                        } else if let Ok(detail) = serde_json::to_string(&serde_json::json!({
                            "attempt": attempt,
                            "progress": progress,
                        })) {
                            // The checkpoint is content-free and bounded by
                            // the fixed progress schema. It is persisted with
                            // the same retryable event batch as other audit
                            // facts, never as raw engine output.
                            pending_db_events.push(PendingDbEvent::new(
                                run_id,
                                "transfer_progress_checkpoint".into(),
                                detail,
                            ));
                        } else {
                            durability_errors
                                .push("could not encode transfer-progress checkpoint".into());
                        }
                    }
                    Event::RetryScheduled {
                        job_id,
                        attempt,
                        delay,
                        failure_class,
                    } => {
                        if active_run.is_some() {
                            self.run_telemetry.record_retry(
                                &job_id,
                                controller::telemetry::RetryNote {
                                    attempt,
                                    retry_at: std::time::Instant::now() + delay,
                                    failure_class,
                                },
                            );
                        }
                    }
                    Event::ProviderCooldown {
                        domain,
                        provider,
                        endpoint,
                        until,
                    } => {
                        if active_run.is_some() {
                            self.run_telemetry
                                .record_cooldown(&domain, provider, endpoint, until);
                        }
                    }
                    Event::Finished(r) => {
                        if let Err(error) = &r {
                            self.run_telemetry
                                .record_run_failure(error, std::time::Instant::now());
                        }
                        static REPORTED_FINISHED_EVENT: std::sync::atomic::AtomicBool =
                            std::sync::atomic::AtomicBool::new(false);
                        if crate::runner::process_supervision_debug_enabled()
                            && !REPORTED_FINISHED_EVENT
                                .swap(true, std::sync::atomic::Ordering::Relaxed)
                        {
                            eprintln!("[process-debug] controller poll consumed terminal event");
                        }
                        done = Some(r);
                    }
                }
            }
        }

        done
    }
}
