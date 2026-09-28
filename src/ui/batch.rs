//! Batch workspace actions owned by the UI layer.
//!
//! Durable admission and worker policy remain in `controller`; this module
//! only translates an admitted batch into the presentation state needed by
//! the egui shell.

use crate::App;
use crate::controller::{
    BatchActionPlan, BatchActionRow, BatchExecutionMode, BulkRetryScope, BulkStateSet,
    build_batch_action_plan,
};
use crate::ui::WorkspaceView;
use crate::ui::batch_filter::selected_visibility_counts;
use crate::ui::job_state_badge;
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};

impl App {
    fn current_batch_action_plan(
        &self,
        execution_mode: BatchExecutionMode,
        retry_scope: BulkRetryScope,
    ) -> BatchActionPlan {
        let mut known_ids = std::collections::HashSet::new();
        let mut rows = self
            .bulk_jobs
            .iter()
            .enumerate()
            .filter_map(|(index, job)| {
                let id = self.bulk_job_ids.get(index)?;
                known_ids.insert(id.clone());
                Some(BatchActionRow {
                    id,
                    selected: self.bulk_selected_ids.contains(id),
                    visible: self.mailbox_matches_filter(job),
                    durable_state: Some(job.state.as_str()),
                    destructive: job.form.profile.delete2,
                })
            })
            .collect::<Vec<_>>();
        for id in self
            .bulk_selected_ids
            .iter()
            .filter(|id| !known_ids.contains(*id))
        {
            rows.push(BatchActionRow {
                id,
                selected: true,
                visible: false,
                durable_state: None,
                destructive: false,
            });
        }
        build_batch_action_plan(
            &rows,
            retry_scope,
            self.form.profile.batch_concurrency,
            execution_mode,
        )
    }

