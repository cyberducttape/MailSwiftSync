//! OS-keyring credential references and OAuth refresh-token rotation.

use super::*;

pub(super) const MAX_KEYRING_CREDENTIAL_BYTES: usize = 64 * 1024;

/// The result of an attempted automatic OAuth refresh, distinguishing "this
/// side is not using OAuth, or has no refresh configuration" (a no-op) from
/// an actual successful exchange, so callers can report a meaningful status
/// message rather than a bare boolean.
pub(crate) enum OAuthRefreshOutcome {
    NotConfigured,
    Refreshed { expires_in: Option<u64> },
}

pub(super) const OAUTH_REFRESH_PERSIST_ATTEMPTS: usize = 3;

pub(super) const OAUTH_REFRESH_PERSIST_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Keyring writes are outside the provider transaction. Retry short-lived
/// backend/IPC failures before declaring that a provider-side rotation has
/// entered the catastrophic recovery state.
pub(super) fn persist_rotated_refresh_config_with_retry<F>(
    config: &crate::oauth_refresh::OAuthRefreshConfig,
    mut persist: F,
) -> Result<(), String>
where
    F: FnMut(&crate::oauth_refresh::OAuthRefreshConfig) -> Result<(), String>,
{
    let mut last_error = None;
    for attempt in 0..OAUTH_REFRESH_PERSIST_ATTEMPTS {
        match persist(config) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                if attempt + 1 < OAUTH_REFRESH_PERSIST_ATTEMPTS {
                    thread::sleep(OAUTH_REFRESH_PERSIST_RETRY_DELAY);
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "OAuth refresh configuration persistence failed".into()))
}

impl Form {
    pub(crate) fn keyring_entry(&self, source: bool) -> Result<Option<Entry>, String> {
        let id = if source {
            self.profile.source_credential_id.trim()
        } else {
            self.profile.destination_credential_id.trim()
        };
        if id.is_empty() {
            return Ok(None);
        }
        Entry::new(Self::KEYRING_SERVICE, id)
            .map(Some)
            .map_err(|error| format!("Could not open OS keyring entry `{id}`: {error}"))
    }

    pub(crate) fn store_keyring_password(&self, source: bool) -> Result<(), String> {
        let entry = self
            .keyring_entry(source)?
            .ok_or("Enter a keyring ID before storing a credential.")?;
        let password = if source {
            self.source_password.as_str()
        } else {
            self.destination_password.as_str()
        };
        if password.is_empty() {
            return Err("Enter a credential before storing it in the OS keyring.".into());
        }
        entry
            .set_password(password)
            .map_err(|error| format!("Could not store the credential in the OS keyring: {error}"))
    }

    pub(crate) fn load_keyring_password(&mut self, source: bool) -> Result<(), String> {
        let entry = self
            .keyring_entry(source)?
            .ok_or("Enter a keyring ID before loading a password.")?;
        let password = entry.get_password().map_err(|error| {
            format!("Could not load the credential from the OS keyring: {error}")
        })?;
        if password.len() > MAX_KEYRING_CREDENTIAL_BYTES {
            return Err(format!(
                "OS keyring credential exceeds the {MAX_KEYRING_CREDENTIAL_BYTES}-byte limit"
            ));
        }
        let password = SecretString::new(password);
        if source {
            self.source_password = password;
        } else {
            self.destination_password = password;
        }
        Ok(())
    }

    pub(crate) fn delete_keyring_password(&self, source: bool) -> Result<(), String> {
        let entry = self
            .keyring_entry(source)?
            .ok_or("Enter a keyring ID before deleting a password.")?;
        entry
            .delete_credential()
            .map_err(|error| format!("Could not delete the OS keyring credential: {error}"))
    }

    pub(crate) fn load_configured_keyring_credentials(&mut self) -> Result<(), String> {
        self.load_static_configured_keyring_credentials()?;
        // An automatic OAuth refresh reference is itself a credential source.
        // Refresh it during preflight as well as live admission so a profile
        // that has no ordinary credential ID can still obtain the access token
        // required for authenticated readiness checks.
        if auth_method_is_oauth(&self.profile.source_auth)
            && !self
                .profile
                .source_oauth_refresh_credential_id
                .trim()
                .is_empty()
        {
            self.refresh_oauth_access_token(true)?;
        }
        if auth_method_is_oauth(&self.profile.destination_auth)
            && !self
                .profile
                .destination_oauth_refresh_credential_id
                .trim()
                .is_empty()
        {
            self.refresh_oauth_access_token(false)?;
        }
        Ok(())
    }

