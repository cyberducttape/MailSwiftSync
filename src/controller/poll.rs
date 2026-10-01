use crate::*;

pub(crate) const WORKER_STOPPED_UNEXPECTEDLY: &str =
    "worker terminated unexpectedly before returning a result";

/// Outcome of draining one single-result background channel.
pub(crate) enum WorkerPoll<T> {
    Pending,
    Ready(T),
    /// The worker dropped its sender (exit or panic) without a result.
    Stopped,
}

/// Drain a single-result worker channel. Both a result and a disconnected
/// sender clear the slot, so a crashed worker can never leave its receiver
/// behind to make `background_work_pending` and the start/import guards
/// believe the operation is still running.
pub(crate) fn poll_worker<T>(slot: &mut Option<Receiver<T>>) -> WorkerPoll<T> {
    let Some(receiver) = slot.as_ref() else {
        return WorkerPoll::Pending;
    };
    match receiver.try_recv() {
        Ok(value) => {
            *slot = None;
            WorkerPoll::Ready(value)
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            *slot = None;
            WorkerPoll::Stopped
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => WorkerPoll::Pending,
    }
}

impl App {
    pub(crate) fn poll(&mut self) {
        // Exact callers (probe results, assessment, admission) recompute the
        // fingerprint themselves; this per-frame check only expires stale
        // observations promptly, so it need not hash files on every repaint.
        let staleness_check_due = self
            .capability_staleness_checked_at
            .is_none_or(|checked| checked.elapsed() >= std::time::Duration::from_secs(1));
        if staleness_check_due {
            self.capability_staleness_checked_at = Some(std::time::Instant::now());
        }
        if staleness_check_due && self.invalidate_stale_capability_observation() {
            self.set_status(
                "Readiness observations expired because the migration plan changed; run discovery again.",
                StatusSeverity::Warning,
            );
        }
        match poll_worker(&mut self.bulk_import_receiver) {
            WorkerPoll::Ready(result) => self.apply_bulk_import_result(result),
            WorkerPoll::Stopped => {
                self.apply_bulk_import_result(Err(format!(
                    "Mailbox import {WORKER_STOPPED_UNEXPECTEDLY}; no rows were imported."
                )));
                self.set_status(
                    format!("Mailbox import {WORKER_STOPPED_UNEXPECTEDLY}."),
                    StatusSeverity::Error,
                );
            }
            WorkerPoll::Pending => {}
        }
        if let Some(receiver) = &self.manual_oauth_refresh_receiver {
            match receiver.try_recv() {
                Ok(result) => {
                    self.manual_oauth_refresh_receiver = None;
                    self.complete_manual_oauth_refresh(result);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.manual_oauth_refresh_receiver = None;
                    self.set_status(
                        "OAuth refresh worker stopped before returning a result.",
                        StatusSeverity::Error,
                    );
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        match poll_worker(&mut self.keyring_operation_receiver) {
            WorkerPoll::Ready(result) => self.complete_keyring_operation(result),
            WorkerPoll::Stopped => self.set_status(
                format!("OS keyring {WORKER_STOPPED_UNEXPECTEDLY}."),
                StatusSeverity::Error,
            ),
            WorkerPoll::Pending => {}
        }
        loop {
            let message = self
                .oauth_authorization_receiver
                .as_ref()
                .map(Receiver::try_recv);
            match message {
                Some(Ok(crate::ui::OAuthAuthorizationMessage::Progress(stage))) => {
                    self.oauth_authorization_stage = Some(stage);
                }
                Some(Ok(crate::ui::OAuthAuthorizationMessage::Finished(result))) => {
                    self.oauth_authorization_receiver = None;
                    self.oauth_authorization_cancel = None;
                    self.oauth_authorization_stage = None;
                    self.complete_oauth_authorization(result);
                    break;
                }
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    self.oauth_authorization_receiver = None;
                    self.oauth_authorization_cancel = None;
                    self.oauth_authorization_stage = None;
                    self.set_status(
                        self.language.message(
                            "ui.oauth-authorization-worker-stopped-before-returning-a-result",
                        ),
                        StatusSeverity::Error,
                    );
                    break;
                }
                Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => break,
            }
        }
        let start_credentials_result = match poll_worker(&mut self.start_credentials_receiver) {
            WorkerPoll::Ready(result) => Some(result),
            WorkerPoll::Stopped => Some(Err(format!(
                "Credential loading {WORKER_STOPPED_UNEXPECTEDLY}; migration was not started."
            ))),
            WorkerPoll::Pending => None,
        };
        if let Some(result) = start_credentials_result {
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
        let live_auth_result = match poll_worker(&mut self.live_auth_receiver) {
            WorkerPoll::Ready(result) => Some(result),
            WorkerPoll::Stopped => Some(Err(format!(
                "live authentication {WORKER_STOPPED_UNEXPECTEDLY}"
            ))),
            WorkerPoll::Pending => None,
        };
        if let Some(result) = live_auth_result {
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
        let capability_result = match poll_worker(&mut self.capability_receiver) {
            WorkerPoll::Ready(result) => Some(result),
            WorkerPoll::Stopped => {
                self.capability_probe_request_id = None;
                self.capability_probe_fingerprint = None;
                self.set_status(
                    format!("Preflight discovery failed: capability {WORKER_STOPPED_UNEXPECTEDLY}"),
                    StatusSeverity::Error,
                );
                None
            }
            WorkerPoll::Pending => None,
        };
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
        let mut event_cycle = controller::PollEventState {
            active_run: self.active_run.clone(),
            pending_db_events: std::mem::take(&mut self.pending_db_events),
            deferred_events: std::mem::take(&mut self.deferred_events),
            durability_errors: Vec::new(),
            recovered_durability: false,
            bulk_state_changed: false,
            ended_processes: HashSet::new(),
        };
        let mut done = self.process_poll_events(&mut event_cycle);
        let controller::PollEventState {
            active_run,
            mut pending_db_events,
            deferred_events,
            mut durability_errors,
            mut recovered_durability,
            bulk_state_changed,
            ..
        } = event_cycle;
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
            // storage. The retained events will be retried on the next poll;
            // the terminal event itself must be retained after them, or the
            // run would never finish once the worker has exited.
            if let Some(result) = done.take() {
                self.deferred_events.push_back(Event::Finished(result));
            }
        }
        // First service worker requests and commit their durable acknowledgments.
        // Refreshing the presentation snapshot can perform synchronous SQLite
        // reads (including waiting on busy_timeout); it must not consume the
        // same deadline budget as a worker waiting for this controller.
        self.finish_poll_run(done, active_run, cycle_had_durability_errors);

        let was_workspace_stale = self.ui_snapshot.is_stale();
        self.refresh_ui_snapshot();
        if !was_workspace_stale && self.ui_snapshot.is_stale() && !self.running() {
            self.set_status(
                "Durable state view is stale; execution is disabled until SQLite refresh succeeds.",
                StatusSeverity::Error,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WORKER_STOPPED_UNEXPECTEDLY;
    use crate::App;
    use std::sync::mpsc::{Sender, channel};

    fn with_app(test: impl FnOnce(&mut App)) {
        let state_path = std::env::temp_dir().join(format!(
            "mailswiftsync-worker-stop-{}.db",
            uuid::Uuid::new_v4()
        ));
        let mut app = App::from_state_path(Some(&state_path));
        test(&mut app);
        drop(app);
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(state_path.with_extension("lock"));
    }

    /// Simulate both ways a worker can vanish: returning without sending,
    /// and panicking while it owns the sender.
    fn stop_worker<T: Send + 'static>(sender: Sender<T>, panic: bool) {
        let worker = std::thread::spawn(move || {
            let _sender = sender;
            if panic {
                panic!("deliberate worker panic");
            }
        });
        assert_eq!(worker.join().is_err(), panic);
    }

    fn assert_released(app: &App, expected_status: &str) {
        assert!(
            !app.background_work_pending(),
            "a stopped worker left a receiver behind"
        );
        assert_eq!(app.status.severity, crate::StatusSeverity::Error);
        assert!(
            app.status.text.contains(expected_status),
            "unexpected status: {}",
            app.status.text
        );
    }

    #[test]
    fn stopped_credential_loader_releases_start_and_reports() {
        for panic in [false, true] {
            with_app(|app| {
                let (sender, receiver) = channel();
                app.start_credentials_receiver = Some(receiver);
                app.start_credentials_plan = Some("plan".into());
                stop_worker(sender, panic);
                app.poll();
                assert_released(app, "Credential loading");
                assert!(app.status.text.contains(WORKER_STOPPED_UNEXPECTEDLY));
                assert!(app.start_credentials_plan.is_none());
                assert!(!app.credentials_ready_for_start);
            });
        }
    }

    #[test]
    fn stopped_live_auth_probe_releases_admission_and_reports() {
        for panic in [false, true] {
            with_app(|app| {
                let (sender, receiver) = channel();
                app.live_auth_receiver = Some(receiver);
                stop_worker(sender, panic);
                app.poll();
                assert_released(app, "Live authentication failed");
                assert!(app.live_auth_proof.is_none());
            });
        }
    }

    #[test]
    fn stopped_capability_probe_releases_discovery_and_reports() {
        for panic in [false, true] {
            with_app(|app| {
                let (sender, receiver) = channel();
                app.capability_receiver = Some(receiver);
                app.capability_probe_request_id = Some("request".into());
                app.capability_probe_fingerprint =
                    Some(crate::plan_fingerprint_digest(&app.form.plan_fingerprint()));
                stop_worker(sender, panic);
                app.poll();
                assert_released(app, "Preflight discovery failed");
                assert!(app.capability_probe_request_id.is_none());
                assert!(app.capability_probe_fingerprint.is_none());
            });
        }
    }

    #[test]
    fn stopped_bulk_importer_releases_import_and_reports() {
        for panic in [false, true] {
            with_app(|app| {
                let (sender, receiver) = channel();
                app.bulk_import_receiver = Some(receiver);
                stop_worker(sender, panic);
                app.poll();
                assert_released(app, "Mailbox import");
                assert!(app.bulk_message.contains(WORKER_STOPPED_UNEXPECTEDLY));
            });
        }
    }

    #[test]
    fn stopped_oauth_workers_release_their_receivers() {
        for panic in [false, true] {
            with_app(|app| {
                let (sender, receiver) = channel();
                app.manual_oauth_refresh_receiver = Some(receiver);
                stop_worker(sender, panic);
                app.poll();
                assert_released(app, "OAuth refresh worker stopped");
            });
            with_app(|app| {
                let (sender, receiver) = channel();
                app.oauth_authorization_receiver = Some(receiver);
                stop_worker(sender, panic);
                app.poll();
                assert_released(app, "OAuth authorization worker stopped");
                assert!(app.oauth_authorization_cancel.is_none());
            });
        }
    }

    #[test]
    fn stopped_keyring_worker_releases_credential_controls() {
        for panic in [false, true] {
            with_app(|app| {
                let (sender, receiver) = channel();
                app.keyring_operation_receiver = Some(receiver);
                stop_worker(sender, panic);
                app.poll();
                assert_released(app, "OS keyring");
                assert!(!app.keyring_operation_pending());
            });
        }
    }

    #[test]
    fn stopped_execution_worker_without_terminal_event_ends_the_run() {
        for panic in [false, true] {
            with_app(|app| {
                let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                app.receiver = Some(receiver);
                stop_worker_sync(sender, panic);
                app.poll();
                assert!(
                    !app.running(),
                    "execution receiver survived a vanished worker"
                );
                assert!(!app.background_work_pending());
            });
        }
    }

    #[test]
    fn progress_from_the_active_process_reaches_telemetry_and_foreign_progress_does_not() {
        with_app(|app| {
            let (sender, receiver) = std::sync::mpsc::sync_channel(8);
            app.receiver = Some(receiver);
            app.active_run = Some(crate::controller::ActiveRunContext {
                run_id: "run-a".into(),
                project_id: "project-a".into(),
                job_id: Some("job-a".into()),
                batch_job_ids: Vec::new(),
                batch_child_run_ids: Vec::new(),
                batch_child_indices: std::collections::HashMap::new(),
                batch_plan_fingerprints: Vec::new(),
                kind: crate::controller::RunKind::Single,
                dry_run: true,
                plan_fingerprint: "plan-a".into(),
                credential_fingerprint: "credential-a".into(),
            });
            app.run_telemetry.reset(std::time::Instant::now(), 1);
            let progress = |bytes| crate::progress::TransferProgress {
                bytes_copied: bytes,
                messages_copied: 1,
                ..Default::default()
            };
            for (run_id, job_id, bytes) in [("run-a", "job-a", 5_000), ("run-b", "job-a", 9_000)] {
                sender
                    .send(crate::Event::Progress {
                        run_id: run_id.into(),
                        job_id: job_id.into(),
                        progress: progress(bytes),
                    })
                    .unwrap();
            }
            app.poll();
            assert_eq!(app.run_telemetry.totals(), (5_000, 1));
            app.active_run = None;
            app.receiver = None;
        });
    }

    fn stop_worker_sync<T: Send + 'static>(sender: std::sync::mpsc::SyncSender<T>, panic: bool) {
        let worker = std::thread::spawn(move || {
            let _sender = sender;
            if panic {
                panic!("deliberate worker panic");
            }
        });
        assert_eq!(worker.join().is_err(), panic);
    }
}
