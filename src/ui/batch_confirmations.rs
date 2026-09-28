//! Confirmation dialogs for destructive or replacing batch actions.

use crate::App;
use eframe::egui::{self, Color32, RichText};

impl App {
    pub(crate) fn bulk_clear_confirmation(&mut self, ctx: &egui::Context) {
        if !self.bulk_clear_confirm_open || self.running() {
            return;
        }
        let mut open = self.bulk_clear_confirm_open;
        let mut clear = false;
        let mut close_requested = false;
        egui::Window::new(self.language.text("Clear mailbox queue?"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading(self.language.text("Discard the current queue?"));
                ui.label(
                    self.language
                        .text("This removes {} mailbox row(s), selection, in-memory passwords, and the durable batch association from this workspace.")
                        .replace("{}", &self.bulk_jobs.len().to_string()),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button(self.language.text("Keep queue")).clicked() {
                        close_requested = true;
                    }
                    if ui.button(self.language.text("Clear queue")).clicked() {
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
        egui::Window::new(self.language.text("Replace mailbox queue?"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading(self.language.text("Replace the current queue?"));
                ui.label(
                    self.language
                        .text("Importing {} will replace {} current mailbox row(s), selection, in-memory passwords, and the durable batch association.")
                        .replace("{}", &path_label)
                        .replacen("{}", &self.bulk_jobs.len().to_string(), 1),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button(self.language.text("Keep current queue")).clicked() {
                        close_requested = true;
                    }
                    if ui.button(self.language.text("Replace queue")).clicked() {
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

    pub(crate) fn bulk_live_confirmation(&mut self, ctx: &egui::Context) {
        if !self.bulk_live_confirm_open {
            return;
        }
        if self.bulk_confirmation_summary.is_none() {
            let rows = self
                .bulk_jobs
                .iter()
                .enumerate()
                .filter_map(|(index, job)| {
                    let job_id = self.bulk_job_ids.get(index)?;
                    Some(crate::controller::BatchActionRow {
                        id: job_id,
                        selected: self.bulk_row_is_selected(index),
                        visible: self.mailbox_matches_filter(job),
                        durable_state: Some(job.state.as_str()),
                        destructive: job.defaults.profile.delete2,
                    })
                })
                .collect::<Vec<_>>();
            let concurrency = self.form.profile.batch_concurrency.clamp(1, 16);
            let plan = crate::controller::build_batch_action_plan(
                &rows,
                self.bulk_retry_scope,
                concurrency,
                self.bulk_mode,
            );
            let selected_job_ids = rows
                .iter()
                .filter(|row| row.selected)
                .map(|row| row.id.to_owned())
                .collect::<Vec<_>>();

            self.bulk_confirmation_summary = Some(plan.clone());

            self.bulk_confirmation_identity = Some(crate::controller::BatchConfirmationIdentity {
                selected_job_ids,
                retry_scope: self.bulk_retry_scope,
                execution_mode: self.bulk_mode,
                concurrency,
                deletion_enabled: plan.destructive_count > 0,
                action_plan_hash: plan.identity_hash,
            });
        }
        let summary = self
            .bulk_confirmation_summary
            .as_ref()
            .cloned()
            .expect("confirmation summary is initialized above");
        let selected_ids = self.bulk_selected_ids.iter().cloned().collect::<Vec<_>>();

        let stored_identity = self.bulk_confirmation_identity.clone();
        let mut close = false;

        let response = egui::Modal::new(egui::Id::new("live_batch_migration_confirmation")).show(ctx, |ui| {
                ui.heading(
                    RichText::new(self.language.text("This will change destination mailboxes"))
                        .color(self.theme_colors().danger),
                );
                ui.label(
                    self.language
                        .text("{} eligible of {} explicitly selected · {} blocked")
                        .replace("{}", &summary.eligible_count.to_string())
                        .replacen("{}", &summary.explicit_selection_count.to_string(), 1)
                        .replacen("{}", &summary.blocked_count.to_string(), 1),
                );
                ui.label(
                    self.language
                        .text("Selected scope: {} explicit · {} visible · {} hidden by current filter")
                        .replace("{}", &summary.explicit_selection_count.to_string())
                        .replacen(
                            "{}",
                            &summary
                                .explicit_selection_count
                                .saturating_sub(summary.hidden_selection_count)
                                .to_string(),
                            1,
                        )
                        .replacen("{}", &summary.hidden_selection_count.to_string(), 1),
                );
                ui.label(
                    RichText::new(self.language.text("Sample of selected mailboxes:")).strong(),
                );
                for job_id in selected_ids.iter().take(5) {
                    if let Some(index) = self.bulk_job_ids.iter().position(|id| id == job_id) {
                        let profile = &self.bulk_jobs[index].defaults.profile;
                        ui.label(
                            self.language
                                .text("• {}: {} → {}")
                                .replace("{}", &self.bulk_jobs[index].label)
                                .replacen("{}", &profile.source_user, 1)
                                .replacen("{}", &profile.destination_user, 1),
                        );
                    }
                }
                if selected_ids.len() > 5 {
                    ui.label(
                        self.language
                            .text("… plus {} more selected")
                            .replace("{}", &(selected_ids.len() - 5).to_string()),
                    );
                }
                if summary.hidden_selection_count > 0
                    && ui
                        .button(self.language.text("View all selected"))
                        .clicked()
                {
                    self.bulk_search.clear();
                    self.bulk_state_filter = "all".into();
                    close = true;
                }
                for reason in &summary.blocked_reasons {
                    ui.label(RichText::new(reason).color(self.theme_colors().danger));
                }
                ui.label(
                    self.language
                        .text("Worker concurrency: {}")
                        .replace("{}", &summary.concurrency.to_string()),
                );
                ui.label(
                    RichText::new(
                        self.language
                            .text("Destination deletion: {}")
                            .replace(
                                "{}",
                                self.language.text(if summary.destructive_count > 0 {
                                    "ENABLED ⚠"
                                } else {
                                    "disabled"
                                }),
                            ),
                    )
                    .color(if summary.destructive_count > 0 {
                        self.theme_colors().danger
                    } else {
                        self.theme_colors().text_secondary
                    }),
                );
                ui.label(
                    self.language
                        .text("Scope: {}.")
                        .replace("{}", summary.retry_scope.label()),
                );
                ui.label(
                    self.language
                        .text("Plan identity: {}")
                        .replace("{}", &summary.identity_hash),
                );
                ui.label(self.language.text(
                    "Each mailbox must already have a matching successful preflight. Source mail is not deleted by default.",
                ));
                ui.label(
                    RichText::new(
                        self.language.text(
                            "Review the queue, concurrency, throttles, and exact plans before continuing.",
                        ),
                    )
                    .color(self.theme_colors().text_secondary),
                );
                ui.horizontal(|ui| {
                    let cancel = ui.button(self.language.text("Cancel"));
                    if !self.bulk_live_confirm_focus_requested {
                        cancel.request_focus();
                        self.bulk_live_confirm_focus_requested = true;
                    }
                    if cancel.clicked() {
                        close = true;
                    }
                    if ui
                        .add_enabled(
                            summary.blocked_count == 0 && summary.eligible_count > 0,
                            egui::Button::new(
                                RichText::new(self.language.text("I understand — start batch")).color(Color32::WHITE),
                            )
                            .fill(self.theme_colors().danger),
                        )
                        .clicked()
                    {
                        let current_concurrency = self.form.profile.batch_concurrency.clamp(1, 16);
                        let identity_matches = stored_identity.as_ref().is_some_and(|stored| {
                            stored.concurrency == current_concurrency
                                && stored.retry_scope == self.bulk_retry_scope
                                && stored.execution_mode == self.bulk_mode
                                && stored.deletion_enabled == (summary.destructive_count > 0)
                                && stored.action_plan_hash == summary.identity_hash
                        });

                        if identity_matches {
                            close = true;
                            self.bulk_live_confirmed = true;
                            self.start_bulk();
                        } else {
                            close = true;
                            self.bulk_message = self
                                .language
                                .text("Confirmation stale: concurrency, scope, or settings changed while dialog was open. Review the queue and try again.")
                                .into();
                            self.bulk_confirmation_identity = None;
                        }
                    }
                });
            });
        self.bulk_live_confirm_open = !(close || response.should_close());
        if !self.bulk_live_confirm_open {
            self.bulk_live_confirm_focus_requested = false;
            self.bulk_confirmation_summary = None;
            self.bulk_confirmation_identity = None;
        }
    }
}
