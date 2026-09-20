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
            has_jobs: !self.bulk_jobs.is_empty(),
            live_confirmed: self.bulk_live_confirmed,
            persistence_available: self.persistence_available,
        }) {
            BatchStartDecision::Block(block) => {
                self.bulk_message = block.message().into();
                return;
            }
            BatchStartDecision::ConfirmLive => {
                self.refresh_ui_snapshot_now();
                self.bulk_live_confirm_open = true;
                self.bulk_confirmation_summary = None;
                return;
            }
            BatchStartDecision::Proceed => {}
        }
        if live {
            self.bulk_live_confirmed = false;
            if self.bulk_project_id.is_none() || self.bulk_job_ids.len() != self.bulk_jobs.len() {
                self.bulk_message =
                    "Run a successful preflight for this queue before starting live migrations."
                        .into();
                return;
            }
        }
        self.durability_error = false;
        self.durability_recovery_pending = false;
        self.pending_batch_evidence.clear();
        self.pending_batch_checkpoints.clear();
        let run_id = uuid::Uuid::new_v4().to_string();
        let admission = match admit_batch_launch(BatchLaunchRequest {
            store: &self.store,
            requested_project_id: self.bulk_project_id.as_deref(),
            source_jobs: &self.bulk_jobs,
            queue_job_ids: &self.bulk_job_ids,
            selected_ids: &self.bulk_selected_ids,
            retry_scope: self.bulk_retry_scope,
            mode,
            fallback_profile: &self.form.profile,
            expected_credential_fingerprints: &self.bulk_preflight_credential_fingerprints,
            run_id: &run_id,
        }) {
            Ok(value) => value,
            Err(error) => {
                self.bulk_message = error;
                return;
            }
        };
        let selected_indices = admission.selected_indices;
        let jobs = admission.jobs;
        let project_id = admission.project_id;
        let job_ids = admission.job_ids;
        let prepared = admission.prepared;
        let active_run = admission.active_run;
        let selected_job_ids = prepared.selected_job_ids.clone();
        let queue_checkpoints = prepared.queue_checkpoints.clone();
        let job_count = jobs.len();
        let concurrency = self.form.profile.batch_concurrency.clamp(1, 16);
        self.bulk_project_id = Some(project_id.clone());
        self.selected_project_id = Some(project_id.clone());
        self.bulk_job_ids = job_ids;
        self.rebuild_bulk_job_index();
        self.bulk_live_run = live;
        self.run_id = Some(run_id.clone());
        self.locked_profile = Some(self.form.profile.clone());
        self.active_run = Some(active_run);
        for &index in &selected_indices {
            if let Some(job) = self.bulk_jobs.get_mut(index) {
                job.state = "Queued".into();
            }
        }
        self.mark_bulk_state_changed();
        let worker = launch_batch_worker(BatchExecutionContext {
            concurrency,
            mode,
            retry_count: self.form.profile.batch_retry_count.min(3),
            job_count,
            queue_job_ids: selected_job_ids,
            child_run_ids: self
                .active_run
                .as_ref()
                .map(|run| run.batch_child_run_ids.clone())
                .unwrap_or_default(),
            queue_checkpoints,
            batch_project_id: project_id.clone(),
            batch_run_id: run_id.clone(),
            jobs,
        });
        self.cancel_requested = Some(worker.cancel.clone());
        self.receiver = Some(worker.receiver);
        self.run_started_at = Some(Instant::now());
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
