//! Activity and bounded engine-output presentation.

use crate::ui::{
    StatusSeverity, WorkspaceView, contains_ascii_case_insensitive, needs_operator_review,
    status_color,
};
use crate::{App, MAX_ACTIVITY_HISTORY_ROWS};
use eframe::egui::{self, RichText};

impl App {
    pub(crate) fn activity_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Activity");
        ui.label(RichText::new("Live output is retained here for operator review. Durable run history remains available after restart.").color(self.theme_colors().text_secondary));
        ui.add_space(12.0);
        if !self.workspace_read_only
            && let Some(job) = self.job_id.as_deref()
        {
            let state = self
                .ui_snapshot
                .verification_rows
                .iter()
                .find(|mailbox| mailbox.job.id == job)
                .map(|mailbox| mailbox.job.state.clone());
            match state.as_deref() {
                Some(state) if needs_operator_review(state) => {
                    if ui.button("Prepare safe retry  →").clicked() {
                        self.form.dry_run = true;
                        self.live_confirmed = false;
                        self.active_view = WorkspaceView::Plan;
                        self.set_status(
                            "Retry prepared as a dry preflight. Review the exact plan before any live run.",
                            StatusSeverity::Info,
                        );
                    }
                }
                Some(_) | None => {}
            }
        }
        ui.group(|ui| {
            ui.horizontal(|ui| {
                let running = self.running();
                ui.heading(if running {
                    "Run in progress"
                } else {
                    "No active run"
                });
                ui.label(
                    RichText::new(&self.status.text)
                        .color(status_color(self.status.severity, self.theme_colors())),
                );
                if ui.button("Copy support summary").clicked() {
                    ui.ctx().copy_text(self.support_summary());
                }
                ui.menu_button("Raw output…", |ui| {
                    ui.label(
                        RichText::new("May contain mailbox metadata")
                            .color(self.theme_colors().warning),
                    );
                    if ui.button("Copy redacted engine output").clicked() {
                        ui.ctx()
                            .copy_text(self.output.iter().cloned().collect::<Vec<_>>().join("\n"));
                        ui.close();
                    }
                });
                if running && ui.button("Stop migration").clicked() {
                    self.stop_confirm_open = true;
                }
            });
            egui::ScrollArea::vertical()
                .hscroll(true)
                .stick_to_bottom(true)
                .max_height(420.0)
                .show_rows(ui, 20.0, self.output.len(), |ui, rows| {
                    for index in rows {
                        if let Some(line) = self.output.get(index) {
                            ui.add(
                                egui::Label::new(RichText::new(line).monospace().size(14.0))
                                    .wrap_mode(egui::TextWrapMode::Extend),
                            );
                        }
                    }
                });
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.heading("Durable run history");
            let history_label = if self.activity_show_all {
                "Show recent 20"
            } else {
                "Show up to 250 runs"
            };
            if ui.button(history_label).clicked() {
                self.activity_show_all = !self.activity_show_all;
            }
        });
        let Some(_project) = self.active_project_id().map(str::to_owned) else {
            ui.label(
                RichText::new("Create or restore a project to see durable runs.")
                    .color(self.theme_colors().text_secondary),
            );
            return;
        };
        let run_limit = if self.activity_show_all {
            MAX_ACTIVITY_HISTORY_ROWS
        } else {
            20
        };
        ui.horizontal_wrapped(|ui| {
            ui.label("Filter history");
            ui.add(
                egui::TextEdit::singleline(&mut self.activity_search)
                    .hint_text("mailbox, phase, engine, run ID, or detail")
                    .desired_width(280.0),
            );
            egui::ComboBox::from_id_salt("activity_status_filter")
                .selected_text(match self.activity_status_filter.as_str() {
                    "errors" => "Errors and attention",
                    "running" => "Running",
                    "completed" => "Completed",
                    _ => "All statuses",
                })
                .show_ui(ui, |ui| {
                    for (value, label) in [
                        ("all", "All statuses"),
                        ("errors", "Errors and attention"),
                        ("running", "Running"),
                        ("completed", "Completed"),
                    ] {
                        ui.selectable_value(&mut self.activity_status_filter, value.into(), label);
                    }
                });
        });
        let runs = self
            .ui_snapshot
            .runs
            .iter()
            .take(run_limit as usize)
            .cloned()
            .collect::<Vec<_>>();
        match runs {
            runs if runs.is_empty() => {
                ui.label(
                    RichText::new("No durable runs recorded yet.")
                        .color(self.theme_colors().text_secondary),
                );
            }
            runs => {
                let search = self.activity_search.trim();
                let visible = runs
                    .iter()
                    .enumerate()
                    .filter(|(_, run)| {
                        let status_match = match self.activity_status_filter.as_str() {
                            "errors" => run.status != "completed" && run.status != "running",
                            "running" => run.status == "running",
                            "completed" => run.status == "completed",
                            _ => true,
                        };
                        let mailbox = run
                            .destination_mailbox
                            .as_deref()
                            .or(run.source_mailbox.as_deref())
                            .unwrap_or("batch");
                        let text_match = search.is_empty()
                            || [
                                run.id.as_str(),
                                mailbox,
                                run.phase_at_start.as_str(),
                                run.engine.as_str(),
                                run.status.as_str(),
                                run.detail.as_str(),
                            ]
                            .iter()
                            .any(|value| contains_ascii_case_insensitive(value, search));
                        status_match && text_match
                    })
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>();
                ui.label(
                    RichText::new(format!(
                        "{} visible of {} loaded",
                        visible.len(),
                        runs.len()
                    ))
                    .color(self.theme_colors().text_secondary),
                );
                if self.activity_show_all && runs.len() == MAX_ACTIVITY_HISTORY_ROWS as usize {
                    ui.label(
                        RichText::new("Showing the newest 250 runs. Export the audit report for complete history.")
                            .color(self.theme_colors().text_secondary),
                    );
                }
                egui::Grid::new("durable_run_history")
                    .striped(true)
                    .show(ui, |ui| {
                        for label in [
                            "Run", "Mailbox", "Stage", "Engine", "Status", "Started", "Finished",
                            "Detail",
                        ] {
                            ui.strong(label);
                        }
                        ui.end_row();
                    });
                egui::ScrollArea::vertical()
                    .id_salt("durable_run_history_rows")
                    .max_height(420.0)
                    .show_rows(ui, 32.0, visible.len(), |ui, rows| {
                        egui::Grid::new("durable_run_history_rows_grid")
                            .striped(true)
                            .show(ui, |ui| {
                                for row in rows {
                                    let index = visible[row];
                                    let run = &runs[index];
                                    ui.label(
                                        RichText::new(&run.id[..8.min(run.id.len())]).monospace(),
                                    );
                                    ui.label(
                                        run.destination_mailbox
                                            .as_deref()
                                            .or(run.source_mailbox.as_deref())
                                            .unwrap_or("Batch")
                                            .to_owned(),
                                    );
                                    ui.label(&run.phase_at_start);
                                    ui.label(&run.engine);
                                    ui.label(RichText::new(&run.status).color(
                                        if run.status == "completed" {
                                            self.theme_colors().success
                                        } else if run.status == "running" {
                                            self.theme_colors().info
                                        } else {
                                            self.theme_colors().danger
                                        },
                                    ));
                                    ui.label(&run.started_at);
                                    ui.label(run.finished_at.as_deref().unwrap_or("in progress"));
                                    ui.label(if run.detail.is_empty() {
                                        "—"
                                    } else {
                                        &run.detail
                                    });
                                    ui.end_row();
                                }
                            });
                    });
            }
        }
    }
}
