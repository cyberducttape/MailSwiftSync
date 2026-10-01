use crate::ui::format_phase_name;
use crate::ui::status_color;
use crate::*;

/// The single next readiness step for a one-mailbox plan. Plan and Overview
/// both present this, so the operator never sees two competing next actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanWorkflowStep {
    Assess,
    TestAccounts,
    DryPreflight,
    StartLive,
}

fn plan_workflow_step(
    assessment_complete: bool,
    dovecot: bool,
    account_test_current: bool,
    live_preflight_current: bool,
) -> PlanWorkflowStep {
    if !assessment_complete {
        PlanWorkflowStep::Assess
    } else if !dovecot && !account_test_current {
        PlanWorkflowStep::TestAccounts
    } else if live_preflight_current {
        PlanWorkflowStep::StartLive
    } else {
        PlanWorkflowStep::DryPreflight
    }
}

fn plan_configuration_enabled(migration_running: bool) -> bool {
    !migration_running
}

fn selection_inspector_in_side_panel(
    view: WorkspaceView,
    has_selection: bool,
    inspector_open: bool,
    wide_layout: bool,
) -> bool {
    view == WorkspaceView::Mailboxes && has_selection && inspector_open && wide_layout
}

impl eframe::App for App {
    /// Runs before every frame and, unlike `ui`, also while the window is
    /// minimized or hidden, so a running migration keeps draining events.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        if ctx.input(|input| input.viewport().close_requested()) && self.running() {
            // The desktop owns the controller. Closing it would trigger the
            // platform process-containment policy and interrupt the engine;
            // do not let a window-manager close silently become a stop action.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_during_run_confirm_open = true;
        }
        // eframe only repaints on input. Background work reports through
        // channels drained by `poll`, and workers wait (with timeouts) for
        // durable acknowledgements, so keep frames coming while any is in
        // flight — otherwise an idle window stalls or fails a migration.
        if !self.deferred_events.is_empty() || !self.pending_db_events.is_empty() {
            ctx.request_repaint();
        } else if self.background_work_pending() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_zoom_factor(self.ui_scale);
        let colors = self.theme_colors();

        ctx.set_visuals(colors.visuals(self.dark_mode));

        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, colors.background);

        let header_frame = egui::Frame::side_top_panel(ui.style())
            .fill(colors.panel)
            .inner_margin(egui::Margin::symmetric(16, 10))
            .stroke(egui::Stroke::new(1.0, colors.quiet_border()));
        egui::Panel::top("project_header")
            .resizable(false)
            .show_separator_line(false)
            .frame(header_frame)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("MailSwiftSync")
                            .size(16.0)
                            .strong()
                            .color(colors.info),
                    );
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    let project_name = self
                        .ui_snapshot
                        .project
                        .as_ref()
                        .map(|project| project.name.as_str())
                        .unwrap_or_else(|| self.language.message("ui.no-project-selected"));
                    ui.label(egui::RichText::new(project_name).strong());
                    if let Some(project) = self.ui_snapshot.project.as_ref() {
                        crate::ui::pill(
                            ui,
                            self.language.text(format_phase_name(project.phase)),
                            colors.info,
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.running() && ui.button(self.language.message("ui.stop")).clicked() {
                            self.stop_confirm_open = true;
                            self.stop_confirm_focus_requested = false;
                        }
                        if self.ui_snapshot.is_stale() {
                            ui.label(
                                egui::RichText::new(
                                    self.ui_snapshot.stale_notice().unwrap_or_else(|| {
                                        self.language.message("ui.durable-view-is-stale").to_owned()
                                    }),
                                )
                                .color(colors.warning),
                            );
                        }
                        // Status text is stored as a String; translate it
                        // when it is a fixed catalog message.
                        let status_text = self
                            .language
                            .lookup(&self.status.text)
                            .unwrap_or(&self.status.text);
                        let status_response = ui.add(
                            egui::Label::new(
                                egui::RichText::new(status_text)
                                    .color(status_color(self.status.severity, colors)),
                            )
                            .truncate()
                            .sense(egui::Sense::click()),
                        );
                        status_response.clone().on_hover_text(status_text);
                        if status_response.clicked() {
                            self.activity_search = self.run_id.clone().unwrap_or_default();
                            self.active_view = WorkspaceView::Activity;
                        }
                    });
                });
            });

        let navigation_frame = egui::Frame::side_top_panel(ui.style())
            .fill(colors.panel)
            .inner_margin(egui::Margin::symmetric(10, 14))
            .stroke(egui::Stroke::new(1.0, colors.quiet_border()));
        let compact_navigation = ui.available_width() < 1040.0;
        egui::Panel::left("workspace_navigation")
            .resizable(false)
            .show_separator_line(false)
            .exact_size(if compact_navigation { 64.0 } else { 200.0 })
            .frame(navigation_frame)
            .show(ui, |ui| {
                if !compact_navigation {
                    crate::ui::section_label(ui, self.language.message("ui.workspace"));
                    ui.add_space(4.0);
                }
                for (view, icon, label) in [
                    (
                        WorkspaceView::Overview,
                        crate::ui::WorkspaceIcon::Overview,
                        "Overview",
                    ),
                    (WorkspaceView::Plan, crate::ui::WorkspaceIcon::Plan, "Plan"),
                    (
                        WorkspaceView::Mailboxes,
                        crate::ui::WorkspaceIcon::Mailboxes,
                        "Mailboxes",
                    ),
                    (
                        WorkspaceView::Activity,
                        crate::ui::WorkspaceIcon::Activity,
                        "Activity",
                    ),
                    (
                        WorkspaceView::Verification,
                        crate::ui::WorkspaceIcon::Verification,
                        "Verification",
                    ),
                ] {
                    let item = if compact_navigation {
                        crate::ui::nav_icon_item(
                            ui,
                            self.active_view == view,
                            icon,
                            self.language.text(label),
                        )
                    } else {
                        crate::ui::nav_item(
                            ui,
                            self.active_view == view,
                            icon,
                            self.language.text(label),
                        )
                    };
                    if item.clicked() {
                        self.active_view = view;
                        self.refresh_ui_snapshot_now();
                        // Re-check readiness staleness on the next frame so a
                        // page never shows observations for an edited plan.
                        self.capability_staleness_checked_at = None;
                    }
                }
                ui.add_space(12.0);
                if !compact_navigation {
                    crate::ui::section_label(ui, self.language.message("ui.manage"));
                    ui.add_space(4.0);
                }
                let projects_item = if compact_navigation {
                    crate::ui::nav_icon_item(
                        ui,
                        self.projects_open,
                        crate::ui::WorkspaceIcon::Projects,
                        self.language.message("ui.projects"),
                    )
                } else {
                    crate::ui::nav_item(
                        ui,
                        self.projects_open,
                        crate::ui::WorkspaceIcon::Projects,
                        self.language.message("ui.projects"),
                    )
                };
                if projects_item.clicked() {
                    self.projects_open = true;
                }
                let settings_item = if compact_navigation {
                    crate::ui::nav_icon_item(
                        ui,
                        self.settings_open,
                        crate::ui::WorkspaceIcon::Settings,
                        self.language.message("ui.settings"),
                    )
                } else {
                    crate::ui::nav_item(
                        ui,
                        self.settings_open,
                        crate::ui::WorkspaceIcon::Settings,
                        self.language.message("ui.settings"),
                    )
                };
                if settings_item.clicked() {
                    self.settings_open = true;
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    let (dot, text) = if self.running() {
                        (
                            colors.success,
                            self.language.message("ui.migration-running"),
                        )
                    } else {
                        (
                            colors.text_secondary,
                            self.language.message("ui.no-active-migration"),
                        )
                    };
                    ui.horizontal(|ui| {
                        let (dot_rect, _) =
                            ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot_rect.center(), 3.0, dot);
                        if !compact_navigation {
                            ui.label(
                                egui::RichText::new(text)
                                    .small()
                                    .color(colors.text_secondary),
                            );
                        }
                    });
                });
            });

        if self.active_view != WorkspaceView::Mailboxes || self.bulk_selection_is_empty() {
            self.bulk_inspector_open = false;
        }
        let wide_mailbox_layout = ui.available_width() >= 1120.0;
        self.bulk_inspector_side_panel = selection_inspector_in_side_panel(
            self.active_view,
            !self.bulk_selection_is_empty(),
            self.bulk_inspector_open,
            wide_mailbox_layout,
        );
        if self.active_view == WorkspaceView::Mailboxes
            && !self.bulk_selection_is_empty()
            && self.bulk_inspector_side_panel
        {
            // Once per frame, before the drawer that renders it.
            self.refresh_bulk_selection_view();
            egui::Panel::right("selection_review_drawer")
                .resizable(true)
                .default_size(320.0)
                .show(ui, |ui| {
                    if let Some(job_id) = self.selection_review_drawer(ui) {
                        self.job_id = Some(job_id);
                        self.active_view = WorkspaceView::Verification;
                    }
                });
        }

        let content_frame = egui::Frame::central_panel(ui.style())
            .fill(colors.background)
            .inner_margin(egui::Margin::symmetric(24, 18));
        egui::CentralPanel::default()
            .frame(content_frame)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Keep line lengths readable on wide displays.
                        ui.set_max_width(ui.available_width().min(1120.0));
                        match self.active_view {
                            WorkspaceView::Overview => self.overview_view(ui),
                            WorkspaceView::Plan => self.plan_view(ui),
                            WorkspaceView::Mailboxes => self.mailbox_view(ui),
                            WorkspaceView::Activity => self.activity_view(ui),
                            WorkspaceView::Verification => self.verification_view(ui),
                        }
                    });
            });

        self.projects_dialog(&ctx);
        self.settings_dialog(&ctx);
        self.keyring_dialog(&ctx);
        self.credential_delete_confirmation(&ctx);
        self.engine_dialog(&ctx);
        self.preview(&ctx);
        self.advanced_dialog(&ctx);
        self.live_confirmation(&ctx);
        self.stop_confirmation(&ctx);
        self.bulk_sheet_selection(&ctx);
        self.bulk_plaintext_import_confirmation(&ctx);
        self.bulk_clear_confirmation(&ctx);
        self.bulk_import_confirmation(&ctx);
        self.bulk_live_confirmation(&ctx);
        self.close_during_run_confirmation(&ctx);
    }
}

