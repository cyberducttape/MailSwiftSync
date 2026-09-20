use crate::*;

impl App {
    pub(crate) fn finish_poll_run(
        &mut self,
        done: Option<Result<StreamOutcome, String>>,
        active_run: Option<ActiveRunContext>,
        cycle_had_durability_errors: bool,
    ) {
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
            // The streaming parser profile is selected from the executable
            // version before launch. Never re-parse the bounded UI transcript
            // as a fallback, because that would bypass the profile invariant
            // and could promote incomplete output.
            let terminal_evidence = self.pending_evidence.clone();
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
