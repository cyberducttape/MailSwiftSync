//! Shared Overview workflow presentation.

use crate::App;
use crate::core;
use crate::migration_plan::completeness as plan_completeness;
use crate::ui::confidence::{
    ConfidenceInputs, ConfidenceSection, ConfidenceState, migration_confidence,
};
use crate::ui::status::RecommendedAction;
use crate::ui::{StatusSeverity, WorkspaceView};
use crate::ui::{customer_proof_ready, recommended_workspace_action};
use eframe::egui::{self, RichText};

impl App {
    pub(crate) fn overview_view(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        crate::ui::page_header(
            ui,
            self.language.message("ui.migration-overview"),
            self.language
                .text("A calm, evidence-led workspace for moving mailboxes safely."),
        );
        if !self.persistence_available {
            let state_path = self
                .state_path
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| self.language.message("ui.unavailable").to_owned());
            crate::ui::card(ui, |ui| {
                ui.label(
                    RichText::new(self.language.message("ui.durable-workspace-unavailable"))
                        .strong()
                        .color(colors.danger),
                );
                ui.label(self.language.message("ui.mailswiftsync-cannot-open-its-durable-workspace-this-session-is-temporary-l-8737d30905"));
                ui.horizontal_wrapped(|ui| {
                    ui.label(self.language.message("ui.state-file"));
                    ui.monospace(&state_path);
                    if ui
                        .button(self.language.message("ui.copy-state-path"))
                        .clicked()
                    {
                        ui.ctx().copy_text(state_path.clone());
                    }
                    if ui
                        .button(self.language.message("ui.open-activity-diagnostics"))
                        .clicked()
                    {
                        self.active_view = WorkspaceView::Activity;
                    }
                });
            });
            ui.add_space(8.0);
        }
        let project = self.ui_snapshot.project.clone();
        let phase = project
            .as_ref()
            .map(|value| value.phase)
            .unwrap_or(core::Phase::Discovery);
        let attention_count = self.ui_snapshot.mailbox_counts.needs_review;
        let mailbox_counts = self.ui_snapshot.mailbox_counts;
        let batch_summary = match self.bulk_queue_summary() {
            Ok(summary) => summary,
            Err(error) => {
                crate::ui::card(ui, |ui| {
                    ui.label(
                        RichText::new(self.language.text("Queue status unavailable"))
                            .strong()
                            .color(colors.danger),
                    );
                    ui.label(RichText::new(error).color(colors.text_secondary));
                    if ui
                        .button(self.language.text("Open Mailboxes to retry queue reads"))
                        .clicked()
                    {
                        self.active_view = WorkspaceView::Mailboxes;
                    }
                });
                return;
            }
        };
        let has_bulk_jobs = batch_summary.total > 0;
        let has_durable_jobs = mailbox_counts.total > 0;
        let has_mailboxes = has_durable_jobs || has_bulk_jobs;
        let workspace_attention_count = if has_bulk_jobs {
            batch_summary.attention
        } else {
            attention_count
        };
        let proof_ready = project.as_ref().is_some_and(|project| {
            customer_proof_ready(project.phase, attention_count, self.ui_snapshot.is_stale())
        });
        let next_action = recommended_workspace_action(
            phase,
            !self.preflight.is_empty(),
            workspace_attention_count,
            self.running(),
            has_bulk_jobs,
            proof_ready,
        );

