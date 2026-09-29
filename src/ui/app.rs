use crate::ui::format_phase_name;
use crate::ui::status_color;
use crate::*;

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
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
                    ui.label(
                        egui::RichText::new(format!(
                            "{}: {}",
                            self.language.text("Engine"),
                            self.language.text(self.form.engine().label())
                        ))
                        .small()
                        .color(colors.text_secondary),
                    );
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
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&self.status.text)
                                    .color(status_color(self.status.severity, colors)),
                            )
                            .truncate(),
                        );
                    });
                });
            });

        let navigation_frame = egui::Frame::side_top_panel(ui.style())
            .fill(colors.panel)
            .inner_margin(egui::Margin::symmetric(10, 14))
            .stroke(egui::Stroke::new(1.0, colors.quiet_border()));
        egui::Panel::left("workspace_navigation")
            .resizable(false)
            .show_separator_line(false)
            .exact_size(200.0)
            .frame(navigation_frame)
            .show(ui, |ui| {
                crate::ui::section_label(ui, self.language.text("Workspace"));
                ui.add_space(4.0);
                for (view, icon, label) in [
                    (WorkspaceView::Overview, "📊", "Overview"),
                    (WorkspaceView::Plan, "📋", "Plan"),
                    (WorkspaceView::Mailboxes, "✉", "Mailboxes"),
                    (WorkspaceView::Activity, "⏱", "Activity"),
                    (WorkspaceView::Verification, "✔", "Verification"),
                ] {
                    if crate::ui::nav_item(
                        ui,
                        self.active_view == view,
                        icon,
                        self.language.text(label),
                    )
                    .clicked()
                    {
                        self.active_view = view;
                        self.refresh_ui_snapshot_now();
                    }
                }
                ui.add_space(12.0);
                crate::ui::section_label(ui, self.language.text("Manage"));
                ui.add_space(4.0);
                if crate::ui::nav_item(ui, self.projects_open, "🗄", self.language.text("Projects"))
                    .clicked()
                {
                    self.projects_open = true;
                }
                if crate::ui::nav_item(ui, self.settings_open, "⚙", self.language.text("Settings"))
                    .clicked()
                {
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
                        ui.label(
                            egui::RichText::new(text)
                                .small()
                                .color(colors.text_secondary),
                        );
                    });
                });
            });

        if self.active_view == WorkspaceView::Mailboxes && !self.bulk_jobs.is_empty() {
            egui::Panel::right("selection_review_drawer")
                .resizable(true)
                .default_size(320.0)
                .show(ui, |ui| self.selection_review_drawer(ui));
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
    fn plan_view(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        crate::ui::page_header(
            ui,
            self.language.text("Migration plan"),
            self.language
                .text("Configure endpoints and credentials before running a dry preflight."),
        );
        crate::ui::card(ui, |ui| {
            crate::ui::form_row(ui, self.language.text("Plan name"), |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.form.profile.name).desired_width(360.0))
            });
            crate::ui::form_row(ui, self.language.text("Engine"), |ui| {
                for (engine, label) in [
                    (core::Engine::ImapSync, "imapsync"),
                    (core::Engine::Dovecot, "Dovecot"),
                ] {
                    let selected = self.form.profile.engine == engine;
                    if ui
                        .add(egui::Button::selectable(selected, label).frame_when_inactive(true))
                        .clicked()
                    {
                        self.form.profile.engine = engine;
                    }
                }
                ui.add_space(16.0);
                ui.checkbox(
                    &mut self.form.dry_run,
                    self.language.text("Dry run / preflight"),
                );
            });
        });
        ui.add_space(16.0);
        ui.columns(2, |columns| {
            {
                let ui = &mut columns[0];
                self.provider_field(ui, true);
                let password_required = !auth_method_is_oauth(&self.form.profile.source_auth);
                super::account::render_account(
                    ui,
                    self.language,
                    self.language.text("Source account"),
                    &mut self.form.profile.source_host,
                    &mut self.form.profile.source_user,
                    &mut self.form.profile.source_auth,
                    &mut self.form.source_password,
                    password_required,
                    !self.form.profile.source_credential_id.trim().is_empty(),
                    colors.info,
                );
                ui.add_space(10.0);
                crate::ui::card(ui, |ui| {
                    crate::ui::section_label(ui, self.language.text("Connection details"));
                    Self::text_field(
                        ui,
                        self.language.text("Port"),
                        &mut self.form.profile.source_port,
                    );
                    Self::text_field(
                        ui,
                        self.language.text("Credential ID"),
                        &mut self.form.profile.source_credential_id,
                    );
                    Self::text_field(
                        ui,
                        self.language.text("CA bundle"),
                        &mut self.form.profile.source_ca_bundle,
                    );
                    Self::text_field(
                        ui,
                        self.language.text("Certificate pin (SHA-256)"),
                        &mut self.form.profile.source_certificate_pin_sha256,
                    );
                    Self::tls_field(
                        ui,
                        self.language.text("TLS"),
                        &mut self.form.profile.source_tls,
                        "source_tls",
                    );
                    ui.checkbox(
                        &mut self.form.profile.allow_insecure_source_transport,
                        self.language
                            .text("Allow insecure source transport (review carefully)"),
                    );
                });
            }
            {
                let ui = &mut columns[1];
                self.provider_field(ui, false);
                let password_required = !auth_method_is_oauth(&self.form.profile.destination_auth);
                super::account::render_account(
                    ui,
                    self.language,
                    self.language.text("Destination account"),
                    &mut self.form.profile.destination_host,
                    &mut self.form.profile.destination_user,
                    &mut self.form.profile.destination_auth,
                    &mut self.form.destination_password,
                    password_required,
                    !self
                        .form
                        .profile
                        .destination_credential_id
                        .trim()
                        .is_empty(),
                    colors.success,
                );
                ui.add_space(10.0);
                crate::ui::card(ui, |ui| {
                    crate::ui::section_label(ui, self.language.text("Connection details"));
                    Self::text_field(
                        ui,
                        self.language.text("Port"),
                        &mut self.form.profile.destination_port,
                    );
                    Self::text_field(
                        ui,
                        self.language.text("Credential ID"),
                        &mut self.form.profile.destination_credential_id,
                    );
                    Self::text_field(
                        ui,
                        self.language.text("CA bundle"),
                        &mut self.form.profile.destination_ca_bundle,
                    );
                    Self::text_field(
                        ui,
                        self.language.text("Certificate pin (SHA-256)"),
                        &mut self.form.profile.destination_certificate_pin_sha256,
                    );
                    Self::tls_field(
                        ui,
                        self.language.text("TLS"),
                        &mut self.form.profile.destination_tls,
                        "destination_tls",
                    );
                });
            }
        });
        ui.add_space(16.0);
        crate::ui::card(ui, |ui| self.provider_runbook_panel(ui));
        ui.add_space(16.0);
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
                            format!("{}: {error}", self.language.text("Could not save profile")),
                            StatusSeverity::Error,
                        ),
                    }
                }
                if ui.button(self.language.text("Assess plan")).clicked() {
                    self.assess_plan();
                }
                if ui.button(self.language.text("Preview command")).clicked() {
                    self.preview = true;
                }
                if ui.button(self.language.text("Advanced")).clicked() {
                    self.advanced_open = true;
                }
                ui.separator();
                if crate::ui::primary_button(ui, self.language.text("Run preflight")).clicked() {
                    self.start_capability_probe();
                }
                let live_enabled = !self.form.dry_run;
                let live = egui::Button::new(
                    egui::RichText::new(self.language.text("Start live migration")).color(
                        if live_enabled {
                            egui::Color32::WHITE
                        } else {
                            colors.text_secondary
                        },
                    ),
                )
                .fill(if live_enabled {
                    colors.danger.gamma_multiply(0.85)
                } else {
                    colors.panel
                });
                if ui.add_enabled(live_enabled, live).clicked() {
                    self.start();
                }
            });
            if self.form.dry_run {
                ui.label(
                    egui::RichText::new(
                        self.language
                            .text("Clear Dry run / preflight to enable live migration."),
                    )
                    .small()
                    .color(colors.text_secondary),
                );
            }
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
            self.language.text("Source preset")
        } else {
            self.language.text("Destination preset")
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
            ui.label(egui::RichText::new(self.language.text(selected.defaults().note)).small());
        }
        ui.add_space(6.0);
    }

    fn provider_runbook_panel(&self, ui: &mut egui::Ui) {
        let runbook = crate::core::provider_runbooks::RunbookGenerator::generate(
            self.source_provider.runbook_name(),
            self.destination_provider.runbook_name(),
        );
        ui.collapsing(
            self.language.text("Provider readiness runbook"),
            |ui| {
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
