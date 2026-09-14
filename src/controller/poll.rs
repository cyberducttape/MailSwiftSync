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
        let mut pending_db_events = std::mem::take(&mut self.pending_db_events);
        let mut deferred_events = std::mem::take(&mut self.deferred_events);
        let mut durability_errors = Vec::new();
        let mut recovered_durability = false;
        let mut bulk_state_changed = false;
        let active_run = self.active_run.clone();
        let mut ended_processes = HashSet::new();
        let mut done = self.process_poll_events(
            &active_run,
            &mut pending_db_events,
            &mut deferred_events,
            &mut durability_errors,
            &mut recovered_durability,
            &mut bulk_state_changed,
            &mut ended_processes,
        );
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
        self.finish_poll_run(done, active_run, cycle_had_durability_errors);
    }
}
