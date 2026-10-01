use crate::{App, StatusSeverity, auth_method_is_oauth, credentials::SecretString};
use eframe::egui::{self, Color32, RichText};
use sha2::{Digest, Sha256};
use std::sync::{Arc, atomic::AtomicBool, mpsc};

use super::app_state::{CredentialDeleteTarget, ManualOAuthRefreshResult};

use super::ThemeColors;

pub(crate) fn password_reveal_allowed(editable: bool, requested: bool) -> bool {
    editable && requested
}

pub(crate) fn password_visibility_id(title: &str) -> egui::Id {
    egui::Id::new(("password_visibility", title))
}

fn authorization_missing(
    local_dovecot: bool,
    saved_credential: bool,
    has_session_credential: bool,
) -> bool {
    !local_dovecot && !saved_credential && !has_session_credential
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
            Some(language.message("ui.is-required").replace("{}", label))
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
            ui.label(RichText::new(message).color(danger).size(12.0));
        }
    };
    crate::ui::card(ui, |ui| {
        ui.label(RichText::new(title).size(16.0).strong().color(color));
        ui.label(
            RichText::new(if title.to_lowercase().contains("dovecot") {
                language.message("ui.local-dovecot-account")
            } else {
                language.message("ui.imap-connection")
            })
            .size(12.0)
            .color(ui.visuals().weak_text_color()),
        );
        crate::ui::form_row(ui, language.message("ui.server"), |ui| {
            ui.add_enabled(
                editable,
                egui::TextEdit::singleline(host).desired_width(f32::INFINITY),
            )
        });
        inline_error(ui, language.message("ui.server"), host, true);
        crate::ui::form_row(ui, language.message("ui.user"), |ui| {
            ui.add_enabled(
                editable,
                egui::TextEdit::singleline(user).desired_width(f32::INFINITY),
            )
        });
        inline_error(ui, language.message("ui.user"), user, true);
        let local_dovecot = title.to_lowercase().contains("dovecot");
        let authorization_missing =
            authorization_missing(local_dovecot, saved_credential, !password.is_empty());
        let authorization_heading = if authorization_missing {
            RichText::new(format!(
                "⚠ {}",
                language.message("ui.account-authorization")
            ))
            .strong()
            .color(danger)
        } else {
            RichText::new(language.message("ui.account-authorization"))
        };
        egui::CollapsingHeader::new(authorization_heading)
            .id_salt(("account_authorization", title))
            .default_open(authorization_missing)
            .open(authorization_missing.then_some(true))
            .show(ui, |ui| {
        crate::ui::form_row(ui, language.message("ui.authentication"), |ui| {
            egui::ComboBox::from_id_salt(("auth_method", title))
                .selected_text(if auth_method_is_oauth(auth_method) {
                    "OAuth 2.0 / XOAUTH2"
                } else {
                    language.message("ui.password")
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(auth_method, "password".into(), language.message("ui.password"));
                    ui.selectable_value(auth_method, "oauth2".into(), "OAuth 2.0 / XOAUTH2");
                });
        });
        if auth_method_is_oauth(auth_method) {
            ui.collapsing(language.message("ui.oauth-setup-guidance"), |ui| {
                ui.label(
                    RichText::new(
                        language.message("ui.use-a-provider-issued-access-token-with-imap-scope-tokens-stay-in-this-sess-7703cd486b"),
                    )
                    .size(12.0)
                    .color(ui.visuals().weak_text_color()),
                );
            });
        }
        let secret_label = if auth_method_is_oauth(auth_method) {
            language.message("ui.access-token")
        } else {
            language.message("ui.password")
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
                authorization_missing,
            );
        }
            });
    });
}

