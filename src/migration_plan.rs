mod credential_identity;
mod profile;

use crate::atomic_artifact::write_private_atomic;
use crate::command::remove_option;
use crate::imap_probe::{command_endpoint_parts, command_port};
use crate::{
    DOVECOT_SYNC_LOCK_WAIT_SECONDS, core, create_secret_directory,
    credentials::{self, SecretString},
    endpoint, engine,
    plan_identity::{
        configured_file_content_identity, executable_content_identity, snapshot_sha256,
    },
    write_secret_file,
};
use keyring::Entry;
pub(crate) use profile::{
    DovecotMigrationStrategy, Profile, RunPlanSnapshot, RunProfileSnapshot, auth_method_is_oauth,
    completeness, default_auth_method, default_destination_tls, default_doveadm_path,
    default_migration_timeout_hours, default_source_tls,
};
use sha2::{Digest, Sha256};
use std::{
    io::{Error, ErrorKind, Read},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};
use zeroize::Zeroizing;

const MAX_PROFILE_BYTES: u64 = 1024 * 1024;
const MAX_KEYRING_CREDENTIAL_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DovecotConfigDialect {
    Legacy23,
    Modern24,
}

fn dovecot_config_dialect(version: &str) -> Result<DovecotConfigDialect, String> {
    let version_token = version.split_whitespace().next().unwrap_or_default();
    let mut components = version_token.split('.');
    let major = components
        .next()
        .and_then(|component| component.parse::<u32>().ok());
    let minor = components
        .next()
        .and_then(|component| component.parse::<u32>().ok());
    match (major, minor) {
        (Some(2), Some(3)) => Ok(DovecotConfigDialect::Legacy23),
        (Some(2), Some(4)) => Ok(DovecotConfigDialect::Modern24),
        _ => Err(format!(
            "unsupported or unrecognized Dovecot version {version:?}; native migration currently supports Dovecot 2.3 and 2.4"
        )),
    }
}

fn detect_dovecot_config_dialect(doveadm_path: &str) -> Result<DovecotConfigDialect, String> {
    let version = crate::runner::probe_engine_version(doveadm_path)
        .or_else(|| {
            let configured = resolve_executable_path(doveadm_path)?;
            let path_doveadm = resolve_executable_path("doveadm")?;
            (configured == path_doveadm)
                .then(|| crate::runner::probe_engine_version("dovecot"))
                .flatten()
        })
        .ok_or_else(|| {
        "could not determine the configured doveadm version; refusing to guess its mail-location configuration syntax".to_owned()
        })?;
    dovecot_config_dialect(&version)
}

fn resolve_executable_path(executable: &str) -> Option<PathBuf> {
    let path = Path::new(executable);
    if path.components().count() > 1 || path.is_absolute() {
        return path.canonicalize().ok();
    }
    let search_path = std::env::var_os("PATH")?;
    std::env::split_paths(&search_path)
        .map(|directory| directory.join(path))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| candidate.canonicalize().ok())
}

fn append_imapc_mail_settings(args: &mut Vec<String>, dialect: DovecotConfigDialect) {
    args.extend(["-o".into()]);
    match dialect {
        DovecotConfigDialect::Legacy23 => args.push("mail_location=imapc:".into()),
        DovecotConfigDialect::Modern24 => {
            args.push("mail_driver=imapc".into());
            args.extend(["-o".into(), "mail_path=".into()]);
        }
    }
}

fn read_profile_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_PROFILE_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("profile exceeds the {MAX_PROFILE_BYTES}-byte limit"),
        ));
    }
    let mut text = String::new();
    file.by_ref()
        .take(MAX_PROFILE_BYTES + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_PROFILE_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("profile exceeds the {MAX_PROFILE_BYTES}-byte limit"),
        ));
    }
    Ok(text)
}

pub(crate) fn decode_report_run_snapshot(
    snapshot: &str,
) -> Result<Option<RunPlanSnapshot>, String> {
    if snapshot.trim().is_empty() {
        return Ok(None);
    }
    if snapshot.len() as u64 > MAX_PROFILE_BYTES {
        return Err(format!(
            "The evidence run plan snapshot exceeds the {MAX_PROFILE_BYTES}-byte limit"
        ));
    }
    toml::from_str(snapshot)
        .map(Some)
        .map_err(|error| format!("The evidence run plan snapshot is corrupt: {error}"))
}

pub(crate) fn validate_certificate_pin(value: &str, label: &str) -> Result<(), String> {
    let pin = value.trim();
    if pin.is_empty() {
        return Ok(());
    }
    if pin.len() != 64 || !pin.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "{label} must be a 64-character SHA-256 certificate fingerprint"
        ));
    }
    Ok(())
}

/// The result of an attempted automatic OAuth refresh, distinguishing "this
/// side is not using OAuth, or has no refresh configuration" (a no-op) from
/// an actual successful exchange, so callers can report a meaningful status
/// message rather than a bare boolean.
pub(crate) enum OAuthRefreshOutcome {
    NotConfigured,
    Refreshed { expires_in: Option<u64> },
}

