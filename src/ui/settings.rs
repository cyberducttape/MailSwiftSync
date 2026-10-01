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
    preferences: &mut crate::ui::AppearancePreferences,
    branding: &mut crate::branding::OperatorBranding,
    engine: core::Engine,
) -> SettingsResult {
    let theme = &mut preferences.theme;
    let dark_mode = &mut preferences.dark_mode;
    let ui_scale = &mut preferences.ui_scale;
    let language = &mut preferences.language;
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
    egui::Window::new(language.message("ui.settings"))
        .open(&mut window_open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.heading(language.message("ui.operator-settings"));
            ui.label(RichText::new(language.message("ui.appearance-and-workspace-tools-live-here-migration-connection-and-engine-ch-5f254b5b82")).color(if ui.visuals().dark_mode { crate::ui::ThemeColors::dark().text_secondary } else { crate::ui::ThemeColors::light().text_secondary }));
            ui.add_space(8.0);
            crate::ui::card(ui, |ui| {
                ui.heading(language.message("ui.appearance"));
                ui.horizontal(|ui| {
                    ui.label(language.message("ui.color-pack"));
                    egui::ComboBox::from_id_salt("appearance_theme")
                        .selected_text(theme.label())
                        .show_ui(ui, |ui| {
                            for option in crate::ui::ThemeKind::recommended() {
                                if ui.selectable_value(theme, *option, option.label()).clicked()
                                    && let Err(value) = (crate::ui::AppearancePreferences {
                                        theme: *theme,
                                        language: *language,
                                        dark_mode: *dark_mode,
                                        ui_scale: *ui_scale,
                                    }).save()
                                {
                                    error = Some(format!("{}: {value}", language.message("ui.could-not-save-appearance-preference")));
                                }
                            }
                        });
                });
                ui.collapsing(language.message("ui.additional-themes"), |ui| {
                    for option in crate::ui::ThemeKind::additional() {
                        if ui.selectable_value(theme, *option, option.label()).clicked()
                            && let Err(value) = (crate::ui::AppearancePreferences {
                                theme: *theme,
                                language: *language,
                                dark_mode: *dark_mode,
                                ui_scale: *ui_scale,
                            })
                            .save()
                        {
                            error = Some(format!(
                                "{}: {value}",
                                language.message("ui.could-not-save-appearance-preference")
                            ));
                        }
                    }
                });
                let theme_is_variable = matches!(theme, crate::ui::ThemeKind::Default);
                if theme_is_variable {
                    ui.horizontal(|ui| {
                        ui.label(language.message("ui.theme"));
                        let label = language.text(if *dark_mode { "Dark" } else { "Light" });
                        if ui.button(label).clicked() {
                            *dark_mode = !*dark_mode;
                            if let Err(value) = (crate::ui::AppearancePreferences {
                                theme: *theme,
                                language: *language,
                                dark_mode: *dark_mode,
                                ui_scale: *ui_scale,
                            })
                            .save()
                            {
                                error = Some(format!("{}: {value}", language.message("ui.could-not-save-appearance-preference")));
                            }
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.label(language.message("ui.theme"));
                        ui.label(RichText::new(theme.label()).color(if ui.visuals().dark_mode { crate::ui::ThemeColors::dark().text_secondary } else { crate::ui::ThemeColors::light().text_secondary }));
                        ui.label(RichText::new(language.message("ui.fixed-palette")).size(12.0).color(if ui.visuals().dark_mode { crate::ui::ThemeColors::dark().text_secondary } else { crate::ui::ThemeColors::light().text_secondary }));
                    });
                }
                ui.horizontal(|ui| {
                    ui.label(format!("{}: {:.0}%", language.message("ui.interface-size"), *ui_scale * 100.0));
                    let scale_before = *ui_scale;
                    if ui.button(language.message("ui.decrease")).clicked() {
                        *ui_scale = (*ui_scale - 0.10).max(0.90);
                    }
                    if ui.button(language.message("ui.increase")).clicked() {
                        *ui_scale = (*ui_scale + 0.10).min(crate::MAX_UI_SCALE);
                    }
                    if *ui_scale != scale_before
                        && let Err(value) = (crate::ui::AppearancePreferences {
                            theme: *theme,
                            language: *language,
                            dark_mode: *dark_mode,
                            ui_scale: *ui_scale,
                        })
                        .save()
                    {
                        error = Some(format!(
                            "{}: {value}",
                            language.message("ui.could-not-save-appearance-preference")
                        ));
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(language.message("ui.language"));
                    egui::ComboBox::from_id_salt("appearance_language")
                        .selected_text(language.label())
                        .show_ui(ui, |ui| {
                            for option in crate::ui::UiLanguage::all() {
                                if ui.selectable_value(language, *option, option.label()).clicked()
                                    && let Err(value) = (crate::ui::AppearancePreferences {
                                        theme: *theme,
                                        language: *language,
                                        dark_mode: *dark_mode,
                                        ui_scale: *ui_scale,
                                    }).save()
                                {
                                    error = Some(format!("{}: {value}", language.message("ui.could-not-save-appearance-preference")));
                                }
                            }
                        });
                });
            });
            ui.add_space(8.0);
            crate::ui::card(ui, |ui| {
                ui.heading(language.message("ui.report-branding"));
                ui.label(RichText::new(language.message("ui.optional-applied-to-customer-proof-exports-as-an-issued-by-line-independent-90154a5591")).size(12.0).color(if ui.visuals().dark_mode { crate::ui::ThemeColors::dark().text_secondary } else { crate::ui::ThemeColors::light().text_secondary }));
                ui.horizontal(|ui| {
                    ui.label(language.message("ui.agency-operator-name"));
                    ui.text_edit_singleline(&mut branding.name);
                });
                ui.horizontal(|ui| {
                    ui.label(language.message("ui.contact"));
                    ui.text_edit_singleline(&mut branding.contact);
                });
                if ui.button(language.message("ui.save-report-branding")).clicked()
                    && let Err(value) = branding.save()
                {
                    error = Some(format!("{}: {value}", language.message("ui.could-not-save-report-branding")));
                }
            });
            ui.add_space(8.0);
            crate::ui::card(ui, |ui| {
                ui.heading(language.message("ui.workspace-tools"));
                if ui.button(language.message("ui.open-migration-plan-856d9db3")).clicked() {
                    action = Some(SettingsAction::OpenMigrationPlan);
                    close_requested = true;
                }
                if ui.button(language.message("ui.project-browser")).clicked() {
                    action = Some(SettingsAction::OpenProjectBrowser);
                    close_requested = true;
                }
                ui.label(RichText::new(format!("{} {}. {}", language.message("ui.current-engine"), language.text(engine.label()), language.message("ui.connection-credentials-advanced-options-and-readiness-are-available-from-th-89dbcd1dd5"))).size(12.0).color(if ui.visuals().dark_mode { crate::ui::ThemeColors::dark().text_secondary } else { crate::ui::ThemeColors::light().text_secondary }));
            });
        });
    *open = window_open && !close_requested;
    SettingsResult { action, error }
}

impl App {
    pub(crate) fn settings_dialog(&mut self, ctx: &egui::Context) {
        let mut preferences = crate::ui::AppearancePreferences {
            theme: self.theme,
            language: self.language,
            dark_mode: self.dark_mode,
            ui_scale: self.ui_scale,
        };
        let result = show(
            ctx,
            &mut self.settings_open,
            &mut preferences,
            &mut self.branding,
            self.form.engine(),
        );
        self.theme = preferences.theme;
        self.language = preferences.language;
        self.dark_mode = preferences.dark_mode;
        self.ui_scale = preferences.ui_scale;
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
