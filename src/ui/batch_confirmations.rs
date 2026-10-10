//! Confirmation dialogs for destructive or replacing batch actions.

use crate::App;
use crate::atomic_artifact::write_private_atomic;
use crate::controller::batch_admission::selection_value;
use eframe::egui::{self, Color32, RichText};

impl App {
    pub(crate) fn bulk_plaintext_import_confirmation(&mut self, ctx: &egui::Context) {
        let modal_id = egui::Id::new("plaintext_mailbox_import_confirmation");
        if !crate::bulk_import::plaintext_secret_import_enabled()
            || self.pending_plaintext_import.is_none()
            || self.running()
        {
            crate::ui::reset_initial_focus(ctx, modal_id);
            return;
        }
        let path_label = self
            .pending_plaintext_import
            .as_deref()
            .and_then(std::path::Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("the selected file")
            .to_owned();
        let mut acknowledged = false;
        let mut cancel = false;
        let mut checkbox = self.plaintext_import_dialog_acknowledged;
        let response = egui::Modal::new(modal_id).show(ctx, |ui| {
            let modal_heading =
                ui.heading(self.language.message("ui.plaintext-import-warning-title"));
            crate::ui::name_modal(ui, &modal_heading);
            ui.label(
                self.language
                    .message("ui.plaintext-import-warning-body")
                    .replace("{}", &path_label),
            );
            ui.add_space(8.0);
            ui.checkbox(
                &mut checkbox,
                self.language.message("ui.plaintext-import-warning-ack"),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let cancel_button = ui.button(self.language.message("ui.cancel"));
                crate::ui::focus_on_open(ui, &cancel_button, modal_id);
                cancel |= cancel_button.clicked();
                if ui
                    .add_enabled(
                        checkbox,
                        egui::Button::new(self.language.message("ui.continue-import")),
                    )
                    .clicked()
                {
                    acknowledged = true;
                }
            });
        });
        self.plaintext_import_dialog_acknowledged = checkbox;
        if cancel || response.should_close() {
            self.pending_plaintext_import = None;
            self.plaintext_import_dialog_acknowledged = false;
            self.bulk_message = self
                .language
                .message("ui.plaintext-import-cancelled")
                .into();
            crate::ui::reset_initial_focus(ctx, modal_id);
        } else if acknowledged {
            crate::ui::reset_initial_focus(ctx, modal_id);
            self.bulk_plaintext_import_acknowledged = true;
            self.plaintext_import_dialog_acknowledged = false;
            if let Some(path) = self.pending_plaintext_import.take() {
                self.request_bulk_import_after_ack(path);
            }
        }
    }

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
            let modal_heading = ui.heading(self.language.message("ui.clear-mailbox-queue"));
crate::ui::name_modal(ui, &modal_heading);
            ui.label(
                self.language
                    .text("This removes {} mailbox row(s), selection, in-memory passwords, and the durable batch association from this workspace.")
                    .replace("{}", &self.queue.len().to_string()),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let keep = ui.button(self.language.message("ui.keep-queue"));
                crate::ui::focus_on_open(ui, &keep, modal_id);
                if keep.clicked() {
                    close_requested = true;
                }
                if ui
                    .button(
                        RichText::new(self.language.message("ui.clear-queue"))
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
            let modal_heading = ui.heading(self.language.message("ui.replace-mailbox-queue"));
crate::ui::name_modal(ui, &modal_heading);
            ui.label(
                self.language
                    .text("Importing {} will replace {} current mailbox row(s), selection, in-memory passwords, and the durable batch association.")
                    .replace("{}", &path_label)
                    .replacen("{}", &self.queue.len().to_string(), 1),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let keep = ui.button(self.language.message("ui.keep-current-queue"));
                crate::ui::focus_on_open(ui, &keep, modal_id);
                if keep.clicked() {
                    close_requested = true;
                }
                if ui
                    .button(
                        RichText::new(self.language.message("ui.replace-queue"))
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
            } else {
                self.bulk_plaintext_import_acknowledged = false;
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
            self.bulk_confirmation_summary = Some(plan.clone());

            self.bulk_confirmation_identity = Some(crate::controller::BatchConfirmationIdentity {
                retry_scope: self.bulk_retry_scope,
                execution_mode: self.bulk_mode,
                concurrency: plan.concurrency,
                launch_starts_per_second:
                    crate::migration_plan::effective_batch_process_starts_per_second(
                        self.form.profile.batch_process_starts_per_second,
                    ),
                max_messages_per_second: self.form.profile.max_messages_per_second,
                max_bytes_per_second: self.form.profile.max_bytes_per_second,
                deletion_enabled: plan.destructive_count > 0,
                action_plan_hash: plan.identity_hash,
            });
        }
        let Some(summary) = self.bulk_confirmation_summary.as_ref().cloned() else {
            // Keep the destructive action fail-closed if another UI event
            // invalidates the derived summary between frames.
            self.bulk_live_confirm_open = false;
            self.bulk_confirmation_identity = None;
            self.bulk_message = self
                .language
                .message("ui.batch-confirmation-summary-unavailable")
                .into();
            return;
        };
        let selected_count = self.bulk_selection_count();
        let selected_samples = self
            .queue
            .project_id()
            .and_then(|project_id| {
                crate::controller::queue::selected_rows(
                    &self.store,
                    project_id,
                    |id| self.bulk_is_selected(id),
                    5,
                )
                .ok()
            })
            .unwrap_or_default()
            .into_iter()
            .map(|row| (row.label, row.source_user, row.destination_user))
            .collect::<Vec<_>>();
        // Fail safe: an unreadable queue is treated as a possible collision
        // so the acknowledgement is still requested.
        let ambiguous_case_collision = self.queue.project_id().is_none_or(|project_id| {
            crate::controller::queue::selected_case_collision(&self.store, project_id, |id| {
                self.bulk_is_selected(id)
            })
            .unwrap_or(true)
        });

        // Cutover readiness facts for the whole queue. An unreadable queue is
        // shown as unknown rather than as zero outstanding work.
        let queue_review = self.bulk_queue_summary().ok();

        let stored_identity = self.bulk_confirmation_identity.clone();
        let mut close = false;

        let response = egui::Modal::new(egui::Id::new("live_batch_migration_confirmation")).show(ctx, |ui| {
                let modal_heading = ui.heading(
                    RichText::new(self.language.message("ui.this-will-change-destination-mailboxes"))
                        .color(self.theme_colors().danger),
                );
crate::ui::name_modal(ui, &modal_heading);
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
                    RichText::new(self.language.message("ui.sample-of-selected-mailboxes")).strong(),
                );
                for (label, source, destination) in &selected_samples {
                    ui.label(
                        self.language
                            .text("• {}: {} → {}")
                            .replace("{}", label)
                            .replacen("{}", source, 1)
                            .replacen("{}", destination, 1),
                    );
                }
                if selected_count > 5 {
                    ui.label(
                        self.language
                            .text("… plus {} more selected")
                            .replace("{}", &(selected_count - 5).to_string()),
                    );
                }
                if summary.hidden_selection_count > 0
                    && ui
                        .button(self.language.message("ui.view-all-selected"))
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
                match queue_review {
                    Some(queue) => {
                        let outstanding = queue.attention
                            + queue.failed
                            + queue.cancelled
                            + queue.verification_difference
                            + queue.delta_required;
                        ui.label(
                            RichText::new(
                                self.language
                                    .text("Outstanding in this queue: {} need attention · {} verification differences · {} delta required")
                                    .replace("{}", &(queue.attention + queue.failed + queue.cancelled).to_string())
                                    .replacen("{}", &queue.verification_difference.to_string(), 1)
                                    .replacen("{}", &queue.delta_required.to_string(), 1),
                            )
                            .color(if outstanding > 0 {
                                self.theme_colors().warning
                            } else {
                                self.theme_colors().text_secondary
                            }),
                        );
                    }
                    None => {
                        ui.label(
                            RichText::new(self.language.text(
                                "Outstanding queue review could not be read; resolve the queue error before a cutover pass.",
                            ))
                            .color(self.theme_colors().danger),
                        );
                    }
                }
                ui.label(
                    RichText::new(self.language.text(
                        "Destination capacity is not checked per mailbox in batch mode; confirm quota headroom with each provider before the final delta.",
                    ))
                    .color(self.theme_colors().text_secondary),
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
                if ambiguous_case_collision {
                    ui.label(
                        RichText::new(self.language.text(
                            "⚠ Selected destination names differ only by case on a provider whose account-name case rules are unknown. They may be the same mailbox; verify the provider identities before proceeding. Acknowledged case-only aliases must run at concurrency 1.",
                        ))
                        .strong()
                        .color(self.theme_colors().warning),
                    );
                    ui.checkbox(
                        &mut self.bulk_destination_case_acknowledged,
                        self.language.text(
                            "I reviewed these destination accounts and acknowledge the possible identity collision",
                        ),
                    );
                } else {
                    self.bulk_destination_case_acknowledged = false;
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
                    let cancel = ui.button(self.language.message("ui.cancel"));
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
                                    || self.bulk_destination_loss_acknowledged)
                                && (!ambiguous_case_collision
                                    || (self.bulk_destination_case_acknowledged
                                        && summary.concurrency == 1)),
                            egui::Button::new(
                                RichText::new(self.language.message("ui.i-understand-start-batch")).color(Color32::WHITE),
                            )
                            .fill(self.theme_colors().danger),
                        )
                        .clicked()
                    {
                        let current_concurrency = crate::migration_plan::effective_batch_concurrency(
                            self.form.profile.batch_concurrency,
                        );
                        let identity_matches = stored_identity.as_ref().is_some_and(|stored| {
                            stored.concurrency == current_concurrency
                                && stored.launch_starts_per_second
                                    == crate::migration_plan::effective_batch_process_starts_per_second(
                                        self.form.profile.batch_process_starts_per_second,
                                    )
                                && stored.max_messages_per_second
                                    == self.form.profile.max_messages_per_second
                                && stored.max_bytes_per_second
                                    == self.form.profile.max_bytes_per_second
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
            self.bulk_destination_case_acknowledged = false;
        }
    }

    /// Write the explicit selection as secret-free JSON for review outside
    /// the application. An empty selection exports nothing, never "all".
    pub(crate) fn export_bulk_selection(&self) -> Result<(), String> {
        if self.bulk_selection_is_empty() {
            return Err(self
                .language
                .text("Select one or more rows to export.")
                .into());
        }
        let rows = match self.queue.project_id() {
            Some(project_id) => crate::controller::queue::selected_rows(
                &self.store,
                project_id,
                |id| self.bulk_is_selected(id),
                usize::MAX,
            )?,
            None => Vec::new(),
        };
        let value = selection_value(&rows);
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
        let Some(project_id) = self.queue.project_id().map(str::to_owned) else {
            return;
        };
        match crate::controller::queue::apply_keyring_to_queue(
            &self.store,
            &project_id,
            &value,
            source,
            &self.queue.session_secrets,
        ) {
            Ok(applied) => {
                self.mark_bulk_jobs_changed();
                self.bulk_message = self
                    .language
                    .text("Applied the keyring ID to {} row(s) without a credential reference.")
                    .replace("{}", &applied.to_string());
            }
            Err(error) => self.bulk_message = error,
        }
    }
}
