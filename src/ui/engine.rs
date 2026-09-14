//! Migration-engine selection and local Dovecot configuration view.

use crate::{App, core};
use eframe::egui::{self, RichText};

impl App {
    pub(crate) fn engine_dialog(&mut self, ctx: &egui::Context) {
        if !self.engine_open {
            return;
        }
        let mut open = self.engine_open;
        let mut close_requested = false;
        egui::Window::new("Choose migration engine")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("How should this migration run?");
                ui.label(RichText::new("Select the execution engine that fits the destination. MailSwiftSync owns planning, safety gates, orchestration, and verification; the selected engine owns message transfer.").color(self.theme_colors().text_secondary));
                ui.add_space(8.0);
                let editable = !self.running();
                ui.add_enabled_ui(editable, |ui| {
                    for engine in [core::Engine::Auto, core::Engine::Dovecot, core::Engine::ImapSync] {
                        ui.radio_value(&mut self.form.profile.engine, engine, engine.label());
                        if self.form.profile.engine == engine {
                            ui.label(RichText::new(engine.description()).size(11.0).color(self.theme_colors().text_secondary));
                        }
                    }
                    if self.form.profile.engine == core::Engine::Dovecot {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label("doveadm execution").on_hover_text(
                                "Local doveadm is supported. Remote execution remains disabled until a secret-safe broker is available.",
                            );
                            egui::ComboBox::from_id_salt("dovecot_execution")
                                .selected_text(match self.form.profile.dovecot_execution.as_str() {
                                    "local" => "Local machine",
                                    "ssh" => "Unavailable (secret broker required)",
                                    _ => "Automatic (local-only inference)",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.form.profile.dovecot_execution,
                                        "local".into(),
                                        "Local machine",
                                    );
                                    ui.selectable_value(
                                        &mut self.form.profile.dovecot_execution,
                                        "automatic".into(),
                                        "Automatic (local-only inference)",
                                    );
                                });
                        });
                        ui.horizontal(|ui| {
                            ui.label("doveadm");
                            ui.text_edit_singleline(&mut self.form.profile.doveadm_path);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Config");
                            ui.text_edit_singleline(&mut self.form.profile.dovecot_config);
                        });
                        ui.label(RichText::new("Remote Dovecot execution is unavailable until a secret-safe broker is implemented. Use local doveadm or imapsync.").size(11.0).color(self.theme_colors().danger));
                        ui.label(RichText::new("Dry mode only lists the destination mailbox. A live run uses sync -1; enabling destination deletion switches to backup.").size(11.0).color(self.theme_colors().text_secondary));
                    }
                });
                if !editable {
                    ui.label(RichText::new("Engine and execution settings are locked while a migration is running.").color(self.theme_colors().text_secondary));
                }
                ui.add_space(8.0);
                if ui.button("Continue to migration plan").clicked() {
                    close_requested = true;
                }
            });
        self.engine_open = open && !close_requested;
    }
}
