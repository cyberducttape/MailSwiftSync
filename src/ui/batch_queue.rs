//! Batch queue lifecycle, selection, import, and summary state.

use crate::App;
use crate::bulk_import::{BulkImportResult, PendingSheetImport};
use crate::controller::{BulkRetryScope, BulkStateSet};
use crate::ui::display_state_key;

impl App {
    pub(crate) fn choose_bulk_import(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV or XLSX", &["csv", "xlsx"])
            .pick_file()
        {
            self.request_bulk_import(path);
        }
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
                self.bulk_retry_scope = BulkRetryScope::default();
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
        self.bulk_retry_scope = BulkRetryScope::default();
        self.bulk_preflight_credential_fingerprints.clear();
        self.bulk_message = "Queue cleared; its durable batch association was discarded.".into();
    }

    pub(crate) fn mark_bulk_jobs_changed(&mut self) {
        self.bulk_jobs_generation = self.bulk_jobs_generation.wrapping_add(1);
        self.bulk_summary = None;
        self.bulk_search_values.clear();
        self.bulk_state_indices.clear();
        self.bulk_search_match_indices.clear();
        self.bulk_search_matches_valid = false;
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

    pub(crate) fn set_bulk_job_state(&mut self, index: usize, state: String) {
        if let Some(job) = self.bulk_jobs.get(index) {
            let old_key = crate::ui::display_state_key(&job.state);
            if let Some(indices) = self.bulk_state_indices.get_mut(&old_key) {
                indices.remove(&index);
            }
        }
        let new_key = crate::ui::display_state_key(&state);
        self.bulk_state_indices
            .entry(new_key)
            .or_default()
            .insert(index);
        if let Some(job) = self.bulk_jobs.get_mut(index) {
            job.state = state;
        }
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
}
