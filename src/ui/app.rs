use crate::ui::status_color;
use crate::*;

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        let ctx = ui.ctx().clone();
        let colors = self.theme_colors();
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, colors.background);
        ui.visuals_mut().override_text_color = Some(colors.text_primary);
        ui.visuals_mut().widgets.noninteractive.bg_fill = colors.panel;
        ui.visuals_mut().window_fill = colors.window;
        ui.visuals_mut().hyperlink_color = colors.link;
        ui.visuals_mut().selection.bg_fill = colors.selection;
        ui.visuals_mut().widgets.noninteractive.fg_stroke.color = colors.border;

        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("MailSwiftSync");
                ui.separator();
                for (view, label) in [
                    (WorkspaceView::Overview, "Overview"),
                    (WorkspaceView::Plan, "Plan"),
                    (WorkspaceView::Mailboxes, "Mailboxes"),
                    (WorkspaceView::Activity, "Activity"),
                    (WorkspaceView::Verification, "Verification"),
                ] {
                    if ui
                        .selectable_label(self.active_view == view, label)
                        .clicked()
                    {
                        self.active_view = view;
                        self.refresh_ui_snapshot_now();
                    }
                }
                if ui.button("Projects").clicked() {
                    self.projects_open = true;
                }
                if ui.button("Settings").clicked() {
                    self.settings_open = true;
                }
            });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(&self.status.text)
                        .color(status_color(self.status.severity, self.theme_colors())),
                );
                if self.running() && ui.button("Stop").clicked() {
                    self.stop_confirm_open = true;
                }
                if self.ui_snapshot.is_stale() {
                    ui.label(
                        egui::RichText::new(
                            self.ui_snapshot
                                .stale_notice()
                                .unwrap_or_else(|| "Durable view is stale".to_owned()),
                        )
                        .color(self.theme_colors().warning),
                    );
                }
            });
            ui.separator();

            match self.active_view {
                WorkspaceView::Overview => self.overview_view(ui),
                WorkspaceView::Plan => self.plan_view(ui),
                WorkspaceView::Mailboxes => self.mailbox_view(ui),
                WorkspaceView::Activity => self.activity_view(ui),
                WorkspaceView::Verification => self.verification_view(ui),
            }
        });

        self.projects_dialog(&ctx);
        self.settings_dialog(&ctx);
        self.keyring_dialog(&ctx);
        self.engine_dialog(&ctx);
        self.preview(&ctx);
        self.advanced_dialog(&ctx);
        self.live_confirmation(&ctx);
        self.stop_confirmation(&ctx);
        self.bulk_dialog(&ctx);
        self.bulk_sheet_selection(&ctx);
        self.bulk_clear_confirmation(&ctx);
        self.bulk_import_confirmation(&ctx);
        self.bulk_live_confirmation(&ctx);
    }
}

impl App {
    fn plan_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Migration plan");
        ui.label("Configure endpoints and credentials before running a dry preflight.");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Plan name");
            ui.text_edit_singleline(&mut self.form.profile.name);
        });
        ui.collapsing("Source", |ui| {
            self.provider_field(ui, true);
            let password_required = !auth_method_is_oauth(&self.form.profile.source_auth);
            let color = self.theme_colors().info;
            super::account::render_account(
                ui,
                "SOURCE ACCOUNT",
                &mut self.form.profile.source_host,
                &mut self.form.profile.source_user,
                &mut self.form.profile.source_auth,
                &mut self.form.source_password,
                password_required,
                !self.form.profile.source_credential_id.trim().is_empty(),
                color,
            );
            Self::text_field(ui, "Port", &mut self.form.profile.source_port);
            Self::text_field(
                ui,
                "Credential ID",
                &mut self.form.profile.source_credential_id,
            );
            Self::text_field(ui, "CA bundle", &mut self.form.profile.source_ca_bundle);
            Self::text_field(
                ui,
                "Certificate pin (SHA-256)",
                &mut self.form.profile.source_certificate_pin_sha256,
            );
            Self::tls_field(ui, "TLS", &mut self.form.profile.source_tls);
            ui.checkbox(
                &mut self.form.profile.allow_insecure_source_transport,
                "Allow insecure source transport (review carefully)",
            );
        });
        ui.collapsing("Destination", |ui| {
            self.provider_field(ui, false);
            let password_required = !auth_method_is_oauth(&self.form.profile.destination_auth);
            let color = self.theme_colors().success;
            super::account::render_account(
                ui,
                "DESTINATION ACCOUNT",
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
            Self::text_field(ui, "Port", &mut self.form.profile.destination_port);
            Self::text_field(
                ui,
                "Credential ID",
                &mut self.form.profile.destination_credential_id,
            );
            Self::text_field(
                ui,
                "CA bundle",
                &mut self.form.profile.destination_ca_bundle,
            );
            Self::text_field(
                ui,
                "Certificate pin (SHA-256)",
                &mut self.form.profile.destination_certificate_pin_sha256,
            );
            Self::tls_field(ui, "TLS", &mut self.form.profile.destination_tls);
        });
        ui.horizontal(|ui| {
            ui.label("Engine");
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
            ui.checkbox(&mut self.form.dry_run, "Dry run / preflight");
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("Save / create project").clicked() {
                self.create_project();
            }
            if ui.button("Save profile").clicked() {
                match self.form.save() {
                    Ok(()) => self.set_status(
                        "Profile saved without credential material.",
                        StatusSeverity::Success,
                    ),
                    Err(error) => self.set_status(
                        format!("Could not save profile: {error}"),
                        StatusSeverity::Error,
                    ),
                }
            }
            if ui.button("Assess plan").clicked() {
                self.assess_plan();
            }
            if ui.button("Run preflight").clicked() {
                self.start_capability_probe();
            }
            if ui.button("Preview command").clicked() {
                self.preview = true;
            }
            if ui.button("Advanced").clicked() {
                self.advanced_open = true;
            }
            if ui
                .add_enabled(
                    !self.form.dry_run,
                    egui::Button::new("Start live migration"),
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
            "Source preset"
        } else {
            "Destination preset"
        })
        .selected_text(current.label())
        .show_ui(ui, |ui| {
            for preset in ProviderPreset::ALL {
                ui.selectable_value(&mut selected, preset, preset.label());
            }
        });
        if selected != current {
            if source {
                self.source_provider = selected;
            } else {
                self.destination_provider = selected;
            }
            self.apply_provider_preset(source, selected);
            ui.label(egui::RichText::new(selected.defaults().note).size(11.0));
        }
    }
}
