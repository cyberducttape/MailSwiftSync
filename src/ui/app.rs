use crate::*;

impl eframe::App for App {
    #[allow(clippy::possible_missing_else, clippy::collapsible_if)]
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        // Keep operator-facing tables, status text, and logs readable on a
        // migration workstation. This is a default scale, not a substitute
        // for a future persisted Appearance preference.
        ctx.set_zoom_factor(self.ui_scale);
        self.poll();
        // Capability observations are only meaningful for the exact plan that
        // produced them. Re-check before rendering every frame so editing the
        // migration plan cannot leave a stale readiness result visible until
        // the operator opens the readiness view or another event arrives.
        if self.invalidate_stale_capability_observation() {
            self.set_status(
                "Readiness observations expired because the migration plan changed; run discovery again.",
                StatusSeverity::Warning,
            );
        }
        let colors = self.theme_colors();
        let plan_controls_enabled =
            !self.running() && !self.workspace_read_only && !self.ui_snapshot.is_stale();
        if !plan_controls_enabled {
            ctx.data_mut(|data| {
                for title in ["01  SOURCE MAILBOX", "02  DESTINATION MAILBOX"] {
                    data.remove::<bool>(password_visibility_id(title));
                }
            });
        }
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("plan_controls_enabled"),
                plan_controls_enabled,
            );
        });
        let mut v = if self.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        v.override_text_color = Some(colors.text_primary);
        v.panel_fill = colors.background;
        v.extreme_bg_color = colors.background;
        v.panel_fill = colors.panel;
        v.window_fill = colors.window;
        v.widgets.active.bg_fill = colors.info;
        v.widgets.hovered.bg_fill = colors.selection;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colors.border);
        ctx.set_visuals(v);
        let mut header_project_selection = None;
        let mut header_all_projects_requested = false;
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(colors.window)
                    .inner_margin(egui::Margin::symmetric(24, 15)),
            )
            .show(ctx, |ui: &mut egui::Ui| {
                ui.horizontal_wrapped(|ui: &mut egui::Ui| {
                    ui.label(
                        RichText::new("MAILSWIFTSYNC")
                            .strong()
                            .size(23.0)
                            .color(colors.text_primary),
                    );
                    ui.label(
                        RichText::new("mailbox migration control plane")
                            .italics()
                            .color(colors.link),
                    );
                    // The full project browser is available from the header,
                    // so the switcher must not silently hide older projects
                    // once the ledger grows beyond the first page.
                    let projects = &self.ui_snapshot.projects;
                    if !projects.is_empty() || self.selected_project_id.is_some() {
                        let selected_name = self
                            .selected_project_id
                            .as_deref()
                            .and_then(|id| projects.iter().find(|project| project.id == id))
                            .map(|project| project.name.clone())
                            .or_else(|| {
                                self.ui_snapshot.project.as_ref().and_then(|project| {
                                    self.selected_project_id
                                        .as_deref()
                                        .filter(|id| *id == project.id)
                                        .map(|_| project.name.clone())
                                })
                            })
                            .unwrap_or_else(|| "Selected project".into());
                        ui.add_enabled_ui(!self.running(), |ui| {
                            egui::ComboBox::from_id_salt("project_switcher")
                                .selected_text(selected_name)
                                .width(180.0)
                                .show_ui(ui, |ui| {
                                    for project in projects {
                                        let selected = self.selected_project_id.as_deref()
                                            == Some(project.id.as_str());
                                        if ui
                                            .selectable_label(
                                                selected,
                                                format!(
                                                    "{} · {}",
                                                    project.name,
                                                    format_phase_name(project.phase)
                                                ),
                                            )
                                            .clicked()
                                        {
                                            header_project_selection = Some(project.id.clone());
                                        }
                                    }
                                    ui.separator();
                                    if ui.selectable_label(false, "All projects…").clicked() {
                                        header_all_projects_requested = true;
                                    }
                                });
                        });
                    }
                    if ui.button("⚙ Settings").clicked() {
                        self.settings_open = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui: &mut egui::Ui| {
                        if self.ui_snapshot.is_stale() {
                            ui.label(
                                RichText::new("⚠ DURABLE VIEW STALE")
                                    .strong()
                                    .color(colors.warning),
                            );
                            ui.separator();
                        }
                        if self.running() {
                            ui.add(egui::Spinner::new());
                            if let Some(started) = self.run_started_at {
                                ui.label(format!("elapsed {}", format_elapsed(started.elapsed())));
                            }
                        }
                        ui.label(
                            RichText::new(&self.status.text)
                                .color(status_color(self.status.severity, colors)),
                        );
                        ui.separator();
                        ui.label(
                            RichText::new(if self.workspace_read_only {
                                "HISTORICAL VIEW"
                            } else if self.form.dry_run {
                                "PREFLIGHT"
                            } else {
                                "LIVE MIGRATION"
                            })
                            .strong()
                            .color(if self.workspace_read_only {
                                colors.info
                            } else if self.form.dry_run {
                                colors.success
                            } else {
                                colors.danger
                            }),
                        );
                    });
                });
            });
        if let Some(project_id) = header_project_selection {
            self.select_workspace_project(project_id);
        }
        if header_all_projects_requested {
            self.ui_all_projects_loaded = true;
            self.refresh_ui_snapshot_now();
            self.projects_open = true;
        }
        egui::SidePanel::left("workspace_nav")
            .resizable(false)
            .default_width(185.0)
            .frame(
                egui::Frame::new()
                    .fill(colors.panel)
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui: &mut egui::Ui| {
                ui.label(
                    RichText::new("WORKSPACE")
                        .size(11.0)
                        .strong()
                        .color(colors.text_secondary),
                );
                ui.add_space(6.0);
                for (view, label) in [
                    (WorkspaceView::Overview, "Overview"),
                    (WorkspaceView::Plan, "Migration plan"),
                    (WorkspaceView::Mailboxes, "Mailboxes"),
                    (WorkspaceView::Activity, "Activity"),
                    (WorkspaceView::Verification, "Verification"),
                ] {
                    let selected = self.active_view == view;
                    if ui
                        .add_sized(
                            [ui.available_width(), 32.0],
                            egui::Button::selectable(selected, RichText::new(label).strong()),
                        )
                        .clicked()
                    {
                        self.active_view = view;
                    }
                    ui.add_space(4.0);
                }
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(colors.background)
                    .inner_margin(egui::Margin::same(24)),
            )
            .show(ctx, |ui: &mut egui::Ui| {
                if let Some(notice) = self.ui_snapshot.stale_notice() {
                    ui.colored_label(colors.warning, format!("⚠ {notice}"));
                    ui.add_space(8.0);
                }
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.project_summary(ui);
                        if self.active_view == WorkspaceView::Plan {
                        ui.add_enabled_ui(plan_controls_enabled, |ui| {
                            ui.add_space(14.0);
                            ui.heading("Migration plan");
                            ui.label(RichText::new("Set up the connection, run preflight, then deliberately promote this project through each migration phase.").color(self.theme_colors().text_secondary));
                            ui.add_space(14.0);
                            ui.group(|ui| {
                                ui.label(RichText::new("PLAN TOOLS").strong());
                                ui.horizontal_wrapped(|ui| {
                                    if ui.button(format!("Engine: {}", self.form.engine().label())).clicked() {
                                        self.engine_open = true;
                                    }
                                    if ui.button("Credentials").clicked() {
                                        self.keyring_open = true;
                                    }
                                    if ui.button("Advanced options").clicked() {
                                        self.advanced_open = true;
                                    }
                                    if ui.button("Readiness & preflight").clicked() {
                                        self.active_view = WorkspaceView::Overview;
                                        self.assess_plan();
                                    }
                                });
                                ui.label(RichText::new("These controls modify the active migration plan and are included in its review and preflight gate.").size(11.0).color(self.theme_colors().text_secondary));
                            });
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                ui.label("Project name");
                                ui.text_edit_singleline(&mut self.form.profile.name);
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| if ui.button("Save non-secret profile").clicked() {
                                    match self.form.save() {
                                        Ok(()) => self.set_status(
                                            "Profile saved; passwords were not saved",
                                            StatusSeverity::Success,
                                        ),
                                        Err(e) => self.set_status(
                                            format!("Could not save profile: {e}"),
                                            StatusSeverity::Error,
                                        ),
                                    }
                                });
                            });
                            ui.add_space(10.0);
                            let destination_password_required =
                                self.form.engine() != core::Engine::Dovecot;
                            ui.group(|ui| {
                                ui.label(RichText::new("Endpoint presets").strong());
                                ui.label(RichText::new("These presets fill connection hints only. Discovery, provider policy, and credentials still require explicit preflight.").size(11.0).color(self.theme_colors().text_secondary));
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("Source provider");
                                    let mut source_changed = false;
                                    egui::ComboBox::from_id_salt("source_provider_preset")
                                        .selected_text(self.source_provider.label())
                                        .show_ui(ui, |ui| {
                                            for preset in ProviderPreset::ALL {
                                                source_changed |= ui.selectable_value(
                                                    &mut self.source_provider,
                                                    preset,
                                                    preset.label(),
                                                ).changed();
                                            }
                                        });
                                    ui.separator();
                                    ui.label("Destination provider");
                                    let mut destination_changed = false;
                                    egui::ComboBox::from_id_salt("destination_provider_preset")
                                        .selected_text(self.destination_provider.label())
                                        .show_ui(ui, |ui| {
                                            for preset in ProviderPreset::ALL {
                                                destination_changed |= ui.selectable_value(
                                                    &mut self.destination_provider,
                                                    preset,
                                                    preset.label(),
                                                ).changed();
                                            }
                                        });
                                    if source_changed {
                                        self.apply_provider_preset(true, self.source_provider);
                                    }
                                    if destination_changed
                                        && self.form.engine() != core::Engine::Dovecot
                                    {
                                        self.apply_provider_preset(false, self.destination_provider);
                                    }
                                });
                                ui.label(RichText::new(format!(
                                    "Source: {}",
                                    self.source_provider.defaults().note
                                )).size(11.0).color(self.theme_colors().text_secondary));
                                if self.form.engine() != core::Engine::Dovecot {
                                    ui.label(RichText::new(format!(
                                        "Destination: {}",
                                        self.destination_provider.defaults().note
                                    )).size(11.0).color(self.theme_colors().text_secondary));
                                }
                            });
                            ui.add_space(10.0);
                            if ui.available_width() > 900.0 {
                                ui.columns(2, |c| {
                                    render_account(&mut c[0], "01  SOURCE MAILBOX", &mut self.form.profile.source_host, &mut self.form.profile.source_user, &mut self.form.profile.source_auth, &mut self.form.source_password, true, !self.form.profile.source_credential_id.trim().is_empty(), colors.info);
                                    render_account(&mut c[1], if self.form.engine() == core::Engine::Dovecot { "02  LOCAL DOVECOT DESTINATION" } else { "02  DESTINATION MAILBOX" }, &mut self.form.profile.destination_host, &mut self.form.profile.destination_user, &mut self.form.profile.destination_auth, &mut self.form.destination_password, destination_password_required, !self.form.profile.destination_credential_id.trim().is_empty(), colors.success);
                                });
                            } else {
                                render_account(ui, "01  SOURCE MAILBOX", &mut self.form.profile.source_host, &mut self.form.profile.source_user, &mut self.form.profile.source_auth, &mut self.form.source_password, true, !self.form.profile.source_credential_id.trim().is_empty(), colors.info);
                                ui.add_space(8.0);
                                render_account(ui, if self.form.engine() == core::Engine::Dovecot { "02  LOCAL DOVECOT DESTINATION" } else { "02  DESTINATION MAILBOX" }, &mut self.form.profile.destination_host, &mut self.form.profile.destination_user, &mut self.form.profile.destination_auth, &mut self.form.destination_password, destination_password_required, !self.form.profile.destination_credential_id.trim().is_empty(), colors.success);
                            }
                            let dovecot_destination = self.form.engine() == core::Engine::Dovecot;
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Source port");
                                ui.add(egui::TextEdit::singleline(&mut self.form.profile.source_port).desired_width(70.0));
                                ui.label("TLS");
                                egui::ComboBox::from_id_salt("source_tls").selected_text(&self.form.profile.source_tls).show_ui(ui, |ui| for mode in ["imaps", "starttls", "plain"] { ui.selectable_value(&mut self.form.profile.source_tls, mode.into(), mode); });
                                if dovecot_destination {
                                    ui.separator();
                                    ui.label(RichText::new("Destination: local Dovecot storage").color(self.theme_colors().text_secondary));
                                } else {
                                    ui.separator();
                                    ui.label("Destination port");
                                    ui.add(egui::TextEdit::singleline(&mut self.form.profile.destination_port).desired_width(70.0));
                                    ui.label("TLS");
                                    egui::ComboBox::from_id_salt("destination_tls").selected_text(&self.form.profile.destination_tls).show_ui(ui, |ui| for mode in ["imaps", "starttls"] { ui.selectable_value(&mut self.form.profile.destination_tls, mode.into(), mode); });
                                }
                            });
                            ui.collapsing("Enterprise certificate trust (optional)", |ui| {
                                if self.form.engine() == core::Engine::Dovecot {
                                    ui.label(RichText::new("Dovecot mode uses the source CA bundle for local imapc TLS. Certificate pins are not enforced by Dovecot and are rejected by validation.").color(self.theme_colors().warning));
                                    ui.horizontal(|ui| {
                                        ui.label("Source CA bundle");
                                        ui.add(egui::TextEdit::singleline(&mut self.form.profile.source_ca_bundle).desired_width(300.0).hint_text("/path/to/company-ca.pem"));
                                    });
                                } else {
                                    ui.label(RichText::new("Use a PEM CA bundle for private PKI, or pin the leaf certificate's SHA-256 fingerprint. Public/system roots remain enabled.").color(self.theme_colors().text_secondary));
                                    ui.horizontal(|ui| {
                                        ui.label("Source CA bundle");
                                        ui.add(egui::TextEdit::singleline(&mut self.form.profile.source_ca_bundle).desired_width(300.0).hint_text("/path/to/company-ca.pem"));
                                        ui.label("SHA-256 pin");
                                        ui.add(egui::TextEdit::singleline(&mut self.form.profile.source_certificate_pin_sha256).desired_width(300.0).hint_text("64 hex characters"));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Destination CA bundle");
                                        ui.add(egui::TextEdit::singleline(&mut self.form.profile.destination_ca_bundle).desired_width(300.0).hint_text("/path/to/company-ca.pem"));
                                        ui.label("SHA-256 pin");
                                        ui.add(egui::TextEdit::singleline(&mut self.form.profile.destination_certificate_pin_sha256).desired_width(300.0).hint_text("64 hex characters"));
                                    });
                                    ui.label(RichText::new("Pins are checked in the authenticated readiness probe; a mismatch blocks execution. Do not use a pin as a substitute for an approved CA unless your security policy explicitly permits it.").color(self.theme_colors().text_secondary));
                                }
                            });
                            ui.add_space(14.0);
                            ui.group(|ui| {
                                ui.heading("03  SYNC RULES");
                                ui.label(RichText::new("Execution mode").strong());
                                ui.horizontal(|ui| {
                                    ui.selectable_value(&mut self.form.dry_run, true, "Preflight")
                                        .on_hover_text("Authenticate and validate the plan without intentionally changing the destination.");
                                    ui.selectable_value(&mut self.form.dry_run, false, "Live migration")
                                        .on_hover_text("Run the selected migration and allow destination changes.");
                                });
                                ui.label(RichText::new(if self.form.dry_run {
                                    "Preflight checks access and mapping without intentionally changing the destination."
                                } else if self.form.engine() == core::Engine::Dovecot {
                                    "Live migration is enabled; review the Dovecot strategy and merge behavior before starting."
                                } else {
                                    "Live migration is enabled; review the destination and deletion warning before starting."
                                }).color(if self.form.dry_run { self.theme_colors().text_secondary } else { self.theme_colors().danger }));
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut self.form.profile.automap, "Map standard folders automatically");
                                    ui.checkbox(&mut self.form.profile.justfolders, "Folders only");
                                    ui.checkbox(&mut self.form.profile.addheader, "Add Message-ID header when needed");
                                });
                                ui.horizontal(|ui| { ui.label("Extra imapsync options"); ui.text_edit_singleline(&mut self.form.profile.extra_options); });
                                ui.horizontal(|ui| { ui.label("imapsync executable"); ui.text_edit_singleline(&mut self.form.profile.imapsync_path); });
                            });
                            if self.form.engine() == core::Engine::ImapSync && self.form.profile.delete2 {
                                ui.group(|ui| {
                                    ui.label(RichText::new("⚠ DESTINATION DELETION ENABLED").strong().color(self.theme_colors().danger));
                                    ui.label(RichText::new("Messages that exist only on the destination may be removed during live migration.").color(self.theme_colors().danger));
                                });
                            }
                        });
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui.button("Preview redacted command").clicked() { self.preview = true; }
                            if self.running() {
                                if ui.button("Stop migration").clicked() { self.stop_confirm_open = true; }
                            } else {
                                let label = if self.form.dry_run { "Run preflight  →" } else { "Start live migration  →" };
                                if ui.add_enabled(true, egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(if self.form.dry_run { self.theme_colors().info } else { self.theme_colors().danger })).clicked() { self.start(); }
                            }
                            if !self.form.dry_run && !self.running() { ui.label(RichText::new("Live migration can add mail to the destination. Review readiness before continuing.").color(self.theme_colors().danger)); }
                        });
                        ui.add_space(14.0);
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.heading("Execution journal");
                                ui.label(RichText::new(if self.running() { "streaming output" } else { "waiting" }).color(self.theme_colors().text_secondary));
                                if ui.button("Copy support summary").clicked() {
                                    ui.ctx().copy_text(self.support_summary());
                                }
                                ui.menu_button("Raw output…", |ui| {
                                    ui.label(
                                        RichText::new("May contain mailbox metadata")
                                            .color(self.theme_colors().warning),
                                    );
                                    if ui.button("Copy redacted engine output").clicked() {
                                        ui.ctx().copy_text(
                                            self.output.iter().cloned().collect::<Vec<_>>().join("\n"),
                                        );
                                        ui.close();
                                    }
                                });
                            });
                            egui::ScrollArea::vertical()
                                .hscroll(true)
                                .stick_to_bottom(true)
                                .max_height(180.0)
                                .show_rows(ui, 20.0, self.output.len(), |ui, rows| {
                                    for index in rows {
                                        if let Some(line) = self.output.get(index) {
                                            ui.add(egui::Label::new(RichText::new(line).monospace().size(14.0)).wrap_mode(egui::TextWrapMode::Extend));
                                        }
                                    }
                                });
                        });
                        ui.add_space(8.0);
                        ui.label(RichText::new("Passwords never enter the saved profile. The selected engine receives credentials only for the active process; local process visibility still matters.").size(11.0).color(self.theme_colors().text_secondary));
                        }
                    });
            });
        self.preview(ctx);
        self.bulk_dialog(ctx);
        self.bulk_clear_confirmation(ctx);
        self.bulk_import_confirmation(ctx);
        self.bulk_sheet_selection(ctx);
        self.bulk_live_confirmation(ctx);
        self.settings_dialog(ctx);
        self.projects_dialog(ctx);
        self.keyring_dialog(ctx);
        self.advanced_dialog(ctx);
        self.engine_dialog(ctx);
        self.live_confirmation(ctx);
        self.stop_confirmation(ctx);
        if self.running()
            || self.capability_receiver.is_some()
            || self.live_auth_receiver.is_some()
            || self.bulk_import_receiver.is_some()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
