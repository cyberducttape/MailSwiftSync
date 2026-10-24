//! Project hold: a banner on every page while new launches are held, the
//! button that opens the hold dialog, and the dialog itself.

use crate::App;
use eframe::egui::{self, RichText};

impl App {
    /// The operator name recorded for holds placed and lifted from this
    /// window; the same identity wave approvals use.
    fn hold_operator(&mut self) -> String {
        if self.waves.approver.trim().is_empty() {
            self.waves.approver = std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_default();
        }
        self.waves.approver.trim().to_owned()
    }

    /// Shown above every workspace page while the selected project is held,
    /// so a pause placed from the fleet API or the CLI is never invisible.
    pub(crate) fn hold_banner(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        if let Some(hold) = self.ui_snapshot.project_hold.clone() {
            let project_id = self
                .ui_snapshot
                .project
                .as_ref()
                .map(|project| project.id.clone());
            crate::ui::card(ui, |ui| {
                ui.label(
                    RichText::new(self.language.message("ui.hold-banner-title"))
                        .strong()
                        .color(colors.warning),
                );
                ui.label(
                    self.language
                        .message("ui.hold-banner-detail")
                        .replace("{reason}", &hold.reason)
                        .replace("{by}", &hold.held_by)
                        .replace("{at}", &hold.held_at),
                );
                if let Some(project_id) = project_id
                    && ui.button(self.language.message("ui.hold-resume")).clicked()
                {
                    let operator = self.hold_operator();
                    let result = self.store.release_project_hold(&project_id, &operator);
                    self.hold_message = Some(match result {
                        Ok(_) => (true, self.language.message("ui.hold-resumed").to_owned()),
                        Err(error) => (false, error.to_string()),
                    });
                    self.refresh_ui_snapshot_now();
                }
            });
            ui.add_space(12.0);
        }
        if let Some((ok, message)) = &self.hold_message {
            ui.label(RichText::new(message).color(if *ok {
                colors.success
            } else {
                colors.danger
            }));
            ui.add_space(8.0);
        }
    }

    /// Offered wherever a run can be started or stopped; hidden while the
    /// project is already held (the banner offers Resume instead).
    pub(crate) fn hold_button(&mut self, ui: &mut egui::Ui) {
        if self.ui_snapshot.project.is_none() || self.ui_snapshot.project_hold.is_some() {
            return;
        }
        if ui.button(self.language.message("ui.hold-open")).clicked() {
            self.hold_draft = Some(String::new());
            self.hold_focus_requested = false;
        }
    }

    pub(crate) fn hold_dialog(&mut self, ctx: &egui::Context) {
        if self.hold_draft.is_none() {
            return;
        }
        // Keep the draft while the project snapshot is still loading.
        let Some(project_id) = self
            .ui_snapshot
            .project
            .as_ref()
            .map(|project| project.id.clone())
        else {
            return;
        };
        let mut reason = self.hold_draft.take().unwrap_or_default();
        let mut operator = self.hold_operator();
        let mut close = false;
        let mut confirm = false;
        let response =
            egui::Modal::new(egui::Id::new("project_hold_confirmation")).show(ctx, |ui| {
                let heading = ui.heading(self.language.message("ui.hold-dialog-title"));
                crate::ui::name_modal(ui, &heading);
                ui.label(self.language.message("ui.hold-dialog-body"));
                ui.add_space(8.0);
                crate::ui::form_row(ui, self.language.message("ui.operator"), |ui| {
                    ui.add(egui::TextEdit::singleline(&mut operator).desired_width(260.0))
                });
                crate::ui::form_row(ui, self.language.message("ui.hold-reason"), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut reason)
                            .hint_text(self.language.message("ui.hold-reason-hint"))
                            .desired_width(260.0),
                    )
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let keep = ui.button(self.language.message("ui.hold-cancel"));
                    if !self.hold_focus_requested {
                        keep.request_focus();
                        self.hold_focus_requested = true;
                    }
                    if keep.clicked() {
                        close = true;
                    }
                    let ready = !operator.trim().is_empty() && !reason.trim().is_empty();
                    if ui
                        .add_enabled(
                            ready,
                            egui::Button::new(self.language.message("ui.hold-confirm")),
                        )
                        .clicked()
                    {
                        confirm = true;
                    }
                });
            });
        self.waves.approver = operator.clone();
        if confirm {
            let result = self.store.hold_project(&project_id, &operator, &reason);
            self.hold_message = Some(match result {
                Ok(_) => (true, self.language.message("ui.hold-placed").to_owned()),
                Err(error) => (false, error.to_string()),
            });
            self.refresh_ui_snapshot_now();
            return;
        }
        if !(close || response.should_close()) {
            self.hold_draft = Some(reason);
        }
    }
}