        let first_run = project.is_none() && self.queue.is_empty();
        let (configured, total) = plan_completeness(&self.form.profile);
        let primary = overview_primary(OverviewPrimaryInputs {
            running: self.running(),
            attention: workspace_attention_count,
            proof_ready,
            has_imported_queue: has_bulk_jobs,
            phase,
            has_project: project.is_some(),
            plan_complete: configured == total,
        });
        if has_bulk_jobs {
            self.command_center_card(ui, batch_summary);
            ui.add_space(16.0);
            self.waves_card(ui);
            ui.add_space(16.0);
        }
        crate::ui::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            self.lifecycle_card(
                ui,
                &next_action,
                primary,
                first_run,
                workspace_attention_count,
                batch_summary.total,
            );
        });
        ui.add_space(16.0);

        ui.columns(3, |columns| {
            crate::ui::card(&mut columns[0], |ui| {
                ui.set_min_width(ui.available_width());
                crate::ui::section_label(ui, self.language.message("ui.project-status"));
                ui.label(RichText::new(if project.is_some() {
                    self.language.message("ui.project-created")
                } else if has_bulk_jobs {
                    self.language.message("ui.batch-queue-loaded")
                } else {
                    self.language.message("ui.no-project-yet")
                }).size(17.0).strong());
                ui.label(RichText::new(if project.is_some() {
                    self.language.message("ui.state-is-durable-and-ready-for-review")
                } else if has_bulk_jobs {
                    self.language.message("ui.review-the-imported-rows-then-run-a-durable-preflight")
                } else {
                    self.language.message("ui.start-by-configuring-endpoints-or-importing-a-mailbox-list")
                }).color(colors.text_secondary));
            });
            crate::ui::card(&mut columns[1], |ui| {
                ui.set_min_width(ui.available_width());
                crate::ui::section_label(ui, self.language.message("ui.mailboxes-814bfa48"));
                if !has_mailboxes {
                    ui.label(RichText::new(self.language.message("ui.none-configured")).size(17.0).strong());
                    ui.label(RichText::new(self.language.message("ui.use-mailboxes-to-review-scope-before-running-anything")).color(colors.text_secondary));
                } else if !has_durable_jobs {
                    ui.label(RichText::new(self.language.message("ui.mailbox-jobs-in-scope").replace("{}", &batch_summary.total.to_string())).size(17.0).strong());
                    ui.label(
                        RichText::new(self.language
                            .text("{} imported · {} queued · {} preflight · {} ready · {} attention · {} unresolved")
                            .replacen("{}", &batch_summary.imported.to_string(), 1)
                            .replacen("{}", &batch_summary.queued.to_string(), 1)
                            .replacen("{}", &batch_summary.preflight.to_string(), 1)
                            .replacen("{}", &batch_summary.ready.to_string(), 1)
                            .replacen("{}", &batch_summary.attention.to_string(), 1)
                            .replacen("{}", &batch_summary.unresolved().to_string(), 1))
                        .color(colors.text_secondary),
                    );
                    ui.label(RichText::new(self.language.message("ui.imported-rows-are-not-durable-until-preflight-admission-succeeds")).small().color(colors.text_secondary));
                } else {
                    ui.label(RichText::new(self.language.message("ui.total").replace("{}", &mailbox_counts.total.to_string())).size(17.0).strong());
                    ui.label(
                        RichText::new(self.language
                            .text("{} ready · {} running · {} verified")
                            .replacen("{}", &mailbox_counts.ready.to_string(), 1)
                            .replacen("{}", &mailbox_counts.running.to_string(), 1)
                            .replacen("{}", &mailbox_counts.verified.to_string(), 1))
                        .color(colors.text_secondary),
                    );
                    if attention_count > 0 {
                        ui.label(
                            RichText::new(self.language.message("ui.require-operator-attention").replace("{}", &attention_count.to_string()))
                                .color(colors.danger),
                        );
                    }
                }
            });
            crate::ui::card(&mut columns[2], |ui| {
                ui.set_min_width(ui.available_width());
                crate::ui::section_label(ui, self.language.message("ui.evidence-c0f115f0"));
                ui.label(RichText::new(if proof_ready {
                    self.language.message("ui.customer-proof-ready")
                } else if project.is_some() {
                    self.language.message("ui.review-required")
                } else {
                    self.language.message("ui.not-available")
                }).size(17.0).strong().color(if proof_ready { colors.success } else { colors.text_primary }));
                ui.label(RichText::new(if proof_ready {
                    self.language.message("ui.open-verification-to-export-the-customer-safe-evidence-artifact")
                } else {
                    self.language.message("ui.open-verification-to-review-evidence-customer-proof-remains-gated-until-the-8e499fb7b4")
                }).color(colors.text_secondary));
            });
        });
        ui.add_space(16.0);

        if !self.queue.is_empty() && self.bulk_selection_view_dirty {
            self.refresh_bulk_selection_view();
        }
        self.operator_cockpit(ui, phase, workspace_attention_count, has_bulk_jobs);
        ui.add_space(16.0);

        self.migration_confidence_panel(
            ui,
            has_bulk_jobs,
            batch_summary,
            proof_ready,
            if has_bulk_jobs {
                batch_summary.unresolved()
            } else {
                workspace_attention_count
            },
        );
        ui.add_space(16.0);

        self.attention_center(ui);
        self.overview_readiness_controls(ui);
        ui.add_space(16.0);
        self.overview_operational_notices(ui);
        ui.add_space(16.0);

        crate::ui::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            crate::ui::section_label(ui, self.language.message("ui.safety-contract"));
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                for text in [
                    "Preflight is the default",
                    "Saved profiles exclude passwords",
                    "Source mail is read-only by default",
                ] {
                    ui.label(
                        RichText::new(format!("✓ {}", self.language.text(text)))
                            .color(colors.success),
                    );
                }
                if self.form.profile.source_tls == "plain" {
                    ui.label(
                        RichText::new(
                            self.language
                                .text("! Source transport is cleartext by explicit configuration"),
                        )
                        .color(colors.danger),
                    );
                } else {
                    ui.label(
                        RichText::new(
                            self.language
                                .text("✓ Encrypted source transport with certificate verification"),
                        )
                        .color(colors.success),
                    );
                }
            });
        });
    }

    fn operator_cockpit(
        &mut self,
        ui: &mut egui::Ui,
        phase: core::Phase,
        attention_count: usize,
        has_bulk_jobs: bool,
    ) {
        let colors = self.theme_colors();
        let (configured, total) = plan_completeness(&self.form.profile);
        let preflight_blockers = self
            .preflight
            .iter()
            .filter(|(_, _, passed)| !passed)
            .count();
        let plan_digest = crate::plan_identity::fingerprint_digest(&self.form.plan_fingerprint());
        let accounts_current = crate::controller::capability_observation_matches(
            self.capability_observation_fingerprint.as_deref(),
            &plan_digest,
        ) && self.source_capabilities.is_some()
            && self.destination_capabilities.is_some();
        let selected = self.bulk_selection_view.selected_loaded;
        let live_eligible = self.bulk_selection_view.live_eligible;
        let delta_eligible = self.bulk_selection_view.delta_eligible;
        let phase_key = match phase {
            core::Phase::Discovery => "ui.discovery",
            core::Phase::Preflight => "ui.preflight-54614e80",
            core::Phase::Pilot => "ui.pilot",
            core::Phase::Seed => "ui.seed",
            core::Phase::CatchUp => "ui.catch-up",
            core::Phase::FinalDelta => "ui.final-delta",
            core::Phase::Verification => "ui.verification",
            core::Phase::Complete => "ui.complete",
            core::Phase::Attention => "ui.attention-c2eb8cd9",
        };
        let verification_key = if self.form.engine() == core::Engine::Dovecot {
            "ui.verification-level-1"
        } else if self.form.profile.body_hash_verification {
            "ui.verification-level-3"
        } else {
            "ui.verification-level-2"
        };
        let destination_policy = self.form.profile.destination_mutation_policy();

        crate::ui::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(self.language.message("ui.operator-cockpit"));
                crate::ui::pill(
                    ui,
                    self.language.message(phase_key),
                    if phase == core::Phase::Attention {
                        colors.warning
                    } else {
                        colors.info
                    },
                );
            });
            ui.label(
                RichText::new(self.language.message("ui.operator-cockpit-summary"))
                    .small()
                    .color(colors.text_secondary),
            );
            ui.add_space(8.0);
            egui::Grid::new("overview_operator_cockpit")
                .num_columns(2)
                .spacing([18.0, 7.0])
                .show(ui, |ui| {
                    ui.label(RichText::new(self.language.message("ui.plan-readiness")).strong());
                    ui.label(RichText::new(format!("{configured}/{total}")).color(
                        if configured == total {
                            colors.success
                        } else {
                            colors.warning
                        },
                    ));
                    ui.end_row();

                    ui.label(RichText::new(self.language.message("ui.preflight-status")).strong());
                    let preflight_status = if self.preflight.is_empty() {
                        self.language.message("ui.not-assessed").to_owned()
                    } else if preflight_blockers > 0 {
                        self.language
                            .message("ui.blocker-count")
                            .replace("{}", &preflight_blockers.to_string())
                    } else {
                        self.language.message("ui.no-blockers-recorded").to_owned()
                    };
                    ui.label(
                        RichText::new(preflight_status).color(if self.preflight.is_empty() {
                            colors.text_secondary
                        } else if preflight_blockers > 0 {
                            colors.warning
                        } else {
                            colors.success
                        }),
                    );
                    ui.end_row();

                    ui.label(RichText::new(self.language.message("ui.account-tests")).strong());
                    ui.label(
                        RichText::new(if accounts_current {
                            self.language.message("ui.current-for-plan")
                        } else if self.source_capabilities.is_some()
                            || self.destination_capabilities.is_some()
                        {
                            self.language.message("ui.stale-for-plan")
                        } else if self.form.engine() == core::Engine::Dovecot {
                            self.language.message("ui.checked-during-dovecot-preflight")
                        } else {
                            self.language.message("ui.not-tested")
                        })
                        .color(if accounts_current {
                            colors.success
                        } else {
                            colors.text_secondary
                        }),
                    );
                    ui.end_row();

                    ui.label(RichText::new(self.language.message("ui.mailbox-attention")).strong());
                    ui.label(RichText::new(attention_count.to_string()).color(
                        if attention_count > 0 {
                            colors.warning
                        } else {
                            colors.success
                        },
                    ));
                    ui.end_row();

                    ui.label(
                        RichText::new(self.language.message("ui.selected-batch-eligibility"))
                            .strong(),
                    );
                    let eligibility = if selected == 0 {
                        self.language.message("ui.no-mailboxes-selected").to_owned()
                    } else {
                        self.language
                            .text("{} selected · {} live eligible · {} final-delta eligible")
                            .replacen("{}", &selected.to_string(), 1)
                            .replacen("{}", &live_eligible.to_string(), 1)
                            .replacen("{}", &delta_eligible.to_string(), 1)
                    };
                    ui.label(RichText::new(eligibility).color(
                        if selected > 0 && live_eligible == 0 {
                            colors.warning
                        } else {
                            colors.text_primary
                        },
                    ));
                    ui.end_row();

                    ui.label(
                        RichText::new(self.language.message("ui.planned-verification")).strong(),
                    );
                    ui.label(self.language.message(verification_key));
                    ui.end_row();

                    ui.label(
                        RichText::new(
                            self.language
                                .message("ui.destination-mutation-policy-summary"),
                        )
                        .strong(),
                    );
                    ui.label(
                        RichText::new(self.language.text(destination_policy.label())).color(
                            if destination_policy.may_remove_destination_state() {
                                colors.danger
                            } else {
                                colors.success
                            },
                        ),
                    );
                    ui.end_row();
                });
            if destination_policy.may_remove_destination_state() {
                ui.label(
                    RichText::new(self.language.text(destination_policy.warning()))
                        .small()
                        .color(colors.danger),
                );
            }
            if has_bulk_jobs && selected > 0 && live_eligible == 0 && delta_eligible == 0 {
                ui.label(
                    RichText::new(
                        self.language
                            .message("ui.selected-mailboxes-not-action-eligible"),
                    )
                    .small()
                    .color(colors.warning),
                );
            }
        });
    }

    fn migration_confidence_panel(
        &mut self,
        ui: &mut egui::Ui,
        has_bulk_jobs: bool,
        batch_summary: crate::controller::BulkQueueSummary,
        proof_ready: bool,
        attention_count: usize,
    ) {
        let plan_digest = crate::plan_identity::fingerprint_digest(&self.form.plan_fingerprint());
        let accounts_current = crate::controller::capability_observation_matches(
            self.capability_observation_fingerprint.as_deref(),
            &plan_digest,
        ) && self.source_capabilities.is_some()
            && self.destination_capabilities.is_some();
        let (preflight, preflight_scope, preflight_label, preflight_action, requires_preflight) =
            if has_bulk_jobs {
                if batch_summary.total > 0
                    && batch_summary.imported + batch_summary.queued + batch_summary.preflight
                        == batch_summary.total
                {
                    (
                        None,
                        "mailboxes preflighted and ready",
                        "Mailbox preflight",
                        "Run preflight",
                        true,
                    )
                } else {
                    (
                        Some((
                            batch_summary.ready + batch_summary.verified,
                            batch_summary.unresolved(),
                            batch_summary.total,
                        )),
                        "mailboxes preflighted and ready",
                        "Mailbox preflight",
                        "Review mailboxes",
                        true,
                    )
                }
            } else if self.ui_snapshot.mailbox_counts.total > 0 {
                let counts = self.ui_snapshot.mailbox_counts;
                (
                    Some((
                        counts.ready + counts.verified,
                        counts.needs_review,
                        counts.total,
                    )),
                    "mailboxes preflighted and ready",
                    "Mailbox preflight",
                    "Review mailboxes",
                    true,
                )
            } else if self.preflight.is_empty() {
                (
                    None,
                    "plan validation checks passed",
                    "Plan assessment",
                    "Review assessment",
                    false,
                )
            } else {
                (
                    Some((
                        self.preflight
                            .iter()
                            .filter(|(_, _, passed)| *passed)
                            .count(),
                        self.preflight
                            .iter()
                            .filter(|(_, _, passed)| !*passed)
                            .count(),
                        self.preflight.len(),
                    )),
                    "plan validation checks passed",
                    "Plan assessment",
                    "Review assessment",
                    false,
                )
            };
        let quota_exceeded = if accounts_current && !has_bulk_jobs {
            self.destination_capabilities
                .as_ref()
                .filter(|capabilities| capabilities.quota_observed)
                .map(|capabilities| capabilities.quota_exceeded)
        } else {
            None
        };
        let (configured, total) = plan_completeness(&self.form.profile);
        let destination_tls = self.form.profile.destination_tls != "plain";
        let findings = migration_confidence(ConfidenceInputs {
            plan_complete: configured == total,
            encrypted_transport: self.form.profile.source_tls != "plain" && destination_tls,
            tls_observed: accounts_current && !has_bulk_jobs,
            // The currently tested mailbox is not evidence for every row in
            // a heterogeneous/imported queue.
            accounts_current: accounts_current && !has_bulk_jobs,
            preflight,
            preflight_scope,
            preflight_label,
            preflight_action,
            requires_preflight,
            attention_count,
            quota_exceeded,
            provider_pair: &format!(
                "{} → {}",
                self.form.profile.source_host, self.form.profile.destination_host
            ),
            // A provider probe is not a qualification run. This becomes true
            // only when machine-generated, verified packs are loaded here.
            provider_qualified: false,
            proof_ready,
            verification_level: if self.form.engine() == core::Engine::Dovecot {
                "Level 1 — Aggregate evidence"
            } else if self.form.profile.body_hash_verification {
                "Level 3 — Bounded content fingerprints"
            } else {
                "Level 2 — Per-message metadata reconciliation"
            },
        });
        let colors = self.theme_colors();
        let mut navigate = None;
        let blocked = findings
            .iter()
            .filter(|finding| finding.state == ConfidenceState::Blocked)
            .count();
        let unresolved = findings
            .iter()
            .filter(|finding| {
                matches!(
                    finding.state,
                    ConfidenceState::Warning | ConfidenceState::Unknown
                )
            })
            .count();
        crate::ui::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(self.language.message("ui.ready-to-migrate"));
                ui.label(
                    RichText::new(if blocked > 0 {
                        format!("{blocked} blocker(s)")
                    } else if unresolved > 0 {
                        format!("{unresolved} check(s) need review")
                    } else {
                        "All checks established".to_owned()
                    })
                    .small()
                    .color(if blocked > 0 {
                        colors.danger
                    } else if unresolved > 0 {
                        colors.warning
                    } else {
                        colors.success
                    }),
                );
            });
            ui.label(
                RichText::new(
                    self.language
                        .text("A simple go/no-go view. Unknown means not established—not passed."),
                )
                .small()
                .color(colors.text_secondary),
            );
            ui.add_space(6.0);
            egui::Grid::new("overview_readiness_summary")
                .num_columns(2)
                .spacing([24.0, 5.0])
                .show(ui, |ui| {
                    for finding in findings.iter().filter(|finding| {
                        matches!(
                            finding.section,
                            ConfidenceSection::Transfer
                                | ConfidenceSection::Verification
                                | ConfidenceSection::ProviderQualification
                        )
                    }) {
                        let (symbol, color, label) = match finding.state {
                            ConfidenceState::Ready => ("✓", colors.success, "Ready"),
                            ConfidenceState::Blocked => ("!", colors.danger, "Blocked"),
                            ConfidenceState::Warning => ("⚠", colors.warning, "Review"),
                            ConfidenceState::Unknown => ("?", colors.text_secondary, "Unknown"),
                        };
                        ui.label(RichText::new(format!("{symbol} {}", finding.label)).strong());
                        ui.label(RichText::new(label).color(color));
                        ui.end_row();
                    }
                });
            let primary = findings
                .iter()
                .find(|finding| finding.state == ConfidenceState::Blocked)
                .or_else(|| {
                    findings
                        .iter()
                        .find(|finding| finding.state != ConfidenceState::Ready)
                });
            if let Some(finding) = primary {
                if ui
                    .button(self.language.text(finding.remediation))
                    .on_hover_text(self.language.text(finding.consequence))
                    .clicked()
                {
                    navigate = Some(finding.destination);
                }
            }
            egui::CollapsingHeader::new(self.language.message("ui.advanced-readiness-details"))
                .default_open(false)
                .show(ui, |ui| {
                    for section in [
                        ConfidenceSection::Transfer,
                        ConfidenceSection::Verification,
                        ConfidenceSection::ProviderQualification,
                        ConfidenceSection::Cutover,
                    ] {
                        let title = match section {
                            ConfidenceSection::Transfer => "Transfer readiness",
                            ConfidenceSection::Verification => "Verification readiness",
                            ConfidenceSection::ProviderQualification => "Provider qualification",
                            ConfidenceSection::Cutover => "Cutover",
                        };
                        egui::CollapsingHeader::new(self.language.text(title)).show(ui, |ui| {
                            for finding in
                                findings.iter().filter(|finding| finding.section == section)
                            {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(RichText::new(finding.label).strong());
                                    ui.label(&finding.evidence);
                                    if ui
                                        .small_button(self.language.text(finding.remediation))
                                        .clicked()
                                    {
                                        navigate = Some(finding.destination);
                                    }
                                });
                            }
                        });
                    }
                });
        });
        if let Some(destination) = navigate {
            self.active_view = destination;
        }
    }

    pub(crate) fn source_transport_warning(&mut self, ui: &mut egui::Ui) {
        if self.form.profile.source_tls != "plain" {
            return;
        }
        crate::ui::card(ui, |ui| {
            ui.label(
                RichText::new(self.language.message("ui.insecure-source-transport"))
                    .strong()
                    .color(self.theme_colors().danger),
            );
            ui.label(
                self.language
                    .text("Plain IMAP can expose the source password and mailbox data in transit."),
            );
            let response = ui.add_enabled(
                !self.running(),
                egui::Checkbox::new(
                    &mut self.form.profile.allow_insecure_source_transport,
                    self.language
                        .text("I understand and explicitly allow cleartext source transport"),
                ),
            );
            response.on_hover_text(
                self.language.message("ui.use-imaps-or-starttls-whenever-possible-this-acknowledgement-is-required-be-5eab98ee65"),
            );
        });
    }

    fn attention_center(&mut self, ui: &mut egui::Ui) {
        let attention = self
            .ui_snapshot
            .verification_rows
            .iter()
            .filter(|mailbox| mailbox.attention_reason.is_some())
            .collect::<Vec<_>>();
        let mut reason_counts = self
            .ui_snapshot
            .attention_reason_counts
            .iter()
            .filter_map(|(key, count)| {
                crate::core::AttentionReason::parse(key).map(|reason| (reason, *count))
            })
            .collect::<Vec<_>>();
        reason_counts.sort_by(|(reason_a, count_a), (reason_b, count_b)| {
            count_b
                .cmp(count_a)
                .then_with(|| reason_a.as_str().cmp(reason_b.as_str()))
        });
        if attention.is_empty() && reason_counts.is_empty() {
            return;
        }
        let colors = self.theme_colors();
        crate::ui::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(self.language.message("ui.attention-center"))
                        .strong()
                        .color(colors.warning),
                );
                ui.label(
                    RichText::new(
                        self.language
                            .text("{} shown · {} total need review")
                            .replacen("{}", &attention.len().to_string(), 1)
                            .replacen(
                                "{}",
                                &self.ui_snapshot.mailbox_counts.needs_review.to_string(),
                                1,
                            ),
                    )
                    .color(colors.text_secondary),
                );
                if ui
                    .button(self.language.message("ui.open-verification-08754c14"))
                    .clicked()
                {
                    self.active_view = WorkspaceView::Verification;
                }
            });
            ui.label(
                RichText::new(
                    self.language.text(
                        "Each item names the durable reason and the next safe operator action.",
                    ),
                )
                .size(12.0)
                .color(colors.text_secondary),
            );
            if !reason_counts.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for (reason, count) in &reason_counts {
                        let label = format!("{} · {count}", self.language.text(reason.label()));
                        if ui.button(label).clicked() {
                            self.verification_attention_reason = Some(*reason);
                            self.verification_filter = "review".into();
                            self.verification_search.clear();
                            self.verification_offset = 0;
                            self.verification_cursor = None;
                            self.verification_cursor_stack.clear();
                            self.active_view = WorkspaceView::Verification;
                        }
                    }
                });
            }
            for mailbox in attention.iter().take(5) {
                let Some(reason) = mailbox.attention_reason else {
                    continue;
                };
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!(
                        "{} → {}",
                        mailbox.job.source_mailbox, mailbox.job.destination_mailbox
                    ));
                    ui.label(
                        RichText::new(self.language.text(reason.label())).color(colors.warning),
                    );
                });
                ui.label(
                    RichText::new(self.language.text(reason.recommended_action()))
                        .color(colors.text_secondary),
                );
            }
            if attention.len() > 5 {
                ui.label(
                    RichText::new(
                        self.language
                            .text("… plus {} more on this page")
                            .replace("{}", &(attention.len() - 5).to_string()),
                    )
                    .color(colors.text_secondary),
                );
            }
        });
    }

    fn overview_operational_notices(&mut self, ui: &mut egui::Ui) {
        if self.active_view != WorkspaceView::Overview
            && self.active_project_id().is_none()
            && self.queue.is_empty()
        {
            let colors = self.theme_colors();
            crate::ui::card(ui, |ui| {
                ui.heading(self.language.message("ui.start-a-safe-migration"));
                ui.label(RichText::new(self.language.message("ui.mailswiftsync-guides-every-migration-through-a-reviewed-preflight-before-an-48ba765ba1")).color(colors.text_secondary));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        self.language
                            .text("Start by choosing the source, destination, and account access."),
                    )
                    .color(colors.text_secondary),
                );
                ui.horizontal(|ui| {
                    if ui
                        .button(self.language.message("ui.import-mailbox-list"))
                        .clicked()
                    {
                        self.active_view = WorkspaceView::Mailboxes;
                    }
                    ui.label(
                        RichText::new(
                            self.language
                                .text("For one mailbox, continue with the migration plan below."),
                        )
                        .size(12.0)
                        .color(colors.text_secondary),
                    );
                });
            });
            ui.add_space(10.0);
        }
        if self.process_review_required {
            crate::ui::card(ui, |ui| {
                ui.label(
                    RichText::new(
                        self.language
                            .message("ui.process-ownership-review-required"),
                    )
                    .strong()
                    .color(self.theme_colors().danger),
                );
                ui.label(self.language.message("ui.mailswiftsync-could-not-prove-that-a-previously-recorded-migration-process-c8d2a03624"));
                if ui
                    .button(
                        self.language
                            .text("I confirmed no unverified migration process remains"),
                    )
                    .clicked()
                {
                    match self.acknowledge_process_review() {
                        Ok(()) => {
                            self.process_review_required = false;
                            self.set_status(self.language.message("ui.process-review-acknowledged-execution-gates-are-available-again"), StatusSeverity::Success);
                        }
                        Err(error) => self.set_status(
                            self.language
                                .text("Could not clear reviewed process identities: {error}")
                                .replace("{error}", &error.to_string()),
                            StatusSeverity::Error,
                        ),
                    }
                }
            });
        }
        if !self.workspace_read_only {
            self.source_transport_warning(ui);
        }
        if self.workspace_read_only {
            crate::ui::card(ui, |ui| {
                ui.label(
                    RichText::new(self.language.message("ui.historical-project-read-only"))
                        .strong()
                        .color(self.theme_colors().info),
                );
                ui.label(self.language.message("ui.you-are-viewing-durable-history-for-this-project-the-editable-migration-pla-7ce9c8cb0f"));
                if ui
                    .button(self.language.message("ui.start-a-new-migration"))
                    .clicked()
                {
                    self.start_new_migration();
                }
            });
            ui.add_space(8.0);
        }
    }

    pub(crate) fn overview_readiness_controls(&mut self, ui: &mut egui::Ui) {
        // Stale readiness observations are expired by `poll`, which runs
        // before every frame and immediately after navigation.
        crate::ui::card(ui, |ui| {
            ui.heading(self.language.message("ui.preflight-readiness"));
            ui.label(RichText::new(self.language.message("ui.plan-completeness-is-separate-from-live-network-checks-run-the-authenticate-f0804b8578")).color(self.theme_colors().text_secondary));
            ui.horizontal_wrapped(|ui| {
                let probe_enabled = self.capability_receiver.is_none()
                    && self.form.engine() != core::Engine::Dovecot
                    && !self.running()
                    && !self.workspace_read_only;
                if ui
                    .add_enabled(
                        probe_enabled,
                        egui::Button::new(
                            self.language
                                .message("ui.test-accounts-and-inspect-namespaces"),
                        ),
                    )
                    .clicked()
                {
                    self.start_capability_probe();
                }
                if ui
                    .add_enabled(
                        !self.running() && !self.workspace_read_only,
                        egui::Button::new(self.language.message("ui.refresh-assessment")),
                    )
                    .clicked()
                {
                    self.assess_plan();
                }
                if self.active_project_id().is_none()
                    && ui
                        .add_enabled(
                            !self.running() && !self.workspace_read_only,
                            egui::Button::new(self.language.message("ui.create-project-from-plan")),
                        )
                        .clicked()
                {
                    self.create_project();
                }
            });
            if self.form.engine() == core::Engine::Dovecot {
                ui.label(RichText::new(self.language.message("ui.dovecot-preflight-checks-the-configured-imapc-source-destination-readiness-df2b2fa248")).color(self.theme_colors().text_secondary));
            }
            if self.preflight.is_empty() {
                ui.label(
                    self.language
                        .text("No preflight assessment has been recorded for the current plan."),
                );
            } else {
                let mut open_plan = false;
                egui::Grid::new("overview_preflight_controls")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong(self.language.message("ui.check"));
                        ui.strong(self.language.message("ui.result"));
                        ui.strong(self.language.message("ui.action"));
                        ui.end_row();
                        for (name, detail, passed) in &self.preflight {
                            let status = ui.label(
                                RichText::new(if *passed { "✓" } else { "!" }).color(if *passed {
                                    self.theme_colors().success
                                } else {
                                    self.theme_colors().danger
                                }),
                            );
                            if !passed {
                                status.on_hover_text(self.language.message(
                                    "ui.preflight-check-not-satisfied-may-block-readiness",
                                ));
                            }
                            ui.label(RichText::new(name).strong());
                            ui.label(detail);
                            if !passed {
                                let fix_button = ui.add_enabled(
                                    !self.running() && !self.workspace_read_only,
                                    egui::Button::new(self.language.message("ui.fix-in-plan")),
                                );
                                if fix_button
                                    .on_hover_text(
                                        self.language.message("ui.fix-preflight-plan-tooltip"),
                                    )
                                    .clicked()
                                {
                                    open_plan = true;
                                }
                            } else {
                                ui.label(self.language.message("ui.not-applicable"));
                            }
                            ui.end_row();
                        }
                    });
                if open_plan {
                    self.active_view = WorkspaceView::Plan;
                }
            }
        });
        if let Some(project) = self
            .ui_snapshot
            .project
            .as_ref()
            .filter(|project| project.phase == core::Phase::Complete)
        {
            let project_id = project.id.clone();
            ui.add_space(10.0);
            crate::ui::card(ui, |ui| {
                ui.heading(self.language.message("ui.project-controls"));
                ui.label(RichText::new(self.language.message("ui.this-project-is-complete-and-read-only-reopening-requires-an-audit-reason-a-47a60c2fa0")).color(self.theme_colors().danger));
                ui.horizontal(|ui| {
                    let field_label = ui.label(self.language.message("ui.reason"));
                    crate::ui::LabelledField::link_label(
                        &ui.text_edit_singleline(&mut self.reopen_reason),
                        &field_label,
                    );
                    if ui
                        .add_enabled(
                            !self.running() && !self.reopen_reason.trim().is_empty(),
                            egui::Button::new(self.language.message("ui.reopen-project")),
                        )
                        .clicked()
                    {
                        match self.reopen_completed_project(&project_id, &self.reopen_reason) {
                            Ok(()) => {
                                self.reopen_reason.clear();
                                self.set_status(
                                    self.language
                                        .message("ui.project-reopened-for-documented-review"),
                                    StatusSeverity::Warning,
                                );
                            }
                            Err(error) => self.set_status(
                                self.language
                                    .text("Could not reopen project: {error}")
                                    .replace("{error}", &error.to_string()),
                                StatusSeverity::Error,
                            ),
                        }
                    }
                });
            });
        }
    }

    /// The Overview's one authoritative lifecycle: the durable project
    /// phases, named exactly as everywhere else, with a single primary
    /// action. Assessment is part of Discovery/Preflight, not a second
    /// workflow.
    fn lifecycle_card(
        &mut self,
        ui: &mut egui::Ui,
        next_action: &RecommendedAction,
        primary: OverviewPrimary,
        first_run: bool,
        attention_count: usize,
        imported_count: usize,
    ) {
        let colors = self.theme_colors();
        let current = match self.active_project_id() {
            None => core::Phase::Discovery,
            Some(_) => self
                .ui_snapshot
                .project
                .as_ref()
                .map(|project| project.phase)
                .unwrap_or(core::Phase::Discovery),
        };
        let current_index = LIFECYCLE
            .iter()
            .position(|(phase, _, _)| *phase == current)
            .unwrap_or(usize::MAX);
        ui.horizontal(|ui| {
            crate::ui::section_label(ui, self.language.message("ui.migration-lifecycle"));
            if current == core::Phase::Attention {
                crate::ui::pill(
                    ui,
                    self.language.message("ui.attention-required"),
                    colors.danger,
                );
            }
        });
        if first_run {
            ui.label(
                RichText::new(self.language.message("ui.start-your-first-migration"))
                    .size(18.0)
                    .strong(),
            );
            ui.label(RichText::new(self.language.message("ui.mailswiftsync-guides-every-migration-through-a-reviewable-preflight-before-219ddd077f")).color(colors.text_secondary));
        }
        ui.add_space(6.0);
        let steps = LIFECYCLE
            .iter()
            .enumerate()
            .map(|(index, (_, title_key, detail_key))| {
                (
                    self.language.message(title_key),
                    Some(self.language.message(detail_key)),
                    step_state(index, current_index),
                )
            })
            .collect::<Vec<_>>();
        crate::ui::stepper(ui, &steps, colors.success, colors.info);
        ui.add_space(10.0);
        ui.label(
            RichText::new(self.language.message("ui.recommended-next-step"))
                .strong()
                .color(colors.text_secondary),
        );
        ui.add_enabled_ui(!self.workspace_read_only, |ui| match primary {
            OverviewPrimary::PlanStep => self.plan_workflow_controls(ui),
            OverviewPrimary::ConfigurePlan => {
                ui.label(
                    RichText::new(self.language.message("ui.begin-in-prepare-choose-the-source-and-destination-then-test-both-accounts-a8911f7278"))
                        .size(14.0),
                );
                ui.horizontal_wrapped(|ui| {
                    if crate::ui::primary_button(
                        ui,
                        self.language.message("ui.configure-first-mailbox"),
                    )
                    .clicked()
                    {
                        self.active_view = WorkspaceView::Plan;
                    }
                    if ui
                        .link(self.language.message("ui.import-mailbox-list"))
                        .clicked()
                    {
                        self.active_view = WorkspaceView::Mailboxes;
                    }
                });
            }
            OverviewPrimary::Navigate => {
                ui.label(RichText::new(self.language.text(next_action.text)).size(14.0));
                let label = if !self.running() && attention_count > 0 {
                    self.language
                        .text("Review attention items ({})  →")
                        .replace("{}", &attention_count.to_string())
                } else if !self.running() && next_action.destination == WorkspaceView::Mailboxes
                    && imported_count > 0
                {
                    self.language
                        .text("Review imported mailboxes ({})  →")
                        .replace("{}", &imported_count.to_string())
                } else {
                    self.language.text(next_action.button_label).to_owned()
                };
                if crate::ui::primary_button(ui, &label).clicked() {
                    self.active_view = next_action.destination;
                }
            }
        });
        if current == core::Phase::Attention {
            ui.label(RichText::new(self.language.message("ui.a-mailbox-or-run-needs-operator-review-normal-lifecycle-progress-is-paused-46a8d56892")).small().color(colors.danger));
        }
    }
}

