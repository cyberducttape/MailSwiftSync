//! OS keyring CRUD off the UI thread.
//!
//! Credential stores are external OS services: Secret Service may need D-Bus
//! activation or an unlock prompt, Keychain may block on user consent, and
//! enterprise Windows credential managers are not guaranteed to answer
//! promptly. Every keyring call from the account screen therefore runs on a
//! worker and reports back through a single-result channel drained by `poll`,
//! the same model the OAuth refresh and authorization flows use.

use crate::credentials::SecretString;
use crate::oauth_refresh::OAuthRefreshConfig;
use crate::ui::app_state::CredentialDeleteTarget;
use crate::{App, StatusSeverity};
use std::sync::mpsc;
use std::thread;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyringOperation {
    StorePassword { source: bool },
    LoadPassword { source: bool },
    Delete(CredentialDeleteTarget),
    StoreOAuthRefresh { source: bool },
}

pub(crate) struct KeyringOperationResult {
    pub(crate) operation: KeyringOperation,
    /// Keyring ID the worker used. A loaded secret is applied only while the
    /// profile still names the same entry.
    pub(crate) credential_id: String,
    pub(crate) outcome: Result<Option<SecretString>, String>,
}

fn keyring_reference(form: &crate::Form, operation: KeyringOperation) -> String {
    let profile = &form.profile;
    let (source, oauth) = match operation {
        KeyringOperation::StorePassword { source }
        | KeyringOperation::LoadPassword { source }
        | KeyringOperation::Delete(CredentialDeleteTarget::Password { source }) => (source, false),
        KeyringOperation::StoreOAuthRefresh { source }
        | KeyringOperation::Delete(CredentialDeleteTarget::OAuthRefresh { source }) => {
            (source, true)
        }
    };
    match (source, oauth) {
        (true, false) => &profile.source_credential_id,
        (false, false) => &profile.destination_credential_id,
        (true, true) => &profile.source_oauth_refresh_credential_id,
        (false, true) => &profile.destination_oauth_refresh_credential_id,
    }
    .trim()
    .to_owned()
}

fn run_keyring_operation(
    mut form: crate::Form,
    operation: KeyringOperation,
    oauth_config: Option<OAuthRefreshConfig>,
) -> Result<Option<SecretString>, String> {
    match operation {
        KeyringOperation::StorePassword { source } => {
            form.store_keyring_password(source).map(|()| None)
        }
        KeyringOperation::LoadPassword { source } => {
            form.load_keyring_password(source)?;
            Ok(Some(if source {
                form.source_password.clone()
            } else {
                form.destination_password.clone()
            }))
        }
        KeyringOperation::Delete(CredentialDeleteTarget::Password { source }) => {
            form.delete_keyring_password(source).map(|()| None)
        }
        KeyringOperation::Delete(CredentialDeleteTarget::OAuthRefresh { source }) => {
            form.delete_oauth_refresh_config(source).map(|()| None)
        }
        KeyringOperation::StoreOAuthRefresh { source } => {
            let config = oauth_config.ok_or("OAuth refresh configuration is missing.")?;
            form.store_oauth_refresh_config(source, &config)
                .map(|()| None)
        }
    }
}

impl App {
    pub(crate) fn keyring_operation_pending(&self) -> bool {
        self.keyring_operation_receiver.is_some()
    }

    /// Start one keyring operation on a worker. Only one runs at a time; the
    /// credential fields stay locked until its result has been applied.
    pub(crate) fn begin_keyring_operation(&mut self, operation: KeyringOperation) {
        if self.keyring_operation_pending() {
            self.set_status(
                self.language
                    .text("An OS keyring operation is already in progress."),
                StatusSeverity::Warning,
            );
            return;
        }
        let oauth_config = match operation {
            KeyringOperation::StoreOAuthRefresh { .. } => Some(OAuthRefreshConfig {
                token_endpoint: self.oauth_refresh_editor_endpoint.clone(),
                client_id: self.oauth_refresh_editor_client_id.clone(),
                client_secret: self.oauth_refresh_editor_client_secret.clone(),
                refresh_token: self.oauth_refresh_editor_refresh_token.clone(),
            }),
            _ => None,
        };
        let form = self.form.clone();
        let credential_id = keyring_reference(&form, operation);
        let (tx, rx) = mpsc::channel();
        self.keyring_operation_receiver = Some(rx);
        self.set_status(
            self.language.text("Waiting for the OS keyring…"),
            StatusSeverity::Info,
        );
        thread::spawn(move || {
            let outcome = run_keyring_operation(form, operation, oauth_config);
            let _ = tx.send(KeyringOperationResult {
                operation,
                credential_id,
                outcome,
            });
        });
    }

