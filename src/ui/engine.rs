//! Migration-engine selection and local Dovecot configuration view.

use crate::engine_install::{
    Elevation, InstallPlan, InstallProgress, InstalledEngine, QUALIFIED_IMAPSYNC_VERSION,
};
use crate::{App, StatusSeverity, core};
use eframe::egui::{self, RichText};
use std::sync::mpsc;

/// Messages from the engine install/check worker.
pub(crate) enum EngineSetupMessage {
    Progress(InstallProgress),
    Installed(Result<InstalledEngine, String>),
    Checked { version: String, qualified: bool },
}

impl App {
    pub(crate) fn engine_dialog(&mut self, ctx: &egui::Context) {
        if !self.engine_open {
            return;
        }
        let mut open = self.engine_open;
        let mut close_requested = false;
        egui::Window::new(self.language.message("ui.choose-migration-engine"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading(self.language.message("ui.how-should-this-migration-run"));
                ui.label(RichText::new(self.language.message("ui.select-the-execution-engine-that-fits-the-destination-mailswiftsync-owns-pl-d8fb4be0d4")).color(self.theme_colors().text_secondary));
                ui.add_space(8.0);
                let editable = !self.running();
                ui.add_enabled_ui(editable, |ui| {
                    for engine in [core::Engine::Auto, core::Engine::Dovecot, core::Engine::ImapSync] {
                        ui.radio_value(
                            &mut self.form.profile.engine,
                            engine,
                            self.language.text(engine.label()),
                        );
                        if self.form.profile.engine == engine {
                            ui.label(RichText::new(self.language.text(engine.description())).size(12.0).color(self.theme_colors().text_secondary));
                        }
                    }
                    if self.form.profile.engine == core::Engine::Dovecot {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let field_label = ui.label("doveadm");
                            crate::ui::LabelledField::link_label(&ui.text_edit_singleline(&mut self.form.profile.doveadm_path), &field_label);
                        });
                        ui.horizontal(|ui| {
                            let field_label = ui.label(self.language.message("ui.config"));
                            crate::ui::LabelledField::link_label(&ui.text_edit_singleline(&mut self.form.profile.dovecot_config), &field_label);
                        });
                        ui.label(RichText::new(self.language.message("ui.native-dovecot-execution-is-local-only-until-a-secret-safe-broker-is-implemented")).size(12.0).color(self.theme_colors().text_secondary));
                        ui.label(RichText::new(self.language.message("ui.dry-mode-only-lists-the-destination-mailbox-native-dovecot-uses-the-selecte-338016b9c5")).size(12.0).color(self.theme_colors().text_secondary));
                    }
                });
                if !editable {
                    ui.label(RichText::new(self.language.message("ui.engine-and-execution-settings-are-locked-while-a-migration-is-running")).color(self.theme_colors().text_secondary));
                }
                if self.form.profile.engine != core::Engine::Dovecot {
                    ui.separator();
                    ui.add_enabled_ui(editable, |ui| self.imapsync_setup_section(ui));
                }
                ui.add_space(8.0);
                if ui.button(self.language.message("ui.continue-to-migration-plan")).clicked() {
                    close_requested = true;
                }
            });
        self.engine_open = open && !close_requested;
    }

    /// Check or install the qualified imapsync without leaving the app.
    fn imapsync_setup_section(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        ui.label(
            RichText::new(
                self.language
                    .text("imapsync {} engine")
                    .replace("{}", QUALIFIED_IMAPSYNC_VERSION),
            )
            .strong(),
        );
        let busy = self.engine_setup_receiver.is_some();
        let plan = crate::engine_install::plan_for_host();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(self.language.text("Check installed engine")),
                )
                .clicked()
            {
                self.start_engine_check();
            }
            let can_install = match plan {
                InstallPlan::Automatic(crate::engine_install::InstallMethod::DebianPackage) => {
                    crate::engine_install::desktop_elevation_available()
                }
                InstallPlan::Automatic(_) => true,
                InstallPlan::Manual(_) => false,
            };
            if can_install {
                let label = self
                    .language
                    .text("Install qualified imapsync {}")
                    .replace("{}", QUALIFIED_IMAPSYNC_VERSION);
                let clicked = if busy {
                    ui.add_enabled(false, egui::Button::new(label)).clicked()
                } else {
                    crate::ui::primary_button(ui, &label).clicked()
                };
                if clicked {
                    self.start_engine_install();
                }
            }
        });
        match plan {
            InstallPlan::Manual(guidance) => {
                ui.label(
                    RichText::new(self.language.text(guidance))
                        .size(12.0)
                        .color(colors.text_secondary),
                );
            }
            InstallPlan::Automatic(crate::engine_install::InstallMethod::DebianPackage)
                if !crate::engine_install::desktop_elevation_available() =>
            {
                ui.label(
                    RichText::new(self.language.text(
                        "Desktop elevation (pkexec) is unavailable; run `mailswiftsync install-engine` in a terminal.",
                    ))
                    .size(12.0)
                    .color(colors.text_secondary),
                );
            }
            InstallPlan::Automatic(_) => {
                ui.label(
                    RichText::new(self.language.text(
                        "Downloads from the official imapsync site and installs only if the pinned SHA-256 matches.",
                    ))
                    .size(12.0)
                    .color(colors.text_secondary),
                );
            }
        }
        if let Some(progress) = &self.engine_setup_progress {
            let text = match progress {
                InstallProgress::Downloading {
                    received,
                    total: Some(total),
                } if *total > 0 => self
                    .language
                    .text("Downloading… {}%")
                    .replace("{}", &(received * 100 / total).to_string()),
                InstallProgress::Downloading { .. } => {
                    self.language.text("Downloading…").to_owned()
                }
                InstallProgress::Verified => self.language.text("Download verified.").to_owned(),
                InstallProgress::Installing => self
                    .language
                    .text("Installing… approve the system prompt if one appears.")
                    .to_owned(),
                InstallProgress::Checking => self
                    .language
                    .text("Confirming the engine version…")
                    .to_owned(),
            };
            ui.label(RichText::new(text).color(colors.info));
        }
        if let Some((text, severity)) = &self.engine_setup_status {
            ui.label(RichText::new(text).color(crate::ui::status_color(*severity, colors)));
        }
    }

    pub(crate) fn start_engine_check(&mut self) {
        let path = match self.form.profile.imapsync_path.trim() {
            "" => "imapsync".to_owned(),
            path => path.to_owned(),
        };
        let (tx, rx) = mpsc::channel();
        self.engine_setup_receiver = Some(rx);
        self.engine_setup_status = None;
        std::thread::spawn(move || {
            let identity = crate::runner::resolve_imapsync_identity(&path);
            let _ = tx.send(EngineSetupMessage::Checked {
                qualified: identity.output_profile
                    == crate::verification::ImapsyncOutputProfile::Packaged2314,
                version: identity.version,
            });
        });
    }

    fn start_engine_install(&mut self) {
        let InstallPlan::Automatic(method) = crate::engine_install::plan_for_host() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.engine_setup_receiver = Some(rx);
        self.engine_setup_status = None;
        self.engine_setup_progress = None;
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let result = crate::engine_install::install(method, Elevation::Pkexec, |event| {
                let _ = progress_tx.send(EngineSetupMessage::Progress(event));
            });
            let _ = tx.send(EngineSetupMessage::Installed(result));
        });
    }

    /// Drain the engine worker; a vanished worker releases the dialog.
    pub(crate) fn poll_engine_setup(&mut self) {
        loop {
            let message = self
                .engine_setup_receiver
                .as_ref()
                .map(mpsc::Receiver::try_recv);
            match message {
                Some(Ok(EngineSetupMessage::Progress(progress))) => {
                    self.engine_setup_progress = Some(progress);
                }
                Some(Ok(EngineSetupMessage::Checked { version, qualified })) => {
                    self.engine_setup_receiver = None;
                    self.engine_setup_status = Some(if qualified {
                        (
                            self.language
                                .text("✓ imapsync {} is qualified")
                                .replace("{}", &version),
                            StatusSeverity::Success,
                        )
                    } else if version == "unknown" {
                        (
                            self.language
                                .text("! imapsync was not found or did not report a version")
                                .to_owned(),
                            StatusSeverity::Warning,
                        )
                    } else {
                        (
                            self.language
                                .text("! imapsync {} is not the qualified version; transfers would be unverified")
                                .replace("{}", &version),
                            StatusSeverity::Warning,
                        )
                    });
                }
                Some(Ok(EngineSetupMessage::Installed(result))) => {
                    self.engine_setup_receiver = None;
                    self.engine_setup_progress = None;
                    match result {
                        Ok(engine) => {
                            self.form.profile.imapsync_path =
                                engine.executable.to_string_lossy().into_owned();
                            let text = self
                                .language
                                .text("✓ Installed imapsync {} and selected it for this plan")
                                .replace("{}", &engine.version);
                            self.set_status(text.clone(), StatusSeverity::Success);
                            self.engine_setup_status = Some((text, StatusSeverity::Success));
                        }
                        Err(error) => {
                            self.engine_setup_status =
                                Some((format!("✕ {error}"), StatusSeverity::Error));
                        }
                    }
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.engine_setup_receiver = None;
                    self.engine_setup_progress = None;
                    self.engine_setup_status = Some((
                        format!(
                            "✕ Engine setup {}",
                            crate::controller::poll::WORKER_STOPPED_UNEXPECTEDLY
                        ),
                        StatusSeverity::Error,
                    ));
                }
                Some(Err(mpsc::TryRecvError::Empty)) | None => break,
            }
        }
    }
}
