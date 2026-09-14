use crate::atomic_artifact::write_private_atomic;
use crate::command::{remove_option, shell_quote};
use crate::imap_probe::{command_endpoint_parts, command_port};
use crate::{
    DOVECOT_SYNC_LOCK_WAIT_SECONDS, RunPlanSnapshot, RunProfileSnapshot, core,
    create_secret_directory,
    credentials::{self, SecretString},
    endpoint, engine,
    plan_identity::{
        configured_file_content_identity, executable_content_identity, snapshot_sha256,
    },
    restrict_directory_permissions, validate_certificate_pin, write_secret_file,
};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Profile {
    pub(crate) name: String,
    pub(crate) source_host: String,
    #[serde(default)]
    pub(crate) source_port: String,
    #[serde(default = "default_source_tls")]
    pub(crate) source_tls: String,
    #[serde(default)]
    pub(crate) source_ca_bundle: String,
    #[serde(default)]
    pub(crate) source_certificate_pin_sha256: String,
    /// Explicit operator acknowledgement required before a live cleartext
    /// source connection. This is part of the plan fingerprint.
    #[serde(default)]
    pub(crate) allow_insecure_source_transport: bool,
    pub(crate) source_user: String,
    /// `password` uses LOGIN/password authentication; `oauth2` uses an
    /// operator-supplied OAuth 2.0 access token with XOAUTH2.
    #[serde(default = "default_auth_method")]
    pub(crate) source_auth: String,
    #[serde(default)]
    pub(crate) source_credential_id: String,
    pub(crate) destination_host: String,
    pub(crate) destination_user: String,
    #[serde(default = "default_auth_method")]
    pub(crate) destination_auth: String,
    #[serde(default)]
    pub(crate) destination_credential_id: String,
    #[serde(default)]
    pub(crate) destination_port: String,
    #[serde(default = "default_destination_tls")]
    pub(crate) destination_tls: String,
    #[serde(default)]
    pub(crate) destination_ca_bundle: String,
    #[serde(default)]
    pub(crate) destination_certificate_pin_sha256: String,
    pub(crate) imapsync_path: String,
    #[serde(default)]
    pub(crate) engine: core::Engine,
    #[serde(default = "default_doveadm_path")]
    pub(crate) doveadm_path: String,
    #[serde(default = "default_ssh_path")]
    pub(crate) ssh_path: String,
    /// Where to invoke doveadm. `automatic` preserves legacy hostname-based
    /// inference; new profiles should prefer an explicit location.
    #[serde(default = "default_dovecot_execution")]
    pub(crate) dovecot_execution: String,
    #[serde(default)]
    pub(crate) dovecot_ssh_user: String,
    #[serde(default)]
    pub(crate) dovecot_config: String,
    #[serde(default = "default_batch_concurrency")]
    pub(crate) batch_concurrency: usize,
    #[serde(default)]
    pub(crate) batch_retry_count: usize,
    /// Optional imapsync throttle. Zero means unlimited.
    #[serde(default)]
    pub(crate) max_messages_per_second: u32,
    /// Optional imapsync throttle. Zero means unlimited.
    #[serde(default)]
    pub(crate) max_bytes_per_second: u64,
    /// Maximum runtime for one migration process, in hours.
    #[serde(default = "default_migration_timeout_hours")]
    pub(crate) migration_timeout_hours: u64,
    /// Legacy profile field retained for deserialization compatibility. Remote
    /// Dovecot execution is rejected until a deployment-independent secret
    /// broker is available, so this value has no effect.
    #[serde(default)]
    pub(crate) allow_remote_password_in_argv: bool,
    pub(crate) automap: bool,
    pub(crate) addheader: bool,
    pub(crate) justfolders: bool,
    pub(crate) sync_internaldates: bool,
    pub(crate) useuid: bool,
    pub(crate) usecache: bool,
    pub(crate) fastio1: bool,
    pub(crate) fastio2: bool,
    pub(crate) allowsizemismatch: bool,
    pub(crate) delete2: bool,
    pub(crate) extra_options: String,
}

