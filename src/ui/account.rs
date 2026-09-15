use crate::{App, StatusSeverity, auth_method_is_oauth, credentials::SecretString};
use eframe::egui::{self, Color32, RichText};

use super::ThemeColors;

pub(crate) fn password_reveal_allowed(editable: bool, requested: bool) -> bool {
    editable && requested
}

pub(crate) fn password_visibility_id(title: &str) -> egui::Id {
    egui::Id::new(("password_visibility", title))
}

/// Render one source or destination account editor. The form controller owns
/// the values; this module owns only their presentation and validation hints.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_account(
    ui: &mut egui::Ui,
    title: &str,
    host: &mut String,
    user: &mut String,
    auth_method: &mut String,
    password: &mut SecretString,
    password_required: bool,
    saved_credential: bool,
    color: Color32,
) {
    let editable = ui.ctx().data(|data| {
        data.get_temp::<bool>(egui::Id::new("plan_controls_enabled"))
            .unwrap_or(true)
    });
    let danger = if ui.visuals().dark_mode {
        ThemeColors::dark().danger
    } else {
        ThemeColors::light().danger
    };
    let inline_error = |ui: &mut egui::Ui, label: &str, value: &str, required: bool| {
        let message = if required && value.trim().is_empty() {
            Some(format!("{label} is required."))
        } else if !value.is_empty() && value.chars().any(char::is_control) {
            Some(format!("{label} contains an invalid control character."))
        } else {
            None
        };
        if let Some(message) = message {
            ui.label(RichText::new(message).color(danger).size(11.0));
        }
    };
    ui.group(|ui| {
        ui.heading(RichText::new(title).color(color));
        ui.label(
            RichText::new(if title.contains("DOVECOT") {
                "Local Dovecot account"
            } else {
                "IMAP connection"
            })
            .size(11.0)
            .color(if ui.visuals().dark_mode {
                ThemeColors::dark().text_secondary
            } else {
                ThemeColors::light().text_secondary
            }),
        );
        ui.horizontal(|ui| {
            ui.label("Server");
            ui.add_enabled(editable, egui::TextEdit::singleline(host));
        });
        inline_error(ui, "Server", host, true);
        ui.horizontal(|ui| {
            ui.label("User");
            ui.add_enabled(editable, egui::TextEdit::singleline(user));
        });
        inline_error(ui, "User", user, true);
        ui.horizontal(|ui| {
            ui.label("Authentication");
            egui::ComboBox::from_id_salt(("auth_method", title))
                .selected_text(if auth_method_is_oauth(auth_method) {
                    "OAuth 2.0 / XOAUTH2"
                } else {
                    "Password"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(auth_method, "password".into(), "Password");
                    ui.selectable_value(auth_method, "oauth2".into(), "OAuth 2.0 / XOAUTH2");
                });
        });
        if auth_method_is_oauth(auth_method) {
            ui.label(
                RichText::new(
                    "Use a currently valid provider-issued access token with IMAP scope. Tokens are session-only unless stored in the OS keyring. MailSwiftSync does not request provider consent, but can refresh an expired token automatically if you configure a refresh token in the OS keyring dialog's Automatic OAuth refresh section.",
                )
                .size(11.0)
                .color(if ui.visuals().dark_mode {
                    ThemeColors::dark().text_secondary
                } else {
                    ThemeColors::light().text_secondary
                }),
            );
        }
        ui.horizontal(|ui| {
            ui.label(if auth_method_is_oauth(auth_method) {
                "Access token"
            } else {
                "Password"
            });
            let visibility_id = password_visibility_id(title);
            let visible = ui.ctx().data_mut(|data| {
                let requested = data.get_temp::<bool>(visibility_id).unwrap_or(false);
                if !editable {
                    data.remove::<bool>(visibility_id);
                }
                password_reveal_allowed(editable, requested)
            });
            ui.add_enabled(
                editable,
                egui::TextEdit::singleline(password.as_mut_string()).password(!visible),
            );
            if ui
                .add_enabled(
                    editable,
                    egui::Button::new(if visible { "Hide" } else { "Show" }),
                )
                .clicked()
            {
                ui.ctx()
                    .data_mut(|data| data.insert_temp(visibility_id, !visible));
            }
        });
        if saved_credential && password.is_empty() {
            ui.label(
                RichText::new(if auth_method_is_oauth(auth_method) {
                    "Saved OAuth credential configured; session token not required."
                } else {
                    "Saved credential configured; session password not required."
                })
                .color(if ui.visuals().dark_mode {
                    ThemeColors::dark().success
                } else {
                    ThemeColors::light().success
                })
                .size(12.0),
            );
        } else {
            inline_error(
                ui,
                if auth_method_is_oauth(auth_method) {
                    "Access token"
                } else {
                    "Password"
                },
                password.as_str(),
                password_required && !saved_credential,
            );
        }
    });
}

