//! Batch workspace actions owned by the UI layer.
//!
//! Durable admission and worker policy remain in `controller`; this module
//! only translates an admitted batch into the presentation state needed by
//! the egui shell.

use crate::App;
use crate::controller::{
    BatchExecutionMode, BulkRetryScope, BulkStateSet, suggested_batch_project_name,
};
use crate::ui::{WorkspaceView, display_state_key};
use crate::{core, ui::job_state_badge};
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};

impl App {
    pub(crate) fn mailbox_view(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        ui.heading("Mailboxes");
        ui.label(RichText::new("Review, filter, select, and operate on customer mailboxes without reopening the legacy queue window.").color(self.theme_colors().text_secondary));
        ui.add_space(12.0);
        if self.historical_mailbox_view(ui) {
            return;
        }
        if self.bulk_jobs.is_empty() {
            ui.group(|ui| {
                ui.heading("No bulk mailbox list loaded");
                ui.label("A single mailbox can be configured from the migration plan.");
                if ui.button("Open migration plan").clicked() {
                    self.active_view = WorkspaceView::Plan;
                }
                if ui.button("Import CSV / Excel…").clicked() {
                    self.bulk_open = true;
                }
            });
        } else {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} mailbox jobs in scope", self.bulk_jobs.len()))
                        .strong(),
                );
                if ui.button("Import / edit queue").clicked() {
                    self.bulk_open = true;
                }
            });
            let summary = self.bulk_queue_summary();
            ui.group(|ui| {
                ui.label(RichText::new("QUEUE HEALTH").strong().size(11.0));
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{} imported", summary.imported));
                    ui.label(format!("{} queued", summary.queued));
                    ui.label(format!("{} preflight", summary.preflight));
                    ui.label(format!("{} ready", summary.ready));
                    ui.label(format!("{} running", summary.running));
                    ui.label(format!("{} verified", summary.verified));
                    if summary.attention > 0 {
                        ui.label(RichText::new(format!("{} attention", summary.attention)).color(colors.warning));
                    }
                    if summary.failed > 0 {
                        ui.label(RichText::new(format!("{} failed", summary.failed)).color(colors.danger));
                    }
                    if summary.delta_required > 0 {
                        ui.label(format!("{} delta required", summary.delta_required));
                    }
                    let unresolved = summary.unresolved();
                    ui.label(RichText::new(format!("{} unresolved", unresolved)).color(
                        if unresolved == 0 { colors.success } else { colors.danger },
                    ));
                });
                ui.label(RichText::new("Use the state filter and Select visible to act on a focused set; live execution still requires a matching preflight.").size(11.0).color(colors.text_secondary));
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Search");
                ui.add(
                    egui::TextEdit::singleline(&mut self.bulk_search)
                        .hint_text("mailbox, host, or user")
                        .desired_width(220.0),
                );
                egui::ComboBox::from_id_salt("mailbox_state_filter")
                    .selected_text(match self.bulk_state_filter.as_str() {
                        "imported" => "Imported",
                        "attention" => "Attention",
                        "failed" => "Failed",
                        "delta_required" => "Delta required",
                        "verified" | "verified_with_exceptions" => "Verified",
                        "ready" => "Ready",
                        _ => "All states",
                    })
                    .show_ui(ui, |ui| {
                        for (value, label) in [
                            ("all", "All states"),
                            ("imported", "Imported"),
                            ("ready", "Ready"),
                            ("attention", "Attention"),
                            ("failed", "Failed"),
                            ("delta_required", "Delta required"),
                            ("verified", "Verified"),
                            ("verified_with_exceptions", "Verified with exceptions"),
                        ] {
                            ui.selectable_value(&mut self.bulk_state_filter, value.into(), label);
                        }
                    });
                if ui.button("Select visible").clicked() {
                    for (index, job) in self.bulk_jobs.iter().enumerate() {
                        if self.mailbox_matches_filter(job)
                            && let Some(id) = self.bulk_job_ids.get(index)
                        {
                            self.bulk_selected_ids.insert(id.clone());
                        }
                    }
                }
                if ui.button("Select unresolved").clicked() {
                    self.select_bulk_state_set(BulkStateSet::Unresolved);
                }
                if ui.button("Select attention").clicked() {
                    self.select_bulk_state_set(BulkStateSet::Attention);
                }
                if ui.button("Clear selection").clicked() {
                    self.bulk_selected_ids.clear();
                }
            });
            // Rebuild normalized search values and filtered indices only when
            // the queue, query, or state filter changes. The table still
            // virtualizes row widgets without doing a full filter pass on
            // every repaint.
            self.refresh_bulk_filter_cache();
            let visible_indices = std::mem::take(&mut self.bulk_visible_indices);
            ui.label(
                RichText::new(format!(
                    "{} visible · {} selected",
                    visible_indices.len(),
                    self.bulk_selected_ids.len()
                ))
                .color(self.theme_colors().text_secondary),
            );
            let has_selection = !self.bulk_selected_ids.is_empty();
            let mut run_preflight = false;
            let mut run_live = false;
            let mut run_delta = false;
            let mut review_selected = false;
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Selected mailbox actions").strong());
                if ui
                    .add_enabled(
                        has_selection && !self.running(),
                        egui::Button::new("Run preflight"),
                    )
                    .clicked()
                {
                    run_preflight = true;
                }
                if ui
                    .add_enabled(
                        has_selection && !self.running(),
                        egui::Button::new(
                            RichText::new("Run live migration").color(Color32::WHITE),
                        )
                        .fill(self.theme_colors().danger),
                    )
                    .clicked()
                {
                    run_live = true;
                }
                if ui
                    .add_enabled(
                        has_selection && !self.running(),
                        egui::Button::new("Run final delta"),
                    )
                    .clicked()
                {
                    run_delta = true;
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new("Review verification"))
                    .clicked()
                {
                    review_selected = true;
                }
                if !has_selection {
                    ui.label(
                        RichText::new("Select one or more rows to enable actions.")
                            .color(self.theme_colors().text_secondary),
                    );
                }
            });
            if run_preflight {
                self.bulk_mode = BatchExecutionMode::Preflight;
                self.bulk_retry_scope = BulkRetryScope::All;
                self.start_bulk();
            } else if run_live {
                self.bulk_mode = BatchExecutionMode::Live;
                self.bulk_retry_scope = BulkRetryScope::All;
                self.start_bulk();
            } else if run_delta {
                self.bulk_mode = BatchExecutionMode::Live;
                self.bulk_retry_scope = BulkRetryScope::DeltaRequired;
                self.start_bulk();
            } else if review_selected
                && let Some(job_id) = self
                    .bulk_job_ids
                    .iter()
                    .find(|id| self.bulk_selected_ids.contains(*id))
            {
                self.job_id = Some(job_id.clone());
                self.active_view = WorkspaceView::Verification;
            }
            ui.add_space(8.0);
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::auto())
                .column(Column::remainder())
                .column(Column::remainder())
                .column(Column::remainder())
                .column(Column::auto())
                .column(Column::remainder())
                .header(32.0, |mut header| {
                    for label in [
                        "",
                        "Mailbox",
                        "Source",
                        "Destination",
                        "State",
                        "Operator action",
                    ] {
                        header.col(|ui| {
                            ui.strong(label);
                        });
                    }
                })
                .body(|body| {
                    body.rows(42.0, visible_indices.len(), |mut row| {
                        let index = visible_indices[row.index()];
                        let job = &self.bulk_jobs[index];
                        let Some(job_id) = self.bulk_job_ids.get(index) else {
                            return;
                        };
                        row.col(|ui| {
                            let mut selected = self.bulk_selected_ids.contains(job_id);
                            if ui.checkbox(&mut selected, "").changed() {
                                if selected {
                                    self.bulk_selected_ids.insert(job_id.clone());
                                } else {
                                    self.bulk_selected_ids.remove(job_id);
                                }
                            }
                        });
                        row.col(|ui| {
                            ui.label(&job.label);
                        });
                        row.col(|ui| {
                            ui.label(format!(
                                "{}\n{}",
                                job.form.profile.source_host, job.form.profile.source_user
                            ));
                        });
                        row.col(|ui| {
                            ui.label(format!(
                                "{}\n{}",
                                job.form.profile.destination_host,
                                job.form.profile.destination_user
                            ));
                        });
                        row.col(|ui| {
                            let (badge, color) = job_state_badge(&job.state, colors);
                            ui.label(RichText::new(badge).color(color));
                        });
                        row.col(|ui| {
                            if job.state == "attention" {
                                if let Some(reason) = self
                                    .cached_report_mailbox(job_id)
                                    .and_then(|mailbox| mailbox.attention_reason.as_ref())
                                {
                                    ui.label(
                                        RichText::new(reason.recommended_action())
                                            .color(self.theme_colors().text_secondary),
                                    );
                                } else {
                                    ui.label(
                                        RichText::new("Inspect durable run detail")
                                            .color(self.theme_colors().text_secondary),
                                    );
                                }
                            }
                        });
                    });
                });
            self.bulk_visible_indices = visible_indices;
            ui.label(RichText::new("When a selection is present, batch actions apply only to selected rows. With no selection, the chosen retry scope applies to all matching rows.").color(self.theme_colors().text_secondary));
        }
    }
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
                        ui.label(format!("{} imported", summary.imported));
                        ui.label(format!("{} queued", summary.queued));
                        ui.label(format!("{} preflight", summary.preflight));
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
                if matches!(self.form.profile.name.trim(), "" | "New migration" | "Batch migration" | "Batch validation") {
                    let suggested_name = suggested_batch_project_name(&self.form.profile);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Suggested durable name: {suggested_name}")).color(self.theme_colors().text_secondary));
                        if ui.add_enabled(queue_editable, egui::Button::new("Use suggestion")).clicked() {
                            self.form.profile.name = suggested_name;
                        }
                    });
                }
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
                ui.label(RichText::new("Required columns: source_host, source_user, destination_host, destination_user. Optional: project_name, source_credential_id, destination_credential_id, name. Password columns are rejected by default; use keyring IDs or enter missing credentials in the masked fields below. Plaintext password imports require MAILSWIFTSYNC_ALLOW_PLAINTEXT_SECRETS=1. project_name names the durable customer migration; name labels each mailbox row. Engine options remain trusted application settings and cannot be imported from a spreadsheet.").size(11.0).color(self.theme_colors().text_secondary));
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
}
