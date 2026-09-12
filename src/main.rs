#[allow(dead_code)] // The control-plane API is consumed by the next orchestration UI layer.
mod core;
mod credentials;
mod engine;
mod process;
mod verification;

use credentials::{
    CleanupGuard, cleanup_paths, cleanup_stale_secret_directories, create_secret_directory,
    restrict_directory_permissions, restrict_file_permissions, secret_runtime_base,
    write_secret_file,
};
use process::{
    InstanceLock, ProcessLaunchLimiter, acquire_instance_lock, collect_redacted_lines,
    configure_process_group, for_each_lossy_line, linux_process_identity, recorded_process_matches,
    terminate_recorded_process_group, wait_with_timeout,
};

use calamine::{Reader, open_workbook_auto};
use eframe::{
    egui,
    egui::{Color32, RichText, Stroke},
};
use keyring::Entry;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Display;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::Duration,
};
use zeroize::Zeroizing;

const NAVY: Color32 = Color32::from_rgb(17, 26, 43);
const BLUE: Color32 = Color32::from_rgb(45, 113, 205);
const TEAL: Color32 = Color32::from_rgb(24, 158, 166);
const SKY: Color32 = Color32::from_rgb(235, 243, 252);
const MUTED: Color32 = Color32::from_rgb(103, 119, 139);
const ALERT: Color32 = Color32::from_rgb(193, 74, 61);
const MAX_VISIBLE_OUTPUT_LINES: usize = 10_000;
const BATCH_PROCESS_STARTS_PER_SECOND: usize = 2;
const DOVECOT_SYNC_LOCK_WAIT_SECONDS: u64 = 300;
const MAX_PENDING_EVENTS: usize = 4_096;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Profile {
    name: String,
    source_host: String,
    #[serde(default)]
    source_port: String,
    #[serde(default = "default_source_tls")]
    source_tls: String,
    /// Explicit operator acknowledgement required before a live cleartext
    /// source connection. This is part of the plan fingerprint.
    #[serde(default)]
    allow_insecure_source_transport: bool,
    source_user: String,
    #[serde(default)]
    source_credential_id: String,
    destination_host: String,
    destination_user: String,
    #[serde(default)]
    destination_credential_id: String,
    #[serde(default)]
    destination_port: String,
    #[serde(default = "default_destination_tls")]
    destination_tls: String,
    imapsync_path: String,
    #[serde(default)]
    engine: core::Engine,
    #[serde(default = "default_doveadm_path")]
    doveadm_path: String,
    #[serde(default = "default_ssh_path")]
    ssh_path: String,
    /// Where to invoke doveadm. `automatic` preserves legacy hostname-based
    /// inference; new profiles should prefer an explicit location.
    #[serde(default = "default_dovecot_execution")]
    dovecot_execution: String,
    #[serde(default)]
    dovecot_ssh_user: String,
    #[serde(default)]
    dovecot_config: String,
    #[serde(default = "default_batch_concurrency")]
    batch_concurrency: usize,
    #[serde(default)]
    batch_retry_count: usize,
    /// Optional imapsync throttle. Zero means unlimited.
    #[serde(default)]
    max_messages_per_second: u32,
    /// Optional imapsync throttle. Zero means unlimited.
    #[serde(default)]
    max_bytes_per_second: u64,
    /// Maximum runtime for one migration process, in hours.
    #[serde(default = "default_migration_timeout_hours")]
    migration_timeout_hours: u64,
    /// Remote Dovecot currently receives this value in a destination-side
    /// command override. Keep the unsafe compatibility path opt-in until a
    /// deployment-independent secret broker is available.
    #[serde(default)]
    allow_remote_password_in_argv: bool,
    automap: bool,
    addheader: bool,
    justfolders: bool,
    sync_internaldates: bool,
    useuid: bool,
    usecache: bool,
    fastio1: bool,
    fastio2: bool,
    allowsizemismatch: bool,
    delete2: bool,
    extra_options: String,
}

/// The durable run snapshot deliberately does not serialize `Profile`.
/// Operator-supplied extra options are retained only as a digest so a
/// password or token embedded in an expert option cannot enter SQLite or an
/// exported report.
#[derive(Serialize)]
struct RunPlanSnapshot {
    dry_run: bool,
    profile: RunProfileSnapshot,
}

#[derive(Serialize)]
struct RunProfileSnapshot {
    name: String,
    source_host: String,
    source_port: String,
    source_tls: String,
    allow_insecure_source_transport: bool,
    source_user: String,
    source_credential_id: String,
    destination_host: String,
    destination_user: String,
    destination_credential_id: String,
    destination_port: String,
    destination_tls: String,
    imapsync_path: String,
    engine: core::Engine,
    doveadm_path: String,
    ssh_path: String,
    dovecot_execution: String,
    dovecot_ssh_user: String,
    dovecot_config: String,
    batch_concurrency: usize,
    batch_retry_count: usize,
    max_messages_per_second: u32,
    max_bytes_per_second: u64,
    migration_timeout_hours: u64,
    allow_remote_password_in_argv: bool,
    automap: bool,
    addheader: bool,
    justfolders: bool,
    sync_internaldates: bool,
    useuid: bool,
    usecache: bool,
    fastio1: bool,
    fastio2: bool,
    allowsizemismatch: bool,
    delete2: bool,
    extra_options_sha256: String,
}

fn default_doveadm_path() -> String {
    "doveadm".into()
}
fn default_ssh_path() -> String {
    "ssh".into()
}
fn default_dovecot_execution() -> String {
    "automatic".into()
}
fn default_batch_concurrency() -> usize {
    2
}
fn default_migration_timeout_hours() -> u64 {
    24
}
fn default_source_tls() -> String {
    "imaps".into()
}

fn default_destination_tls() -> String {
    "imaps".into()
}

fn effective_destination_tls(mode: &str) -> &str {
    if mode == "starttls" {
        "starttls"
    } else {
        "imaps"
    }
}

fn plan_snapshot_sha256(snapshot: &str) -> String {
    format!("{:x}", Sha256::digest(snapshot.as_bytes()))
}

fn validate_batch_throttle(profile: &Profile, concurrency: usize) -> Result<(), String> {
    let workers = concurrency.max(1);
    if profile.max_messages_per_second > 0 && profile.max_messages_per_second < workers as u32 {
        return Err(format!(
            "Messages/second target must be at least the batch concurrency ({workers}), or reduce concurrency."
        ));
    }
    if profile.max_bytes_per_second > 0 && profile.max_bytes_per_second < workers as u64 {
        return Err(format!(
            "Bytes/second target must be at least the batch concurrency ({workers}), or reduce concurrency."
        ));
    }
    Ok(())
}

/// Serialize the durable queue configuration without retaining free-form
/// expert options. Restored rows must be re-reviewed against the current
/// trusted application profile; the launch-time run snapshot retains the
/// SHA-256 identity of the options that were actually used.
fn durable_batch_profile_config(profile: &Profile) -> Result<String, String> {
    let mut safe_profile = profile.clone();
    safe_profile.extra_options.clear();
    toml::to_string(&safe_profile)
        .map_err(|error| format!("Could not serialize batch plan: {error}"))
}

fn default_imap_port(tls_mode: &str) -> u16 {
    match tls_mode {
        "starttls" | "plain" => 143,
        _ => 993,
    }
}

fn dovecot_ssl_mode(mode: &str) -> &str {
    match mode {
        "plain" => "no",
        other => other,
    }
}
#[derive(Clone)]
struct Form {
    profile: Profile,
    source_password: Zeroizing<String>,
    destination_password: Zeroizing<String>,
    dry_run: bool,
}
impl Default for Form {
    fn default() -> Self {
        Self {
            profile: Profile {
                name: "New migration".into(),
                imapsync_path: "imapsync".into(),
                source_tls: default_source_tls(),
                destination_tls: default_destination_tls(),
                doveadm_path: default_doveadm_path(),
                ssh_path: default_ssh_path(),
                dovecot_execution: default_dovecot_execution(),
                migration_timeout_hours: default_migration_timeout_hours(),
                automap: true,
                ..Default::default()
            },
            source_password: Zeroizing::new(String::new()),
            destination_password: Zeroizing::new(String::new()),
            dry_run: true,
        }
    }
}
impl Form {
    const KEYRING_SERVICE: &'static str = "com.mailswiftsync.mailbox";