impl App {
    pub(crate) fn keyring_dialog(&mut self, ctx: &egui::Context) {
        if !self.keyring_open {
            return;
        }
        let mut open = self.keyring_open;
        egui::Window::new("OS keyring credentials")
            .open(&mut open)
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Keyring IDs are non-secret references saved in the profile. Passwords and OAuth access tokens stay in the operating system credential store and are loaded only into the active session.",
                    )
                    .color(self.theme_colors().text_secondary),
                );
                ui.add_space(8.0);
                let editable = !self.running();
                ui.add_enabled_ui(editable, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Source ID");
                        ui.text_edit_singleline(&mut self.form.profile.source_credential_id);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Destination ID");
                        ui.text_edit_singleline(&mut self.form.profile.destination_credential_id);
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let source_label = if auth_method_is_oauth(&self.form.profile.source_auth) {
                            "Store source token"
                        } else {
                            "Store source password"
                        };
                        if ui.button(source_label).clicked() {
                            match self.form.store_keyring_password(true) {
                                Ok(()) => self.set_status("Source credential stored in OS keyring", StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                        if ui.button("Load source").clicked() {
                            match self.form.load_keyring_password(true) {
                                Ok(()) => self.set_status("Source credential loaded", StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                        if ui.button("Delete source").clicked() {
                            match self.form.delete_keyring_password(true) {
                                Ok(()) => self.set_status("Source credential deleted from OS keyring", StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        let destination_label = if auth_method_is_oauth(&self.form.profile.destination_auth) {
                            "Store destination token"
                        } else {
                            "Store destination password"
                        };
                        if ui.button(destination_label).clicked() {
                            match self.form.store_keyring_password(false) {
                                Ok(()) => self.set_status("Destination credential stored in OS keyring", StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                        if ui.button("Load destination").clicked() {
                            match self.form.load_keyring_password(false) {
                                Ok(()) => self.set_status("Destination credential loaded", StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                        if ui.button("Delete destination").clicked() {
                            match self.form.delete_keyring_password(false) {
                                Ok(()) => self.set_status("Destination credential deleted from OS keyring", StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                    });
                });
                if !editable {
                    ui.label(RichText::new("Credential settings are locked while a migration is running.").color(self.theme_colors().text_secondary));
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "This is password storage, not OAuth/Modern Auth. Do not use it as a substitute for provider-specific OAuth setup or unattended secret brokering.",
                    )
                    .size(11.0)
                    .color(self.theme_colors().danger),
                );
                ui.separator();
                self.oauth_refresh_section(ui, editable);
            });
        self.keyring_open = open;
    }

    /// Optional automatic-refresh configuration: an operator who has
    /// registered their own OAuth application with the provider and holds a
    /// refresh token can store it here so `reload_configured_keyring_credentials`
    /// exchanges it for a fresh access token before each live launch instead
    /// of requiring a freshly copied token every time.
    fn oauth_refresh_section(&mut self, ui: &mut egui::Ui, editable: bool) {
        ui.label(RichText::new("Automatic OAuth refresh (optional)").strong());
        ui.label(
            RichText::new(
                "Requires an OAuth application you have already registered with the provider and a refresh token obtained through its consent flow. MailSwiftSync does not perform consent; it only exchanges an existing refresh token for a fresh access token before each live launch.",
            )
            .size(11.0)
            .color(self.theme_colors().text_secondary),
        );
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                ui.label("Source refresh ID");
                ui.text_edit_singleline(&mut self.form.profile.source_oauth_refresh_credential_id);
            });
            ui.horizontal(|ui| {
                ui.label("Destination refresh ID");
                ui.text_edit_singleline(
                    &mut self.form.profile.destination_oauth_refresh_credential_id,
                );
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Token endpoint");
                ui.text_edit_singleline(&mut self.oauth_refresh_editor_endpoint);
            });
            ui.horizontal(|ui| {
                ui.label("Client ID");
                ui.text_edit_singleline(&mut self.oauth_refresh_editor_client_id);
            });
            ui.horizontal(|ui| {
                ui.label("Client secret (if required)");
                ui.add(
                    egui::TextEdit::singleline(
                        self.oauth_refresh_editor_client_secret.as_mut_string(),
                    )
                    .password(true),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Refresh token");
                ui.add(
                    egui::TextEdit::singleline(
                        self.oauth_refresh_editor_refresh_token.as_mut_string(),
                    )
                    .password(true),
                );
            });
            ui.label(
                RichText::new(
                    "The fields above are entered once per store; they are cleared from memory immediately afterward and are never written to the profile or durable ledger.",
                )
                .size(11.0)
                .color(self.theme_colors().text_secondary),
            );
            ui.horizontal(|ui| {
                if ui.button("Store for source").clicked() {
                    self.store_oauth_refresh_editor(true);
                }
                if ui.button("Store for destination").clicked() {
                    self.store_oauth_refresh_editor(false);
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Refresh source now").clicked() {
                    self.run_manual_oauth_refresh(true);
                }
                if ui.button("Refresh destination now").clicked() {
                    self.run_manual_oauth_refresh(false);
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Delete source refresh config").clicked() {
                    match self.form.delete_oauth_refresh_config(true) {
                        Ok(()) => self.set_status(
                            "Source OAuth refresh configuration deleted",
                            StatusSeverity::Success,
                        ),
                        Err(error) => self.set_status(error, StatusSeverity::Error),
                    }
                }
                if ui.button("Delete destination refresh config").clicked() {
                    match self.form.delete_oauth_refresh_config(false) {
                        Ok(()) => self.set_status(
                            "Destination OAuth refresh configuration deleted",
                            StatusSeverity::Success,
                        ),
                        Err(error) => self.set_status(error, StatusSeverity::Error),
                    }
                }
            });
        });
    }

    fn run_manual_oauth_refresh(&mut self, source: bool) {
        let side = if source { "Source" } else { "Destination" };
        match self.form.refresh_oauth_access_token(source) {
            Ok(crate::migration_plan::OAuthRefreshOutcome::Refreshed { expires_in }) => {
                let message = match expires_in {
                    Some(seconds) => {
                        format!("{side} OAuth access token refreshed (expires in {seconds}s)")
                    }
                    None => format!("{side} OAuth access token refreshed"),
                };
                self.set_status(message, StatusSeverity::Success);
            }
            Ok(crate::migration_plan::OAuthRefreshOutcome::NotConfigured) => self.set_status(
                format!(
                    "No automatic refresh is configured for the {}",
                    side.to_lowercase()
                ),
                StatusSeverity::Info,
            ),
            Err(error) => self.set_status(error, StatusSeverity::Error),
        }
    }

    fn store_oauth_refresh_editor(&mut self, source: bool) {
        let config = crate::oauth_refresh::OAuthRefreshConfig {
            token_endpoint: std::mem::take(&mut self.oauth_refresh_editor_endpoint),
            client_id: std::mem::take(&mut self.oauth_refresh_editor_client_id),
            client_secret: std::mem::take(&mut self.oauth_refresh_editor_client_secret),
            refresh_token: std::mem::take(&mut self.oauth_refresh_editor_refresh_token),
        };
        let side = if source { "Source" } else { "Destination" };
        match self.form.store_oauth_refresh_config(source, &config) {
            Ok(()) => self.set_status(
                format!("{side} OAuth refresh configuration stored in OS keyring"),
                StatusSeverity::Success,
            ),
            Err(error) => self.set_status(error, StatusSeverity::Error),
        }
    }
}
