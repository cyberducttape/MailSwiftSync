use crate::{App, StatusSeverity, auth_method_is_oauth, credentials::SecretString};
use eframe::egui::{self, Color32, RichText};
use sha2::{Digest, Sha256};
use std::sync::mpsc;

use super::app_state::{CredentialDeleteTarget, ManualOAuthRefreshResult};

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
    language: crate::ui::UiLanguage,
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
    let danger = ui.visuals().error_fg_color;
    let inline_error = |ui: &mut egui::Ui, label: &str, value: &str, required: bool| {
        let message = if required && value.trim().is_empty() {
            Some(language.text("{} is required.").replace("{}", label))
        } else if !value.is_empty() && value.chars().any(char::is_control) {
            Some(
                language
                    .text("{} contains an invalid control character.")
                    .replace("{}", label),
            )
        } else {
            None
        };
        if let Some(message) = message {
            ui.label(RichText::new(message).color(danger).size(11.0));
        }
    };
    crate::ui::card(ui, |ui| {
        ui.label(RichText::new(title).size(16.0).strong().color(color));
        ui.label(
            RichText::new(if title.to_lowercase().contains("dovecot") {
                language.text("Local Dovecot account")
            } else {
                language.text("IMAP connection")
            })
            .size(11.0)
            .color(ui.visuals().weak_text_color()),
        );
        crate::ui::form_row(ui, language.text("Server"), |ui| {
            ui.add_enabled(
                editable,
                egui::TextEdit::singleline(host).desired_width(f32::INFINITY),
            )
        });
        inline_error(ui, language.text("Server"), host, true);
        crate::ui::form_row(ui, language.text("User"), |ui| {
            ui.add_enabled(
                editable,
                egui::TextEdit::singleline(user).desired_width(f32::INFINITY),
            )
        });
        inline_error(ui, language.text("User"), user, true);
        ui.collapsing(language.text("Account authorization"), |ui| {
        crate::ui::form_row(ui, language.text("Authentication"), |ui| {
            egui::ComboBox::from_id_salt(("auth_method", title))
                .selected_text(if auth_method_is_oauth(auth_method) {
                    "OAuth 2.0 / XOAUTH2"
                } else {
                    language.text("Password")
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(auth_method, "password".into(), language.text("Password"));
                    ui.selectable_value(auth_method, "oauth2".into(), "OAuth 2.0 / XOAUTH2");
                });
        });
        if auth_method_is_oauth(auth_method) {
            ui.collapsing(language.text("OAuth setup guidance"), |ui| {
                ui.label(
                    RichText::new(
                        language.text("Use a provider-issued access token with IMAP scope. Tokens stay in this session unless stored in the OS keyring. For automatic refresh, run `mailswiftsync oauth-authorize` and add its keyring ID in Advanced migration settings."),
                    )
                    .size(11.0)
                    .color(ui.visuals().weak_text_color()),
                );
            });
        }
        let secret_label = if auth_method_is_oauth(auth_method) {
            language.text("Access token")
        } else {
            language.text("Password")
        };
        crate::ui::form_row(ui, secret_label, |ui| {
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
                egui::TextEdit::singleline(password.as_mut_string())
                    .password(!visible)
                    .desired_width((ui.available_width() - 64.0).max(80.0)),
            );
            if ui
                .add_enabled(
                    editable,
                    egui::Button::new(language.text(if visible { "Hide" } else { "Show" })),
                )
                .clicked()
            {
                ui.ctx()
                    .data_mut(|data| data.insert_temp(visibility_id, !visible));
            }
        });
        if saved_credential && password.is_empty() {
            ui.label(
                RichText::new(language.text(if auth_method_is_oauth(auth_method) {
                    "Saved OAuth credential configured; session token not required."
                } else {
                    "Saved credential configured; session password not required."
                }))
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
                language.text(if auth_method_is_oauth(auth_method) {
                    "Access token"
                } else {
                    "Password"
                }),
                password.as_str(),
                password_required && !saved_credential,
            );
        }
        });
        if !saved_credential && password.is_empty() {
            ui.label(
                RichText::new(
                    language.text("Expand account authorization to add a password or OAuth token."),
                )
                .small()
                .color(ui.visuals().weak_text_color()),
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
        egui::Window::new(self.language.text("OS keyring credentials"))
            .open(&mut open)
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                    self.language.text("Keyring IDs are non-secret references saved in the profile. Passwords and OAuth access tokens stay in the operating system credential store and are loaded only into the active session."),
                    )
                    .color(self.theme_colors().text_secondary),
                );
                ui.add_space(8.0);
                let editable = !self.running() && self.manual_oauth_refresh_receiver.is_none();
                ui.add_enabled_ui(editable, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(self.language.text("Source ID"));
                        ui.text_edit_singleline(&mut self.form.profile.source_credential_id);
                    });
                    ui.horizontal(|ui| {
                        ui.label(self.language.text("Destination ID"));
                        ui.text_edit_singleline(&mut self.form.profile.destination_credential_id);
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let source_label = if auth_method_is_oauth(&self.form.profile.source_auth) {
                            self.language.text("Store source token")
                        } else {
                            self.language.text("Store source password")
                        };
                        if ui.button(source_label).clicked() {
                            match self.form.store_keyring_password(true) {
                                Ok(()) => self.set_status(self.language.text("Source credential stored in OS keyring"), StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                        if ui.button(self.language.text("Load source")).clicked() {
                            match self.form.load_keyring_password(true) {
                                Ok(()) => self.set_status(self.language.text("Source credential loaded"), StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                        if ui.button(self.language.text("Delete source")).clicked() {
                            self.credential_delete_confirmation =
                                Some(CredentialDeleteTarget::Password { source: true });
                        }
                    });
                    ui.horizontal(|ui| {
                        let destination_label = if auth_method_is_oauth(&self.form.profile.destination_auth) {
                            self.language.text("Store destination token")
                        } else {
                            self.language.text("Store destination password")
                        };
                        if ui.button(destination_label).clicked() {
                            match self.form.store_keyring_password(false) {
                                Ok(()) => self.set_status(self.language.text("Destination credential stored in OS keyring"), StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                        if ui.button(self.language.text("Load destination")).clicked() {
                            match self.form.load_keyring_password(false) {
                                Ok(()) => self.set_status(self.language.text("Destination credential loaded"), StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                        if ui.button(self.language.text("Delete destination")).clicked() {
                            self.credential_delete_confirmation =
                                Some(CredentialDeleteTarget::Password { source: false });
                        }
                    });
                });
                if !editable {
                    ui.label(RichText::new(self.language.text("Credential settings are locked while a migration is running or OAuth refresh is in progress.")).color(self.theme_colors().text_secondary));
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        self.language.text("This is password storage, not OAuth/Modern Auth. Do not use it as a substitute for provider-specific OAuth setup or unattended secret brokering."),
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
        ui.label(RichText::new(self.language.text("Automatic OAuth refresh (optional)")).strong());
        ui.label(
            RichText::new(
                self.language.text("Requires an OAuth application you have registered with the provider. Run `mailswiftsync oauth-authorize` to complete consent in a browser and store the refresh configuration under a keyring ID, or enter a refresh token obtained through the provider's own tooling below. Before each live launch MailSwiftSync exchanges it for a fresh access token."),
            )
            .size(11.0)
            .color(self.theme_colors().text_secondary),
        );
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                ui.label(self.language.text("Source refresh ID"));
                ui.text_edit_singleline(&mut self.form.profile.source_oauth_refresh_credential_id);
            });
            ui.horizontal(|ui| {
                ui.label(self.language.text("Destination refresh ID"));
                ui.text_edit_singleline(
                    &mut self.form.profile.destination_oauth_refresh_credential_id,
                );
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(self.language.text("Token endpoint"));
                ui.text_edit_singleline(&mut self.oauth_refresh_editor_endpoint);
            });
            ui.horizontal(|ui| {
                ui.label(self.language.text("Client ID"));
                ui.text_edit_singleline(&mut self.oauth_refresh_editor_client_id);
            });
            ui.horizontal(|ui| {
                ui.label(self.language.text("Client secret (if required)"));
                ui.add(
                    egui::TextEdit::singleline(
                        self.oauth_refresh_editor_client_secret.as_mut_string(),
                    )
                    .password(true),
                );
            });
            ui.horizontal(|ui| {
                ui.label(self.language.text("Refresh token"));
                ui.add(
                    egui::TextEdit::singleline(
                        self.oauth_refresh_editor_refresh_token.as_mut_string(),
                    )
                    .password(true),
                );
            });
            ui.label(
                RichText::new(
                    self.language.text("The fields above are entered once per store; they are cleared from memory immediately afterward and are never written to the profile or durable ledger."),
                )
                .size(11.0)
                .color(self.theme_colors().text_secondary),
            );
            ui.horizontal(|ui| {
                if ui.button(self.language.text("Store for source")).clicked() {
                    self.store_oauth_refresh_editor(true);
                }
                if ui.button(self.language.text("Store for destination")).clicked() {
                    self.store_oauth_refresh_editor(false);
                }
            });
            ui.horizontal(|ui| {
                let refresh_pending = self.manual_oauth_refresh_receiver.is_some();
                if ui
                    .add_enabled(
                        !refresh_pending,
                        egui::Button::new(if refresh_pending {
                            self.language.text("Refreshing OAuth token…")
                        } else {
                            self.language.text("Refresh source now")
                        }),
                    )
                    .clicked()
                {
                    self.run_manual_oauth_refresh(true);
                }
                if ui
                    .add_enabled(
                        !refresh_pending,
                        egui::Button::new(self.language.text("Refresh destination now")),
                    )
                    .clicked()
                {
                    self.run_manual_oauth_refresh(false);
                }
            });
            ui.horizontal(|ui| {
                if ui.button(self.language.text("Delete source refresh config")).clicked() {
                    self.credential_delete_confirmation =
                        Some(CredentialDeleteTarget::OAuthRefresh { source: true });
                }
                if ui.button(self.language.text("Delete destination refresh config")).clicked() {
                    self.credential_delete_confirmation =
                        Some(CredentialDeleteTarget::OAuthRefresh { source: false });
                }
            });
        });
    }

    fn run_manual_oauth_refresh(&mut self, source: bool) {
        if self.manual_oauth_refresh_receiver.is_some() {
            return;
        }
        let form = self.form.clone();
        let worker_marker = manual_oauth_refresh_marker(&form, source);
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("mailswiftsync-oauth-refresh-ui".into())
            .spawn(move || {
                let mut form = form;
                let result = form.refresh_oauth_access_token(source).map(|outcome| {
                    let access_token = if source {
                        form.source_password
                    } else {
                        form.destination_password
                    };
                    (outcome, access_token)
                });
                let _ = sender.send(ManualOAuthRefreshResult {
                    source,
                    marker: worker_marker,
                    result,
                });
            }) {
            Ok(_) => {
                self.manual_oauth_refresh_receiver = Some(receiver);
                self.set_status(
                    "Refreshing OAuth token in the background…",
                    StatusSeverity::Info,
                );
            }
            Err(error) => self.set_status(
                format!("Could not start OAuth refresh worker: {error}"),
                StatusSeverity::Error,
            ),
        }
    }

    pub(crate) fn complete_manual_oauth_refresh(&mut self, result: ManualOAuthRefreshResult) {
        if result.marker != manual_oauth_refresh_marker(&self.form, result.source) {
            self.set_status(
                "OAuth refresh finished after account settings changed; its access token was discarded. Review the account and refresh again.",
                StatusSeverity::Warning,
            );
            return;
        }
        let side = self.language.text(if result.source {
            "Source"
        } else {
            "Destination"
        });
        match result.result {
            Ok((crate::migration_plan::OAuthRefreshOutcome::Refreshed { expires_in }, token)) => {
                if result.source {
                    self.form.source_password = token;
                } else {
                    self.form.destination_password = token;
                }
                let message = match expires_in {
                    Some(seconds) => self
                        .language
                        .text("{} OAuth access token refreshed (expires in {}s)")
                        .replacen("{}", side, 1)
                        .replacen("{}", &seconds.to_string(), 1),
                    None => self
                        .language
                        .text("{} OAuth access token refreshed")
                        .replace("{}", side),
                };
                self.set_status(message, StatusSeverity::Success);
            }
            Ok((crate::migration_plan::OAuthRefreshOutcome::NotConfigured, _)) => {
                self.set_status(
                    self.language
                        .text("No automatic refresh is configured for the {}")
                        .replace("{}", &side.to_lowercase()),
                    StatusSeverity::Info,
                );
            }
            Err(error) => self.set_status(error, StatusSeverity::Error),
        }
    }

    pub(crate) fn credential_delete_confirmation(&mut self, ctx: &egui::Context) {
        let Some(target) = self.credential_delete_confirmation else {
            return;
        };
        let (source, kind, keyring_id) = match target {
            CredentialDeleteTarget::Password { source } => (
                source,
                self.language.text("saved password / access token"),
                if source {
                    self.form.profile.source_credential_id.as_str()
                } else {
                    self.form.profile.destination_credential_id.as_str()
                }
                .to_owned(),
            ),
            CredentialDeleteTarget::OAuthRefresh { source } => (
                source,
                self.language.text("automatic OAuth refresh configuration"),
                if source {
                    self.form
                        .profile
                        .source_oauth_refresh_credential_id
                        .as_str()
                } else {
                    self.form
                        .profile
                        .destination_oauth_refresh_credential_id
                        .as_str()
                }
                .to_owned(),
            ),
        };
        let side = self
            .language
            .text(if source { "Source" } else { "Destination" });
        let impact = match target {
            CredentialDeleteTarget::Password { .. } => self.language.text(
                "This permanently removes the saved credential from the OS keyring.",
            ),
            CredentialDeleteTarget::OAuthRefresh { .. } => self.language.text(
                "This removes only the locally stored OAuth refresh configuration; it does not revoke the provider token.",
            ),
        };
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("credential_delete_confirmation"))
            .show(ctx, |ui| {
                ui.heading(
                    RichText::new(self.language.text("Delete saved credential?"))
                        .color(self.theme_colors().danger),
                );
                ui.label(
                    self.language
                        .text("{}: {} · keyring ID: {}")
                        .replacen("{}", side, 1)
                        .replacen("{}", kind, 1)
                        .replacen("{}", &keyring_id, 1),
                );
                ui.label(format!(
                    "{impact} {}",
                    self.language.text("The profile reference and any credential already loaded in this session are not changed.")
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let cancel = ui.button(self.language.text("Cancel"));
                    if !self.credential_delete_focus_requested {
                        cancel.request_focus();
                        self.credential_delete_focus_requested = true;
                    }
                    if cancel.clicked() {
                        close = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(self.language.text("Delete saved credential"))
                                .fill(self.theme_colors().danger),
                        )
                        .clicked()
                    {
                        close = true;
                        let result = match target {
                            CredentialDeleteTarget::Password { source } => {
                                self.form.delete_keyring_password(source)
                            }
                            CredentialDeleteTarget::OAuthRefresh { source } => {
                                self.form.delete_oauth_refresh_config(source)
                            }
                        };
                        match result {
                            Ok(()) => self.set_status(
                                self.language.text(match target {
                                    CredentialDeleteTarget::Password { source: true } => {
                                        "Source credential deleted from OS keyring"
                                    }
                                    CredentialDeleteTarget::Password { source: false } => {
                                        "Destination credential deleted from OS keyring"
                                    }
                                    CredentialDeleteTarget::OAuthRefresh { source: true } => {
                                        "Source OAuth refresh configuration deleted"
                                    }
                                    CredentialDeleteTarget::OAuthRefresh { source: false } => {
                                        "Destination OAuth refresh configuration deleted"
                                    }
                                }),
                                StatusSeverity::Success,
                            ),
                            Err(error) => self.set_status(error, StatusSeverity::Error),
                        }
                    }
                });
            });
        if close || response.should_close() {
            self.credential_delete_confirmation = None;
            self.credential_delete_focus_requested = false;
        }
    }

    fn store_oauth_refresh_editor(&mut self, source: bool) {
        let side = self
            .language
            .text(if source { "Source" } else { "Destination" });
        let result = store_oauth_refresh_editor_values(
            &mut self.oauth_refresh_editor_endpoint,
            &mut self.oauth_refresh_editor_client_id,
            &mut self.oauth_refresh_editor_client_secret,
            &mut self.oauth_refresh_editor_refresh_token,
            |config| self.form.store_oauth_refresh_config(source, config),
        );
        match result {
            Ok(()) => {
                self.set_status(
                    self.language
                        .text("{} OAuth refresh configuration stored in OS keyring")
                        .replace("{}", side),
                    StatusSeverity::Success,
                );
            }
            Err(error) => self.set_status(error, StatusSeverity::Error),
        }
    }
}

fn store_oauth_refresh_editor_values(
    endpoint: &mut String,
    client_id: &mut String,
    client_secret: &mut crate::credentials::SecretString,
    refresh_token: &mut crate::credentials::SecretString,
    persist: impl FnOnce(&crate::oauth_refresh::OAuthRefreshConfig) -> Result<(), String>,
) -> Result<(), String> {
    let config = crate::oauth_refresh::OAuthRefreshConfig {
        token_endpoint: endpoint.clone(),
        client_id: client_id.clone(),
        client_secret: client_secret.clone(),
        refresh_token: refresh_token.clone(),
    };
    persist(&config)?;
    endpoint.clear();
    client_id.clear();
    *client_secret = crate::credentials::SecretString::default();
    *refresh_token = crate::credentials::SecretString::default();
    Ok(())
}

fn manual_oauth_refresh_marker(form: &crate::migration_plan::Form, source: bool) -> String {
    let (host, user, auth, refresh_id, access_token) = if source {
        (
            &form.profile.source_host,
            &form.profile.source_user,
            &form.profile.source_auth,
            &form.profile.source_oauth_refresh_credential_id,
            form.source_password.as_str(),
        )
    } else {
        (
            &form.profile.destination_host,
            &form.profile.destination_user,
            &form.profile.destination_auth,
            &form.profile.destination_oauth_refresh_credential_id,
            form.destination_password.as_str(),
        )
    };
    let mut digest = Sha256::new();
    for value in [
        host.as_str(),
        user.as_str(),
        auth.as_str(),
        refresh_id.as_str(),
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    digest.update((access_token.len() as u64).to_be_bytes());
    digest.update(access_token.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{manual_oauth_refresh_marker, store_oauth_refresh_editor_values};
    use crate::migration_plan::Form;

    #[test]
    fn manual_refresh_marker_is_side_scoped_and_never_contains_token_material() {
        let mut form = Form::default();
        form.profile.source_host = "imap.source.example".into();
        form.profile.source_user = "source@example.test".into();
        form.profile.source_auth = "oauth2".into();
        form.profile.source_oauth_refresh_credential_id = "source-refresh".into();
        form.source_password = crate::credentials::SecretString::from("DO_NOT_EXPOSE");
        let initial = manual_oauth_refresh_marker(&form, true);
        assert!(!initial.contains("DO_NOT_EXPOSE"));
        form.profile.destination_host = "other.example".into();
        assert_eq!(initial, manual_oauth_refresh_marker(&form, true));
        form.profile.source_host = "changed.example".into();
        assert_ne!(initial, manual_oauth_refresh_marker(&form, true));
    }

    #[test]
    fn failed_oauth_config_persistence_preserves_editor_values() {
        let mut endpoint = "https://issuer.example/token".to_owned();
        let mut client_id = "registered-client".to_owned();
        let mut client_secret = crate::credentials::SecretString::from("client-secret");
        let mut refresh_token = crate::credentials::SecretString::from("refresh-token");

        let result = store_oauth_refresh_editor_values(
            &mut endpoint,
            &mut client_id,
            &mut client_secret,
            &mut refresh_token,
            |_| Err("keyring unavailable".into()),
        );

        assert_eq!(result.unwrap_err(), "keyring unavailable");
        assert_eq!(endpoint, "https://issuer.example/token");
        assert_eq!(client_id, "registered-client");
        assert_eq!(client_secret.as_str(), "client-secret");
        assert_eq!(refresh_token.as_str(), "refresh-token");
    }

    #[test]
    fn successful_oauth_config_persistence_clears_editor_values() {
        let mut endpoint = "https://issuer.example/token".to_owned();
        let mut client_id = "registered-client".to_owned();
        let mut client_secret = crate::credentials::SecretString::from("client-secret");
        let mut refresh_token = crate::credentials::SecretString::from("refresh-token");

        store_oauth_refresh_editor_values(
            &mut endpoint,
            &mut client_id,
            &mut client_secret,
            &mut refresh_token,
            |config| {
                assert_eq!(config.client_secret.as_str(), "client-secret");
                assert_eq!(config.refresh_token.as_str(), "refresh-token");
                Ok(())
            },
        )
        .unwrap();

        assert!(endpoint.is_empty());
        assert!(client_id.is_empty());
        assert!(client_secret.is_empty());
        assert!(refresh_token.is_empty());
    }
}
