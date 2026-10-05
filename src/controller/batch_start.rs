//! Batch launch orchestration owned by the controller layer.

use crate::controller::{
    BatchExecutionContext, BatchLaunchRequest, BatchStartContext, BatchStartDecision,
    admit_batch_launch, launch_batch_worker,
};
use crate::{App, StatusSeverity};
use std::time::Instant;

impl App {
    /// Admit and start a batch after the UI has selected an execution mode.
    /// Rendering remains in `ui/batch.rs`; durable admission and worker
    /// ownership remain in the controller layer.
    pub(crate) fn start_bulk(&mut self) {
        let mode = self.bulk_mode;
        let live = mode.is_live();
        match crate::controller::batch_start_decision(BatchStartContext {
            mode,
            durable_view_stale: self.ui_snapshot.is_stale(),
            profile_available: self.profile_available,
            read_only_project: self.workspace_read_only,
            process_review_required: self.process_review_required,
            has_jobs: !self.queue.is_empty(),
            live_confirmed: self.bulk_live_confirmed,
            persistence_available: self.persistence_available,
        }) {
            BatchStartDecision::Block(block) => {
                self.bulk_message = self.language.text(block.message()).into();
                return;
            }
            BatchStartDecision::ConfirmLive => {
                self.refresh_ui_snapshot_now();
                self.bulk_live_confirm_open = true;
                self.bulk_live_confirm_focus_requested = false;
                self.bulk_confirmation_summary = None;
                return;
            }
            BatchStartDecision::Proceed => {}
        }
        if live {
            self.bulk_live_confirmed = false;
        }
        let Some(project_id) = self.queue.project_id().map(str::to_owned) else {
            self.bulk_message = self
                .language
                .text("Import a mailbox file before running a batch.")
                .into();
            return;
        };
        self.durability_error = false;
        self.durability_recovery_pending = false;
        self.pending_batch_evidence.clear();
        self.pending_batch_mismatches.clear();
        self.pending_batch_checkpoints.clear();
        // A selected wave launches only inside its maintenance window.
        if live
            && let Some(wave) = self.selected_wave()
            && let Some(window) = wave.settings.maintenance_window.as_deref()
            && crate::maintenance_window::MaintenanceWindow::parse(window)
                .is_ok_and(|window| !window.contains_now())
        {
            self.bulk_message = self
                .language
                .message("ui.wave-outside-window")
                .replace("{wave}", &wave.settings.name)
                .replace("{window}", window);
            return;
        }
        let batch_profile = self.effective_batch_profile();
        let run_id = uuid::Uuid::new_v4().to_string();
        let selection_scope = self.bulk_selection_scope();
        let loader = match crate::controller::queue::ledger_job_loader(
            &self.store,
            self.state_path
                .clone()
                .filter(|_| self.persistence_available),
            &project_id,
            batch_profile.clone(),
            self.queue.session_secrets.clone(),
            mode.is_preflight(),
        ) {
            Ok(loader) => loader,
            Err(error) => {
                self.bulk_message = error;
                return;
            }
        };
        let admission = match admit_batch_launch(BatchLaunchRequest {
            store: &self.store,
            project_id: &project_id,
            selection_scope: &selection_scope,
            retry_scope: self.bulk_retry_scope,
            mode,
            fallback_profile: &batch_profile,
            expected_credential_fingerprints: &self.queue.preflight_credentials,
            session_secrets: &self.queue.session_secrets,
            expected_action_plan_hash: self
                .bulk_confirmation_summary
                .as_ref()
                .map(|plan| plan.identity_hash.as_str()),
            acknowledge_ambiguous_destination_case: self.bulk_destination_case_acknowledged,
            run_id: &run_id,
        }) {
            Ok(value) => value,
            Err(error) => {
                self.bulk_message = error;
                return;
            }
        };
        let prepared = admission.prepared;
        let selected_job_ids = prepared.selected_job_ids.clone();
        let job_count = selected_job_ids.len();
        let concurrency =
            crate::migration_plan::effective_batch_concurrency(batch_profile.batch_concurrency);
        self.selected_project_id = Some(project_id.clone());
        self.bulk_live_run = live;
        self.run_id = Some(run_id.clone());
        self.locked_profile = Some(batch_profile.clone());
        self.active_run = Some(admission.active_run);
        // Admission recorded the children as durably queued.
        self.mark_bulk_jobs_changed();
        let worker = launch_batch_worker(BatchExecutionContext {
            concurrency,
            provider_rate_ceilings: prepared.provider_rate_ceilings,
            launch_starts_per_second:
                crate::migration_plan::effective_batch_process_starts_per_second(
                    self.form.profile.batch_process_starts_per_second,
                ),
            mode,
            retry_count: self.form.profile.batch_retry_count.min(3),
            job_count,
            queue_job_ids: selected_job_ids,
            child_run_ids: self
                .active_run
                .as_ref()
                .map(|run| run.batch_child_run_ids.clone())
                .unwrap_or_default(),
            queue_checkpoints: prepared.queue_checkpoints,
            batch_project_id: project_id,
            batch_run_id: run_id,
            loader,
            imapsync_executables: prepared.imapsync_executables,
            verification_state_path: self.state_path.clone(),
            diagnostic_logger: self.diagnostic_logger.clone(),
        });
        self.cancel_requested = Some(worker.cancel.clone());
        self.receiver = Some(worker.receiver);
        self.run_started_at = Some(Instant::now());
        self.run_telemetry.reset(Instant::now(), job_count);
        self.set_status(
            format!(
                "{}: {} jobs",
                if live {
                    "Batch migration"
                } else {
                    "Batch validation"
                },
                job_count
            ),
            StatusSeverity::Info,
        );
        self.output.clear();
    }
}
