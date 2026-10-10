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
                    self.language
                        .text("Execution completed without a durable run context"),
                    StatusSeverity::Error,
                );
                self.receiver = None;
                return;
            };
            let mut succeeded = r.is_ok();
            let terminal_failure_class = |error: &str| {
                let provider = crate::controller::failure::provider_for_sided_error(
                    &run_context.source_provider,
                    &run_context.destination_provider,
                    error,
                );
                crate::controller::failure::classify_failure_for_provider(provider, error)
            };
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
            if !was_bulk_run && run_context.job_id.is_some() {
                let final_state = if succeeded && run_context.dry_run {
                    "ready"
                } else if !succeeded {
                    if r.as_ref().err().is_some_and(|error| {
                        terminal_failure_class(error) == FailureClass::Verification
                    }) {
                        "attention"
                    } else if r.as_ref().err().is_some_and(|error| {
                        terminal_failure_class(error) == FailureClass::Cancellation
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
                } else if r.as_ref().err().is_some_and(|error| {
                    terminal_failure_class(error) == FailureClass::Verification
                }) {
                    "verification_failed"
                } else if r.as_ref().err().is_some_and(|error| {
                    terminal_failure_class(error) == FailureClass::Cancellation
                }) {
                    "cancelled"
                } else {
                    "failed"
                };
                // A transfer that completed without evidence records why
                // verification fell short, including any safety limit.
                let verification_failure =
                    (succeeded && !run_context.dry_run && terminal_evidence.is_none())
                        .then_some(self.pending_verification_failure.as_deref())
                        .flatten();
                let detail = r.as_ref().err().map_or_else(
                    || {
                        verification_failure
                            .map(crate::controller::failure::incomplete_verification_detail)
                            .unwrap_or_default()
                    },
                    |error| {
                        let provider = crate::controller::failure::provider_for_sided_error(
                            &run_context.source_provider,
                            &run_context.destination_provider,
                            error,
                        );
                        crate::controller::failure::classified_failure_detail_for_provider(
                            provider, error,
                        )
                    },
                );
                let terminal_write = if !was_bulk_run
                    && let (Some(job), Some(state)) = (&run_context.job_id, direct_final_state)
                {
                    let preflight_plan = (succeeded && run_context.dry_run)
                        .then_some(run_context.plan_fingerprint.as_str());
                    if run_status == "completed" {
                        if let Some(evidence) = terminal_evidence.as_ref() {
                            self.store.finish_run_for_mailbox_with_evidence_and_mismatches_and_preflight_plan_and_checkpoint(
                                project,
                                job,
                                run_id,
                                run_status,
                                state,
                                &detail,
                                evidence,
                                &self.pending_mismatches,
                                preflight_plan,
                                terminal_checkpoint.as_deref(),
                            )
                        } else {
                            self.store
                                .finish_run_for_mailbox_with_preflight_plan_and_checkpoint(
                                    project,
                                    job,
                                    run_id,
                                    run_status,
                                    state,
                                    &detail,
                                    preflight_plan,
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
                // An invariant rejection is deterministic: retrying the same
                // write would hang the controller forever. Record the run as
                // failed and the mailbox for operator review, with the reason,
                // so nothing is presented as verified.
                let terminal_write = match (terminal_write, run_context.job_id.as_deref()) {
                    (Err(error), Some(job))
                        if crate::core::is_ledger_rejection(&error) && !was_bulk_run =>
                    {
                        let review_detail =
                            format!("{detail} — the result could not be recorded: {error}");
                        match self.store.finish_run_for_mailbox_with_checkpoint(
                            project,
                            job,
                            run_id,
                            "failed",
                            "attention",
                            &review_detail,
                            None,
                        ) {
                            Ok(()) => {
                                push_visible_output(
                                    &mut self.output,
                                    format!(
                                        "[durability] The ledger rejected this result ({error}); the mailbox needs operator review."
                                    ),
                                );
                                succeeded = false;
                                direct_final_state = Some("attention");
                                Ok(())
                            }
                            Err(_) => Err(error),
                        }
                    }
                    (result, _) => result,
                };
                let terminal_write_ok = match terminal_write {
                    Ok(()) => true,
                    Err(error) => {
                        if crate::runner::process_supervision_debug_enabled()
                            && !self.durability_recovery_pending
                        {
                            eprintln!(
                                "[process-debug] first terminal persistence failure for run {run_id}: {error}"
                            );
                        }
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
                    if terminal_evidence.is_some()
                        && let Some(path) = run_context
                            .verification_state_path
                            .as_deref()
                            .and_then(|path| run_context.job_id.as_deref().map(|job| (path, job)))
                            .map(|(path, job)| crate::core::durable_stage_path(path, job))
                        && let Err(error) =
                            crate::core::MessageMetadataStage::cleanup_durable_stage(&path)
                    {
                        push_visible_output(
                            &mut self.output,
                            format!(
                                "[durability] Evidence committed; verification stage cleanup deferred: {error}"
                            ),
                        );
                    }
                    if self.durability_recovery_pending && !cycle_had_durability_errors {
                        self.durability_error = false;
                        self.durability_recovery_pending = false;
                    }
                    // These inputs belong to this terminal commit. Do not
                    // consume them before SQLite acknowledges the commit, or
                    // a retry would be unable to reproduce the same durable
                    // result.
                    self.pending_evidence = None;
                    self.pending_verification_failure = None;
                    self.pending_mismatches = core::MismatchSet::default();
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
                    // An approved cutover owns lifecycle advancement.  A
                    // worker completing a seed/catch-up/final-delta pass must
                    // not silently collapse the staged workflow into the
                    // generic Verification/Complete path; the operator (or
                    // an audited cutover command) advances it after reviewing
                    // the durable evidence and maintenance window.
                    let staged_cutover = self.store.cutover_stage(project).ok().flatten().is_some();
                    if staged_cutover {
                        self.set_status(
                            self.language.text(
                                "Cutover pass completed; review durable evidence before advancing the workflow",
                            ),
                            StatusSeverity::Info,
                        );
                    } else if run_context.dry_run {
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
                    self.language.text(
                        "Migration result requires durable storage; retrying terminal commit",
                    ),
                    StatusSeverity::Error,
                );
                return;
            }
            let (completion_status, completion_severity) = if self.durability_error {
                (
                    self.language
                        .text("Migration result requires durability review")
                        .to_owned(),
                    StatusSeverity::Error,
                )
            } else {
                match r {
                    Ok(_) => (
                        self.language
                            .text(successful_run_status(
                                run_context.dry_run,
                                was_bulk_run,
                                direct_final_state,
                            ))
                            .to_owned(),
                        successful_run_severity(
                            run_context.dry_run,
                            was_bulk_run,
                            direct_final_state,
                        ),
                    ),
                    Err(e) => (
                        self.language
                            .text("Migration failed: {error}")
                            .replace("{error}", &e),
                        StatusSeverity::Error,
                    ),
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
            self.pending_batch_mismatches.clear();
            // Keep the durable queue after completion so a validated batch
            // can be promoted to live execution, and failed/live jobs can be
            // deliberately retried or run through another delta pass.
            if !was_bulk_run && self.selected_project_id.as_deref() == self.queue.project_id() {
                self.selected_project_id = None;
            }
            self.queue.clear_all_transient();
            self.bulk_live_run = false;
            self.live_confirmed = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::controller::{ActiveRunContext, RunKind};
    use crate::*;

    fn with_app(test: impl FnOnce(&mut App)) {
        let state_path = std::env::temp_dir().join(format!(
            "mailswiftsync-verification-limit-{}.db",
            uuid::Uuid::new_v4()
        ));
        let mut app = App::from_state_path(Some(&state_path));
        test(&mut app);
        drop(app);
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(state_path.with_extension("lock"));
    }

    /// Drive one single-mailbox live run whose transfer succeeds while its
    /// verification reports `verification_error`, then return the durable
    /// mailbox state, attention reason, run status, and run detail.
    fn finish_with_verification_failure(
        verification_error: &str,
    ) -> (String, Option<core::AttentionReason>, String, String) {
        let mut result = None;
        with_app(|app| {
            let project = app
                .store
                .create_project("limits", "source", "destination")
                .unwrap();
            let job = app
                .store
                .add_mailbox(
                    &project.id,
                    "source@example.test",
                    "destination@example.test",
                )
                .unwrap();
            app.store
                .begin_run(&project.id, &job, "limit-run", "imapsync")
                .unwrap();
            app.active_run = Some(ActiveRunContext {
                run_id: "limit-run".into(),
                project_id: project.id.clone(),
                job_id: Some(job.clone()),
                batch_job_ids: Vec::new(),
                batch_child_run_ids: Vec::new(),
                batch_child_indices: Default::default(),
                batch_plan_fingerprints: Vec::new(),
                kind: RunKind::Single,
                dry_run: false,
                plan_fingerprint: String::new(),
                credential_fingerprint: String::new(),
                source_provider: "generic".into(),
                destination_provider: "generic".into(),
                verification_state_path: None,
            });
            app.run_id = Some("limit-run".into());
            let (sender, receiver) = std::sync::mpsc::sync_channel(8);
            sender
                .send(Event::VerificationFailed(verification_error.to_owned()))
                .unwrap();
            sender
                .send(Event::Finished(Ok(StreamOutcome::Completed)))
                .unwrap();
            drop(sender);
            app.receiver = Some(receiver);
            for _ in 0..10 {
                app.poll();
            }
            let snapshot = app
                .store
                .project_report_snapshot(&project.id)
                .unwrap()
                .unwrap();
            let mailbox = &snapshot.mailboxes[0];
            let run = snapshot
                .runs
                .iter()
                .find(|run| run.run.id == "limit-run")
                .unwrap();
            result = Some((
                mailbox.job.state.clone(),
                mailbox.attention_reason,
                run.run.status.clone(),
                run.run.detail.clone(),
            ));
        });
        result.unwrap()
    }

    #[test]
    fn completed_transfer_with_a_verification_limit_records_the_limit_for_review() {
        let error = core::VerificationLimit::BodyHashMessageCount
            .tag("imap.example.test: body-hash proof is limited to 100000 messages per endpoint");
        let (state, reason, run_status, detail) = finish_with_verification_failure(&error);
        // The transfer completed; the evidence did not. Never verified,
        // never a failed migration.
        assert_eq!(state, "attention");
        assert_eq!(run_status, "completed");
        assert_eq!(
            reason,
            Some(core::AttentionReason::VerificationLimitExceeded)
        );
        assert_eq!(
            core::VerificationLimit::from_detail(&detail),
            Some(core::VerificationLimit::BodyHashMessageCount)
        );
        assert!(detail.contains(core::VerificationLimit::BodyHashMessageCount.guidance()));
    }

    #[test]
    fn completed_transfer_with_a_verifier_error_records_incomplete_evidence() {
        let (state, reason, run_status, detail) =
            finish_with_verification_failure("imap.example.test: connection reset by peer");
        assert_eq!(state, "attention");
        assert_eq!(run_status, "completed");
        assert_eq!(reason, Some(core::AttentionReason::VerificationIncomplete));
        assert!(detail.contains("connection reset by peer"), "{detail}");
        assert_eq!(core::VerificationLimit::from_detail(&detail), None);
    }

    #[test]
    fn a_ledger_rejected_result_goes_to_review_instead_of_retrying_forever() {
        with_app(|app| {
            let project = app
                .store
                .create_project("rejected", "source", "destination")
                .unwrap();
            let job = app
                .store
                .add_mailbox(
                    &project.id,
                    "source@example.test",
                    "destination@example.test",
                )
                .unwrap();
            app.store
                .begin_run(&project.id, &job, "rejected-run", "imapsync")
                .unwrap();
            app.active_run = Some(ActiveRunContext {
                run_id: "rejected-run".into(),
                project_id: project.id.clone(),
                job_id: Some(job.clone()),
                batch_job_ids: Vec::new(),
                batch_child_run_ids: Vec::new(),
                batch_child_indices: Default::default(),
                batch_plan_fingerprints: Vec::new(),
                kind: RunKind::Single,
                dry_run: false,
                plan_fingerprint: String::new(),
                credential_fingerprint: String::new(),
                source_provider: "generic".into(),
                destination_provider: "generic".into(),
                verification_state_path: None,
            });
            app.run_id = Some("rejected-run".into());
            // An exact label over counters that disagree violates a store
            // invariant; no retry of this write can ever succeed.
            let contradictory = core::MailboxEvidence {
                verification_method: core::VerificationMethod::MetadataReconciliation,
                verification_outcome: Some(core::VerificationOutcome::ExactMetadataMatch),
                source_messages: 2,
                destination_messages: 1,
                source_bytes: 20,
                destination_bytes: 10,
                unmatched_messages: Some(0),
                failed_messages: 0,
                source_folders: 1,
                destination_folders: 1,
                authoritative: true,
                missing_messages: 0,
                extra_messages: 0,
                modified_messages: 0,
                probable_messages: 0,
                flag_verification: None,
            };
            let (sender, receiver) = std::sync::mpsc::sync_channel(8);
            sender.send(Event::Evidence(contradictory)).unwrap();
            sender
                .send(Event::Finished(Ok(StreamOutcome::Completed)))
                .unwrap();
            drop(sender);
            app.receiver = Some(receiver);
            for _ in 0..10 {
                app.poll();
            }
            assert!(app.deferred_events.is_empty());
            assert!(!app.running());
            let snapshot = app
                .store
                .project_report_snapshot(&project.id)
                .unwrap()
                .unwrap();
            assert_eq!(snapshot.mailboxes[0].job.state, "attention");
            let run = snapshot
                .runs
                .iter()
                .find(|run| run.run.id == "rejected-run")
                .unwrap();
            assert_eq!(run.run.status, "failed");
            assert!(
                run.run.detail.contains(
                    "ledger rejected the write: an exact verification label contradicts the evidence counters"
                ),
                "{}",
                run.run.detail
            );
        });
    }
}