impl App {
    fn close_during_run_confirmation(&mut self, ctx: &egui::Context) {
        if !self.close_during_run_confirm_open {
            return;
        }
        if !self.running() {
            self.close_during_run_confirm_open = false;
            return;
        }
        let mut open = true;
        egui::Modal::new(egui::Id::new("close_during_run_confirmation"))
            .show(ctx, |ui| {
                let modal_heading = ui.heading(self.language.message("ui.migration-still-running"));
crate::ui::name_modal(ui, &modal_heading);
                ui.label(self.language.message("ui.this-desktop-owns-the-migration-controller-closing-the-window-will-interrup-27f78da0f7"));
                ui.label(self.language.message("ui.to-stop-safely-use-stop-migration-in-activity-and-wait-for-the-run-to-finis-e79da693d4"));
                ui.horizontal(|ui| {
                    let stay = ui.button(self.language.message("ui.keep-window-open"));
                    if stay.clicked() {
                        open = false;
                    }
                    if ui.button(self.language.message("ui.go-to-activity")).clicked() {
                        self.active_view = WorkspaceView::Activity;
                        open = false;
                    }
                });
            });
        self.close_during_run_confirm_open = open;
    }

    fn current_dry_preflight_ready(&self) -> bool {
        let Some(job_id) = self.job_id.as_deref() else {
            return false;
        };
        let current_credentials = self.form.credential_binding_fingerprint();
        if self.preflight_credential_fingerprint.as_deref() != Some(current_credentials.as_str()) {
            return false;
        }
        let current_plan = crate::plan_identity::fingerprint_digest(&self.form.plan_fingerprint());
        let durable_plan_matches = matches!(
            self.store.preflight_plan(job_id),
            Ok(Some(ref fingerprint)) if fingerprint == &current_plan
        );
        let mailbox_ready = matches!(
            self.store.mailbox_state(job_id),
            Ok(Some(ref state)) if state == "ready" || state == "delta_required"
        );
        let project_ready = self.project_id.as_deref().is_some_and(|project_id| {
            self.store
                .project(project_id)
                .ok()
                .flatten()
                .is_some_and(|project| {
                    matches!(
                        project.phase,
                        core::Phase::Preflight
                            | core::Phase::Pilot
                            | core::Phase::Seed
                            | core::Phase::CatchUp
                            | core::Phase::FinalDelta
                            | core::Phase::Verification
                    )
                })
        });
        durable_plan_matches && mailbox_ready && project_ready
    }

