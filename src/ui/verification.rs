//! Verification workspace and evidence-review presentation.

use crate::App;
use crate::ui::{StatusSeverity, customer_proof_ready, job_state_badge};
use eframe::egui::{self, RichText};

impl App {
    pub(crate) fn verification_view(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        ui.heading(self.language.text("Verification"));
        ui.label(RichText::new(self.language.text("Do not trust a completed process until the destination reconciles with the source.")).color(self.theme_colors().text_secondary));
        ui.add_space(12.0);
        ui.group(|ui| {
            ui.heading(self.language.text("Verification and audit report"));
            ui.label(RichText::new(self.language.text("The transfer engine is only one part of the migration. This report is the operator-facing proof of what arrived and what still needs attention.")).color(self.theme_colors().text_secondary));
            if self.active_project_id().is_some() {
                let proof_ready = self.ui_snapshot.project.as_ref().is_some_and(|project| {
                    customer_proof_ready(
                        project.phase,
                        self.ui_snapshot.mailbox_counts.needs_review,
                        self.ui_snapshot.is_stale(),
                    )
                });
                if ui.button(self.language.text("Export project report…")).clicked() { self.report_export_result("Project report", self.export_project_report()); }
                if ui.button(self.language.text("Export project JSON…")).clicked() { self.report_export_result("Project JSON", self.export_project_json()); }
                if ui
                    .add_enabled(
                        proof_ready,
                        egui::Button::new(self.language.text("Export customer proof JSON…")),
                    )
                    .on_disabled_hover_text(
                        self.language.text("Customer proof becomes available after the durable project is Complete, every mailbox is verified, and the state view is current."),
                    )
                    .clicked()
                {
                    self.report_export_result("Customer proof", self.export_customer_proof());
                }
                if ui.button(self.language.text("Export support bundle…")).clicked() { self.report_export_result("Support bundle", self.export_support_bundle_dialog()); }
                if ui.button(self.language.text("Export project health…")).clicked() { self.report_export_result("Project health export", self.export_project_health()); }
                if let Some(project) = self.ui_snapshot.project.as_ref() {
                    if customer_proof_ready(
                        project.phase,
                        self.ui_snapshot.mailbox_counts.needs_review,
                        self.ui_snapshot.is_stale(),
                    )
                    {
                        ui.label(
                            RichText::new(
                                self.language.text("Customer proof is ready: the durable project is Complete and no mailbox requires review."),
                            )
                            .color(self.theme_colors().success),
                        );
                    } else {
                        ui.label(
                            RichText::new(
                                self.language.text("Customer proof remains gated until the durable project is Complete, every mailbox is verified, and the state view is current."),
                            )
                            .color(self.theme_colors().warning),
                        );
                    }
                }
            }
            let selected_project = self.active_project_id().map(str::to_owned);
            if selected_project.is_some() {
                if self.ui_snapshot.verification_loaded {
                    let mailbox_counts = self.ui_snapshot.mailbox_counts;
                    ui.separator();
                    ui.heading(self.language.text("Mailbox evidence"));
                    ui.label(self.language.text("{} of {} verified · {} require review").replace("{}", &mailbox_counts.verified.to_string()).replacen("{}", &mailbox_counts.total.to_string(), 1).replacen("{}", &mailbox_counts.needs_review.to_string(), 1));
                    ui.horizontal_wrapped(|ui| {
                        ui.label(self.language.text("Search"));
                        ui.add(egui::TextEdit::singleline(&mut self.verification_search).hint_text(self.language.text("mailbox or destination")).desired_width(220.0));
                        egui::ComboBox::from_id_salt("verification_result_filter")
                            .selected_text(self.language.text(match self.verification_filter.as_str() { "review" => "Needs review", "verified" => "Verified", "difference" => "Differences", _ => "All results" }))
                            .show_ui(ui, |ui| {
                                for (value, label) in [("all", "All results"), ("review", "Needs review"), ("verified", "Verified"), ("difference", "Differences")] {
                                    ui.selectable_value(&mut self.verification_filter, value.into(), self.language.text(label));
                                }
                            });
                    });
                    self.refresh_verification_filter_cache();
                    let verification_rows = &self.ui_snapshot.verification_rows;
                    let visible = &self.verification_visible_indices;
                    ui.label(RichText::new(format!("{} visible on page · showing {}–{} of {}", visible.len(), self.verification_offset + 1, (self.verification_offset as usize + verification_rows.len()).min(mailbox_counts.total), mailbox_counts.total)).color(self.theme_colors().text_secondary));
                    egui::ScrollArea::vertical().id_salt("verification_mailbox_list").max_height(360.0).show_rows(ui, 32.0, visible.len(), |ui, visible_rows| {
                        egui::Grid::new("verification_mailboxes").striped(true).min_col_width(140.0).show(ui, |ui| {
                            if visible_rows.start == 0 { ui.strong(self.language.text("Mailbox")); ui.strong(self.language.text("Evidence")); ui.strong(self.language.text("Result")); ui.end_row(); }
                            for row in visible_rows {
                                let mailbox = &verification_rows[visible[row]];
                                let evidence_label = mailbox.evidence.as_ref().map(|(_, evidence, _)| evidence.verification_outcome().display_label()).unwrap_or("No evidence");
                                let (badge, color) = job_state_badge(&mailbox.job.state, colors);
                                if ui.selectable_label(self.job_id.as_deref() == Some(mailbox.job.id.as_str()), &mailbox.job.destination_mailbox).clicked() { self.job_id = Some(mailbox.job.id.clone()); }
                                ui.label(evidence_label);
                                ui.label(RichText::new(self.language.text(badge)).color(color));
                                ui.end_row();
                            }
                        });
                    });
                    ui.horizontal(|ui| {
                        let previous = ui.add_enabled(self.verification_offset > 0, egui::Button::new(self.language.text("← Previous"))).clicked();
                        let next = ui.add_enabled(self.verification_offset as usize + verification_rows.len() < mailbox_counts.total, egui::Button::new(self.language.text("Next →"))).clicked();
                        if previous { self.verification_offset = self.verification_offset.saturating_sub(200); }
                        else if next { self.verification_offset = self.verification_offset.saturating_add(200); }
                    });
                } else {
                    ui.separator();
                    ui.label(RichText::new(self.language.text("Loading mailbox verification…")).color(self.theme_colors().text_secondary));
                }
            }
            let selected_mailbox = self.job_id.as_deref().and_then(|job_id| self.cached_report_mailbox(job_id)).cloned();
            if let Some(mailbox) = selected_mailbox {
                let assurance = mailbox.assurance();
                ui.separator();
                ui.heading(self.language.text("Assurance"));
                ui.label(if assurance.unresolved {
                    self.language.text("Attention required: this mailbox is not currently safe to close.")
                } else if assurance.transfer_completed && assurance.inventory_reconciled {
                    self.language.text("Transfer and inventory evidence support this mailbox's current state.")
                } else {
                    self.language.text("Assurance is incomplete; review the missing facts before proceeding.")
                });
                for (label, value) in [
                    (self.language.text("Transfer"), if assurance.transfer_completed { self.language.text("completed") } else { self.language.text("not complete") }),
                    (self.language.text("Destination reachable"), match assurance.destination_reachable { Some(true) => self.language.text("confirmed"), Some(false) => self.language.text("failed"), None => self.language.text("unknown") }),
                    (self.language.text("Inventory reconciled"), if assurance.inventory_reconciled { self.language.text("confirmed") } else { self.language.text("not confirmed") }),
                    (self.language.text("Message-level evidence"), if assurance.message_level_evidence { self.language.text("collected") } else { self.language.text("not collected") }),
                    (self.language.text("Differences"), if assurance.differences_found == 0 { self.language.text("none recorded") } else { self.language.text("found") }),
                    (self.language.text("Verification authority"), assurance.verification_authority.as_deref().unwrap_or(self.language.text("unknown"))),
                ] {
                    ui.horizontal(|ui| { ui.label(RichText::new(label).strong()); ui.label(value); });
                }
                match mailbox.evidence.as_ref() {
                    Some((_, evidence, _)) => {
                        ui.label(self.language.text("Durable mailbox reconciliation"));
                        ui.label(
                            RichText::new(
                                self.language.text("Evidence labels describe metadata and aggregate reconciliation; message bodies were not compared."),
                            )
                            .italics()
                            .color(self.theme_colors().text_secondary),
                        );
                        if ui.button(self.language.text("Export verification report…")).clicked() { self.report_export_result("Verification report", self.export_verification_report()); }
                        for (label, value) in [("Verification method", evidence.verification_method().as_str().into()), ("Verification outcome", evidence.verification_outcome().display_label().into()), ("Folders", format!("{} source / {} destination", evidence.source_folders, evidence.destination_folders)), ("Messages", format!("{} source / {} destination", evidence.source_messages, evidence.destination_messages)), ("Bytes", format!("{} source / {} destination", evidence.source_bytes, evidence.destination_bytes)), ("Unresolved", evidence.unresolved_count().map_or_else(|| "unknown".into(), |count| count.to_string())), ("Missing", evidence.missing_count().to_string()), ("Extra", evidence.extra_count().to_string()), ("Modified", evidence.modified_count().to_string()), ("Failed", evidence.failed_messages.to_string()), ("Reason", evidence.verification_reason().unwrap_or("none").into())] {
                            ui.horizontal(|ui| { ui.label(RichText::new(label).strong()); ui.label(value); });
                        }
                    }
                    None => {
                        ui.label(RichText::new(self.language.text("The transfer finished, but no mailbox-level evidence has been captured yet.")).color(self.theme_colors().danger));
                    }
                }
                match mailbox.job.state.as_str() {
                    "verification_difference" => {
                        ui.separator();
                        ui.heading(self.language.text("Accept residual difference"));
                        ui.label(RichText::new(self.language.text("This records an auditable exception; it does not change the underlying evidence or claim exact equality.")).color(self.theme_colors().text_secondary));
                        ui.horizontal(|ui| { ui.label(self.language.text("Operator")); ui.add(egui::TextEdit::singleline(&mut self.verification_exception_operator).desired_width(220.0)); });
                        ui.add(egui::TextEdit::multiline(&mut self.verification_exception_reason).hint_text(self.language.text("Why is this difference acceptable? Include the change-ticket or customer approval reference.")).desired_rows(3));
                        let can_accept = !self.verification_exception_operator.trim().is_empty() && !self.verification_exception_reason.trim().is_empty();
                        if ui.add_enabled(can_accept, egui::Button::new(self.language.text("Accept and mark verified with exceptions"))).clicked() {
                            match selected_project.as_deref() {
                                Some(project_id) => match self.store.accept_verification_difference(project_id, &mailbox.job.id, &self.verification_exception_operator, &self.verification_exception_reason) {
                                    Ok(()) => { self.set_status(self.language.text("Verification exception recorded durably"), StatusSeverity::Success); self.verification_exception_reason.clear(); }
                                    Err(error) => self.set_status(self.language.text("Could not accept verification exception: {error}").replace("{error}", &error.to_string()), StatusSeverity::Error),
                                },
                                None => self.set_status(self.language.text("No active project selected"), StatusSeverity::Warning),
                            }
                        }
                    }
                    "verified_with_exceptions" => {
                        if let Some(acceptance) = mailbox.acceptance.as_ref() {
                            ui.separator();
                            ui.label(RichText::new(self.language.text("Verified with exceptions")).strong().color(colors.warning));
                            ui.label(format!("Accepted by {} at {}: {}", acceptance.operator, acceptance.accepted_at, acceptance.reason));
                        }
                    }
                    _ => {}
                }
            } else if self.job_id.is_some() && selected_project.is_some() {
                ui.label(RichText::new("The selected mailbox is not present in the cached project snapshot. Refresh the workspace before viewing or exporting its evidence.").color(self.theme_colors().danger));
            } else {
                ui.label(self.language.text("Run a migration to create a durable mailbox evidence record."));
            }
        });
    }
}