/// The operator workflow, in lifecycle order. Titles use operator language
/// ("Import accounts", "Final sync") rather than internal phase names; the
/// detail line names the concrete action for that step.
const LIFECYCLE: [(core::Phase, &str, &str); 8] = [
    (
        core::Phase::Discovery,
        "ui.workflow-step-prepare",
        "ui.phase-action-prepare",
    ),
    (
        core::Phase::Preflight,
        "ui.workflow-step-preflight",
        "ui.phase-action-preflight",
    ),
    (
        core::Phase::Pilot,
        "ui.workflow-step-pilot",
        "ui.phase-action-pilot",
    ),
    (
        core::Phase::Seed,
        "ui.workflow-step-seed",
        "ui.phase-action-seed",
    ),
    (
        core::Phase::CatchUp,
        "ui.workflow-step-catch-up",
        "ui.phase-action-catch-up",
    ),
    (
        core::Phase::FinalDelta,
        "ui.workflow-step-cutover",
        "ui.phase-action-cutover",
    ),
    (
        core::Phase::Verification,
        "ui.workflow-step-verify",
        "ui.phase-action-verify",
    ),
    (
        core::Phase::Complete,
        "ui.workflow-step-complete",
        "ui.phase-action-complete",
    ),
];

/// What the Overview's single primary button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverviewPrimary {
    /// Nothing is configured yet: open the plan.
    ConfigurePlan,
    /// Run the plan's own next readiness step (assess, test, preflight, live).
    PlanStep,
    /// Navigate to the page that owns the recommended action.
    Navigate,
}