    fn plan_view(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        crate::ui::page_header(
            ui,
            self.language.message("ui.migration-plan"),
            self.language
                .text("Choose the systems and accounts involved in this migration."),
        );
        if self.running() {
            ui.label(
                egui::RichText::new(self.language.message("ui.plan-locked-during-migration"))
                    .small()
                    .color(colors.warning),
            );
            ui.add_space(8.0);
        }
        let dovecot = self.form.engine() == core::Engine::Dovecot;
        ui.add_enabled_ui(plan_configuration_enabled(self.running()), |ui| {
        crate::ui::card(ui, |ui| {
            crate::ui::form_row(ui, self.language.message("ui.plan-name"), |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.form.profile.name).desired_width(360.0))
            });
            if dovecot {
                // The expert method lives under Advanced; never leave it as
                // hidden state that changes destination semantics.
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {}",
                        self.language.message("ui.migration-method"),
                        self.language.message("ui.local-dovecot-migration-doveadm")
                    ))
                    .small()
                    .color(colors.warning),
                );
            }
        });
        crate::ui::card(ui, |ui| {
            egui::CollapsingHeader::new(
                egui::RichText::new(self.language.message("ui.advanced-migration-settings"))
                    .strong(),
            )
            .id_salt("advanced_migration_settings")
            .show(ui, |ui| {
                crate::ui::section_label(ui, self.language.message("ui.migration-method"));
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(
                        &mut self.form.profile.engine,
                        core::Engine::ImapSync,
                        self.language.message("ui.standard-imap-migration-imapsync"),
                    );
                    ui.radio_value(
                        &mut self.form.profile.engine,
                        core::Engine::Dovecot,
                        self.language.message("ui.local-dovecot-migration-doveadm"),
                    );
                });
                ui.label(
                    egui::RichText::new(self.language.text(if self.form.engine() == core::Engine::Dovecot {
                        "Advanced method: runs local Dovecot tools and follows Dovecot-specific destination semantics."
                    } else {
                        "Recommended for provider-to-provider moves; runs the imapsync engine."
                    }))
                    .small()
                    .color(colors.text_secondary),
                );
                ui.add_space(8.0);
                // Buttons carry their own names; "Tools" is a group caption,
                // not a field label, so it is not linked to them.
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(self.language.message("ui.tools"))
                            .color(ui.visuals().weak_text_color()),
                    );
                    if ui
                        .button(format!(
                            "{}…",
                            self.language.message("ui.os-keyring-credentials")
                        ))
                        .clicked()
                    {
                        self.keyring_open = true;
                    }
                    if ui
                        .button(format!(
                            "{}…",
                            self.language.message("ui.choose-migration-engine")
                        ))
                        .clicked()
                    {
                        self.engine_open = true;
                    }
                });
            });
        });
        ui.add_space(16.0);
        if ui.available_width() < 900.0 {
            self.endpoint_plan_panel(ui, true, dovecot, colors.info);
            self.endpoint_plan_panel(ui, false, dovecot, colors.success);
        } else {
            ui.columns(2, |columns| {
                self.endpoint_plan_panel(&mut columns[0], true, dovecot, colors.info);
                self.endpoint_plan_panel(&mut columns[1], false, dovecot, colors.success);
            });
        }
        ui.add_space(12.0);
        let mutation_policy = self.form.profile.destination_mutation_policy();
        let folder_summary = if self.form.profile.justfolders {
            self.language.message("ui.folders-only")
        } else if self.form.profile.automap {
            self.language.message("ui.mail-and-standard-folder-mapping")
        } else {
            self.language.message("ui.mail-and-original-folder-names")
        };
        let verification_summary = if dovecot {
            self.language.message("ui.level-1-aggregate-evidence")
        } else if self.form.profile.body_hash_verification {
            self.language
                .message("ui.level-3-bounded-content-fingerprints")
        } else {
            self.language
                .message("ui.level-2-metadata-reconciliation-plan-dependent")
        };
        crate::ui::card(ui, |ui| {
            ui.heading(self.language.message("ui.migration-policy"));
            egui::Grid::new("migration_policy_summary")
                .num_columns(2)
                .spacing([16.0, 5.0])
                .show(ui, |ui| {
                    ui.label(self.language.message("ui.folder-handling"));
                    ui.label(folder_summary);
                    ui.end_row();
                    ui.label(self.language.message("ui.destination-behavior"));
                    ui.label(
                        egui::RichText::new(self.language.text(mutation_policy.label())).color(
                            if mutation_policy.may_remove_destination_state() {
                                colors.danger
                            } else {
                                colors.success
                            },
                        ),
                    );
                    ui.end_row();
                    ui.label(self.language.message("ui.verification"));
                    ui.label(verification_summary);
                    ui.end_row();
                });
        });
        ui.add_space(8.0);
        egui::CollapsingHeader::new(
            self.language
                .message("ui.provider-qualification-and-detailed-simulation"),
        )
        .id_salt("provider_qualification_and_simulation")
        .show(ui, |ui| {
            self.provider_qualification_card(ui);
            ui.add_space(8.0);
            self.migration_simulation_card(ui);
        });
        if !dovecot {
            ui.add_space(16.0);
            egui::CollapsingHeader::new(self.language.message("ui.advanced-engine-options"))
                .id_salt("advanced_engine_options")
                .show(ui, |ui| {
                    crate::ui::card(ui, |ui| {
                        crate::ui::section_label(ui, self.language.message("ui.imapsync-options"));
                        ui.horizontal_wrapped(|ui| {
                            ui.checkbox(
                                &mut self.form.profile.automap,
                                self.language
                                    .message("ui.map-standard-folders-automatically"),
                            );
                            ui.checkbox(
                                &mut self.form.profile.justfolders,
                                self.language.message("ui.folders-only"),
                            );
                            ui.checkbox(
                                &mut self.form.profile.addheader,
                                self.language
                                    .message("ui.add-message-id-header-when-needed"),
                            );
                        });
                        Self::text_field(
                            ui,
                            self.language.message("ui.extra-imapsync-options"),
                            &mut self.form.profile.extra_options,
                        );
                        crate::ui::form_row(
                            ui,
                            self.language.message("ui.imapsync-executable"),
                            |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.form.profile.imapsync_path,
                                    )
                                    .hint_text(
                                        self.language
                                            .text("Leave empty to use imapsync from PATH."),
                                    )
                                    .desired_width(f32::INFINITY),
                                )
                            },
                        );
                    })
                });
        }
        // Both engines: Dovecot backup mirrors, imapsync --delete2 deletes.
        let policy = self.form.profile.destination_mutation_policy();
        if policy.may_remove_destination_state() {
            ui.add_space(12.0);
            crate::ui::card(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "⚠ {}",
                        self.language.message("ui.destination-state-may-be-removed")
                    ))
                    .strong()
                    .color(colors.danger),
                );
                ui.label(
                    egui::RichText::new(self.language.text(policy.warning())).color(colors.danger),
                );
            });
        }
        ui.add_space(16.0);
        crate::ui::card(ui, |ui| self.provider_runbook_panel(ui));
        });
        ui.add_space(16.0);
        crate::ui::card(ui, |ui| {
            crate::ui::section_label(ui, self.language.message("ui.next-readiness-action"));
            self.plan_workflow_controls(ui);
        });
        ui.add_space(8.0);
        egui::CollapsingHeader::new(self.language.message("ui.plan-tools"))
            .id_salt("plan_tools")
            .show(ui, |ui| {
                crate::ui::card(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button(self.language.message("ui.save-create-project"))
                            .clicked()
                        {
                            self.create_project();
                        }
                        if ui
                            .button(self.language.message("ui.save-profile"))
                            .clicked()
                        {
                            match self.form.save() {
                                Ok(()) => self.set_status(
                                    self.language
                                        .text("Profile saved without credential material."),
                                    StatusSeverity::Success,
                                ),
                                Err(error) => self.set_status(
                                    format!(
                                        "{}: {error}",
                                        self.language.message("ui.could-not-save-profile")
                                    ),
                                    StatusSeverity::Error,
                                ),
                            }
                        }
                        if ui
                            .button(self.language.message("ui.preview-command"))
                            .clicked()
                        {
                            self.preview = true;
                        }
                        if ui.button(self.language.message("ui.advanced")).clicked() {
                            self.advanced_open = true;
                        }
                    });
                });
            });
    }

    pub(crate) fn current_plan_workflow_step(&self) -> PlanWorkflowStep {
        let current_plan = crate::plan_identity::fingerprint_digest(&self.form.plan_fingerprint());
        let account_test_current = crate::controller::capability_observation_matches(
            self.capability_observation_fingerprint.as_deref(),
            &current_plan,
        ) && self.source_capabilities.is_some()
            && self.destination_capabilities.is_some();
        plan_workflow_step(
            !self.preflight.is_empty(),
            self.form.engine() == core::Engine::Dovecot,
            account_test_current,
            self.current_dry_preflight_ready(),
        )
    }

    pub(crate) fn plan_workflow_hint(step: PlanWorkflowStep) -> &'static str {
        match step {
            PlanWorkflowStep::Assess => "Review the proposed plan before testing either account.",
            PlanWorkflowStep::TestAccounts => {
                "Authenticate both IMAP accounts and inspect folder namespaces."
            }
            PlanWorkflowStep::DryPreflight => {
                "Run the engine's non-writing preflight after account testing."
            }
            PlanWorkflowStep::StartLive => {
                "The exact plan passed dry preflight; live execution still requires explicit confirmation."
            }
        }
    }

    /// Render the one primary readiness button (or Stop while running) and
    /// its hint. Shared by the Plan page and the Overview lifecycle card.
    pub(crate) fn plan_workflow_controls(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        let workflow_step = self.current_plan_workflow_step();
        if self.running() {
            if ui
                .button(self.language.message("ui.stop-migration"))
                .clicked()
            {
                self.stop_confirm_open = true;
                self.stop_confirm_focus_requested = false;
            }
        } else {
            let label = match workflow_step {
                PlanWorkflowStep::Assess => self.language.message("ui.assess-configuration"),
                PlanWorkflowStep::TestAccounts => {
                    if self.capability_receiver.is_some() {
                        self.language.message("ui.testing-accounts")
                    } else {
                        self.language
                            .message("ui.test-accounts-and-inspect-namespaces")
                    }
                }
                PlanWorkflowStep::DryPreflight => {
                    self.language.message("ui.run-preflight-3cd0b7eb")
                }
                PlanWorkflowStep::StartLive => self.language.message("ui.start-live-migration"),
            };
            let in_flight = workflow_step == PlanWorkflowStep::TestAccounts
                && self.capability_receiver.is_some();
            let clicked = if in_flight {
                ui.add_enabled(false, egui::Button::new(label)).clicked()
            } else {
                crate::ui::primary_button(ui, label).clicked()
            };
            if clicked {
                match workflow_step {
                    PlanWorkflowStep::Assess => self.assess_plan(),
                    PlanWorkflowStep::TestAccounts => self.start_capability_probe(),
                    PlanWorkflowStep::DryPreflight => {
                        self.form.dry_run = true;
                        self.start();
                    }
                    PlanWorkflowStep::StartLive => {
                        self.form.dry_run = false;
                        self.start();
                    }
                }
            }
        }
        ui.label(
            egui::RichText::new(self.language.text(Self::plan_workflow_hint(workflow_step)))
                .small()
                .color(colors.text_secondary),
        );
    }

    fn endpoint_plan_panel(
        &mut self,
        ui: &mut egui::Ui,
        source: bool,
        dovecot: bool,
        color: egui::Color32,
    ) {
        self.account_card(ui, source, dovecot, color);
        let language = self.language;
        let fixed_endpoint = (source || !dovecot)
            && !matches!(
                self.effective_provider(source),
                ProviderPreset::GenericImap | ProviderPreset::CpanelDovecot
            );
        ui.add_space(10.0);
        let id_salt = if source {
            "source_connection_settings"
        } else {
            "destination_connection_settings"
        };
        egui::CollapsingHeader::new(language.message("ui.advanced-connection-settings"))
            .id_salt(id_salt)
            .show(ui, |ui| {
                crate::ui::card(ui, |ui| {
                    if source || !dovecot {
                        let profile = &mut self.form.profile;
                        let (host, auth) = if source {
                            (&mut profile.source_host, &mut profile.source_auth)
                        } else {
                            (&mut profile.destination_host, &mut profile.destination_auth)
                        };
                        if fixed_endpoint {
                            Self::text_field(ui, language.message("ui.server"), host);
                        }
                        crate::ui::form_row(ui, language.message("ui.authentication"), |ui| {
                            egui::ComboBox::from_id_salt(("auth_method", source))
                                .selected_text(if auth_method_is_oauth(auth) {
                                    "OAuth 2.0 / XOAUTH2"
                                } else {
                                    language.message("ui.password")
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        auth,
                                        "password".into(),
                                        language.message("ui.password"),
                                    );
                                    ui.selectable_value(
                                        auth,
                                        "oauth2".into(),
                                        "OAuth 2.0 / XOAUTH2",
                                    );
                                })
                        });
                    }
                    if source {
                        Self::text_field(
                            ui,
                            language.message("ui.port"),
                            &mut self.form.profile.source_port,
                        );
                        Self::text_field(
                            ui,
                            language.message("ui.credential-id"),
                            &mut self.form.profile.source_credential_id,
                        );
                        Self::text_field(
                            ui,
                            language.message("ui.ca-bundle"),
                            &mut self.form.profile.source_ca_bundle,
                        );
                        if !dovecot {
                            Self::text_field(
                                ui,
                                language.message("ui.certificate-pin-sha-256"),
                                &mut self.form.profile.source_certificate_pin_sha256,
                            );
                        }
                        Self::tls_field(
                            ui,
                            language.message("ui.tls"),
                            &mut self.form.profile.source_tls,
                            "source_tls",
                        );
                        ui.checkbox(
                            &mut self.form.profile.allow_insecure_source_transport,
                            language.message("ui.allow-insecure-source-transport-review-carefully"),
                        );
                    } else if dovecot {
                        ui.label(
                            egui::RichText::new(
                                language.message("ui.destination-local-dovecot-storage"),
                            )
                            .color(self.theme_colors().text_secondary),
                        );
                        Self::text_field(
                            ui,
                            language.message("ui.credential-id"),
                            &mut self.form.profile.destination_credential_id,
                        );
                    } else {
                        Self::text_field(
                            ui,
                            language.message("ui.port"),
                            &mut self.form.profile.destination_port,
                        );
                        Self::text_field(
                            ui,
                            language.message("ui.credential-id"),
                            &mut self.form.profile.destination_credential_id,
                        );
                        Self::text_field(
                            ui,
                            language.message("ui.ca-bundle"),
                            &mut self.form.profile.destination_ca_bundle,
                        );
                        Self::text_field(
                            ui,
                            language.message("ui.certificate-pin-sha-256"),
                            &mut self.form.profile.destination_certificate_pin_sha256,
                        );
                        Self::tls_field(
                            ui,
                            language.message("ui.tls"),
                            &mut self.form.profile.destination_tls,
                            "destination_tls",
                        );
                    }
                });
            });
    }

    fn text_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
        crate::ui::form_row(ui, label, |ui| {
            ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY))
        });
    }

    fn tls_field(ui: &mut egui::Ui, label: &str, value: &mut String, id_salt: &str) {
        crate::ui::form_row(ui, label, |ui| {
            egui::ComboBox::from_id_salt(id_salt)
                .selected_text(value.as_str())
                .show_ui(ui, |ui| {
                    for mode in ["imaps", "starttls", "plain"] {
                        ui.selectable_value(value, mode.to_owned(), mode);
                    }
                })
        });
    }

    fn provider_runbook_panel(&self, ui: &mut egui::Ui) {
        ui.collapsing(
            self.language.message("ui.provider-readiness-runbook"),
            |ui| {
                // Built only while expanded; the runbook allocates every step.
                let runbook = crate::core::provider_runbooks::RunbookGenerator::generate(
                    self.source_provider.runbook_name(),
                    self.destination_provider.runbook_name(),
                );
                ui.label(
                    self.language
                        .text("Read-only operational guidance. Preflight and live admission remain authoritative."),
                );
                ui.label(
                    self.language
                        .text("Guidance version: {}")
                        .replace("{}", &runbook.guidance_version),
                );
                ui.label(
                    egui::RichText::new(runbook.provider)
                        .strong()
                        .color(self.theme_colors().info),
                );
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        for step in &runbook.pre_migration_checklist {
                            ui.separator();
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}. {}",
                                    step.step_number, step.action
                                ))
                                .strong(),
                            );
                            ui.label(format!("{} {}", self.language.message("ui.why"), step.why));
                            ui.label(format!(
                                "{} {}",
                                self.language.message("ui.success"),
                                step.success_indicator
                            ));
                        }
                        if !runbook.known_issues.is_empty() {
                            ui.separator();
                            ui.label(
                                egui::RichText::new(self.language.message("ui.known-provider-issues"))
                                    .strong()
                                    .color(self.theme_colors().warning),
                            );
                            for issue in &runbook.known_issues {
                                ui.label(format!("{}: {}", issue.issue, issue.workaround));
                            }
                        }
                    });
            },
        );
    }
}

