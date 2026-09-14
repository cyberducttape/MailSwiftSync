//! Shared Overview workflow presentation.

use crate::App;
use crate::core;
use crate::migration_plan::completeness as plan_completeness;
use crate::ui::{StatusSeverity, WorkspaceView, format_phase_name};
use eframe::egui::{self, RichText};

impl App {
    pub(crate) fn source_transport_warning(&mut self, ui: &mut egui::Ui) {
        if self.form.profile.source_tls != "plain" {
            return;
        }
        ui.group(|ui| {
            ui.label(RichText::new("INSECURE SOURCE TRANSPORT").strong().color(self.theme_colors().danger));
            ui.label("Plain IMAP can expose the source password and mailbox data in transit.");
            let response = ui.add_enabled(
                !self.running(),
                egui::Checkbox::new(
                    &mut self.form.profile.allow_insecure_source_transport,
                    "I understand and explicitly allow cleartext source transport",
                ),
            );
            response.on_hover_text(
                "Use IMAPS or STARTTLS whenever possible. This acknowledgement is required before any authenticated operation, including dry preflight, and is included in the preflight fingerprint.",
            );
        });
    }

    pub(crate) fn project_summary(&mut self, ui: &mut egui::Ui) {
        self.lifecycle_stepper(ui);
        if self.active_project_id().is_none() && self.bulk_jobs.is_empty() {
            let colors = self.theme_colors();
            ui.group(|ui| {
                ui.heading("Start a safe migration");
                ui.label(RichText::new("MailSwiftSync guides every migration through a reviewed preflight before any destination changes are allowed.").color(colors.text_secondary));
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    for (number, title, detail) in [
                        ("1", "Connect", "Configure source and destination"),
                        ("2", "Preflight", "Authenticate and review blockers"),
                        ("3", "Pilot", "Start with a small mailbox set"),
                    ] {
                        ui.group(|ui| {
                            ui.label(RichText::new(format!("{number}  {title}")).strong().color(colors.info));
                            ui.label(RichText::new(detail).size(11.0).color(colors.text_secondary));
                        });
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Import mailbox list").clicked() {
                        self.bulk_open = true;
                    }
                    ui.label(RichText::new("For one mailbox, continue with the migration plan below.").size(11.0).color(colors.text_secondary));
                });
            });
            ui.add_space(10.0);
        }
        if self.process_review_required {
            ui.group(|ui| {
                ui.label(RichText::new("PROCESS OWNERSHIP REVIEW REQUIRED").strong().color(self.theme_colors().danger));
                ui.label("MailSwiftSync could not prove that a previously recorded migration process is gone. Do not start another migration until you have checked the host process list and confirmed no MailSwiftSync engine remains.");
                if ui.button("I confirmed no unverified migration process remains").clicked() {
                    match self.store.clear_active_processes_after_review() {
                        Ok(()) => {
                            self.process_review_required = false;
                            self.set_status("Process review acknowledged; execution gates are available again.", StatusSeverity::Success);
                        }
                        Err(error) => self.set_status(format!("Could not clear reviewed process identities: {error}"), StatusSeverity::Error),
                    }
                }
            });
        }
        if !self.workspace_read_only {
            self.source_transport_warning(ui);
        }
        if self.workspace_read_only {
            ui.group(|ui| {
                ui.label(RichText::new("HISTORICAL PROJECT · READ ONLY").strong().color(self.theme_colors().info));
                ui.label("You are viewing durable history for this project. The editable migration plan and execution controls are detached until you start a new migration.");
                if ui.button("Start a new migration").clicked() {
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
                ui.heading("Migration workspace");
                ui.label(RichText::new(if self.form.dry_run { "PREFLIGHT" } else { "LIVE MIGRATION" }).strong().color(if self.form.dry_run { self.theme_colors().success } else { self.theme_colors().danger }));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("{passed}/{total} configuration items complete")).strong().color(if passed == total { self.theme_colors().success } else { self.theme_colors().danger }));
                });
            });
            ui.add_space(5.0);
            ui.label(RichText::new("Recommended next step: run preflight, review blockers, then select a small pilot mailbox.").color(self.theme_colors().text_secondary));
        });
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
                ui.label(RichText::new("MIGRATION LIFECYCLE").size(11.0).strong().color(self.theme_colors().text_secondary));
                if current == core::Phase::Attention {
                    ui.label(RichText::new("ATTENTION REQUIRED").strong().color(self.theme_colors().danger));
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
                            format_phase_name(*phase)
                        ))
                        .strong()
                        .color(color),
                    );
                }
            });
            if current == core::Phase::Attention {
                ui.label(RichText::new("A mailbox or run needs operator review. Normal lifecycle progress is paused until it is resolved.").size(11.0).color(self.theme_colors().danger));
            }
        });
    }
}
