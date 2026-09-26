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
                            ui.label("doveadm");
                            ui.text_edit_singleline(&mut self.form.profile.doveadm_path);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Config");
                            ui.text_edit_singleline(&mut self.form.profile.dovecot_config);
                        });
                        ui.label(RichText::new("Native Dovecot execution is local-only until a secret-safe broker is implemented.").size(11.0).color(self.theme_colors().text_secondary));
                        ui.label(RichText::new("Dry mode only lists the destination mailbox. Native Dovecot uses the selected migration strategy; backup and sync -1 have different merge behavior.").size(11.0).color(self.theme_colors().text_secondary));
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
