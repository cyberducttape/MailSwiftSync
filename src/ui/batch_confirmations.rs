//! Confirmation dialogs for destructive or replacing batch actions.

use crate::App;
use eframe::egui;

impl App {
    pub(crate) fn bulk_clear_confirmation(&mut self, ctx: &egui::Context) {
        if !self.bulk_clear_confirm_open || self.running() {
            return;
        }
        let mut open = self.bulk_clear_confirm_open;
        let mut clear = false;
        let mut close_requested = false;
        egui::Window::new("Clear mailbox queue?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Discard the current queue?");
                ui.label(format!(
                    "This removes {} mailbox row(s), selection, in-memory passwords, and the durable batch association from this workspace.",
                    self.bulk_jobs.len()
                ));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Keep queue").clicked() {
                        close_requested = true;
                    }
                    if ui.button("Clear queue").clicked() {
                        clear = true;
                        close_requested = true;
                    }
                });
            });
        self.bulk_clear_confirm_open = open && !close_requested;
        if clear {
            self.clear_bulk_queue();
        }
    }

    pub(crate) fn bulk_import_confirmation(&mut self, ctx: &egui::Context) {
        if self.pending_bulk_import.is_none() || self.running() {
            return;
        }
        let path_label = self
            .pending_bulk_import
            .as_deref()
            .and_then(std::path::Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("the selected file")
            .to_owned();
        let mut open = true;
        let mut close_requested = false;
        let mut replace = false;
        egui::Window::new("Replace mailbox queue?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Replace the current queue?");
                ui.label(format!(
                    "Importing {path_label} will replace {} current mailbox row(s), selection, in-memory passwords, and the durable batch association.",
                    self.bulk_jobs.len()
                ));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Keep current queue").clicked() {
                        close_requested = true;
                    }
                    if ui.button("Replace queue").clicked() {
                        replace = true;
                        close_requested = true;
                    }
                });
            });
        if close_requested || !open {
            let path = self.pending_bulk_import.take();
            if replace && let Some(path) = path {
                self.import_bulk(&path);
            }
        }
    }
}
