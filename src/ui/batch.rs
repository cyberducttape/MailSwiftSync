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
    pub(crate) fn current_batch_action_plan(
        &self,
        execution_mode: BatchExecutionMode,
        retry_scope: BulkRetryScope,
    ) -> BatchActionPlan {
        // The plan only describes selected rows, so project the selection
        // instead of the whole queue. Selected IDs missing from the queue
        // stay in the plan with no durable state so they cannot disappear
        // from a safety confirmation.
        let rows = self
            .bulk_selected_ids
            .iter()
            .map(|id| match self.bulk_job_index(id) {
                Some(index) => BatchActionRow {
                    id,
                    selected: true,
                    visible: self.bulk_visible_indices.binary_search(&index).is_ok(),
                    durable_state: Some(self.bulk_jobs[index].state.as_str()),
                    destructive: self.bulk_jobs[index].defaults.profile.delete2,
                },
                None => BatchActionRow {
                    id,
                    selected: true,
                    visible: false,
                    durable_state: None,
                    destructive: false,
                },
            })
            .collect::<Vec<_>>();
        build_batch_action_plan(
            &rows,
            retry_scope,
            self.form.profile.batch_concurrency,
            execution_mode,
        )
    }

    /// Queue position of a job ID, if it belongs to the current queue.
    pub(crate) fn bulk_job_index(&self, id: &str) -> Option<usize> {
        self.bulk_job_index_by_id
            .get(id)
            .copied()
            .filter(|&index| index < self.bulk_jobs.len())
    }

    /// Selected rows a live action with this scope would run. Matches
    /// `build_batch_action_plan`'s eligible count without hashing the
    /// selection on every frame.
    fn selected_live_eligible_count(&self, retry_scope: BulkRetryScope) -> usize {
        self.bulk_selected_ids
            .iter()
            .filter_map(|id| self.bulk_job_index(id))
            .filter(|&index| retry_scope.includes(&self.bulk_jobs[index].state))
            .count()
    }

    pub(crate) fn mailbox_view(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        crate::ui::page_header(
            ui,
            self.language.text("Mailboxes"),
            self.language
                .text("Review, filter, select, and operate on customer mailboxes."),
        );
        if self.historical_mailbox_view(ui) {
            return;
        }
        // Import results, start blocks, and queue-tool outcomes.
        if !self.bulk_message.is_empty() {
            crate::ui::card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&self.bulk_message).color(colors.text_primary));
                    if ui.small_button(self.language.text("Dismiss")).clicked() {
                        self.bulk_message.clear();
                    }
                });
            });
            ui.add_space(8.0);
        }
        if self.bulk_jobs.is_empty() {
            ui.label(
                RichText::new(
                    self.language
                        .text("0 selected · 0 visible · 0 hidden by current filter"),
                )
                .color(colors.text_secondary),
            );
            crate::ui::card(ui, |ui| {
                ui.label(
                    RichText::new(self.language.text("No bulk mailbox list loaded"))
                        .size(17.0)
                        .strong(),
                );
                ui.label(
                    self.language
                        .text("A single mailbox can be configured from the migration plan."),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if crate::ui::primary_button(ui, self.language.text("Import CSV / XLSX…"))
                        .clicked()
                    {
                        self.choose_bulk_import();
                    }
                    if ui
                        .button(self.language.text("Open migration plan"))
                        .clicked()
                    {
                        self.active_view = WorkspaceView::Plan;
                    }
                });
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
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(
                        self.language
                            .text("{} mailbox jobs in scope")
                            .replace("{}", &self.bulk_jobs.len().to_string()),
                    )
                    .strong(),
                );
                if ui
                    .add_enabled(
                        !self.running(),
                        egui::Button::new(self.language.text("Import CSV / XLSX…")),
                    )
                    .clicked()
                {
                    self.choose_bulk_import();
                }
                if ui
                    .add_enabled(
                        !self.bulk_selected_ids.is_empty(),
                        egui::Button::new(self.language.text("Export selected set…")),
                    )
                    .on_hover_text(self.language.text(
                        "Writes the selected rows as JSON without credentials or engine options.",
                    ))
                    .clicked()
                {
                    self.bulk_message = match self.export_bulk_selection() {
                        Ok(()) => self
                            .language
                            .text("Selected batch rows exported without credentials or engine options.")
                            .to_owned(),
                        Err(error) => error,
                    };
                }
                if ui
                    .add_enabled(
                        !self.running(),
                        egui::Button::new(self.language.text("Clear queue")),
                    )
                    .clicked()
                {
                    self.bulk_clear_confirm_open = true;
                }
            });
            let summary = self.bulk_queue_summary();
            crate::ui::card(ui, |ui| {
                crate::ui::section_label(ui, self.language.text("QUEUE HEALTH"));
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    for (count, label, color) in [
                        (summary.imported, "{} imported", colors.text_secondary),
                        (summary.queued, "{} queued", colors.text_secondary),
                        (summary.preflight, "{} preflight", colors.info),
                        (summary.ready, "{} ready", colors.info),
                        (summary.running, "{} running", colors.info),
                        (summary.verified, "{} verified", colors.success),
                    ] {
                        crate::ui::pill(
                            ui,
                            &self.language.text(label).replace("{}", &count.to_string()),
                            color,
                        );
                    }
                    if summary.attention > 0 {
                        crate::ui::pill(
                            ui,
                            &self
                                .language
                                .text("{} attention")
                                .replace("{}", &summary.attention.to_string()),
                            colors.warning,
                        );
                    }
                    if summary.failed > 0 {
                        crate::ui::pill(
                            ui,
                            &self
                                .language
                                .text("{} failed")
                                .replace("{}", &summary.failed.to_string()),
                            colors.danger,
                        );
                    }
                    if summary.delta_required > 0 {
                        crate::ui::pill(
                            ui,
                            &self
                                .language
                                .text("{} delta required")
                                .replace("{}", &summary.delta_required.to_string()),
                            colors.warning,
                        );
                    }
                    let unresolved = summary.unresolved();
                    crate::ui::pill(
                        ui,
                        &self
                            .language
                            .text("{} unresolved")
                            .replace("{}", &unresolved.to_string()),
                        if unresolved == 0 {
                            colors.success
                        } else {
                            colors.danger
                        },
                    );
                });
                ui.add_space(2.0);
                ui.label(RichText::new(self.language.text("Use the state filter and Select visible to act on a focused set; live execution still requires a matching preflight.")).small().color(colors.text_secondary));
            });
            ui.add_space(12.0);
            self.queue_settings_card(ui);
            ui.add_space(12.0);
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
                    // Use the same cached filter the table renders, so the
                    // selection is exactly the rows on screen.
                    self.refresh_bulk_filter_cache();
                    self.bulk_selected_ids = self
                        .bulk_visible_indices
                        .iter()
                        .filter_map(|&index| self.bulk_job_ids.get(index).cloned())
                        .collect();
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
                &self.bulk_job_index_by_id,
            );

            let status_text = self
                .language
                .text("{} selected · {} visible · {} hidden by current filter")
                .replacen("{}", &self.bulk_selected_ids.len().to_string(), 1)
                .replacen("{}", &visible_and_selected.to_string(), 1)
                .replacen("{}", &hidden_selected.to_string(), 1);

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
            let selected_count = self.bulk_selected_ids.len();
            let live_count = self.selected_live_eligible_count(BulkRetryScope::All);
            let delta_count = self.selected_live_eligible_count(BulkRetryScope::DeltaRequired);
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
                                .replace("{}", &selected_count.to_string()),
                        ),
                    )
                    .clicked()
                {
                    run_preflight = true;
                }
                let live_enabled = has_selection && !self.running();
                let live_label = RichText::new(
                    self.language
                        .text("Run live migration ({})")
                        .replace("{}", &live_count.to_string()),
                );
                if ui
                    .add_enabled(
                        live_enabled,
                        if live_enabled {
                            egui::Button::new(live_label.color(Color32::WHITE))
                                .fill(self.theme_colors().danger.gamma_multiply(0.85))
                        } else {
                            egui::Button::new(live_label)
                        },
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
                                .replace("{}", &delta_count.to_string()),
                        ),
                    )
                    .clicked()
                {
                    run_delta = true;
                }
                if ui
                    .add_enabled(
                        // Rows only have durable verification records once a
                        // preflight has admitted the queue into a project.
                        self.bulk_selected_ids.len() == 1 && self.bulk_project_id.is_some(),
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
                .column(Column::remainder().at_least(80.0).clip(true))
                .column(Column::remainder().at_least(110.0).clip(true))
                .column(Column::remainder().at_least(110.0).clip(true))
                .column(Column::auto().at_least(96.0).clip(true))
                .column(Column::remainder().at_least(90.0).clip(true))
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
                                .replace("{}", &job.source_user)
                                .replacen("{}", &job.destination_user, 1);
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
                            crate::ui::endpoint_cell(ui, &job.source_host, &job.source_user);
                        });
                        row.col(|ui| {
                            crate::ui::endpoint_cell(
                                ui,
                                &job.destination_host,
                                &job.destination_user,
                            );
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
                                        RichText::new(
                                            self.language.text(reason.recommended_action()),
                                        )
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

    /// Worker pool, transient retries, and OS-keyring references for rows
    /// without credentials. Collapsed by default to keep the table in view.
    fn queue_settings_card(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        let editable = !self.running();
        let mut apply_source = false;
        let mut apply_destination = false;
        crate::ui::card(ui, |ui| {
            egui::CollapsingHeader::new(
                RichText::new(self.language.text("Queue settings")).strong(),
            )
            .id_salt("queue_settings")
            .show(ui, |ui| {
                crate::ui::form_row(ui, self.language.text("Concurrent workers"), |ui| {
                    ui.add_enabled(
                        editable,
                        egui::DragValue::new(&mut self.form.profile.batch_concurrency).range(1..=16),
                    );
                    ui.label(
                        RichText::new(
                            self.language
                                .text("Applies to preflight and live migration."),
                        )
                        .small()
                        .color(colors.text_secondary),
                    );
                });
                crate::ui::form_row(ui, self.language.text("Transient retries"), |ui| {
                    ui.add_enabled(
                        editable,
                        egui::DragValue::new(&mut self.form.profile.batch_retry_count).range(0..=3),
                    );
                    ui.label(
                        RichText::new(self.language.text(
                            "Authentication and configuration failures are never retried.",
                        ))
                        .small()
                        .color(colors.text_secondary),
                    );
                });
                ui.add_space(8.0);
                crate::ui::section_label(ui, self.language.text("Passwordless queue credentials"));
                ui.label(
                    RichText::new(self.language.text("Apply an existing OS-keyring reference to rows that do not already have a password or credential ID. The secret itself is never copied into the queue."))
                        .small()
                        .color(colors.text_secondary),
                );
                for (label, value, button, flag) in [
                    (
                        "Source keyring ID",
                        &mut self.bulk_source_keyring_apply,
                        "Apply to empty source rows",
                        &mut apply_source,
                    ),
                    (
                        "Destination keyring ID",
                        &mut self.bulk_destination_keyring_apply,
                        "Apply to empty destination rows",
                        &mut apply_destination,
                    ),
                ] {
                    crate::ui::form_row(ui, self.language.text(label), |ui| {
                        ui.add_enabled(
                            editable,
                            egui::TextEdit::singleline(value).desired_width(180.0),
                        );
                        if ui
                            .add_enabled(editable, egui::Button::new(self.language.text(button)))
                            .clicked()
                        {
                            *flag = true;
                        }
                    });
                }
            });
        });
        if apply_source {
            self.apply_bulk_keyring_id(true);
        }
        if apply_destination {
            self.apply_bulk_keyring_id(false);
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
                let profile = &job.defaults.profile;
                let destructive = if profile.delete2 {
                    self.language
                        .text("DESTRUCTIVE: destination deletion enabled")
                } else {
                    self.language.text("destination deletion disabled")
                };
                crate::ui::card(ui, |ui| {
                    ui.label(RichText::new(&job.label).strong());
                    ui.label(format!("{} → {}", job.source_user, job.destination_user));
                    ui.label(format!("{} → {}", job.source_host, job.destination_host));
                    let summary = review_state_summary(
                        self.language.text("State: {} · {}"),
                        self.language.text(crate::ui::display_job_state(&job.state)),
                        destructive,
                    );
                    ui.label(if profile.delete2 {
                        RichText::new(summary)
                            .strong()
                            .color(self.theme_colors().danger)
                    } else {
                        RichText::new(summary)
                    });
                });
            }
        });
    }
}

/// Fill the review drawer's "State: {} · {}" template. Each placeholder is
/// replaced once, in order, so the destination-deletion status is never
/// overwritten by the state.
fn review_state_summary(template: &str, state: &str, deletion: &str) -> String {
    template
        .replacen("{}", state, 1)
        .replacen("{}", deletion, 1)
}

#[cfg(test)]
mod review_drawer_tests {
    use super::review_state_summary;

    #[test]
    fn review_summary_shows_state_and_destination_deletion_status() {
        let summary = review_state_summary(
            "State: {} · {}",
            "Ready",
            "DESTRUCTIVE: destination deletion enabled",
        );
        assert_eq!(
            summary,
            "State: Ready · DESTRUCTIVE: destination deletion enabled"
        );
    }
}
