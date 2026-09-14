//! Verification workspace and evidence-review presentation.

use crate::App;
use crate::ui::{StatusSeverity, customer_proof_ready, job_state_badge};
use eframe::egui::{self, RichText};

impl App {
    pub(crate) fn verification_view(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        ui.heading("Verification");
        ui.label(RichText::new("Do not trust a completed process until the destination reconciles with the source.").color(self.theme_colors().text_secondary));
        ui.add_space(12.0);
        ui.group(|ui| {
            ui.heading("Verification and audit report");
            ui.label(RichText::new("The transfer engine is only one part of the migration. This report is the operator-facing proof of what arrived and what still needs attention.").color(self.theme_colors().text_secondary));
            if self.active_project_id().is_some() {
                let proof_ready = self.ui_snapshot.project.as_ref().is_some_and(|project| {
                    customer_proof_ready(
                        project.phase,
                        self.ui_snapshot.mailbox_counts.needs_review,
                        self.ui_snapshot.is_stale(),
                    )
                });
                if ui.button("Export project report…").clicked() { self.report_export_result("Project report", self.export_project_report()); }
                if ui.button("Export project JSON…").clicked() { self.report_export_result("Project JSON", self.export_project_json()); }
                if ui
                    .add_enabled(
                        proof_ready,
                        egui::Button::new("Export customer proof JSON…"),
                    )
                    .on_disabled_hover_text(
                        "Customer proof becomes available after the durable project is Complete, every mailbox is verified, and the state view is current.",
                    )
                    .clicked()
                {
                    self.report_export_result("Customer proof", self.export_customer_proof());
                }
                if ui.button("Export support bundle…").clicked() { self.report_export_result("Support bundle", self.export_support_bundle_dialog()); }
                if ui.button("Export project health…").clicked() { self.report_export_result("Project health export", self.export_project_health()); }
                if let Some(project) = self.ui_snapshot.project.as_ref() {
                    if customer_proof_ready(
                        project.phase,
                        self.ui_snapshot.mailbox_counts.needs_review,
                        self.ui_snapshot.is_stale(),
                    )
                    {
                        ui.label(
                            RichText::new(
                                "Customer proof is ready: the durable project is Complete and no mailbox requires review.",
                            )
                            .color(self.theme_colors().success),
                        );
                    } else {
                        ui.label(
                            RichText::new(
                                "Customer proof remains gated until the durable project is Complete, every mailbox is verified, and the state view is current.",
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
                    ui.heading("Mailbox evidence");
                    ui.label(format!("{verified} of {} verified · {review} require review", mailbox_counts.total, verified = mailbox_counts.verified, review = mailbox_counts.needs_review));
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Search");
                        ui.add(egui::TextEdit::singleline(&mut self.verification_search).hint_text("mailbox or destination").desired_width(220.0));
                        egui::ComboBox::from_id_salt("verification_result_filter")
                            .selected_text(match self.verification_filter.as_str() { "review" => "Needs review", "verified" => "Verified", "difference" => "Differences", _ => "All results" })
                            .show_ui(ui, |ui| {
                                for (value, label) in [("all", "All results"), ("review", "Needs review"), ("verified", "Verified"), ("difference", "Differences")] {
                                    ui.selectable_value(&mut self.verification_filter, value.into(), label);
                                }
                            });
                    });
                    self.refresh_verification_filter_cache();
                    let verification_rows = &self.ui_snapshot.verification_rows;
                    let visible = &self.verification_visible_indices;
                    ui.label(RichText::new(format!("{} visible on page · showing {}–{} of {}", visible.len(), self.verification_offset + 1, (self.verification_offset as usize + verification_rows.len()).min(mailbox_counts.total), mailbox_counts.total)).color(self.theme_colors().text_secondary));
                    egui::ScrollArea::vertical().id_salt("verification_mailbox_list").max_height(360.0).show_rows(ui, 32.0, visible.len(), |ui, visible_rows| {
                        egui::Grid::new("verification_mailboxes").striped(true).min_col_width(140.0).show(ui, |ui| {
                            if visible_rows.start == 0 { ui.strong("Mailbox"); ui.strong("Evidence"); ui.strong("Result"); ui.end_row(); }
                            for row in visible_rows {
                                let mailbox = &verification_rows[visible[row]];
                                let evidence_label = mailbox.evidence.as_ref().map(|(_, evidence, _)| evidence.evidence_level()).unwrap_or("No evidence");
                                let (badge, color) = job_state_badge(&mailbox.job.state, colors);
                                if ui.selectable_label(self.job_id.as_deref() == Some(mailbox.job.id.as_str()), &mailbox.job.destination_mailbox).clicked() { self.job_id = Some(mailbox.job.id.clone()); }
                                ui.label(evidence_label);
                                ui.label(RichText::new(badge).color(color));
                                ui.end_row();
                            }
                        });
                    });
                    ui.horizontal(|ui| {
                        let previous = ui.add_enabled(self.verification_offset > 0, egui::Button::new("← Previous")).clicked();
                        let next = ui.add_enabled(self.verification_offset as usize + verification_rows.len() < mailbox_counts.total, egui::Button::new("Next →")).clicked();
                        if previous { self.verification_offset = self.verification_offset.saturating_sub(200); }
                        else if next { self.verification_offset = self.verification_offset.saturating_add(200); }
                    });
                } else {
                    ui.separator();
                    ui.label(RichText::new("Loading mailbox verification…").color(self.theme_colors().text_secondary));
                }
            }
            let selected_mailbox = self.job_id.as_deref().and_then(|job_id| self.cached_report_mailbox(job_id)).cloned();
            if let Some(mailbox) = selected_mailbox {
                match mailbox.evidence.as_ref() {
                    Some((_, evidence, _)) => {
                        ui.label("Durable mailbox reconciliation");
                        if ui.button("Export verification report…").clicked() { self.report_export_result("Verification report", self.export_verification_report()); }
                        for (label, value) in [("Folders", format!("{} source / {} destination", evidence.source_folders, evidence.destination_folders)), ("Messages", format!("{} source / {} destination", evidence.source_messages, evidence.destination_messages)), ("Bytes", format!("{} source / {} destination", evidence.source_bytes, evidence.destination_bytes)), ("Unmatched", evidence.unmatched_messages.to_string()), ("Failed", evidence.failed_messages.to_string()), ("Evidence level", evidence.evidence_level().into())] {
                            ui.horizontal(|ui| { ui.label(RichText::new(label).strong()); ui.label(value); });
                        }
                    }
                    None => {
                        ui.label(RichText::new("The transfer finished, but no mailbox-level evidence has been captured yet.").color(self.theme_colors().danger));
                    }
                }
                match mailbox.job.state.as_str() {
                    "verification_difference" => {
                        ui.separator();
                        ui.heading("Accept residual difference");
                        ui.label(RichText::new("This records an auditable exception; it does not change the underlying evidence or claim exact equality.").color(self.theme_colors().text_secondary));
                        ui.horizontal(|ui| { ui.label("Operator"); ui.add(egui::TextEdit::singleline(&mut self.verification_exception_operator).desired_width(220.0)); });
                        ui.add(egui::TextEdit::multiline(&mut self.verification_exception_reason).hint_text("Why is this difference acceptable? Include the change-ticket or customer approval reference.").desired_rows(3));
                        let can_accept = !self.verification_exception_operator.trim().is_empty() && !self.verification_exception_reason.trim().is_empty();
                        if ui.add_enabled(can_accept, egui::Button::new("Accept and mark verified with exceptions")).clicked() {
                            match selected_project.as_deref() {
                                Some(project_id) => match self.store.accept_verification_difference(project_id, &mailbox.job.id, &self.verification_exception_operator, &self.verification_exception_reason) {
                                    Ok(()) => { self.set_status("Verification exception recorded durably", StatusSeverity::Success); self.verification_exception_reason.clear(); }
                                    Err(error) => self.set_status(format!("Could not accept verification exception: {error}"), StatusSeverity::Error),
                                },
                                None => self.set_status("No active project selected", StatusSeverity::Warning),
                            }
                        }
                    }
                    "verified_with_exceptions" => {
                        if let Some(acceptance) = mailbox.acceptance.as_ref() {
                            ui.separator();
                            ui.label(RichText::new("Verified with exceptions").strong().color(colors.warning));
                            ui.label(format!("Accepted by {} at {}: {}", acceptance.operator, acceptance.accepted_at, acceptance.reason));
                        }
                    }
                    _ => {}
                }
            } else if self.job_id.is_some() && selected_project.is_some() {
                ui.label(RichText::new("The selected mailbox is not present in the cached project snapshot. Refresh the workspace before viewing or exporting its evidence.").color(self.theme_colors().danger));
            } else {
                ui.label("Run a migration to create a durable mailbox evidence record.");
            }
        });
    }
}
