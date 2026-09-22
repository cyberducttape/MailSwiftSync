use crate::App;
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
    theme: &mut crate::ui::ThemeKind,
    dark_mode: &mut bool,
    ui_scale: &mut f32,
    branding: &mut crate::branding::OperatorBranding,
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
                    ui.label("Color pack");
                    egui::ComboBox::from_id_salt("appearance_theme")
                        .selected_text(theme.label())
                        .show_ui(ui, |ui| {
                            for option in crate::ui::ThemeKind::all() {
                                if ui.selectable_value(theme, *option, option.label()).clicked()
                                    && let Err(value) = (crate::ui::AppearancePreferences {
                                        theme: *theme,
                                        dark_mode: *dark_mode,
                                        ui_scale: *ui_scale,
                                    }).save()
                                {
                                    error = Some(format!("Could not save appearance preference: {value}"));
                                }
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label("Theme");
                    let label = if *dark_mode { "Dark" } else { "Light" };
                    if ui.button(label).clicked() {
                        *dark_mode = !*dark_mode;
                        if let Err(value) = (crate::ui::AppearancePreferences {
                            theme: *theme,
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
                        theme: *theme,
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
                ui.heading("Report branding");
                ui.label(RichText::new("Optional. Applied to customer-proof exports as an \"issued by\" line; independent of the migration plan and never affects preflight/live execution. Leave blank to omit it entirely.").size(11.0).color(if ui.visuals().dark_mode { crate::ui::ThemeColors::dark().text_secondary } else { crate::ui::ThemeColors::light().text_secondary }));
                ui.horizontal(|ui| {
                    ui.label("Agency/operator name");
                    ui.text_edit_singleline(&mut branding.name);
                });
                ui.horizontal(|ui| {
                    ui.label("Contact");
                    ui.text_edit_singleline(&mut branding.contact);
                });
                if ui.button("Save report branding").clicked()
                    && let Err(value) = branding.save()
                {
                    error = Some(format!("Could not save report branding: {value}"));
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

impl App {
    pub(crate) fn settings_dialog(&mut self, ctx: &egui::Context) {
        let result = show(
            ctx,
            &mut self.settings_open,
            &mut self.theme,
            &mut self.dark_mode,
            &mut self.ui_scale,
            &mut self.branding,
            self.form.engine(),
        );
        if let Some(error) = result.error {
            self.set_status(error, super::StatusSeverity::Error);
        }
        match result.action {
            Some(SettingsAction::OpenMigrationPlan) => {
                self.active_view = super::WorkspaceView::Plan;
            }
            Some(SettingsAction::OpenProjectBrowser) => {
                self.projects_open = true;
            }
            None => {}
        }
    }
}
