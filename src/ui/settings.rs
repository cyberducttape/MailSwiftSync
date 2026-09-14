use crate::core;
use eframe::egui::{self, RichText};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SettingsAction {
    OpenMigrationPlan,
    OpenProjectBrowser,
}

pub(crate) struct SettingsResult {
    pub(crate) action: Option<SettingsAction>,
    pub(crate) error: Option<String>,
}

/// Render operator/workspace settings without depending on the application
/// shell. Migration controls intentionally live on the plan screen; this
/// dialog only returns navigation actions to the shell.
pub(crate) fn show(
    ctx: &egui::Context,
    open: &mut bool,
    dark_mode: &mut bool,
    ui_scale: &mut f32,
    engine: core::Engine,
) -> SettingsResult {
    if !*open {
        return SettingsResult {
            action: None,
            error: None,
        };
    }
    let mut window_open = *open;
    let mut close_requested = false;
    let mut action = None;
    let mut error = None;
    egui::Window::new("Settings")
        .open(&mut window_open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.heading("Operator settings");
            ui.label(RichText::new("Appearance and workspace tools live here. Migration connection and engine choices belong on the Migration plan so the active plan stays visible while you configure it.").color(if ui.visuals().dark_mode { crate::ui::ThemeColors::dark().text_secondary } else { crate::ui::ThemeColors::light().text_secondary }));
            ui.add_space(8.0);
            ui.group(|ui| {
                ui.heading("Appearance");
                ui.horizontal(|ui| {
                    ui.label("Theme");
                    let label = if *dark_mode { "Dark" } else { "Light" };
                    if ui.button(label).clicked() {
                        *dark_mode = !*dark_mode;
                        if let Err(value) = (crate::ui::AppearancePreferences {
                            dark_mode: *dark_mode,
                            ui_scale: *ui_scale,
                        })
                        .save()
                        {
                            error = Some(format!("Could not save appearance preference: {value}"));
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(format!("Interface size: {:.0}%", *ui_scale * 100.0));
                    if ui.button("Decrease").clicked() {
                        *ui_scale = (*ui_scale - 0.10).max(0.90);
                    }
                    if ui.button("Increase").clicked() {
                        *ui_scale = (*ui_scale + 0.10).min(1.50);
                    }
                });
                if ui.button("Save appearance preferences").clicked()
                    && let Err(value) = (crate::ui::AppearancePreferences {
                        dark_mode: *dark_mode,
                        ui_scale: *ui_scale,
                    })
                    .save()
                {
                    error = Some(format!("Could not save appearance preference: {value}"));
                }
            });
            ui.add_space(8.0);
            ui.group(|ui| {
                ui.heading("Workspace tools");
                if ui.button("Open Migration plan").clicked() {
                    action = Some(SettingsAction::OpenMigrationPlan);
                    close_requested = true;
                }
                if ui.button("Project browser").clicked() {
                    action = Some(SettingsAction::OpenProjectBrowser);
                    close_requested = true;
                }
                ui.label(RichText::new(format!("Current engine: {}. Connection, credentials, advanced options, and readiness are available from the Migration plan.", engine.label())).size(11.0).color(if ui.visuals().dark_mode { crate::ui::ThemeColors::dark().text_secondary } else { crate::ui::ThemeColors::light().text_secondary }));
            });
        });
    *open = window_open && !close_requested;
    SettingsResult { action, error }
}
