//! Migration-plan configuration views.

use crate::App;
use eframe::egui::{self, RichText};

impl App {
    pub(crate) fn advanced_dialog(&mut self, ctx: &egui::Context) {
        if !self.advanced_open {
            return;
        }
        let mut open = self.advanced_open;
        egui::Window::new("Advanced migration options")
            .open(&mut open)
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("These controls affect the imapsync fallback. Dovecot-native migrations use doveadm and server-side consistency rules.").color(self.theme_colors().text_secondary));
                ui.add_space(8.0);
                let editable = !self.running();
                ui.add_enabled_ui(editable, |ui| {
                    ui.group(|ui| {
                        ui.heading("Reliability and metadata");
                        ui.checkbox(&mut self.form.profile.sync_internaldates, "Sync internal dates  (--syncinternaldates)");
                        ui.checkbox(&mut self.form.profile.useuid, "Use message UIDs when available  (--useuid)");
                        ui.checkbox(&mut self.form.profile.usecache, "Use imapsync cache  (--usecache)");
                        ui.checkbox(&mut self.form.profile.allowsizemismatch, "Allow message-size mismatch  (--allowsizemismatch)");
                    });
                    ui.add_space(8.0);
                    ui.group(|ui| {
                        ui.heading("Performance");
                        ui.checkbox(&mut self.form.profile.fastio1, "Fast I/O for source  (--fastio1)")
                            .on_hover_text("Uses imapsync's faster source I/O path; test this with the provider before a production cutover.");
                        ui.checkbox(&mut self.form.profile.fastio2, "Fast I/O for destination  (--fastio2)")
                            .on_hover_text("Uses imapsync's faster destination I/O path; provider behavior varies.");
                        ui.horizontal(|ui| {
                            ui.label("Messages/second target (0 = unlimited)")
                                .on_hover_text("For a batch this is an aggregate target: MailSwiftSync divides it across concurrent imapsync workers. A single run uses the value unchanged.");
                            ui.add(egui::DragValue::new(&mut self.form.profile.max_messages_per_second).range(0..=100_000));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Bytes/second target (0 = unlimited)")
                                .on_hover_text("For a batch this is an aggregate target: MailSwiftSync divides it across concurrent imapsync workers. A single run uses the value unchanged.");
                            ui.add(egui::DragValue::new(&mut self.form.profile.max_bytes_per_second).range(0..=u64::MAX));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Process timeout (hours)")
                                .on_hover_text("Maximum wall-clock time for one engine process. It is a safety bound, not an estimate of completion time.");
                            ui.add(egui::DragValue::new(&mut self.form.profile.migration_timeout_hours).range(1..=720));
                        });
                        ui.label(RichText::new("Batch targets are divided across workers and process starts are globally paced; provider-side limits still take precedence. A finite target must be at least the worker count.").size(11.0).color(self.theme_colors().text_secondary));
                    });
                    ui.add_space(8.0);
                    ui.group(|ui| {
                        ui.heading(RichText::new("Destructive destination option").color(self.theme_colors().danger));
                        ui.checkbox(&mut self.form.profile.delete2, "Delete destination messages missing from source  (--delete2)");
                        ui.label(RichText::new("Use only for an intentionally exact backup after a tested preflight. This can remove destination mail.").size(11.0).color(self.theme_colors().danger));
                    });
                });
                if !editable {
                    ui.label(RichText::new("Advanced plan settings are locked while a migration is running.").color(self.theme_colors().text_secondary));
                }
                ui.add_space(8.0);
                ui.label("The Extra imapsync options field accepts only the documented safe tuning and diagnostic allowlist. Connection, credential, TLS, destructive, logging, and unknown flags are rejected.");
            });
        self.advanced_open = open;
    }
}
