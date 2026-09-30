//! Verification workspace and evidence-review presentation.

use crate::App;
use crate::ui::{StatusSeverity, customer_proof_ready, job_state_badge};
use eframe::egui::{self, RichText};

impl App {
    pub(crate) fn verification_view(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        crate::ui::page_header(
            ui,
            self.language.message("ui.verification"),
            self.language.message("ui.do-not-trust-a-completed-process-until-the-destination-reconciles-with-the-source"),
        );
        crate::ui::card(ui, |ui| {
            ui.heading(self.language.message("ui.verification-levels"));
            for key in [
                "ui.verification-level-1-description",
                "ui.verification-level-2-description",
                "ui.verification-level-3-description",
            ] {
                ui.label(self.language.message(key));
            }
        });
        crate::ui::card(ui, |ui| {
            ui.heading(self.language.message("ui.verification-and-audit-report"));
            ui.label(RichText::new(self.language.message("ui.the-transfer-engine-is-only-one-part-of-the-migration-this-report-is-the-op-9798e5abba")).color(self.theme_colors().text_secondary));
            if self.active_project_id().is_some() {
                let proof_ready = self.ui_snapshot.project.as_ref().is_some_and(|project| {
                    customer_proof_ready(
                        project.phase,
                        self.ui_snapshot.mailbox_counts.needs_review,
                        self.ui_snapshot.is_stale(),
                    )
                });
                if ui
                    .button(self.language.message("ui.export-project-report"))
                    .clicked()
                {
                    self.report_export_result("Project report", self.export_project_report());
                }
                if ui
                    .button(self.language.message("ui.export-project-json"))
                    .clicked()
                {
                    self.report_export_result("Project JSON", self.export_project_json());
                }
                if ui
                    .add_enabled(
                        proof_ready,
                        egui::Button::new(self.language.message("ui.export-customer-proof-json")),
                    )
                    .on_disabled_hover_text(
                        self.language.message("ui.customer-proof-becomes-available-after-the-durable-project-is-complete-ever-891f123df0"),
                    )
                    .clicked()
                {
                    self.report_export_result("Customer proof", self.export_customer_proof());
                }
                if ui
                    .button(self.language.message("ui.export-support-bundle"))
                    .clicked()
                {
                    self.report_export_result(
                        "Support bundle",
                        self.export_support_bundle_dialog(),
                    );
                }
                if ui
                    .button(self.language.message("ui.export-project-health"))
                    .clicked()
                {
                    self.report_export_result(
                        "Project health export",
                        self.export_project_health(),
                    );
                }
                if let Some(project) = self.ui_snapshot.project.as_ref() {
                    if customer_proof_ready(
                        project.phase,
                        self.ui_snapshot.mailbox_counts.needs_review,
                        self.ui_snapshot.is_stale(),
                    ) {
                        ui.label(
                            RichText::new(
                                self.language.message("ui.customer-proof-is-ready-the-durable-project-is-complete-and-no-mailbox-requires-review"),
                            )
                            .color(self.theme_colors().success),
                        );
                    } else {
                        ui.label(
                            RichText::new(
                                self.language.message("ui.customer-proof-remains-gated-until-the-durable-project-is-complete-every-ma-b96a6c4f9d"),
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
                    ui.heading(self.language.message("ui.mailbox-evidence"));
                    ui.label(
                        self.language
                            .message("ui.terminal-verification-results-count")
                            .replace("{}", &mailbox_counts.verified.to_string())
                            .replacen("{}", &mailbox_counts.total.to_string(), 1)
                            .replacen("{}", &mailbox_counts.needs_review.to_string(), 1),
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.label(self.language.message("ui.search"));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.verification_search)
                                .hint_text(self.language.message("ui.mailbox-or-destination"))
                                .desired_width(220.0),
                        );
                        egui::ComboBox::from_id_salt("verification_result_filter")
                            .selected_text(self.language.text(
                                match self.verification_filter.as_str() {
                                    "review" => "Needs review",
                                    "verified" => "Verified",
                                    "difference" => "Differences",
                                    _ => "All results",
                                },
                            ))
                            .show_ui(ui, |ui| {
                                for (value, label) in [
                                    ("all", "All results"),
                                    ("review", "Needs review"),
                                    ("verified", "Verified"),
                                    ("difference", "Differences"),
                                ] {
                                    ui.selectable_value(
                                        &mut self.verification_filter,
                                        value.into(),
                                        self.language.text(label),
                                    );
                                }
                            });
                        let previous_reason = self.verification_attention_reason;
                        let selected_reason = self
                            .verification_attention_reason
                            .map(|reason| self.language.text(reason.label()).to_owned())
                            .unwrap_or_else(|| {
                                self.language.message("ui.all-attention-reasons").to_owned()
                            });
                        egui::ComboBox::from_id_salt("verification_attention_reason_filter")
                            .selected_text(selected_reason)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.verification_attention_reason,
                                    None,
                                    self.language.message("ui.all-attention-reasons"),
                                );
                                for reason in [
                                    crate::core::AttentionReason::Interrupted,
                                    crate::core::AttentionReason::VerificationIncomplete,
                                    crate::core::AttentionReason::VerificationDifference,
                                    crate::core::AttentionReason::ProcessIdentityUnverified,
                                    crate::core::AttentionReason::AuthenticationFailed,
                                    crate::core::AttentionReason::TransportFailed,
                                    crate::core::AttentionReason::PolicyBlocked,
                                    crate::core::AttentionReason::ConfigurationInvalid,
                                    crate::core::AttentionReason::CapacityLimited,
                                    crate::core::AttentionReason::MessageRejected,
                                    crate::core::AttentionReason::Unknown,
                                ] {
                                    ui.selectable_value(
                                        &mut self.verification_attention_reason,
                                        Some(reason),
                                        self.language.text(reason.label()),
                                    );
                                }
                            });
                        if previous_reason != self.verification_attention_reason {
                            self.verification_offset = 0;
                            self.verification_cursor = None;
                            self.verification_cursor_stack.clear();
                        }
                    });
                    self.refresh_verification_filter_cache();
                    let filtered_total = self
                        .verification_attention_reason
                        .map(|reason| {
                            self.ui_snapshot
                                .attention_reason_counts
                                .get(reason.as_str())
                                .copied()
                                .unwrap_or_default()
                        })
                        .unwrap_or(mailbox_counts.total);
                    let verification_rows = &self.ui_snapshot.verification_rows;
                    let visible = &self.verification_visible_indices;
                    ui.label(
                        RichText::new(
                            self.language
                                .text("{} visible on page · showing {}–{} of {}")
                                .replacen("{}", &visible.len().to_string(), 1)
                                .replacen("{}", &(self.verification_offset + 1).to_string(), 1)
                                .replacen(
                                    "{}",
                                    &((self.verification_offset as usize
                                        + verification_rows.len())
                                    .min(filtered_total))
                                    .to_string(),
                                    1,
                                )
                                .replacen("{}", &filtered_total.to_string(), 1),
                        )
                        .color(self.theme_colors().text_secondary),
                    );
                    egui::ScrollArea::vertical()
                        .id_salt("verification_mailbox_list")
                        .max_height(360.0)
                        .show_rows(ui, 32.0, visible.len(), |ui, visible_rows| {
                            egui::Grid::new("verification_mailboxes")
                                .striped(true)
                                .min_col_width(140.0)
                                .show(ui, |ui| {
                                    if visible_rows.start == 0 {
                                        ui.strong(self.language.message("ui.mailbox"));
                                        ui.strong(self.language.message("ui.evidence-03867aea"));
                                        ui.strong(self.language.message("ui.result"));
                                        ui.end_row();
                                    }
                                    for row in visible_rows {
                                        let mailbox = &verification_rows[visible[row]];
                                        let evidence_label = mailbox
                                            .evidence
                                            .as_ref()
                                            .map(|(_, evidence, _)| {
                                                format!(
                                                    "{} · {}",
                                                    self.language.message(verification_level_key(
                                                        evidence.verification_method(),
                                                        evidence.verification_outcome(),
                                                    )),
                                                    self.language.text(
                                                        evidence
                                                            .verification_outcome()
                                                            .display_label()
                                                    )
                                                )
                                            })
                                            .unwrap_or_else(|| {
                                                self.language.message("ui.no-evidence").to_owned()
                                            });
                                        let (badge, color) =
                                            job_state_badge(&mailbox.job.state, colors);
                                        if ui
                                            .selectable_label(
                                                self.job_id.as_deref()
                                                    == Some(mailbox.job.id.as_str()),
                                                &mailbox.job.destination_mailbox,
                                            )
                                            .clicked()
                                        {
                                            self.job_id = Some(mailbox.job.id.clone());
                                        }
                                        ui.label(&evidence_label);
                                        ui.label(
                                            RichText::new(self.language.text(badge)).color(color),
                                        );
                                        ui.end_row();
                                    }
                                });
                        });
                    ui.horizontal(|ui| {
                        let previous = ui
                            .add_enabled(
                                self.verification_offset > 0,
                                egui::Button::new(self.language.message("ui.previous")),
                            )
                            .clicked();
                        let next = ui
                            .add_enabled(
                                self.verification_offset as usize + verification_rows.len()
                                    < filtered_total,
                                egui::Button::new(self.language.message("ui.next")),
                            )
                            .clicked();
                        if previous {
                            self.verification_offset = self.verification_offset.saturating_sub(200);
                            self.verification_cursor =
                                self.verification_cursor_stack.pop().flatten();
                        } else if next {
                            if let Some(last_rowid) = self.ui_snapshot.verification_last_rowid {
                                self.verification_cursor_stack
                                    .push(self.verification_cursor);
                                self.verification_cursor = Some(last_rowid);
                            }
                            self.verification_offset = self.verification_offset.saturating_add(200);
                        }
                    });
                } else {
                    ui.separator();
                    ui.label(
                        RichText::new(self.language.message("ui.loading-mailbox-verification"))
                            .color(self.theme_colors().text_secondary),
                    );
                }
            }
            let selected_mailbox = self
                .job_id
                .as_deref()
                .and_then(|job_id| self.cached_report_mailbox(job_id))
                .cloned();
            if let Some(mailbox) = selected_mailbox {
                let assurance = mailbox.assurance();
                ui.separator();
                ui.heading(self.language.message("ui.assurance"));
                ui.label(if assurance.unresolved {
                    self.language
                        .text("Attention required: this mailbox is not currently safe to close.")
                } else if assurance.transfer_completed && assurance.inventory_reconciled {
                    self.language.text(
                        "Transfer and inventory evidence support this mailbox's current state.",
                    )
                } else {
                    self.language.text(
                        "Assurance is incomplete; review the missing facts before proceeding.",
                    )
                });
                for (label, value) in [
                    (
                        self.language.message("ui.transfer"),
                        if assurance.transfer_completed {
                            self.language.message("ui.completed-4ddb3e96")
                        } else {
                            self.language.message("ui.not-complete")
                        },
                    ),
                    (
                        self.language.message("ui.destination-reachable"),
                        match assurance.destination_reachable {
                            Some(true) => self.language.message("ui.confirmed"),
                            Some(false) => self.language.message("ui.failed-5d28a90f"),
                            None => self.language.message("ui.unknown-b23a6a84"),
                        },
                    ),
                    (
                        self.language.message("ui.inventory-reconciled"),
                        if assurance.inventory_reconciled {
                            self.language.message("ui.confirmed")
                        } else {
                            self.language.message("ui.not-confirmed")
                        },
                    ),
                    (
                        self.language.message("ui.message-level-evidence"),
                        if assurance.message_level_evidence {
                            self.language.message("ui.collected")
                        } else {
                            self.language.message("ui.not-collected")
                        },
                    ),
                    (
                        self.language.message("ui.differences"),
                        if assurance.differences_found == 0 {
                            self.language.message("ui.none-recorded")
                        } else {
                            self.language.message("ui.found")
                        },
                    ),
                    (
                        self.language.message("ui.verification-authority"),
                        assurance
                            .verification_authority
                            .as_deref()
                            .unwrap_or(self.language.message("ui.unknown-b23a6a84")),
                    ),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(label).strong());
                        ui.label(value);
                    });
                }
                match mailbox.evidence.as_ref() {
                    Some((_, evidence, _)) => {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(self.language.message("ui.recorded-evidence-level"))
                                    .strong(),
                            );
                            ui.label(self.language.message(verification_level_key(
                                evidence.verification_method(),
                                evidence.verification_outcome(),
                            )));
                        });
                        ui.label(self.language.message("ui.durable-mailbox-reconciliation"));
                        ui.label(
                            RichText::new(
                                self.language.text(match evidence.evidence_scope() {
                                    crate::core::EvidenceScope::BodyHashed => {
                                        "Evidence includes bounded RFC822 body fingerprints from both accounts; provider-specific qualification remains required."
                                    }
                                    crate::core::EvidenceScope::EngineConfirmed
                                    | crate::core::EvidenceScope::AggregateReconciled => {
                                        "Evidence labels describe metadata and aggregate reconciliation; message bodies were not compared."
                                    }
                                }),
                            )
                            .italics()
                            .color(self.theme_colors().text_secondary),
                        );
                        if ui
                            .button(self.language.message("ui.export-verification-report"))
                            .clicked()
                        {
                            self.report_export_result(
                                "Verification report",
                                self.export_verification_report(),
                            );
                        }
                        for (label, value) in [
                            (
                                self.language.message("ui.verification-method"),
                                self.language
                                    .text(evidence.verification_method().as_str())
                                    .to_owned(),
                            ),
                            (
                                self.language.message("ui.verification-outcome"),
                                self.language
                                    .text(evidence.verification_outcome().display_label())
                                    .to_owned(),
                            ),
                            (
                                self.language.message("ui.folders"),
                                format!(
                                    "{} / {}",
                                    evidence.source_folders, evidence.destination_folders
                                ),
                            ),
                            (
                                self.language.message("ui.messages"),
                                format!(
                                    "{} / {}",
                                    evidence.source_messages, evidence.destination_messages
                                ),
                            ),
                            (
                                self.language.message("ui.bytes"),
                                format!(
                                    "{} / {}",
                                    evidence.source_bytes, evidence.destination_bytes
                                ),
                            ),
                            (
                                self.language.message("ui.unresolved-36afca80"),
                                evidence.unresolved_count().map_or_else(
                                    || self.language.message("ui.unknown-b23a6a84").to_owned(),
                                    |count| count.to_string(),
                                ),
                            ),
                            (
                                self.language.message("ui.missing"),
                                evidence.missing_count().to_string(),
                            ),
                            (
                                self.language.message("ui.extra"),
                                evidence.extra_count().to_string(),
                            ),
                            (
                                self.language.message("ui.modified"),
                                evidence.modified_count().to_string(),
                            ),
                            (
                                self.language.message("ui.failed-031a8f0f"),
                                evidence.failed_messages.to_string(),
                            ),
                            (
                                self.language.message("ui.reason"),
                                evidence.verification_reason().map_or_else(
                                    || self.language.message("ui.none").to_owned(),
                                    ToOwned::to_owned,
                                ),
                            ),
                        ] {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(label).strong());
                                ui.label(value);
                            });
                        }
                    }
                    None => {
                        ui.label(RichText::new(self.language.message("ui.the-transfer-finished-but-no-mailbox-level-evidence-has-been-captured-yet")).color(self.theme_colors().danger));
                    }
                }
                match mailbox.job.state.as_str() {
                    "verification_difference" => {
                        ui.separator();
                        ui.heading(self.language.message("ui.accept-residual-difference"));
                        ui.label(RichText::new(self.language.message("ui.this-records-an-auditable-exception-it-does-not-change-the-underlying-evide-d1c2d9bc90")).color(self.theme_colors().text_secondary));
                        ui.horizontal(|ui| {
                            ui.label(self.language.message("ui.operator"));
                            ui.add(
                                egui::TextEdit::singleline(
                                    &mut self.verification_exception_operator,
                                )
                                .desired_width(220.0),
                            );
                        });
                        ui.add(egui::TextEdit::multiline(&mut self.verification_exception_reason).hint_text(self.language.message("ui.why-is-this-difference-acceptable-include-the-change-ticket-or-customer-app-f7056f09b1")).desired_rows(3));
                        let can_accept = !self.verification_exception_operator.trim().is_empty()
                            && !self.verification_exception_reason.trim().is_empty();
                        if ui
                            .add_enabled(
                                can_accept,
                                egui::Button::new(
                                    self.language
                                        .text("Accept and mark verified with exceptions"),
                                ),
                            )
                            .clicked()
                        {
                            match selected_project.as_deref() {
                                Some(project_id) => match self.accept_verification_difference(
                                    project_id,
                                    &mailbox.job.id,
                                    &self.verification_exception_operator,
                                    &self.verification_exception_reason,
                                ) {
                                    Ok(()) => {
                                        self.set_status(
                                            self.language
                                                .text("Verification exception recorded durably"),
                                            StatusSeverity::Success,
                                        );
                                        self.verification_exception_reason.clear();
                                    }
                                    Err(error) => self.set_status(
                                        self.language
                                            .text(
                                                "Could not accept verification exception: {error}",
                                            )
                                            .replace("{error}", &error.to_string()),
                                        StatusSeverity::Error,
                                    ),
                                },
                                None => self.set_status(
                                    self.language.message("ui.no-active-project-selected"),
                                    StatusSeverity::Warning,
                                ),
                            }
                        }
                    }
                    "verified_with_exceptions" => {
                        if let Some(acceptance) = mailbox.acceptance.as_ref() {
                            ui.separator();
                            ui.label(
                                RichText::new(
                                    self.language
                                        .message("ui.verified-with-exceptions-7ec28107"),
                                )
                                .strong()
                                .color(colors.warning),
                            );
                            ui.label(
                                self.language
                                    .text("Accepted by {operator} at {time}: {reason}")
                                    .replace("{operator}", &acceptance.operator)
                                    .replace("{time}", &acceptance.accepted_at)
                                    .replace("{reason}", &acceptance.reason),
                            );
                        }
                    }
                    _ => {}
                }
            } else if self.job_id.is_some() && selected_project.is_some() {
                ui.label(RichText::new(self.language.message("ui.the-selected-mailbox-is-not-present-in-the-cached-project-snapshot-refresh-2b6e345d2f")).color(self.theme_colors().danger));
            } else {
                ui.label(
                    self.language
                        .text("Run a migration to create a durable mailbox evidence record."),
                );
            }
        });
    }
}

