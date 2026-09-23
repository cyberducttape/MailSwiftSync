//! Event consumption for the execution controller.

use crate::*;

impl App {
    /// Consume at most the configured number of worker events for one UI
    /// cycle. This keeps process/event reduction separate from refresh and
    /// terminal completion bookkeeping in the outer poll loop.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_poll_events(
        &mut self,
        active_run: &Option<ActiveRunContext>,
        pending_db_events: &mut Vec<PendingDbEvent>,
        deferred_events: &mut VecDeque<Event>,
        durability_errors: &mut Vec<String>,
        recovered_durability: &mut bool,
        bulk_state_changed: &mut bool,
        ended_processes: &mut HashSet<(String, String)>,
    ) -> Option<Result<StreamOutcome, String>> {
        let mut done = None;
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
                            ended_processes,
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
                            let bulk_index = self
                                .bulk_job_index_by_id
                                .get(&job_id)
                                .copied()
                                .unwrap_or(index);
                            if let Some(job) = self.bulk_jobs.get_mut(bulk_index) {
                                job.state = state.clone();
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
                            // Durable state is authoritative. Do not show a
                            // terminal child state in the editable queue until
                            // the run/mailbox transaction has committed.
                            if completion_persisted
                                && let Some(bulk_index) =
                                    self.bulk_job_index_by_id.get(&job_id).copied()
                                && let Some(job) = self.bulk_jobs.get_mut(bulk_index)
                            {
                                job.state = display_job_state(&final_state).into();
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
                                });
                                break;
                            } else {
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

        done
    }
}