    fn path() -> std::path::PathBuf {
        dirs_next::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("mailswiftsync/profile.toml")
    }
    fn load() -> Self {
        let mut form = Self::default();
        let path = Self::path();
        let legacy = dirs_next::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("sourcecraft-imapsync/profile.toml");
        if let Ok(text) =
            std::fs::read_to_string(&path).or_else(|_| std::fs::read_to_string(legacy))
            && let Ok(profile) = toml::from_str(&text)
        {
            form.profile = profile;
        }
        form
    }
    fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let content = toml::to_string_pretty(&self.profile).map_err(|e| e.to_string())?;
        let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, content).map_err(|e| e.to_string())?;
        restrict_file_permissions(&temporary).map_err(|e| e.to_string())?;
        std::fs::File::open(&temporary)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        if let Err(error) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.to_string());
        }
        sync_directory(path.parent()).map_err(|e| e.to_string())?;
        Ok(())
    }
    fn keyring_entry(&self, source: bool) -> Result<Option<Entry>, String> {
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
    fn store_keyring_password(&self, source: bool) -> Result<(), String> {
        let entry = self
            .keyring_entry(source)?
            .ok_or("Enter a keyring ID before storing a password.")?;
        let password = if source {
            self.source_password.as_str()
        } else {
            self.destination_password.as_str()
        };
        if password.is_empty() {
            return Err("Enter a password before storing it in the OS keyring.".into());
        }
        entry
            .set_password(password)
            .map_err(|error| format!("Could not store the credential in the OS keyring: {error}"))
    }
    fn load_keyring_password(&mut self, source: bool) -> Result<(), String> {
        let entry = self
            .keyring_entry(source)?
            .ok_or("Enter a keyring ID before loading a password.")?;
        let password = entry.get_password().map_err(|error| {
            format!("Could not load the credential from the OS keyring: {error}")
        })?;
        if source {
            self.source_password = Zeroizing::new(password);
        } else {
            self.destination_password = Zeroizing::new(password);
        }
        Ok(())
    }
    fn delete_keyring_password(&self, source: bool) -> Result<(), String> {
        let entry = self
            .keyring_entry(source)?
            .ok_or("Enter a keyring ID before deleting a password.")?;
        entry
            .delete_credential()
            .map_err(|error| format!("Could not delete the OS keyring credential: {error}"))
    }
    fn load_configured_keyring_credentials(&mut self) -> Result<(), String> {
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
    fn validate(&self) -> Result<(), String> {
        self.validate_internal(true)
    }
    fn validate_for_import(&self) -> Result<(), String> {
        self.validate_internal(false)
    }
    fn validate_internal(&self, require_credentials: bool) -> Result<(), String> {
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
            required.push(("Source password", self.source_password.as_str()));
        }
        if require_credentials && self.engine() != core::Engine::Dovecot {
            required.push(("Destination password", self.destination_password.as_str()));
        }
        let source_port = self.profile.source_port.trim();
        if (!source_port.is_empty() && source_port.parse::<u16>().is_err()) || source_port == "0" {
            return Err("Source IMAP port must be a number between 1 and 65535.".into());
        }
        if !matches!(
            self.profile.source_tls.as_str(),
            "imaps" | "starttls" | "plain"
        ) {
            return Err("Source TLS mode must be imaps, starttls, or plain.".into());
        }
        let destination_port = self.profile.destination_port.trim();
        if (!destination_port.is_empty() && destination_port.parse::<u16>().is_err())
            || destination_port == "0"
        {
            return Err("Destination IMAP port must be a number between 1 and 65535.".into());
        }
        if !matches!(
            self.profile.destination_tls.as_str(),
            "" | "imaps" | "starttls"
        ) {
            return Err("Destination TLS mode must be imaps or starttls.".into());
        }
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
            ("Source password", self.source_password.as_str()),
            ("Destination password", self.destination_password.as_str()),
        ] {
            if !value.is_empty() && value.chars().any(char::is_control) {
                return Err(format!("{label} cannot contain control characters."));
            }
        }
        self.extra_options_valid()?;
        Ok(())
    }
    fn args(&self, redact: bool) -> Vec<String> {
        self.args_with_throttle_divisor(redact, 1)
    }
    fn args_with_throttle_divisor(&self, redact: bool, throttle_divisor: usize) -> Vec<String> {
        engine::imapsync_args(
            &self.profile,
            self.source_password.as_str(),
            self.destination_password.as_str(),
            self.dry_run,
            redact,
            throttle_divisor,
        )
    }
    fn extra_options_valid(&self) -> Result<(), String> {
        engine::validate_extra_options(&self.profile.extra_options)
    }
    fn prepared_command(&self) -> Result<PreparedCommand, String> {
        self.prepared_command_with_throttle_divisor(1)
    }
    fn prepared_command_with_throttle_divisor(
        &self,
        throttle_divisor: usize,
    ) -> Result<PreparedCommand, String> {
        if self.engine() == core::Engine::Dovecot {
            if !self.local_doveadm() && !self.profile.allow_remote_password_in_argv {
                return Err("Remote Dovecot execution is disabled by default because the source password may be visible in the destination command line. Enable the explicit remote-password compatibility acknowledgement only on a trusted destination, or use a secret broker.".into());
            }
            let (executable, args) = self.command(false);
            let env = if self.local_doveadm() {
                vec![(
                    "MAILSWIFTSYNC_IMAPC_PASSWORD".into(),
                    self.source_password.to_string(),
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
        args.extend([
            "--passfile1".into(),
            source_file.to_string_lossy().into_owned(),
            "--passfile2".into(),
            destination_file.to_string_lossy().into_owned(),
        ]);
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
    fn plan_fingerprint(&self) -> String {
        let mut planned = self.clone();
        planned.dry_run = false;
        let (executable, mut args) = planned.command(true);
        if planned.engine() == core::Engine::ImapSync {
            remove_option(&mut args, "--password1");
            remove_option(&mut args, "--password2");
        }
        format!(
            "{}\n{}\ncredential-source1={}\ncredential-source2={}\ninsecure-source-transport-ack={}",
            executable,
            args.join("\u{1f}"),
            planned.profile.source_credential_id.trim(),
            planned.profile.destination_credential_id.trim(),
            planned.profile.allow_insecure_source_transport,
        )
    }

    /// Serialize the launch configuration without session passwords or raw
    /// expert-option values. This is persisted with the run so historical
    /// reports do not depend on the currently edited form.
    fn plan_snapshot(&self) -> String {
        let extra_options_sha256 = format!(
            "{:x}",
            Sha256::digest(self.profile.extra_options.as_bytes())
        );
        let profile = &self.profile;
        let snapshot = RunPlanSnapshot {
            dry_run: self.dry_run,
            profile: RunProfileSnapshot {
                name: profile.name.clone(),
                source_host: profile.source_host.clone(),
                source_port: profile.source_port.clone(),
                source_tls: profile.source_tls.clone(),
                allow_insecure_source_transport: profile.allow_insecure_source_transport,
                source_user: profile.source_user.clone(),
                source_credential_id: profile.source_credential_id.clone(),
                destination_host: profile.destination_host.clone(),
                destination_user: profile.destination_user.clone(),
                destination_credential_id: profile.destination_credential_id.clone(),
                destination_port: profile.destination_port.clone(),
                destination_tls: profile.destination_tls.clone(),
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
            },
        };
        toml::to_string(&snapshot).unwrap_or_default()
    }

    fn requires_insecure_transport_ack(&self) -> bool {
        self.profile.source_tls == "plain" && !self.profile.allow_insecure_source_transport
    }
    fn engine(&self) -> core::Engine {
        match self.profile.engine {
            // The desktop cannot safely infer the destination's mail stack
            // from a hostname or from a locally installed executable.
            // Hostnames are not reliable server fingerprints. Auto is an
            // explicit conservative default, not environment detection.
            core::Engine::Auto => core::Engine::ImapSync,
            selected => selected,
        }
    }
    fn command(&self, redact: bool) -> (String, Vec<String>) {
        if self.engine() != core::Engine::Dovecot {
            return (self.profile.imapsync_path.clone(), self.args(redact));
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
            endpoint_parts(&self.profile.source_host, source_default_port)
                .unwrap_or_else(|_| (self.profile.source_host.clone(), source_default_port));
        let source_port = self
            .profile
            .source_port
            .parse::<u16>()
            .unwrap_or(endpoint_port);
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
        if !self.profile.source_port.trim().is_empty() {
            args.extend([
                "-o".into(),
                format!("imapc_port={}", self.profile.source_port.trim()),
            ]);
        } else {
            args.extend(["-o".into(), format!("imapc_port={source_port}")]);
        }
        if self.dry_run {
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
    fn wrap_dovecot(&self, args: Vec<String>) -> (String, Vec<String>) {
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
    fn local_doveadm(&self) -> bool {
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
    fn dovecot_verification_commands(&self, redact: bool) -> Vec<(String, Vec<String>)> {
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
            endpoint_parts(&self.profile.source_host, source_default_port)
                .unwrap_or_else(|_| (self.profile.source_host.clone(), source_default_port));
        let source_port = self
            .profile
            .source_port
            .parse::<u16>()
            .unwrap_or(endpoint_port);
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

    fn dovecot_destination_preflight_commands(&self) -> Vec<(String, Vec<String>)> {
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

struct PreparedCommand {
    executable: String,
    args: Vec<String>,
    cleanup: Vec<PathBuf>,
    env: Vec<(String, String)>,
}

fn remove_option(args: &mut Vec<String>, option: &str) {
    if let Some(index) = args.iter().position(|arg| arg == option) {
        args.remove(index);
        if index < args.len() {
            args.remove(index);
        }
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "._/@=:-,".contains(character))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn parse_shell_words(input: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut in_token = false;
    for character in input.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            in_token = true;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            in_token = true;
            continue;
        }
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => current.push(character),
            None if character == '\'' || character == '"' => quote = Some(character),
            None if character.is_whitespace() => {
                if in_token {
                    words.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            None => current.push(character),
        }
        if !character.is_whitespace() || quote.is_some() {
            in_token = true;
        }
    }
    if escaped {
        return Err("unfinished escape".into());
    }
    if quote.is_some() {
        return Err("unterminated quote".into());
    }
    if in_token {
        words.push(current);
    }
    Ok(words)
}

enum Event {
    Line(String),
    ProcessStarted(
        String,
        String,
        u32,
        Option<u64>,
        Option<u32>,
        Option<u32>,
        String,
    ),
    ClaimBatch {
        project_id: String,
        job_id: String,
        parent_run_id: String,
        child_run_id: String,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    JobState {
        job_id: String,
        child_run_id: String,
        state: String,
    },
    JobFinished {
        job_id: String,
        child_run_id: String,
        state: String,
        detail: String,
    },
    BatchEvidence {
        job_id: String,
        child_run_id: String,
        evidence: core::MailboxEvidence,
    },
    Evidence(core::MailboxEvidence),
    VerificationFailed(String),
    Finished(Result<StreamOutcome, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamOutcome {
    Completed,
    DeltaRequired,
}

#[derive(Debug)]
struct StreamResult {
    outcome: StreamOutcome,
    imapsync_evidence: Option<core::MailboxEvidence>,
}

// The process runner keeps each security-sensitive input explicit at the call
// site: executable, args, environment, event sink, redaction prefix/secrets,
// cancellation, and operator-selected timeout.
#[allow(clippy::too_many_arguments)]
fn run_streaming(
    executable: &str,
    args: &[String],
    env: &[(String, String)],
    tx: &mpsc::SyncSender<Event>,
    run_id: &str,
    job_id: &str,
    prefix: &str,
    cancel: &AtomicBool,
    secrets: &[String],
    timeout: Duration,
    dovecot_exit_two_is_delta: bool,
) -> Result<StreamResult, String> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {executable}: {error}"))?;
    let identity = linux_process_identity(child.id());
    let (start_ticks, process_group, session_id) = identity
        .map(|(start, group, session)| (Some(start), Some(group), Some(session)))
        .unwrap_or((None, None, None));
    let _ = tx.send(Event::ProcessStarted(
        run_id.to_owned(),
        job_id.to_owned(),
        child.id(),
        start_ticks,
        process_group,
        session_id,
        executable.to_owned(),
    ));
    let stdout = child.stdout.take().ok_or("stdout pipe unavailable")?;
    let stderr = child.stderr.take().ok_or("stderr pipe unavailable")?;
    let out_tx = tx.clone();
    let out_prefix = prefix.to_owned();
    let out_secrets = secrets.to_vec();
    let tail = Arc::new(Mutex::new(VecDeque::with_capacity(200)));
    let evidence_lines = Arc::new(Mutex::new(Vec::new()));
    let out_tail = Arc::clone(&tail);
    let out_evidence_lines = Arc::clone(&evidence_lines);
    let out_thread = thread::spawn(move || {
        for_each_lossy_line(stdout, |line| {
            let mut safe = line;
            for secret in &out_secrets {
                if !secret.is_empty() {
                    safe = safe.replace(secret, "[REDACTED]");
                }
            }
            record_process_tail(&out_tail, &safe);
            record_evidence_line(&out_evidence_lines, &safe);
            let _ = out_tx.send(Event::Line(format!("{out_prefix}{safe}")));
        })
    });
    let err_tx = tx.clone();
    let err_prefix = prefix.to_owned();
    let err_secrets = secrets.to_vec();
    let err_tail = Arc::clone(&tail);
    let err_evidence_lines = Arc::clone(&evidence_lines);
    let err_thread = thread::spawn(move || {
        for_each_lossy_line(stderr, |line| {
            let mut safe = line;
            for secret in &err_secrets {
                if !secret.is_empty() {
                    safe = safe.replace(secret, "[REDACTED]");
                }
            }
            record_process_tail(&err_tail, &safe);
            record_evidence_line(&err_evidence_lines, &safe);
            let _ = err_tx.send(Event::Line(format!("{err_prefix}[stderr] {safe}")));
        })
    });
    let result = wait_with_timeout(&mut child, timeout, cancel)
        .map_err(|error| error.to_string())
        .and_then(|status| {
            if status.success() {
                Ok(StreamOutcome::Completed)
            } else if dovecot_exit_two_is_delta && status.code() == Some(2) {
                Ok(StreamOutcome::DeltaRequired)
            } else {
                Err(format!("process exited with {status}"))
            }
        });
    let stdout_reader = out_thread
        .join()
        .map_err(|_| "stdout reader thread panicked".to_owned());
    let stderr_reader = err_thread
        .join()
        .map_err(|_| "stderr reader thread panicked".to_owned());
    let reader_error = stdout_reader
        .as_ref()
        .err()
        .cloned()
        .or_else(|| stderr_reader.as_ref().err().cloned())
        .or_else(|| {
            stdout_reader
                .as_ref()
                .ok()
                .and_then(|result| result.as_ref().err())
                .map(|error| format!("stdout reader failed: {error}"))
        })
        .or_else(|| {
            stderr_reader
                .as_ref()
                .ok()
                .and_then(|result| result.as_ref().err())
                .map(|error| format!("stderr reader failed: {error}"))
        });
    match result {
        Ok(outcome) if reader_error.is_none() => {
            let imapsync_evidence = evidence_lines
                .lock()
                .ok()
                .and_then(|lines| verification::parse_imapsync_evidence(&lines));
            Ok(StreamResult {
                outcome,
                imapsync_evidence,
            })
        }
        result => {
            let reader_error = reader_error.unwrap_or_default();
            let process_error = match result {
                Ok(_) => String::new(),
                Err(error) => error,
            };
            let error = [process_error, reader_error]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("; ");
            let recent = process_tail_text(&tail);
            if recent.is_empty() {
                Err(error)
            } else {
                Err(format!("{error}; recent output: {recent}"))
            }
        }
    }
}

fn record_evidence_line(lines: &Mutex<Vec<String>>, line: &str) {
    const EVIDENCE_MARKERS: [&str; 7] = [
        "Host1 Nb folders:",
        "Host2 Nb folders:",
        "Host1 Nb messages:",
        "Host2 Nb messages:",
        "Host1 Total size:",
        "Host2 Total size:",
        "Detected ",
    ];
    if (line.contains("The sync looks good") || EVIDENCE_MARKERS.iter().any(|m| line.contains(m)))
        && let Ok(mut lines) = lines.lock()
        && lines.len() < 64
    {
        lines.push(line.to_owned());
    }
}

fn record_process_tail(tail: &Mutex<VecDeque<String>>, line: &str) {
    if let Ok(mut tail) = tail.lock() {
        if tail.len() == 200 {
            tail.pop_front();
        }
        tail.push_back(line.to_owned());
    }
}

fn process_tail_text(tail: &Mutex<VecDeque<String>>) -> String {
    tail.lock()
        .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" | "))
        .unwrap_or_default()
}

fn run_capture_lines(
    executable: &str,
    args: &[String],
    env: &[(String, String)],
    cancel: &AtomicBool,
    secrets: &[String],
    timeout: Duration,
) -> Result<(ExitStatus, Vec<String>), String> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {executable}: {error}"))?;
    let stdout = child.stdout.take().ok_or("stdout pipe unavailable")?;
    let stderr = child.stderr.take().ok_or("stderr pipe unavailable")?;
    let out_secrets = secrets.to_vec();
    let out_thread = thread::spawn(move || collect_redacted_lines(stdout, &out_secrets));
    let err_secrets = secrets.to_vec();
    let err_thread = thread::spawn(move || collect_redacted_lines(stderr, &err_secrets));
    let status = wait_with_timeout(&mut child, timeout, cancel).map_err(|error| error.to_string());
    let stdout_lines = out_thread
        .join()
        .map_err(|_| "stdout reader thread panicked".to_owned())?
        .map_err(|error| format!("stdout reader failed: {error}"));
    let stderr_lines = err_thread
        .join()
        .map_err(|_| "stderr reader thread panicked".to_owned())?
        .map_err(|error| format!("stderr reader failed: {error}"));
    let mut lines = stdout_lines?;
    lines.extend(
        stderr_lines?
            .into_iter()
            .map(|line| format!("[stderr] {line}")),
    );
    let status = status?;
    Ok((status, lines))
}

fn run_dovecot_destination_preflight(
    commands: &[(String, Vec<String>)],
    tx: &mpsc::SyncSender<Event>,
    cancel: &AtomicBool,
    timeout: Duration,
    prefix: &str,
) -> Result<(), String> {
    for (index, (executable, args)) in commands.iter().enumerate() {
        let (status, lines) = run_capture_lines(executable, args, &[], cancel, &[], timeout)?;
        for line in lines {
            let _ = tx.send(Event::Line(format!(
                "{prefix}[destination preflight/{}] {line}",
                index + 1
            )));
        }
        if !status.success() {
            return Err(format!(
                "Dovecot destination preflight command {} exited with {status}",
                index + 1
            ));
        }
    }
    Ok(())
}

fn run_dovecot_verification(
    commands: &[(String, Vec<String>)],
    verification_env: &[(String, String)],
    secrets: &[String],
    tx: &mpsc::SyncSender<Event>,
    cancel: &AtomicBool,
    timeout: Duration,
    prefix: &str,
) -> Result<core::MailboxEvidence, String> {
    let mut reports = Vec::with_capacity(commands.len());
    for (index, (verify_exe, verify_args)) in commands.iter().enumerate() {
        let (status, report) = run_capture_lines(
            verify_exe,
            verify_args,
            if index == 0 { verification_env } else { &[] },
            cancel,
            secrets,
            timeout,
        )?;
        for line in &report {
            let _ = tx.send(Event::Line(format!(
                "{prefix}[verification/{}] {line}",
                index + 1
            )));
        }
        if !status.success() {
            return Err(format!(
                "Dovecot verification command {} exited with {status}",
                index + 1
            ));
        }
        reports.push(report);
    }
    if reports.len() < 2 {
        return Err("Dovecot verification returned incomplete reports".into());
    }
    verification::parse_dovecot_evidence(&reports[0], &reports[1])
        .ok_or_else(|| "Dovecot status output was incomplete".into())
}

#[derive(Clone)]
struct BulkJob {
    label: String,
    form: Form,
    state: String,
}

fn duplicate_bulk_destination(jobs: &[BulkJob]) -> Option<String> {
    let mut destinations = HashSet::new();
    for (index, job) in jobs.iter().enumerate() {
        let profile = &job.form.profile;
        let (destination_host, destination_port) = endpoint_parts(&profile.destination_host, 993)
            .unwrap_or_else(|_| (profile.destination_host.trim().to_owned(), 993));
        let key = format!(
            "{}:{}:{}",
            destination_host.to_ascii_lowercase(),
            destination_port,
            profile.destination_user.to_ascii_lowercase()
        );
        if !destinations.insert(key) {
            return Some(format!(
                "Mailbox {} targets a destination mailbox already used by another batch row; concurrent writes to one mailbox are blocked.",
                index + 1
            ));
        }
    }
    None
}

fn durable_batch_matches_queue(
    stored: &[core::MailboxJob],
    desired: &[(String, String, String)],
) -> bool {
    stored.len() == desired.len()
        && stored.iter().zip(desired.iter()).all(
            |(stored, (source_mailbox, destination_mailbox, config))| {
                stored.source_mailbox == *source_mailbox
                    && stored.destination_mailbox == *destination_mailbox
                    && stored.config.as_deref() == Some(config.as_str())
            },
        )
}

fn apply_keyring_id_to_jobs(jobs: &mut [BulkJob], id: &str, source: bool) -> usize {
    let mut applied = 0;
    for job in jobs {
        let password_empty = if source {
            job.form.source_password.is_empty()
        } else {
            job.form.destination_password.is_empty()
        };
        let credential_id = if source {
            &mut job.form.profile.source_credential_id
        } else {
            &mut job.form.profile.destination_credential_id
        };
        if password_empty && credential_id.trim().is_empty() {
            *credential_id = id.to_owned();
            applied += 1;
        }
    }
    applied
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceView {
    Overview,
    Plan,
    Mailboxes,
    Activity,
    Verification,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Single,
    Batch,
}
#[derive(Clone)]
struct ActiveRunContext {
    run_id: String,
    project_id: String,
    job_id: Option<String>,
    batch_job_ids: Vec<String>,
    batch_plan_fingerprints: Vec<String>,
    batch_child_run_ids: Vec<String>,
    kind: RunKind,
    dry_run: bool,
    engine: core::Engine,
    plan_fingerprint: String,
}
struct App {
    form: Form,
    output: VecDeque<String>,
    receiver: Option<Receiver<Event>>,
    status: String,
    preview: bool,
    bulk_jobs: Vec<BulkJob>,
    bulk_open: bool,
    bulk_message: String,
    advanced_open: bool,
    engine_open: bool,
    store: core::StateStore,
    /// Held for the lifetime of the application. An advisory OS lock is
    /// released automatically if the process crashes, so a later instance
    /// can safely perform orphan recovery without killing a live sibling.
    _instance_lock: Option<InstanceLock>,
    persistence_available: bool,
    project_id: Option<String>,
    job_id: Option<String>,
    run_id: Option<String>,
    active_run: Option<ActiveRunContext>,
    cancel_requested: Option<Arc<AtomicBool>>,
    bulk_project_id: Option<String>,
    bulk_job_ids: Vec<String>,
    cockpit_open: bool,
    preflight: Vec<(String, String, bool)>,
    capability_receiver:
        Option<Receiver<Result<(core::ServerCapabilities, core::ServerCapabilities), String>>>,
    source_capabilities: Option<core::ServerCapabilities>,
    destination_capabilities: Option<core::ServerCapabilities>,
    live_confirm_open: bool,
    live_confirmed: bool,
    durability_error: bool,
    keyring_open: bool,
    active_view: WorkspaceView,
    pending_evidence: Option<core::MailboxEvidence>,
    pending_batch_evidence: HashMap<String, core::MailboxEvidence>,
    run_started_at: Option<std::time::Instant>,
    dark_mode: bool,
    bulk_live_confirm_open: bool,
    bulk_live_confirmed: bool,
    bulk_live_run: bool,
    bulk_source_keyring_apply: String,
    bulk_destination_keyring_apply: String,
}
impl Default for App {
    fn default() -> Self {
        let state_path = dirs_next::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("mailswiftsync/state.db");
        if let Some(parent) = state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
            let _ = restrict_directory_permissions(parent);
        }
        let instance_lock = acquire_instance_lock(&state_path);
        let (store, persistence_warning) = match &instance_lock {
            Ok(_) => match core::StateStore::open(&state_path) {
                Ok(store) => (store, None),
                Err(error) => (
                    core::StateStore::in_memory().expect("SQLite memory store must be available"),
                    Some(format!("Persistent SQLite state unavailable: {error}")),
                ),
            },
            Err(error) => (
                core::StateStore::in_memory().expect("SQLite memory store must be available"),
                Some(format!("Persistent SQLite state unavailable: {error}")),
            ),
        };
        let (recovered, orphaned, unverified_processes) = if persistence_warning.is_none() {
            let processes = store.active_processes().unwrap_or_default();
            let mut unverified = 0;
            for process in &processes {
                if process.pid > 0 && recorded_process_matches(process) {
                    terminate_recorded_process_group(process.pid);
                } else {
                    unverified += 1;
                }
            }
            (
                store.recover_abandoned_jobs().unwrap_or(0),
                processes.len(),
                unverified,
            )
        } else {
            (0, 0, 0)
        };
        // Only the lock owner may reconcile stale runtime secrets. At this
        // point startup recovery has already handled any recorded child
        // process, so an old directory cannot belong to a live application.
        if persistence_warning.is_none() {
            cleanup_stale_secret_directories(&secret_runtime_base());
        }
        let mut initial_output = persistence_warning.clone().map_or_else(
            || vec!["Ready. Start with a dry run against a test destination mailbox.".into()],
            |warning| {
                vec![
                    warning.clone(),
                    "WARNING: this session is not durable.".into(),
                ]
            },
        );
        if orphaned > 0 {
            initial_output.push(format!(
                "Startup found {orphaned} recorded migration process(es); verified identities were terminated before recovery."
            ));
        }
        if unverified_processes > 0 {
            initial_output.push(format!(
                "{unverified_processes} recorded process identity(ies) could not be verified and were not signalled; review the affected jobs before retrying."
            ));
        }
        if recovered > 0 {
            initial_output.push(format!(
                "Recovered {recovered} interrupted job(s) into Attention for review."
            ));
        }
        let initial_output: VecDeque<String> = initial_output.into_iter().collect();
        let mut form = Form::load();
        if form.profile.destination_tls.is_empty() {
            form.profile.destination_tls = default_destination_tls();
        }
        let restored_project = persistence_warning
            .is_none()
            .then(|| store.latest_project().ok().flatten())
            .flatten();
        let (project_id, job_id) = restored_project
            .as_ref()
            .filter(|project| {
                project.source_endpoint == form.profile.source_host
                    && project.destination_endpoint == form.profile.destination_host
            })
            .map(|project| {
                let job_id = store.first_mailbox(&project.id).ok().flatten();
                if let Some(job) = &job_id
                    && let Ok(Some((source_user, destination_user, state))) =
                        store.mailbox_identity(job)
                {
                    // Restore non-secret mailbox identity and make recovery
                    // state visible immediately. Passwords remain blank and
                    // must be entered again before a live run.
                    form.profile.source_user = source_user;
                    form.profile.destination_user = destination_user;
                    if state == "attention" {
                        form.dry_run = true;
                    }
                }
                (Some(project.id.clone()), job_id)
            })
            .unwrap_or((None, None));
        let mut restored_bulk_jobs = Vec::new();
        let mut restored_bulk_job_ids = Vec::new();
        let restored_bulk_project_id = restored_project.as_ref().and_then(|project| {
            if !matches!(
                project.name.as_str(),
                "Batch validation" | "Batch migration"
            ) || project.source_endpoint != "batch"
                || project.destination_endpoint != "batch"
            {
                return None;
            }
            let jobs = store.mailboxes(&project.id).ok()?;
            for job in jobs {
                let profile = job
                    .config
                    .as_deref()
                    .and_then(|config| toml::from_str::<Profile>(config).ok())
                    .unwrap_or_default();
                let mut profile = profile;
                if profile.destination_tls.is_empty() {
                    profile.destination_tls = default_destination_tls();
                }
                restored_bulk_jobs.push(BulkJob {
                    label: format!("{} → {}", job.source_mailbox, job.destination_mailbox),
                    form: Form {
                        profile,
                        source_password: Zeroizing::new(String::new()),
                        destination_password: Zeroizing::new(String::new()),
                        dry_run: true,
                    },
                    state: display_job_state(&job.state).into(),
                });
                restored_bulk_job_ids.push(job.id);
            }
            Some(project.id.clone())
        });
        Self {
            form,
            output: initial_output,
            receiver: None,
            status: persistence_warning.clone().unwrap_or_else(|| "Idle".into()),
            preview: false,
            bulk_jobs: restored_bulk_jobs,
            bulk_open: false,
            bulk_message: if restored_bulk_project_id.is_some() {
                "Restored durable batch queue; credentials must be entered again before validation."
                    .into()
            } else {
                "Import a CSV, XLS, or XLSX file to build a reviewable queue.".into()
            },
            advanced_open: false,
            engine_open: true,
            store,
            _instance_lock: instance_lock.ok(),
            persistence_available: persistence_warning.is_none(),
            project_id,
            job_id,
            run_id: None,
            active_run: None,
            cancel_requested: None,
            bulk_project_id: restored_bulk_project_id,
            bulk_job_ids: restored_bulk_job_ids,
            cockpit_open: false,
            preflight: Vec::new(),
            capability_receiver: None,
            source_capabilities: None,
            destination_capabilities: None,
            live_confirm_open: false,
            live_confirmed: false,
            durability_error: false,
            keyring_open: false,
            active_view: WorkspaceView::Overview,
            pending_evidence: None,
            pending_batch_evidence: HashMap::new(),
            run_started_at: None,
            dark_mode: false,
            bulk_live_confirm_open: false,
            bulk_live_confirmed: false,
            bulk_live_run: false,
            bulk_source_keyring_apply: String::new(),
            bulk_destination_keyring_apply: String::new(),
        }
    }
}

fn format_phase_name(phase: core::Phase) -> &'static str {
    match phase {
        core::Phase::Discovery => "Discovery",
        core::Phase::Preflight => "Preflight",
        core::Phase::Pilot => "Pilot",
        core::Phase::Seed => "Seed",
        core::Phase::CatchUp => "Catch-up",
        core::Phase::FinalDelta => "Final delta",
        core::Phase::Verification => "Verification",
        core::Phase::Complete => "Complete",
        core::Phase::Attention => "Attention",
    }
}

fn format_elapsed(elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs();
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn recommended_next_action(
    phase: core::Phase,
    has_preflight: bool,
    attention_count: usize,
    running: bool,
) -> &'static str {
    if running {
        return "A migration is running — monitor Activity; press Escape to request cancellation.";
    }
    if attention_count > 0 {
        return "Review Attention items before starting another migration.";
    }
    match phase {
        core::Phase::Discovery => {
            "Create the project, then run a dry preflight against a test mailbox."
        }
        core::Phase::Preflight if !has_preflight => {
            "Run the dry preflight and review every blocker before going live."
        }
        core::Phase::Preflight => "Review the preflight, then choose a small pilot mailbox.",
        core::Phase::Pilot => "Review the pilot result and prepare the seed operation.",
        core::Phase::Seed => "Run the seed operation, then schedule a catch-up pass.",
        core::Phase::CatchUp => "Run catch-up during the migration window and review its result.",
        core::Phase::FinalDelta => {
            "Run the final delta, then open Verification for reconciliation."
        }
        core::Phase::Verification => {
            "Review evidence for each mailbox and export the verification report."
        }
        core::Phase::Complete => {
            "The project is complete; export the report and retain the audit record."
        }
        core::Phase::Attention => "Review Attention items before starting another migration.",
    }
}

fn durable_single_identity_matches(
    project: &core::Project,
    mailbox: &core::MailboxJob,
    profile: &Profile,
) -> bool {
    project.source_endpoint == profile.source_host
        && project.destination_endpoint == profile.destination_host
        && mailbox.source_mailbox == profile.source_user
        && mailbox.destination_mailbox == profile.destination_user
}

fn endpoint_parts(input: &str, default_port: u16) -> Result<(String, u16), String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("empty endpoint".into());
    }
    if let Some(rest) = input.strip_prefix('[') {
        let end = rest.find(']').ok_or("IPv6 endpoint is missing ]")?;
        let host = rest[..end].to_owned();
        if host.is_empty() {
            return Err("IPv6 endpoint has an empty host".into());
        }
        let suffix = &rest[end + 1..];
        if !suffix.is_empty() && !suffix.starts_with(':') {
            return Err("invalid characters after IPv6 endpoint".into());
        }
        let port = suffix
            .strip_prefix(':')
            .map(|value| value.parse::<u16>())
            .transpose()
            .map_err(|_| "invalid endpoint port".to_owned())?
            .unwrap_or(default_port);
        if port == 0 {
            return Err("endpoint port must be between 1 and 65535".into());
        }
        return Ok((host, port));
    }
    if input.matches(':').count() == 1
        && let Some((host, port)) = input.rsplit_once(':')
        && let Ok(port) = port.parse::<u16>()
    {
        if host.is_empty() {
            return Err("endpoint has an empty host".into());
        }
        if port == 0 {
            return Err("endpoint port must be between 1 and 65535".into());
        }
        return Ok((host.to_owned(), port));
    }
    Ok((input.to_owned(), default_port))
}

fn imap_quote(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err("IMAP quoted value cannot contain control characters".into());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn read_imap_tagged(
    stream: &mut StreamOwned<ClientConnection, TcpStream>,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
) -> Result<(), String> {
    loop {
        let count = stream.read(buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err(format!("IMAP connection closed before {tag} completed"));
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..count]));
        if response
            .lines()
            .any(|line| line.starts_with(&format!("{tag} ")))
        {
            return Ok(());
        }
        if response.len() > 1_048_576 {
            return Err("IMAP preflight response exceeded 1 MiB".into());
        }
    }
}

fn imap_command_succeeded(response: &str, tag: &str) -> bool {
    response
        .lines()
        .any(|line| line.starts_with(&format!("{tag} OK")))
}

fn probe_tls_capabilities(
    host: &str,
    user: &str,
    password: &str,
) -> Result<core::ServerCapabilities, String> {
    let (server_name, port) =
        endpoint_parts(host, 993).map_err(|error| format!("Invalid IMAP host {host}: {error}"))?;
    let address = if server_name.contains(':') {
        format!("[{server_name}]:{port}")
    } else {
        format!("{server_name}:{port}")
    };
    let sockets = address
        .to_socket_addrs()
        .map_err(|error| format!("{host}: {error}"))?
        .collect::<Vec<_>>();
    if sockets.is_empty() {
        return Err(format!("{host}: no address found"));
    }
    let mut last_error = None;
    let mut tcp = None;
    for socket in sockets {
        match TcpStream::connect_timeout(&socket, Duration::from_secs(8)) {
            Ok(stream) => {
                tcp = Some(stream);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let tcp = tcp.ok_or_else(|| {
        format!(
            "{host}: could not connect to any resolved address: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown connection error".into())
        )
    })?;
    tcp.set_read_timeout(Some(Duration::from_secs(8)))
        .map_err(|e| e.to_string())?;
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from(server_name.to_owned())
        .map_err(|e| format!("{host}: invalid TLS server name: {e}"))?;
    let connection = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| format!("{host}: TLS configuration failed: {e}"))?;
    let mut stream = StreamOwned::new(connection, tcp);
    // IMAP requires the server greeting before the client sends a command.
    // Keep the greeting in the response so capability parsing can also use a
    // PREAUTH greeting when a provider advertises capabilities there.
    let mut response = String::new();
    let mut buffer = [0; 4096];
    loop {
        let count = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..count]));
        if response.contains("\r\n") || response.len() > 65_536 {
            break;
        }
    }
    let greeting = response.to_ascii_uppercase();
    if !greeting.contains("* OK") && !greeting.contains("* PREAUTH") {
        return Err(format!("{host}: server greeting was missing or invalid"));
    }
    stream
        .write_all(b"a001 CAPABILITY\r\n")
        .map_err(|e| e.to_string())?;
    read_imap_tagged(&mut stream, "a001", &mut response, &mut buffer)?;
    if !imap_command_succeeded(&response, "a001") {
        return Err(format!("{host}: pre-auth CAPABILITY failed"));
    }
    let preauth = greeting.contains("* PREAUTH");
    if !preauth {
        let login = format!(
            "a002 LOGIN {} {}\r\n",
            imap_quote(user)?,
            imap_quote(password)?
        );
        stream
            .write_all(login.as_bytes())
            .map_err(|e| e.to_string())?;
        read_imap_tagged(&mut stream, "a002", &mut response, &mut buffer)?;
        if !imap_command_succeeded(&response, "a002") {
            return Err(format!("{host}: IMAP authentication failed"));
        }
    }
    // RFC 9051 permits capabilities to change after authentication, so the
    // post-auth response is the one used for readiness decisions.
    stream
        .write_all(b"a003 CAPABILITY\r\n")
        .map_err(|e| e.to_string())?;
    let mut post_auth_response = String::new();
    read_imap_tagged(&mut stream, "a003", &mut post_auth_response, &mut buffer)?;
    if !imap_command_succeeded(&post_auth_response, "a003") {
        return Err(format!("{host}: post-auth CAPABILITY failed"));
    }
    stream
        .write_all(b"a004 NAMESPACE\r\n")
        .map_err(|e| e.to_string())?;
    let mut _namespace_response = String::new();
    read_imap_tagged(&mut stream, "a004", &mut _namespace_response, &mut buffer)?;
    // NAMESPACE is useful for mapping, but not required by IMAP or by every
    // usable migration endpoint. LIST remains the authoritative inventory
    // gate; callers may surface this response as a compatibility warning.
    stream
        .write_all(b"a005 LIST \"\" \"*\"\r\n")
        .map_err(|e| e.to_string())?;
    let mut list_response = String::new();
    read_imap_tagged(&mut stream, "a005", &mut list_response, &mut buffer)?;
    if !imap_command_succeeded(&list_response, "a005") {
        return Err(format!("{host}: folder inventory failed"));
    }
    let _ = stream.write_all(b"a006 LOGOUT\r\n");
    let caps = core::ServerCapabilities::parse(&post_auth_response);
    if caps.values.is_empty() {
        return Err(format!(
            "{host}: server did not return a CAPABILITY response"
        ));
    }
    Ok(caps)
}

impl App {
    fn active_project_id(&self) -> Option<&str> {
        self.project_id
            .as_deref()
            .or(self.bulk_project_id.as_deref())
    }

    fn start_capability_probe(&mut self) {
        if let Err(error) = self.form.load_configured_keyring_credentials() {
            self.status = error;
            return;
        }
        if let Err(error) = self.form.validate() {
            self.status = format!("Preflight input is invalid: {error}");
            return;
        }
        if self.form.engine() == core::Engine::Dovecot {
            self.status = "Authenticated dual-endpoint IMAPS probing is for imapsync mode; Dovecot destination readiness is checked by the native dry preflight.".into();
            return;
        }
        if self.form.profile.source_tls != "imaps" {
            self.status = "Capability discovery currently supports IMAPS only; use the configured engine preflight for plain or STARTTLS sources.".into();
            return;
        }
        let source = if self.form.profile.source_port.trim().is_empty() {
            self.form.profile.source_host.trim().to_owned()
        } else if let Ok((host, _)) = endpoint_parts(self.form.profile.source_host.trim(), 993) {
            let port = self.form.profile.source_port.trim();
            if host.contains(':') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            }
        } else {
            self.form.profile.source_host.trim().to_owned()
        };
        let destination = self.form.profile.destination_host.trim().to_owned();
        if source.is_empty() || destination.is_empty() {
            self.status = "Enter both IMAP hosts before capability discovery.".into();
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.capability_receiver = Some(rx);
        self.status = "Authenticating and inspecting IMAPS readiness…".into();
        let source_user = self.form.profile.source_user.clone();
        let source_password = self.form.source_password.clone();
        let destination_user = self.form.profile.destination_user.clone();
        let destination_password = self.form.destination_password.clone();
        thread::spawn(move || {
            let result = probe_tls_capabilities(&source, &source_user, source_password.as_str())
                .and_then(|left| {
                    probe_tls_capabilities(
                        &destination,
                        &destination_user,
                        destination_password.as_str(),
                    )
                    .map(|right| (left, right))
                });
            let _ = tx.send(result);
        });
    }
    fn assess_plan(&mut self) {
        self.preflight = vec![
            (
                "Source endpoint".into(),
                if self.form.profile.source_host.is_empty() {
                    "Missing source server".into()
                } else {
                    self.form.profile.source_host.clone()
                },
                !self.form.profile.source_host.is_empty(),
            ),
            (
                "Destination endpoint".into(),
                if self.form.profile.destination_host.is_empty() {
                    "Missing destination server".into()
                } else {
                    self.form.profile.destination_host.clone()
                },
                !self.form.profile.destination_host.is_empty(),
            ),
            (
                "Safety mode".into(),
                if self.form.dry_run {
                    "Dry run enabled — destination will not be changed".into()
                } else {
                    "Live mode enabled — destination may be changed".into()
                },
                self.form.dry_run,
            ),
            (
                "Destructive options".into(),
                if self.form.profile.delete2 {
                    "--delete2 enabled: destination-only messages may be removed".into()
                } else {
                    "No destination deletion option selected".into()
                },
                !self.form.profile.delete2,
            ),
            (
                "Credential persistence".into(),
                "Passwords are excluded from saved profiles and the SQLite ledger".into(),
                true,
            ),
        ];
        if let Some(caps) = &self.source_capabilities {
            self.preflight.push((
                "Source capabilities".into(),
                caps.strategy().join(" · "),
                true,
            ));
        }
        if let Some(caps) = &self.destination_capabilities {
            self.preflight.push((
                "Destination capabilities".into(),
                caps.strategy().join(" · "),
                true,
            ));
        }
        if self.form.engine() == core::Engine::ImapSync {
            self.preflight.push((
                "Transport security".into(),
                match self.form.profile.source_tls.as_str() {
                    "imaps" => {
                        "TLS required for source and destination; imapsync will receive --ssl1 and --ssl2".into()
                    }
                    "starttls" => {
                        "STARTTLS required for source; TLS required for destination; cleartext fallback prohibited".into()
                    }
                    _ => "WARNING: source cleartext is explicitly configured; destination TLS remains required".into(),
                },
                self.form.profile.source_tls != "plain",
            ));
        }
    }
    fn create_project(&mut self) {
        self.assess_plan();
        if self.form.profile.source_host.trim().is_empty()
            || self.form.profile.destination_host.trim().is_empty()
        {
            self.status = "Enter source and destination hosts before creating a project.".into();
            return;
        }
        match self.store.create_project_with_mailbox(
            &self.form.profile.name,
            &self.form.profile.source_host,
            &self.form.profile.destination_host,
            &self.form.profile.source_user,
            &self.form.profile.destination_user,
        ) {
            Ok((project, job)) => {
                self.project_id = Some(project.id);
                self.job_id = Some(job);
                self.status = "Project created; ready for preflight review".into();
            }
            Err(e) => self.status = format!("Could not create project: {e}"),
        }
    }
    fn cockpit(&mut self, ctx: &egui::Context) {
        if !self.cockpit_open {
            return;
        }
        let mut open = self.cockpit_open;
        egui::Window::new("Migration Project Cockpit").open(&mut open).default_width(820.0).default_height(560.0).show(ctx, |ui| {
            ui.heading("Operator view"); ui.label(RichText::new("A durable migration project records phases and evidence independently of the desktop session.").color(MUTED)); ui.add_space(10.0);
            if ui.add_enabled(self.capability_receiver.is_none() && self.form.engine() != core::Engine::Dovecot, egui::Button::new("Run authenticated IMAPS readiness probe")).clicked() { self.start_capability_probe(); }
            if self.form.engine() == core::Engine::Dovecot { ui.label(RichText::new("Dovecot dry preflight checks the remote imapc source; destination readiness still requires administrative review.").size(11.0).color(MUTED)); }
            if self.project_id.is_none() && ui.button("Create project from current migration plan").clicked() { self.create_project(); }
            if let Some(id) = &self.project_id {
                match self.store.project(id) {
                    Ok(Some(project)) => {
                        ui.group(|ui| { ui.horizontal(|ui| { ui.heading(&project.name); ui.label(RichText::new(format!("ID {}", &project.id[..8])).monospace().color(MUTED)); }); ui.label(format!("{}  →  {}", project.source_endpoint, project.destination_endpoint)); });
                        ui.add_space(10.0); ui.label(RichText::new("MIGRATION PHASE").size(11.0).color(MUTED));
                        ui.horizontal_wrapped(|ui| for phase in [core::Phase::Discovery, core::Phase::Preflight, core::Phase::Pilot, core::Phase::Seed, core::Phase::CatchUp, core::Phase::FinalDelta, core::Phase::Verification, core::Phase::Complete] { let active = phase == project.phase; ui.label(RichText::new(format!("{} {phase:?}", if active { "●" } else { "○" })).strong().color(if active { TEAL } else { MUTED })); });
                        ui.add_space(10.0); if project.phase == core::Phase::Discovery && ui.button("Accept preflight review").clicked() { let _ = self.store.transition(&project.id, core::Phase::Preflight); self.status = "Phase advanced to Preflight".into(); }
                    }
                    Ok(None) => { self.project_id = None; }, Err(e) => self.status = format!("Could not read project: {e}"),
                }
            }
            ui.add_space(12.0); ui.separator(); ui.heading("Preflight assessment"); if ui.button("Refresh assessment").clicked() { self.assess_plan(); }
            egui::Grid::new("preflight").striped(true).show(ui, |ui| { ui.strong("Check"); ui.strong("Result"); ui.end_row(); for (name, detail, pass) in &self.preflight { ui.label(RichText::new(if *pass { "✓" } else { "!" }).color(if *pass { TEAL } else { ALERT })); ui.label(RichText::new(name).strong()); ui.label(detail); ui.end_row(); } });
            ui.add_space(10.0); ui.label(RichText::new("Next engine milestones: server capability negotiation, folder discovery, UIDVALIDITY-aware checkpoints, and message-level verification evidence.").size(11.0).color(MUTED));
        });
        self.cockpit_open = open;
    }

    fn readiness_score(&self) -> (usize, usize) {
        let checks = 5
            + usize::from(self.source_capabilities.is_some())
            + usize::from(self.destination_capabilities.is_some());
        let passed = [
            !self.form.profile.source_host.trim().is_empty(),
            !self.form.profile.destination_host.trim().is_empty(),
            !self.form.profile.source_user.trim().is_empty(),
            !self.form.profile.destination_user.trim().is_empty(),
            self.form.dry_run && !self.form.profile.delete2,
        ]
        .into_iter()
        .filter(|ok| *ok)
        .count()
            + usize::from(self.source_capabilities.is_some())
            + usize::from(self.destination_capabilities.is_some());
        (passed, checks)
    }

    fn lifecycle_stepper(&self, ui: &mut egui::Ui) {
        let phases = [
            core::Phase::Discovery,
            core::Phase::Preflight,
            core::Phase::Pilot,
            core::Phase::Seed,
            core::Phase::CatchUp,
            core::Phase::FinalDelta,
            core::Phase::Verification,
            core::Phase::Complete,
        ];
        let current = self
            .project_id
            .as_deref()
            .and_then(|id| self.store.project(id).ok().flatten())
            .map(|project| project.phase)
            .unwrap_or(core::Phase::Discovery);
        let current_index = phases
            .iter()
            .position(|phase| *phase == current)
            .unwrap_or(usize::MAX);
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("MIGRATION LIFECYCLE").size(11.0).strong().color(MUTED));
                if current == core::Phase::Attention {
                    ui.label(RichText::new("ATTENTION REQUIRED").strong().color(ALERT));
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (index, phase) in phases.iter().enumerate() {
                    if index > 0 {
                        ui.label(RichText::new("→").color(MUTED));
                    }
                    let color = if current_index != usize::MAX && index < current_index {
                        TEAL
                    } else if index == current_index {
                        BLUE
                    } else {
                        MUTED
                    };
                    ui.label(
                        RichText::new(format!(
                            "{} {}",
                            if current_index != usize::MAX && index < current_index {
                                "✓"
                            } else if index == current_index {
                                "●"
                            } else {
                                "○"
                            },
                            format_phase_name(*phase)
                        ))
                        .strong()
                        .color(color),
                    );
                }
            });
            if current == core::Phase::Attention {
                ui.label(RichText::new("A mailbox or run needs operator review. Normal lifecycle progress is paused until it is resolved.").size(11.0).color(ALERT));
            }
        });
    }

    fn source_transport_warning(&mut self, ui: &mut egui::Ui) {
        if self.form.profile.source_tls != "plain" {
            return;
        }
        ui.group(|ui| {
            ui.label(RichText::new("INSECURE SOURCE TRANSPORT").strong().color(ALERT));
            ui.label("Plain IMAP can expose the source password and mailbox data in transit.");
            let response = ui.add_enabled(
                !self.running(),
                egui::Checkbox::new(
                    &mut self.form.profile.allow_insecure_source_transport,
                    "I understand and explicitly allow cleartext source transport",
                ),
            );
            response.on_hover_text(
                "Use IMAPS or STARTTLS whenever possible. This acknowledgement is required before any authenticated operation, including dry preflight, and is included in the preflight fingerprint.",
            );
        });
    }

    fn project_summary(&mut self, ui: &mut egui::Ui) {
        self.lifecycle_stepper(ui);
        self.source_transport_warning(ui);
        if self.active_view != WorkspaceView::Plan {
            match self.active_view {
                WorkspaceView::Overview => self.overview_view(ui),
                WorkspaceView::Mailboxes => self.mailbox_view(ui),
                WorkspaceView::Activity => self.activity_view(ui),
                WorkspaceView::Verification => self.verification_view(ui),
                WorkspaceView::Plan => {}
            }
            // The legacy plan renderer follows this summary in the same panel.
            // Hide it when a dedicated workspace view is selected.
            ui.set_invisible();
            return;
        }
        let (passed, total) = self.readiness_score();
        let percent = (passed * 100 / total.max(1)) as u8;
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.heading("Migration workspace");
                ui.label(RichText::new(if self.form.dry_run { "SIMULATION" } else { "LIVE CHANGES" })
                    .strong()
                    .color(if self.form.dry_run { TEAL } else { ALERT }));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("{percent}% ready")).strong().color(if percent >= 80 { TEAL } else { ALERT }));
                });
            });
            ui.add_space(5.0);
            ui.horizontal_wrapped(|ui| {
                for (label, active) in [
                    ("Discovery", self.source_capabilities.is_some() || self.destination_capabilities.is_some()),
                    ("Preflight", !self.preflight.is_empty()),
                    ("Pilot", false),
                    ("Seed", false),
                    ("Catch-up", false),
                    ("Verify", false),
                ] {
                    ui.label(RichText::new(format!("{} {label}", if active { "●" } else { "○" }))
                        .size(11.0)
                        .color(if active { TEAL } else { MUTED }));
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new("Recommended next step: run preflight, review blockers, then select a small pilot mailbox.").color(MUTED));
        });
    }

    fn overview_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Migration overview");
        ui.label(
            RichText::new("A calm, evidence-led workspace for moving mailboxes safely.")
                .color(MUTED),
        );
        ui.add_space(16.0);
        let project = self
            .project_id
            .as_deref()
            .and_then(|id| self.store.project(id).ok().flatten());
        let phase = project
            .as_ref()
            .map(|value| value.phase)
            .unwrap_or(core::Phase::Discovery);
        let durable_jobs = project
            .as_ref()
            .and_then(|value| self.store.mailboxes(&value.id).ok())
            .unwrap_or_default();
        let attention_count = durable_jobs
            .iter()
            .filter(|job| needs_operator_review(&job.state))
            .count();
        let next_action = recommended_next_action(
            phase,
            !self.preflight.is_empty(),
            attention_count,
            self.running(),
        );
        ui.group(|ui| {
            ui.label(
                RichText::new("CURRENT PHASE")
                    .size(11.0)
                    .strong()
                    .color(MUTED),
            );
            ui.heading(format_phase_name(phase));
            if attention_count > 0 {
                ui.label(
                    RichText::new(format!(
                        "{} mailbox item(s) need attention",
                        attention_count
                    ))
                    .color(ALERT),
                );
            }
        });
        if !self.preflight.is_empty() {
            ui.add_space(10.0);
            ui.group(|ui| {
                ui.heading("Preflight assessment");
                egui::Grid::new("overview_preflight")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("Check");
                        ui.strong("Result");
                        ui.end_row();
                        for (name, detail, passed) in &self.preflight {
                            ui.label(
                                RichText::new(if *passed { "✓" } else { "!" }).color(if *passed {
                                    TEAL
                                } else {
                                    ALERT
                                }),
                            );
                            ui.label(RichText::new(name).strong());
                            ui.label(detail);
                            ui.end_row();
                        }
                    });
                ui.label(
                    RichText::new(
                        "Review every failed check before promoting a project to live execution.",
                    )
                    .size(11.0)
                    .color(MUTED),
                );
            });
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.group(|ui| {
                ui.label(RichText::new("PROJECT STATUS").size(11.0).color(MUTED));
                ui.heading(if self.project_id.is_some() {
                    "Project created"
                } else {
                    "No project yet"
                });
                ui.label(if self.project_id.is_some() {
                    "State is durable and ready for review."
                } else {
                    "Start by configuring endpoints or importing a mailbox list."
                });
            });
            ui.group(|ui| {
                ui.label(RichText::new("MAILBOXES").size(11.0).color(MUTED));
                if durable_jobs.is_empty() {
                    ui.heading("None configured");
                    ui.label("Use Mailboxes to review scope before running anything.");
                } else {
                    let counts = project_health_state_counts(&durable_jobs);
                    ui.heading(format!("{} total", durable_jobs.len()));
                    ui.label(format!(
                        "{} ready · {} running · {} verified",
                        counts.get("ready").copied().unwrap_or(0),
                        counts.get("running").copied().unwrap_or(0),
                        counts.get("verified").copied().unwrap_or(0),
                    ));
                    if attention_count > 0 {
                        ui.label(
                            RichText::new(format!(
                                "{} require operator attention",
                                attention_count
                            ))
                            .color(ALERT),
                        );
                    }
                }
            });
            ui.group(|ui| {
                ui.label(RichText::new("EVIDENCE").size(11.0).color(MUTED));
                ui.heading(if phase == core::Phase::Complete {
                    "Available"
                } else {
                    "Pending"
                });
                ui.label("Open Verification to review evidence and export the customer report.");
            });
        });
        ui.add_space(16.0);
        ui.group(|ui| {
            ui.heading("Recommended next step");
            ui.label(next_action);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Open migration plan  →").clicked() {
                    self.active_view = WorkspaceView::Plan;
                }
                if ui.button("Open Project Cockpit").clicked() {
                    self.cockpit_open = true;
                    self.assess_plan();
                }
                if ui.button("Import mailbox list").clicked() {
                    self.bulk_open = true;
                }
            });
        });
        ui.add_space(14.0);
        ui.label(RichText::new("Safety contract").strong());
        ui.horizontal_wrapped(|ui| {
            for text in [
                "Simulation is the default",
                "Saved profiles exclude passwords",
                "Source mail is read-only by default",
            ] {
                ui.label(RichText::new(format!("✓ {text}")).color(TEAL));
            }
            if self.form.profile.source_tls == "plain" {
                ui.label(
                    RichText::new("! Source transport is cleartext by explicit configuration")
                        .color(ALERT),
                );
            } else {
                ui.label(
                    RichText::new("✓ Encrypted source transport with certificate verification")
                        .color(TEAL),
                );
            }
        });
    }

    fn mailbox_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Mailboxes");
        ui.label(RichText::new("Review the migration scope before execution. Import CSV/XLSX for bulk work or configure a single mailbox in the plan.").color(MUTED));
        ui.add_space(12.0);
        if self.bulk_jobs.is_empty() {
            ui.group(|ui| {
                ui.heading("No bulk mailbox list loaded");
                ui.label("A single mailbox can be configured from the migration plan.");
                if ui.button("Open migration plan").clicked() {
                    self.active_view = WorkspaceView::Plan;
                }
                if ui.button("Import CSV / XLSX…").clicked() {
                    self.bulk_open = true;
                }
            });
        } else {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} mailbox jobs in scope", self.bulk_jobs.len()))
                        .strong(),
                );
                if ui.button("Review batch queue").clicked() {
                    self.bulk_open = true;
                }
            });
            ui.add_space(8.0);
            egui::Grid::new("mailbox_overview")
                .striped(true)
                .min_col_width(150.0)
                .show(ui, |ui| {
                    ui.strong("Mailbox");
                    ui.strong("Source");
                    ui.strong("Destination");
                    ui.strong("Readiness");
                    ui.end_row();
                    for job in self.bulk_jobs.iter().take(100) {
                        ui.label(&job.label);
                        ui.label(format!(
                            "{}\n{}",
                            job.form.profile.source_host, job.form.profile.source_user
                        ));
                        ui.label(format!(
                            "{}\n{}",
                            job.form.profile.destination_host, job.form.profile.destination_user
                        ));
                        ui.label(RichText::new(&job.state).color(TEAL));
                        ui.end_row();
                    }
                });
        }
    }

    fn activity_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Activity");
        ui.label(RichText::new("Live output is retained here for operator review. Durable run history remains available after restart.").color(MUTED));
        ui.add_space(12.0);
        if let Some(job) = self.job_id.as_deref() {
            let state = self.store.mailbox_state(job).ok().flatten();
            if state.as_deref().is_some_and(needs_operator_review)
                && ui.button("Prepare safe retry  →").clicked()
            {
                self.form.dry_run = true;
                self.live_confirmed = false;
                self.active_view = WorkspaceView::Plan;
                self.status =
                    "Retry prepared as a dry preflight. Review the exact plan before any live run."
                        .into();
            }
        }
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.heading(if self.running() {
                    "Run in progress"
                } else {
                    "No active run"
                });
                ui.label(
                    RichText::new(&self.status).color(if self.status.contains("Failed") {
                        ALERT
                    } else {
                        BLUE
                    }),
                );
            });
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .max_height(420.0)
                .show(ui, |ui| {
                    for line in &self.output {
                        ui.label(RichText::new(line).monospace().size(12.0));
                    }
                });
        });
        ui.add_space(14.0);
        ui.heading("Durable run history");
        let Some(project) = self.active_project_id() else {
            ui.label(
                RichText::new("Create or restore a project to see durable runs.").color(MUTED),
            );
            return;
        };
        match self.store.recent_runs(project, 20) {
            Ok(runs) if runs.is_empty() => {
                ui.label(RichText::new("No durable runs recorded yet.").color(MUTED));
            }
            Ok(runs) => {
                egui::Grid::new("durable_run_history")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("Run");
                        ui.strong("Engine");
                        ui.strong("Status");
                        ui.strong("Started");
                        ui.strong("Finished");
                        ui.strong("Detail");
                        ui.end_row();
                        for run in runs {
                            ui.label(RichText::new(&run.id[..8.min(run.id.len())]).monospace());
                            ui.label(run.engine);
                            ui.label(RichText::new(&run.status).color(
                                if run.status == "completed" {
                                    TEAL
                                } else if run.status == "running" {
                                    BLUE
                                } else {
                                    ALERT
                                },
                            ));
                            ui.label(run.started_at);
                            ui.label(run.finished_at.unwrap_or_else(|| "in progress".into()));
                            ui.label(if run.detail.is_empty() {
                                "—".into()
                            } else {
                                run.detail
                            });
                            ui.end_row();
                        }
                    });
            }
            Err(error) => {
                ui.label(
                    RichText::new(format!("Could not read run history: {error}")).color(ALERT),
                );
            }
        }
    }

    fn export_verification_report(&self) -> Result<(), String> {
        let job = self
            .job_id
            .as_deref()
            .ok_or("No mailbox evidence is available yet.")?;
        let (evidence_run_id, evidence) = self
            .store
            .latest_evidence_for_run(job)
            .map_err(|e| e.to_string())?
            .ok_or("No mailbox evidence is available yet.")?;
        let state = self
            .store
            .mailbox_state(job)
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| "unknown".into());
        let project_id = self
            .store
            .project_id_for_mailbox(job)
            .map_err(|e| e.to_string())?
            .ok_or("The mailbox job no longer belongs to a project.")?;
        let project = self
            .store
            .project(&project_id)
            .map_err(|e| e.to_string())?
            .ok_or("The mailbox project no longer exists.")?;
        let (source_mailbox, destination_mailbox) = self
            .store
            .mailbox_identity(job)
            .map_err(|e| e.to_string())?
            .map(|(source, destination, _)| (source, destination))
            .ok_or("The mailbox job no longer exists.")?;
        let run = self
            .store
            .run(&evidence_run_id)
            .map_err(|e| e.to_string())?
            .ok_or("The evidence refers to a run that is no longer available.")?;
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-verification.md")
            .save_file()
            .ok_or("Report export cancelled.")?;
        let report = format!(
            "# MailSwiftSync verification report\n\n- Project: {}\n- Source endpoint: {}\n- Destination endpoint: {}\n- Source mailbox: {}\n- Destination mailbox: {}\n- Engine: {}\n- Run ID: `{}`\n- Run status: `{}`\n- Started: `{}`\n- Finished: `{}`\n- Mailbox state: `{}`\n- Evidence level: `{}`\n- Evidence source: `{}`\n\n## Execution plan snapshot\n\nThe snapshot excludes session passwords and raw extra-option values. It retains an SHA-256 digest for expert-option identity without copying those values into the ledger or report.\n\n```toml\n{}\n```\n\n| Metric | Source | Destination |\n|---|---:|---:|\n| Folders | {} | {} |\n| Messages | {} | {} |\n| Virtual size | {} | {} |\n| Unmatched messages | {} | — |\n| Failed messages | {} | — |\n\nThis report distinguishes engine-confirmed output from aggregate reconciliation. Neither is independent message-level proof; provider-specific warnings and deeper verification require additional review.",
            markdown_escape(&project.name),
            markdown_escape(&project.source_endpoint),
            markdown_escape(&project.destination_endpoint),
            markdown_escape(&source_mailbox),
            markdown_escape(&destination_mailbox),
            markdown_escape(&run.engine),
            run.id,
            run.status,
            run.started_at,
            run.finished_at.as_deref().unwrap_or("in progress"),
            state,
            evidence.evidence_level(),
            if evidence.authoritative {
                "engine-confirmed summary"
            } else {
                "aggregate mailbox totals"
            },
            run.plan_snapshot,
            evidence.source_folders,
            evidence.destination_folders,
            evidence.source_messages,
            evidence.destination_messages,
            evidence.source_bytes,
            evidence.destination_bytes,
            evidence.unmatched_messages,
            evidence.failed_messages
        );
        write_private_atomic(&path, &report).map_err(|e| e.to_string())
    }

    fn export_project_report(&self) -> Result<(), String> {
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        let project = self
            .store
            .project(project_id)
            .map_err(|e| e.to_string())?
            .ok_or("The durable migration project no longer exists.")?;
        let jobs = self
            .store
            .mailboxes(project_id)
            .map_err(|e| e.to_string())?;
        if jobs.is_empty() {
            return Err("The project has no mailbox jobs to report.".into());
        }
        let runs = self
            .store
            .recent_runs(project_id, 20)
            .map_err(|e| e.to_string())?;
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-project-report.md")
            .save_file()
            .ok_or("Report export cancelled.")?;
        let verified = jobs.iter().filter(|job| job.state == "verified").count();
        let attention = jobs
            .iter()
            .filter(|job| needs_operator_review(&job.state))
            .count();
        let mut report = format!(
            "# MailSwiftSync project report\n\n- Project: {}\n- Project ID: `{}`\n- Source endpoint: {}\n- Destination endpoint: {}\n- Phase: `{:?}`\n- Mailboxes: {}\n- Verified: {}\n- Attention required: {}\n\n## Mailbox results\n\n| Source mailbox | Destination mailbox | State | Evidence run | Evidence | Source messages | Destination messages | Unmatched | Failed |\n|---|---|---|---|---|---:|---:|---:|---:|\n",
            markdown_escape(&project.name),
            project.id,
            markdown_escape(&project.source_endpoint),
            markdown_escape(&project.destination_endpoint),
            project.phase,
            jobs.len(),
            verified,
            attention,
        );
        for job in jobs {
            if let Some((evidence_run_id, evidence)) = self
                .store
                .latest_evidence_for_run(&job.id)
                .map_err(|e| e.to_string())?
            {
                report.push_str(&format!(
                    "| {} | {} | `{}` | `{}` | {} | {} | {} | {} | {} |\n",
                    markdown_escape(&job.source_mailbox),
                    markdown_escape(&job.destination_mailbox),
                    job.state,
                    evidence_run_id,
                    evidence.evidence_level(),
                    evidence.source_messages,
                    evidence.destination_messages,
                    evidence.unmatched_messages,
                    evidence.failed_messages,
                ));
            } else {
                report.push_str(&format!(
                    "| {} | {} | `{}` | — | missing | — | — | — | — |\n",
                    markdown_escape(&job.source_mailbox),
                    markdown_escape(&job.destination_mailbox),
                    job.state,
                ));
            }
        }
        report.push_str("\n## Recent runs\n\n| Run | Engine | Status | Plan reference | Started | Finished | Detail |\n|---|---|---|---|---|---|---|\n");
        for run in runs {
            report.push_str(&format!(
                "| `{}` | {} | `{}` | `{}` | {} | {} | {} |\n",
                run.id,
                markdown_escape(&run.engine),
                run.status,
                plan_snapshot_sha256(&run.plan_snapshot),
                run.started_at,
                run.finished_at.unwrap_or_else(|| "in progress".into()),
                markdown_escape(if run.detail.is_empty() {
                    "—"
                } else {
                    &run.detail
                }),
            ));
        }
        report.push_str("\nEvidence levels describe what was actually established. Engine-confirmed output is not independent message-level reconciliation, and aggregate totals are not proof of message identity. Missing evidence or any state other than `verified` requires operator review before declaring the project complete.\n");
        write_private_atomic(&path, &report).map_err(|e| e.to_string())
    }

    fn export_project_json(&self) -> Result<(), String> {
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        let project = self
            .store
            .project(project_id)
            .map_err(|e| e.to_string())?
            .ok_or("The durable migration project no longer exists.")?;
        let jobs = self
            .store
            .mailboxes(project_id)
            .map_err(|e| e.to_string())?;
        if jobs.is_empty() {
            return Err("The project has no mailbox jobs to report.".into());
        }
        let runs = self
            .store
            .recent_runs(project_id, 20)
            .map_err(|e| e.to_string())?;
        let mailboxes = jobs
            .into_iter()
            .map(|job| {
                let evidence = self
                    .store
                    .latest_evidence_for_run(&job.id)
                    .map_err(|e| e.to_string())?;
                Ok(match evidence {
                    Some((evidence_run_id, evidence)) => serde_json::json!({
                        "id": job.id,
                        "source_mailbox": job.source_mailbox,
                        "destination_mailbox": job.destination_mailbox,
                        "state": job.state,
                        "evidence": {
                            "run_id": evidence_run_id,
                            "scope": if evidence.authoritative { "engine-confirmed" } else { "aggregate" },
                            "evidence_level": evidence.evidence_level(),
                            "authoritative": evidence.authoritative,
                            "source_folders": evidence.source_folders,
                            "destination_folders": evidence.destination_folders,
                            "source_messages": evidence.source_messages,
                            "destination_messages": evidence.destination_messages,
                            "source_bytes": evidence.source_bytes,
                            "destination_bytes": evidence.destination_bytes,
                            "unmatched_messages": evidence.unmatched_messages,
                            "failed_messages": evidence.failed_messages,
                        }
                    }),
                    None => serde_json::json!({
                        "id": job.id,
                        "source_mailbox": job.source_mailbox,
                        "destination_mailbox": job.destination_mailbox,
                        "state": job.state,
                        "evidence": null
                    }),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let run_values = runs
            .into_iter()
            .map(|run| {
                serde_json::json!({
                    "id": run.id,
                    "job_id": run.job_id,
                    "parent_run_id": run.parent_run_id,
                    "engine": run.engine,
                    "plan_snapshot_sha256": plan_snapshot_sha256(&run.plan_snapshot),
                    "status": run.status,
                    "started_at": run.started_at,
                    "finished_at": run.finished_at,
                    "detail": run.detail,
                })
            })
            .collect::<Vec<_>>();
        let value = serde_json::json!({
            "format": "mailswiftsync-project-report",
            "format_version": 1,
            "project": {
                "id": project.id,
                "name": project.name,
                "source_endpoint": project.source_endpoint,
                "destination_endpoint": project.destination_endpoint,
                "phase": format!("{:?}", project.phase),
            },
            "mailboxes": mailboxes,
            "runs": run_values,
            "note": "Aggregate evidence is not message-level reconciliation; unresolved or missing evidence requires operator review."
        });
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-project-report.json")
            .save_file()
            .ok_or("Report export cancelled.")?;
        let report = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        write_private_atomic(&path, &report).map_err(|e| e.to_string())
    }

    fn export_project_health(&self) -> Result<(), String> {
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        let project = self
            .store
            .project(project_id)
            .map_err(|e| e.to_string())?
            .ok_or("The durable migration project no longer exists.")?;
        let jobs = self
            .store
            .mailboxes(project_id)
            .map_err(|e| e.to_string())?;
        let runs = self
            .store
            .recent_runs(project_id, 20)
            .map_err(|e| e.to_string())?;
        let attention = jobs
            .iter()
            .filter(|job| needs_operator_review(&job.state))
            .map(|job| {
                serde_json::json!({
                    "id": job.id,
                    "source_mailbox": job.source_mailbox,
                    "destination_mailbox": job.destination_mailbox,
                    "state": job.state,
                })
            })
            .collect::<Vec<_>>();
        let recent_runs = runs
            .iter()
            .map(|run| {
                serde_json::json!({
                    "id": run.id,
                    "job_id": run.job_id,
                    "parent_run_id": run.parent_run_id,
                    "engine": run.engine,
                    "plan_snapshot_sha256": plan_snapshot_sha256(&run.plan_snapshot),
                    "status": run.status,
                    "started_at": run.started_at,
                    "finished_at": run.finished_at,
                    "detail": run.detail,
                })
            })
            .collect::<Vec<_>>();
        let value = serde_json::json!({
            "format": "mailswiftsync-project-health",
            "version": 1,
            "project": {
                "id": project.id,
                "name": project.name,
                "phase": format!("{:?}", project.phase),
                "source_endpoint": project.source_endpoint,
                "destination_endpoint": project.destination_endpoint,
            },
            "mailboxes": {
                "total": jobs.len(),
                "by_state": project_health_state_counts(&jobs),
                "attention": attention,
            },
            "recent_runs": recent_runs,
        });
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-project-health.json")
            .save_file()
            .ok_or("Health export cancelled.")?;
        let report = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        write_private_atomic(&path, &report).map_err(|e| e.to_string())
    }

    fn verification_view(&self, ui: &mut egui::Ui) {
        ui.heading("Verification");
        ui.label(RichText::new("Do not trust a completed process until the destination reconciles with the source.").color(MUTED));
        ui.add_space(12.0);
        ui.group(|ui| {
            ui.heading("Verification and audit report");
            ui.label(RichText::new("The transfer engine is only one part of the migration. This report is the operator-facing proof of what arrived and what still needs attention.").color(MUTED));
            if self.active_project_id().is_some() {
                if ui.button("Export project report…").clicked() {
                    let _ = self.export_project_report();
                }
                if ui.button("Export project JSON…").clicked() {
                    let _ = self.export_project_json();
                }
                if ui.button("Export project health…").clicked() {
                    let _ = self.export_project_health();
                }
            }
            if let Some(job) = &self.job_id {
                match self.store.evidence(job) {
                    Ok(Some(evidence)) => {
                        ui.label("Durable mailbox reconciliation");
                        if ui.button("Export verification report…").clicked() {
                            let _ = self.export_verification_report();
                        }
                        for (label, value) in [
                            ("Folders", format!("{} source / {} destination", evidence.source_folders, evidence.destination_folders)),
                            ("Messages", format!("{} source / {} destination", evidence.source_messages, evidence.destination_messages)),
                            ("Bytes", format!("{} source / {} destination", evidence.source_bytes, evidence.destination_bytes)),
                            ("Unmatched", evidence.unmatched_messages.to_string()),
                            ("Failed", evidence.failed_messages.to_string()),
                            ("Evidence level", evidence.evidence_level().into()),
                        ] { ui.horizontal(|ui| { ui.label(RichText::new(label).strong()); ui.label(value); }); }
                    }
                    Ok(None) => { ui.label(RichText::new("The transfer finished, but no mailbox-level evidence has been captured yet.").color(ALERT)); }
                    Err(error) => { ui.label(RichText::new(format!("Could not read evidence: {error}")).color(ALERT)); }
                }
            } else {
                ui.label("Run a migration to create a durable mailbox evidence record.");
            }
        });
    }
    fn job_from_values(
        values: &HashMap<String, String>,
        base: &Form,
        row: usize,
    ) -> Result<BulkJob, String> {
        let get = |key: &str| {
            values
                .get(key)
                .map(String::as_str)
                .unwrap_or("")
                .trim()
                .to_owned()
        };
        let mut form = base.clone();
        form.profile.source_host = get("source_host");
        form.profile.source_user = get("source_user");
        if let Some(value) = values.get("source_credential_id") {
            form.profile.source_credential_id = value.trim().to_owned();
        }
        // Whitespace is meaningful in passwords. Trim only semantic fields;
        // otherwise a valid credential such as ` Secret ` is silently changed.
        form.source_password =
            Zeroizing::new(values.get("source_password").cloned().unwrap_or_default());
        form.profile.destination_host = get("destination_host");
        form.profile.destination_user = get("destination_user");
        if let Some(value) = values.get("destination_credential_id") {
            form.profile.destination_credential_id = value.trim().to_owned();
        }
        form.destination_password = Zeroizing::new(
            values
                .get("destination_password")
                .cloned()
                .unwrap_or_default(),
        );
        form.validate_for_import()
            .map_err(|e| format!("Row {row}: {e}"))?;
        let label = values
            .get("name")
            .filter(|v| !v.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "Row {row}: {} → {}",
                    form.profile.source_user, form.profile.destination_user
                )
            });
        Ok(BulkJob {
            label,
            form,
            state: "Ready".into(),
        })
    }
    fn import_bulk(&mut self, path: &std::path::Path) {
        let ext = path
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let result = if ext == "csv" {
            Self::read_csv(path, &self.form)
        } else if ext == "xls" || ext == "xlsx" {
            Self::read_sheet(path, &self.form)
        } else {
            Err("Choose a .csv, .xls, or .xlsx file.".into())
        };
        match result {
            Ok(jobs) => {
                self.bulk_message = format!(
                    "Imported {} ready jobs. Review the queue before running.",
                    jobs.len()
                );
                // A new file is a new durable batch scope. Never let a queue
                // replacement reuse the project/job IDs from an older file.
                self.bulk_project_id = None;
                self.bulk_job_ids.clear();
                self.bulk_jobs = jobs;
            }
            Err(e) => self.bulk_message = e,
        }
    }

    fn apply_bulk_keyring_id(&mut self, source: bool) {
        let value = if source {
            self.bulk_source_keyring_apply.trim().to_owned()
        } else {
            self.bulk_destination_keyring_apply.trim().to_owned()
        };
        if value.is_empty() {
            self.bulk_message = format!(
                "Enter a {} keyring ID before applying it.",
                if source { "source" } else { "destination" }
            );
            return;
        }
        let applied = apply_keyring_id_to_jobs(&mut self.bulk_jobs, &value, source);
        self.bulk_message = format!(
            "Applied the {} keyring ID to {applied} row(s) without a credential reference.",
            if source { "source" } else { "destination" }
        );
    }
    fn read_csv(path: &std::path::Path, base: &Form) -> Result<Vec<BulkJob>, String> {
        let mut reader = csv::Reader::from_path(path).map_err(|e| e.to_string())?;
        let headers = reader
            .headers()
            .map_err(|e| e.to_string())?
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        Self::validate_headers(&headers, base)?;
        let mut jobs = Vec::new();
        for (index, record) in reader.records().enumerate() {
            let record = record.map_err(|e| e.to_string())?;
            if record.len() != headers.len() {
                return Err(format!(
                    "Row {} has {} values but the header has {} columns.",
                    index + 2,
                    record.len(),
                    headers.len()
                ));
            }
            let values = headers
                .iter()
                .zip(record.iter())
                .map(|(h, v)| (h.clone(), v.to_owned()))
                .collect();
            jobs.push(Self::job_from_values(&values, base, index + 2)?);
        }
        if jobs.is_empty() {
            return Err("The file has no migration rows.".into());
        }
        Ok(jobs)
    }
    fn read_sheet(path: &std::path::Path, base: &Form) -> Result<Vec<BulkJob>, String> {
        let mut book = open_workbook_auto(path).map_err(|e| e.to_string())?;
        let range = book
            .worksheet_range_at(0)
            .ok_or("The workbook has no worksheets.")?
            .map_err(|e| e.to_string())?;
        let mut rows = range.rows();
        let headers = rows
            .next()
            .ok_or("The worksheet is empty.")?
            .iter()
            .map(|x| x.to_string().trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        Self::validate_headers(&headers, base)?;
        let mut jobs = Vec::new();
        for (index, row) in rows.enumerate() {
            if row.iter().all(|cell| cell.to_string().trim().is_empty()) {
                continue;
            }
            let values = headers
                .iter()
                .zip(row.iter())
                .map(|(h, v)| (h.clone(), v.to_string()))
                .collect();
            jobs.push(Self::job_from_values(&values, base, index + 2)?);
        }
        if jobs.is_empty() {
            return Err("The worksheet has no migration rows.".into());
        }
        Ok(jobs)
    }
    fn validate_headers(headers: &[String], _base: &Form) -> Result<(), String> {
        let mut seen = HashSet::new();
        for header in headers {
            if header.is_empty() || !seen.insert(header.clone()) {
                return Err(
                    "The migration file contains an empty or duplicate column header.".into(),
                );
            }
        }
        if seen.contains("extra_options") {
            return Err(
                "The migration file cannot contain extra_options; configure trusted engine options in the application instead of importing executable command settings.".into(),
            );
        }
        let required = vec![
            "source_host",
            "source_user",
            "destination_host",
            "destination_user",
        ];
        let missing = required
            .into_iter()
            .filter(|header| !seen.contains(*header))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Missing required column(s): {}.",
                missing.join(", ")
            ))
        }
    }
    fn start_bulk(&mut self) {
        if self.bulk_jobs.is_empty() {
            self.bulk_message = "Import a file before starting the queue.".into();
            return;
        }
        let live = !self.form.dry_run;
        if live && !self.bulk_live_confirmed {
            self.bulk_live_confirm_open = true;
            return;
        }
        if live {
            self.bulk_live_confirmed = false;
            if self.bulk_project_id.is_none() || self.bulk_job_ids.len() != self.bulk_jobs.len() {
                self.bulk_message = "Run a successful dry validation for this queue before starting live migrations.".into();
                return;
            }
            for (index, (job_id, job)) in self
                .bulk_job_ids
                .iter()
                .zip(self.bulk_jobs.iter())
                .enumerate()
            {
                let state = self.store.mailbox_state(job_id).ok().flatten();
                let preflight = self.store.preflight_plan(job_id).ok().flatten();
                if !matches!(
                    state.as_deref(),
                    Some(
                        "ready"
                            | "delta_required"
                            | "verification_difference"
                            | "failed"
                            | "attention"
                            | "cancelled"
                            | "completed"
                            | "verified"
                    )
                ) || preflight.as_deref() != Some(job.form.plan_fingerprint().as_str())
                {
                    self.bulk_message = format!(
                        "Mailbox {} is not ready for live execution. Re-run dry validation after reviewing its exact plan.",
                        index + 1
                    );
                    return;
                }
            }
        }
        if !self.persistence_available {
            self.bulk_message = "Batch execution requires durable SQLite storage.".into();
            return;
        }
        let mut jobs = self.bulk_jobs.clone();
        if live && let Some(error) = duplicate_bulk_destination(&jobs) {
            self.bulk_message = error;
            return;
        }
        for job in &mut jobs {
            if let Err(error) = job.form.load_configured_keyring_credentials() {
                self.bulk_message =
                    format!("Could not load credentials for {}: {error}", job.label);
                return;
            }
        }
        for job in &mut jobs {
            job.form.dry_run = !live;
        }
        if let Some((index, error)) = jobs
            .iter()
            .enumerate()
            .find_map(|(index, job)| job.form.validate().err().map(|error| (index, error)))
        {
            self.bulk_message =
                format!("Mailbox {} is not ready for validation: {error}", index + 1);
            return;
        }
        if live
            && jobs
                .iter()
                .any(|job| job.form.requires_insecure_transport_ack())
        {
            self.bulk_message = "Live batch blocked: explicitly acknowledge that plain IMAP exposes credentials and mail in transit for every affected row.".into();
            return;
        }
        let concurrency = self.form.profile.batch_concurrency.clamp(1, 16);
        if let Err(error) = validate_batch_throttle(&self.form.profile, concurrency) {
            self.bulk_message = error;
            return;
        }
        self.durability_error = false;
        self.pending_batch_evidence.clear();
        let mailboxes = jobs
            .iter()
            .map(|job| {
                let config = durable_batch_profile_config(&job.form.profile)?;
                Ok((
                    job.form.profile.source_user.clone(),
                    job.form.profile.destination_user.clone(),
                    config,
                ))
            })
            .collect::<Result<Vec<_>, String>>();
        let mailboxes = match mailboxes {
            Ok(value) => value,
            Err(error) => {
                self.bulk_message = error;
                return;
            }
        };
        let reusable_project = self.bulk_project_id.clone().filter(|project_id| {
            self.store
                .mailboxes(project_id)
                .ok()
                .is_some_and(|stored| durable_batch_matches_queue(&stored, &mailboxes))
        });
        let (project_id, job_ids) = if let Some(project_id) = reusable_project {
            (project_id, self.bulk_job_ids.clone())
        } else {
            let (project, job_ids) = match self.store.create_project_with_mailbox_configs(
                "Batch migration",
                "batch",
                "batch",
                &mailboxes,
            ) {
                Ok(value) => value,
                Err(error) => {
                    self.bulk_message = format!("Could not create durable batch: {error}");
                    return;
                }
            };
            (project.id, job_ids)
        };
        self.bulk_project_id = Some(project_id.clone());
        self.bulk_job_ids = job_ids;
        self.bulk_live_run = live;
        let expected_plans = if live {
            jobs.iter()
                .map(|job| job.form.plan_fingerprint())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let plan_snapshot = jobs
            .iter()
            .map(|job| job.form.plan_snapshot())
            .collect::<Vec<_>>()
            .join("\n--- batch mailbox plan ---\n");
        let run_id = uuid::Uuid::new_v4().to_string();
        self.run_id = Some(run_id.clone());
        let child_plans = jobs
            .iter()
            .map(|job| core::BatchChildPlan {
                engine: job.form.engine().label().to_owned(),
                plan_snapshot: job.form.plan_snapshot(),
            })
            .collect::<Vec<_>>();
        let child_run_ids = match self.store.begin_batch_run_with_children(
            &project_id,
            &self.bulk_job_ids,
            &run_id,
            if live {
                "batch migration"
            } else {
                "batch validation"
            },
            &expected_plans,
            &plan_snapshot,
            &child_plans,
        ) {
            Ok(ids) => ids,
            Err(error) => {
                self.bulk_message = format!("Could not start durable batch run: {error}");
                return;
            }
        };
        self.active_run = Some(ActiveRunContext {
            run_id: run_id.clone(),
            project_id: project_id.clone(),
            job_id: None,
            batch_job_ids: self.bulk_job_ids.clone(),
            batch_plan_fingerprints: jobs.iter().map(|job| job.form.plan_fingerprint()).collect(),
            batch_child_run_ids: child_run_ids,
            kind: RunKind::Batch,
            dry_run: !live,
            engine: self.form.engine(),
            plan_fingerprint: String::new(),
        });
        for job in &mut self.bulk_jobs {
            job.state = "Queued".into();
        }
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_requested = Some(cancel.clone());
        self.receiver = Some(rx);
        self.run_started_at = Some(std::time::Instant::now());
        self.status = format!(
            "{}: {} jobs",
            if live {
                "Batch migration"
            } else {
                "Batch validation"
            },
            jobs.len()
        );
        self.output.clear();
        let retry_count = self.form.profile.batch_retry_count.min(3);
        let job_count = jobs.len();
        let (job_tx, job_rx) = crossbeam_channel::unbounded();
        let queue_job_ids = self.bulk_job_ids.clone();
        let child_run_ids = self
            .active_run
            .as_ref()
            .map(|run| run.batch_child_run_ids.clone())
            .unwrap_or_default();
        for (index, job) in jobs.into_iter().enumerate() {
            job_tx
                .send((
                    index,
                    queue_job_ids[index].clone(),
                    child_run_ids[index].clone(),
                    job,
                ))
                .expect("batch workers are created immediately after queue setup");
        }
        drop(job_tx);
        let launch_limiter = Arc::new(ProcessLaunchLimiter::new(BATCH_PROCESS_STARTS_PER_SECOND));
        let batch_project_id = project_id.clone();
        let batch_run_id = run_id.clone();
        thread::spawn(move || {
            let failed = Arc::new(AtomicBool::new(false));
            let terminal_jobs = Arc::new(Mutex::new(HashSet::new()));
            let mut workers = Vec::with_capacity(concurrency);
            for _ in 0..concurrency {
                let job_rx = job_rx.clone();
                let failed = Arc::clone(&failed);
                let terminal_jobs = Arc::clone(&terminal_jobs);
                let tx = tx.clone();
                let cancel = Arc::clone(&cancel);
                let launch_limiter = Arc::clone(&launch_limiter);
                let batch_project_id = batch_project_id.clone();
                let batch_run_id = batch_run_id.clone();
                workers.push(thread::spawn(move || {
                    while let Ok((index, job_id, child_run_id, job)) = job_rx.recv() {
                        if cancel.load(Ordering::Relaxed) {
                            let _ = tx.send(Event::JobState {
                                job_id: job_id.clone(),
                                child_run_id: child_run_id.clone(),
                                state: "Cancelled".into(),
                            });
                            let _ = tx.send(Event::JobFinished {
                                job_id: job_id.clone(),
                                child_run_id: child_run_id.clone(),
                                state: "cancelled".into(),
                                detail: "cancelled before worker claim".into(),
                            });
                            if let Ok(mut terminal) = terminal_jobs.lock() {
                                terminal.insert(index);
                            }
                            continue;
                        }
                        let _ = tx.send(Event::Line(format!(
                            "══ Job {}: {} ══",
                            index + 1,
                            job.label
                        )));
                        let mut completed = false;
                        let mut delta_required = false;
                        for attempt in 0..=retry_count {
                            if !launch_limiter.acquire(&cancel) {
                                break;
                            }
                            let (claim_tx, claim_rx) = mpsc::sync_channel(1);
                            if tx
                                .send(Event::ClaimBatch {
                                    project_id: batch_project_id.clone(),
                                    job_id: job_id.clone(),
                                    parent_run_id: batch_run_id.clone(),
                                    child_run_id: child_run_id.clone(),
                                    reply: claim_tx,
                                })
                                .is_err()
                            {
                                failed.store(true, Ordering::Relaxed);
                                break;
                            }
                            let claim_result = loop {
                                if cancel.load(Ordering::Relaxed) {
                                    break Err("cancelled by operator before durable claim".to_owned());
                                }
                                match claim_rx.recv_timeout(Duration::from_millis(100)) {
                                    Ok(result) => break result,
                                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                                        break Err("durable claim response was lost".to_owned())
                                    }
                                }
                            };
                            if let Err(error) = claim_result {
                                let cancelled = error.contains("cancelled");
                                if !cancelled {
                                    failed.store(true, Ordering::Relaxed);
                                }
                                let _ = tx.send(Event::Line(format!(
                                    "[{}] {}",
                                    index + 1,
                                    error
                                )));
                                let _ = tx.send(Event::JobState {
                                    job_id: job_id.clone(),
                                    child_run_id: child_run_id.clone(),
                                    state: if cancelled { "Cancelled" } else { "Failed" }.into(),
                                });
                                let _ = tx.send(Event::JobFinished {
                                    job_id: job_id.clone(),
                                    child_run_id: child_run_id.clone(),
                                    state: if cancelled { "cancelled" } else { "failed" }.into(),
                                    detail: error,
                                });
                                if let Ok(mut terminal) = terminal_jobs.lock() {
                                    terminal.insert(index);
                                }
                                break;
                            }
                            let _ = tx.send(Event::JobState {
                                job_id: job_id.clone(),
                                child_run_id: child_run_id.clone(),
                                state: "Running".into(),
                            });
                            if attempt > 0 {
                                let _ = tx.send(Event::Line(format!(
                                    "[{}] retry attempt {attempt}/{retry_count}",
                                    index + 1
                                )));
                                let _ = tx.send(Event::JobState {
                                    job_id: job_id.clone(),
                                    child_run_id: child_run_id.clone(),
                                    state: "Running".into(),
                                });
                            }
                            let prepared = job
                                .form
                                .prepared_command_with_throttle_divisor(concurrency);
                            let result = match prepared {
                                Ok(command) => {
                                    let cleanup_guard = CleanupGuard::new(command.cleanup.clone());
                                    let result = run_streaming(
                                        &command.executable,
                                        &command.args,
                                        &command.env,
                                        &tx,
                                        &child_run_id,
                                        &job_id,
                                        &format!("[{}] ", index + 1),
                                        &cancel,
                                        &[
                                            job.form.source_password.to_string(),
                                            job.form.destination_password.to_string(),
                                        ],
                                        Duration::from_secs(
                                            job.form.profile.migration_timeout_hours * 60 * 60,
                                        ),
                                        job.form.engine() == core::Engine::Dovecot
                                            && !job.form.dry_run,
                                    )
                                    .map(|stream| {
                                        if !job.form.dry_run
                                            && let Some(evidence) = stream.imapsync_evidence
                                        {
                                            let _ = tx.send(Event::BatchEvidence {
                                                job_id: job_id.clone(),
                                                child_run_id: child_run_id.clone(),
                                                evidence,
                                            });
                                        }
                                        stream.outcome
                                    });
                                    let result = if result.is_ok()
                                        && job.form.dry_run
                                        && job.form.engine() == core::Engine::Dovecot
                                    {
                                        result.and_then(|outcome| {
                                            run_dovecot_destination_preflight(
                                                &job.form.dovecot_destination_preflight_commands(),
                                                &tx,
                                                &cancel,
                                                Duration::from_secs(
                                                    job.form.profile.migration_timeout_hours * 60 * 60,
                                                ),
                                                &format!("[{}] ", index + 1),
                                            )
                                            .map(|_| outcome)
                                        })
                                    } else {
                                        result
                                    };
                                    drop(cleanup_guard);
                                    if result.is_ok()
                                        && !job.form.dry_run
                                        && job.form.engine() == core::Engine::Dovecot
                                    {
                                        let verification = job.form.dovecot_verification_commands(false);
                                        let verification_secret =
                                            job.form.source_password.to_string();
                                        let verification_env = if job.form.local_doveadm() {
                                            vec![(
                                                "MAILSWIFTSYNC_IMAPC_PASSWORD".into(),
                                                verification_secret.clone(),
                                            )]
                                        } else {
                                            Vec::new()
                                        };
                                        result.and_then(|outcome| {
                                            run_dovecot_verification(
                                                &verification,
                                                &verification_env,
                                                std::slice::from_ref(&verification_secret),
                                                &tx,
                                                &cancel,
                                                Duration::from_secs(
                                                    job.form.profile.migration_timeout_hours
                                                        * 60
                                                        * 60,
                                                ),
                                                &format!("[{}] ", index + 1),
                                            )
                                            .map(|evidence| {
                                                let _ = tx.send(Event::BatchEvidence {
                                                    job_id: job_id.clone(),
                                                    child_run_id: child_run_id.clone(),
                                                    evidence,
                                                });
                                                outcome
                                            })
                                        })
                                    } else {
                                        result
                                    }
                                }
                                Err(error) => Err(error),
                            };
                            match result {
                                Ok(outcome) => {
                                    if outcome == StreamOutcome::DeltaRequired {
                                        let _ = tx.send(Event::Line(format!(
                                            "[{}] Dovecot reports an incomplete synchronization; another delta pass is required",
                                            index + 1
                                        )));
                                        delta_required = true;
                                    }
                                    completed = true;
                                    break;
                                }
                                Err(error)
                                    if !error.contains("cancelled")
                                        && attempt < retry_count
                                        && is_transient_batch_error(&error) =>
                                {
                                    let _ = tx.send(Event::Line(format!(
                                        "[{}] [{}] transient failure; retrying: {error}",
                                        index + 1,
                                        classify_failure(&error).label()
                                    )));
                                    let _ = tx.send(Event::JobState {
                                        job_id: job_id.clone(),
                                        child_run_id: child_run_id.clone(),
                                        state: "Failed".into(),
                                    });
                                    let delay = Duration::from_secs(1_u64 << attempt.min(5));
                                    let started = std::time::Instant::now();
                                    while started.elapsed() < delay {
                                        if cancel.load(Ordering::Relaxed) {
                                            break;
                                        }
                                        thread::sleep(Duration::from_millis(100));
                                    }
                                    if cancel.load(Ordering::Relaxed) {
                                        break;
                                    }
                                }
                                Err(error) => {
                                    let cancelled = error.contains("cancelled");
                                    if !cancelled {
                                        failed.store(true, Ordering::Relaxed);
                                    }
                                    let _ = tx.send(Event::Line(format!(
                                        "[{}] [{}] failed: {error}",
                                        index + 1,
                                        classify_failure(&error).label()
                                    )));
                                    let _ = tx.send(Event::JobState {
                                        job_id: job_id.clone(),
                                        child_run_id: child_run_id.clone(),
                                        state: if cancelled {
                                            "Cancelled"
                                        } else {
                                            "Failed"
                                        }
                                        .into(),
                                    });
                                    let _ = tx.send(Event::JobFinished {
                                        job_id: job_id.clone(),
                                        child_run_id: child_run_id.clone(),
                                        state: if cancelled {
                                            "cancelled"
                                        } else {
                                            "failed"
                                        }
                                        .into(),
                                        detail: error,
                                    });
                                    if let Ok(mut terminal) = terminal_jobs.lock() {
                                        terminal.insert(index);
                                    }
                                    break;
                                }
                            }
                        }
                        if completed {
                            let terminal_state = if job.form.dry_run {
                                "ready"
                            } else if delta_required {
                                "delta_required"
                            } else {
                                "completed"
                            };
                            let _ = tx.send(Event::JobState {
                                job_id: job_id.clone(),
                                child_run_id: child_run_id.clone(),
                                state: if delta_required {
                                    "DeltaRequired"
                                } else {
                                    "Completed"
                                }
                                .into(),
                            });
                            let _ = tx.send(Event::JobFinished {
                                job_id: job_id.clone(),
                                child_run_id: child_run_id.clone(),
                                state: terminal_state.into(),
                                detail: if delta_required {
                                    "Dovecot reports that another delta pass is required".into()
                                } else {
                                    "process completed".into()
                                },
                            });
                            if let Ok(mut terminal) = terminal_jobs.lock() {
                                terminal.insert(index);
                            }
                        } else if cancel.load(Ordering::Relaxed) {
                            let _ = tx.send(Event::JobState {
                                job_id: job_id.clone(),
                                child_run_id: child_run_id.clone(),
                                state: "Cancelled".into(),
                            });
                            let _ = tx.send(Event::JobFinished {
                                job_id: job_id.clone(),
                                child_run_id: child_run_id.clone(),
                                state: "cancelled".into(),
                                detail: "cancelled by operator".into(),
                            });
                            if let Ok(mut terminal) = terminal_jobs.lock() {
                                terminal.insert(index);
                            }
                        }
                    }
                }));
            }
            let mut worker_panicked = false;
            for worker in workers {
                if worker.join().is_err() {
                    worker_panicked = true;
                    failed.store(true, Ordering::Relaxed);
                }
            }
            if worker_panicked {
                let unresolved = terminal_jobs
                    .lock()
                    .map(|terminal| {
                        (0..job_count)
                            .filter(|index| !terminal.contains(index))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_else(|_| (0..job_count).collect());
                for index in unresolved {
                    let Some(job_id) = queue_job_ids.get(index).cloned() else {
                        continue;
                    };
                    let Some(child_run_id) = child_run_ids.get(index).cloned() else {
                        continue;
                    };
                    let _ = tx.send(Event::Line(format!(
                        "[{}] worker stopped unexpectedly; job moved to Attention",
                        index + 1
                    )));
                    let _ = tx.send(Event::JobState {
                        job_id: job_id.clone(),
                        child_run_id: child_run_id.clone(),
                        state: "Attention".into(),
                    });
                    let _ = tx.send(Event::JobFinished {
                        job_id,
                        child_run_id,
                        state: "attention".into(),
                        detail: "worker stopped unexpectedly".into(),
                    });
                }
                let _ = tx.send(Event::Line(
                    "A batch worker stopped unexpectedly; unresolved jobs require review before retrying."
                        .into(),
                ));
            }
            let _ = tx.send(Event::Finished(if cancel.load(Ordering::Relaxed) {
                Err("batch cancelled".into())
            } else if failed.load(Ordering::Relaxed) {
                Err("one or more batch jobs failed".into())
            } else {
                Ok(StreamOutcome::Completed)
            }));
        });
    }
    fn running(&self) -> bool {
        self.receiver.is_some()
    }
    fn redact_output(&self, line: &str) -> String {
        let mut safe = line.to_owned();
        for secret in [
            self.form.source_password.as_str(),
            self.form.destination_password.as_str(),
        ] {
            if !secret.is_empty() {
                safe = safe.replace(secret, "[REDACTED]");
            }
        }
        safe
    }
    fn report_store_error<E: Display>(&mut self, operation: &str, result: Result<(), E>) {
        if let Err(error) = result {
            self.durability_error = true;
            push_visible_output(
                &mut self.output,
                format!("[durability] {operation} failed: {error}"),
            );
            self.status = format!("Durability error: {operation}");
        }
    }
    fn start(&mut self) {
        if !self.form.dry_run && !self.live_confirmed {
            self.live_confirm_open = true;
            return;
        }
        if !self.form.dry_run {
            self.live_confirmed = false;
        }
        if let Err(error) = self.form.load_configured_keyring_credentials() {
            self.status = error;
            return;
        }
        if let Err(e) = self.form.validate() {
            self.status = e;
            return;
        }
        if let (Some(project_id), Some(job_id)) = (self.project_id.clone(), self.job_id.clone()) {
            let identity_matches = self
                .store
                .project(&project_id)
                .ok()
                .flatten()
                .zip(self.store.mailbox_identity(&job_id).ok().flatten())
                .is_some_and(|(project, (source, destination, state))| {
                    let mailbox = core::MailboxJob {
                        id: job_id.clone(),
                        source_mailbox: source,
                        destination_mailbox: destination,
                        state,
                        config: None,
                    };
                    durable_single_identity_matches(&project, &mailbox, &self.form.profile)
                });
            if !identity_matches {
                if self.form.dry_run {
                    // A changed identity is a new durable plan. Keep the
                    // previous project history intact and create a fresh
                    // project/job below rather than attaching the run to the
                    // old mailbox record.
                    self.project_id = None;
                    self.job_id = None;
                } else {
                    self.status = "The current mailbox identity differs from the durable project. Run a new dry preflight for this plan before starting live migration.".into();
                    return;
                }
            }
        }
        if !self.form.dry_run && self.form.requires_insecure_transport_ack() {
            self.status = "Live migration blocked: acknowledge the cleartext source-transport risk before continuing.".into();
            return;
        }
        self.durability_error = false;
        if !self.form.dry_run {
            if !self.persistence_available {
                self.status =
                    "Live migration is disabled because durable SQLite storage is unavailable."
                        .into();
                return;
            }
            let preflight_ready = self
                .project_id
                .as_deref()
                .and_then(|id| self.store.project(id).ok().flatten())
                .is_some_and(|project| {
                    matches!(
                        project.phase,
                        core::Phase::Preflight
                            | core::Phase::Pilot
                            | core::Phase::Seed
                            | core::Phase::CatchUp
                            | core::Phase::FinalDelta
                            | core::Phase::Verification
                    )
                });
            let plan_matches = self.job_id.as_deref().is_some_and(|job| {
                self.store
                    .preflight_plan(job)
                    .ok()
                    .flatten()
                    .is_some_and(|plan| plan == self.form.plan_fingerprint())
            });
            let mailbox_ready = self.job_id.as_deref().is_some_and(|job| {
                self.store
                    .mailbox_state(job)
                    .ok()
                    .flatten()
                    .is_some_and(|state| state == "ready" || state == "delta_required")
            });
            if !preflight_ready || !mailbox_ready || !plan_matches {
                self.status = "Run a successful dry preflight for this exact mailbox plan before starting live migration.".into();
                return;
            }
        }
        if self.project_id.is_none() {
            if let Ok((project, job)) = self.store.create_project_with_mailbox(
                &self.form.profile.name,
                &self.form.profile.source_host,
                &self.form.profile.destination_host,
                &self.form.profile.source_user,
                &self.form.profile.destination_user,
            ) {
                self.project_id = Some(project.id.clone());
                self.job_id = Some(job);
            } else {
                self.status = "Could not create durable migration project".into();
                return;
            }
        }
        let prepared = match self.form.prepared_command() {
            Ok(command) => command,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        let exe = prepared.executable;
        let args = prepared.args;
        let cleanup = prepared.cleanup;
        let prepared_env = prepared.env;
        let plan_fingerprint = self.form.plan_fingerprint();
        let plan_snapshot = self.form.plan_snapshot();
        let run_engine = self.form.engine();
        let run_dry_run = self.form.dry_run;
        let (run_project_id, run_job_id) = match (self.project_id.clone(), self.job_id.clone()) {
            (Some(project), Some(job)) => (project, job),
            _ => {
                cleanup_paths(&cleanup);
                self.status = "Could not start without a durable mailbox project.".into();
                return;
            }
        };
        let run_id = uuid::Uuid::new_v4().to_string();
        if let Err(error) = self.store.begin_run_with_snapshot(
            &run_project_id,
            &run_job_id,
            &run_id,
            run_engine.label(),
            &plan_snapshot,
        ) {
            cleanup_paths(&cleanup);
            self.status = format!("Could not record durable run; nothing was started: {error}");
            return;
        }
        self.run_id = Some(run_id.clone());
        self.active_run = Some(ActiveRunContext {
            run_id: run_id.clone(),
            project_id: run_project_id,
            job_id: Some(run_job_id.clone()),
            batch_job_ids: Vec::new(),
            batch_plan_fingerprints: Vec::new(),
            batch_child_run_ids: Vec::new(),
            kind: RunKind::Single,
            dry_run: run_dry_run,
            engine: run_engine,
            plan_fingerprint,
        });
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_requested = Some(cancel.clone());
        self.receiver = Some(rx);
        self.run_started_at = Some(std::time::Instant::now());
        self.status = if self.form.dry_run {
            "Dry run in progress".into()
        } else {
            "Sync in progress".into()
        };
        self.output = VecDeque::from([format!(
            "Starting {} with {}…",
            if self.form.dry_run {
                "safe dry run"
            } else {
                "synchronization"
            },
            self.form.engine().label()
        )]);
        let verification = if !self.form.dry_run && self.form.engine() == core::Engine::Dovecot {
            self.form.dovecot_verification_commands(false)
        } else {
            Vec::new()
        };
        let destination_preflight =
            if self.form.dry_run && self.form.engine() == core::Engine::Dovecot {
                self.form.dovecot_destination_preflight_commands()
            } else {
                Vec::new()
            };
        let verification_secret = self.form.source_password.to_string();
        let verification_env = if self.form.local_doveadm() {
            vec![(
                "MAILSWIFTSYNC_IMAPC_PASSWORD".into(),
                self.form.source_password.to_string(),
            )]
        } else {
            Vec::new()
        };
        let output_secrets = vec![
            self.form.source_password.to_string(),
            self.form.destination_password.to_string(),
        ];
        let migration_timeout =
            Duration::from_secs(self.form.profile.migration_timeout_hours * 60 * 60);
        let process_job_id = run_job_id.clone();
        let cleanup_guard = CleanupGuard::new(cleanup.clone());
        thread::spawn(move || {
            let _cleanup_guard = cleanup_guard;
            let mut result = run_streaming(
                &exe,
                &args,
                &prepared_env,
                &tx,
                &run_id,
                &process_job_id,
                "",
                &cancel,
                &output_secrets,
                migration_timeout,
                run_engine == core::Engine::Dovecot && !run_dry_run,
            );
            if result.is_ok() && !destination_preflight.is_empty() {
                result = result.and_then(|outcome| {
                    run_dovecot_destination_preflight(
                        &destination_preflight,
                        &tx,
                        &cancel,
                        migration_timeout,
                        "",
                    )
                    .map(|_| outcome)
                });
            }
            if result.is_ok() && !verification.is_empty() {
                result = result.and_then(|stream| {
                    run_dovecot_verification(
                        &verification,
                        &verification_env,
                        std::slice::from_ref(&verification_secret),
                        &tx,
                        &cancel,
                        migration_timeout,
                        "",
                    )
                    .map(|evidence| {
                        let _ = tx.send(Event::Evidence(evidence));
                        stream
                    })
                    .map_err(|error| {
                        let _ = tx.send(Event::VerificationFailed(error.clone()));
                        format!("migration completed; Dovecot verification failed: {error}")
                    })
                });
            }
            if let Ok(stream) = &result
                && let Some(evidence) = stream.imapsync_evidence.clone()
            {
                let _ = tx.send(Event::Evidence(evidence));
            }
            let _ = tx.send(Event::Finished(result.map(|stream| stream.outcome)));
        });
    }
    fn poll(&mut self) {
        if let Some(receiver) = &self.capability_receiver
            && let Ok(result) = receiver.try_recv()
        {
            match result {
                Ok((source, destination)) => {
                    self.source_capabilities = Some(source);
                    self.destination_capabilities = Some(destination);
                    self.status = "Capability discovery complete".into();
                    self.assess_plan();
                }
                Err(error) => self.status = format!("Preflight discovery failed: {error}"),
            }
            self.capability_receiver = None;
        }
        let mut done = None;
        let mut pending_db_events: Vec<(String, String, String, String)> = Vec::new();
        let mut durability_errors = Vec::new();
        let active_run = self.active_run.clone();
        if let Some(rx) = &self.receiver {
            while let Ok(event) = rx.try_recv() {
                match event {
                    Event::ClaimBatch {
                        project_id,
                        job_id,
                        parent_run_id,
                        child_run_id,
                        reply,
                    } => {
                        let result = self
                            .store
                            .claim_batch_mailbox_for_child(
                                &project_id,
                                &job_id,
                                &parent_run_id,
                                &child_run_id,
                            )
                            .map_err(|error| error.to_string());
                        if let Err(error) = &result {
                            durability_errors.push(format!(
                                "durable claim for child run {child_run_id} failed: {error}"
                            ));
                        }
                        let _ = reply.send(result);
                    }
                    Event::ProcessStarted(
                        process_run_id,
                        job_id,
                        pid,
                        start_ticks,
                        process_group,
                        session_id,
                        executable,
                    ) => {
                        if active_run.is_some()
                            && let Err(error) = self.store.register_process(&core::ActiveProcess {
                                run_id: process_run_id,
                                job_id,
                                pid,
                                start_ticks,
                                process_group,
                                session_id,
                                executable,
                            })
                        {
                            durability_errors
                                .push(format!("persist process identity failed: {error}"));
                        }
                    }
                    Event::Line(s) => {
                        // Runner threads redact secrets before publishing events.
                        // Do not re-read mutable form fields here: the operator
                        // may have edited the next plan while this run was active.
                        let safe = s;
                        if let Some(project) =
                            active_run.as_ref().map(|run| run.project_id.as_str())
                            && let Some(run_id) = active_run.as_ref().map(|run| run.run_id.as_str())
                        {
                            pending_db_events.push((
                                project.to_owned(),
                                run_id.to_owned(),
                                "run_output".into(),
                                safe.clone(),
                            ));
                        }
                        push_visible_output(&mut self.output, safe);
                    }
                    Event::JobState {
                        job_id,
                        child_run_id,
                        state,
                    } => {
                        if let Some(run) = active_run.as_ref()
                            && matches!(run.kind, RunKind::Batch)
                            && let Some(index) =
                                run.batch_job_ids.iter().position(|id| id == &job_id)
                            && run.batch_child_run_ids.get(index) == Some(&child_run_id)
                        {
                            if let Some(job) = self.bulk_jobs.get_mut(index) {
                                job.state = state.clone();
                            }
                            let durable_state = match state.as_str() {
                                "Running" => "running",
                                "Completed" if !self.bulk_live_run => "ready",
                                "Completed" => "completed",
                                "DeltaRequired" => "delta_required",
                                "Failed" => "failed",
                                "Cancelled" => "cancelled",
                                "Queued" => "queued",
                                _ => "attention",
                            };
                            // A Running event is informational. The worker has
                            // already received an acknowledged ClaimBatch
                            // response before it can launch the process; do
                            // not perform a second asynchronous claim here.
                            let result = if durable_state == "running" {
                                Ok(())
                            } else {
                                self.store.set_mailbox_state(&job_id, durable_state)
                            };
                            if let Err(error) = result {
                                durability_errors
                                    .push(format!("persist batch mailbox state failed: {error}"));
                            }
                            if state == "Completed"
                                && !self.bulk_live_run
                                && let Some(fingerprint) = run.batch_plan_fingerprints.get(index)
                                && let Err(error) =
                                    self.store.set_preflight_plan(&job_id, fingerprint)
                            {
                                durability_errors
                                    .push(format!("persist batch preflight plan failed: {error}"));
                            }
                        } else {
                            durability_errors.push(format!(
                                "ignored batch state event for unknown child run {child_run_id}"
                            ));
                        }
                    }
                    Event::JobFinished {
                        job_id,
                        child_run_id,
                        state,
                        detail,
                    } => {
                        if let Some(run) = active_run.as_ref()
                            && matches!(run.kind, RunKind::Batch)
                            && let Some(index) =
                                run.batch_job_ids.iter().position(|id| id == &job_id)
                            && run.batch_child_run_ids.get(index) == Some(&child_run_id)
                        {
                            let run_status = if matches!(
                                state.as_str(),
                                "ready" | "completed" | "delta_required"
                            ) {
                                "completed"
                            } else if state == "cancelled" {
                                "cancelled"
                            } else {
                                "failed"
                            };
                            let evidence = self.pending_batch_evidence.remove(&child_run_id);
                            let final_state = evidence.as_ref().map_or(state.clone(), |value| {
                                if value.is_exact_match() && state != "delta_required" {
                                    "verified".into()
                                } else if state == "delta_required" {
                                    "delta_required".into()
                                } else {
                                    "verification_difference".into()
                                }
                            });
                            if let Some(index) =
                                run.batch_job_ids.iter().position(|id| id == &job_id)
                                && let Some(job) = self.bulk_jobs.get_mut(index)
                            {
                                job.state = display_job_state(&final_state).into();
                            }
                            let result = if let Some(value) = evidence.as_ref() {
                                self.store.finish_run_for_mailbox_with_evidence(
                                    &run.project_id,
                                    &job_id,
                                    &child_run_id,
                                    run_status,
                                    &final_state,
                                    &detail,
                                    value,
                                )
                            } else {
                                self.store.finish_run_for_mailbox(
                                    &run.project_id,
                                    &job_id,
                                    &child_run_id,
                                    run_status,
                                    &final_state,
                                    &detail,
                                )
                            };
                            if let Err(error) = result {
                                durability_errors.push(format!(
                                    "persist child run {} completion failed: {error}",
                                    index + 1
                                ));
                            }
                        } else {
                            durability_errors.push(format!(
                                "ignored batch completion event for unknown child run {child_run_id}"
                            ));
                        }
                    }
                    Event::BatchEvidence {
                        job_id,
                        child_run_id,
                        evidence,
                    } => {
                        if let Some(run) = active_run.as_ref()
                            && matches!(run.kind, RunKind::Batch)
                            && run
                                .batch_job_ids
                                .iter()
                                .position(|id| id == &job_id)
                                .and_then(|index| run.batch_child_run_ids.get(index))
                                == Some(&child_run_id)
                        {
                            self.pending_batch_evidence.insert(child_run_id, evidence);
                        } else {
                            durability_errors.push(format!(
                                "ignored evidence event for unknown child run {child_run_id}"
                            ));
                        }
                    }
                    Event::Evidence(evidence) => {
                        let evidence_level = evidence.evidence_level();
                        // Hold evidence until Finished so its history, run
                        // status, mailbox state, and terminal event commit
                        // together. In particular, this permits the
                        // evidence-backed running -> verified transition
                        // without weakening ordinary state transitions.
                        self.pending_evidence = Some(evidence);
                        if let Some(project) =
                            active_run.as_ref().map(|run| run.project_id.as_str())
                            && let Some(run_id) = active_run.as_ref().map(|run| run.run_id.as_str())
                        {
                            pending_db_events.push((
                                project.to_owned(),
                                run_id.to_owned(),
                                "verification_evidence".into(),
                                format!("evidence level: {evidence_level}"),
                            ));
                        }
                    }
                    Event::VerificationFailed(detail) => {
                        let safe = self.redact_output(&detail);
                        push_visible_output(&mut self.output, format!("[verification] {safe}"));
                        if let Some(project) =
                            active_run.as_ref().map(|run| run.project_id.as_str())
                            && let Some(run_id) = active_run.as_ref().map(|run| run.run_id.as_str())
                        {
                            pending_db_events.push((
                                project.to_owned(),
                                run_id.to_owned(),
                                "verification_pending".into(),
                                safe,
                            ));
                        }
                    }
                    Event::Finished(r) => done = Some(r),
                }
            }
        }
        if !pending_db_events.is_empty() {
            let batch = pending_db_events
                .iter()
                .map(|(_, _, kind, detail)| (kind.as_str(), detail.as_str()))
                .collect::<Vec<_>>();
            let result = active_run.as_ref().map_or_else(
                || Err(rusqlite::Error::InvalidQuery),
                |run| self.store.record_run_events_batch(&run.run_id, &batch),
            );
            self.report_store_error("record execution events", result);
        }
        for error in durability_errors {
            self.report_store_error("batch event persistence", Err(error));
        }
        if let Some(r) = done {
            let Some(run_context) = active_run else {
                self.status = "Execution completed without a durable run context".into();
                self.receiver = None;
                return;
            };
            let succeeded = r.is_ok();
            let delta_required = r
                .as_ref()
                .is_ok_and(|outcome| *outcome == StreamOutcome::DeltaRequired);
            let was_bulk_run = matches!(run_context.kind, RunKind::Batch);
            let mut direct_final_state = None;
            let terminal_evidence = if succeeded && !run_context.dry_run {
                self.pending_evidence.take().or_else(|| {
                    (run_context.engine == core::Engine::ImapSync)
                        .then(|| {
                            let output = self.output.iter().cloned().collect::<Vec<_>>();
                            verification::parse_imapsync_evidence(&output)
                        })
                        .flatten()
                })
            } else {
                self.pending_evidence.take()
            };
            if succeeded && !run_context.dry_run && terminal_evidence.is_none() {
                let _ = self.store.record_event(
                    &run_context.project_id,
                    "verification_pending",
                    "completed transfer did not provide complete verification evidence",
                );
            }
            if !was_bulk_run && let Some(job) = &run_context.job_id {
                if succeeded && run_context.dry_run {
                    let _ = self
                        .store
                        .set_preflight_plan(job, &run_context.plan_fingerprint);
                }
                let final_state = if succeeded && run_context.dry_run {
                    "ready"
                } else if !succeeded {
                    if r.as_ref()
                        .err()
                        .is_some_and(|error| error.contains("verification"))
                    {
                        "attention"
                    } else if r
                        .as_ref()
                        .err()
                        .is_some_and(|error| error.contains("cancelled"))
                    {
                        "cancelled"
                    } else {
                        "failed"
                    }
                } else if let Some(evidence) = terminal_evidence.as_ref() {
                    if evidence.is_exact_match() && !delta_required {
                        "verified"
                    } else if delta_required {
                        "delta_required"
                    } else {
                        "verification_difference"
                    }
                } else if !run_context.dry_run {
                    "attention"
                } else {
                    "completed"
                };
                direct_final_state = Some(final_state);
            }
            {
                let project = &run_context.project_id;
                let run_id = &run_context.run_id;
                let run_status = if succeeded {
                    "completed"
                } else if r
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.contains("verification"))
                {
                    "verification_failed"
                } else if r
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.contains("cancelled"))
                {
                    "cancelled"
                } else {
                    "failed"
                };
                let detail = r
                    .as_ref()
                    .err()
                    .map(|error| classified_failure_detail(error))
                    .unwrap_or_default();
                let terminal_write = if !was_bulk_run
                    && let (Some(job), Some(state)) = (&run_context.job_id, direct_final_state)
                {
                    if run_status == "completed" {
                        if let Some(evidence) = terminal_evidence.as_ref() {
                            self.store.finish_run_for_mailbox_with_evidence(
                                project, job, run_id, run_status, state, &detail, evidence,
                            )
                        } else {
                            self.store.finish_run_for_mailbox(
                                project, job, run_id, run_status, state, &detail,
                            )
                        }
                    } else {
                        self.store.finish_run_for_mailbox(
                            project, job, run_id, run_status, state, &detail,
                        )
                    }
                } else {
                    self.store.finish_run(run_id, run_status, &detail)
                };
                if let Err(error) = terminal_write {
                    push_visible_output(
                        &mut self.output,
                        format!("[durability] Could not persist terminal state: {error}"),
                    );
                }
                if was_bulk_run {
                    let result = self.store.record_event(
                        project,
                        "run_finished",
                        if succeeded { "success" } else { "failure" },
                    );
                    self.report_store_error("record batch run completion", result);
                }
                if succeeded {
                    if run_context.dry_run {
                        let result = self.store.transition(project, core::Phase::Preflight);
                        self.report_store_error("advance project phase", result);
                    } else {
                        let fully_verified = self.store.all_mailboxes_verified(project);
                        match fully_verified {
                            Ok(true) => {
                                let verification =
                                    self.store.transition(project, core::Phase::Verification);
                                if let Err(error) = verification {
                                    self.report_store_error(
                                        "advance project to Verification",
                                        Err(error),
                                    );
                                } else {
                                    let complete =
                                        self.store.transition(project, core::Phase::Complete);
                                    self.report_store_error(
                                        "complete fully verified project",
                                        complete,
                                    );
                                }
                            }
                            Ok(false) => {
                                let result =
                                    self.store.transition(project, core::Phase::Verification);
                                self.report_store_error("advance project phase", result);
                            }
                            Err(error) => self.report_store_error(
                                "check project verification before completion",
                                Err(error),
                            ),
                        }
                    }
                } else {
                    let result = self.store.transition(project, core::Phase::Attention);
                    self.report_store_error("move project to Attention", result);
                }
            }
            self.status = if self.durability_error {
                "Migration result requires durability review".into()
            } else {
                match r {
                    Ok(_) => Self::successful_run_status(
                        run_context.dry_run,
                        was_bulk_run,
                        direct_final_state,
                    )
                    .into(),
                    Err(e) => format!("Failed: {e}"),
                }
            };
            self.receiver = None;
            self.cancel_requested = None;
            self.run_started_at = None;
            self.run_id = None;
            self.active_run = None;
            self.pending_batch_evidence.clear();
            // Keep the durable queue after completion so a validated batch
            // can be promoted to live execution, and failed/live jobs can be
            // deliberately retried or run through another delta pass.
            if !was_bulk_run {
                self.bulk_project_id = None;
                self.bulk_job_ids.clear();
            }
            self.bulk_live_run = false;
            self.live_confirmed = false;
        }
    }
    fn successful_run_status(
        dry_run: bool,
        was_bulk_run: bool,
        final_state: Option<&str>,
    ) -> &'static str {
        if dry_run {
            "Preflight completed successfully"
        } else if was_bulk_run {
            "Batch transfer completed; review per-mailbox verification results"
        } else {
            match final_state {
                Some("verified") => "Migration completed and verified",
                Some("delta_required") => "Migration completed; final delta or review required",
                Some("verification_difference") => {
                    "Migration completed; verification found differences requiring review"
                }
                _ => "Migration completed; verification requires operator review",
            }
        }
    }

    fn account(
        ui: &mut egui::Ui,
        title: &str,
        host: &mut String,
        user: &mut String,
        password: &mut String,
        color: Color32,
    ) {
        let inline_error = |ui: &mut egui::Ui, label: &str, value: &str, required: bool| {
            let message = if required && value.trim().is_empty() {
                Some(format!("{label} is required."))
            } else if !value.is_empty() && value.chars().any(char::is_control) {
                Some(format!("{label} contains an invalid control character."))
            } else {
                None
            };
            if let Some(message) = message {
                ui.label(RichText::new(message).color(ALERT).size(11.0));
            }
        };
        ui.group(|ui| {
            ui.heading(RichText::new(title).color(color));
            ui.label(RichText::new("IMAP connection").size(11.0).color(MUTED));
            ui.horizontal(|ui| {
                ui.label("Server");
                ui.text_edit_singleline(host);
            });
            inline_error(ui, "Server", host, true);
            ui.horizontal(|ui| {
                ui.label("User");
                ui.text_edit_singleline(user);
            });
            inline_error(ui, "User", user, true);
            ui.horizontal(|ui| {
                ui.label("Password");
                let visibility_id = ui.make_persistent_id(title).with("password_visibility");
                let visible = ui
                    .ctx()
                    .data_mut(|data| data.get_temp::<bool>(visibility_id).unwrap_or(false));
                ui.add(egui::TextEdit::singleline(password).password(!visible));
                if ui.button(if visible { "Hide" } else { "Show" }).clicked() {
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(visibility_id, !visible));
                }
            });
            inline_error(ui, "Password", password, title.starts_with("01"));
        });
    }
    fn preview(&mut self, ctx: &egui::Context) {
        if !self.preview {
            return;
        }
        egui::Window::new("Command preview")
            .open(&mut self.preview)
            .default_width(670.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Passwords are redacted. They are never written to the saved profile.",
                    )
                    .color(MUTED),
                );
                let (exe, args) = self.form.command(true);
                let mut cmd = format!("{} {}", exe, args.join(" "));
                ui.add(
                    egui::TextEdit::multiline(&mut cmd)
                        .code_editor()
                        .desired_rows(10)
                        .interactive(false),
                );
            });
    }
    fn bulk_dialog(&mut self, ctx: &egui::Context) {
        if !self.bulk_open {
            return;
        }
        let mut open = self.bulk_open;
        egui::Window::new("Batch migration queue").open(&mut open).default_width(850.0).default_height(540.0).show(ctx, |ui| {
            ui.heading("Import → review → validate");
            ui.label(RichText::new(&self.bulk_message).color(MUTED));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.running(), egui::Button::new("Import CSV / XLSX…")).clicked() && let Some(path) = rfd::FileDialog::new().add_filter("Migration lists", &["csv", "xls", "xlsx"]).pick_file() { self.import_bulk(&path); }
                if ui.add_enabled(!self.running(), egui::Button::new("Clear queue")).clicked() {
                    self.bulk_jobs.clear();
                    self.bulk_project_id = None;
                    self.bulk_job_ids.clear();
                    self.bulk_message = "Queue cleared; its durable batch association was discarded.".into();
                }
                let label = if self.form.dry_run {
                    format!("Run {} dry validations", self.bulk_jobs.len())
                } else {
                    format!("Run {} live migrations", self.bulk_jobs.len())
                };
                if ui.add_enabled(!self.running() && !self.bulk_jobs.is_empty(), egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(BLUE)).clicked() { self.start_bulk(); }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label("Concurrent validations");
                ui.add(egui::Slider::new(&mut self.form.profile.batch_concurrency, 1..=16));
                ui.label(RichText::new("bounded 1–16 workers").size(11.0).color(MUTED));
            });
            ui.horizontal(|ui| {
                ui.label("Transient retries");
                ui.add(egui::Slider::new(&mut self.form.profile.batch_retry_count, 0..=3));
                ui.label(RichText::new("auth/configuration failures are never retried").size(11.0).color(MUTED));
            });
            ui.label(RichText::new("Passwordless queue credentials").strong());
            ui.label(RichText::new("Apply an existing OS-keyring reference to rows that do not already have a password or credential ID. The secret itself is never copied into the queue.").size(11.0).color(MUTED));
            let queue_editable = !self.running();
            let mut apply_source = false;
            let mut apply_destination = false;
            ui.horizontal(|ui| {
                ui.label("Source keyring ID");
                ui.add_enabled(
                    queue_editable,
                    egui::TextEdit::singleline(&mut self.bulk_source_keyring_apply)
                        .desired_width(180.0),
                );
                if ui
                    .add_enabled(queue_editable, egui::Button::new("Apply to empty source rows"))
                    .clicked()
                {
                    apply_source = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Destination keyring ID");
                ui.add_enabled(
                    queue_editable,
                    egui::TextEdit::singleline(&mut self.bulk_destination_keyring_apply)
                        .desired_width(180.0),
                );
                if ui
                    .add_enabled(
                        queue_editable,
                        egui::Button::new("Apply to empty destination rows"),
                    )
                    .clicked()
                {
                    apply_destination = true;
                }
            });
            if apply_source {
                self.apply_bulk_keyring_id(true);
            }
            if apply_destination {
                self.apply_bulk_keyring_id(false);
            }
            ui.label(RichText::new("Required columns: source_host, source_user, destination_host, destination_user. Optional: source_password, destination_password, source_credential_id, destination_credential_id, name. Engine options remain trusted application settings and cannot be imported from a spreadsheet. Enter missing credentials in the masked fields below.").size(11.0).color(MUTED));
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("bulk_jobs").striped(true).min_col_width(120.0).show(ui, |ui| {
                    ui.strong("#"); ui.strong("Migration"); ui.strong("Source"); ui.strong("Destination"); ui.strong("Source password"); ui.strong("Destination password"); ui.strong("Status"); ui.end_row();
                    for (index, job) in self.bulk_jobs.iter_mut().enumerate() {
                        ui.label((index + 1).to_string());
                        ui.label(&job.label);
                        ui.label(format!("{}\n{}", job.form.profile.source_host, job.form.profile.source_user));
                        ui.label(format!("{}\n{}", job.form.profile.destination_host, job.form.profile.destination_user));
                        ui.add(egui::TextEdit::singleline(&mut *job.form.source_password).password(true).desired_width(120.0));
                        if job.form.engine() == core::Engine::Dovecot { ui.label("Not required"); } else { ui.add(egui::TextEdit::singleline(&mut *job.form.destination_password).password(true).desired_width(120.0)); }
                        ui.label(RichText::new(&job.state).color(TEAL));
                        ui.end_row();
                    }
                });
            });
            ui.add_space(8.0); ui.label(RichText::new("Imported passwords are used only for this open queue. Saving a profile never saves them.").size(11.0).color(ALERT));
        });
        self.bulk_open = open;
    }
    fn bulk_live_confirmation(&mut self, ctx: &egui::Context) {
        if !self.bulk_live_confirm_open {
            return;
        }
        let mut open = self.bulk_live_confirm_open;
        let mut close = false;
        egui::Window::new("Confirm live batch migration")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading(RichText::new("This will change destination mailboxes").color(ALERT));
                ui.label(format!(
                    "{} queued mailbox processes may run concurrently.",
                    self.bulk_jobs.len()
                ));
                ui.label("Each mailbox must already have a matching successful dry validation. Source mail is not deleted by default.");
                ui.label(RichText::new("Review the queue, concurrency, throttles, and exact plans before continuing.").color(MUTED));
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("I understand — start batch").color(Color32::WHITE)).fill(ALERT))
                        .clicked()
                    {
                        close = true;
                        self.bulk_live_confirmed = true;
                        self.start_bulk();
                    }
                });
            });
        self.bulk_live_confirm_open = open && !close;
    }
    fn advanced_dialog(&mut self, ctx: &egui::Context) {
        if !self.advanced_open {
            return;
        }
        egui::Window::new("Advanced migration options").open(&mut self.advanced_open).default_width(620.0).show(ctx, |ui| {
            ui.label(RichText::new("These controls affect the imapsync fallback. Dovecot-native migrations use doveadm and server-side consistency rules.").color(MUTED));
            ui.add_space(8.0);
            ui.group(|ui| { ui.heading("Reliability and metadata"); ui.checkbox(&mut self.form.profile.sync_internaldates, "Sync internal dates  (--syncinternaldates)"); ui.checkbox(&mut self.form.profile.useuid, "Use message UIDs when available  (--useuid)"); ui.checkbox(&mut self.form.profile.usecache, "Use imapsync cache  (--usecache)"); ui.checkbox(&mut self.form.profile.allowsizemismatch, "Allow message-size mismatch  (--allowsizemismatch)"); });
            ui.add_space(8.0);
            ui.group(|ui| {
                ui.heading("Performance");
                ui.checkbox(&mut self.form.profile.fastio1, "Fast I/O for source  (--fastio1)")
                    .on_hover_text("Uses imapsync's faster source I/O path; test this with the provider before a production cutover.");
                ui.checkbox(&mut self.form.profile.fastio2, "Fast I/O for destination  (--fastio2)")
                    .on_hover_text("Uses imapsync's faster destination I/O path; provider behavior varies.");
                ui.horizontal(|ui| {
                    ui.label("Messages/second target (0 = unlimited)")
                        .on_hover_text("For a batch this is an aggregate target: MailSwiftSync divides it across concurrent imapsync workers. A single run uses the value unchanged.");
                    ui.add(egui::DragValue::new(&mut self.form.profile.max_messages_per_second).range(0..=100_000));
                });
                ui.horizontal(|ui| {
                    ui.label("Bytes/second target (0 = unlimited)")
                        .on_hover_text("For a batch this is an aggregate target: MailSwiftSync divides it across concurrent imapsync workers. A single run uses the value unchanged.");
                    ui.add(egui::DragValue::new(&mut self.form.profile.max_bytes_per_second).range(0..=u64::MAX));
                });
                ui.horizontal(|ui| {
                    ui.label("Process timeout (hours)")
                        .on_hover_text("Maximum wall-clock time for one engine process. It is a safety bound, not an estimate of completion time.");
                    ui.add(egui::DragValue::new(&mut self.form.profile.migration_timeout_hours).range(1..=720));
                });
                ui.label(RichText::new("Batch targets are divided across workers and process starts are globally paced; provider-side limits still take precedence. A finite target must be at least the worker count.").size(11.0).color(MUTED));
            });
            ui.add_space(8.0);
            ui.group(|ui| { ui.heading(RichText::new("Destructive destination option").color(ALERT)); ui.checkbox(&mut self.form.profile.delete2, "Delete destination messages missing from source  (--delete2)"); ui.label(RichText::new("Use only for an intentionally exact backup after a tested dry run. This can remove destination mail.").size(11.0).color(ALERT)); });
                ui.add_space(8.0); ui.label("The Extra imapsync options field accepts only the documented safe tuning and diagnostic allowlist. Connection, credential, TLS, destructive, logging, and unknown flags are rejected.");
        });
    }
    fn keyring_dialog(&mut self, ctx: &egui::Context) {
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
                        "Keyring IDs are non-secret references saved in the profile. Passwords stay in the operating system credential store and are loaded only into the active session.",
                    )
                    .color(MUTED),
                );
                ui.add_space(8.0);
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
                    if ui.button("Store source password").clicked() {
                        self.status = match self.form.store_keyring_password(true) {
                            Ok(()) => "Source password stored in OS keyring".into(),
                            Err(error) => error,
                        };
                    }
                    if ui.button("Load source").clicked() {
                        self.status = match self.form.load_keyring_password(true) {
                            Ok(()) => "Source credential loaded".into(),
                            Err(error) => error,
                        };
                    }
                    if ui.button("Delete source").clicked() {
                        self.status = match self.form.delete_keyring_password(true) {
                            Ok(()) => "Source credential deleted from OS keyring".into(),
                            Err(error) => error,
                        };
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Store destination password").clicked() {
                        self.status = match self.form.store_keyring_password(false) {
                            Ok(()) => "Destination password stored in OS keyring".into(),
                            Err(error) => error,
                        };
                    }
                    if ui.button("Load destination").clicked() {
                        self.status = match self.form.load_keyring_password(false) {
                            Ok(()) => "Destination credential loaded".into(),
                            Err(error) => error,
                        };
                    }
                    if ui.button("Delete destination").clicked() {
                        self.status = match self.form.delete_keyring_password(false) {
                            Ok(()) => "Destination credential deleted from OS keyring".into(),
                            Err(error) => error,
                        };
                    }
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "This is password storage, not OAuth/Modern Auth. Do not use it as a substitute for provider-specific OAuth setup or unattended secret brokering.",
                    )
                    .size(11.0)
                    .color(ALERT),
                );
            });
        self.keyring_open = open;
    }
    fn engine_dialog(&mut self, ctx: &egui::Context) {
        if !self.engine_open {
            return;
        }
        let mut open = self.engine_open;
        let mut close_requested = false;
        egui::Window::new("Choose migration engine").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.heading("How should this migration run?");
            ui.label(RichText::new("Select the execution engine that fits the destination. MailSwiftSync owns planning, safety gates, orchestration, and verification; the selected engine owns message transfer.").color(MUTED));
            ui.add_space(8.0);
            for engine in [core::Engine::Auto, core::Engine::Dovecot, core::Engine::ImapSync] {
                ui.radio_value(&mut self.form.profile.engine, engine, engine.label());
                if self.form.profile.engine == engine {
                    ui.label(RichText::new(engine.description()).size(11.0).color(MUTED));
                }
            }
            if self.form.profile.engine == core::Engine::Dovecot {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("doveadm execution").on_hover_text(
                        "Choose where doveadm runs. Automatic preserves legacy localhost/SSH inference; explicit modes avoid hostname ambiguity.",
                    );
                    egui::ComboBox::from_id_salt("dovecot_execution")
                        .selected_text(match self.form.profile.dovecot_execution.as_str() {
                            "local" => "Local machine",
                            "ssh" => "Destination over SSH",
                            _ => "Automatic (legacy inference)",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.form.profile.dovecot_execution,
                                "local".into(),
                                "Local machine",
                            );
                            ui.selectable_value(
                                &mut self.form.profile.dovecot_execution,
                                "ssh".into(),
                                "Destination over SSH",
                            );
                            ui.selectable_value(
                                &mut self.form.profile.dovecot_execution,
                                "automatic".into(),
                                "Automatic (legacy inference)",
                            );
                        });
                });
                ui.horizontal(|ui| { ui.label("doveadm"); ui.text_edit_singleline(&mut self.form.profile.doveadm_path); });
                ui.horizontal(|ui| { ui.label("SSH executable"); ui.text_edit_singleline(&mut self.form.profile.ssh_path); });
                ui.horizontal(|ui| { ui.label("SSH user (optional)"); ui.text_edit_singleline(&mut self.form.profile.dovecot_ssh_user); });
                ui.horizontal(|ui| { ui.label("Config"); ui.text_edit_singleline(&mut self.form.profile.dovecot_config); });
                ui.checkbox(&mut self.form.profile.allow_remote_password_in_argv, "I understand the remote Dovecot password may be visible in process arguments");
                ui.label(RichText::new("Required only for remote execution until keyring/secret-broker delivery is available. Never enable this on an untrusted destination.").size(11.0).color(ALERT));
                ui.label(RichText::new("Dry mode only lists the destination mailbox. A live run uses sync -1; enabling destination deletion switches to backup.").size(11.0).color(MUTED));
            }
            ui.add_space(8.0);
            if ui.button("Continue to migration plan").clicked() { close_requested = true; }
        });
        self.engine_open = open && !close_requested;
    }

    fn live_confirmation(&mut self, ctx: &egui::Context) {
        if !self.live_confirm_open {
            return;
        }
        let mut open = self.live_confirm_open;
        let mut close_requested = false;
        egui::Window::new("Confirm live migration").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.heading(RichText::new("Destination changes require confirmation").color(ALERT));
            ui.label(format!("This will invoke {} with the current credentials and rules.", self.form.engine().label()));
            ui.add_space(8.0);
            ui.label(RichText::new(format!("Project: {}", self.form.profile.name)).strong());
            ui.label(format!("{}  →  {}", self.form.profile.source_host, self.form.profile.destination_host));
            ui.label("Source mail is not deleted by default. Destination deletion is disabled unless explicitly enabled in Advanced options.");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() { close_requested = true; }
                if ui.add(egui::Button::new(RichText::new("I understand — start migration").color(Color32::WHITE)).fill(ALERT)).clicked() { close_requested = true; self.live_confirmed = true; self.start(); }
            });
        });
        self.live_confirm_open = open && !close_requested;
    }
}

