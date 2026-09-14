//! Batch workspace actions owned by the UI layer.
//!
//! Durable admission and worker policy remain in `controller`; this module
//! only translates an admitted batch into the presentation state needed by
//! the egui shell.

use crate::atomic_artifact::write_private_atomic;
use crate::bulk_import::{BulkImportResult, BulkJob, PendingSheetImport};
use crate::controller::batch_admission::{apply_keyring_id, selection_value};
use crate::controller::{
    BatchExecutionMode, BatchLaunchRequest, BatchStartContext, BatchStartDecision, BulkRetryScope,
    BulkStateSet, admit_batch_launch, launch_batch_worker,
};
use crate::ui::{contains_ascii_case_insensitive, display_state_key};
use crate::{App, StatusSeverity};
use crate::{core, ui::job_state_badge};
use eframe::egui::{self, Color32, RichText};
use std::collections::HashSet;
use std::time::Instant;

impl App {
    pub(crate) fn bulk_dialog(&mut self, ctx: &egui::Context) {
        let colors = self.theme_colors();
        if !self.bulk_open {
            return;
        }
        let mut open = self.bulk_open;
        egui::Window::new("Batch migration queue")
            .open(&mut open)
            .default_width(850.0)
            .default_height(540.0)
            .show(ctx, |ui| {
                ui.heading("Import → review → validate");
                ui.label(RichText::new(&self.bulk_message).color(self.theme_colors().text_secondary));
                ui.add_space(8.0);
                let summary = self.bulk_queue_summary();
                ui.group(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(format!("{} total", summary.total));
                        ui.label(format!("{} ready", summary.ready));
                        ui.label(format!("{} running", summary.running));
                        ui.label(format!("{} verified", summary.verified));
                        if summary.failed > 0 { ui.label(RichText::new(format!("{} failed", summary.failed)).color(colors.danger)); }
                        if summary.attention > 0 { ui.label(RichText::new(format!("{} attention", summary.attention)).color(colors.warning)); }
                        if summary.delta_required > 0 { ui.label(format!("{} delta required", summary.delta_required)); }
                        if summary.verification_difference > 0 { ui.label(RichText::new(format!("{} verification differences", summary.verification_difference)).color(colors.warning)); }
                        if summary.cancelled > 0 { ui.label(RichText::new(format!("{} cancelled", summary.cancelled)).color(colors.warning)); }
                        let unresolved_count = summary.unresolved();
                        ui.label(RichText::new(format!("{} unresolved", unresolved_count)).color(if unresolved_count > 0 { self.theme_colors().danger } else { self.theme_colors().success }));
                        if !self.bulk_selected_ids.is_empty() { ui.label(format!("{} selected", self.bulk_selected_ids.len())); }
                        if ui.button("Select unresolved").clicked() { self.select_bulk_state_set(BulkStateSet::Unresolved); }
                        if ui.button("Select failed").clicked() { self.select_bulk_state_set(BulkStateSet::Failed); }
                        if ui.button("Select attention").clicked() { self.select_bulk_state_set(BulkStateSet::Attention); }
                        if !self.bulk_selected_ids.is_empty() && ui.button("Clear selection").clicked() { self.bulk_selected_ids.clear(); }
                    });
                    ui.label(RichText::new("Focused selections apply to preflight and live scope controls below; live execution still requires matching preflight and confirmation.").size(11.0).color(self.theme_colors().text_secondary));
                });
                ui.add_space(8.0);
                let previous_bulk_mode = self.bulk_mode;
                ui.horizontal(|ui| {
                    ui.label("Batch mode");
                    ui.selectable_value(&mut self.bulk_mode, BatchExecutionMode::Preflight, "Preflight");
                    ui.selectable_value(&mut self.bulk_mode, BatchExecutionMode::Live, "Live migration");
                });
                if self.bulk_mode != previous_bulk_mode {
                    self.bulk_live_confirmed = false;
                    self.bulk_confirmation_summary = None;
                }
                let queue_editable = !self.running();
                ui.horizontal(|ui| {
                    ui.label("Customer/project name");
                    ui.add_enabled(queue_editable, egui::TextEdit::singleline(&mut self.form.profile.name).desired_width(280.0).hint_text("e.g. Acme Corp cutover"));
                });
                ui.label(RichText::new("Used for the durable project and customer evidence when imported rows do not provide project_name.").size(11.0).color(self.theme_colors().text_secondary));
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.running() && self.bulk_import_receiver.is_none(), egui::Button::new("Import CSV / Excel…")).clicked() && let Some(path) = rfd::FileDialog::new().add_filter("Migration lists", &["csv", "xls", "xlsx"]).pick_file() { self.request_bulk_import(path); }
                    if ui.add_enabled(!self.running(), egui::Button::new("Clear queue")).clicked() { if self.bulk_jobs.is_empty() { self.clear_bulk_queue(); } else { self.bulk_clear_confirm_open = true; } }
                    if ui.add_enabled(!self.running() && !self.bulk_jobs.is_empty(), egui::Button::new("Export selected set…")).clicked() { self.bulk_message = match self.export_bulk_selection() { Ok(()) => "Selected batch rows exported without credentials or engine options.".into(), Err(error) => error }; }
                    let live_count = self.bulk_jobs.iter().filter(|job| self.bulk_retry_scope.includes(&display_state_key(&job.state))).count();
                    let label = if self.bulk_mode.is_preflight() { format!("Run {} preflight checks", self.bulk_jobs.len()) } else { format!("Start {live_count} live migrations") };
                    let can_start = !self.running() && !self.bulk_jobs.is_empty() && (self.bulk_mode.is_preflight() || live_count > 0);
                    if ui.add_enabled(can_start, egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(if self.bulk_mode.is_preflight() { self.theme_colors().info } else { self.theme_colors().danger })).clicked() { self.start_bulk(); }
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| { ui.label("Maximum concurrent workers"); ui.add_enabled(queue_editable, egui::Slider::new(&mut self.form.profile.batch_concurrency, 1..=16)); ui.label(RichText::new("Applies to both preflight and live migration; bounded to 1–16 workers").size(11.0).color(self.theme_colors().text_secondary)); });
                ui.horizontal(|ui| { ui.label("Transient retries"); ui.add_enabled(queue_editable, egui::Slider::new(&mut self.form.profile.batch_retry_count, 0..=3)); ui.label(RichText::new("auth/configuration failures are never retried").size(11.0).color(self.theme_colors().text_secondary)); });
                if self.bulk_mode.is_live() {
                    ui.add_enabled_ui(queue_editable, |ui| { egui::ComboBox::from_id_salt("bulk_retry_scope").selected_text(self.bulk_retry_scope.label()).show_ui(ui, |ui| { for scope in [BulkRetryScope::Unresolved, BulkRetryScope::FailedAttention, BulkRetryScope::DeltaRequired, BulkRetryScope::VerificationDifference, BulkRetryScope::All] { ui.selectable_value(&mut self.bulk_retry_scope, scope, scope.label()); } }); });
                    ui.label(RichText::new(format!("Live scope: {}. Verified rows run only with the explicit all-rows scope.", self.bulk_retry_scope.label())).size(11.0).color(self.theme_colors().text_secondary));
                }
                ui.label(RichText::new("Passwordless queue credentials").strong());
                ui.label(RichText::new("Apply an existing OS-keyring reference to rows that do not already have a password or credential ID. The secret itself is never copied into the queue.").size(11.0).color(self.theme_colors().text_secondary));
                let mut apply_source = false;
                let mut apply_destination = false;
                ui.horizontal(|ui| { ui.label("Source keyring ID"); ui.add_enabled(queue_editable, egui::TextEdit::singleline(&mut self.bulk_source_keyring_apply).desired_width(180.0)); if ui.add_enabled(queue_editable, egui::Button::new("Apply to empty source rows")).clicked() { apply_source = true; } });
                ui.horizontal(|ui| { ui.label("Destination keyring ID"); ui.add_enabled(queue_editable, egui::TextEdit::singleline(&mut self.bulk_destination_keyring_apply).desired_width(180.0)); if ui.add_enabled(queue_editable, egui::Button::new("Apply to empty destination rows")).clicked() { apply_destination = true; } });
                if apply_source { self.apply_bulk_keyring_id(true); }
                if apply_destination { self.apply_bulk_keyring_id(false); }
                ui.label(RichText::new("Required columns: source_host, source_user, destination_host, destination_user. Optional: project_name, source_password, destination_password, source_credential_id, destination_credential_id, name. project_name names the durable customer migration; name labels each mailbox row. Engine options remain trusted application settings and cannot be imported from a spreadsheet. Enter missing credentials in the masked fields below.").size(11.0).color(self.theme_colors().text_secondary));
                ui.separator();
                egui::Grid::new("bulk_jobs").striped(true).min_col_width(120.0).show(ui, |ui| {
                    ui.strong("#"); ui.strong("Migration"); ui.strong("Source"); ui.strong("Destination"); ui.strong("Source password"); ui.strong("Destination password"); ui.strong("Status"); ui.end_row();
                    let row_count = self.bulk_jobs.len();
                    egui::ScrollArea::vertical().show_rows(ui, 42.0, row_count, |ui, rows| { for index in rows { let job = &mut self.bulk_jobs[index]; ui.label((index + 1).to_string()); ui.label(&job.label); ui.label(format!("{}\n{}", job.form.profile.source_host, job.form.profile.source_user)); ui.label(format!("{}\n{}", job.form.profile.destination_host, job.form.profile.destination_user)); ui.add_enabled(queue_editable, egui::TextEdit::singleline(job.form.source_password.as_mut_string()).password(true).desired_width(120.0)); if job.form.engine() == core::Engine::Dovecot { ui.label("Not required"); } else { ui.add_enabled(queue_editable, egui::TextEdit::singleline(job.form.destination_password.as_mut_string()).password(true).desired_width(120.0)); } let (badge, color) = job_state_badge(&job.state, colors); ui.label(RichText::new(badge).color(color)); ui.end_row(); } });
                });
                ui.add_space(8.0); ui.label(RichText::new("Imported passwords are used only for this open queue. Saving a profile never saves them.").size(11.0).color(self.theme_colors().danger));
            });
        self.bulk_open = open;
    }

    pub(crate) fn bulk_clear_confirmation(&mut self, ctx: &egui::Context) {
        if !self.bulk_clear_confirm_open || self.running() {
            return;
        }
        let mut open = self.bulk_clear_confirm_open;
        let mut clear = false;
        let mut close_requested = false;
        egui::Window::new("Clear mailbox queue?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Discard the current queue?");
                ui.label(format!(
                    "This removes {} mailbox row(s), selection, in-memory passwords, and the durable batch association from this workspace.",
                    self.bulk_jobs.len()
                ));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Keep queue").clicked() {
                        close_requested = true;
                    }
                    if ui.button("Clear queue").clicked() {
                        clear = true;
                        close_requested = true;
                    }
                });
            });
        self.bulk_clear_confirm_open = open && !close_requested;
        if clear {
            self.clear_bulk_queue();
        }
    }

    pub(crate) fn bulk_import_confirmation(&mut self, ctx: &egui::Context) {
        if self.pending_bulk_import.is_none() || self.running() {
            return;
        }
        let path_label = self
            .pending_bulk_import
            .as_deref()
            .and_then(std::path::Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("the selected file")
            .to_owned();
        let mut open = true;
        let mut close_requested = false;
        let mut replace = false;
        egui::Window::new("Replace mailbox queue?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Replace the current queue?");
                ui.label(format!(
                    "Importing {path_label} will replace {} current mailbox row(s), selection, in-memory passwords, and the durable batch association.",
                    self.bulk_jobs.len()
                ));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Keep current queue").clicked() {
                        close_requested = true;
                    }
                    if ui.button("Replace queue").clicked() {
                        replace = true;
                        close_requested = true;
                    }
                });
            });
        if close_requested || !open {
            let path = self.pending_bulk_import.take();
            if replace && let Some(path) = path {
                self.import_bulk(&path);
            }
        }
    }

    pub(crate) fn bulk_sheet_selection(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_sheet_import.as_ref() else {
            return;
        };
        let path_label = pending
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("the workbook")
            .to_owned();
        let sheets = pending.sheets.clone();
        let mut open = true;
        let mut cancel = false;
        let mut import = false;
        egui::Window::new("Choose worksheet")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Select the migration worksheet");
                ui.label(format!(
                    "{path_label} contains {} worksheet(s). Choose the sheet with the mailbox headers.",
                    sheets.len()
                ));
                egui::ComboBox::from_id_salt("bulk_sheet_selection")
                    .selected_text(
                        sheets
                            .get(self.bulk_sheet_index)
                            .map(String::as_str)
                            .unwrap_or("Select a worksheet"),
                    )
                    .show_ui(ui, |ui| {
                        for (index, name) in sheets.iter().enumerate() {
                            ui.selectable_value(&mut self.bulk_sheet_index, index, name);
                        }
                    });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "The selected worksheet is parsed and validated in the background. Other worksheets are not imported.",
                    )
                    .size(11.0)
                    .color(self.theme_colors().text_secondary),
                );
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.button("Import selected worksheet").clicked() {
                        import = true;
                    }
                });
            });
        if cancel || !open {
            self.pending_sheet_import = None;
            self.bulk_message = "Worksheet selection cancelled; no rows were imported.".into();
        } else if import && let Some(pending) = self.pending_sheet_import.take() {
            self.begin_sheet_import(pending.path, self.bulk_sheet_index);
        }
    }

    pub(crate) fn bulk_live_confirmation(&mut self, ctx: &egui::Context) {
        if !self.bulk_live_confirm_open {
            return;
        }
        if self.bulk_confirmation_summary.is_none() {
            let mut durable_state_error = None;
            let eligible_indices = self
                .bulk_jobs
                .iter()
                .enumerate()
                .filter_map(|(index, _)| {
                    let job_id = self.bulk_job_ids.get(index)?;
                    let state = match self.cached_report_mailbox(job_id) {
                        Some(mailbox) => mailbox.job.state.as_str(),
                        None => {
                            durable_state_error = Some(
                                "Could not read durable mailbox state for confirmation; refresh the workspace and try again."
                                    .into(),
                            );
                            return None;
                        }
                    };
                    (self.bulk_row_is_selected(index) && self.bulk_retry_scope.includes(state))
                        .then_some(index)
                })
                .collect::<HashSet<_>>();
            let deletion_enabled = eligible_indices
                .iter()
                .any(|index| self.bulk_jobs[*index].form.profile.delete2);
            self.bulk_confirmation_summary = Some(crate::controller::BulkConfirmationSummary {
                eligible_count: eligible_indices.len(),
                deletion_enabled,
                durable_state_error,
                concurrency: self.form.profile.batch_concurrency.clamp(1, 16),
                scope: self.bulk_retry_scope,
            });
        }
        let summary = self
            .bulk_confirmation_summary
            .as_ref()
            .cloned()
            .expect("confirmation summary is initialized above");
        let mut open = self.bulk_live_confirm_open;
        let mut close = false;
        egui::Window::new("Confirm live batch migration")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading(RichText::new("This will change destination mailboxes").color(self.theme_colors().danger));
                ui.label(format!("{} mailboxes selected", summary.eligible_count));
                if let Some(error) = &summary.durable_state_error {
                    ui.label(RichText::new(error).color(self.theme_colors().danger));
                }
                ui.label(format!("Worker concurrency: {}", summary.concurrency));
                ui.label(
                    RichText::new(format!(
                        "Destination deletion: {}",
                        if summary.deletion_enabled { "ENABLED ⚠" } else { "disabled" }
                    ))
                    .color(if summary.deletion_enabled { self.theme_colors().danger } else { self.theme_colors().text_secondary }),
                );
                ui.label(format!("Scope: {}.", summary.scope.label()));
                ui.label("Each mailbox must already have a matching successful preflight. Source mail is not deleted by default.");
                ui.label(RichText::new("Review the queue, concurrency, throttles, and exact plans before continuing.").color(self.theme_colors().text_secondary));
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui
                        .add_enabled(
                            summary.durable_state_error.is_none() && summary.eligible_count > 0,
                            egui::Button::new(RichText::new("I understand — start batch").color(Color32::WHITE))
                                .fill(self.theme_colors().danger),
                        )
                        .clicked()
                    {
                        close = true;
                        self.bulk_live_confirmed = true;
                        self.start_bulk();
                    }
                });
            });
        self.bulk_live_confirm_open = open && !close;
        if !self.bulk_live_confirm_open {
            self.bulk_confirmation_summary = None;
        }
    }

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