/// Count only locally provable plan fields. Network authentication and
/// server capability checks belong to the explicit preflight, not this UI
/// completeness indicator.
pub(crate) fn completeness(profile: &Profile) -> (usize, usize) {
    let checks = 4;
    let passed = [
        !profile.source_host.trim().is_empty(),
        !profile.destination_host.trim().is_empty(),
        !profile.source_user.trim().is_empty(),
        !profile.destination_user.trim().is_empty(),
    ]
    .into_iter()
    .filter(|ok| *ok)
    .count();
    (passed, checks)
}
pub(crate) fn default_doveadm_path() -> String {
    "doveadm".into()
}
pub(crate) fn default_ssh_path() -> String {
    "ssh".into()
}
pub(crate) fn default_dovecot_execution() -> String {
    "automatic".into()
}
pub(crate) fn default_batch_concurrency() -> usize {
    2
}
pub(crate) fn default_migration_timeout_hours() -> u64 {
    24
}
pub(crate) fn default_source_tls() -> String {
    "imaps".into()
}

pub(crate) fn default_auth_method() -> String {
    "password".into()
}

pub(crate) fn auth_method_is_oauth(method: &str) -> bool {
    method == "oauth2"
}

pub(crate) fn default_destination_tls() -> String {
    "imaps".into()
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
                ssh_path: default_ssh_path(),
                dovecot_execution: default_dovecot_execution(),
                migration_timeout_hours: default_migration_timeout_hours(),
                automap: true,
                ..Default::default()
            },
            source_password: SecretString::default(),
            destination_password: SecretString::default(),
            dry_run: true,
        }
    }
}
impl Form {
    pub(crate) const KEYRING_SERVICE: &'static str = "com.mailswiftsync.mailbox";

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
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => Some((path, text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::read_to_string(&legacy) {
                    Ok(text) => Some((legacy, text)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(format!("could not read legacy profile: {error}")),
                }
            }
            Err(error) => return Err(format!("could not read profile: {error}")),
        };
        if let Some((path, text)) = text {
            form.profile = toml::from_str(&text).map_err(|error| {
                format!("could not decode saved profile {}: {error}", path.display())
            })?;
        }
        Ok(form)
    }
    pub(crate) fn save(&self) -> Result<(), String> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            restrict_directory_permissions(parent).map_err(|e| e.to_string())?;
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
        let password = SecretString::new(entry.get_password().map_err(|error| {
            format!("Could not load the credential from the OS keyring: {error}")
        })?);
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
    /// value when a credential reference is authoritative.
    pub(crate) fn reload_configured_keyring_credentials(&mut self) -> Result<(), String> {
        if !self.profile.source_credential_id.trim().is_empty() {
            self.load_keyring_password(true)?;
        }
        if !self.profile.destination_credential_id.trim().is_empty() {
            self.load_keyring_password(false)?;
        }
        Ok(())
    }

