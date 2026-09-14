//! Batch workspace actions owned by the UI layer.
//!
//! Durable admission and worker policy remain in `controller`; this module
//! only translates an admitted batch into the presentation state needed by
//! the egui shell.

use crate::atomic_artifact::write_private_atomic;
use crate::bulk_import::{BulkImportResult, BulkJob, PendingSheetImport};
use crate::controller::batch_admission::{apply_keyring_id, selection_value};
use crate::controller::{
    BatchLaunchRequest, BatchStartContext, BatchStartDecision, BulkStateSet, admit_batch_launch,
    launch_batch_worker,
};
use crate::ui::{contains_ascii_case_insensitive, display_state_key};
use crate::{App, StatusSeverity};
use std::time::Instant;

impl App {
    pub(crate) fn mailbox_matches_filter(&self, job: &BulkJob) -> bool {
        let state = job.state.to_ascii_lowercase().replace(' ', "_");
        if !self.bulk_state_filter.is_empty()
            && self.bulk_state_filter != "all"
            && state != self.bulk_state_filter
            && !(self.bulk_state_filter == "delta_required" && state.contains("delta"))
            && !(self.bulk_state_filter == "verification_difference"
                && state.contains("verification"))
        {
            return false;
        }
        let search = self.bulk_search.trim();
        search.is_empty()
            || [
                job.label.as_str(),
                job.form.profile.source_host.as_str(),
                job.form.profile.source_user.as_str(),
                job.form.profile.destination_host.as_str(),
                job.form.profile.destination_user.as_str(),
            ]
            .iter()
            .any(|value| contains_ascii_case_insensitive(value, search))
    }

    pub(crate) fn rebuild_bulk_search_values(&mut self) {
        self.bulk_search_values = self
            .bulk_jobs
            .iter()
            .map(|job| {
                [
                    job.label.as_str(),
                    job.form.profile.source_host.as_str(),
                    job.form.profile.source_user.as_str(),
                    job.form.profile.destination_host.as_str(),
                    job.form.profile.destination_user.as_str(),
                ]
                .join(" ")
                .to_ascii_lowercase()
            })
            .collect();
    }

    pub(crate) fn refresh_bulk_filter_cache(&mut self) {
        let raw_search = self.bulk_search.trim().to_owned();
        let cache_is_current = self.bulk_filter_cache_search == raw_search
            && self.bulk_filter_cache_state == self.bulk_state_filter
            && self.bulk_filter_cache_generation == self.bulk_jobs_generation
            && self.bulk_search_values.len() == self.bulk_jobs.len();
        if cache_is_current {
            return;
        }
        if self.bulk_search_values.len() != self.bulk_jobs.len() {
            self.rebuild_bulk_search_values();
        }
        let normalized_search = raw_search.to_ascii_lowercase();
        self.bulk_visible_indices.clear();
        for (index, job) in self.bulk_jobs.iter().enumerate() {
            let state = display_state_key(&job.state);
            let state_matches = self.bulk_state_filter.is_empty()
                || self.bulk_state_filter == "all"
                || state == self.bulk_state_filter
                || (self.bulk_state_filter == "delta_required" && state.contains("delta"))
                || (self.bulk_state_filter == "verification_difference"
                    && state.contains("verification"));
            let search_matches = normalized_search.is_empty()
                || self
                    .bulk_search_values
                    .get(index)
                    .is_some_and(|value| value.contains(&normalized_search));
            if state_matches && search_matches {
                self.bulk_visible_indices.push(index);
            }
        }
        self.bulk_filter_cache_search = raw_search;
        self.bulk_filter_cache_state = self.bulk_state_filter.clone();
        self.bulk_filter_cache_generation = self.bulk_jobs_generation;
    }

    pub(crate) fn bulk_row_is_selected(&self, index: usize) -> bool {
        self.bulk_selected_ids.is_empty()
            || self
                .bulk_job_ids
                .get(index)
                .is_some_and(|job_id| self.bulk_selected_ids.contains(job_id))
    }

    pub(crate) fn select_bulk_state_set(&mut self, set: BulkStateSet) {
        self.bulk_selected_ids = self
            .bulk_jobs
            .iter()
            .enumerate()
            .filter(|(_, job)| set.matches(&display_state_key(&job.state)))
            .filter_map(|(index, _)| self.bulk_job_ids.get(index).cloned())
            .collect();
        self.bulk_state_filter = "all".into();
        self.bulk_message = format!(
            "Selected {} mailbox row(s) for focused review.",
            self.bulk_selected_ids.len()
        );
    }

    pub(crate) fn export_bulk_selection(&self) -> Result<(), String> {
        if self.bulk_jobs.is_empty() {
            return Err("The batch queue has no mailbox rows to export.".into());
        }
        let value = selection_value(&self.bulk_jobs, &self.bulk_selected_ids, &self.bulk_job_ids);
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-batch-selection.json")
            .save_file()
            .ok_or("Batch selection export cancelled.")?;
        let report = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
        write_private_atomic(&path, &report).map_err(|error| error.to_string())
    }

