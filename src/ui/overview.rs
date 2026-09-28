//! Shared Overview workflow presentation.

use crate::App;
use crate::core;
use crate::migration_plan::completeness as plan_completeness;
use crate::ui::{StatusSeverity, WorkspaceView, format_phase_name};
use crate::ui::{
    customer_proof_ready, recommended_batch_next_action, recommended_next_action,
    workflow_step_index,
};
use eframe::egui::{self, RichText};

impl App {
    pub(crate) fn overview_view(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.language.text("Migration overview"));
        ui.label(
            RichText::new(
                self.language
                    .text("A calm, evidence-led workspace for moving mailboxes safely."),
            )
            .color(self.theme_colors().text_secondary),
        );
        ui.add_space(16.0);
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
        let next_action =
            recommended_batch_next_action(has_bulk_jobs, workspace_attention_count, self.running())
                .unwrap_or_else(|| {
                    recommended_next_action(
                        phase,
                        !self.preflight.is_empty(),
                        attention_count,
                        self.running(),
                    )
                });
        self.overview_readiness_controls(ui);
        ui.add_space(14.0);
        let workflow_index = workflow_step_index(phase, !self.preflight.is_empty(), has_mailboxes);
        ui.group(|ui| {
            ui.label(RichText::new(self.language.text("MIGRATION WORKFLOW")).strong().size(11.0));
            ui.horizontal_wrapped(|ui| {
                for (index, (title, detail)) in [
                    ("Connect", "endpoints"),
                    ("Assess", "readiness"),
                    ("Preflight", "review"),
                    ("Migrate", "execute"),
                    ("Verify", "evidence"),
                    ("Deliver", "customer proof"),
                ]
                .into_iter()
                .enumerate()
                {
                    let (marker, color) = if index < workflow_index {
                        ("✓", self.theme_colors().success)
                    } else if index == workflow_index {
                        ("●", self.theme_colors().info)
                    } else {
                        ("○", self.theme_colors().text_secondary)
                    };
                    ui.group(|ui| {
                        ui.label(
                            RichText::new(format!("{marker} {}", self.language.text(title)))
                                .strong()
                                .color(color),
                        );
                        ui.label(
                            RichText::new(self.language.text(detail)).color(self.theme_colors().text_secondary),
                        );
                    });
                    if index < 5 {
                        ui.label(RichText::new("→").color(self.theme_colors().text_secondary));
                    }
                }
            });
            ui.label(
                RichText::new(self.language.text("The highlighted step is the current operator focus. A completed-looking step never bypasses the durable execution gates."))
                    .size(11.0)
                    .color(self.theme_colors().text_secondary),
            );
        });
        self.project_summary(ui);
        ui.add_space(14.0);
        self.attention_center(ui);
        ui.add_space(14.0);
        if project.is_none() && self.bulk_jobs.is_empty() {
            ui.group(|ui| {
                ui.heading(self.language.text("Start your first migration"));
                ui.label(self.language.text("MailSwiftSync guides every migration through a reviewable preflight before any destination changes are allowed."));
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    for (number, title, detail) in [
                        ("1", "Connect", "Enter source and destination endpoints."),
                        ("2", "Assess", "Run a dry preflight and review the plan."),
                        ("3", "Prove", "Migrate, verify, and export customer evidence."),
                    ] {
                        ui.group(|ui| {
                            ui.label(RichText::new(format!("{number}  {}", self.language.text(title))).strong());
                            ui.label(RichText::new(self.language.text(detail)).color(self.theme_colors().text_secondary));
                        });
                    }
                });
                if ui.button(self.language.text("Configure first mailbox  →")).clicked() {
                    self.active_view = WorkspaceView::Plan;
                }
                ui.label(RichText::new(self.language.text("For multiple mailboxes, use Batch after reviewing one representative pilot.")).color(self.theme_colors().text_secondary));
            });
            ui.add_space(14.0);
        }
        ui.group(|ui| {
            ui.label(
                RichText::new(self.language.text("CURRENT PHASE"))
                    .size(11.0)
                    .strong()
                    .color(self.theme_colors().text_secondary),
            );
            ui.heading(self.language.text(format_phase_name(phase)));
            if attention_count > 0 {
                ui.label(
                    RichText::new(format!(
                        "{} mailbox item(s) need attention",
                        attention_count
                    ))
                    .color(self.theme_colors().danger),
                );
            }
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.group(|ui| {
                ui.label(
                    RichText::new(self.language.text("PROJECT STATUS"))
                        .size(11.0)
                        .color(self.theme_colors().text_secondary),
                );
                ui.heading(if project.is_some() {
                    self.language.text("Project created")
                } else if has_bulk_jobs {
                    self.language.text("Batch queue loaded")
                } else {
                    self.language.text("No project yet")
                });
                ui.label(if project.is_some() {
                    self.language.text("State is durable and ready for review.")
                } else if has_bulk_jobs {
                    self.language.text("Review the imported rows, then run a durable preflight.")
                } else {
                    self.language.text("Start by configuring endpoints or importing a mailbox list.")
                });
            });
            ui.group(|ui| {
                ui.label(
                    RichText::new(self.language.text("MAILBOXES"))
                        .size(11.0)
                        .color(self.theme_colors().text_secondary),
                );
                if !has_mailboxes {
                    ui.heading(self.language.text("None configured"));
                    ui.label(self.language.text("Use Mailboxes to review scope before running anything."));
                } else if !has_durable_jobs {
                    ui.heading(format!("{} queued", batch_summary.total));
                    ui.label(format!(
                        "{} imported · {} queued · {} preflight · {} ready · {} attention · {} unresolved",
                        batch_summary.imported,
                        batch_summary.queued,
                        batch_summary.preflight,
                        batch_summary.ready,
                        batch_summary.attention,
                        batch_summary.unresolved(),
                    ));
                    ui.label(self.language.text("Imported rows are not durable until preflight admission succeeds."));
                } else {
                    ui.heading(format!("{} total", mailbox_counts.total));
                    ui.label(format!(
                        "{} ready · {} running · {} verified",
                        mailbox_counts.ready, mailbox_counts.running, mailbox_counts.verified,
                    ));
                    if attention_count > 0 {
                        ui.label(
                            RichText::new(format!(
                                "{} require operator attention",
                                attention_count
                            ))
                            .color(self.theme_colors().danger),
                        );
                    }
                }
            });
            ui.group(|ui| {
                ui.label(
                    RichText::new(self.language.text("EVIDENCE"))
                        .size(11.0)
                        .color(self.theme_colors().text_secondary),
                );
                ui.heading(if proof_ready {
                    self.language.text("Customer proof ready")
                } else if project.is_some() {
                    self.language.text("Review required")
                } else {
                    self.language.text("Not available")
                });
                ui.label(if proof_ready {
                    self.language.text("Open Verification to export the customer-safe evidence artifact.")
                } else {
                    self.language.text("Open Verification to review evidence; customer proof remains gated until the durable state is complete.")
                });
            });
        });
        ui.add_space(16.0);
        ui.group(|ui| {
            ui.heading(self.language.text("Recommended next step"));
            ui.label(self.language.text(next_action));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.workspace_read_only,
                        egui::Button::new(self.language.text("Open migration plan  →")),
                    )
                    .clicked()
                {
                    self.active_view = WorkspaceView::Plan;
                }
                if ui
                    .add_enabled(
                        !self.workspace_read_only,
                        egui::Button::new(self.language.text("Refresh preflight assessment")),
                    )
                    .clicked()
                {
                    self.assess_plan();
                }
                if ui
                    .add_enabled(
                        !self.workspace_read_only,
                        egui::Button::new(self.language.text("Import mailbox list")),
                    )
                    .clicked()
                {
                    self.active_view = WorkspaceView::Mailboxes;
                }
            });
        });
        ui.add_space(14.0);
        ui.label(RichText::new(self.language.text("Safety contract")).strong());
        ui.horizontal_wrapped(|ui| {
            for text in [
                "Preflight is the default",
                "Saved profiles exclude passwords",
                "Source mail is read-only by default",
            ] {
                ui.label(
                    RichText::new(format!("✓ {}", self.language.text(text)))
                        .color(self.theme_colors().success),
                );
            }
            if self.form.profile.source_tls == "plain" {
                ui.label(
                    RichText::new(
                        self.language
                            .text("! Source transport is cleartext by explicit configuration"),
                    )
                    .color(self.theme_colors().danger),
                );
            } else {
                ui.label(
                    RichText::new(
                        self.language
                            .text("✓ Encrypted source transport with certificate verification"),
                    )
                    .color(self.theme_colors().success),
                );
            }
        });
    }

    pub(crate) fn source_transport_warning(&mut self, ui: &mut egui::Ui) {
        if self.form.profile.source_tls != "plain" {
            return;
        }
        ui.group(|ui| {
            ui.label(RichText::new(self.language.text("INSECURE SOURCE TRANSPORT")).strong().color(self.theme_colors().danger));
            ui.label(self.language.text("Plain IMAP can expose the source password and mailbox data in transit."));
            let response = ui.add_enabled(
                !self.running(),
                egui::Checkbox::new(
                    &mut self.form.profile.allow_insecure_source_transport,
                    self.language.text("I understand and explicitly allow cleartext source transport"),
                ),
            );
            response.on_hover_text(
                self.language.text("Use IMAPS or STARTTLS whenever possible. This acknowledgement is required before any authenticated operation, including dry preflight, and is included in the preflight fingerprint."),
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
        if attention.is_empty() {
            return;
        }
        let colors = self.theme_colors();
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(self.language.text("ATTENTION CENTER"))
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
                if ui.button(self.language.text("Open verification")).clicked() {
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
            for mailbox in attention.iter().take(5) {
                let Some(reason) = mailbox.attention_reason else {
                    continue;
                };
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&mailbox.job.source_mailbox).strong());
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
                    RichText::new(format!("… plus {} more on this page", attention.len() - 5))
                        .color(colors.text_secondary),
                );
            }
        });
    }

    pub(crate) fn project_summary(&mut self, ui: &mut egui::Ui) {
        self.lifecycle_stepper(ui);
        if self.active_view != WorkspaceView::Overview
            && self.active_project_id().is_none()
            && self.bulk_jobs.is_empty()
        {
            let colors = self.theme_colors();
            ui.group(|ui| {
                ui.heading(self.language.text("Start a safe migration"));
                ui.label(RichText::new(self.language.text("MailSwiftSync guides every migration through a reviewed preflight before any destination changes are allowed.")).color(colors.text_secondary));
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    for (number, title, detail) in [
                        ("1", "Connect", "Configure source and destination"),
                        ("2", "Preflight", "Authenticate and review blockers"),
                        ("3", "Pilot", "Start with a small mailbox set"),
                    ] {
                        ui.group(|ui| {
                            ui.label(RichText::new(format!("{number}  {}", self.language.text(title))).strong().color(colors.info));
                            ui.label(RichText::new(self.language.text(detail)).size(11.0).color(colors.text_secondary));
                        });
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button(self.language.text("Import mailbox list")).clicked() {
                        self.active_view = WorkspaceView::Mailboxes;
                    }
                    ui.label(RichText::new(self.language.text("For one mailbox, continue with the migration plan below.")).size(11.0).color(colors.text_secondary));
                });
            });
            ui.add_space(10.0);
        }
        if self.process_review_required {
            ui.group(|ui| {
                ui.label(RichText::new(self.language.text("PROCESS OWNERSHIP REVIEW REQUIRED")).strong().color(self.theme_colors().danger));
                ui.label(self.language.text("MailSwiftSync could not prove that a previously recorded migration process is gone. Do not start another migration until you have checked the host process list and confirmed no MailSwiftSync engine remains."));
                if ui.button(self.language.text("I confirmed no unverified migration process remains")).clicked() {
                    match self.store.clear_active_processes_after_review() {
                        Ok(()) => {
                            self.process_review_required = false;
                            self.set_status(self.language.text("Process review acknowledged; execution gates are available again."), StatusSeverity::Success);
                        }
                        Err(error) => self.set_status(self.language.text("Could not clear reviewed process identities: {error}").replace("{error}", &error.to_string()), StatusSeverity::Error),
                    }
                }
            });
        }
        if !self.workspace_read_only {
            self.source_transport_warning(ui);
        }
        if self.workspace_read_only {
            ui.group(|ui| {
                ui.label(RichText::new(self.language.text("HISTORICAL PROJECT · READ ONLY")).strong().color(self.theme_colors().info));
                ui.label(self.language.text("You are viewing durable history for this project. The editable migration plan and execution controls are detached until you start a new migration."));
                if ui.button(self.language.text("Start a new migration")).clicked() {
                    self.start_new_migration();
                }
            });
            ui.add_space(8.0);
        }
        if self.active_view != WorkspaceView::Plan {
            match self.active_view {
                WorkspaceView::Overview => self.overview_view(ui),
                WorkspaceView::Mailboxes => self.mailbox_view(ui),
                WorkspaceView::Activity => self.activity_view(ui),
                WorkspaceView::Verification => self.verification_view(ui),
                WorkspaceView::Plan => {}
            }
            return;
        }
        let (passed, total) = plan_completeness(&self.form.profile);
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.heading(self.language.text("Migration workspace"));
                ui.label(RichText::new(self.language.text(if self.form.dry_run { "PREFLIGHT" } else { "LIVE MIGRATION" })).strong().color(if self.form.dry_run { self.theme_colors().success } else { self.theme_colors().danger }));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(self.language.text("{}/{} configuration items complete").replacen("{}", &passed.to_string(), 1).replacen("{}", &total.to_string(), 1)).strong().color(if passed == total { self.theme_colors().success } else { self.theme_colors().danger }));
                });
            });
            ui.add_space(5.0);
            ui.label(RichText::new(self.language.text("Recommended next step: run preflight, review blockers, then select a small pilot mailbox.")).color(self.theme_colors().text_secondary));
        });
    }

    pub(crate) fn overview_readiness_controls(&mut self, ui: &mut egui::Ui) {
        if self.invalidate_stale_capability_observation() {
            self.set_status(
                self.language.text("Readiness observations expired because the migration plan changed; run discovery again."),
                StatusSeverity::Warning,
            );
        }
        ui.group(|ui| {
            ui.heading(self.language.text("Preflight & readiness"));
            ui.label(RichText::new(self.language.text("Plan completeness is separate from live network checks. Run the authenticated probe before live migration.")).color(self.theme_colors().text_secondary));
            ui.horizontal_wrapped(|ui| {
                let probe_enabled = self.capability_receiver.is_none()
                    && self.form.engine() != core::Engine::Dovecot
                    && !self.running()
                    && !self.workspace_read_only;
                if ui.add_enabled(probe_enabled, egui::Button::new(self.language.text("Run authenticated readiness probe"))).clicked() {
                    self.start_capability_probe();
                }
                if ui.add_enabled(!self.running() && !self.workspace_read_only, egui::Button::new(self.language.text("Refresh assessment"))).clicked() {
                    self.assess_plan();
                }
                if self.active_project_id().is_none()
                    && ui.add_enabled(!self.running() && !self.workspace_read_only, egui::Button::new(self.language.text("Create project from plan"))).clicked()
                {
                    self.create_project();
                }
            });
            if self.form.engine() == core::Engine::Dovecot {
                ui.label(RichText::new(self.language.text("Dovecot preflight checks the configured imapc source; destination readiness still requires administrative review.")).color(self.theme_colors().text_secondary));
            }
            if self.preflight.is_empty() {
                ui.label(self.language.text("No preflight assessment has been recorded for the current plan."));
            } else {
                egui::Grid::new("overview_preflight_controls").striped(true).show(ui, |ui| {
                    ui.strong(self.language.text("Check"));
                    ui.strong(self.language.text("Result"));
                    ui.end_row();
                    for (name, detail, passed) in &self.preflight {
                        ui.label(RichText::new(if *passed { "✓" } else { "!" }).color(if *passed { self.theme_colors().success } else { self.theme_colors().danger }));
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
            ui.group(|ui| {
                ui.heading(self.language.text("Project controls"));
                ui.label(RichText::new(self.language.text("This project is complete and read-only. Reopening requires an audit reason and returns it to Attention.")).color(self.theme_colors().danger));
                ui.horizontal(|ui| {
                    ui.label(self.language.text("Reason"));
                    ui.text_edit_singleline(&mut self.reopen_reason);
                    if ui.add_enabled(!self.running() && !self.reopen_reason.trim().is_empty(), egui::Button::new(self.language.text("Reopen project"))).clicked() {
                        match self.store.reopen_project(&project_id, &self.reopen_reason) {
                            Ok(()) => {
                                self.reopen_reason.clear();
                                self.set_status(self.language.text("Project reopened for documented review"), StatusSeverity::Warning);
                            }
                            Err(error) => self.set_status(self.language.text("Could not reopen project: {error}").replace("{error}", &error.to_string()), StatusSeverity::Error),
                        }
                    }
                });
            });
        }
    }

    pub(crate) fn lifecycle_stepper(&self, ui: &mut egui::Ui) {
        let phases = [
            core::Phase::Discovery,
            core::Phase::Preflight,
            core::Phase::Pilot,
            core::Phase::Seed,
            core::Phase::CatchUp,
            core::Phase::FinalDelta,
            core::Phase::Verification,
            core::Phase::Complete,
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
            .position(|phase| *phase == current)
            .unwrap_or(usize::MAX);
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(self.language.text("MIGRATION LIFECYCLE")).size(11.0).strong().color(self.theme_colors().text_secondary));
                if current == core::Phase::Attention {
                    ui.label(RichText::new(self.language.text("ATTENTION REQUIRED")).strong().color(self.theme_colors().danger));
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (index, phase) in phases.iter().enumerate() {
                    if index > 0 {
                        ui.label(RichText::new("→").color(self.theme_colors().text_secondary));
                    }
                    let color = if current_index != usize::MAX && index < current_index {
                        self.theme_colors().success
                    } else if index == current_index {
                        self.theme_colors().info
                    } else {
                        self.theme_colors().text_secondary
                    };
                    ui.label(
                        RichText::new(format!(
                            "{} {}",
                            if current_index != usize::MAX && index < current_index { "✓" }
                            else if index == current_index { "●" } else { "○" },
                            self.language.text(format_phase_name(*phase))
                        ))
                        .strong()
                        .color(color),
                    );
                }
            });
            if current == core::Phase::Attention {
                ui.label(RichText::new(self.language.text("A mailbox or run needs operator review. Normal lifecycle progress is paused until it is resolved.")).size(11.0).color(self.theme_colors().danger));
            }
        });
    }
}