fn markdown_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\n', " ")
}

fn push_visible_output(output: &mut VecDeque<String>, line: String) {
    if output.len() >= MAX_VISIBLE_OUTPUT_LINES {
        output.pop_front();
    }
    output.push_back(line);
}

fn display_job_state(state: &str) -> &'static str {
    match state {
        "queued" => "Queued",
        "preflight" => "Preflight",
        "ready" => "Ready",
        "running" => "Running",
        "delta_required" => "Delta required",
        "verification_difference" => "Verification difference",
        "completed" => "Completed",
        "verified" => "Verified",
        "failed" => "Failed",
        "cancelled" => "Cancelled",
        "attention" => "Attention",
        _ => "Unknown",
    }
}

fn needs_operator_review(state: &str) -> bool {
    matches!(
        state,
        "attention" | "failed" | "cancelled" | "verification_difference"
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailureClass {
    Authentication,
    Quota,
    Transport,
    Configuration,
    Message,
    Unknown,
}

impl FailureClass {
    fn label(self) -> &'static str {
        match self {
            Self::Authentication => "authentication",
            Self::Quota => "quota",
            Self::Transport => "transport",
            Self::Configuration => "configuration",
            Self::Message => "message",
            Self::Unknown => "unknown",
        }
    }
}

fn project_health_state_counts(jobs: &[core::MailboxJob]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for job in jobs {
        *counts.entry(job.state.clone()).or_insert(0) += 1;
    }
    counts
}

fn classify_failure(error: &str) -> FailureClass {
    let error = error.to_ascii_lowercase();
    if [
        "authentication",
        "auth failed",
        "authentification",
        "invalid credentials",
        "login denied",
        "login failed",
        "authenticationfailed",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Authentication
    } else if [
        "overquota",
        "over quota",
        "quota exceeded",
        "quota",
        "mailbox is full",
        "insufficient storage",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Quota
    } else if [
        "message too large",
        "too large",
        "append failed",
        "invalid message",
        "message rejected",
        "msg rejected",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Message
    } else if [
        "timed out",
        "timeout",
        "connection reset",
        "connection refused",
        "network is unreachable",
        "broken pipe",
        "temporarily unavailable",
        "try again",
        "throttl",
        "rate limit",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Transport
    } else if [
        "invalid option",
        "unknown option",
        "could not start",
        "not found",
        "no such file",
        "configuration",
        "invalid endpoint",
        "missing",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Configuration
    } else {
        FailureClass::Unknown
    }
}

fn is_transient_batch_error(error: &str) -> bool {
    classify_failure(error) == FailureClass::Transport
}

fn classified_failure_detail(error: &str) -> String {
    format!("[{}] {error}", classify_failure(error).label())
}

fn write_private_atomic(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        std::fs::write(&temporary, content)?;
        restrict_file_permissions(&temporary)?;
        std::fs::File::open(&temporary)?.sync_all()?;
        std::fs::rename(&temporary, path)?;
        sync_directory(path.parent())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn sync_directory(path: Option<&std::path::Path>) -> std::io::Result<()> {
    if let Some(path) = path {
        std::fs::File::open(path)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_: Option<&std::path::Path>) -> std::io::Result<()> {
    Ok(())
}

impl eframe::App for App {
    #[allow(clippy::possible_missing_else, clippy::collapsible_if)]
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) && self.running() {
            if let Some(cancel) = &self.cancel_requested {
                cancel.store(true, Ordering::Relaxed);
                self.status = "Cancellation requested (Escape)…".into();
            }
        }
        let mut v = if self.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        v.panel_fill = if self.dark_mode {
            Color32::from_rgb(25, 34, 49)
        } else {
            SKY
        };
        v.window_fill = if self.dark_mode {
            Color32::from_rgb(31, 42, 59)
        } else {
            Color32::WHITE
        };
        v.widgets.active.bg_fill = BLUE;
        v.widgets.hovered.bg_fill = Color32::from_rgb(215, 230, 248);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(205, 219, 235));
        ctx.set_visuals(v);
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(24, 15)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("MAILSWIFTSYNC")
                            .strong()
                            .size(23.0)
                            .color(NAVY),
                    );
                    ui.label(
                        RichText::new("mailbox migration control plane")
                            .italics()
                            .color(MUTED),
                    );
                    if ui.button("Batch queue").clicked() {
                        self.bulk_open = true;
                    }
                    if ui.button("Credentials").clicked() {
                        self.keyring_open = true;
                    }
                    if ui
                        .button(if self.dark_mode {
                            "Light theme"
                        } else {
                            "Dark theme"
                        })
                        .clicked()
                    {
                        self.dark_mode = !self.dark_mode;
                    }
                    if ui
                        .button(format!("Engine: {}", self.form.engine().label()))
                        .clicked()
                    {
                        self.engine_open = true;
                    }
                    if ui.button("Advanced options").clicked() {
                        self.advanced_open = true;
                    }
                    if ui.button("Project cockpit").clicked() {
                        self.cockpit_open = true;
                        self.assess_plan();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.running() {
                            ui.add(egui::Spinner::new());
                            if let Some(started) = self.run_started_at {
                                ui.label(format!("elapsed {}", format_elapsed(started.elapsed())));
                            }
                        }
                        ui.label(RichText::new(&self.status).color(
                            if self.status.starts_with("Failed") {
                                ALERT
                            } else {
                                BLUE
                            },
                        ));
                        ui.separator();
                        ui.label(
                            RichText::new(if self.form.dry_run {
                                "SAFE MODE"
                            } else {
                                "LIVE MODE"
                            })
                            .strong()
                            .color(if self.form.dry_run {
                                TEAL
                            } else {
                                ALERT
                            }),
                        );
                    });
                });
            });
        egui::SidePanel::left("workspace_nav")
            .resizable(false)
            .default_width(185.0)
            .frame(
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.label(RichText::new("WORKSPACE").size(11.0).strong().color(MUTED));
                ui.add_space(6.0);
                for (view, label, detail) in [
                    (
                        WorkspaceView::Overview,
                        "Overview",
                        "Readiness and next step",
                    ),
                    (
                        WorkspaceView::Plan,
                        "Migration plan",
                        "Endpoints and sync rules",
                    ),
                    (WorkspaceView::Mailboxes, "Mailboxes", "Scope and queue"),
                    (WorkspaceView::Activity, "Activity", "Runs and diagnostics"),
                    (
                        WorkspaceView::Verification,
                        "Verification",
                        "Evidence and confidence",
                    ),
                ] {
                    let selected = self.active_view == view;
                    if ui
                        .selectable_label(selected, RichText::new(label).strong())
                        .clicked()
                    {
                        self.active_view = view;
                    }
                    ui.label(RichText::new(detail).size(10.0).color(MUTED));
                    ui.add_space(5.0);
                }
            });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(SKY).inner_margin(egui::Margin::same(24))).show(ctx, |ui| { self.project_summary(ui); ui.add_space(14.0); ui.heading("Migration plan"); ui.label(RichText::new("Set up the connection, run preflight, then deliberately promote this project through each migration phase.").color(MUTED)); ui.add_space(14.0); ui.horizontal(|ui| { ui.label("Project name"); ui.text_edit_singleline(&mut self.form.profile.name); ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| if ui.button("Save non-secret profile").clicked() { self.status = match self.form.save() { Ok(()) => "Profile saved; passwords were not saved".into(), Err(e) => format!("Could not save profile: {e}") }; }); }); ui.add_space(10.0); ui.columns(2, |c| { Self::account(&mut c[0], "01  SOURCE MAILBOX", &mut self.form.profile.source_host, &mut self.form.profile.source_user, &mut self.form.source_password, BLUE); Self::account(&mut c[1], "02  DESTINATION MAILBOX", &mut self.form.profile.destination_host, &mut self.form.profile.destination_user, &mut self.form.destination_password, TEAL); }); ui.horizontal(|ui| { ui.label("Source port"); ui.add(egui::TextEdit::singleline(&mut self.form.profile.source_port).desired_width(70.0)); ui.label("TLS"); egui::ComboBox::from_id_salt("source_tls").selected_text(&self.form.profile.source_tls).show_ui(ui, |ui| { for mode in ["imaps", "starttls", "plain"] { ui.selectable_value(&mut self.form.profile.source_tls, mode.into(), mode); } }); ui.separator(); ui.label("Destination port"); ui.add(egui::TextEdit::singleline(&mut self.form.profile.destination_port).desired_width(70.0)); ui.label("TLS"); egui::ComboBox::from_id_salt("destination_tls").selected_text(&self.form.profile.destination_tls).show_ui(ui, |ui| { for mode in ["imaps", "starttls"] { ui.selectable_value(&mut self.form.profile.destination_tls, mode.into(), mode); } }); }); ui.add_space(14.0); ui.group(|ui| { ui.heading("03  SYNC RULES"); ui.checkbox(&mut self.form.dry_run, "Simulation mode — validate access and mapping without changing the destination"); ui.horizontal(|ui| { ui.checkbox(&mut self.form.profile.automap, "Map standard folders automatically"); ui.checkbox(&mut self.form.profile.justfolders, "Folders only"); ui.checkbox(&mut self.form.profile.addheader, "Add Message-ID header when needed"); }); ui.horizontal(|ui| { ui.label("Extra imapsync options"); ui.text_edit_singleline(&mut self.form.profile.extra_options); }); ui.horizontal(|ui| { ui.label("imapsync executable"); ui.text_edit_singleline(&mut self.form.profile.imapsync_path); }); }); ui.add_space(14.0); ui.horizontal(|ui| { if ui.button("Preview safe command").clicked() { self.preview = true; } if self.running() { if ui.button("Cancel running process").clicked() { if let Some(cancel) = &self.cancel_requested { cancel.store(true, Ordering::Relaxed); self.status = "Cancellation requested…".into(); } } } else { let label = if self.form.dry_run { "Run preflight simulation  →" } else { "Start live migration  →" }; if ui.add_enabled(true, egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(if self.form.dry_run { BLUE } else { ALERT })).clicked() { self.start(); } } if !self.form.dry_run && !self.running() { ui.label(RichText::new("Live mode can add mail to the destination. Review Project Cockpit first.").color(ALERT)); } }); ui.add_space(14.0); ui.group(|ui| { ui.horizontal(|ui| { ui.heading("Execution journal"); ui.label(RichText::new(if self.running() { "streaming output" } else { "waiting" }).color(MUTED)); }); egui::ScrollArea::vertical().stick_to_bottom(true).max_height(180.0).show(ui, |ui| for line in &self.output { ui.label(RichText::new(line).monospace().size(12.0)); }); }); ui.add_space(8.0); ui.label(RichText::new("Passwords never enter the saved profile. The selected engine receives credentials only for the active process; local process visibility still matters.").size(11.0).color(MUTED)); });
        self.preview(ctx);
        self.bulk_dialog(ctx);
        self.bulk_live_confirmation(ctx);
        self.keyring_dialog(ctx);
        self.advanced_dialog(ctx);
        self.engine_dialog(ctx);
        self.cockpit(ctx);
        self.live_confirmation(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

fn main() -> eframe::Result<()> {
    eframe::run_native(
        "MailSwiftSync",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1040.0, 760.0])
                .with_min_inner_size([800.0, 620.0]),
            ..Default::default()
        },
        Box::new(|_| Ok(Box::<App>::default())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dovecot_form() -> Form {
        let mut form = Form::default();
        form.profile.engine = core::Engine::Dovecot;
        form.profile.source_host = "old.example".into();
        form.profile.source_user = "old-user".into();
        form.profile.destination_host = "localhost".into();
        form.profile.destination_user = "new-user".into();
        form.source_password = String::from("secret").into();
        form.destination_password = String::from("unused").into();
        form
    }

    #[test]
    fn dovecot_plan_uses_additive_sync_by_default() {
        let mut form = dovecot_form();
        form.dry_run = false;
        let (exe, args) = form.command(true);
        assert_eq!(exe, "doveadm");
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "imapc_host=old.example"])
        );
        assert!(args.contains(&"sync".into()));
        assert!(args.contains(&"-1".into()));
        assert!(args.windows(2).any(|pair| {
            pair[0] == "-l" && pair[1] == DOVECOT_SYNC_LOCK_WAIT_SECONDS.to_string()
        }));
        assert!(!args.iter().any(|arg| arg == "secret"));
    }

    #[test]
    fn plan_snapshot_reference_is_stable_without_exposing_snapshot() {
        let snapshot = "dry_run = false\nsource_host = \"old.example\"";
        let reference = plan_snapshot_sha256(snapshot);
        assert_eq!(reference.len(), 64);
        assert_eq!(reference, plan_snapshot_sha256(snapshot));
        assert!(!reference.contains("old.example"));
        assert_ne!(reference, plan_snapshot_sha256("dry_run = true"));
    }

    #[test]
    fn local_dovecot_credentials_use_child_environment() {
        let mut form = dovecot_form();
        form.source_password = String::from("secret").into();
        let prepared = form.prepared_command().unwrap();
        assert!(
            prepared
                .args
                .iter()
                .any(|arg| { arg == "imapc_password=$ENV:MAILSWIFTSYNC_IMAPC_PASSWORD" })
        );
        assert!(!prepared.args.iter().any(|arg| arg.contains("secret")));
        assert_eq!(
            prepared.env,
            vec![("MAILSWIFTSYNC_IMAPC_PASSWORD".into(), "secret".into())]
        );
    }

    #[test]
    fn imapsync_plan_includes_explicit_throttles() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        form.profile.max_messages_per_second = 25;
        form.profile.max_bytes_per_second = 1_048_576;
        let args = form.args(true);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--maxmessagespersecond", "25"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--maxbytespersecond", "1048576"])
        );
    }

    #[test]
    fn batch_throttles_are_divided_across_workers() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        form.profile.max_messages_per_second = 25;
        form.profile.max_bytes_per_second = 1_048_576;
        let args = form.args_with_throttle_divisor(true, 4);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--maxmessagespersecond", "6"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--maxbytespersecond", "262144"])
        );
    }

    #[test]
    fn process_launch_limiter_honors_cancellation() {
        let limiter = ProcessLaunchLimiter::new(1);
        let cancel = AtomicBool::new(true);
        assert!(!limiter.acquire(&cancel));
    }

    #[test]
    fn process_launch_limiter_spaces_sequential_starts() {
        let limiter = ProcessLaunchLimiter::new(20);
        let cancel = AtomicBool::new(false);
        assert!(limiter.acquire(&cancel));
        let started = std::time::Instant::now();
        assert!(limiter.acquire(&cancel));
        assert!(started.elapsed() >= Duration::from_millis(35));
    }

    #[test]
    fn batch_throttle_rejects_target_below_worker_count() {
        let mut profile = Profile {
            max_messages_per_second: 1,
            ..Profile::default()
        };
        assert!(validate_batch_throttle(&profile, 2).is_err());
        profile.max_messages_per_second = 2;
        assert!(validate_batch_throttle(&profile, 2).is_ok());
        profile.max_bytes_per_second = 1;
        assert!(validate_batch_throttle(&profile, 2).is_err());
    }

    #[test]
    fn plan_snapshot_excludes_raw_extra_options() {
        let mut form = dovecot_form();
        form.profile.extra_options = "--custom-secret bearer-token-value".into();
        let snapshot = form.plan_snapshot();
        assert!(!snapshot.contains("bearer-token-value"));
        assert!(snapshot.contains("extra_options_sha256"));
        let expected = format!(
            "{:x}",
            Sha256::digest(form.profile.extra_options.as_bytes())
        );
        assert!(snapshot.contains(&expected));
    }

    #[test]
    fn durable_batch_config_excludes_raw_extra_options() {
        let profile = Profile {
            extra_options: "--debug secret-bearing-value".into(),
            ..Profile::default()
        };
        let config = durable_batch_profile_config(&profile).unwrap();
        assert!(!config.contains("secret-bearing-value"));
        assert!(config.contains("extra_options"));
    }

    #[test]
    fn durable_single_identity_rejects_edited_plan() {
        let project = core::Project {
            id: "project".into(),
            name: "Pilot".into(),
            source_endpoint: "old.example".into(),
            destination_endpoint: "new.example".into(),
            phase: core::Phase::Preflight,
        };
        let mailbox = core::MailboxJob {
            id: "job".into(),
            source_mailbox: "alice@example.com".into(),
            destination_mailbox: "alice@example.com".into(),
            state: "ready".into(),
            config: None,
        };
        let mut profile = Profile {
            source_host: "old.example".into(),
            destination_host: "new.example".into(),
            source_user: "alice@example.com".into(),
            destination_user: "alice@example.com".into(),
            ..Profile::default()
        };
        assert!(durable_single_identity_matches(
            &project, &mailbox, &profile
        ));
        profile.destination_host = "other.example".into();
        assert!(!durable_single_identity_matches(
            &project, &mailbox, &profile
        ));
    }

    #[test]
    fn dovecot_dry_plan_is_non_mutating() {
        let form = dovecot_form();
        let (_, args) = form.command(true);
        assert!(args.windows(2).any(|pair| pair == ["mailbox", "list"]));
        assert!(!args.contains(&"backup".into()));
        assert!(!args.contains(&"sync".into()));
    }

    #[test]
    fn dovecot_destination_preflight_checks_user_and_mailboxes() {
        let form = dovecot_form();
        let commands = form.dovecot_destination_preflight_commands();
        assert_eq!(commands.len(), 2);
        assert!(
            commands[0]
                .1
                .windows(2)
                .any(|pair| pair == ["user", "new-user"])
        );
        assert!(
            commands[1]
                .1
                .windows(2)
                .any(|pair| pair == ["mailbox", "list"])
        );
        assert!(
            commands[1]
                .1
                .windows(2)
                .any(|pair| pair == ["-u", "new-user"])
        );
    }

    #[test]
    fn plan_fingerprint_binds_credential_references_without_passwords() {
        let mut first = dovecot_form();
        first.profile.source_credential_id = "source-prod".into();
        first.profile.destination_credential_id = "destination-prod".into();
        let mut second = first.clone();
        second.profile.source_credential_id = "source-other".into();
        assert_ne!(first.plan_fingerprint(), second.plan_fingerprint());
        assert!(!first.plan_fingerprint().contains("secret"));
        assert!(first.plan_fingerprint().contains("source-prod"));
        assert!(first.plan_fingerprint().contains("destination-prod"));
    }

    #[test]
    fn remote_dovecot_plan_uses_batch_ssh_to_destination() {
        let mut form = dovecot_form();
        form.profile.dovecot_ssh_user = "migration".into();
        let (exe, args) = form.command(true);
        assert_eq!(exe, "ssh");
        assert!(args.starts_with(&[
            "-o".into(),
            "BatchMode=yes".into(),
            "migration@localhost".into()
        ]));
        assert!(
            args.last()
                .is_some_and(|command| command.contains("doveadm"))
        );
        assert!(
            args.last()
                .is_some_and(|command| command.contains("imapc_host=old.example"))
        );
    }

    #[test]
    fn remote_dovecot_execution_requires_explicit_secret_exposure_ack() {
        let mut form = dovecot_form();
        form.profile.dovecot_ssh_user = "migration".into();
        assert!(
            form.prepared_command()
                .err()
                .is_some_and(|error| error.contains("disabled by default"))
        );
        form.profile.allow_remote_password_in_argv = true;
        assert!(form.prepared_command().is_ok());
    }

    #[test]
    fn remote_dovecot_rejects_ssh_option_like_targets() {
        let mut form = dovecot_form();
        form.profile.dovecot_execution = "ssh".into();
        form.profile.allow_remote_password_in_argv = true;
        form.profile.destination_host = "-oProxyCommand=unsafe".into();
        assert!(form.validate().unwrap_err().contains("SSH host"));
        form.profile.destination_host = "mail.example".into();
        form.profile.dovecot_ssh_user = "admin user".into();
        assert!(form.validate().unwrap_err().contains("SSH username"));
    }

    #[test]
    fn dovecot_execution_location_can_be_explicit() {
        let mut form = dovecot_form();
        form.profile.destination_host = "mail.example".into();
        form.profile.dovecot_execution = "local".into();
        assert!(form.local_doveadm());
        form.profile.dovecot_execution = "ssh".into();
        assert!(!form.local_doveadm());
        form.profile.dovecot_execution = "invalid".into();
        assert!(form.validate().is_err());
    }

    #[test]
    fn remote_arguments_are_shell_quoted() {
        assert_eq!(shell_quote("plain-value"), "plain-value");
        assert_eq!(shell_quote("pa ss'word"), "'pa ss'\\''word'");
    }

    #[test]
    fn imap_quoted_values_reject_command_injection_controls() {
        assert_eq!(
            imap_quote("user@example.test").unwrap(),
            "\"user@example.test\""
        );
        assert!(imap_quote("secret\r\na002 NOOP").is_err());
    }

    #[test]
    fn extra_options_preserve_quoted_arguments() {
        assert_eq!(
            parse_shell_words("--foo 'two words' \"three four\"").unwrap(),
            ["--foo", "two words", "three four"]
        );
        assert!(parse_shell_words("--broken '").is_err());
    }

    #[test]
    fn extra_options_cannot_override_preflighted_connection() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        form.profile.extra_options = "--host1=attacker.example".into();
        assert!(form.validate().unwrap_err().contains("controlled"));
        form.profile.extra_options = "--password2 leaked".into();
        assert!(form.validate().is_err());
        form.profile.extra_options = "--sslargs1 SSL_verify_mode=0".into();
        assert!(form.validate().is_err());
        form.profile.extra_options = "-delete2".into();
        assert!(form.validate().is_err());
        form.profile.extra_options = "--logdir /tmp/elsewhere".into();
        assert!(form.validate().is_err());
    }

    #[test]
    fn extra_options_require_the_safe_engine_allowlist() {
        let mut form = dovecot_form();
        form.profile.extra_options = "--nofoldersizes --timeout=30".into();
        assert!(form.validate().is_ok());
        form.profile.extra_options = "--custom-helper /tmp/helper".into();
        let error = form.validate().unwrap_err();
        assert!(error.contains("safe imapsync option allowlist"));
    }

    #[test]
    fn validation_rejects_imap_command_control_characters() {
        let mut form = dovecot_form();
        form.profile.source_user = "user\r\nNOOP".into();
        assert!(form.validate().unwrap_err().contains("control characters"));
        form.profile.source_user = "user".into();
        form.source_password = String::from("secret\nLOGIN injected").into();
        assert!(form.validate().unwrap_err().contains("control characters"));
    }

    #[test]
    fn validation_rejects_invalid_keyring_ids() {
        let mut form = dovecot_form();
        form.profile.source_credential_id = "bad\nentry".into();
        assert!(form.validate().unwrap_err().contains("keyring ID"));
        form.profile.source_credential_id = "x".repeat(257);
        assert!(form.validate().unwrap_err().contains("keyring ID"));
    }

    #[test]
    fn bulk_headers_allow_credentials_to_be_entered_after_import() {
        let base = Form::default();
        assert!(
            App::validate_headers(
                &[
                    "source_host",
                    "source_user",
                    "destination_host",
                    "destination_user"
                ]
                .map(String::from),
                &base,
            )
            .is_ok()
        );
        assert!(
            App::validate_headers(
                &["source_host", "source_user", "destination_host"].map(String::from),
                &base,
            )
            .is_err()
        );
        assert!(
            App::validate_headers(
                &[
                    "source_host",
                    "source_user",
                    "destination_host",
                    "destination_user",
                    "extra_options",
                ]
                .map(String::from),
                &base,
            )
            .unwrap_err()
            .contains("cannot contain extra_options")
        );
    }

    #[test]
    fn passwordless_bulk_row_is_importable_but_not_runnable() {
        let mut values = HashMap::new();
        values.insert("source_host".into(), "old.example".into());
        values.insert("source_user".into(), "old@example".into());
        values.insert("destination_host".into(), "new.example".into());
        values.insert("destination_user".into(), "new@example".into());
        let job = App::job_from_values(&values, &Form::default(), 2).unwrap();
        assert!(job.form.source_password.is_empty());
        assert!(job.form.validate().is_err());
    }

    #[test]
    fn bulk_import_preserves_password_whitespace() {
        let mut values = HashMap::new();
        values.insert("source_host".into(), "old.example".into());
        values.insert("source_user".into(), "old@example".into());
        values.insert("source_password".into(), " Secret123 ".into());
        values.insert("destination_host".into(), "new.example".into());
        values.insert("destination_user".into(), "new@example".into());
        values.insert("destination_password".into(), " Destination! ".into());
        let job = App::job_from_values(&values, &Form::default(), 2).unwrap();
        assert_eq!(job.form.source_password.as_str(), " Secret123 ");
        assert_eq!(job.form.destination_password.as_str(), " Destination! ");
    }

    #[test]
    fn bulk_import_preserves_per_row_keyring_references() {
        let mut values = HashMap::new();
        values.insert("source_host".into(), "old.example".into());
        values.insert("source_user".into(), "old@example".into());
        values.insert("source_credential_id".into(), "source-alice".into());
        values.insert("destination_host".into(), "new.example".into());
        values.insert("destination_user".into(), "new@example".into());
        values.insert(
            "destination_credential_id".into(),
            "destination-alice".into(),
        );
        let job = App::job_from_values(&values, &Form::default(), 2).unwrap();
        assert_eq!(job.form.profile.source_credential_id, "source-alice");
        assert_eq!(
            job.form.profile.destination_credential_id,
            "destination-alice"
        );
        assert!(job.form.source_password.is_empty());
        assert!(job.form.destination_password.is_empty());

        let mut base = Form::default();
        base.profile.source_credential_id = "shared-source".into();
        base.profile.destination_credential_id = "shared-destination".into();
        let inherited = App::job_from_values(
            &values
                .into_iter()
                .filter(|(key, _)| {
                    key != "source_credential_id" && key != "destination_credential_id"
                })
                .collect(),
            &base,
            3,
        )
        .unwrap();
        assert_eq!(inherited.form.profile.source_credential_id, "shared-source");
        assert_eq!(
            inherited.form.profile.destination_credential_id,
            "shared-destination"
        );
    }

    #[test]
    fn bulk_keyring_apply_fills_only_missing_source_references() {
        let mut with_password = BulkJob {
            label: "password".into(),
            form: Form::default(),
            state: "Ready".into(),
        };
        with_password.form.source_password = String::from("already-present").into();
        let mut with_reference = BulkJob {
            label: "reference".into(),
            form: Form::default(),
            state: "Ready".into(),
        };
        with_reference.form.profile.source_credential_id = "existing".into();
        let empty = BulkJob {
            label: "empty".into(),
            form: Form::default(),
            state: "Ready".into(),
        };
        let mut jobs = vec![with_password, with_reference, empty];
        assert_eq!(
            apply_keyring_id_to_jobs(&mut jobs, "shared-source", true),
            1
        );
        assert!(jobs[0].form.profile.source_credential_id.is_empty());
        assert_eq!(jobs[1].form.profile.source_credential_id, "existing");
        assert_eq!(jobs[2].form.profile.source_credential_id, "shared-source");
    }

    #[test]
    fn verification_report_write_is_atomic_and_private() {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-report-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("report.md");
        write_private_atomic(&path, "report body").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "report body");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn batch_retry_classifier_excludes_authentication_failures() {
        assert!(is_transient_batch_error("connection reset by peer"));
        assert!(is_transient_batch_error("operation timed out"));
        assert!(!is_transient_batch_error("IMAP authentication failed"));
        assert!(!is_transient_batch_error(
            "invalid destination configuration"
        ));
    }

    #[test]
    fn failure_taxonomy_keeps_operator_actions_distinct() {
        assert_eq!(
            classify_failure("AUTHENTICATIONFAILED"),
            FailureClass::Authentication
        );
        assert_eq!(classify_failure("OVERQUOTA"), FailureClass::Quota);
        assert_eq!(
            classify_failure("connection reset by peer"),
            FailureClass::Transport
        );
        assert_eq!(
            classify_failure("message too large for destination"),
            FailureClass::Message
        );
        assert_eq!(
            classify_failure("unknown option --bad"),
            FailureClass::Configuration
        );
        assert_eq!(classified_failure_detail("OVERQUOTA"), "[quota] OVERQUOTA");
    }

    #[test]
    fn successful_live_status_never_overclaims_missing_evidence() {
        assert_eq!(
            App::successful_run_status(false, false, Some("verified")),
            "Migration completed and verified"
        );
        assert_eq!(
            App::successful_run_status(false, false, Some("delta_required")),
            "Migration completed; final delta or review required"
        );
        assert_eq!(
            App::successful_run_status(false, false, None),
            "Migration completed; verification requires operator review"
        );
        assert_eq!(
            App::successful_run_status(false, true, None),
            "Batch transfer completed; review per-mailbox verification results"
        );
    }

    #[test]
    fn bulk_rejects_duplicate_destination_mailboxes() {
        let mut first = Form::default();
        first.profile.destination_host = "mail.example".into();
        first.profile.destination_user = "user@example".into();
        let mut second = first.clone();
        second.profile.source_user = "different@example".into();
        let jobs = vec![
            BulkJob {
                label: "first".into(),
                form: first,
                state: "Ready".into(),
            },
            BulkJob {
                label: "second".into(),
                form: second,
                state: "Ready".into(),
            },
        ];
        assert!(duplicate_bulk_destination(&jobs).is_some());
    }

    #[test]
    fn duplicate_destination_detection_normalizes_explicit_default_port() {
        let mut first = Form::default();
        first.profile.destination_host = "mail.example".into();
        first.profile.destination_user = "user@example".into();
        let mut second = first.clone();
        second.profile.destination_host = "mail.example:993".into();
        let jobs = vec![
            BulkJob {
                label: "first".into(),
                form: first,
                state: "Ready".into(),
            },
            BulkJob {
                label: "second".into(),
                form: second,
                state: "Ready".into(),
            },
        ];
        assert!(duplicate_bulk_destination(&jobs).is_some());
    }

    #[test]
    fn edited_batch_identity_cannot_reuse_old_durable_queue() {
        let stored = vec![core::MailboxJob {
            id: "job-1".into(),
            source_mailbox: "alice@example.com".into(),
            destination_mailbox: "alice@example.net".into(),
            state: "ready".into(),
            config: Some("engine = 'imapsync'".into()),
        }];
        let same = vec![(
            "alice@example.com".into(),
            "alice@example.net".into(),
            "engine = 'imapsync'".into(),
        )];
        let edited = vec![(
            "bob@example.com".into(),
            "bob@example.net".into(),
            "engine = 'imapsync'".into(),
        )];

        assert!(durable_batch_matches_queue(&stored, &same));
        assert!(!durable_batch_matches_queue(&stored, &edited));
    }

    #[test]
    fn project_health_counts_group_durable_mailbox_states() {
        let jobs = vec![
            core::MailboxJob {
                id: "one".into(),
                source_mailbox: "one".into(),
                destination_mailbox: "one".into(),
                state: "verified".into(),
                config: None,
            },
            core::MailboxJob {
                id: "two".into(),
                source_mailbox: "two".into(),
                destination_mailbox: "two".into(),
                state: "attention".into(),
                config: None,
            },
            core::MailboxJob {
                id: "three".into(),
                source_mailbox: "three".into(),
                destination_mailbox: "three".into(),
                state: "verified".into(),
                config: None,
            },
        ];
        let counts = project_health_state_counts(&jobs);
        assert_eq!(counts.get("verified"), Some(&2));
        assert_eq!(counts.get("attention"), Some(&1));
        assert!(!needs_operator_review("verified"));
        assert!(needs_operator_review("verification_difference"));
    }

    #[test]
    fn subprocess_output_reader_survives_invalid_utf8() {
        let bytes = b"first\n\xff\xfe\nlast\n";
        let lines = process::read_lossy_lines(std::io::Cursor::new(bytes));
        assert_eq!(lines, ["first", "��", "last"]);
    }

    #[test]
    fn visible_output_retention_is_bounded_without_shifting() {
        let mut output = VecDeque::new();
        for index in 0..=MAX_VISIBLE_OUTPUT_LINES {
            push_visible_output(&mut output, index.to_string());
        }

        assert_eq!(output.len(), MAX_VISIBLE_OUTPUT_LINES);
        assert_eq!(output.front().map(String::as_str), Some("1"));
        assert_eq!(
            output.back().and_then(|line| line.parse::<usize>().ok()),
            Some(MAX_VISIBLE_OUTPUT_LINES)
        );
    }

    #[cfg(unix)]
    #[test]
    fn live_dovecot_exit_code_two_is_a_delta_outcome() {
        let (tx, _rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let cancel = AtomicBool::new(false);
        let args = vec!["-c".into(), "exit 2".into()];
        let outcome = run_streaming(
            "/bin/sh",
            &args,
            &[],
            &tx,
            "test-run",
            "test-job",
            "",
            &cancel,
            &[],
            Duration::from_secs(5),
            true,
        )
        .unwrap();
        assert_eq!(outcome.outcome, StreamOutcome::DeltaRequired);
    }

    #[cfg(unix)]
    #[test]
    fn streaming_captures_bounded_imapsync_evidence_without_full_log() {
        let (tx, _rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let cancel = AtomicBool::new(false);
        let args = vec![
            "-c".into(),
            "printf '%s\\n' 'Host1 Nb folders: 2' 'Host2 Nb folders: 2' 'Host1 Nb messages: 7' 'Host2 Nb messages: 7' 'Host1 Total size: 100' 'Host2 Total size: 100' 'The sync looks good'".into(),
        ];
        let result = run_streaming(
            "/bin/sh",
            &args,
            &[],
            &tx,
            "test-run",
            "test-job",
            "",
            &cancel,
            &[],
            Duration::from_secs(5),
            false,
        )
        .unwrap();
        assert_eq!(result.outcome, StreamOutcome::Completed);
        let evidence = result.imapsync_evidence.unwrap();
        assert_eq!(evidence.source_messages, 7);
        assert_eq!(evidence.destination_messages, 7);
        assert!(evidence.authoritative);
    }

    #[cfg(unix)]
    #[test]
    fn dry_dovecot_exit_code_two_is_not_a_delta_outcome() {
        let (tx, _rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let cancel = AtomicBool::new(false);
        let args = vec!["-c".into(), "exit 2".into()];
        let outcome = run_streaming(
            "/bin/sh",
            &args,
            &[],
            &tx,
            "test-run",
            "test-job",
            "",
            &cancel,
            &[],
            Duration::from_secs(5),
            false,
        );
        assert!(outcome.is_err());
    }

    #[test]
    fn cleanup_guard_removes_secret_directory_on_scope_exit() {
        let directory = create_secret_directory().unwrap();
        {
            let _guard = CleanupGuard::new(vec![directory.clone()]);
            assert!(directory.is_dir());
        }
        assert!(!directory.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recorded_process_identity_rejects_start_time_mismatch() {
        let process = core::ActiveProcess {
            run_id: "run".into(),
            job_id: "job".into(),
            pid: std::process::id(),
            start_ticks: Some(0),
            process_group: Some(std::process::id()),
            session_id: Some(std::process::id()),
            executable: "test".into(),
        };
        assert!(!recorded_process_matches(&process));
    }

    #[test]
    fn imap_preflight_requires_tagged_ok_responses() {
        assert!(imap_command_succeeded(
            "* CAPABILITY IMAP4rev1\na001 OK done",
            "a001"
        ));
        assert!(!imap_command_succeeded(
            "* CAPABILITY IMAP4rev1\na001 NO denied",
            "a001"
        ));
        assert!(!imap_command_succeeded(
            "* CAPABILITY IMAP4rev1\na001 BAD denied",
            "a001"
        ));
    }

    #[test]
    fn removing_secret_options_removes_values_starting_with_dashes() {
        let mut args = vec![
            "--password1".into(),
            "--looks-like-an-option".into(),
            "--host".into(),
            "mail".into(),
        ];
        remove_option(&mut args, "--password1");
        assert_eq!(args, ["--host", "mail"]);
    }

    #[test]
    fn endpoint_parser_handles_ports_and_ipv6() {
        assert_eq!(
            endpoint_parts("mail.example:8143", 993).unwrap(),
            ("mail.example".into(), 8143)
        );
        assert_eq!(
            endpoint_parts("[2001:db8::1]:993", 143).unwrap(),
            ("2001:db8::1".into(), 993)
        );
        assert!(endpoint_parts("mail.example:0", 993).is_err());
        assert!(endpoint_parts("[2001:db8::1]garbage", 993).is_err());
    }

    #[test]
    fn imap_default_port_matches_transport_mode() {
        assert_eq!(default_imap_port("imaps"), 993);
        assert_eq!(default_imap_port("starttls"), 143);
        assert_eq!(default_imap_port("plain"), 143);
        assert_eq!(Form::default().profile.destination_tls, "imaps");
    }

    #[test]
    fn imapsync_destination_transport_and_port_are_typed() {
        let mut form = Form::default();
        form.profile.source_host = "source.example".into();
        form.profile.source_user = "source-user".into();
        form.profile.destination_host = "destination.example".into();
        form.profile.destination_user = "destination-user".into();
        form.source_password = String::from("source-secret").into();
        form.destination_password = String::from("destination-secret").into();
        form.profile.destination_tls = "starttls".into();
        let args = form.args(true);
        assert!(args.windows(2).any(|pair| pair == ["--port2", "143"]));
        assert!(args.contains(&"--tls2".into()));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--tlsargs2", "SSL_verify_mode=1"])
        );
        assert!(!args.iter().any(|arg| arg == "--ssl2"));
        form.profile.destination_port = "8143".into();
        let args = form.args(true);
        assert!(args.windows(2).any(|pair| pair == ["--port2", "8143"]));
    }

    #[test]
    fn secret_runtime_isolated_below_xdg_runtime_directory() {
        assert_eq!(
            credentials::secret_runtime_base_from(Some(PathBuf::from("/run/user/1000"))),
            PathBuf::from("/run/user/1000/mailswiftsync")
        );
        assert!(
            credentials::secret_runtime_base_from(Some(PathBuf::from("")))
                .ends_with("mailswiftsync-runtime")
        );
    }

    #[test]
    fn state_lock_prevents_two_instances_and_releases_on_drop() {
        let state_path =
            std::env::temp_dir().join(format!("mailswiftsync-lock-{}.db", uuid::Uuid::new_v4()));
        let first = acquire_instance_lock(&state_path).unwrap();
        assert!(
            acquire_instance_lock(&state_path)
                .unwrap_err()
                .contains("project database")
        );
        drop(first);
        let second = acquire_instance_lock(&state_path).unwrap();
        drop(second);
        let _ = std::fs::remove_file(state_path.with_extension("lock"));
    }

    #[cfg(unix)]
    #[test]
    fn recorded_process_group_termination_stops_orphaned_child() {
        let mut command = Command::new("sh");
        command.args(["-c", "trap '' TERM; sleep 30"]);
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        assert!(child.try_wait().unwrap().is_none());

        terminate_recorded_process_group(pid);

        let status = child.wait().unwrap();
        assert!(!status.success());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_reaps_matching_process_before_recovering_mailbox() {
        let mut command = Command::new("sh");
        command.args(["-c", "trap '' TERM; sleep 30"]);
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        let (start_ticks, process_group, session_id) = linux_process_identity(pid).unwrap();
        let process = core::ActiveProcess {
            run_id: "run-startup-recovery".into(),
            job_id: String::new(),
            pid,
            start_ticks: Some(start_ticks),
            process_group: Some(process_group),
            session_id: Some(session_id),
            executable: "sh".into(),
        };
        let db = core::StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        let mut process = process;
        process.job_id = job.clone();
        db.begin_run(&project.id, &job, &process.run_id, "test")
            .unwrap();
        db.register_process(&process).unwrap();

        assert!(recorded_process_matches(&process));
        terminate_recorded_process_group(process.pid);
        assert_eq!(db.recover_abandoned_jobs().unwrap(), 1);
        assert_eq!(
            db.mailbox_state(&job).unwrap().as_deref(),
            Some("attention")
        );
        assert!(db.active_processes().unwrap().is_empty());
        assert_eq!(
            db.run_status(&process.run_id).unwrap().as_deref(),
            Some("abandoned")
        );
        assert!(!child.wait().unwrap().success());
        // The retry is only attempted after the matching process has been
        // reaped and recovery has changed the mailbox out of `running`.
        db.begin_run(&project.id, &job, "run-after-reap", "test")
            .unwrap();
        db.finish_run_for_mailbox(
            &project.id,
            &job,
            "run-after-reap",
            "cancelled",
            "cancelled",
            "test retry cleanup",
        )
        .unwrap();
    }

    #[test]
    fn recommended_action_prioritizes_interrupted_work() {
        assert_eq!(
            recommended_next_action(core::Phase::Preflight, true, 2, false),
            "Review Attention items before starting another migration."
        );
        assert_eq!(
            recommended_next_action(core::Phase::Preflight, true, 0, true),
            "A migration is running — monitor Activity; press Escape to request cancellation."
        );
    }

    #[test]
    fn recommended_action_describes_phase_without_synthetic_readiness() {
        assert_eq!(
            recommended_next_action(core::Phase::Discovery, false, 0, false),
            "Create the project, then run a dry preflight against a test mailbox."
        );
        assert_eq!(
            recommended_next_action(core::Phase::Verification, true, 0, false),
            "Review evidence for each mailbox and export the verification report."
        );
    }

    #[test]
    fn dovecot_plain_tls_maps_to_dovecot_no() {
        let mut form = dovecot_form();
        form.profile.source_tls = "plain".into();
        let (_, args) = form.command(true);
        assert!(args.iter().any(|arg| arg == "imapc_ssl=no"));
    }

    #[test]
    fn plain_source_requires_explicit_live_transport_ack_and_binds_plan() {
        let mut form = dovecot_form();
        form.profile.source_tls = "plain".into();
        assert!(form.requires_insecure_transport_ack());
        assert!(form.validate().is_err());
        let without_ack = form.plan_fingerprint();
        form.profile.allow_insecure_source_transport = true;
        assert!(!form.requires_insecure_transport_ack());
        assert!(form.validate().is_ok());
        assert_ne!(without_ack, form.plan_fingerprint());
    }

    #[test]
    fn imapsync_runtime_plan_uses_ephemeral_passfiles() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        let prepared = form.prepared_command().unwrap();
        assert!(!prepared.args.contains(&"--password1".into()));
        assert!(!prepared.args.contains(&"--password2".into()));
        let source_index = prepared
            .args
            .iter()
            .position(|arg| arg == "--passfile1")
            .unwrap();
        let destination_index = prepared
            .args
            .iter()
            .position(|arg| arg == "--passfile2")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&prepared.args[source_index + 1]).unwrap(),
            "secret"
        );
        assert_eq!(
            std::fs::read_to_string(&prepared.args[destination_index + 1]).unwrap(),
            "unused"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&prepared.args[source_index + 1])
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(prepared.env.is_empty());
        assert!(!prepared.args.iter().any(|arg| arg == "secret"));
        assert!(prepared.args.iter().any(|arg| arg == "--ssl1"));
        assert!(prepared.args.iter().any(|arg| arg == "--ssl2"));
        assert!(
            prepared
                .args
                .windows(2)
                .any(|pair| { pair == ["--sslargs1", "SSL_verify_mode=1"] })
        );
        assert!(
            prepared
                .args
                .windows(2)
                .any(|pair| { pair == ["--sslargs2", "SSL_verify_mode=1"] })
        );
        cleanup_paths(&prepared.cleanup);
        assert!(!std::path::Path::new(&prepared.args[source_index + 1]).exists());
    }

    #[test]
    fn imapsync_summary_becomes_durable_evidence_input() {
        let lines = [
            "Host1 Nb folders: 3 folders".into(),
            "Host2 Nb folders: 3 folders".into(),
            "Host1 Nb messages: 42 messages".into(),
            "Host2 Nb messages: 42 messages".into(),
            "Host1 Total size: 1000 bytes".into(),
            "Host2 Total size: 1000 bytes".into(),
            "The sync looks good, all 42 identified messages in host1 are on host2.".into(),
            "Detected 0 errors".into(),
        ];
        let evidence = verification::parse_imapsync_evidence(&lines).unwrap();
        assert_eq!(evidence.confidence_percent(), 100);
        assert_eq!(evidence.source_messages, 42);
    }

    #[test]
    fn incomplete_imapsync_summary_is_not_evidence() {
        assert!(verification::parse_imapsync_evidence(&["Detected 0 errors".into()]).is_none());
    }

    #[test]
    fn dovecot_status_aggregates_mailbox_evidence() {
        let source = vec![
            "INBOX messages=10 vsize=100".into(),
            "Archive messages=2 vsize=50".into(),
        ];
        let destination = vec![
            "INBOX messages=10 vsize=100".into(),
            "Archive messages=2 vsize=50".into(),
        ];
        let evidence = verification::parse_dovecot_evidence(&source, &destination).unwrap();
        assert_eq!(evidence.source_folders, 2);
        assert_eq!(evidence.source_messages, 12);
        assert_eq!(evidence.confidence_percent(), 85);
    }
}