struct OverviewPrimaryInputs {
    running: bool,
    attention: usize,
    proof_ready: bool,
    has_imported_queue: bool,
    phase: core::Phase,
    has_project: bool,
    plan_complete: bool,
}

fn overview_primary(inputs: OverviewPrimaryInputs) -> OverviewPrimary {
    if inputs.running
        || inputs.attention > 0
        || inputs.proof_ready
        || inputs.has_imported_queue
        || !matches!(
            inputs.phase,
            core::Phase::Discovery | core::Phase::Preflight
        )
    {
        OverviewPrimary::Navigate
    } else if !inputs.has_project && !inputs.plan_complete {
        OverviewPrimary::ConfigurePlan
    } else {
        OverviewPrimary::PlanStep
    }
}

/// Map a step position to its stepper state; `usize::MAX` means no step is
/// current (for example, while the project is in Attention).
fn step_state(index: usize, current: usize) -> crate::ui::StepState {
    if current != usize::MAX && index < current {
        crate::ui::StepState::Done
    } else if index == current {
        crate::ui::StepState::Current
    } else {
        crate::ui::StepState::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::{LIFECYCLE, OverviewPrimary, OverviewPrimaryInputs, overview_primary};

    #[test]
    fn workflow_steps_use_operator_language_in_every_locale() {
        let internal = [
            "Discovery",
            "Preflight",
            "Seed",
            "Final delta",
            "FinalDelta",
            "Catch-up",
        ];
        for language in [
            crate::ui::language::UiLanguage::English,
            crate::ui::language::UiLanguage::German,
        ] {
            for (_, title_key, detail_key) in LIFECYCLE {
                let title = language.message(title_key);
                assert!(!title.is_empty() && title != title_key, "{title_key}");
                assert!(!language.message(detail_key).is_empty(), "{detail_key}");
                assert!(
                    !internal.contains(&title),
                    "{title_key} shows internal phase name {title:?}"
                );
            }
        }
        assert_eq!(
            crate::ui::language::UiLanguage::English.message(LIFECYCLE[0].1),
            "Add accounts"
        );
    }
    use crate::core::Phase;

    fn inputs() -> OverviewPrimaryInputs {
        OverviewPrimaryInputs {
            running: false,
            attention: 0,
            proof_ready: false,
            has_imported_queue: false,
            phase: Phase::Discovery,
            has_project: false,
            plan_complete: true,
        }
    }

    #[test]
    fn lifecycle_uses_every_durable_phase_once_in_order() {
        let phases: Vec<_> = LIFECYCLE.iter().map(|(phase, _, _)| *phase).collect();
        assert_eq!(
            phases,
            [
                Phase::Discovery,
                Phase::Preflight,
                Phase::Pilot,
                Phase::Seed,
                Phase::CatchUp,
                Phase::FinalDelta,
                Phase::Verification,
                Phase::Complete,
            ]
        );
    }

    #[test]
    fn readiness_phases_reuse_the_plan_step_as_the_single_primary_action() {
        assert_eq!(overview_primary(inputs()), OverviewPrimary::PlanStep);
        assert_eq!(
            overview_primary(OverviewPrimaryInputs {
                phase: Phase::Preflight,
                has_project: true,
                ..inputs()
            }),
            OverviewPrimary::PlanStep
        );
        assert_eq!(
            overview_primary(OverviewPrimaryInputs {
                plan_complete: false,
                ..inputs()
            }),
            OverviewPrimary::ConfigurePlan
        );
    }

    #[test]
    fn running_attention_batches_and_later_phases_navigate() {
        for case in [
            OverviewPrimaryInputs {
                running: true,
                ..inputs()
            },
            OverviewPrimaryInputs {
                attention: 3,
                ..inputs()
            },
            OverviewPrimaryInputs {
                has_imported_queue: true,
                ..inputs()
            },
            OverviewPrimaryInputs {
                proof_ready: true,
                ..inputs()
            },
            OverviewPrimaryInputs {
                phase: Phase::Seed,
                has_project: true,
                ..inputs()
            },
        ] {
            assert_eq!(overview_primary(case), OverviewPrimary::Navigate);
        }
    }
}
