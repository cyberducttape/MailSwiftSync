use crate::*;

impl eframe::App for App {
    #[allow(clippy::possible_missing_else, clippy::collapsible_if)]
    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // egui 0.35 changed the panel API - TopBottomPanel and SidePanel are not available
        // This is a placeholder implementation that needs to be refactored with the new API
        // Keep operator-facing tables, status text, and logs readable on a
        // migration workstation. This is a default scale, not a substitute
        // for a future persisted Appearance preference.
        // ctx.set_zoom_factor(self.ui_scale);
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
        // Placeholder UI - panels not available in egui 0.35 without proper refactoring
    }
}
