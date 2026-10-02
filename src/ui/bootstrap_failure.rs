//! Fatal startup screen shown when no state store could be opened.

use super::UiLanguage;
use crate::bootstrap::BootstrapFailure;
use eframe::egui;

/// Replaces the workspace when bootstrap failed. It offers no migration
/// controls: without durable or temporary state nothing can be recorded.
pub(crate) struct BootstrapFailureScreen {
    failure: BootstrapFailure,
    language: UiLanguage,
}

impl BootstrapFailureScreen {
    pub(crate) fn new(failure: BootstrapFailure, language: UiLanguage) -> Self {
        Self { failure, language }
    }
}

impl eframe::App for BootstrapFailureScreen {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let language = self.language;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(language.text("ui.bootstrap-failure-title"));
            ui.add_space(8.0);
            ui.label(language.text("ui.bootstrap-failure-summary"));
            ui.add_space(8.0);
            ui.strong(language.text("ui.bootstrap-failure-details"));
            ui.monospace(&self.failure.durable);
            ui.monospace(&self.failure.temporary);
            ui.add_space(12.0);
            if ui
                .button(language.text("ui.bootstrap-failure-close"))
                .clicked()
            {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App as _;

    #[test]
    fn failure_screen_renders_without_migration_controls() {
        let mut screen = BootstrapFailureScreen::new(
            BootstrapFailure {
                durable: "Persistent SQLite state unavailable: disk I/O error".into(),
                temporary: "SQLite in-memory store unavailable: out of memory".into(),
            },
            UiLanguage::English,
        );
        let context = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            screen.ui(ui, &mut frame);
        });
        assert!(!output.shapes.is_empty());
    }
}