#[cfg(test)]
mod workflow_tests {
    use super::{
        PlanWorkflowStep, plan_configuration_enabled, plan_workflow_step,
        selection_inspector_in_side_panel,
    };
    use crate::ui::WorkspaceView;

    #[test]
    fn plan_actions_follow_assess_test_preflight_live_sequence() {
        assert_eq!(
            plan_workflow_step(false, false, false, false),
            PlanWorkflowStep::Assess
        );
        assert_eq!(
            plan_workflow_step(true, false, false, false),
            PlanWorkflowStep::TestAccounts
        );
        assert_eq!(
            plan_workflow_step(true, false, true, false),
            PlanWorkflowStep::DryPreflight
        );
        assert_eq!(
            plan_workflow_step(true, false, true, true),
            PlanWorkflowStep::StartLive
        );
    }

    #[test]
    fn local_dovecot_skips_unsupported_imap_probe_step() {
        assert_eq!(
            plan_workflow_step(true, true, false, false),
            PlanWorkflowStep::DryPreflight
        );
    }

    #[test]
    fn plan_configuration_locks_only_while_a_migration_is_running() {
        assert!(plan_configuration_enabled(false));
        assert!(!plan_configuration_enabled(true));
    }

    #[test]
    fn batch_inspector_uses_a_side_panel_only_when_requested_and_wide() {
        assert!(!selection_inspector_in_side_panel(
            WorkspaceView::Mailboxes,
            true,
            false,
            true
        ));
        assert!(!selection_inspector_in_side_panel(
            WorkspaceView::Mailboxes,
            true,
            true,
            false
        ));
        assert!(!selection_inspector_in_side_panel(
            WorkspaceView::Mailboxes,
            false,
            true,
            true
        ));
        assert!(!selection_inspector_in_side_panel(
            WorkspaceView::Overview,
            true,
            true,
            true
        ));
        assert!(selection_inspector_in_side_panel(
            WorkspaceView::Mailboxes,
            true,
            true,
            true
        ));
    }

    #[test]
    fn active_migration_cancels_window_close_request() {
        use crate::ui::App;
        use eframe::App as EframeApp;
        use eframe::egui::{Context, RawInput, ViewportCommand, ViewportEvent, ViewportId};

        let state_path = std::env::temp_dir().join(format!(
            "mailswiftsync-close-guard-{}.db",
            uuid::Uuid::new_v4()
        ));
        let mut app = App::from_state_path(Some(&state_path));
        let (_sender, receiver) = std::sync::mpsc::channel();
        app.receiver = Some(receiver);
        let context = Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let mut input = RawInput::default();
        input
            .viewports
            .entry(ViewportId::ROOT)
            .or_default()
            .events
            .push(ViewportEvent::Close);
        let output = context.run_ui(input, |ui| EframeApp::logic(&mut app, ui.ctx(), &mut frame));

        assert!(app.close_during_run_confirm_open);
        assert!(
            output.viewport_output[&ViewportId::ROOT]
                .commands
                .iter()
                .any(|command| matches!(command, ViewportCommand::CancelClose))
        );

        drop(app);
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(state_path.with_extension("lock"));
    }
}