    pub(crate) fn mailbox_view(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        ui.heading(self.language.text("Mailboxes"));
        ui.label(RichText::new(self.language.text("Review, filter, select, and operate on customer mailboxes without reopening the legacy queue window.")).color(self.theme_colors().text_secondary));
        ui.add_space(12.0);
        if self.historical_mailbox_view(ui) {
            return;
        }
        if self.bulk_jobs.is_empty() {
            ui.label(
                RichText::new("0 selected · 0 visible · 0 hidden by current filter")
                    .color(colors.text_secondary),
            );
            ui.group(|ui| {
                ui.heading(self.language.text("No bulk mailbox list loaded"));
                ui.label(
                    self.language
                        .text("A single mailbox can be configured from the migration plan."),
                );
                if ui
                    .button(self.language.text("Open migration plan"))
                    .clicked()
                {
                    self.active_view = WorkspaceView::Plan;
                }
                if ui
                    .button(self.language.text("Import CSV / XLSX…"))
                    .clicked()
                {
                    self.choose_bulk_import();
                }
                ui.label(
                    RichText::new(
                        self.language
                            .text("CSV or XLSX only; legacy .xls files must be converted first."),
                    )
                    .size(11.0)
                    .color(colors.text_secondary),
                );
            });
        } else {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} mailbox jobs in scope", self.bulk_jobs.len()))
                        .strong(),
                );
                if ui
                    .button(self.language.text("Import CSV / XLSX…"))
                    .clicked()
                {
                    self.choose_bulk_import();
                }
            });
            let summary = self.bulk_queue_summary();
            ui.group(|ui| {
                ui.label(RichText::new(self.language.text("QUEUE HEALTH")).strong().size(11.0));
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
                ui.label(RichText::new(self.language.text("Use the state filter and Select visible to act on a focused set; live execution still requires a matching preflight.")).size(11.0).color(colors.text_secondary));
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(self.language.text("Search"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.bulk_search)
                        .hint_text(self.language.text("mailbox, host, or user"))
                        .desired_width(220.0),
                );
                egui::ComboBox::from_id_salt("mailbox_state_filter")
                    .selected_text(self.language.text(match self.bulk_state_filter.as_str() {
                        "imported" => "Imported",
                        "attention" => "Attention",
                        "failed" => "Failed",
                        "delta_required" => "Delta required",
                        "verified" | "verified_with_exceptions" => "Verified",
                        "ready" => "Ready",
                        _ => "All states",
                    }))
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
                            ui.selectable_value(
                                &mut self.bulk_state_filter,
                                value.into(),
                                self.language.text(label),
                            );
                        }
                    });
                if ui.button(self.language.text("Select visible")).clicked() {
                    self.bulk_selected_ids.clear();
                    for (index, job) in self.bulk_jobs.iter().enumerate() {
                        if self.mailbox_matches_filter(job)
                            && let Some(id) = self.bulk_job_ids.get(index)
                        {
                            self.bulk_selected_ids.insert(id.clone());
                        }
                    }
                }
                if ui.button(self.language.text("Select unresolved")).clicked() {
                    self.select_bulk_state_set(BulkStateSet::Unresolved);
                }
                if ui.button(self.language.text("Select attention")).clicked() {
                    self.select_bulk_state_set(BulkStateSet::Attention);
                }
                if ui.button(self.language.text("Clear selection")).clicked() {
                    self.bulk_selected_ids.clear();
                }
            });
            // Rebuild normalized search values and filtered indices only when
            // the queue, query, or state filter changes. The table still
            // virtualizes row widgets without doing a full filter pass on
            // every repaint.
            self.refresh_bulk_filter_cache();
            let visible_indices = std::mem::take(&mut self.bulk_visible_indices);

            let (_, visible_and_selected, hidden_selected) = selected_visibility_counts(
                &self.bulk_selected_ids,
                &visible_indices,
                &self.bulk_job_ids,
            );

            let status_text = format!(
                "{} selected · {} visible · {} hidden by current filter",
                self.bulk_selected_ids.len(),
                visible_and_selected,
                hidden_selected
            );

            ui.label(RichText::new(status_text).color(self.theme_colors().text_secondary));
            if hidden_selected > 0 {
                ui.label(
                    RichText::new(
                        self.language
                            .text("⚠ {} mailbox(es) are selected but hidden by the current filter. They will still be included in batch operations.")
                            .replace("{}", &hidden_selected.to_string()),
                    )
                    .color(self.theme_colors().warning),
                );
            }
            let has_selection = !self.bulk_selected_ids.is_empty();
            let live_plan =
                self.current_batch_action_plan(BatchExecutionMode::Live, BulkRetryScope::All);
            let delta_plan = self
                .current_batch_action_plan(BatchExecutionMode::Live, BulkRetryScope::DeltaRequired);
            let mut run_preflight = false;
            let mut run_live = false;
            let mut run_delta = false;
            let mut review_selected = false;
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(self.language.text("Selected mailbox actions")).strong());
                if ui
                    .add_enabled(
                        has_selection && !self.running(),
                        egui::Button::new(
                            self.language
                                .text("Run preflight ({})")
                                .replace("{}", &live_plan.explicit_selection_count.to_string()),
                        ),
                    )
                    .clicked()
                {
                    run_preflight = true;
                }
                if ui
                    .add_enabled(
                        has_selection && !self.running(),
                        egui::Button::new(
                            RichText::new(
                                self.language
                                    .text("Run live migration ({})")
                                    .replace("{}", &live_plan.eligible_count.to_string()),
                            )
                            .color(Color32::WHITE),
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
                        egui::Button::new(
                            self.language
                                .text("Run final delta ({})")
                                .replace("{}", &delta_plan.eligible_count.to_string()),
                        ),
                    )
                    .clicked()
                {
                    run_delta = true;
                }
                if ui
                    .add_enabled(
                        self.bulk_selected_ids.len() == 1,
                        egui::Button::new(self.language.text("Review verification")),
                    )
                    .clicked()
                {
                    review_selected = true;
                }
                if !has_selection {
                    ui.label(
                        RichText::new(
                            self.language
                                .text("Select one or more rows to enable actions."),
                        )
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
                            ui.strong(self.language.text(label));
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
                            let accessible_name = self
                                .language
                                .text("Select {} → {}")
                                .replace("{}", &job.form.profile.source_user)
                                .replacen("{}", &job.form.profile.destination_user, 1);
                            let response = ui.checkbox(&mut selected, "");
                            response.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::Checkbox,
                                    ui.is_enabled(),
                                    selected,
                                    accessible_name.clone(),
                                )
                            });
                            if response.changed() {
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
                            ui.label(RichText::new(self.language.text(badge)).color(color));
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
                                        RichText::new(
                                            self.language.text("Inspect durable run detail"),
                                        )
                                        .color(self.theme_colors().text_secondary),
                                    );
                                }
                            }
                        });
                    });
                });
            self.bulk_visible_indices = visible_indices;
            ui.label(
                RichText::new(self.language.text("Batch actions apply only to explicitly selected rows. Use Select unresolved or Select visible to create a selection."))
                    .color(self.theme_colors().text_secondary),
            );
        }
    }

    pub(crate) fn selection_review_drawer(&self, ui: &mut egui::Ui) {
        ui.heading(
            self.language
                .text("Review selected ({})")
                .replace("{}", &self.bulk_selected_ids.len().to_string()),
        );
        ui.label(
            RichText::new(
                self.language
                    .text("Selected mailbox scope remains explicit while this drawer is open."),
            )
            .size(11.0)
            .color(self.theme_colors().text_secondary),
        );
        ui.separator();
        if self.bulk_selected_ids.is_empty() {
            ui.label(
                RichText::new(self.language.text("No mailboxes selected."))
                    .color(self.theme_colors().text_secondary),
            );
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, job) in self.bulk_jobs.iter().enumerate() {
                let Some(job_id) = self.bulk_job_ids.get(index) else {
                    continue;
                };
                if !self.bulk_selected_ids.contains(job_id) {
                    continue;
                }
                let profile = &job.form.profile;
                let destructive = if profile.delete2 {
                    self.language
                        .text("DESTRUCTIVE: destination deletion enabled")
                } else {
                    self.language.text("destination deletion disabled")
                };
                ui.group(|ui| {
                    ui.label(RichText::new(&job.label).strong());
                    ui.label(format!(
                        "{} → {}",
                        profile.source_user, profile.destination_user
                    ));
                    ui.label(format!(
                        "{} → {}",
                        profile.source_host, profile.destination_host
                    ));
                    ui.label(
                        self.language
                            .text("State: {} · {}")
                            .replace(
                                "{}",
                                self.language.text(crate::ui::display_job_state(&job.state)),
                            )
                            .replacen("{}", destructive, 1),
                    );
                });
            }
        });
    }
}
