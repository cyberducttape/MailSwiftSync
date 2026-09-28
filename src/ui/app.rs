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

        egui::Panel::top("project_header")
            .resizable(false)
            .default_size(44.0)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong("MailSwiftSync");
                    ui.separator();
                    let project_name = self
                        .ui_snapshot
                        .project
                        .as_ref()
                        .map(|project| project.name.as_str())
                        .unwrap_or_else(|| self.language.text("No project selected"));
                    ui.label(project_name);
                    if let Some(project) = self.ui_snapshot.project.as_ref() {
                        ui.label(format!(
                            "{}: {}",
                            self.language.text("Phase"),
                            format_phase_name(project.phase)
                        ));
                    }
                    ui.label(format!(
                        "{}: {}",
                        self.language.text("Engine"),
                        self.language.text(self.form.engine().label())
                    ));
                    ui.separator();
                    ui.label(
                        egui::RichText::new(&self.status.text)
                            .color(status_color(self.status.severity, self.theme_colors())),
                    );
                    if self.running() && ui.button(self.language.text("Stop")).clicked() {
                        self.stop_confirm_open = true;
                        self.stop_confirm_focus_requested = false;
                    }
                    if self.ui_snapshot.is_stale() {
                        ui.label(
                            egui::RichText::new(self.ui_snapshot.stale_notice().unwrap_or_else(
                                || self.language.text("Durable view is stale").to_owned(),
                            ))
                            .color(self.theme_colors().warning),
                        );
                    }
                });
            });

        egui::Panel::left("workspace_navigation")
            .resizable(true)
            .default_size(190.0)
            .show(ui, |ui| {
                ui.heading(self.language.text("Workspace"));
                ui.separator();
                for (view, label) in [
                    (WorkspaceView::Overview, "Overview"),
                    (WorkspaceView::Plan, "Plan"),
                    (WorkspaceView::Mailboxes, "Mailboxes"),
                    (WorkspaceView::Activity, "Activity"),
                    (WorkspaceView::Verification, "Verification"),
                ] {
                    if ui
                        .selectable_label(self.active_view == view, self.language.text(label))
                        .clicked()
                    {
                        self.active_view = view;
                        self.refresh_ui_snapshot_now();
                    }
                }
                ui.add_space(12.0);
                if ui.button(self.language.text("Projects")).clicked() {
                    self.projects_open = true;
                }
                if ui.button(self.language.text("Settings")).clicked() {
                    self.settings_open = true;
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.separator();
                    ui.label(self.language.text("Task center"));
                    ui.label(
                        egui::RichText::new(if self.running() {
                            self.language.text("Migration running")
                        } else {
                            self.language.text("No active migration")
                        })
                        .color(self.theme_colors().text_secondary),
                    );
                });
            });

        if self.active_view == WorkspaceView::Mailboxes && !self.bulk_jobs.is_empty() {
            egui::Panel::right("selection_review_drawer")
                .resizable(true)
                .default_size(320.0)
                .show(ui, |ui| self.selection_review_drawer(ui));
        }

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match self.active_view {
                    WorkspaceView::Overview => self.overview_view(ui),
                    WorkspaceView::Plan => self.plan_view(ui),
                    WorkspaceView::Mailboxes => self.mailbox_view(ui),
                    WorkspaceView::Activity => self.activity_view(ui),
                    WorkspaceView::Verification => self.verification_view(ui),
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
        ui.heading(self.language.text("Migration plan"));
        ui.label(
            self.language
                .text("Configure endpoints and credentials before running a dry preflight."),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(self.language.text("Plan name"));
            ui.text_edit_singleline(&mut self.form.profile.name);
        });
        ui.collapsing(self.language.text("Source"), |ui| {
            self.provider_field(ui, true);
            let password_required = !auth_method_is_oauth(&self.form.profile.source_auth);
            let color = self.theme_colors().info;
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
                color,
            );
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
            );
            ui.checkbox(
                &mut self.form.profile.allow_insecure_source_transport,
                self.language
                    .text("Allow insecure source transport (review carefully)"),
            );
        });
        ui.collapsing(self.language.text("Destination"), |ui| {
            self.provider_field(ui, false);
            let password_required = !auth_method_is_oauth(&self.form.profile.destination_auth);
            let color = self.theme_colors().success;
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
                color,
            );
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
            );
        });
        self.provider_runbook_panel(ui);
        ui.horizontal(|ui| {
            ui.label(self.language.text("Engine"));
            ui.selectable_value(
                &mut self.form.profile.engine,
                core::Engine::ImapSync,
                "imapsync",
            );
            ui.selectable_value(
                &mut self.form.profile.engine,
                core::Engine::Dovecot,
                "Dovecot",
            );
            ui.checkbox(
                &mut self.form.dry_run,
                self.language.text("Dry run / preflight"),
            );
        });
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
            if ui.button(self.language.text("Run preflight")).clicked() {
                self.start_capability_probe();
            }
            if ui.button(self.language.text("Preview command")).clicked() {
                self.preview = true;
            }
            if ui.button(self.language.text("Advanced")).clicked() {
                self.advanced_open = true;
            }
            if ui
                .add_enabled(
                    !self.form.dry_run,
                    egui::Button::new(self.language.text("Start live migration")),
                )
                .clicked()
            {
                self.start();
            }
        });
    }

    fn text_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.text_edit_singleline(value);
        });
    }

    fn tls_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
        egui::ComboBox::from_label(label)
            .selected_text(value.as_str())
            .show_ui(ui, |ui| {
                for mode in ["imaps", "starttls", "plain"] {
                    ui.selectable_value(value, mode.to_owned(), mode);
                }
            });
    }

    fn provider_field(&mut self, ui: &mut egui::Ui, source: bool) {
        let current = if source {
            self.source_provider
        } else {
            self.destination_provider
        };
        let mut selected = current;
        egui::ComboBox::from_label(if source {
            self.language.text("Source preset")
        } else {
            self.language.text("Destination preset")
        })
        .selected_text(self.language.text(current.label()))
        .show_ui(ui, |ui| {
            for preset in ProviderPreset::ALL {
                ui.selectable_value(&mut selected, preset, self.language.text(preset.label()));
            }
        });
        if selected != current {
            if source {
                self.source_provider = selected;
            } else {
                self.destination_provider = selected;
            }
            self.apply_provider_preset(source, selected);
            ui.label(egui::RichText::new(self.language.text(selected.defaults().note)).size(11.0));
        }
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
