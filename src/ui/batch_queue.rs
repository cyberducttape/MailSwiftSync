//! Batch queue lifecycle, selection, import, and summary state.

use crate::App;
use crate::bulk_import::{BulkImportResult, PendingSheetImport};
use crate::controller::{BulkRetryScope, BulkStateSet};
use crate::ui::display_state_key;

pub(crate) fn mailbox_import_available(running: bool, import_in_progress: bool) -> bool {
    !running && !import_in_progress
}

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
        self.bulk_all_selected = false;
        let mut selected = std::collections::HashSet::new();
        if let Some(project_id) = self.queue.project_id().map(str::to_owned) {
            let queue = &self.queue;
            let result = self.store.queue_scan(&project_id, |row| {
                let presented = queue.presented_state_of(&row.id, &row.state);
                if set.matches(&display_state_key(presented)) {
                    selected.insert(row.id);
                }
            });
            if let Err(error) = result {
                self.bulk_message = format!("Could not read the mailbox queue: {error}");
                return;
            }
        }
        self.bulk_selected_ids = selected;
        self.bulk_selection_view_dirty = true;
        self.bulk_state_filter = "all".into();
        self.bulk_message = self
            .language
            .text("Selected {} mailbox row(s) for focused review.")
            .replace("{}", &self.bulk_selection_count().to_string());
    }

    pub(crate) fn apply_bulk_import_result(&mut self, result: Result<BulkImportResult, String>) {
        let imported = match result {
            Ok(BulkImportResult::Persisted(imported)) => Ok(imported),
            // No durable ledger file: the session's own store holds the queue.
            Ok(BulkImportResult::Jobs(jobs)) => crate::controller::queue::persist_imported_queue(
                &self.store,
                jobs,
                &self.form.profile,
            ),
            Ok(BulkImportResult::Workbook {
                path,
                sheets,
                plaintext_acknowledged,
            }) => {
                self.bulk_sheet_index = 0;
                self.pending_sheet_import = Some(PendingSheetImport {
                    path,
                    sheets,
                    plaintext_acknowledged,
                });
                self.bulk_message = self
                    .language
                    .text("Choose the worksheet containing the migration rows before importing.")
                    .into();
                return;
            }
            Err(error) => Err(error),
        };
        match imported {
            Ok(imported) => {
                self.bulk_message = self
                    .language
                    .text(
                        "Imported {} mailbox rows. Review them and run preflight before migration.",
                    )
                    .replace("{}", &imported.len.to_string());
                self.bulk_retry_scope = BulkRetryScope::default();
                self.clear_bulk_selection();
                self.queue.attach(imported.project_id.clone(), imported.len);
                self.queue.session_secrets = imported.session_secrets;
                self.selected_project_id = Some(imported.project_id);
                self.mark_bulk_jobs_changed();
                self.refresh_ui_snapshot_now();
            }
            Err(error) => self.bulk_message = error,
        }
    }

    pub(crate) fn begin_bulk_import(&mut self, path: std::path::PathBuf) {
        if self.bulk_import_receiver.is_some() {
            self.bulk_message = self
                .language
                .text("A mailbox file is already being imported.")
                .into();
            return;
        }
        self.bulk_message = self
            .language
            .text("Importing {} in the background…")
            .replace(
                "{}",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("mailbox file"),
            );
        let parsed = crate::bulk_import::spawn_import(
            path,
            self.form.clone_without_credentials(),
            std::mem::take(&mut self.bulk_plaintext_import_acknowledged),
        );
        self.bulk_import_receiver = Some(self.persist_import_off_thread(parsed));
    }

    pub(crate) fn begin_sheet_import(
        &mut self,
        path: std::path::PathBuf,
        sheet_index: usize,
        plaintext_acknowledged: bool,
    ) {
        if self.bulk_import_receiver.is_some() {
            self.bulk_message = self
                .language
                .text("A mailbox file is already being imported.")
                .into();
            return;
        }
        self.bulk_message = self
            .language
            .text("Importing the selected worksheet in the background…")
            .into();
        let parsed = crate::bulk_import::spawn_sheet_import(
            path,
            self.form.clone_without_credentials(),
            sheet_index,
            plaintext_acknowledged,
        );
        self.bulk_import_receiver = Some(self.persist_import_off_thread(parsed));
    }

    pub(crate) fn import_bulk(&mut self, path: &std::path::Path) {
        self.begin_bulk_import(path.to_owned());
    }

    pub(crate) fn request_bulk_import(&mut self, path: std::path::PathBuf) {
        if crate::bulk_import::plaintext_secret_import_enabled() {
            self.plaintext_import_dialog_acknowledged = false;
            self.pending_plaintext_import = Some(path);
            return;
        }
        self.request_bulk_import_after_ack(path);
    }

    /// Write parsed rows to the ledger on a worker with its own connection,
    /// so a 100,000-row import never blocks a frame. Without a ledger file
    /// the parsed rows are returned for the session store instead.
    fn persist_import_off_thread(
        &self,
        parsed: std::sync::mpsc::Receiver<Result<BulkImportResult, String>>,
    ) -> std::sync::mpsc::Receiver<Result<BulkImportResult, String>> {
        let Some(state_path) = self
            .state_path
            .clone()
            .filter(|_| self.persistence_available)
        else {
            return parsed;
        };
        let fallback_profile = self.form.profile.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = match parsed.recv() {
                Ok(Ok(BulkImportResult::Jobs(jobs))) => crate::core::StateStore::open(&state_path)
                    .map_err(|error| {
                        format!("Could not open the ledger to record the import: {error}")
                    })
                    .and_then(|store| {
                        crate::controller::queue::persist_imported_queue(
                            &store,
                            jobs,
                            &fallback_profile,
                        )
                    })
                    .map(BulkImportResult::Persisted),
                Ok(other) => other,
                Err(_) => Err("The mailbox import worker stopped unexpectedly.".into()),
            };
            let _ = sender.send(result);
        });
        receiver
    }

    pub(crate) fn request_bulk_import_after_ack(&mut self, path: std::path::PathBuf) {
        if self.queue.is_empty() {
            self.import_bulk(&path);
        } else {
            self.pending_bulk_import = Some(path);
        }
    }

    pub(crate) fn clear_bulk_queue(&mut self) {
        if self.selected_project_id.as_deref() == self.queue.project_id() {
            self.selected_project_id = None;
        }
        self.queue.detach();
        self.mark_bulk_jobs_changed();
        self.clear_bulk_selection();
        self.bulk_retry_scope = BulkRetryScope::default();
        self.bulk_message = self
            .language
            .text("Queue cleared; its durable batch association was discarded.")
            .into();
    }

    pub(crate) fn mark_bulk_jobs_changed(&mut self) {
        self.bulk_selection_view_dirty = true;
        self.queue.invalidate();
        self.recovery.invalidate();
    }

    pub(crate) fn mark_bulk_state_changed(&mut self) {
        self.bulk_selection_view_dirty = true;
        self.recovery.invalidate();
    }

    pub(crate) fn bulk_queue_summary(&mut self) -> crate::controller::BulkQueueSummary {
        self.queue.summary(&self.store)
    }
}

#[cfg(test)]
mod tests {
    use super::mailbox_import_available;

    #[test]
    fn mailbox_import_is_disabled_while_running_or_an_import_is_active() {
        assert!(mailbox_import_available(false, false));
        assert!(!mailbox_import_available(true, false));
        assert!(!mailbox_import_available(false, true));
        assert!(!mailbox_import_available(true, true));
    }
}