    /// Load only static keyring credentials. Live batch admission uses this
    /// to validate the stable credential identity without creating an access
    /// token that may expire while another queued mailbox is running.
    pub(crate) fn load_static_configured_keyring_credentials(&mut self) -> Result<(), String> {
        if self.source_password.is_empty() && !self.profile.source_credential_id.trim().is_empty() {
            self.load_keyring_password(true)?;
        }
        if self.destination_password.is_empty()
            && !self.profile.destination_credential_id.trim().is_empty()
        {
            self.load_keyring_password(false)?;
        }
        Ok(())
    }

    /// Reload configured references immediately before live admission. The
    /// ordinary loader intentionally preserves a password typed into the
    /// current form; live promotion must instead use the current keyring
    /// value when a credential reference is authoritative. When automatic
    /// OAuth refresh is configured for a side, this also exchanges the
    /// stored refresh token for a fresh access token, so a batch queue or a
    /// resumed single mailbox does not launch with a token that expired
    /// while it waited.
    pub(crate) fn reload_configured_keyring_credentials(&mut self) -> Result<(), String> {
        self.load_static_configured_keyring_credentials()?;
        self.refresh_oauth_access_token(true)?;
        self.refresh_oauth_access_token(false)?;
        Ok(())
    }

    pub(super) fn oauth_refresh_keyring_entry(
        &self,
        source: bool,
    ) -> Result<Option<Entry>, String> {
        let id = if source {
            self.profile.source_oauth_refresh_credential_id.trim()
        } else {
            self.profile.destination_oauth_refresh_credential_id.trim()
        };
        if id.is_empty() {
            return Ok(None);
        }
        Entry::new(Self::OAUTH_REFRESH_KEYRING_SERVICE, id)
            .map(Some)
            .map_err(|error| format!("Could not open OS keyring entry `{id}`: {error}"))
    }

    /// Store an automatic-refresh configuration (token endpoint, registered
    /// client credentials, and refresh token) under the keyring ID already
    /// entered for this side. The ID itself is a non-secret profile field,
    /// exactly like the existing password credential ID.
    pub(crate) fn store_oauth_refresh_config(
        &self,
        source: bool,
        config: &crate::oauth_refresh::OAuthRefreshConfig,
    ) -> Result<(), String> {
        let entry = self
            .oauth_refresh_keyring_entry(source)?
            .ok_or("Enter an OAuth refresh keyring ID before storing a refresh configuration.")?;
        entry
            .set_password(&crate::oauth_refresh::encode_refresh_config(config))
            .map_err(|error| {
                format!(
                    "Could not store the OAuth refresh configuration in the OS keyring: {error}"
                )
            })?;
        if let Some(key) = self.oauth_refresh_recovery_key(source) {
            crate::oauth_refresh::clear_refresh_recovery(&key)?;
        }
        Ok(())
    }

    pub(crate) fn load_oauth_refresh_config(
        &self,
        source: bool,
    ) -> Result<Option<crate::oauth_refresh::OAuthRefreshConfig>, String> {
        let Some(entry) = self.oauth_refresh_keyring_entry(source)? else {
            return Ok(None);
        };
        let stored = Zeroizing::new(entry.get_password().map_err(|error| {
            format!("Could not load the OAuth refresh configuration from the OS keyring: {error}")
        })?);
        crate::oauth_refresh::decode_refresh_config(&stored).map(Some)
    }

    #[allow(dead_code)]
    pub(crate) fn delete_oauth_refresh_config(&self, source: bool) -> Result<(), String> {
        let entry = self
            .oauth_refresh_keyring_entry(source)?
            .ok_or("Enter an OAuth refresh keyring ID before deleting a refresh configuration.")?;
        entry.delete_credential().map_err(|error| {
            format!("Could not delete the OS keyring OAuth refresh configuration: {error}")
        })?;
        if let Some(key) = self.oauth_refresh_recovery_key(source) {
            crate::oauth_refresh::clear_refresh_recovery(&key)?;
        }
        Ok(())
    }