    pub(crate) fn complete_keyring_operation(&mut self, result: KeyringOperationResult) {
        let KeyringOperationResult {
            operation,
            credential_id,
            outcome,
        } = result;
        let loaded = match outcome {
            Ok(loaded) => loaded,
            Err(error) => {
                self.set_status(error, StatusSeverity::Error);
                return;
            }
        };
        let message = match operation {
            KeyringOperation::StorePassword { source: true } => self
                .language
                .message("ui.source-credential-stored-in-os-keyring")
                .to_string(),
            KeyringOperation::StorePassword { source: false } => self
                .language
                .message("ui.destination-credential-stored-in-os-keyring")
                .to_string(),
            KeyringOperation::LoadPassword { source } => {
                if keyring_reference(&self.form, operation) != credential_id {
                    self.set_status(
                        self.language.text(
                            "The keyring ID changed while the credential was loading; it was not applied.",
                        ),
                        StatusSeverity::Warning,
                    );
                    return;
                }
                let password = loaded.unwrap_or_default();
                if source {
                    self.form.source_password = password;
                } else {
                    self.form.destination_password = password;
                }
                self.language
                    .message(if source {
                        "ui.source-credential-loaded"
                    } else {
                        "ui.destination-credential-loaded"
                    })
                    .to_string()
            }
            KeyringOperation::Delete(target) => self
                .language
                .text(match target {
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
                })
                .to_string(),
            KeyringOperation::StoreOAuthRefresh { source } => {
                // The editor holds secrets only until they are persisted;
                // a failed store keeps them so the operator can retry.
                self.oauth_refresh_editor_endpoint.clear();
                self.oauth_refresh_editor_client_id.clear();
                self.oauth_refresh_editor_client_secret = SecretString::default();
                self.oauth_refresh_editor_refresh_token = SecretString::default();
                self.language
                    .text("{} OAuth refresh configuration stored in OS keyring")
                    .replace(
                        "{}",
                        self.language
                            .text(if source { "Source" } else { "Destination" }),
                    )
            }
        };
        self.set_status(message, StatusSeverity::Success);
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyringOperation, KeyringOperationResult};
    use crate::App;
    use crate::credentials::SecretString;

    fn with_app(test: impl FnOnce(&mut App)) {
        let state_path = std::env::temp_dir().join(format!(
            "mailswiftsync-keyring-ops-{}.db",
            uuid::Uuid::new_v4()
        ));
        let mut app = App::from_state_path(Some(&state_path));
        test(&mut app);
        drop(app);
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(state_path.with_extension("lock"));
    }

    fn fill_oauth_editor(app: &mut App) {
        app.oauth_refresh_editor_endpoint = "https://issuer.example/token".into();
        app.oauth_refresh_editor_client_id = "registered-client".into();
        app.oauth_refresh_editor_client_secret = SecretString::from("client-secret");
        app.oauth_refresh_editor_refresh_token = SecretString::from("refresh-token");
    }

    #[test]
    fn failed_oauth_config_persistence_preserves_editor_values() {
        with_app(|app| {
            fill_oauth_editor(app);
            app.complete_keyring_operation(KeyringOperationResult {
                operation: KeyringOperation::StoreOAuthRefresh { source: true },
                credential_id: "source-refresh".into(),
                outcome: Err("keyring unavailable".into()),
            });
            assert_eq!(app.status.text, "keyring unavailable");
            assert_eq!(
                app.oauth_refresh_editor_endpoint,
                "https://issuer.example/token"
            );
            assert_eq!(app.oauth_refresh_editor_client_id, "registered-client");
            assert_eq!(
                app.oauth_refresh_editor_client_secret.as_str(),
                "client-secret"
            );
            assert_eq!(
                app.oauth_refresh_editor_refresh_token.as_str(),
                "refresh-token"
            );
        });
    }

    #[test]
    fn successful_oauth_config_persistence_clears_editor_values() {
        with_app(|app| {
            fill_oauth_editor(app);
            app.complete_keyring_operation(KeyringOperationResult {
                operation: KeyringOperation::StoreOAuthRefresh { source: false },
                credential_id: "destination-refresh".into(),
                outcome: Ok(None),
            });
            assert!(app.oauth_refresh_editor_endpoint.is_empty());
            assert!(app.oauth_refresh_editor_client_id.is_empty());
            assert!(app.oauth_refresh_editor_client_secret.is_empty());
            assert!(app.oauth_refresh_editor_refresh_token.is_empty());
        });
    }

    #[test]
    fn loaded_secret_applies_only_to_the_requested_keyring_entry() {
        with_app(|app| {
            app.form.profile.source_credential_id = "source-entry".into();
            app.complete_keyring_operation(KeyringOperationResult {
                operation: KeyringOperation::LoadPassword { source: true },
                credential_id: "source-entry".into(),
                outcome: Ok(Some(SecretString::from("loaded-secret"))),
            });
            assert_eq!(app.form.source_password.as_str(), "loaded-secret");

            app.form.profile.destination_credential_id = "renamed-entry".into();
            app.complete_keyring_operation(KeyringOperationResult {
                operation: KeyringOperation::LoadPassword { source: false },
                credential_id: "original-entry".into(),
                outcome: Ok(Some(SecretString::from("stale-secret"))),
            });
            assert!(app.form.destination_password.is_empty());
            assert_eq!(app.status.severity, crate::StatusSeverity::Warning);
        });
    }

    #[test]
    fn keyring_operation_runs_off_the_ui_thread_and_is_tracked() {
        with_app(|app| {
            // An empty keyring ID fails inside the worker without touching
            // the OS credential store, which exercises the full channel path.
            app.form.profile.source_credential_id.clear();
            app.begin_keyring_operation(KeyringOperation::LoadPassword { source: true });
            assert!(app.keyring_operation_pending());
            assert!(app.background_work_pending());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while app.keyring_operation_pending() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
                app.poll();
            }
            assert!(!app.keyring_operation_pending());
            assert_eq!(app.status.severity, crate::StatusSeverity::Error);
            assert!(
                app.status.text.contains("keyring ID"),
                "{}",
                app.status.text
            );
        });
    }
}