impl App {
    pub(crate) fn keyring_dialog(&mut self, ctx: &egui::Context) {
        if !self.keyring_open {
            return;
        }
        let mut open = self.keyring_open;
        egui::Window::new(self.language.message("ui.os-keyring-credentials"))
            .open(&mut open)
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                    self.language.message("ui.keyring-ids-are-non-secret-references-saved-in-the-profile-passwords-and-oa-ec6c7dd669"),
                    )
                    .color(self.theme_colors().text_secondary),
                );
                ui.add_space(8.0);
                let editable = !self.running()
                    && self.manual_oauth_refresh_receiver.is_none()
                    && self.oauth_authorization_receiver.is_none();
                ui.add_enabled_ui(editable, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(self.language.message("ui.source-id"));
                        ui.text_edit_singleline(&mut self.form.profile.source_credential_id);
                    });
                    ui.horizontal(|ui| {
                        ui.label(self.language.message("ui.destination-id"));
                        ui.text_edit_singleline(&mut self.form.profile.destination_credential_id);
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let source_label = if auth_method_is_oauth(&self.form.profile.source_auth) {
                            self.language.message("ui.store-source-token")
                        } else {
                            self.language.message("ui.store-source-password")
                        };
                        if ui.button(source_label).clicked() {
                            match self.form.store_keyring_password(true) {
                                Ok(()) => self.set_status(self.language.message("ui.source-credential-stored-in-os-keyring"), StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                        if ui.button(self.language.message("ui.load-source")).clicked() {
                            match self.form.load_keyring_password(true) {
                                Ok(()) => self.set_status(self.language.message("ui.source-credential-loaded"), StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                        if ui.button(self.language.message("ui.delete-source")).clicked() {
                            self.credential_delete_confirmation =
                                Some(CredentialDeleteTarget::Password { source: true });
                        }
                    });
                    ui.horizontal(|ui| {
                        let destination_label = if auth_method_is_oauth(&self.form.profile.destination_auth) {
                            self.language.message("ui.store-destination-token")
                        } else {
                            self.language.message("ui.store-destination-password")
                        };
                        if ui.button(destination_label).clicked() {
                            match self.form.store_keyring_password(false) {
                                Ok(()) => self.set_status(self.language.message("ui.destination-credential-stored-in-os-keyring"), StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                        if ui.button(self.language.message("ui.load-destination")).clicked() {
                            match self.form.load_keyring_password(false) {
                                Ok(()) => self.set_status(self.language.message("ui.destination-credential-loaded"), StatusSeverity::Success),
                                Err(error) => self.set_status(error, StatusSeverity::Error),
                            }
                        }
                        if ui.button(self.language.message("ui.delete-destination")).clicked() {
                            self.credential_delete_confirmation =
                                Some(CredentialDeleteTarget::Password { source: false });
                        }
                    });
                });
                if !editable {
                    ui.label(RichText::new(self.language.message("ui.credential-settings-are-locked-while-a-migration-is-running-or-oauth-refres-57304003d5")).color(self.theme_colors().text_secondary));
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        self.language.message("ui.this-is-password-storage-not-oauth-modern-auth-do-not-use-it-as-a-substitut-6c48271e27"),
                    )
                    .size(12.0)
                    .color(self.theme_colors().danger),
                );
                ui.separator();
                self.oauth_authorization_section(ui, editable, ctx);
                ui.separator();
                self.oauth_refresh_section(ui, editable);
            });
        self.keyring_open = open;
    }

    fn oauth_authorization_section(
        &mut self,
        ui: &mut egui::Ui,
        editable: bool,
        ctx: &egui::Context,
    ) {
        ui.heading(self.language.message("ui.connect-provider-account"));
        ui.label(
            RichText::new(self.language.message("ui.oauth-app-registration-required"))
                .size(12.0)
                .color(self.theme_colors().text_secondary),
        );
        let mut start = false;
        let pending = self.oauth_authorization_receiver.is_some();
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                ui.label(self.language.message("ui.connect-account-side"));
                ui.selectable_value(
                    &mut self.oauth_authorization_source,
                    true,
                    self.language.message("ui.source"),
                );
                ui.selectable_value(
                    &mut self.oauth_authorization_source,
                    false,
                    self.language.message("ui.destination"),
                );
            });
            ui.horizontal(|ui| {
                ui.label(self.language.message("ui.provider"));
                egui::ComboBox::from_id_salt("oauth_authorization_provider")
                    .selected_text(match self.oauth_authorization_provider.as_str() {
                        "microsoft" => "Microsoft 365",
                        _ => "Google Workspace",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.oauth_authorization_provider,
                            "google".into(),
                            "Google Workspace",
                        );
                        ui.selectable_value(
                            &mut self.oauth_authorization_provider,
                            "microsoft".into(),
                            "Microsoft 365",
                        );
                    });
            });
            if self.oauth_authorization_provider == "microsoft" {
                ui.horizontal(|ui| {
                    ui.label(self.language.message("ui.microsoft-tenant"));
                    ui.text_edit_singleline(&mut self.oauth_authorization_tenant);
                });
            }
            ui.horizontal(|ui| {
                ui.label(self.language.message("ui.oauth-client-id"));
                ui.text_edit_singleline(&mut self.oauth_authorization_client_id);
            });
            ui.horizontal(|ui| {
                ui.label(self.language.message("ui.oauth-client-secret-optional"));
                ui.add(
                    egui::TextEdit::singleline(
                        self.oauth_authorization_client_secret.as_mut_string(),
                    )
                    .password(true),
                );
            });
            ui.horizontal(|ui| {
                ui.label(self.language.message("ui.sign-in-hint-optional"));
                ui.text_edit_singleline(&mut self.oauth_authorization_login_hint);
            });
            let keyring_id = if self.oauth_authorization_source {
                &mut self.form.profile.source_oauth_refresh_credential_id
            } else {
                &mut self.form.profile.destination_oauth_refresh_credential_id
            };
            ui.horizontal(|ui| {
                ui.label(self.language.message("ui.os-keyring-id"));
                ui.text_edit_singleline(keyring_id);
            });
            let provider_label = if self.oauth_authorization_provider == "microsoft" {
                "Microsoft 365"
            } else {
                "Google Workspace"
            };
            start = ui
                .add_enabled(
                    !pending,
                    egui::Button::new(
                        self.language
                            .message("ui.connect-account-in-browser")
                            .replace("{}", provider_label),
                    ),
                )
                .clicked();
        });
        if start {
            self.start_oauth_authorization(ctx);
        }
        if pending {
            let stage = self.oauth_authorization_stage;
            let waiting_for_browser =
                stage == Some(crate::ui::app_state::OAuthAuthorizationStage::WaitingForBrowser);
            ui.horizontal(|ui| {
                ui.label(self.language.message(match stage {
                    Some(crate::ui::app_state::OAuthAuthorizationStage::WaitingForBrowser) => {
                        "ui.waiting-for-provider-browser-authorization"
                    }
                    Some(crate::ui::app_state::OAuthAuthorizationStage::ExchangingCode) => {
                        "ui.oauth-code-received-completing-authorization"
                    }
                    Some(crate::ui::app_state::OAuthAuthorizationStage::TestingRefresh) => {
                        "ui.testing-oauth-token-refresh"
                    }
                    Some(crate::ui::app_state::OAuthAuthorizationStage::VerifyingImap) => {
                        "ui.verifying-imap-authentication"
                    }
                    None => "ui.waiting-for-provider-browser-authorization",
                }));
                if waiting_for_browser
                    && ui
                        .button(self.language.message("ui.cancel-waiting"))
                        .clicked()
                    && let Some(cancel) = &self.oauth_authorization_cancel
                {
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            });
            if waiting_for_browser {
                ui.label(
                    self.language
                        .message("ui.register-this-loopback-redirect-uri"),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(&self.oauth_authorization_redirect_uri).monospace(),
                    )
                    .selectable(true),
                );
            }
        }
        if let Some(result) = self.oauth_authorization_result.as_ref() {
            ui.separator();
            ui.heading(self.language.message("ui.last-oauth-validation"));
            let current_tenant = if self.oauth_authorization_provider == "microsoft" {
                self.oauth_authorization_tenant.as_str()
            } else {
                ""
            };
            let stale = result.marker
                != oauth_authorization_marker(
                    &self.form,
                    result.source,
                    &self.oauth_authorization_provider,
                    current_tenant,
                    &self.oauth_authorization_client_id,
                );
            if stale {
                ui.label(
                    RichText::new(
                        self.language
                            .message("ui.oauth-validation-settings-changed"),
                    )
                    .color(self.theme_colors().warning),
                );
            }
            ui.label(format!(
                "{} · {}",
                if result.source {
                    self.language.message("ui.source")
                } else {
                    self.language.message("ui.destination")
                },
                result.provider
            ));
            ui.label(format!(
                "{}: {}",
                self.language.message("ui.tenant-setting"),
                if result.tenant.is_empty() {
                    self.language.message("ui.not-applicable")
                } else {
                    &result.tenant
                }
            ));
            ui.label(format!(
                "{} {}",
                if result.stored { "✓" } else { "○" },
                self.language.message("ui.oauth-authorization-stored")
            ));
            ui.label(format!(
                "{} {}",
                if result.refresh_tested { "✓" } else { "○" },
                self.language.message("ui.oauth-token-refresh-tested")
            ));
            ui.label(format!(
                "{} {} · {}",
                if result.imap_authenticated {
                    "✓"
                } else {
                    "○"
                },
                self.language.message("ui.imap-authentication-verified-for"),
                result.mailbox
            ));
            ui.label(format!(
                "{}: {} ({})",
                self.language.message("ui.last-checked"),
                result.checked_at,
                self.language.message("ui.this-session-only")
            ));
            if result.stored {
                ui.label(self.language.message("ui.save-oauth-settings-in-profile"));
            }
            if let Some(detail) = &result.detail {
                ui.label(RichText::new(detail).color(self.theme_colors().warning));
            }
        }
    }

    fn start_oauth_authorization(&mut self, ctx: &egui::Context) {
        let source = self.oauth_authorization_source;
        let provider = self.oauth_authorization_provider.clone();
        let tenant = if provider == "microsoft" {
            self.oauth_authorization_tenant.trim().to_owned()
        } else {
            String::new()
        };
        let client_id = self.oauth_authorization_client_id.trim().to_owned();
        let keyring_id = if source {
            self.form.profile.source_oauth_refresh_credential_id.trim()
        } else {
            self.form
                .profile
                .destination_oauth_refresh_credential_id
                .trim()
        }
        .to_owned();
        if let Err(error) = crate::oauth_authorize::validate_keyring_id(&keyring_id) {
            self.set_status(error, StatusSeverity::Error);
            return;
        }
        let (host, port, user, tls, ca_bundle, pin) = if source {
            (
                self.form.profile.source_host.clone(),
                self.form.profile.source_port.clone(),
                self.form.profile.source_user.clone(),
                self.form.profile.source_tls.clone(),
                self.form.profile.source_ca_bundle.clone(),
                self.form.profile.source_certificate_pin_sha256.clone(),
            )
        } else {
            (
                self.form.profile.destination_host.clone(),
                self.form.profile.destination_port.clone(),
                self.form.profile.destination_user.clone(),
                self.form.profile.destination_tls.clone(),
                self.form.profile.destination_ca_bundle.clone(),
                self.form.profile.destination_certificate_pin_sha256.clone(),
            )
        };
        if user.trim().is_empty() {
            self.set_status(
                self.language
                    .message("ui.configure-mailbox-user-before-oauth-authorization"),
                StatusSeverity::Warning,
            );
            return;
        }
        let endpoint = match crate::imap_probe::endpoint_for_probe(&host, &port) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.set_status(error, StatusSeverity::Error);
                return;
            }
        };
        let overrides = crate::oauth_authorize::ProviderOverrides {
            tenant: (provider == "microsoft").then_some(tenant.clone()),
            ..crate::oauth_authorize::ProviderOverrides::default()
        };
        let client_secret = (!self.oauth_authorization_client_secret.is_empty())
            .then(|| self.oauth_authorization_client_secret.clone());
        let (authorization, url) = match crate::oauth_authorize::PendingAuthorization::begin(
            &provider,
            overrides,
            &client_id,
            client_secret,
            (!self.oauth_authorization_login_hint.trim().is_empty())
                .then_some(self.oauth_authorization_login_hint.trim()),
        ) {
            Ok(value) => value,
            Err(error) => {
                self.set_status(error, StatusSeverity::Error);
                return;
            }
        };
        let redirect_uri = authorization.redirect_uri().to_owned();
        let marker = oauth_authorization_marker(&self.form, source, &provider, &tenant, &client_id);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let mut form = self.form.clone();
        if source {
            form.profile.source_auth = "oauth2".into();
            form.profile.source_oauth_refresh_credential_id = keyring_id.clone();
        } else {
            form.profile.destination_auth = "oauth2".into();
            form.profile.destination_oauth_refresh_credential_id = keyring_id.clone();
        }
        let mailbox = user.clone();
        let provider_label = if provider == "microsoft" {
            "Microsoft 365"
        } else {
            "Google Workspace"
        }
        .to_owned();
        let result_tenant = tenant.clone();
        let (sender, receiver) = mpsc::channel();
        let spawn = std::thread::Builder::new()
            .name("mailswiftsync-oauth-authorization-ui".into())
            .spawn(move || {
                let _ = sender.send(crate::ui::app_state::OAuthAuthorizationMessage::Progress(
                    crate::ui::app_state::OAuthAuthorizationStage::WaitingForBrowser,
                ));
                let mut result = crate::ui::app_state::OAuthAuthorizationResult {
                    source,
                    marker,
                    provider: provider_label,
                    tenant: result_tenant,
                    mailbox: mailbox.clone(),
                    checked_at: String::new(),
                    stored: false,
                    refresh_tested: false,
                    imap_authenticated: false,
                    detail: None,
                };
                match authorization.complete_and_store(&keyring_id, &worker_cancel, |progress| {
                    let stage = match progress {
                        crate::oauth_authorize::AuthorizationProgress::CodeReceived => {
                            crate::ui::app_state::OAuthAuthorizationStage::ExchangingCode
                        }
                        crate::oauth_authorize::AuthorizationProgress::ConfigurationStored => {
                            crate::ui::app_state::OAuthAuthorizationStage::TestingRefresh
                        }
                    };
                    let _ = sender.send(
                        crate::ui::app_state::OAuthAuthorizationMessage::Progress(stage),
                    );
                }) {
                    Ok(()) => {
                        result.stored = true;
                        let _ = sender.send(
                            crate::ui::app_state::OAuthAuthorizationMessage::Progress(
                                crate::ui::app_state::OAuthAuthorizationStage::TestingRefresh,
                            ),
                        );
                        let refresh = form.refresh_oauth_access_token(source);
                        match refresh {
                            Ok(crate::migration_plan::OAuthRefreshOutcome::Refreshed { .. }) => {
                                result.refresh_tested = true;
                                let _ = sender.send(
                                    crate::ui::app_state::OAuthAuthorizationMessage::Progress(
                                        crate::ui::app_state::OAuthAuthorizationStage::VerifyingImap,
                                    ),
                                );
                                let credential = if source {
                                    form.source_password.as_str()
                                } else {
                                    form.destination_password.as_str()
                                };
                                match crate::imap_probe::probe_tls_authentication_with_transport(
                                    &endpoint,
                                    &user,
                                    credential,
                                    "oauth2",
                                    &tls,
                                    &ca_bundle,
                                    &pin,
                                ) {
                                    Ok(()) => result.imap_authenticated = true,
                                    Err(error) => {
                                        result.detail = Some(format!(
                                            "OAuth authorization and token refresh succeeded, but IMAP authentication failed: {error}"
                                        ));
                                    }
                                }
                            }
                            Ok(crate::migration_plan::OAuthRefreshOutcome::NotConfigured) => {
                                result.detail = Some(
                                    "OAuth configuration was stored, but the refresh test found no keyring configuration.".into(),
                                );
                            }
                            Err(error) => {
                                result.detail = Some(format!(
                                    "OAuth authorization was stored, but token refresh failed: {error}"
                                ));
                            }
                        }
                    }
                    Err(error) => result.detail = Some(error),
                }
                result.checked_at = chrono::Local::now()
                    .format("%Y-%m-%d %H:%M:%S %Z")
                    .to_string();
                let _ = sender.send(crate::ui::app_state::OAuthAuthorizationMessage::Finished(
                    result,
                ));
            });
        match spawn {
            Ok(_) => {
                self.oauth_authorization_receiver = Some(receiver);
                self.oauth_authorization_cancel = Some(cancel);
                self.oauth_authorization_stage =
                    Some(crate::ui::app_state::OAuthAuthorizationStage::WaitingForBrowser);
                self.oauth_authorization_redirect_uri = redirect_uri;
                self.oauth_authorization_result = None;
                self.oauth_authorization_client_secret = SecretString::default();
                ctx.open_url(egui::OpenUrl::new_tab(url));
                self.set_status(
                    self.language
                        .message("ui.oauth-browser-opened-waiting-for-authorization"),
                    StatusSeverity::Info,
                );
            }
            Err(error) => self.set_status(
                format!("Could not start OAuth authorization worker: {error}"),
                StatusSeverity::Error,
            ),
        }
    }

    pub(crate) fn complete_oauth_authorization(
        &mut self,
        result: crate::ui::app_state::OAuthAuthorizationResult,
    ) {
        let tenant = if self.oauth_authorization_provider == "microsoft" {
            self.oauth_authorization_tenant.as_str()
        } else {
            ""
        };
        let current_marker = oauth_authorization_marker(
            &self.form,
            result.source,
            &self.oauth_authorization_provider,
            tenant,
            &self.oauth_authorization_client_id,
        );
        if result.marker == current_marker && result.stored {
            if result.source {
                self.form.profile.source_auth = "oauth2".into();
                self.form.source_password = SecretString::default();
            } else {
                self.form.profile.destination_auth = "oauth2".into();
                self.form.destination_password = SecretString::default();
            }
        }
        let successful = result.refresh_tested && result.imap_authenticated;
        self.set_status(
            result.detail.clone().unwrap_or_else(|| {
                if successful {
                    self.language
                        .message("ui.oauth-authorized-refreshed-and-imap-verified")
                        .to_owned()
                } else {
                    self.language
                        .message("ui.oauth-authorization-stored-validation-incomplete")
                        .to_owned()
                }
            }),
            if successful {
                StatusSeverity::Success
            } else if result.detail.is_some() {
                StatusSeverity::Warning
            } else {
                StatusSeverity::Info
            },
        );
        self.oauth_authorization_result = Some(result);
    }

    /// Optional automatic-refresh configuration: an operator who has
    /// registered their own OAuth application with the provider and holds a
    /// refresh token can store it here so `reload_configured_keyring_credentials`
    /// exchanges it for a fresh access token before each live launch instead
    /// of requiring a freshly copied token every time.
    fn oauth_refresh_section(&mut self, ui: &mut egui::Ui, editable: bool) {
        ui.label(
            RichText::new(self.language.message("ui.automatic-oauth-refresh-optional")).strong(),
        );
        ui.label(
            RichText::new(
                self.language.message("ui.requires-an-oauth-application-you-have-registered-with-the-provider-run-mai-0289eda4e9"),
            )
            .size(12.0)
            .color(self.theme_colors().text_secondary),
        );
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                ui.label(self.language.message("ui.source-refresh-id"));
                ui.text_edit_singleline(&mut self.form.profile.source_oauth_refresh_credential_id);
            });
            ui.horizontal(|ui| {
                ui.label(self.language.message("ui.destination-refresh-id"));
                ui.text_edit_singleline(
                    &mut self.form.profile.destination_oauth_refresh_credential_id,
                );
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(self.language.message("ui.token-endpoint"));
                ui.text_edit_singleline(&mut self.oauth_refresh_editor_endpoint);
            });
            ui.horizontal(|ui| {
                ui.label(self.language.message("ui.client-id"));
                ui.text_edit_singleline(&mut self.oauth_refresh_editor_client_id);
            });
            ui.horizontal(|ui| {
                ui.label(self.language.message("ui.client-secret-if-required"));
                ui.add(
                    egui::TextEdit::singleline(
                        self.oauth_refresh_editor_client_secret.as_mut_string(),
                    )
                    .password(true),
                );
            });
            ui.horizontal(|ui| {
                ui.label(self.language.message("ui.refresh-token"));
                ui.add(
                    egui::TextEdit::singleline(
                        self.oauth_refresh_editor_refresh_token.as_mut_string(),
                    )
                    .password(true),
                );
            });
            ui.label(
                RichText::new(
                    self.language.message("ui.the-fields-above-are-entered-once-per-store-they-are-cleared-from-memory-im-9238dc1433"),
                )
                .size(12.0)
                .color(self.theme_colors().text_secondary),
            );
            ui.horizontal(|ui| {
                if ui.button(self.language.message("ui.store-for-source")).clicked() {
                    self.store_oauth_refresh_editor(true);
                }
                if ui.button(self.language.message("ui.store-for-destination")).clicked() {
                    self.store_oauth_refresh_editor(false);
                }
            });
            ui.horizontal(|ui| {
                let refresh_pending = self.manual_oauth_refresh_receiver.is_some();
                if ui
                    .add_enabled(
                        !refresh_pending,
                        egui::Button::new(if refresh_pending {
                            self.language.message("ui.refreshing-oauth-token")
                        } else {
                            self.language.message("ui.refresh-source-now")
                        }),
                    )
                    .clicked()
                {
                    self.run_manual_oauth_refresh(true);
                }
                if ui
                    .add_enabled(
                        !refresh_pending,
                        egui::Button::new(self.language.message("ui.refresh-destination-now")),
                    )
                    .clicked()
                {
                    self.run_manual_oauth_refresh(false);
                }
            });
            ui.horizontal(|ui| {
                if ui.button(self.language.message("ui.delete-source-refresh-config")).clicked() {
                    self.credential_delete_confirmation =
                        Some(CredentialDeleteTarget::OAuthRefresh { source: true });
                }
                if ui.button(self.language.message("ui.delete-destination-refresh-config")).clicked() {
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
                self.language.message("ui.saved-password-access-token"),
                if source {
                    self.form.profile.source_credential_id.as_str()
                } else {
                    self.form.profile.destination_credential_id.as_str()
                }
                .to_owned(),
            ),
            CredentialDeleteTarget::OAuthRefresh { source } => (
                source,
                self.language
                    .message("ui.automatic-oauth-refresh-configuration"),
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
                    RichText::new(self.language.message("ui.delete-saved-credential-a75582b4"))
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
                    self.language.message("ui.the-profile-reference-and-any-credential-already-loaded-in-this-session-are-not-changed")
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let cancel = ui.button(self.language.message("ui.cancel"));
                    if !self.credential_delete_focus_requested {
                        cancel.request_focus();
                        self.credential_delete_focus_requested = true;
                    }
                    if cancel.clicked() {
                        close = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(self.language.message("ui.delete-saved-credential-694a60a1"))
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

fn oauth_authorization_marker(
    form: &crate::migration_plan::Form,
    source: bool,
    provider: &str,
    tenant: &str,
    client_id: &str,
) -> String {
    let (host, port, user, tls, ca, pin, keyring_id) = if source {
        (
            &form.profile.source_host,
            &form.profile.source_port,
            &form.profile.source_user,
            &form.profile.source_tls,
            &form.profile.source_ca_bundle,
            &form.profile.source_certificate_pin_sha256,
            &form.profile.source_oauth_refresh_credential_id,
        )
    } else {
        (
            &form.profile.destination_host,
            &form.profile.destination_port,
            &form.profile.destination_user,
            &form.profile.destination_tls,
            &form.profile.destination_ca_bundle,
            &form.profile.destination_certificate_pin_sha256,
            &form.profile.destination_oauth_refresh_credential_id,
        )
    };
    let mut digest = Sha256::new();
    for value in [
        if source { "source" } else { "destination" },
        provider,
        tenant,
        client_id,
        host,
        port,
        user,
        tls,
        ca,
        pin,
        keyring_id,
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        authorization_missing, manual_oauth_refresh_marker, oauth_authorization_marker,
        store_oauth_refresh_editor_values,
    };
    use crate::migration_plan::Form;

    #[test]
    fn missing_account_authorization_expands_but_saved_or_local_auth_does_not() {
        assert!(authorization_missing(false, false, false));
        assert!(!authorization_missing(false, true, false));
        assert!(!authorization_missing(false, false, true));
        assert!(!authorization_missing(true, false, false));
    }

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
    fn oauth_authorization_marker_binds_provider_and_target_identity() {
        let form = Form::default();
        let initial = oauth_authorization_marker(&form, false, "microsoft", "tenant-a", "client-a");
        assert_eq!(
            initial,
            oauth_authorization_marker(&form, false, "microsoft", "tenant-a", "client-a")
        );
        assert_ne!(
            initial,
            oauth_authorization_marker(&form, true, "microsoft", "tenant-a", "client-a")
        );
        assert_ne!(
            initial,
            oauth_authorization_marker(&form, false, "microsoft", "tenant-b", "client-a")
        );
        assert_ne!(
            initial,
            oauth_authorization_marker(&form, false, "microsoft", "tenant-a", "client-b")
        );
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