    pub(crate) fn apply_bulk_import_result(&mut self, result: Result<BulkImportResult, String>) {
        match result {
            Ok(BulkImportResult::Jobs(jobs)) => {
                self.bulk_message = format!(
                    "Imported {} mailbox rows. Review them and run preflight before migration.",
                    jobs.len()
                );
                if self.selected_project_id == self.bulk_project_id {
                    self.selected_project_id = None;
                }
                self.bulk_project_id = None;
                self.bulk_job_ids.clear();
                self.bulk_job_index_by_id.clear();
                self.bulk_retry_scope = crate::controller::BulkRetryScope::default();
                self.bulk_selected_ids.clear();
                self.bulk_preflight_credential_fingerprints = vec![None; jobs.len()];
                self.bulk_jobs = jobs;
                self.mark_bulk_jobs_changed();
            }
            Ok(BulkImportResult::Workbook { path, sheets }) => {
                self.bulk_sheet_index = 0;
                self.pending_sheet_import = Some(PendingSheetImport { path, sheets });
                self.bulk_message =
                    "Choose the worksheet containing the migration rows before importing.".into();
            }
            Err(error) => self.bulk_message = error,
        }
    }

    pub(crate) fn begin_bulk_import(&mut self, path: std::path::PathBuf) {
        if self.bulk_import_receiver.is_some() {
            self.bulk_message = "A mailbox file is already being imported.".into();
            return;
        }
        self.bulk_message = format!(
            "Importing {} in the background…",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("mailbox file")
        );
        self.bulk_import_receiver = Some(crate::bulk_import::spawn_import(
            path,
            self.form.clone_without_credentials(),
        ));
    }

    pub(crate) fn begin_sheet_import(&mut self, path: std::path::PathBuf, sheet_index: usize) {
        if self.bulk_import_receiver.is_some() {
            self.bulk_message = "A mailbox file is already being imported.".into();
            return;
        }
        self.bulk_message = "Importing the selected worksheet in the background…".into();
        self.bulk_import_receiver = Some(crate::bulk_import::spawn_sheet_import(
            path,
            self.form.clone_without_credentials(),
            sheet_index,
        ));
    }

    pub(crate) fn import_bulk(&mut self, path: &std::path::Path) {
        self.begin_bulk_import(path.to_owned());
    }

    pub(crate) fn request_bulk_import(&mut self, path: std::path::PathBuf) {
        if self.bulk_jobs.is_empty() {
            self.import_bulk(&path);
        } else {
            self.pending_bulk_import = Some(path);
        }
    }

    pub(crate) fn clear_bulk_queue(&mut self) {
        self.bulk_jobs.clear();
        self.mark_bulk_jobs_changed();
        self.bulk_selected_ids.clear();
        if self.selected_project_id == self.bulk_project_id {
            self.selected_project_id = None;
        }
        self.bulk_project_id = None;
        self.bulk_job_ids.clear();
        self.bulk_job_index_by_id.clear();
        self.bulk_retry_scope = crate::controller::BulkRetryScope::default();
        self.bulk_preflight_credential_fingerprints.clear();
        self.bulk_message = "Queue cleared; its durable batch association was discarded.".into();
    }

    pub(crate) fn mark_bulk_jobs_changed(&mut self) {
        self.bulk_jobs_generation = self.bulk_jobs_generation.wrapping_add(1);
        self.bulk_summary = None;
        self.bulk_search_values.clear();
        self.bulk_filter_cache_generation = u64::MAX;
    }

    pub(crate) fn rebuild_bulk_job_index(&mut self) {
        self.bulk_job_index_by_id = self
            .bulk_job_ids
            .iter()
            .enumerate()
            .map(|(index, job_id)| (job_id.clone(), index))
            .collect();
    }

    pub(crate) fn mark_bulk_state_changed(&mut self) {
        self.bulk_jobs_generation = self.bulk_jobs_generation.wrapping_add(1);
        self.bulk_summary = None;
        self.bulk_filter_cache_generation = u64::MAX;
    }

    pub(crate) fn bulk_queue_summary(&mut self) -> crate::controller::BulkQueueSummary {
        if let Some((generation, summary)) = self.bulk_summary
            && generation == self.bulk_jobs_generation
        {
            return summary;
        }
        let summary = crate::controller::BulkQueueSummary::from_jobs(&self.bulk_jobs);
        self.bulk_summary = Some((self.bulk_jobs_generation, summary));
        summary
    }

    pub(crate) fn apply_bulk_keyring_id(&mut self, source: bool) {
        let value = if source {
            self.bulk_source_keyring_apply.trim().to_owned()
        } else {
            self.bulk_destination_keyring_apply.trim().to_owned()
        };
        if value.is_empty() {
            self.bulk_message = format!(
                "Enter a {} keyring ID before applying it.",
                if source { "source" } else { "destination" }
            );
            return;
        }
        let applied = apply_keyring_id(&mut self.bulk_jobs, &value, source);
        self.bulk_message = format!(
            "Applied the {} keyring ID to {applied} row(s) without a credential reference.",
            if source { "source" } else { "destination" }
        );
    }

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
        let worker = launch_batch_worker(
            concurrency,
            mode,
            self.form.profile.batch_retry_count.min(3),
            job_count,
            selected_job_ids,
            self.active_run
                .as_ref()
                .map(|run| run.batch_child_run_ids.clone())
                .unwrap_or_default(),
            queue_checkpoints,
            project_id.clone(),
            run_id.clone(),
            jobs,
        );
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