    pub(super) fn oauth_refresh_recovery_key(&self, source: bool) -> Option<String> {
        let id = if source {
            self.profile.source_oauth_refresh_credential_id.trim()
        } else {
            self.profile.destination_oauth_refresh_credential_id.trim()
        };
        (!id.is_empty()).then(|| format!("{}:{id}", if source { "source" } else { "destination" }))
    }

    /// Exchange a configured refresh token for a fresh access token and
    /// install it as the session credential for this side. Returns
    /// `Ok(OAuthRefreshOutcome::NotConfigured)` without any network activity
    /// when the side is not using OAuth or has no refresh configuration, so
    /// ordinary password-auth plans and operator-typed one-off tokens are
    /// entirely unaffected.
    pub(crate) fn refresh_oauth_access_token(
        &mut self,
        source: bool,
    ) -> Result<OAuthRefreshOutcome, String> {
        let auth_method = if source {
            &self.profile.source_auth
        } else {
            &self.profile.destination_auth
        };
        if !auth_method_is_oauth(auth_method) {
            return Ok(OAuthRefreshOutcome::NotConfigured);
        }
        let recovery_key = self.oauth_refresh_recovery_key(source);
        if let Some(recovery_key) = recovery_key.as_deref()
            && let Some(recovery) = crate::oauth_refresh::take_refresh_recovery(recovery_key)?
        {
            if let Err(error) =
                persist_rotated_refresh_config_with_retry(&recovery.config, |config| {
                    self.store_oauth_refresh_config(source, config)
                })
            {
                let recovery_error =
                    crate::oauth_refresh::put_refresh_recovery(recovery_key.to_owned(), recovery)
                        .err();
                return Err(format!(
                    "[oauth_refresh_rotation_persistence_pending] CRITICAL: OAuth refresh rotation succeeded but the rotated refresh token could not be persisted; no new token exchange was attempted. Retry keyring persistence before resuming the migration (persistence error: {error}{})",
                    recovery_error.map_or_else(String::new, |value| format!(
                        "; recovery retention error: {value}"
                    ))
                ));
            }
            if source {
                self.source_password = recovery.access_token;
            } else {
                self.destination_password = recovery.access_token;
            }
            return Ok(OAuthRefreshOutcome::Refreshed {
                expires_in: recovery.expires_in,
            });
        }
        let Some(config) = self.load_oauth_refresh_config(source)? else {
            return Ok(OAuthRefreshOutcome::NotConfigured);
        };
        let side = if source { "source" } else { "destination" };
        let refreshed = crate::oauth_refresh::refresh_access_token(&config.as_request())
            .map_err(|error| format!("Could not refresh the {side} OAuth access token: {error}"))?;
        let expires_in = refreshed.expires_in;
        if let Some(rotated_refresh_token) = refreshed.refresh_token {
            let rotated = crate::oauth_refresh::OAuthRefreshConfig {
                refresh_token: rotated_refresh_token,
                ..config
            };
            if let Err(error) = persist_rotated_refresh_config_with_retry(&rotated, |config| {
                self.store_oauth_refresh_config(source, config)
            }) {
                let recovery = crate::oauth_refresh::OAuthRefreshRecovery {
                    config: rotated,
                    access_token: refreshed.access_token,
                    expires_in,
                };
                let retention_error = recovery_key
                    .ok_or_else(|| {
                        "OAuth refresh rotation returned a new token but no recovery key was configured"
                            .to_owned()
                    })
                    .and_then(|key| {
                        crate::oauth_refresh::put_refresh_recovery(key, recovery)
                    })
                .err();
                return Err(format!(
                    "[oauth_refresh_rotation_persistence_pending] CRITICAL: OAuth refresh rotation succeeded but the rotated refresh token could not be persisted; the original refresh token may already be consumed. Retry keyring persistence before resuming the migration (persistence error: {error}{})",
                    retention_error.map_or_else(String::new, |value| format!(
                        "; recovery retention error: {value}"
                    ))
                ));
            }
            if source {
                self.source_password = refreshed.access_token;
            } else {
                self.destination_password = refreshed.access_token;
            }
        } else if source {
            self.source_password = refreshed.access_token;
        } else {
            self.destination_password = refreshed.access_token;
        }
        Ok(OAuthRefreshOutcome::Refreshed { expires_in })
    }
}
