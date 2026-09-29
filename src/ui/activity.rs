//! Activity and bounded engine-output presentation.

use crate::ui::{
    StatusSeverity, WorkspaceView, contains_ascii_case_insensitive, needs_operator_review,
    status_color,
};
use crate::{App, MAX_ACTIVITY_HISTORY_ROWS};
use eframe::egui::{self, Color32, RichText};
use std::sync::atomic::Ordering;

impl App {
    pub(crate) fn stop_confirmation(&mut self, ctx: &egui::Context) {
        if !self.stop_confirm_open || !self.running() {
            return;
        }
        let mut close_requested = false;
        let response = egui::Modal::new(egui::Id::new("stop_migration_confirmation")).show(ctx, |ui| {
                ui.heading(RichText::new(self.language.text("The migration will stop where it is")).color(self.theme_colors().danger));
                ui.label(self.language.text("The destination may be partially migrated. A later preflight, delta, or verification pass may be required before continuing."));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let keep_running = ui.button(self.language.text("Keep running"));
                    if !self.stop_confirm_focus_requested {
                        keep_running.request_focus();
                        self.stop_confirm_focus_requested = true;
                    }
                    if keep_running.clicked() {
                        close_requested = true;
                    }
                    if ui.add(egui::Button::new(RichText::new(self.language.text("Stop migration")).color(Color32::WHITE)).fill(self.theme_colors().danger)).clicked() {
                        if let Some(cancel) = &self.cancel_requested {
                            cancel.store(true, Ordering::Relaxed);
                        }
                        self.set_status(
                            self.language.text("Cancellation requested…"),
                            StatusSeverity::Warning,
                        );
                        close_requested = true;
                    }
                });
            });
        self.stop_confirm_open = !(close_requested || response.should_close());
        if !self.stop_confirm_open {
            self.stop_confirm_focus_requested = false;
        }
    }

    pub(crate) fn activity_view(&mut self, ui: &mut egui::Ui) {
        crate::ui::page_header(
            ui,
            self.language.text("Activity"),
            self.language.text("Live output is retained here for operator review. Durable run history remains available after restart."),
        );
        let selected_recovery_guidance = self
            .job_id
            .as_deref()
            .and_then(|job_id| self.cached_report_mailbox(job_id))
            .and_then(|mailbox| {
                mailbox.attention_reason.and_then(|reason| {
                    crate::core::recovery_dashboard::InterruptionReason::from_attention_reason(
                        reason,
                    )
                    .map(|interruption| {
                        (
                            reason.label(),
                            crate::core::recovery_dashboard::RecoveryPlanner::generate_guidance(
                                interruption,
                            ),
                        )
                    })
                })
            });
        if let Some((reason_label, guidance)) = selected_recovery_guidance {
            crate::ui::card(ui, |ui| {
                ui.heading(self.language.text("Recovery guidance"));
                ui.label(
                    RichText::new(self.language.text(reason_label))
                        .color(self.theme_colors().warning),
                );
                ui.label(
                    RichText::new(
                        self.language
                            .text("Do not resume until the current endpoint and durable state have been reviewed."),
                    )
                    .color(self.theme_colors().text_secondary),
                );
                for step in guidance {
                    ui.label(format!("• {}", self.language.text(step)));
                }
            });
            ui.add_space(8.0);
        }
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
                    if ui
                        .button(self.language.text("Prepare safe retry  →"))
                        .clicked()
                    {
                        self.form.dry_run = true;
                        self.live_confirmed = false;
                        self.active_view = WorkspaceView::Plan;
                        self.set_status(
                    self.language.text("Retry prepared as a dry preflight. Review the exact plan before any live run."),
                            StatusSeverity::Info,
                        );
                    }
                }
                Some(_) | None => {}
            }
        }
        crate::ui::card(ui, |ui| {
            ui.horizontal(|ui| {
                let running = self.running();
                ui.heading(if running {
                    self.language.text("Run in progress")
                } else {
                    self.language.text("No active run")
                });
                if let Some(started_at) = self.run_started_at {
                    ui.label(
                        self.language
                            .text("Elapsed {}")
                            .replace("{}", &crate::ui::format_elapsed(started_at.elapsed())),
                    );
                }
                ui.label(
                    RichText::new(&self.status.text)
                        .color(status_color(self.status.severity, self.theme_colors())),
                );
                if ui
                    .button(self.language.text("Copy support summary"))
                    .clicked()
                {
                    ui.ctx().copy_text(self.support_summary());
                }
                ui.menu_button(self.language.text("Raw output…"), |ui| {
                    ui.label(
                        RichText::new(self.language.text("May contain mailbox metadata"))
                            .color(self.theme_colors().warning),
                    );
                    if ui
                        .button(self.language.text("Copy redacted engine output"))
                        .clicked()
                    {
                        ui.ctx()
                            .copy_text(self.output.iter().cloned().collect::<Vec<_>>().join("\n"));
                        ui.close();
                    }
                });
                if running && ui.button(self.language.text("Stop migration")).clicked() {
                    self.stop_confirm_open = true;
                    self.stop_confirm_focus_requested = false;
                }
            });
            ui.add_space(8.0);
            let console = ui.visuals().extreme_bg_color;
            egui::Frame::new()
                .fill(console)
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .hscroll(true)
                        .stick_to_bottom(true)
                        .max_height(420.0)
                        .show_rows(ui, 18.0, self.output.len(), |ui, rows| {
                            for index in rows {
                                if let Some(line) = self.output.get(index) {
                                    ui.add(
                                        egui::Label::new(RichText::new(line).monospace())
                                            .wrap_mode(egui::TextWrapMode::Extend),
                                    );
                                }
                            }
                        });
                });
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.heading(self.language.text("Durable run history"));
            let history_label = if self.activity_show_all {
                self.language.text("Show recent 20")
            } else {
                self.language.text("Show up to 250 runs")
            };
            if ui.button(history_label).clicked() {
                self.activity_show_all = !self.activity_show_all;
            }
        });
        let Some(_project) = self.active_project_id().map(str::to_owned) else {
            ui.label(
                RichText::new(
                    self.language
                        .text("Create or restore a project to see durable runs."),
                )
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
            ui.label(self.language.text("Filter history"));
            ui.add(
                egui::TextEdit::singleline(&mut self.activity_search)
                    .hint_text(
                        self.language
                            .text("mailbox, phase, engine, run ID, or detail"),
                    )
                    .desired_width(280.0),
            );
            egui::ComboBox::from_id_salt("activity_status_filter")
                .selected_text(match self.activity_status_filter.as_str() {
                    "errors" => self.language.text("Errors and attention"),
                    "running" => self.language.text("Running"),
                    "completed" => self.language.text("Completed"),
                    _ => self.language.text("All statuses"),
                })
                .show_ui(ui, |ui| {
                    for (value, label) in [
                        ("all", self.language.text("All statuses")),
                        ("errors", self.language.text("Errors and attention")),
                        ("running", self.language.text("Running")),
                        ("completed", self.language.text("Completed")),
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
                    RichText::new(self.language.text("No durable runs recorded yet."))
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
                    RichText::new(
                        self.language
                            .text("{} visible of {} loaded")
                            .replacen("{}", &visible.len().to_string(), 1)
                            .replacen("{}", &runs.len().to_string(), 1),
                    )
                    .color(self.theme_colors().text_secondary),
                );
                if self.activity_show_all && runs.len() == MAX_ACTIVITY_HISTORY_ROWS as usize {
                    ui.label(
                        RichText::new(self.language.text("Showing the newest 250 runs. Export the audit report for complete history."))
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
                            ui.strong(self.language.text(label));
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
                                    ui.label(
                                        run.finished_at
                                            .as_deref()
                                            .unwrap_or(self.language.text("in progress")),
                                    );
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