    /// A process-local comparison value for the credential material used by
    /// a dry preflight. It is deliberately never persisted or included in a
    /// plan snapshot; it only detects a session/keyring change before live
    /// promotion and forces a fresh dry preflight.
    pub(crate) fn credential_fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.source_password.as_bytes());
        digest.update([0]);
        digest.update(self.destination_password.as_bytes());
        format!("{:x}", digest.finalize())
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
        if require_credentials {
            required.push((
                if auth_method_is_oauth(&self.profile.source_auth) {
                    "Source OAuth 2.0 access token"
                } else {
                    "Source password"
                },
                self.source_password.as_str(),
            ));
        }
        if require_credentials && self.engine() != core::Engine::Dovecot {
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
        if !matches!(
            self.profile.dovecot_execution.as_str(),
            "automatic" | "local" | "ssh"
        ) {
            return Err("Dovecot execution must be automatic, local, or ssh.".into());
        }
        if self.engine() == core::Engine::Dovecot && !self.local_doveadm() {
            for (label, value) in [
                ("Dovecot SSH host", self.profile.destination_host.as_str()),
                (
                    "Dovecot SSH username",
                    self.profile.dovecot_ssh_user.as_str(),
                ),
            ] {
                if (!value.is_empty() && value.starts_with('-'))
                    || value.chars().any(char::is_whitespace)
                {
                    return Err(format!(
                        "{label} cannot begin with '-' or contain whitespace."
                    ));
                }
            }
            return Err("Remote Dovecot execution is not available: the current SSH compatibility path would expose the source password to destination-host process inspection. Use local doveadm or imapsync until a secret broker is implemented.".into());
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
        engine::imapsync_args(
            &self.profile,
            self.source_password.as_str(),
            self.destination_password.as_str(),
            dry_run,
            redact,
            throttle_divisor,
        )
    }
    pub(crate) fn extra_options_valid(&self) -> Result<(), String> {
        engine::validate_extra_options(&self.profile.extra_options)
    }
    #[cfg(test)]
    pub(crate) fn prepared_command(&self) -> Result<PreparedCommand, String> {
        self.prepared_command_with_throttle_divisor(1)
    }
    #[cfg(test)]
    pub(crate) fn prepared_command_with_throttle_divisor(
        &self,
        throttle_divisor: usize,
    ) -> Result<PreparedCommand, String> {
        self.prepared_command_with_throttle_divisor_and_checkpoint(throttle_divisor, None)
    }
    pub(crate) fn prepared_command_with_throttle_divisor_and_checkpoint(
        &self,
        throttle_divisor: usize,
        checkpoint: Option<&str>,
    ) -> Result<PreparedCommand, String> {
        if self.engine() == core::Engine::Dovecot {
            if !self.local_doveadm() {
                return Err("Remote Dovecot execution is not available: the current SSH compatibility path would expose the source password to destination-host process inspection. Use local doveadm or imapsync until a secret broker is implemented.".into());
            }
            let (executable, args) = self.command_with_checkpoint(false, checkpoint);
            let env = if self.local_doveadm() {
                vec![(
                    "MAILSWIFTSYNC_IMAPC_PASSWORD".into(),
                    self.source_password.clone(),
                )]
            } else {
                Vec::new()
            };
            return Ok(PreparedCommand {
                executable,
                args,
                cleanup: Vec::new(),
                env,
            });
        }
        let mut args = self.args_with_throttle_divisor(false, throttle_divisor);
        remove_option(&mut args, "--password1");
        remove_option(&mut args, "--password2");
        remove_option(&mut args, "--oauthaccesstoken1");
        remove_option(&mut args, "--oauthaccesstoken2");
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
            "{}\n{}\ncredential-source1={}\ncredential-source2={}\nsource-auth={}\ndestination-auth={}\ninsecure-source-transport-ack={}\nsource-ca-bundle={}\nsource-ca-bundle-sha256={}\ndestination-ca-bundle={}\ndestination-ca-bundle-sha256={}\nsource-certificate-pin={}\ndestination-certificate-pin={}\nexecution-executable-sha256={}\ndovecot-config-sha256={}",
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
        let extra_options_sha256 = format!(
            "{:x}",
            Sha256::digest(self.profile.extra_options.as_bytes())
        );
        let profile = &self.profile;
        let execution_executable = match self.engine() {
            core::Engine::Dovecot if self.local_doveadm() => &profile.doveadm_path,
            core::Engine::Dovecot => &profile.ssh_path,
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
                destination_host: profile.destination_host.clone(),
                destination_user: profile.destination_user.clone(),
                destination_auth: profile.destination_auth.clone(),
                destination_credential_id: profile.destination_credential_id.clone(),
                destination_port: profile.destination_port.clone(),
                destination_tls: profile.destination_tls.clone(),
                destination_ca_bundle: profile.destination_ca_bundle.clone(),
                destination_certificate_pin_sha256: profile
                    .destination_certificate_pin_sha256
                    .clone(),
                imapsync_path: profile.imapsync_path.clone(),
                engine: profile.engine,
                doveadm_path: profile.doveadm_path.clone(),
                ssh_path: profile.ssh_path.clone(),
                dovecot_execution: profile.dovecot_execution.clone(),
                dovecot_ssh_user: profile.dovecot_ssh_user.clone(),
                dovecot_config: profile.dovecot_config.clone(),
                batch_concurrency: profile.batch_concurrency,
                batch_retry_count: profile.batch_retry_count,
                max_messages_per_second: profile.max_messages_per_second,
                max_bytes_per_second: profile.max_bytes_per_second,
                migration_timeout_hours: profile.migration_timeout_hours,
                allow_remote_password_in_argv: profile.allow_remote_password_in_argv,
                automap: profile.automap,
                addheader: profile.addheader,
                justfolders: profile.justfolders,
                sync_internaldates: profile.sync_internaldates,
                useuid: profile.useuid,
                usecache: profile.usecache,
                fastio1: profile.fastio1,
                fastio2: profile.fastio2,
                allowsizemismatch: profile.allowsizemismatch,
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
        if self.engine() != core::Engine::Dovecot {
            return (
                self.profile.imapsync_path.clone(),
                self.args_with_throttle_divisor_and_mode(redact, 1, dry_run),
            );
        }
        let password = if self.local_doveadm() {
            "$ENV:MAILSWIFTSYNC_IMAPC_PASSWORD"
        } else if redact {
            "••••••••"
        } else {
            self.source_password.as_str()
        };
        let source_default_port = default_imap_port(&self.profile.source_tls);
        let (source_host, endpoint_port) =
            command_endpoint_parts(&self.profile.source_host, source_default_port);
        let source_port = command_port(&self.profile.source_port, endpoint_port);
        let mut args = Vec::new();
        if self.local_doveadm() {
            args.push("-k".into());
        }
        if !self.profile.dovecot_config.trim().is_empty() {
            args.extend(["-c".into(), self.profile.dovecot_config.clone()]);
        }
        args.extend([
            "-o".into(),
            format!("imapc_host={source_host}"),
            "-o".into(),
            format!("imapc_ssl={}", dovecot_ssl_mode(&self.profile.source_tls)),
            "-o".into(),
            format!("imapc_user={}", self.profile.source_user),
            "-o".into(),
            format!("imapc_password={password}"),
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
            args.extend([
                "-o".into(),
                "mail_driver=imapc".into(),
                "-o".into(),
                "mail_path=".into(),
                "mailbox".into(),
                "list".into(),
                "-u".into(),
                self.profile.source_user.clone(),
            ]);
        } else {
            args.extend(["-l".into(), DOVECOT_SYNC_LOCK_WAIT_SECONDS.to_string()]);
            // Dovecot prints a new state string when -s is supplied. An
            // empty state requests an initial stateful pass; a prior
            // committed checkpoint makes later passes incremental.
            args.extend(["-s".into(), checkpoint.unwrap_or_default().to_owned()]);
            args.extend([if self.profile.delete2 {
                "backup"
            } else {
                "sync"
            }
            .into()]);
            if !self.profile.delete2 {
                args.push("-1".into());
            }
            args.extend([
                "-Ru".into(),
                self.profile.destination_user.clone(),
                "imapc:".into(),
            ]);
        }
        self.wrap_dovecot(args)
    }
    pub(crate) fn wrap_dovecot(&self, args: Vec<String>) -> (String, Vec<String>) {
        if self.local_doveadm() {
            (self.profile.doveadm_path.clone(), args)
        } else {
            let target = if self.profile.dovecot_ssh_user.trim().is_empty() {
                self.profile.destination_host.clone()
            } else {
                format!(
                    "{}@{}",
                    self.profile.dovecot_ssh_user, self.profile.destination_host
                )
            };
            let mut command_parts = vec![self.profile.doveadm_path.clone()];
            command_parts.extend(args);
            let ssh_args = vec![
                "-o".into(),
                "BatchMode=yes".into(),
                target,
                command_parts
                    .iter()
                    .map(|argument| shell_quote(argument))
                    .collect::<Vec<_>>()
                    .join(" "),
            ];
            (self.profile.ssh_path.clone(), ssh_args)
        }
    }
    pub(crate) fn local_doveadm(&self) -> bool {
        match self.profile.dovecot_execution.as_str() {
            "local" => true,
            "ssh" => false,
            _ => {
                self.profile.dovecot_ssh_user.trim().is_empty()
                    && ["localhost", "127.0.0.1", "::1"]
                        .contains(&self.profile.destination_host.trim())
            }
        }
    }
    pub(crate) fn dovecot_verification_commands(&self, redact: bool) -> Vec<(String, Vec<String>)> {
        if self.engine() != core::Engine::Dovecot {
            return Vec::new();
        }
        let password = if self.local_doveadm() {
            "$ENV:MAILSWIFTSYNC_IMAPC_PASSWORD"
        } else if redact {
            "••••••••"
        } else {
            self.source_password.as_str()
        };
        let source_default_port = default_imap_port(&self.profile.source_tls);
        let (source_host, endpoint_port) =
            command_endpoint_parts(&self.profile.source_host, source_default_port);
        let source_port = command_port(&self.profile.source_port, endpoint_port);
        let mut source = Vec::new();
        if self.local_doveadm() {
            source.push("-k".into());
        }
        if !self.profile.dovecot_config.trim().is_empty() {
            source.extend(["-c".into(), self.profile.dovecot_config.clone()]);
        }
        source.extend([
            "-o".into(),
            format!("imapc_host={source_host}"),
            "-o".into(),
            format!("imapc_ssl={}", dovecot_ssl_mode(&self.profile.source_tls)),
            "-o".into(),
            format!("imapc_user={}", self.profile.source_user),
            "-o".into(),
            format!("imapc_password={password}"),
            "-o".into(),
            "mail_driver=imapc".into(),
            "-o".into(),
            "mail_path=".into(),
            "mailbox".into(),
            "status".into(),
            "-u".into(),
            self.profile.source_user.clone(),
            "-t".into(),
            "messages,vsize".into(),
            "*".into(),
        ]);
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
        let mut destination = Vec::new();
        if !self.profile.dovecot_config.trim().is_empty() {
            destination.extend(["-c".into(), self.profile.dovecot_config.clone()]);
        }
        destination.extend([
            "mailbox".into(),
            "status".into(),
            "-u".into(),
            self.profile.destination_user.clone(),
            "-t".into(),
            "messages,vsize".into(),
            "*".into(),
        ]);
        vec![self.wrap_dovecot(source), self.wrap_dovecot(destination)]
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
        let mut mailboxes = Vec::new();
        if !self.profile.dovecot_config.trim().is_empty() {
            mailboxes.extend(["-c".into(), self.profile.dovecot_config.clone()]);
        }
        mailboxes.extend([
            "mailbox".into(),
            "list".into(),
            "-u".into(),
            self.profile.destination_user.clone(),
        ]);
        vec![self.wrap_dovecot(user), self.wrap_dovecot(mailboxes)]
    }
}

pub(crate) struct PreparedCommand {
    pub(crate) executable: String,
    pub(crate) args: Vec<String>,
    pub(crate) cleanup: Vec<PathBuf>,
    pub(crate) env: Vec<(String, credentials::SecretString)>,
}