const OAUTH_REFRESH_PERSIST_ATTEMPTS: usize = 3;
const OAUTH_REFRESH_PERSIST_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Keyring writes are outside the provider transaction. Retry short-lived
/// backend/IPC failures before declaring that a provider-side rotation has
/// entered the catastrophic recovery state.
fn persist_rotated_refresh_config_with_retry<F>(
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

pub(crate) fn effective_destination_tls(mode: &str) -> &str {
    if mode == "starttls" {
        "starttls"
    } else {
        "imaps"
    }
}

pub(crate) fn default_imap_port(tls_mode: &str) -> u16 {
    match tls_mode {
        "starttls" | "plain" => 143,
        _ => 993,
    }
}

fn is_microsoft_365_endpoint(configured_endpoint: &str, tls_mode: &str) -> bool {
    let Ok((host, _)) = endpoint::parts(configured_endpoint, default_imap_port(tls_mode)) else {
        // Endpoint syntax is validated separately and will produce its own
        // actionable error. Do not attempt provider identification on invalid input.
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    ["outlook.office365.com", "exchange.microsoft.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

pub(crate) fn dovecot_ssl_mode(mode: &str) -> &str {
    match mode {
        "plain" => "no",
        other => other,
    }
}

pub(crate) fn append_dovecot_source_tls_policy(
    args: &mut Vec<String>,
    mode: &str,
    ca_bundle: &str,
) {
    if mode != "plain" {
        args.extend(["-o".into(), "ssl_client_require_valid_cert=yes".into()]);
        if !ca_bundle.trim().is_empty() {
            args.extend([
                "-o".into(),
                format!("ssl_client_ca_file={}", ca_bundle.trim()),
            ]);
        }
    }
}
#[derive(Clone)]
pub(crate) struct Form {
    pub(crate) profile: Profile,
    pub(crate) source_password: SecretString,
    pub(crate) destination_password: SecretString,
    pub(crate) dry_run: bool,
}

fn decode_saved_profile(path: &Path, text: &str) -> Result<Profile, String> {
    let raw: toml::Value = toml::from_str(text)
        .map_err(|error| format!("could not decode saved profile {}: {error}", path.display()))?;
    let remote_execution = raw
        .get("dovecot_execution")
        .and_then(toml::Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("ssh"));
    let automatic_remote_execution = raw
        .get("dovecot_execution")
        .and_then(toml::Value::as_str)
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("automatic")
                && !["localhost", "127.0.0.1", "::1"].contains(
                    &raw.get("destination_host")
                        .and_then(toml::Value::as_str)
                        .unwrap_or_default()
                        .trim(),
                )
        });
    let remote_user = raw
        .get("dovecot_ssh_user")
        .and_then(toml::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if remote_execution || automatic_remote_execution || remote_user {
        return Err(format!(
            "saved profile {} requests unsupported remote Dovecot execution; use local doveadm or imapsync",
            path.display()
        ));
    }
    raw.try_into()
        .map_err(|error| format!("could not decode saved profile {}: {error}", path.display()))
}

impl Default for Form {
    fn default() -> Self {
        Self {
            profile: Profile {
                name: "New migration".into(),
                imapsync_path: "imapsync".into(),
                source_tls: default_source_tls(),
                source_auth: default_auth_method(),
                destination_tls: default_destination_tls(),
                destination_auth: default_auth_method(),
                doveadm_path: default_doveadm_path(),
                migration_timeout_hours: default_migration_timeout_hours(),
                automap: false,
                sync_internaldates: true,
                ..Default::default()
            },
            source_password: SecretString::default(),
            destination_password: SecretString::default(),
            dry_run: true,
        }
    }
}
fn dovecot_config_path(path: &Path) -> Result<String, String> {
    let value = path
        .to_str()
        .ok_or_else(|| "Dovecot config and secret paths must be valid UTF-8".to_owned())?;
    if value.chars().any(char::is_whitespace) || value.contains(['\n', '\r', '"']) {
        return Err("Dovecot config and secret paths cannot contain whitespace or quotes".into());
    }
    Ok(value.to_owned())
}

fn resolve_dovecot_base_config(configured: &str) -> Result<PathBuf, String> {
    if !configured.trim().is_empty() {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return path.canonicalize().map_err(|error| {
                format!("could not resolve the configured Dovecot config file: {error}")
            });
        }
        return Err("the configured Dovecot config file does not exist or is not a file".into());
    }

    [
        "/etc/dovecot/dovecot.conf",
        "/usr/local/etc/dovecot/dovecot.conf",
        "/opt/homebrew/etc/dovecot/dovecot.conf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
    .ok_or_else(|| {
        String::from(
            "could not locate the default Dovecot config; set the Dovecot Config path explicitly",
        )
    })?
    .canonicalize()
    .map_err(|error| format!("could not resolve the Dovecot config file: {error}"))
}

fn write_dovecot_runtime_config(
    directory: &Path,
    configured_base: &str,
    password_file: &Path,
) -> Result<PathBuf, String> {
    let base = resolve_dovecot_base_config(configured_base)?;
    let base = dovecot_config_path(&base)?;
    let password = dovecot_config_path(password_file)?;
    let runtime_config = directory.join("mailswiftsync-doveadm.conf");
    let _runtime_config_path = dovecot_config_path(&runtime_config)?;
    let contents = format!("!include {base}\nimapc_password = <{password}\n");
    write_secret_file(&runtime_config, &contents)
        .map_err(|error| format!("could not prepare private Dovecot runtime config: {error}"))?;
    Ok(runtime_config)
}

impl Form {
    pub(crate) const KEYRING_SERVICE: &'static str = "com.mailswiftsync.mailbox";
    /// Deliberately a distinct keyring service from `KEYRING_SERVICE`, even
    /// though an operator may reuse the same ID string for both entries: a
    /// refresh configuration carries a client secret and refresh token, not
    /// an access token, and must never be returned by a plain password load.
    pub(crate) const OAUTH_REFRESH_KEYRING_SERVICE: &'static str =
        "com.mailswiftsync.oauth-refresh";

    /// Clone the non-secret migration defaults for a bulk row.
    ///
    /// Imported rows must provide their own credentials (or credential IDs),
    /// so copying the base form's secret fields only creates unnecessary
    /// transient secret material.
    pub(crate) fn clone_without_credentials(&self) -> Self {
        Self {
            profile: self.profile.clone(),
            source_password: SecretString::default(),
            destination_password: SecretString::default(),
            dry_run: self.dry_run,
        }
    }

    pub(crate) fn path() -> Result<std::path::PathBuf, String> {
        dirs_next::config_dir()
            .map(|directory| directory.join("mailswiftsync/profile.toml"))
            .ok_or_else(|| {
                "Cannot determine a durable configuration directory; repair the OS profile or use an explicit state path.".to_owned()
            })
    }
    pub(crate) fn load() -> Result<Self, String> {
        let mut form = Self::default();
        let path = Self::path()?;
        let legacy = path
            .parent()
            .and_then(|parent| parent.parent())
            .map(|directory| directory.join("sourcecraft-imapsync/profile.toml"))
            .ok_or_else(|| "Saved profile path has no configuration directory.".to_owned())?;
        let text = match read_profile_file(&path) {
            Ok(text) => Some((path, text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match read_profile_file(&legacy) {
                    Ok(text) => Some((legacy, text)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(format!("could not read legacy profile: {error}")),
                }
            }
            Err(error) => return Err(format!("could not read profile: {error}")),
        };
        if let Some((path, text)) = text {
            form.profile = decode_saved_profile(&path, &text)?;
        }
        Ok(form)
    }
    #[allow(dead_code)]
    pub(crate) fn save(&self) -> Result<(), String> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            credentials::ensure_private_directory(parent).map_err(|e| e.to_string())?;
        }
        let content = toml::to_string_pretty(&self.profile).map_err(|e| e.to_string())?;
        write_private_atomic(&path, &content).map_err(|e| e.to_string())
    }
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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

    fn oauth_refresh_keyring_entry(&self, source: bool) -> Result<Option<Entry>, String> {
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

    fn oauth_refresh_recovery_key(&self, source: bool) -> Option<String> {
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

    pub(crate) fn validate(&self) -> Result<(), String> {
        self.validate_internal(true)
    }
    pub(crate) fn validate_for_import(&self) -> Result<(), String> {
        self.validate_internal(false)
    }
    pub(crate) fn validate_internal(&self, require_credentials: bool) -> Result<(), String> {
        let mut required = vec![
            ("Source IMAP host", self.profile.source_host.as_str()),
            ("Source username", self.profile.source_user.as_str()),
            (
                "Destination IMAP host",
                self.profile.destination_host.as_str(),
            ),
            (
                "Destination username",
                self.profile.destination_user.as_str(),
            ),
        ];
        let source_automatic_refresh = auth_method_is_oauth(&self.profile.source_auth)
            && !self
                .profile
                .source_oauth_refresh_credential_id
                .trim()
                .is_empty();
        let destination_automatic_refresh = auth_method_is_oauth(&self.profile.destination_auth)
            && !self
                .profile
                .destination_oauth_refresh_credential_id
                .trim()
                .is_empty();
        if require_credentials && !source_automatic_refresh {
            required.push((
                if auth_method_is_oauth(&self.profile.source_auth) {
                    "Source OAuth 2.0 access token"
                } else {
                    "Source password"
                },
                self.source_password.as_str(),
            ));
        }
        if require_credentials
            && self.engine() != core::Engine::Dovecot
            && !destination_automatic_refresh
        {
            required.push((
                if auth_method_is_oauth(&self.profile.destination_auth) {
                    "Destination OAuth 2.0 access token"
                } else {
                    "Destination password"
                },
                self.destination_password.as_str(),
            ));
        }
        if self.engine() == core::Engine::Dovecot
            && (auth_method_is_oauth(&self.profile.source_auth)
                || auth_method_is_oauth(&self.profile.destination_auth))
        {
            return Err(
                "OAuth 2.0 authentication is currently supported for imapsync only; Dovecot native execution requires password authentication.".into(),
            );
        }
        for (label, method) in [
            ("Source authentication", self.profile.source_auth.as_str()),
            (
                "Destination authentication",
                self.profile.destination_auth.as_str(),
            ),
        ] {
            if !matches!(method, "" | "password" | "oauth2") {
                return Err(format!(
                    "{label} must be password or OAuth 2.0 access token."
                ));
            }
        }
        for (label, host, tls_mode, method) in [
            (
                "Source",
                self.profile.source_host.as_str(),
                self.profile.source_tls.as_str(),
                self.profile.source_auth.as_str(),
            ),
            (
                "Destination",
                self.profile.destination_host.as_str(),
                effective_destination_tls(&self.profile.destination_tls),
                self.profile.destination_auth.as_str(),
            ),
        ] {
            if is_microsoft_365_endpoint(host, tls_mode) && method != "oauth2" {
                return Err(format!(
                    "{label} Microsoft 365 IMAP requires OAuth 2.0 / Modern Authentication; Basic Authentication and app passwords are not supported"
                ));
            }
        }
        let source_port = self.profile.source_port.trim();
        if !source_port.is_empty() && source_port.parse::<u16>().map_or(true, |port| port == 0) {
            return Err("Source IMAP port must be a number between 1 and 65535.".into());
        }
        if !matches!(
            self.profile.source_tls.as_str(),
            "imaps" | "starttls" | "plain"
        ) {
            return Err("Source TLS mode must be imaps, starttls, or plain.".into());
        }
        let destination_port = self.profile.destination_port.trim();
        if !destination_port.is_empty()
            && destination_port
                .parse::<u16>()
                .map_or(true, |port| port == 0)
        {
            return Err("Destination IMAP port must be a number between 1 and 65535.".into());
        }
        if !matches!(
            self.profile.destination_tls.as_str(),
            "" | "imaps" | "starttls"
        ) {
            return Err("Destination TLS mode must be imaps or starttls.".into());
        }
        validate_certificate_pin(
            &self.profile.source_certificate_pin_sha256,
            "Source certificate pin",
        )?;
        validate_certificate_pin(
            &self.profile.destination_certificate_pin_sha256,
            "Destination certificate pin",
        )?;
        if self.engine() == core::Engine::Dovecot
            && (!self.profile.source_certificate_pin_sha256.trim().is_empty()
                || !self
                    .profile
                    .destination_certificate_pin_sha256
                    .trim()
                    .is_empty())
        {
            return Err(
                "Certificate pinning is not currently supported by the Dovecot engine; remove the pin or select imapsync."
                    .into(),
            );
        }
        endpoint::parts(
            &self.profile.source_host,
            default_imap_port(&self.profile.source_tls),
        )
        .map_err(|error| format!("Source IMAP host is not a valid endpoint: {error}"))?;
        endpoint::parts(
            &self.profile.destination_host,
            default_imap_port(effective_destination_tls(&self.profile.destination_tls)),
        )
        .map_err(|error| format!("Destination IMAP host is not a valid endpoint: {error}"))?;
        if !(1..=720).contains(&self.profile.migration_timeout_hours) {
            return Err("Migration timeout must be between 1 and 720 hours.".into());
        }
        for (label, value) in required {
            if value.trim().is_empty() {
                return Err(format!("{label} is required."));
            }
            if value.chars().any(char::is_control) {
                return Err(format!("{label} cannot contain control characters."));
            }
        }
        for (label, value) in [
            ("Source keyring ID", &self.profile.source_credential_id),
            (
                "Destination keyring ID",
                &self.profile.destination_credential_id,
            ),
        ] {
            let trimmed = value.trim();
            if trimmed.chars().any(char::is_control) || trimmed.len() > 256 {
                return Err(format!(
                    "{label} must not contain control characters and must be at most 256 bytes."
                ));
            }
        }
        if require_credentials && self.requires_insecure_transport_ack() {
            return Err(
                "Plain IMAP requires an explicit cleartext-transport acknowledgement before any authenticated operation, including dry preflight.".into(),
            );
        }
        for (label, value) in [
            ("Source IMAP host", self.profile.source_host.as_str()),
            (
                "Destination IMAP host",
                self.profile.destination_host.as_str(),
            ),
            ("Source username", self.profile.source_user.as_str()),
            (
                "Destination username",
                self.profile.destination_user.as_str(),
            ),
            ("Source credential", self.source_password.as_str()),
            ("Destination credential", self.destination_password.as_str()),
        ] {
            if !value.is_empty() && value.chars().any(char::is_control) {
                return Err(format!("{label} cannot contain control characters."));
            }
        }
        self.extra_options_valid()?;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn args(&self, redact: bool) -> Vec<String> {
        self.args_with_throttle_divisor(redact, 1)
    }
    #[cfg(test)]
    pub(crate) fn args_with_throttle_divisor(
        &self,
        redact: bool,
        throttle_divisor: usize,
    ) -> Vec<String> {
        self.args_with_throttle_divisor_and_mode(redact, throttle_divisor, self.dry_run)
    }
    pub(crate) fn args_with_throttle_divisor_and_mode(
        &self,
        redact: bool,
        throttle_divisor: usize,
        dry_run: bool,
    ) -> Vec<String> {
        let _ = redact;
        engine::imapsync_preview_args(&self.profile, dry_run, throttle_divisor)
    }
    pub(crate) fn extra_options_valid(&self) -> Result<(), String> {
        engine::validate_extra_options(&self.profile.extra_options)
    }

    /// Build the exact secret-free command shown by the execution-plan
    /// preview. The imapsync branch returns the same validated canonical
    /// argument vector that the runtime command builder is permitted to use;
    /// it must never fall back to silently omitting an invalid expert option.
    pub(crate) fn preview_command(&self) -> Result<(String, Vec<String>), String> {
        self.validate_internal(false)?;
        if self.engine() == core::Engine::Dovecot {
            return Ok(self.command(true));
        }
        Ok((
            self.profile.imapsync_path.clone(),
            engine::try_imapsync_preview_args(&self.profile, self.dry_run, 1)?,
        ))
    }
    #[cfg(test)]
    pub(crate) fn prepared_command(&self) -> Result<PreparedCommand, String> {
        self.prepare_command_with_dialect(1, None, Some(DovecotConfigDialect::Modern24))
    }
    pub(crate) fn prepared_command_with_throttle_divisor_and_checkpoint(
        &self,
        throttle_divisor: usize,
        checkpoint: Option<&str>,
    ) -> Result<PreparedCommand, String> {
        let dovecot_dialect = if self.engine() == core::Engine::Dovecot {
            Some(detect_dovecot_config_dialect(&self.profile.doveadm_path)?)
        } else {
            None
        };
        self.prepare_command_with_dialect(throttle_divisor, checkpoint, dovecot_dialect)
    }

    fn prepare_command_with_dialect(
        &self,
        throttle_divisor: usize,
        checkpoint: Option<&str>,
        dovecot_dialect: Option<DovecotConfigDialect>,
    ) -> Result<PreparedCommand, String> {
        // Keep command preparation as a hard validation boundary. The
        // low-level builder emits canonical options, but it must never be
        // possible to prepare an engine invocation from a profile that has
        // not passed the same validator used by admission.
        self.extra_options_valid()?;
        if self.engine() == core::Engine::Dovecot {
            let dovecot_dialect = dovecot_dialect.ok_or_else(|| {
                "Dovecot command preparation requires a detected configuration dialect".to_owned()
            })?;
            let secret_dir = create_secret_directory()?;
            let source_file = secret_dir.join("source.secret");
            let runtime_config = write_secret_file(&source_file, self.source_password.as_str())
                .map_err(|error| format!("could not prepare source credential file: {error}"))
                .and_then(|()| {
                    write_dovecot_runtime_config(
                        &secret_dir,
                        &self.profile.dovecot_config,
                        &source_file,
                    )
                });
            let runtime_config = match runtime_config {
                Ok(path) => path,
                Err(error) => {
                    let _ = std::fs::remove_dir_all(&secret_dir);
                    return Err(error);
                }
            };
            let config_path = runtime_config.to_string_lossy();
            let (executable, args) = self.command_with_checkpoint_and_mode_and_config(
                false,
                checkpoint,
                self.dry_run,
                Some(&config_path),
                dovecot_dialect,
            );
            let verification = if self.dry_run {
                Vec::new()
            } else {
                self.dovecot_verification_commands_with_config(
                    false,
                    Some(&config_path),
                    dovecot_dialect,
                )
            };
            return Ok(PreparedCommand {
                executable,
                args,
                cleanup: vec![secret_dir],
                env: Vec::new(),
                verification,
            });
        }
        let mut args = engine::imapsync_args(&self.profile, self.dry_run, throttle_divisor);
        let secret_dir = create_secret_directory()?;
        let source_file = secret_dir.join("source.secret");
        let destination_file = secret_dir.join("destination.secret");
        if let Err(error) = write_secret_file(&source_file, self.source_password.as_str())
            .and_then(|_| write_secret_file(&destination_file, self.destination_password.as_str()))
        {
            let _ = std::fs::remove_dir_all(&secret_dir);
            return Err(format!(
                "Could not prepare temporary credential files: {error}"
            ));
        }
        if auth_method_is_oauth(&self.profile.source_auth) {
            args.extend([
                "--oauthaccesstoken1".into(),
                source_file.to_string_lossy().into_owned(),
            ]);
        } else {
            args.extend([
                "--passfile1".into(),
                source_file.to_string_lossy().into_owned(),
            ]);
        }
        if auth_method_is_oauth(&self.profile.destination_auth) {
            args.extend([
                "--oauthaccesstoken2".into(),
                destination_file.to_string_lossy().into_owned(),
            ]);
        } else {
            args.extend([
                "--passfile2".into(),
                destination_file.to_string_lossy().into_owned(),
            ]);
        }
        Ok(PreparedCommand {
            executable: self.profile.imapsync_path.clone(),
            args,
            cleanup: vec![secret_dir],
            env: Vec::new(),
            verification: Vec::new(),
        })
    }
    /// A deterministic, secret-free description of the live execution plan.
    /// It intentionally includes the generated arguments so changing an
    /// option, endpoint, engine, mapping, or credential reference invalidates
    /// an earlier preflight. Password bytes are intentionally excluded.
    pub(crate) fn plan_fingerprint(&self) -> String {
        let (executable, mut args) = self.command_with_checkpoint_and_mode(true, None, false);
        if self.engine() == core::Engine::ImapSync {
            remove_option(&mut args, "--password1");
            remove_option(&mut args, "--password2");
            remove_option(&mut args, "--oauthaccesstoken1");
            remove_option(&mut args, "--oauthaccesstoken2");
        }
        format!(
            "{}\n{}\ncredential-source1={}\ncredential-source2={}\nsource-auth={}\ndestination-auth={}\ninsecure-source-transport-ack={}\nsource-ca-bundle={}\nsource-ca-bundle-sha256={}\ndestination-ca-bundle={}\ndestination-ca-bundle-sha256={}\nsource-certificate-pin={}\ndestination-certificate-pin={}\nexecution-executable-sha256={}\ndovecot-config-sha256={}\nbody-hash-verification={}\nbody-hash-max-bytes={}\nbody-hash-max-total-bytes={}",
            executable,
            args.join("\u{1f}"),
            self.profile.source_credential_id.trim(),
            self.profile.destination_credential_id.trim(),
            self.profile.source_auth,
            self.profile.destination_auth,
            self.profile.allow_insecure_source_transport,
            self.profile.source_ca_bundle.trim(),
            configured_file_content_identity(&self.profile.source_ca_bundle),
            self.profile.destination_ca_bundle.trim(),
            configured_file_content_identity(&self.profile.destination_ca_bundle),
            self.profile
                .source_certificate_pin_sha256
                .trim()
                .to_ascii_lowercase(),
            self.profile
                .destination_certificate_pin_sha256
                .trim()
                .to_ascii_lowercase(),
            executable_content_identity(&executable),
            configured_file_content_identity(&self.profile.dovecot_config),
            self.profile.body_hash_verification,
            self.profile.body_hash_max_bytes,
            self.profile.body_hash_max_total_bytes,
        )
    }

    /// Serialize the launch configuration without session passwords or raw
    /// expert-option values. This is persisted with the run so historical
    /// reports do not depend on the currently edited form.
    #[cfg(test)]
    pub(crate) fn plan_snapshot(&self) -> String {
        self.plan_snapshot_with_checkpoint(None)
            .expect("test snapshot serialization")
    }

    /// Serialize the launch configuration and, when applicable, the identity
    /// of the previously committed Dovecot state supplied to this run. The
    /// state token itself stays out of durable reports; its digest is enough
    /// to prove which resume point was selected.
    pub(crate) fn plan_snapshot_with_checkpoint(
        &self,
        checkpoint: Option<&str>,
    ) -> Result<String, String> {
        let canonical_extra_options = engine::canonical_extra_options(&self.profile.extra_options)?;
        let digest = Sha256::digest(canonical_extra_options.join("\u{1f}").as_bytes());
        let extra_options_sha256 = digest
            .iter()
            .map(|byte| format!("{:02x}", byte))
            .collect::<String>();
        let profile = &self.profile;
        let execution_executable = match self.engine() {
            core::Engine::Dovecot => &profile.doveadm_path,
            core::Engine::ImapSync | core::Engine::Auto => &profile.imapsync_path,
        };
        let snapshot = RunPlanSnapshot {
            dry_run: self.dry_run,
            profile: RunProfileSnapshot {
                name: profile.name.clone(),
                source_host: profile.source_host.clone(),
                source_port: profile.source_port.clone(),
                source_tls: profile.source_tls.clone(),
                source_ca_bundle: profile.source_ca_bundle.clone(),
                source_certificate_pin_sha256: profile.source_certificate_pin_sha256.clone(),
                allow_insecure_source_transport: profile.allow_insecure_source_transport,
                source_user: profile.source_user.clone(),
                source_auth: profile.source_auth.clone(),
                source_credential_id: profile.source_credential_id.clone(),
                source_oauth_refresh_credential_id: profile
                    .source_oauth_refresh_credential_id
                    .clone(),
                destination_host: profile.destination_host.clone(),
                destination_user: profile.destination_user.clone(),
                destination_auth: profile.destination_auth.clone(),
                destination_credential_id: profile.destination_credential_id.clone(),
                destination_oauth_refresh_credential_id: profile
                    .destination_oauth_refresh_credential_id
                    .clone(),
                destination_port: profile.destination_port.clone(),
                destination_tls: profile.destination_tls.clone(),
                destination_ca_bundle: profile.destination_ca_bundle.clone(),
                destination_certificate_pin_sha256: profile
                    .destination_certificate_pin_sha256
                    .clone(),
                imapsync_path: profile.imapsync_path.clone(),
                engine: profile.engine,
                doveadm_path: profile.doveadm_path.clone(),
                dovecot_config: profile.dovecot_config.clone(),
                batch_concurrency: profile.batch_concurrency,
                batch_retry_count: profile.batch_retry_count,
                max_messages_per_second: profile.max_messages_per_second,
                max_bytes_per_second: profile.max_bytes_per_second,
                body_hash_verification: profile.body_hash_verification,
                body_hash_max_bytes: profile.body_hash_max_bytes,
                body_hash_max_total_bytes: profile.body_hash_max_total_bytes,
                migration_timeout_hours: profile.migration_timeout_hours,
                automap: profile.automap,
                addheader: profile.addheader,
                justfolders: profile.justfolders,
                sync_internaldates: profile.sync_internaldates,
                useuid: profile.useuid,
                usecache: profile.usecache,
                fastio1: profile.fastio1,
                fastio2: profile.fastio2,
                allowsizemismatch: profile.allowsizemismatch,
                dovecot_strategy: profile.dovecot_strategy,
                delete2: profile.delete2,
                extra_options_sha256,
                dovecot_checkpoint_sha256: (self.engine() == core::Engine::Dovecot
                    && !self.dry_run)
                    .then(|| checkpoint.map(snapshot_sha256))
                    .flatten(),
                execution_executable_sha256: executable_content_identity(execution_executable),
                source_ca_bundle_sha256: configured_file_content_identity(
                    &profile.source_ca_bundle,
                ),
                destination_ca_bundle_sha256: configured_file_content_identity(
                    &profile.destination_ca_bundle,
                ),
                dovecot_config_sha256: configured_file_content_identity(&profile.dovecot_config),
            },
        };
        toml::to_string(&snapshot)
            .map_err(|error| format!("could not serialize immutable run plan snapshot: {error}"))
    }

    pub(crate) fn requires_insecure_transport_ack(&self) -> bool {
        self.profile.source_tls == "plain" && !self.profile.allow_insecure_source_transport
    }
    pub(crate) fn engine(&self) -> core::Engine {
        match self.profile.engine {
            // The desktop cannot safely infer the destination's mail stack
            // from a hostname or from a locally installed executable.
            // Hostnames are not reliable server fingerprints. Auto is an
            // explicit conservative default, not environment detection.
            core::Engine::Auto => core::Engine::ImapSync,
            selected => selected,
        }
    }
    pub(crate) fn command(&self, redact: bool) -> (String, Vec<String>) {
        self.command_with_checkpoint(redact, None)
    }
    pub(crate) fn command_with_checkpoint(
        &self,
        redact: bool,
        checkpoint: Option<&str>,
    ) -> (String, Vec<String>) {
        self.command_with_checkpoint_and_mode(redact, checkpoint, self.dry_run)
    }
    pub(crate) fn command_with_checkpoint_and_mode(
        &self,
        redact: bool,
        checkpoint: Option<&str>,
        dry_run: bool,
    ) -> (String, Vec<String>) {
        self.command_with_checkpoint_and_mode_and_config(
            redact,
            checkpoint,
            dry_run,
            None,
            DovecotConfigDialect::Modern24,
        )
    }
    fn command_with_checkpoint_and_mode_and_config(
        &self,
        redact: bool,
        checkpoint: Option<&str>,
        dry_run: bool,
        runtime_config: Option<&str>,
        dovecot_dialect: DovecotConfigDialect,
    ) -> (String, Vec<String>) {
        if self.engine() != core::Engine::Dovecot {
            return (
                self.profile.imapsync_path.clone(),
                self.args_with_throttle_divisor_and_mode(redact, 1, dry_run),
            );
        }
        let source_default_port = default_imap_port(&self.profile.source_tls);
        let (source_host, endpoint_port) =
            command_endpoint_parts(&self.profile.source_host, source_default_port);
        let source_port = command_port(&self.profile.source_port, endpoint_port);
        let mut args = Vec::new();
        args.push("-k".into());
        if let Some(config) = runtime_config {
            args.extend(["-c".into(), config.to_owned()]);
        } else if !self.profile.dovecot_config.trim().is_empty() {
            args.extend(["-c".into(), self.profile.dovecot_config.clone()]);
        }
        args.extend([
            "-o".into(),
            format!("imapc_host={source_host}"),
            "-o".into(),
            format!("imapc_ssl={}", dovecot_ssl_mode(&self.profile.source_tls)),
            "-o".into(),
            format!("imapc_user={}", self.profile.source_user),
        ]);
        append_dovecot_source_tls_policy(
            &mut args,
            &self.profile.source_tls,
            &self.profile.source_ca_bundle,
        );
        if !self.profile.source_port.trim().is_empty() {
            args.extend([
                "-o".into(),
                format!("imapc_port={}", self.profile.source_port.trim()),
            ]);
        } else {
            args.extend(["-o".into(), format!("imapc_port={source_port}")]);
        }
        if dry_run {
            append_imapc_mail_settings(&mut args, dovecot_dialect);
            args.extend([
                "mailbox".into(),
                "list".into(),
                "-u".into(),
                self.profile.source_user.clone(),
            ]);
        } else {
            let preservation_sync = self.profile.dovecot_strategy.uses_preservation_sync();
            // `-l`, `-s`, `-1`, and `-u` are dsync subcommand options, not
            // global doveadm options. Put the sync/backup subcommand before
            // them or Dovecot 2.3 rejects the command as invalid.
            args.push(if preservation_sync { "sync" } else { "backup" }.into());
            args.extend(["-l".into(), DOVECOT_SYNC_LOCK_WAIT_SECONDS.to_string()]);
            // Dovecot prints a new state string when -s is supplied. An
            // empty state requests an initial stateful pass; a prior
            // committed checkpoint makes later passes incremental.
            args.extend([
                "-s".into(),
                core::dovecot_checkpoint_state(checkpoint.unwrap_or_default()).to_owned(),
            ]);
            if preservation_sync {
                args.push("-1".into());
            }
            args.extend([
                "-Ru".into(),
                self.profile.destination_user.clone(),
                "imapc:".into(),
            ]);
        }
        (self.profile.doveadm_path.clone(), args)
    }
    #[cfg(test)]
    pub(crate) fn local_doveadm(&self) -> bool {
        true
    }
    #[cfg(test)]
    pub(crate) fn dovecot_verification_commands(&self, redact: bool) -> Vec<(String, Vec<String>)> {
        self.dovecot_verification_commands_with_config(redact, None, DovecotConfigDialect::Modern24)
    }
    fn dovecot_verification_commands_with_config(
        &self,
        redact: bool,
        runtime_config: Option<&str>,
        dovecot_dialect: DovecotConfigDialect,
    ) -> Vec<(String, Vec<String>)> {
        if self.engine() != core::Engine::Dovecot {
            return Vec::new();
        }
        let _ = redact;
        let source_default_port = default_imap_port(&self.profile.source_tls);
        let (source_host, endpoint_port) =
            command_endpoint_parts(&self.profile.source_host, source_default_port);
        let source_port = command_port(&self.profile.source_port, endpoint_port);
        let mut source = Vec::new();
        source.push("-k".into());
        if let Some(config) = runtime_config {
            source.extend(["-c".into(), config.to_owned()]);
        } else if !self.profile.dovecot_config.trim().is_empty() {
            source.extend(["-c".into(), self.profile.dovecot_config.clone()]);
        }
        source.extend([
            "-o".into(),
            format!("imapc_host={source_host}"),
            "-o".into(),
            format!("imapc_ssl={}", dovecot_ssl_mode(&self.profile.source_tls)),
            "-o".into(),
            format!("imapc_user={}", self.profile.source_user),
        ]);
        // These are global `doveadm -o` settings, so they must all precede
        // the `mailbox status` subcommand (the same rule as sync/backup).
        append_dovecot_source_tls_policy(
            &mut source,
            &self.profile.source_tls,
            &self.profile.source_ca_bundle,
        );
        if !self.profile.source_port.trim().is_empty() {
            source.extend([
                "-o".into(),
                format!("imapc_port={}", self.profile.source_port.trim()),
            ]);
        } else {
            source.extend(["-o".into(), format!("imapc_port={source_port}")]);
        }
        append_imapc_mail_settings(&mut source, dovecot_dialect);
        source.extend([
            "mailbox".into(),
            "status".into(),
            "-u".into(),
            self.profile.source_user.clone(),
            "messages vsize uidvalidity".into(),
            "*".into(),
        ]);
        let mut destination = Vec::new();
        if let Some(config) = runtime_config {
            destination.extend(["-c".into(), config.to_owned()]);
        } else if !self.profile.dovecot_config.trim().is_empty() {
            destination.extend(["-c".into(), self.profile.dovecot_config.clone()]);
        }
        destination.extend([
            "mailbox".into(),
            "status".into(),
            "-u".into(),
            self.profile.destination_user.clone(),
            "messages vsize uidvalidity".into(),
            "*".into(),
        ]);
        vec![
            (self.profile.doveadm_path.clone(), source),
            (self.profile.doveadm_path.clone(), destination),
        ]
    }

    pub(crate) fn dovecot_destination_preflight_commands(&self) -> Vec<(String, Vec<String>)> {
        if self.engine() != core::Engine::Dovecot {
            return Vec::new();
        }
        let mut user = Vec::new();
        if !self.profile.dovecot_config.trim().is_empty() {
            user.extend(["-c".into(), self.profile.dovecot_config.clone()]);
        }
        user.extend(["user".into(), self.profile.destination_user.clone()]);
        // Do not open or enumerate the destination mailboxes before the first
        // dsync. Dovecot warns that this can alter INBOX GUID/UIDVALIDITY and
        // cause the initial backup to fail (or reconcile the wrong state).
        // The userdb lookup validates the local destination identity without
        // touching the mailbox store.
        vec![(self.profile.doveadm_path.clone(), user)]
    }
}

pub(crate) struct PreparedCommand {
    pub(crate) executable: String,
    pub(crate) args: Vec<String>,
    pub(crate) cleanup: Vec<PathBuf>,
    pub(crate) env: Vec<(String, credentials::SecretString)>,
    pub(crate) verification: Vec<(String, Vec<String>)>,
}

#[cfg(test)]
mod tests {
    use super::{
        DovecotConfigDialect, Form, MAX_PROFILE_BYTES, OAuthRefreshOutcome,
        decode_report_run_snapshot, decode_saved_profile, detect_dovecot_config_dialect,
        dovecot_config_dialect, persist_rotated_refresh_config_with_retry,
        validate_certificate_pin,
    };
    use crate::SecretString;
    use crate::oauth_refresh::OAuthRefreshConfig;
    use std::path::Path;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn dovecot_mail_location_syntax_tracks_supported_major_minor_versions() {
        assert_eq!(
            dovecot_config_dialect("2.3.19.1 (9b53102964)").unwrap(),
            DovecotConfigDialect::Legacy23
        );
        assert_eq!(
            dovecot_config_dialect("2.4.0 (abc123)").unwrap(),
            DovecotConfigDialect::Modern24
        );
        assert!(dovecot_config_dialect("not-a-version").is_err());
        assert!(dovecot_config_dialect("2.5.0").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn dovecot_config_dialect_is_probed_from_the_selected_doveadm() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-doveadm-version-{}-{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, "#!/bin/sh\nprintf '2.3.19.1 (fixture)\\n'\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            detect_dovecot_config_dialect(&path.to_string_lossy()).unwrap(),
            DovecotConfigDialect::Legacy23
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn dovecot_source_commands_use_the_matching_mail_location_dialect() {
        let mut form = Form::default();
        form.profile.engine = crate::core::Engine::Dovecot;
        form.dry_run = true;
        for (dialect, expected, unexpected) in [
            (
                DovecotConfigDialect::Legacy23,
                "mail_location=imapc:",
                "mail_driver=imapc",
            ),
            (
                DovecotConfigDialect::Modern24,
                "mail_driver=imapc",
                "mail_location=imapc:",
            ),
        ] {
            let (_, args) =
                form.command_with_checkpoint_and_mode_and_config(true, None, true, None, dialect);
            assert!(args.iter().any(|argument| argument == expected));
            assert!(!args.iter().any(|argument| argument == unexpected));
            let verification = form.dovecot_verification_commands_with_config(true, None, dialect);
            assert!(
                verification[0]
                    .1
                    .iter()
                    .any(|argument| argument == expected)
            );
            assert!(
                !verification[0]
                    .1
                    .iter()
                    .any(|argument| argument == unexpected)
            );
            assert!(verification.iter().all(|(_, arguments)| {
                arguments
                    .iter()
                    .any(|argument| argument == "messages vsize uidvalidity")
            }));
        }
    }

    #[test]
    fn legacy_remote_dovecot_profiles_fail_closed() {
        let result = decode_saved_profile(
            Path::new("profile.toml"),
            "dovecot_execution = \"ssh\"\ndovecot_ssh_user = \"migration\"\n",
        );
        let error = match result {
            Ok(_) => panic!("legacy remote profile should be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("unsupported remote Dovecot execution"));

        let result = decode_saved_profile(
            Path::new("profile.toml"),
            "dovecot_execution = \"automatic\"\ndestination_host = \"mail.example\"\n",
        );
        assert!(result.is_err());
    }

    #[test]
    fn report_snapshot_decode_allows_empty_legacy_snapshots() {
        assert!(decode_report_run_snapshot("  \n").unwrap().is_none());
        match decode_report_run_snapshot("not a run snapshot") {
            Ok(_) => panic!("malformed report snapshot was accepted"),
            Err(error) => assert!(error.contains("plan snapshot is corrupt")),
        }
        let oversized = "x".repeat(MAX_PROFILE_BYTES as usize + 1);
        let error = decode_report_run_snapshot(&oversized)
            .err()
            .expect("oversized report snapshot must be rejected");
        assert!(error.contains("exceeds"));
    }

    #[test]
    fn certificate_pins_require_sha256_hex() {
        assert!(validate_certificate_pin(&"ab".repeat(32), "source").is_ok());
        assert!(validate_certificate_pin(&"AB".repeat(32), "source").is_ok());
        assert!(validate_certificate_pin("", "source").is_ok());
        assert!(validate_certificate_pin("not-a-pin", "source").is_err());
        assert!(validate_certificate_pin(&"g".repeat(64), "source").is_err());
    }

    #[test]
    fn credential_free_form_clone_preserves_plan_defaults() {
        let base = Form {
            source_password: "source-secret".into(),
            destination_password: "destination-secret".into(),
            dry_run: false,
            ..Form::default()
        };

        let clone = base.clone_without_credentials();

        assert_eq!(clone.profile.name, base.profile.name);
        assert_eq!(clone.profile.source_tls, base.profile.source_tls);
        assert_eq!(clone.profile.destination_tls, base.profile.destination_tls);
        assert!(!clone.dry_run);
        assert!(clone.source_password.is_empty());
        assert!(clone.destination_password.is_empty());
    }

    #[test]
    fn profile_rust_default_matches_serde_runtime_defaults() {
        let profile = crate::migration_plan::profile::Profile::default();
        assert_eq!(profile.batch_concurrency, 2);
        assert!(!profile.automap);
        assert!(profile.sync_internaldates);
        assert_eq!(profile.source_tls, "imaps");
        assert_eq!(profile.destination_tls, "imaps");
        assert_eq!(profile.source_auth, "password");
        assert_eq!(profile.destination_auth, "password");
    }

    #[test]
    fn microsoft_365_imap_rejects_password_authentication() {
        let mut form = Form::default();
        form.profile.source_host = "outlook.office365.com".into();
        form.profile.source_user = "user@example.com".into();
        form.profile.destination_host = "imap.example.com".into();
        form.profile.destination_user = "user@example.com".into();
        form.profile.source_auth = "password".into();
        let error = form.validate_internal(false).unwrap_err();
        assert!(error.contains("requires OAuth 2.0 / Modern Authentication"));
    }

    #[test]
    fn microsoft_365_admission_normalizes_host_port_case_and_trailing_dot() {
        for host in [
            "outlook.office365.com:993",
            "OUTLOOK.OFFICE365.COM.:993",
            "mail.outlook.office365.com:993",
            "exchange.microsoft.com",
            "autodiscover.exchange.microsoft.com:993",
        ] {
            let mut form = Form::default();
            form.profile.source_host = host.into();
            form.profile.source_user = "user@example.com".into();
            form.profile.destination_host = "imap.example.com".into();
            form.profile.destination_user = "user@example.com".into();
            form.profile.source_auth = "password".into();

            let error = form.validate_internal(false).unwrap_err();
            assert!(
                error.contains("requires OAuth 2.0 / Modern Authentication"),
                "expected Microsoft 365 policy rejection for {host:?}, got: {error}"
            );
        }
    }

    #[test]
    fn microsoft_365_admission_does_not_match_deceptive_domain_suffixes() {
        let mut form = Form::default();
        form.profile.source_host = "exchange.microsoft.com.attacker.example".into();
        form.profile.source_user = "user@example.com".into();
        form.profile.destination_host = "imap.example.com".into();
        form.profile.destination_user = "user@example.com".into();
        form.profile.source_auth = "password".into();

        assert!(form.validate_internal(false).is_ok());
    }

    #[test]
    fn microsoft_365_admission_applies_to_destination_with_embedded_port() {
        let mut form = Form::default();
        form.profile.source_host = "imap.example.com".into();
        form.profile.source_user = "user@example.com".into();
        form.profile.destination_host = "outlook.office365.com:993".into();
        form.profile.destination_user = "user@example.com".into();
        form.profile.destination_auth = "password".into();

        let error = form.validate_internal(false).unwrap_err();
        assert!(error.contains("Destination Microsoft 365 IMAP requires OAuth"));
    }

    #[test]
    fn oauth_refresh_is_a_no_op_for_password_authentication() {
        let mut form = Form::default();
        form.profile.source_auth = "password".into();
        form.profile.source_oauth_refresh_credential_id = "some-id".into();
        assert!(matches!(
            form.refresh_oauth_access_token(true).unwrap(),
            OAuthRefreshOutcome::NotConfigured
        ));
    }

    #[test]
    fn oauth_refresh_is_a_no_op_without_a_configured_refresh_credential_id() {
        let mut form = Form::default();
        form.profile.source_auth = "oauth2".into();
        form.profile.source_oauth_refresh_credential_id = "   ".into();
        assert!(matches!(
            form.refresh_oauth_access_token(true).unwrap(),
            OAuthRefreshOutcome::NotConfigured
        ));
    }

    #[test]
    fn rotated_refresh_persistence_retries_an_injected_first_keyring_failure() {
        let config = OAuthRefreshConfig {
            token_endpoint: "https://oauth.example.test/token".into(),
            client_id: "client".into(),
            client_secret: SecretString::default(),
            refresh_token: SecretString::from("rotated-refresh-token"),
        };
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&attempts);
        persist_rotated_refresh_config_with_retry(&config, |_config| {
            let attempt = observed.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                Err("injected keyring write failure".into())
            } else {
                Ok(())
            }
        })
        .expect("a transient keyring failure should be retried");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
