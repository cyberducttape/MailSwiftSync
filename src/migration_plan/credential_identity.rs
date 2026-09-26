//! Credential identity used to authorize promotion from preflight to live.
//!
//! Access tokens from automatic OAuth refresh are intentionally treated as
//! rotating material. The stable binding covers the approved account and
//! refresh configuration; static credentials continue to bind by material.

use super::{Form, auth_method_is_oauth};
use crate::credentials::SecretString;
use sha2::{Digest, Sha256};

impl Form {
    /// A process-local comparison value for credential material. It is never
    /// persisted or included in a plan snapshot and is used only to bind the
    /// immediate live authentication probe to the bytes it actually tested.
    pub(crate) fn credential_fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.source_password.as_bytes());
        digest.update([0]);
        digest.update(self.destination_password.as_bytes());
        let result = digest.finalize();
        result
            .iter()
            .map(|byte| format!("{:02x}", byte))
            .collect::<String>()
    }

    /// A process-local identity for the credentials approved during
    /// preflight. Automatic OAuth refresh deliberately excludes the bearer
    /// token because it is expected to rotate.
    pub(crate) fn credential_binding_fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        let mut update = |value: &str| {
            digest.update(value.len().to_string().as_bytes());
            digest.update([0]);
            digest.update(value.as_bytes());
            digest.update([0xff]);
        };
        let mut update_side = |side: &str,
                               host: &str,
                               user: &str,
                               auth: &str,
                               credential_id: &str,
                               refresh_id: &str,
                               password: &SecretString,
                               source: bool| {
            update(side);
            update(host);
            update(user);
            update(auth);
            update(credential_id);
            if auth_method_is_oauth(auth) && !refresh_id.trim().is_empty() {
                update("automatic-oauth-refresh");
                update(refresh_id.trim());
                if let Ok(Some(config)) = self.load_oauth_refresh_config(source) {
                    update(&config.token_endpoint);
                    update(&config.client_id);
                } else {
                    update("oauth-refresh-config-unavailable");
                }
            } else {
                update("static-credential-material");
                update(&self.material_fingerprint(password));
            }
        };
        update_side(
            "source",
            &self.profile.source_host,
            &self.profile.source_user,
            &self.profile.source_auth,
            &self.profile.source_credential_id,
            &self.profile.source_oauth_refresh_credential_id,
            &self.source_password,
            true,
        );
        update_side(
            "destination",
            &self.profile.destination_host,
            &self.profile.destination_user,
            &self.profile.destination_auth,
            &self.profile.destination_credential_id,
            &self.profile.destination_oauth_refresh_credential_id,
            &self.destination_password,
            false,
        );
        let result = digest.finalize();
        result
            .iter()
            .map(|byte| format!("{:02x}", byte))
            .collect::<String>()
    }

    fn material_fingerprint(&self, password: &SecretString) -> String {
        let mut digest = Sha256::new();
        digest.update(password.as_bytes());
        let result = digest.finalize();
        result
            .iter()
            .map(|byte| format!("{:02x}", byte))
            .collect::<String>()
    }
}
