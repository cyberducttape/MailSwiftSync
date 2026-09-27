//! Confirmation dialogs for destructive or replacing batch actions.

use crate::App;
use eframe::egui::{self, Color32, RichText};
use std::collections::HashSet;

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
                ui.label(format!(
                    "This removes {} mailbox row(s), selection, in-memory passwords, and the durable batch association from this workspace.",
                    self.bulk_jobs.len()
                ));
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
                ui.label(format!(
                    "Importing {path_label} will replace {} current mailbox row(s), selection, in-memory passwords, and the durable batch association.",
                    self.bulk_jobs.len()
                ));
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

            let selected_job_ids = eligible_indices
                .iter()
                .filter_map(|idx| self.bulk_job_ids.get(*idx).cloned())
                .collect::<Vec<_>>();
            let concurrency = self.form.profile.batch_concurrency.clamp(1, 16);

            self.bulk_confirmation_summary = Some(crate::controller::BulkConfirmationSummary {
                eligible_count: eligible_indices.len(),
                deletion_enabled,
                durable_state_error,
                concurrency,
                scope: self.bulk_retry_scope,
            });

            self.bulk_confirmation_identity = Some(crate::controller::BatchConfirmationIdentity {
                selected_job_ids,
                retry_scope: self.bulk_retry_scope,
                execution_mode: self.bulk_mode,
                concurrency,
                deletion_enabled,
            });
        }
        let summary = self
            .bulk_confirmation_summary
            .as_ref()
            .cloned()
            .expect("confirmation summary is initialized above");
        let selected_ids = self.bulk_selected_ids.iter().cloned().collect::<Vec<_>>();
        let visible_selected = self
            .bulk_jobs
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                self.bulk_job_ids
                    .get(*index)
                    .is_some_and(|id| self.bulk_selected_ids.contains(id))
                    && self.mailbox_matches_filter(&self.bulk_jobs[*index])
            })
            .count();
        let hidden_selected = self
            .bulk_selected_ids
            .len()
            .saturating_sub(visible_selected);

        let stored_identity = self.bulk_confirmation_identity.clone();
        let mut open = self.bulk_live_confirm_open;
        let mut close = false;

        egui::Window::new(self.language.text("Confirm live batch migration"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading(
                    RichText::new(self.language.text("This will change destination mailboxes"))
                        .color(self.theme_colors().danger),
                );
                ui.label(format!("{} mailboxes selected", summary.eligible_count));
                ui.label(format!("Selected scope: {} explicit · {} visible · {} hidden by current filter", selected_ids.len(), visible_selected, hidden_selected));
                ui.label(RichText::new("Sample of selected mailboxes:").strong());
                for job_id in selected_ids.iter().take(5) {
                    if let Some(index) = self.bulk_job_ids.iter().position(|id| id == job_id) {
                        let profile = &self.bulk_jobs[index].form.profile;
                        ui.label(format!("• {}: {} → {}", self.bulk_jobs[index].label, profile.source_user, profile.destination_user));
                    }
                }
                if selected_ids.len() > 5 {
                    ui.label(format!("… plus {} more selected", selected_ids.len() - 5));
                }
                if hidden_selected > 0 && ui.button("View all selected").clicked() {
                    self.bulk_search.clear();
                    self.bulk_state_filter = "all".into();
                    close = true;
                }
                if let Some(error) = &summary.durable_state_error {
                    ui.label(RichText::new(error).color(self.theme_colors().danger));
                }
                ui.label(format!("Worker concurrency: {}", summary.concurrency));
                ui.label(
                    RichText::new(format!(
                        "Destination deletion: {}",
                        if summary.deletion_enabled {
                            "ENABLED ⚠"
                        } else {
                            "disabled"
                        }
                    ))
                    .color(if summary.deletion_enabled {
                        self.theme_colors().danger
                    } else {
                        self.theme_colors().text_secondary
                    }),
                );
                ui.label(format!("Scope: {}.", summary.scope.label()));
                ui.label("Each mailbox must already have a matching successful preflight. Source mail is not deleted by default.");
                ui.label(
                    RichText::new(
                        "Review the queue, concurrency, throttles, and exact plans before continuing.",
                    )
                    .color(self.theme_colors().text_secondary),
                );
                ui.horizontal(|ui| {
                    if ui.button(self.language.text("Cancel")).clicked() {
                        close = true;
                    }
                    if ui
                        .add_enabled(
                            summary.durable_state_error.is_none() && summary.eligible_count > 0,
                            egui::Button::new(
                                RichText::new(self.language.text("I understand — start batch")).color(Color32::WHITE),
                            )
                            .fill(self.theme_colors().danger),
                        )
                        .clicked()
                    {
                        let current_concurrency = self.form.profile.batch_concurrency.clamp(1, 16);
                        let identity_matches = stored_identity.as_ref().map_or(false, |stored| {
                            stored.concurrency == current_concurrency
                                && stored.retry_scope == self.bulk_retry_scope
                                && stored.execution_mode == self.bulk_mode
                                && stored.deletion_enabled == summary.deletion_enabled
                        });

                        if identity_matches {
                            close = true;
                            self.bulk_live_confirmed = true;
                            self.start_bulk();
                        } else {
                            close = true;
                            self.bulk_message = "Confirmation stale: concurrency, scope, or settings changed while dialog was open. Review the queue and try again.".into();
                            self.bulk_confirmation_identity = None;
                        }
                    }
                });
            });
        self.bulk_live_confirm_open = open && !close;
        if !self.bulk_live_confirm_open {
            self.bulk_confirmation_summary = None;
            self.bulk_confirmation_identity = None;
        }
    }
}
