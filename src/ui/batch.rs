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
use crate::ui::job_state_badge;
use eframe::egui::{self, RichText};
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
        let rows: Box<dyn Iterator<Item = BatchActionRow<'_>> + '_> = if self.bulk_all_selected {
            Box::new(
                self.bulk_job_ids
                    .iter()
                    .enumerate()
                    .filter(|(_, id)| self.bulk_is_selected(id))
                    .map(|(index, id)| BatchActionRow {
                        id,
                        selected: true,
                        visible: self.bulk_visible_indices.binary_search(&index).is_ok(),
                        durable_state: Some(self.bulk_jobs[index].state.as_str()),
                        destructive: self.bulk_jobs[index]
                            .defaults
                            .profile
                            .destination_mutation_policy()
                            .may_remove_destination_state(),
                    }),
            )
        } else {
            Box::new(self.bulk_selected_ids.iter().map(|id| {
                match self.bulk_job_index(id) {
                    Some(index) => BatchActionRow {
                        id,
                        selected: true,
                        visible: self.bulk_visible_indices.binary_search(&index).is_ok(),
                        durable_state: Some(self.bulk_jobs[index].state.as_str()),
                        destructive: self.bulk_jobs[index]
                            .defaults
                            .profile
                            .destination_mutation_policy()
                            .may_remove_destination_state(),
                    },
                    None => BatchActionRow {
                        id,
                        selected: true,
                        visible: false,
                        durable_state: None,
                        destructive: false,
                    },
                }
            }))
        };
        build_batch_action_plan(
            rows,
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

    /// Recompute the per-frame selection projection in one pass over the
    /// selection. Selections can cover all 100k rows, so the drawer and the
    /// page's counts share this instead of each walking the selection.
    pub(crate) fn refresh_bulk_selection_view(&mut self) {
        self.refresh_bulk_filter_cache();
        let mut view = std::mem::take(&mut self.bulk_selection_view);
        view.rows.clear();
        view.visible = 0;
        view.live_eligible = 0;
        view.delta_eligible = 0;
        view.ready = 0;
        view.review = 0;
        view.selected_loaded = 0;
        let compact_all = self.bulk_all_selected && self.bulk_selection_count() > 1;
        if self.bulk_all_selected {
            for (index, id) in self.bulk_job_ids.iter().enumerate() {
                if self.bulk_is_selected(id) {
                    self.accumulate_selection_row(&mut view, index, compact_all);
                }
            }
        } else {
            for id in &self.bulk_selected_ids {
                if let Some(index) = self.bulk_job_index(id) {
                    self.accumulate_selection_row(&mut view, index, false);
                }
            }
        }
        view.rows.sort_unstable();
        self.bulk_selection_view = view;
    }

    fn accumulate_selection_row(&self, view: &mut SelectionView, index: usize, omit_index: bool) {
        if !omit_index {
            view.rows.push(index);
        }
        view.selected_loaded += 1;
        let job = &self.bulk_jobs[index];
        view.ready += usize::from(job.state == "ready");
        view.review += usize::from(crate::ui::needs_operator_review(&job.state));
        let state = job.state.as_str();
        // Same eligibility rule as `build_batch_action_plan`.
        view.live_eligible += usize::from(BulkRetryScope::All.includes(state));
        view.delta_eligible += usize::from(BulkRetryScope::DeltaRequired.includes(state));
        view.visible += usize::from(self.bulk_visible_indices.binary_search(&index).is_ok());
    }

    pub(crate) fn mailbox_view(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        crate::ui::page_header(
            ui,
            self.language.message("ui.mailboxes-b23105d5"),
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
                    if ui
                        .small_button(self.language.message("ui.dismiss"))
                        .clicked()
                    {
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
                    RichText::new(self.language.message("ui.no-bulk-mailbox-list-loaded"))
                        .size(17.0)
                        .strong(),
                );
                ui.label(
                    self.language
                        .text("A single mailbox can be configured from the migration plan."),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if crate::ui::primary_button(ui, self.language.message("ui.import-csv-xlsx"))
                        .clicked()
                    {
                        self.choose_bulk_import();
                    }
                    if ui
                        .button(self.language.message("ui.open-migration-plan-0342a28e"))
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
                        egui::Button::new(self.language.message("ui.import-csv-xlsx")),
                    )
                    .clicked()
                {
                    self.choose_bulk_import();
                }
                if ui
                    .add_enabled(
                        !self.bulk_selection_is_empty(),
                        egui::Button::new(self.language.message("ui.export-selected-set")),
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
                        egui::Button::new(self.language.message("ui.clear-queue")),
                    )
                    .clicked()
                {
                    self.bulk_clear_confirm_open = true;
                }
            });
            let summary = self.bulk_queue_summary();
            crate::ui::card(ui, |ui| {
                crate::ui::section_label(ui, self.language.message("ui.queue-health"));
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
                ui.label(RichText::new(self.language.message("ui.use-the-state-filter-and-select-visible-to-act-on-a-focused-set-live-execut-0cd48b4754")).small().color(colors.text_secondary));
            });
            ui.add_space(12.0);
            self.queue_settings_card(ui);
            ui.add_space(12.0);
            let mut selection_changed = false;
            ui.horizontal_wrapped(|ui| {
                ui.label(self.language.message("ui.search"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.bulk_search)
                        .hint_text(self.language.message("ui.mailbox-host-or-user"))
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
                if ui
                    .button(self.language.message("ui.select-visible"))
                    .clicked()
                {
                    // Use the same cached filter the table renders, so the
                    // selection is exactly the rows on screen.
                    self.refresh_bulk_filter_cache();
                    if self.bulk_visible_indices.len() == self.bulk_jobs.len() {
                        self.select_all_bulk_rows();
                    } else {
                        self.bulk_all_selected = false;
                        self.bulk_selected_ids = self
                            .bulk_visible_indices
                            .iter()
                            .filter_map(|&index| self.bulk_job_ids.get(index).cloned())
                            .collect();
                    }
                    selection_changed = true;
                }
                if ui
                    .button(self.language.message("ui.select-unresolved"))
                    .clicked()
                {
                    self.select_bulk_state_set(BulkStateSet::Unresolved);
                    selection_changed = true;
                }
                if ui
                    .button(self.language.message("ui.select-attention"))
                    .clicked()
                {
                    self.select_bulk_state_set(BulkStateSet::Attention);
                    selection_changed = true;
                }
                if ui
                    .button(self.language.message("ui.clear-selection"))
                    .clicked()
                {
                    self.clear_bulk_selection();
                    selection_changed = true;
                }
            });
            // Rebuild normalized search values and filtered indices only when
            // the queue, query, or state filter changes. The table still
            // virtualizes row widgets without doing a full filter pass on
            // every repaint.
            // The shell computed the selection view before drawing the review
            // drawer; recompute only if this frame changed the selection or
            // the filter, and repaint so the drawer catches up.
            if self.refresh_bulk_filter_cache() || selection_changed {
                self.refresh_bulk_selection_view();
                ui.ctx().request_repaint();
            }
            let visible_indices = std::mem::take(&mut self.bulk_visible_indices);
            let visible_and_selected = self.bulk_selection_view.visible;
            // Selected IDs no longer in the queue count as hidden too.
            let hidden_selected = self
                .bulk_selection_count()
                .saturating_sub(visible_and_selected);

            let status_text = self
                .language
                .text("{} selected · {} visible · {} hidden by current filter")
                .replacen("{}", &self.bulk_selection_count().to_string(), 1)
                .replacen("{}", &visible_and_selected.to_string(), 1)
                .replacen("{}", &hidden_selected.to_string(), 1);

            let has_selection = !self.bulk_selection_is_empty();
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(status_text).color(self.theme_colors().text_secondary));
                if has_selection {
                    let review_label = self
                        .language
                        .text("Review selected ({})")
                        .replace("{}", &self.bulk_selection_count().to_string());
                    let review_clicked = ui.small_button(review_label).clicked();
                    let actions_clicked = ui
                        .small_button(self.language.message("ui.actions-toggle-inspector"))
                        .clicked();
                    if review_clicked || actions_clicked {
                        self.bulk_inspector_open = !self.bulk_inspector_open;
                    }
                }
            });
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
            let mut open_assessment = None;
            if has_selection && self.bulk_inspector_open && !self.bulk_inspector_side_panel {
                crate::ui::card(ui, |ui| {
                    open_assessment = self.selection_review_drawer(ui);
                });
            }
            let selected_count = self.bulk_selection_count();
            let live_count = self.bulk_selection_view.live_eligible;
            let delta_count = self.bulk_selection_view.delta_eligible;
            let mut run_preflight = false;
            let mut run_live = false;
            let mut run_delta = false;
            let mut review_selected = false;
            if has_selection && self.bulk_inspector_open {
                ui.horizontal_wrapped(|ui| {
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
                    let live_enabled = live_count > 0 && !self.running();
                    let live_label = RichText::new(
                        self.language
                            .text("Run live migration ({})")
                            .replace("{}", &live_count.to_string()),
                    );
                    if ui
                        .add_enabled(
                            live_enabled,
                            egui::Button::new(live_label).fill(ui.visuals().widgets.active.bg_fill),
                        )
                        .clicked()
                    {
                        run_live = true;
                    }
                    if ui
                        .add_enabled(
                            delta_count > 0 && !self.running(),
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
                            self.bulk_selection_count() == 1 && self.bulk_project_id.is_some(),
                            egui::Button::new(self.language.message("ui.review-verification")),
                        )
                        .clicked()
                    {
                        review_selected = true;
                    }
                });
            }
            if has_selection && self.bulk_inspector_open && live_count < selected_count {
                let ineligible = selected_count - live_count;
                ui.label(
                    RichText::new(
                        self.language
                            .text("{} selected mailbox(es) are unavailable for live migration.")
                            .replace("{}", &ineligible.to_string()),
                    )
                    .color(self.theme_colors().text_secondary),
                );
            }
            if has_selection && self.bulk_inspector_open && delta_count < selected_count {
                let ineligible = selected_count - delta_count;
                ui.label(
                    RichText::new(
                        self.language
                            .text("{} selected mailbox(es) are not marked as requiring a final delta.")
                            .replace("{}", &ineligible.to_string()),
                    )
                    .color(self.theme_colors().text_secondary),
                );
            }
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
                    .find(|id| self.bulk_is_selected(id))
            {
                self.job_id = Some(job_id.clone());
                self.active_view = WorkspaceView::Verification;
            }
            if let Some(job_id) = open_assessment {
                self.job_id = Some(job_id);
                self.active_view = WorkspaceView::Verification;
            }
            ui.add_space(8.0);
            let table_width = ui.available_width();
            let show_endpoints = table_width >= 760.0;
            let show_operator_action = table_width >= 1100.0;
            let mut table = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::auto())
                .column(Column::remainder().at_least(120.0).clip(true));
            if show_endpoints {
                table = table
                    .column(Column::remainder().at_least(110.0).clip(true))
                    .column(Column::remainder().at_least(110.0).clip(true));
            }
            table = table.column(Column::auto().at_least(96.0).clip(true));
            if show_operator_action {
                table = table.column(Column::remainder().at_least(90.0).clip(true));
            }
            table
                .header(32.0, |mut header| {
                    let mut labels = vec!["", "Mailbox"];
                    if show_endpoints {
                        labels.extend(["Source", "Destination"]);
                    }
                    labels.push("State");
                    if show_operator_action {
                        labels.push("Operator action");
                    }
                    for label in labels {
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
                        let mut selected = self.bulk_is_selected(job_id);
                        let all_selected = self.bulk_all_selected;
                        row.col(|ui| {
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
                                if all_selected {
                                    if selected {
                                        self.bulk_selected_ids.remove(job_id);
                                    } else {
                                        self.bulk_selected_ids.insert(job_id.clone());
                                    }
                                } else if selected {
                                    self.bulk_selected_ids.insert(job_id.clone());
                                } else {
                                    self.bulk_selected_ids.remove(job_id);
                                }
                                // Counts and the drawer refresh next frame.
                                ui.ctx().request_repaint();
                            }
                        });
                        row.col(|ui| {
                            ui.label(&job.label);
                        });
                        if show_endpoints {
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
                        }
                        row.col(|ui| {
                            let (badge, color) = job_state_badge(&job.state, colors);
                            ui.label(RichText::new(self.language.text(badge)).color(color));
                        });
                        if show_operator_action {
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
                                                self.language
                                                    .message("ui.inspect-durable-run-detail"),
                                            )
                                            .color(self.theme_colors().text_secondary),
                                        );
                                    }
                                }
                            });
                        }
                    });
                });
            self.bulk_visible_indices = visible_indices;
            ui.label(
                RichText::new(self.language.message("ui.batch-actions-apply-only-to-explicitly-selected-rows-use-select-unresolved-a54ac35192"))
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
                RichText::new(self.language.message("ui.queue-settings")).strong(),
            )
            .id_salt("queue_settings")
            .show(ui, |ui| {
                crate::ui::form_row(ui, self.language.message("ui.concurrent-workers"), |ui| {
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
                crate::ui::form_row(ui, self.language.message("ui.transient-retries"), |ui| {
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
                crate::ui::section_label(ui, self.language.message("ui.passwordless-queue-credentials"));
                ui.label(
                    RichText::new(self.language.message("ui.apply-an-existing-os-keyring-reference-to-rows-that-do-not-already-have-a-p-9e461c8815"))
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

    pub(crate) fn selection_review_drawer(&mut self, ui: &mut egui::Ui) -> Option<String> {
        ui.horizontal(|ui| {
            ui.heading(
                self.language
                    .text("Review selected ({})")
                    .replace("{}", &self.bulk_selection_count().to_string()),
            );
            if ui
                .small_button(self.language.message("ui.close-inspector"))
                .clicked()
            {
                self.bulk_inspector_open = false;
            }
        });
        ui.label(
            RichText::new(
                self.language
                    .text("Selected mailbox scope remains explicit while this drawer is open."),
            )
            .size(11.0)
            .color(self.theme_colors().text_secondary),
        );
        ui.separator();
        if self.bulk_selection_is_empty() {
            ui.label(
                RichText::new(self.language.message("ui.no-mailboxes-selected"))
                    .color(self.theme_colors().text_secondary),
            );
            return None;
        }
        let mut open_assessment = None;
        if self.bulk_selection_count() == 1 {
            if let Some(&index) = self.bulk_selection_view.rows.first() {
                open_assessment = self.review_card(ui, index);
            } else {
                ui.label(
                    self.language
                        .text("The selected mailbox is not present in the current queue."),
                );
            }
            return open_assessment;
        }

        let ready_count = self.bulk_selection_view.ready;
        let review_count = self.bulk_selection_view.review;
        crate::ui::card(ui, |ui| {
            ui.heading(
                self.language
                    .text("{} selected")
                    .replace("{}", &self.bulk_selection_count().to_string()),
            );
            ui.label(
                RichText::new(
                    self.language
                        .text("{} ready for pilot")
                        .replace("{}", &ready_count.to_string()),
                )
                .color(self.theme_colors().success),
            );
            if review_count > 0 {
                ui.label(
                    RichText::new(
                        self.language
                            .text("{} require operator review")
                            .replace("{}", &review_count.to_string()),
                    )
                    .color(self.theme_colors().warning),
                );
            }
            let other_count = self
                .bulk_selection_view
                .selected_loaded
                .saturating_sub(ready_count + review_count);
            if other_count > 0 {
                ui.label(
                    RichText::new(
                        self.language
                            .text("{} in other states; not classified as ready or review-blocked")
                            .replace("{}", &other_count.to_string()),
                    )
                    .color(self.theme_colors().text_secondary),
                );
            }
            let unavailable_count = self
                .bulk_selection_count()
                .saturating_sub(self.bulk_selection_view.selected_loaded);
            if unavailable_count > 0 {
                ui.label(
                    RichText::new(
                        self.language
                            .text("{} selected rows are unavailable in the loaded queue.")
                            .replace("{}", &unavailable_count.to_string()),
                    )
                    .color(self.theme_colors().danger),
                );
            }
            ui.label(
                RichText::new(self.language.message("ui.size-and-completion-time-estimates-require-inventory-and-throughput-data-no-aa15a03260"))
                    .small()
                    .color(self.theme_colors().text_secondary),
            );
        });
        ui.add_space(8.0);
        // Selections can cover the whole 100k-row queue, so lay out only the
        // cards in view. Every card has the same six single-line rows, which
        // gives `show_rows` a uniform height; long values truncate and show
        // the full text on hover.
        let selected = &self.bulk_selection_view.rows;
        let line = ui.text_style_height(&egui::TextStyle::Body);
        let spacing = ui.spacing().item_spacing.y;
        // Six lines, five gaps between them, the card's 12 px vertical
        // margins, and its 1 px border.
        let card_height = 6.0 * line + 5.0 * spacing + 2.0 * 12.0 + 2.0;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, card_height, selected.len(), |ui, rows| {
                for &index in &selected[rows] {
                    self.review_summary_card(ui, &self.bulk_jobs[index]);
                }
            });
        open_assessment
    }

    fn review_summary_card(&self, ui: &mut egui::Ui, job: &crate::bulk_import::BulkJob) {
        let policy = job.defaults.profile.destination_mutation_policy();
        let line = |ui: &mut egui::Ui, text: RichText| {
            let full = text.text().to_owned();
            ui.add(egui::Label::new(text).truncate())
                .on_hover_text(full);
        };
        crate::ui::card(ui, |ui| {
            line(ui, RichText::new(&job.label).strong());
            line(ui, RichText::new(format!("{} →", job.source_user)));
            line(ui, RichText::new(&job.destination_user));
            line(ui, RichText::new(format!("{} →", job.source_host)));
            line(ui, RichText::new(&job.destination_host));
            let (badge, color) = job_state_badge(&job.state, self.theme_colors());
            ui.horizontal(|ui| {
                ui.label(RichText::new(self.language.text(badge)).color(color));
                ui.separator();
                ui.add(
                    egui::Label::new(RichText::new(self.language.text(policy.label())).color(
                        if policy.may_remove_destination_state() {
                            self.theme_colors().danger
                        } else {
                            self.theme_colors().text_primary
                        },
                    ))
                    .truncate(),
                )
                .on_hover_text(self.language.text(policy.warning()));
            });
        });
    }

    fn review_card(&self, ui: &mut egui::Ui, index: usize) -> Option<String> {
        let job = &self.bulk_jobs[index];
        let job_id = self.bulk_job_ids.get(index)?;
        // The same derived policy the batch plan counts and confirms.
        let policy = job.defaults.profile.destination_mutation_policy();
        let removes = policy.may_remove_destination_state();
        let line = |ui: &mut egui::Ui, text: RichText| {
            let full = text.text().to_owned();
            ui.add(egui::Label::new(text).truncate())
                .on_hover_text(full);
        };
        let evidence = self
            .ui_snapshot
            .verification_rows
            .iter()
            .find(|row| row.job.id == *job_id);
        let show_assessment = evidence.is_some();
        let mut open_assessment = false;
        crate::ui::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(&job.label);
                let (badge, color) = job_state_badge(&job.state, self.theme_colors());
                crate::ui::pill(ui, self.language.text(badge), color);
            });
            ui.separator();
            ui.strong(self.language.message("ui.source-0e570ca6"));
            line(ui, RichText::new(&job.source_user).strong());
            line(ui, RichText::new(&job.source_host).small());
            ui.add_space(4.0);
            ui.strong(self.language.message("ui.destination-293d404a"));
            line(ui, RichText::new(&job.destination_user).strong());
            line(ui, RichText::new(&job.destination_host).small());
            ui.add_space(4.0);
            ui.strong(self.language.message("ui.migration-behavior"));
            line(
                ui,
                RichText::new(self.language.text(policy.label()))
                    .strong()
                    .color(if removes {
                        self.theme_colors().danger
                    } else {
                        self.theme_colors().text_primary
                    }),
            );
            line(
                ui,
                RichText::new(self.language.text(policy.warning()))
                    .small()
                    .color(if removes {
                        self.theme_colors().danger
                    } else {
                        self.theme_colors().text_secondary
                    }),
            );
            if let Some((_, inventory, snapshot)) = evidence.and_then(|row| row.evidence.as_ref()) {
                ui.add_space(4.0);
                ui.strong(self.language.message("ui.recorded-verification-inventory"));
                line(
                    ui,
                    RichText::new(
                        self.language
                            .text("{} folders · {} messages · {}")
                            .replacen("{}", &inventory.source_folders.to_string(), 1)
                            .replacen("{}", &inventory.source_messages.to_string(), 1)
                            .replacen("{}", &format_data_size(inventory.source_bytes), 1),
                    ),
                );
                if let Some(snapshot) = snapshot {
                    let digest = crate::plan_identity::fingerprint_digest(snapshot);
                    line(
                        ui,
                        RichText::new(
                            self.language
                                .text("Recorded plan snapshot digest: {}")
                                .replace("{}", &digest[..12]),
                        )
                        .small(),
                    );
                }
            }
            if show_assessment
                && ui
                    .button(self.language.message("ui.view-complete-assessment"))
                    .clicked()
            {
                open_assessment = true;
            }
        });
        if open_assessment {
            return Some(job_id.clone());
        }
        None
    }
}

fn format_data_size(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    if bytes >= GIB as u64 {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else if bytes >= MIB as u64 {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes >= KIB as u64 {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// One frame's projection of the explicit selection onto the queue.
#[derive(Default)]
pub(crate) struct SelectionView {
    /// Queue indices of selected rows, ascending. Compact select-all keeps
    /// only the one-row inspection index rather than materializing the set.
    pub(crate) rows: Vec<usize>,
    /// Selected rows that the current filter shows.
    pub(crate) visible: usize,
    /// Selected rows a live run (all scope) would include.
    pub(crate) live_eligible: usize,
    /// Selected rows a final-delta run would include.
    pub(crate) delta_eligible: usize,
    pub(crate) selected_loaded: usize,
    pub(crate) ready: usize,
    pub(crate) review: usize,
}

#[cfg(test)]
mod selection_inspector_tests {
    use super::format_data_size;

    #[test]
    fn recorded_data_size_uses_readable_binary_units_without_rounding_small_values_to_zero() {
        assert_eq!(format_data_size(0), "0 B");
        assert_eq!(format_data_size(1536), "1.5 KiB");
        assert_eq!(format_data_size(2 * 1024 * 1024), "2.0 MiB");
        assert_eq!(format_data_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