fn verification_level_key(
    method: crate::core::VerificationMethod,
    outcome: crate::core::VerificationOutcome,
) -> &'static str {
    if matches!(
        outcome,
        crate::core::VerificationOutcome::Incomplete | crate::core::VerificationOutcome::Failed
    ) {
        return "ui.verification-incomplete-no-level";
    }
    match method {
        crate::core::VerificationMethod::AggregateEngine
        | crate::core::VerificationMethod::NativeDovecot => "ui.verification-level-1",
        crate::core::VerificationMethod::MetadataReconciliation => "ui.verification-level-2",
        crate::core::VerificationMethod::BodyHash => "ui.verification-level-3",
    }
}

#[cfg(test)]
mod tests {
    use super::verification_level_key;
    use crate::core::{VerificationMethod, VerificationOutcome};

    #[test]
    fn evidence_methods_map_to_precise_operator_levels() {
        assert_eq!(
            verification_level_key(
                VerificationMethod::AggregateEngine,
                VerificationOutcome::ExactMetadataMatch
            ),
            "ui.verification-level-1"
        );
        assert_eq!(
            verification_level_key(
                VerificationMethod::NativeDovecot,
                VerificationOutcome::ExactMetadataMatch
            ),
            "ui.verification-level-1"
        );
        assert_eq!(
            verification_level_key(
                VerificationMethod::MetadataReconciliation,
                VerificationOutcome::ExactMetadataMatch
            ),
            "ui.verification-level-2"
        );
        assert_eq!(
            verification_level_key(
                VerificationMethod::BodyHash,
                VerificationOutcome::ExactBodyMatch
            ),
            "ui.verification-level-3"
        );
        assert_eq!(
            verification_level_key(
                VerificationMethod::MetadataReconciliation,
                VerificationOutcome::Incomplete
            ),
            "ui.verification-incomplete-no-level"
        );
    }
}
