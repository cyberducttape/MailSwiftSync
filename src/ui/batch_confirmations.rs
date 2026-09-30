//! Confirmation dialogs for destructive or replacing batch actions.

use crate::App;
use crate::atomic_artifact::write_private_atomic;
use crate::controller::batch_admission::{apply_keyring_id, selection_value};
use eframe::egui::{self, Color32, RichText};

impl App {
    pub(crate) fn bulk_clear_confirmation(&mut self, ctx: &egui::Context) {
        let modal_id = egui::Id::new("clear_mailbox_queue_confirmation");
        if !self.bulk_clear_confirm_open || self.running() {
            crate::ui::reset_initial_focus(ctx, modal_id);
            return;
        }
        let mut clear = false;
        let mut close_requested = false;
        // A true modal: it blocks the page behind it, starts on the safe
        // choice, and Escape or a click outside keeps the queue.
        let response = egui::Modal::new(modal_id).show(ctx, |ui| {
            ui.heading(self.language.text("Clear mailbox queue?"));
            ui.label(
                self.language
                    .text("This removes {} mailbox row(s), selection, in-memory passwords, and the durable batch association from this workspace.")
                    .replace("{}", &self.bulk_jobs.len().to_string()),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let keep = ui.button(self.language.text("Keep queue"));
                crate::ui::focus_on_open(ui, &keep, modal_id);
                if keep.clicked() {
                    close_requested = true;
                }
                if ui
                    .button(
                        RichText::new(self.language.text("Clear queue"))
                            .color(self.theme_colors().danger),
                    )
                    .clicked()
                {
                    clear = true;
                    close_requested = true;
                }
            });
        });
        if close_requested || response.should_close() {
            self.bulk_clear_confirm_open = false;
            crate::ui::reset_initial_focus(ctx, modal_id);
        }
        if clear {
            self.clear_bulk_queue();
        }
    }

    pub(crate) fn bulk_import_confirmation(&mut self, ctx: &egui::Context) {
        let modal_id = egui::Id::new("replace_mailbox_queue_confirmation");
        if self.pending_bulk_import.is_none() || self.running() {
            crate::ui::reset_initial_focus(ctx, modal_id);
            return;
        }
        let path_label = self
            .pending_bulk_import
            .as_deref()
            .and_then(std::path::Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("the selected file")
            .to_owned();
        let mut close_requested = false;
        let mut replace = false;
        let response = egui::Modal::new(modal_id).show(ctx, |ui| {
            ui.heading(self.language.text("Replace mailbox queue?"));
            ui.label(
                self.language
                    .text("Importing {} will replace {} current mailbox row(s), selection, in-memory passwords, and the durable batch association.")
                    .replace("{}", &path_label)
                    .replacen("{}", &self.bulk_jobs.len().to_string(), 1),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let keep = ui.button(self.language.text("Keep current queue"));
                crate::ui::focus_on_open(ui, &keep, modal_id);
                if keep.clicked() {
                    close_requested = true;
                }
                if ui
                    .button(
                        RichText::new(self.language.text("Replace queue"))
                            .color(self.theme_colors().danger),
                    )
                    .clicked()
                {
                    replace = true;
                    close_requested = true;
                }
            });
        });
        if close_requested || response.should_close() {
            crate::ui::reset_initial_focus(ctx, modal_id);
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
            // Use the same complete row projection as the mailbox cockpit.
            // In particular, selected IDs that are no longer present in the
            // queue remain visible to the planner as blocked/missing rather
            // than disappearing from the confirmation scope.
            let plan = self.current_batch_action_plan(self.bulk_mode, self.bulk_retry_scope);
            let selected_job_ids = self.bulk_selected_ids.iter().cloned().collect::<Vec<_>>();

            self.bulk_confirmation_summary = Some(plan.clone());

            self.bulk_confirmation_identity = Some(crate::controller::BatchConfirmationIdentity {
                selected_job_ids,
                retry_scope: self.bulk_retry_scope,
                execution_mode: self.bulk_mode,
                concurrency: plan.concurrency,
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
                        let job = &self.bulk_jobs[index];
                        ui.label(
                            self.language
                                .text("• {}: {} → {}")
                                .replace("{}", &job.label)
                                .replacen("{}", &job.source_user, 1)
                                .replacen("{}", &job.destination_user, 1),
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
                let removes_destination_state = summary.destructive_count > 0;
                if removes_destination_state {
                    ui.label(
                        RichText::new(
                            self.language
                                .text("⚠ {} selected mailbox(es) use destination mirror or --delete2: destination-only messages/mailboxes may be removed or replaced.")
                                .replace("{}", &summary.destructive_count.to_string()),
                        )
                        .strong()
                        .color(self.theme_colors().danger),
                    );
                    ui.checkbox(
                        &mut self.bulk_destination_loss_acknowledged,
                        self.language.text(
                            "I confirm destination-only mail may be removed for these mailboxes",
                        ),
                    );
                } else {
                    ui.label(
                        RichText::new(self.language.text(
                            "Destination-only messages and mailboxes are kept for every selected mailbox.",
                        ))
                        .color(self.theme_colors().text_secondary),
                    );
                }
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
                            summary.blocked_count == 0
                                && summary.eligible_count > 0
                                && (summary.destructive_count == 0
                                    || self.bulk_destination_loss_acknowledged),
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
            self.bulk_destination_loss_acknowledged = false;
        }
    }

    /// Write the explicit selection as secret-free JSON for review outside
    /// the application. An empty selection exports nothing, never "all".
    pub(crate) fn export_bulk_selection(&self) -> Result<(), String> {
        if self.bulk_selected_ids.is_empty() {
            return Err(self
                .language
                .text("Select one or more rows to export.")
                .into());
        }
        let scope = crate::controller::SelectionScope::Explicit(self.bulk_selected_ids.clone());
        let value = selection_value(&self.bulk_jobs, &scope, &self.bulk_job_ids);
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-batch-selection.json")
            .save_file()
            .ok_or_else(|| {
                self.language
                    .text("Batch selection export cancelled.")
                    .to_owned()
            })?;
        let report = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
        write_private_atomic(&path, &report).map_err(|error| error.to_string())
    }

    /// Reference an existing OS-keyring entry from every row that has
    /// neither a session password nor a credential ID on that side. The
    /// secret itself is never copied into the queue.
    pub(crate) fn apply_bulk_keyring_id(&mut self, source: bool) {
        let value = if source {
            self.bulk_source_keyring_apply.trim().to_owned()
        } else {
            self.bulk_destination_keyring_apply.trim().to_owned()
        };
        if value.is_empty() {
            self.bulk_message = self
                .language
                .text("Enter a keyring ID before applying it.")
                .to_owned();
            return;
        }
        let applied = apply_keyring_id(&mut self.bulk_jobs, &value, source);
        self.bulk_jobs_generation = self.bulk_jobs_generation.wrapping_add(1);
        self.bulk_message = self
            .language
            .text("Applied the keyring ID to {} row(s) without a credential reference.")
            .replace("{}", &applied.to_string());
    }
}
