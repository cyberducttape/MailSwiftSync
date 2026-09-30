use crate::ui::format_phase_name;
use crate::ui::status_color;
use crate::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanWorkflowStep {
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

impl eframe::App for App {
    /// Runs before every frame and, unlike `ui`, also while the window is
    /// minimized or hidden, so a running migration keeps draining events.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
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
                        .unwrap_or_else(|| self.language.text("No project selected"));
                    ui.label(egui::RichText::new(project_name).strong());
                    if let Some(project) = self.ui_snapshot.project.as_ref() {
                        crate::ui::pill(
                            ui,
                            self.language.text(format_phase_name(project.phase)),
                            colors.info,
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.running() && ui.button(self.language.text("Stop")).clicked() {
                            self.stop_confirm_open = true;
                            self.stop_confirm_focus_requested = false;
                        }
                        if self.ui_snapshot.is_stale() {
                            ui.label(
                                egui::RichText::new(
                                    self.ui_snapshot.stale_notice().unwrap_or_else(|| {
                                        self.language.text("Durable view is stale").to_owned()
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
                    crate::ui::section_label(ui, self.language.text("Workspace"));
                    ui.add_space(4.0);
                }
                for (view, icon, label) in [
                    (WorkspaceView::Overview, "📊", "Overview"),
                    (WorkspaceView::Plan, "📋", "Plan"),
                    (WorkspaceView::Mailboxes, "✉", "Mailboxes"),
                    (WorkspaceView::Activity, "⏱", "Activity"),
                    (WorkspaceView::Verification, "✔", "Verification"),
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
                    crate::ui::section_label(ui, self.language.text("Manage"));
                    ui.add_space(4.0);
                }
                let projects_item = if compact_navigation {
                    crate::ui::nav_icon_item(
                        ui,
                        self.projects_open,
                        "🗄",
                        self.language.text("Projects"),
                    )
                } else {
                    crate::ui::nav_item(ui, self.projects_open, "🗄", self.language.text("Projects"))
                };
                if projects_item.clicked() {
                    self.projects_open = true;
                }
                let settings_item = if compact_navigation {
                    crate::ui::nav_icon_item(
                        ui,
                        self.settings_open,
                        "⚙",
                        self.language.text("Settings"),
                    )
                } else {
                    crate::ui::nav_item(ui, self.settings_open, "⚙", self.language.text("Settings"))
                };
                if settings_item.clicked() {
                    self.settings_open = true;
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    let (dot, text) = if self.running() {
                        (colors.success, self.language.text("Migration running"))
                    } else {
                        (
                            colors.text_secondary,
                            self.language.text("No active migration"),
                        )
                    };
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("⏺").color(dot).small());
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

        let wide_mailbox_layout = ui.available_width() >= 1120.0;
        if self.active_view == WorkspaceView::Mailboxes
            && !self.bulk_selected_ids.is_empty()
            && wide_mailbox_layout
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
        self.bulk_clear_confirmation(&ctx);
        self.bulk_import_confirmation(&ctx);
        self.bulk_live_confirmation(&ctx);
    }
}

impl App {
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
            self.language.text("Migration plan"),
            self.language
                .text("Choose the systems and accounts involved in this migration."),
        );
        crate::ui::card(ui, |ui| {
            crate::ui::section_label(ui, self.language.text("Migration method"));
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(
                    &mut self.form.profile.engine,
                    core::Engine::ImapSync,
                    self.language.text("Standard IMAP migration (imapsync)"),
                );
                ui.radio_value(
                    &mut self.form.profile.engine,
                    core::Engine::Dovecot,
                    self.language.text("Local Dovecot migration (doveadm)"),
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
            crate::ui::form_row(ui, self.language.text("Plan name"), |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.form.profile.name).desired_width(360.0))
            });
            ui.checkbox(
                &mut self.form.dry_run,
                self.language.text("Dry run / preflight"),
            );
        });
        crate::ui::card(ui, |ui| {
            egui::CollapsingHeader::new(
                egui::RichText::new(self.language.text("Advanced migration settings")).strong(),
            )
            .id_salt("advanced_migration_settings")
            .show(ui, |ui| {
                crate::ui::form_row(ui, self.language.text("Tools"), |ui| {
                    if ui
                        .button(format!("{}…", self.language.text("OS keyring credentials")))
                        .clicked()
                    {
                        self.keyring_open = true;
                    }
                    if ui
                        .button(format!(
                            "{}…",
                            self.language.text("Choose migration engine")
                        ))
                        .clicked()
                    {
                        self.engine_open = true;
                    }
                });
            });
        });
        ui.add_space(16.0);
        let dovecot = self.form.engine() == core::Engine::Dovecot;
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
        self.provider_qualification_card(ui);
        ui.add_space(12.0);
        self.migration_simulation_card(ui);
        if !dovecot {
            ui.add_space(16.0);
            egui::CollapsingHeader::new(self.language.text("Advanced engine options"))
                .id_salt("advanced_engine_options")
                .show(ui, |ui| {
                    crate::ui::card(ui, |ui| {
                        crate::ui::section_label(ui, self.language.text("imapsync options"));
                        ui.horizontal_wrapped(|ui| {
                            ui.checkbox(
                                &mut self.form.profile.automap,
                                self.language.text("Map standard folders automatically"),
                            );
                            ui.checkbox(
                                &mut self.form.profile.justfolders,
                                self.language.text("Folders only"),
                            );
                            ui.checkbox(
                                &mut self.form.profile.addheader,
                                self.language.text("Add Message-ID header when needed"),
                            );
                        });
                        Self::text_field(
                            ui,
                            self.language.text("Extra imapsync options"),
                            &mut self.form.profile.extra_options,
                        );
                        crate::ui::form_row(ui, self.language.text("imapsync executable"), |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.form.profile.imapsync_path)
                                    .hint_text(
                                        self.language
                                            .text("Leave empty to use imapsync from PATH."),
                                    )
                                    .desired_width(f32::INFINITY),
                            )
                        });
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
                        self.language.text("DESTINATION STATE MAY BE REMOVED")
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
        ui.add_space(16.0);
        let current_plan = crate::plan_identity::fingerprint_digest(&self.form.plan_fingerprint());
        let account_test_current = crate::controller::capability_observation_matches(
            self.capability_observation_fingerprint.as_deref(),
            &current_plan,
        ) && self.source_capabilities.is_some()
            && self.destination_capabilities.is_some();
        let workflow_step = plan_workflow_step(
            !self.preflight.is_empty(),
            dovecot,
            account_test_current,
            self.current_dry_preflight_ready(),
        );
        crate::ui::card(ui, |ui| {
            crate::ui::section_label(ui, self.language.text("MIGRATION WORKFLOW"));
            if self.running() {
                if ui.button(self.language.text("Stop migration")).clicked() {
                    self.stop_confirm_open = true;
                    self.stop_confirm_focus_requested = false;
                }
            } else {
                let label = match workflow_step {
                    PlanWorkflowStep::Assess => self.language.text("Assess configuration"),
                    PlanWorkflowStep::TestAccounts => {
                        if self.capability_receiver.is_some() {
                            self.language.text("Testing accounts…")
                        } else {
                            self.language.text("Test accounts and inspect namespaces")
                        }
                    }
                    PlanWorkflowStep::DryPreflight => self.language.text("Run preflight"),
                    PlanWorkflowStep::StartLive => self.language.text("Start live migration"),
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
            let workflow_hint = match workflow_step {
                PlanWorkflowStep::Assess => {
                    "Review the proposed plan before testing either account."
                }
                PlanWorkflowStep::TestAccounts => {
                    "Authenticate both IMAP accounts and inspect folder namespaces."
                }
                PlanWorkflowStep::DryPreflight => {
                    "Run the engine's non-writing preflight after account testing."
                }
                PlanWorkflowStep::StartLive => {
                    "The exact plan passed dry preflight; live execution still requires explicit confirmation."
                }
            };
            ui.label(
                egui::RichText::new(self.language.text(workflow_hint))
                    .small()
                    .color(colors.text_secondary),
            );
        });
        ui.add_space(8.0);
        egui::CollapsingHeader::new(self.language.text("Plan tools"))
            .id_salt("plan_tools")
            .show(ui, |ui| {
                crate::ui::card(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button(self.language.text("Save / create project"))
                            .clicked()
                        {
                            self.create_project();
                        }
                        if ui.button(self.language.text("Save profile")).clicked() {
                            match self.form.save() {
                                Ok(()) => self.set_status(
                                    self.language
                                        .text("Profile saved without credential material."),
                                    StatusSeverity::Success,
                                ),
                                Err(error) => self.set_status(
                                    format!(
                                        "{}: {error}",
                                        self.language.text("Could not save profile")
                                    ),
                                    StatusSeverity::Error,
                                ),
                            }
                        }
                        if ui.button(self.language.text("Preview command")).clicked() {
                            self.preview = true;
                        }
                        if ui.button(self.language.text("Advanced")).clicked() {
                            self.advanced_open = true;
                        }
                    });
                });
            });
    }

    fn endpoint_plan_panel(
        &mut self,
        ui: &mut egui::Ui,
        source: bool,
        dovecot: bool,
        color: egui::Color32,
    ) {
        self.provider_field(ui, source);
        let language = self.language;
        let saved_credential = if source {
            !self.form.profile.source_credential_id.trim().is_empty()
        } else {
            !self
                .form
                .profile
                .destination_credential_id
                .trim()
                .is_empty()
        };
        if source {
            super::account::render_account(
                ui,
                language,
                language.text("Source account"),
                &mut self.form.profile.source_host,
                &mut self.form.profile.source_user,
                &mut self.form.profile.source_auth,
                &mut self.form.source_password,
                saved_credential,
                color,
            );
        } else {
            // Native Dovecot writes to local storage; destination IMAP
            // authentication controls do not apply in that mode.
            super::account::render_account(
                ui,
                language,
                language.text(if dovecot {
                    "Local Dovecot destination"
                } else {
                    "Destination account"
                }),
                &mut self.form.profile.destination_host,
                &mut self.form.profile.destination_user,
                &mut self.form.profile.destination_auth,
                &mut self.form.destination_password,
                saved_credential,
                color,
            );
        }
        ui.add_space(10.0);
        let id_salt = if source {
            "source_connection_settings"
        } else {
            "destination_connection_settings"
        };
        egui::CollapsingHeader::new(language.text("Advanced connection settings"))
            .id_salt(id_salt)
            .show(ui, |ui| {
                crate::ui::card(ui, |ui| {
                    if source {
                        Self::text_field(
                            ui,
                            language.text("Port"),
                            &mut self.form.profile.source_port,
                        );
                        Self::text_field(
                            ui,
                            language.text("Credential ID"),
                            &mut self.form.profile.source_credential_id,
                        );
                        Self::text_field(
                            ui,
                            language.text("CA bundle"),
                            &mut self.form.profile.source_ca_bundle,
                        );
                        if !dovecot {
                            Self::text_field(
                                ui,
                                language.text("Certificate pin (SHA-256)"),
                                &mut self.form.profile.source_certificate_pin_sha256,
                            );
                        }
                        Self::tls_field(
                            ui,
                            language.text("TLS"),
                            &mut self.form.profile.source_tls,
                            "source_tls",
                        );
                        ui.checkbox(
                            &mut self.form.profile.allow_insecure_source_transport,
                            language.text("Allow insecure source transport (review carefully)"),
                        );
                    } else if dovecot {
                        ui.label(
                            egui::RichText::new(
                                language.text("Destination: local Dovecot storage"),
                            )
                            .color(self.theme_colors().text_secondary),
                        );
                        Self::text_field(
                            ui,
                            language.text("Credential ID"),
                            &mut self.form.profile.destination_credential_id,
                        );
                    } else {
                        Self::text_field(
                            ui,
                            language.text("Port"),
                            &mut self.form.profile.destination_port,
                        );
                        Self::text_field(
                            ui,
                            language.text("Credential ID"),
                            &mut self.form.profile.destination_credential_id,
                        );
                        Self::text_field(
                            ui,
                            language.text("CA bundle"),
                            &mut self.form.profile.destination_ca_bundle,
                        );
                        Self::text_field(
                            ui,
                            language.text("Certificate pin (SHA-256)"),
                            &mut self.form.profile.destination_certificate_pin_sha256,
                        );
                        Self::tls_field(
                            ui,
                            language.text("TLS"),
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
                });
        });
    }

    fn provider_field(&mut self, ui: &mut egui::Ui, source: bool) {
        let current = if source {
            self.source_provider
        } else {
            self.destination_provider
        };
        let mut selected = current;
        let label = if source {
            self.language.text("Where are you migrating from?")
        } else {
            self.language.text("Where are you migrating to?")
        };
        crate::ui::form_row(ui, label, |ui| {
            egui::ComboBox::from_id_salt(("provider_preset", source))
                .selected_text(self.language.text(current.label()))
                .width(ui.available_width().min(260.0))
                .show_ui(ui, |ui| {
                    for preset in ProviderPreset::ALL {
                        ui.selectable_value(
                            &mut selected,
                            preset,
                            self.language.text(preset.label()),
                        );
                    }
                });
        });
        if selected != current {
            if source {
                self.source_provider = selected;
            } else {
                self.destination_provider = selected;
            }
            self.apply_provider_preset(source, selected);
        }
        // Keep the provider's authentication guidance visible while its
        // preset is active, not only on the frame it was chosen.
        if selected != ProviderPreset::GenericImap {
            ui.label(
                egui::RichText::new(self.language.text(selected.defaults().note))
                    .small()
                    .color(self.theme_colors().text_secondary),
            );
        }
        ui.add_space(6.0);
    }

    fn provider_runbook_panel(&self, ui: &mut egui::Ui) {
        ui.collapsing(
            self.language.text("Provider readiness runbook"),
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
                            ui.label(format!("{} {}", self.language.text("Why:"), step.why));
                            ui.label(format!(
                                "{} {}",
                                self.language.text("Success:"),
                                step.success_indicator
                            ));
                        }
                        if !runbook.known_issues.is_empty() {
                            ui.separator();
                            ui.label(
                                egui::RichText::new(self.language.text("Known provider issues"))
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
    use super::{PlanWorkflowStep, plan_workflow_step};

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
}
