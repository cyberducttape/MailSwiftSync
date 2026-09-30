//! Shared Overview workflow presentation.

use crate::App;
use crate::core;
use crate::migration_plan::completeness as plan_completeness;
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
        let batch_summary = self.bulk_queue_summary();
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

        if project.is_none() && self.bulk_jobs.is_empty() {
            crate::ui::card(ui, |ui| {
                ui.label(
                    RichText::new(self.language.message("ui.start-your-first-migration"))
                        .size(18.0)
                        .strong(),
                );
                ui.label(RichText::new(self.language.message("ui.mailswiftsync-guides-every-migration-through-a-reviewable-preflight-before-219ddd077f")).color(colors.text_secondary));
                ui.add_space(10.0);
                ui.label(
                    RichText::new(self.language.message("ui.begin-in-prepare-choose-the-source-and-destination-then-test-both-accounts-a8911f7278"))
                        .color(colors.text_secondary),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if crate::ui::primary_button(
                        ui,
                        self.language.message("ui.configure-first-mailbox"),
                    )
                    .clicked()
                    {
                        self.active_view = WorkspaceView::Plan;
                    }
                    if ui
                        .button(self.language.message("ui.import-mailbox-list"))
                        .clicked()
                    {
                        self.active_view = WorkspaceView::Mailboxes;
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new(self.language.message("ui.for-a-batch-import-the-mailbox-list-and-begin-with-a-small-pilot-during-the-pilot-stage")).small().color(colors.text_secondary));
            });
            ui.add_space(16.0);
        }

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
                    ui.label(RichText::new(self.language.message("ui.queued-fc10ab7d").replace("{}", &batch_summary.total.to_string())).size(17.0).strong());
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

        crate::ui::card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            self.lifecycle_stepper(ui, &next_action);
            if attention_count > 0 {
                ui.label(
                    RichText::new(format!(
                        "{} mailbox item(s) need attention",
                        attention_count
                    ))
                    .color(colors.danger),
                );
            }
        });
        ui.add_space(16.0);
        self.attention_center(ui);
        self.overview_readiness_controls(ui);
        ui.add_space(16.0);
        self.project_summary(ui);
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
                .size(11.0)
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

    pub(crate) fn project_summary(&mut self, ui: &mut egui::Ui) {
        let destination_policy = self.form.profile.destination_mutation_policy();
        if self.active_view != WorkspaceView::Overview
            && self.active_project_id().is_none()
            && self.bulk_jobs.is_empty()
        {
            let colors = self.theme_colors();
            crate::ui::card(ui, |ui| {
                ui.heading(self.language.message("ui.start-a-safe-migration"));
                ui.label(RichText::new(self.language.message("ui.mailswiftsync-guides-every-migration-through-a-reviewed-preflight-before-an-48ba765ba1")).color(colors.text_secondary));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(self.language.text(
                        "Begin in Prepare by choosing the source, destination, and account access.",
                    ))
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
                        .size(11.0)
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
        let (passed, total) = plan_completeness(&self.form.profile);
        crate::ui::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(self.language.message("ui.migration-workspace"));
                crate::ui::pill(
                    ui,
                    self.language.text(if self.form.dry_run {
                        "PREFLIGHT"
                    } else {
                        "LIVE MIGRATION"
                    }),
                    if self.form.dry_run {
                        self.theme_colors().success
                    } else {
                        self.theme_colors().info
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(
                            self.language
                                .text("{}/{} configuration items complete")
                                .replacen("{}", &passed.to_string(), 1)
                                .replacen("{}", &total.to_string(), 1),
                        )
                        .strong()
                        .color(if passed == total {
                            self.theme_colors().success
                        } else {
                            self.theme_colors().warning
                        }),
                    );
                });
            });
            ui.add_space(5.0);
            ui.label(RichText::new(self.language.message("ui.recommended-next-step-run-preflight-review-blockers-then-select-a-small-pilot-mailbox")).color(self.theme_colors().text_secondary));
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    self.language
                        .text("Destination policy: {}")
                        .replace("{}", self.language.text(destination_policy.label())),
                )
                .strong()
                .color(if destination_policy.may_remove_destination_state() {
                    self.theme_colors().danger
                } else {
                    self.theme_colors().success
                }),
            );
            ui.label(
                RichText::new(self.language.text(destination_policy.warning()))
                    .small()
                    .color(if destination_policy.may_remove_destination_state() {
                        self.theme_colors().danger
                    } else {
                        self.theme_colors().text_secondary
                    }),
            );
        });
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
                egui::Grid::new("overview_preflight_controls")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong(self.language.message("ui.check"));
                        ui.strong(self.language.message("ui.result"));
                        ui.end_row();
                        for (name, detail, passed) in &self.preflight {
                            ui.label(RichText::new(if *passed { "✓" } else { "!" }).color(
                                if *passed {
                                    self.theme_colors().success
                                } else {
                                    self.theme_colors().danger
                                },
                            ));
                            ui.label(RichText::new(name).strong());
                            ui.label(detail);
                            ui.end_row();
                        }
                    });
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
                    ui.label(self.language.message("ui.reason"));
                    ui.text_edit_singleline(&mut self.reopen_reason);
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

    pub(crate) fn lifecycle_stepper(&mut self, ui: &mut egui::Ui, next_action: &RecommendedAction) {
        let phases = [
            (core::Phase::Discovery, "Prepare", "ui.phase-action-prepare"),
            (
                core::Phase::Preflight,
                "Preflight",
                "ui.phase-action-preflight",
            ),
            (core::Phase::Pilot, "Pilot", "ui.phase-action-pilot"),
            (core::Phase::Seed, "Seed", "ui.phase-action-seed"),
            (core::Phase::CatchUp, "Catch-up", "ui.phase-action-catch-up"),
            (
                core::Phase::FinalDelta,
                "Cutover",
                "ui.phase-action-cutover",
            ),
            (
                core::Phase::Verification,
                "Verify",
                "ui.phase-action-verify",
            ),
            (
                core::Phase::Complete,
                "Complete",
                "ui.phase-action-complete",
            ),
        ];
        let current = match self.active_project_id() {
            None => core::Phase::Discovery,
            Some(_) => self
                .ui_snapshot
                .project
                .as_ref()
                .map(|project| project.phase)
                .unwrap_or(core::Phase::Discovery),
        };
        let current_index = phases
            .iter()
            .position(|(phase, _, _)| *phase == current)
            .unwrap_or(usize::MAX);
        ui.horizontal(|ui| {
            crate::ui::section_label(ui, self.language.message("ui.migration-lifecycle"));
            if current == core::Phase::Attention {
                crate::ui::pill(
                    ui,
                    self.language.message("ui.attention-required"),
                    self.theme_colors().danger,
                );
            }
        });
        ui.add_space(6.0);
        let steps = phases
            .iter()
            .enumerate()
            .map(|(index, (_, label, detail_key))| {
                (
                    self.language.text(label),
                    Some(self.language.message(detail_key)),
                    step_state(index, current_index),
                )
            })
            .collect::<Vec<_>>();
        crate::ui::stepper(
            ui,
            &steps,
            self.theme_colors().success,
            self.theme_colors().info,
        );
        ui.add_space(10.0);
        ui.label(
            RichText::new(self.language.message("ui.recommended-next-step"))
                .strong()
                .color(self.theme_colors().text_secondary),
        );
        ui.label(RichText::new(self.language.text(next_action.text)).size(14.0));
        ui.add_enabled_ui(!self.workspace_read_only, |ui| {
            ui.horizontal_wrapped(|ui| {
                if crate::ui::primary_button(ui, self.language.text(next_action.button_label))
                    .clicked()
                {
                    self.active_view = next_action.destination;
                }
                if ui
                    .button(self.language.message("ui.refresh-preflight-assessment"))
                    .clicked()
                {
                    self.assess_plan();
                }
                if ui
                    .button(self.language.message("ui.import-mailbox-list"))
                    .clicked()
                {
                    self.active_view = WorkspaceView::Mailboxes;
                }
            });
        });
        if current == core::Phase::Attention {
            ui.label(RichText::new(self.language.message("ui.a-mailbox-or-run-needs-operator-review-normal-lifecycle-progress-is-paused-46a8d56892")).small().color(self.theme_colors().danger));
        }
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
