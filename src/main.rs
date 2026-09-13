mod controller;
mod core;
mod credentials;
mod endpoint;
mod engine;
mod process;
mod reports;
mod ui;
mod verification;

use controller::{ActiveRunContext, LiveAuthProof, RunKind};
use credentials::{
    CleanupGuard, cleanup_paths, cleanup_stale_secret_directories, create_secret_directory,
    restrict_directory_permissions, restrict_file_permissions, secret_runtime_base,
    write_secret_file,
};
#[cfg(test)]
use process::terminate_process_group_by_pid;
use process::{
    InstanceLock, ProcessLaunchLimiter, ProcessOutcome, acquire_instance_lock,
    attach_child_supervisor, collect_redacted_lines_with_callback, configure_process_group,
    for_each_lossy_line, process_identity, recorded_process_matches, terminate_process_group,
    terminate_recorded_process_group, wait_with_timeout,
};

use calamine::{Reader, open_workbook_auto};
use eframe::{
    egui,
    egui::{Color32, RichText, Stroke},
};
use egui_extras::{Column, TableBuilder};
use keyring::Entry;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pemfile::certs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Display;
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    ffi::OsString,
    io::{BufReader, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::Duration,
};
use ui::ThemeColors;
#[cfg(test)]
use ui::contrast_ratio;
use zeroize::Zeroizing;

const BLUE: Color32 = Color32::from_rgb(45, 113, 205);
const TEAL: Color32 = Color32::from_rgb(24, 158, 166);
const MUTED: Color32 = Color32::from_rgb(103, 119, 139);
const ALERT: Color32 = Color32::from_rgb(193, 74, 61);

const MAX_VISIBLE_OUTPUT_LINES: usize = 10_000;
const MAX_VISIBLE_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PROCESS_TAIL_LINES: usize = 200;
const MAX_PROCESS_TAIL_BYTES: usize = 1024 * 1024;
const MAX_DIAGNOSTIC_LINE_BYTES: usize = 16 * 1024;
const MAX_BULK_IMPORT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_BULK_IMPORT_ROWS: usize = 100_000;
const MAX_BULK_IMPORT_COLUMNS: usize = 64;
const MAX_BULK_IMPORT_CELL_BYTES: usize = 64 * 1024;
const BATCH_PROCESS_STARTS_PER_SECOND: usize = 2;
const DOVECOT_SYNC_LOCK_WAIT_SECONDS: u64 = 300;
const MAX_PENDING_EVENTS: usize = 4_096;
const MAX_ACTIVITY_HISTORY_ROWS: u32 = 250;
// Keep one unusually noisy worker from monopolising an egui frame.
const MAX_EVENTS_PER_FRAME: usize = 250;
const PROCESS_REGISTRATION_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_UI_SCALE: f32 = 1.10;
const MIN_UI_SCALE: f32 = 0.90;
const MAX_UI_SCALE: f32 = 1.50;

type OutputObserver = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Profile {
    name: String,
    source_host: String,
    #[serde(default)]
    source_port: String,
    #[serde(default = "default_source_tls")]
    source_tls: String,
    #[serde(default)]
    source_ca_bundle: String,
    #[serde(default)]
    source_certificate_pin_sha256: String,
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
    #[serde(default)]
    destination_ca_bundle: String,
    #[serde(default)]
    destination_certificate_pin_sha256: String,
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
    /// Legacy profile field retained for deserialization compatibility. Remote
    /// Dovecot execution is rejected until a deployment-independent secret
    /// broker is available, so this value has no effect.
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AppearancePreferences {
    dark_mode: bool,
    ui_scale: f32,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            dark_mode: true,
            ui_scale: DEFAULT_UI_SCALE,
        }
    }
}

impl AppearancePreferences {
    fn path() -> PathBuf {
        dirs_next::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("mailswiftsync/appearance.toml")
    }

    fn load() -> Self {
        let preferences = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| toml::from_str::<Self>(&text).ok())
            .unwrap_or_default();
        let ui_scale = if preferences.ui_scale.is_finite() {
            preferences.ui_scale.clamp(MIN_UI_SCALE, MAX_UI_SCALE)
        } else {
            DEFAULT_UI_SCALE
        };
        Self {
            dark_mode: preferences.dark_mode,
            ui_scale,
        }
    }

    fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            restrict_directory_permissions(parent).map_err(|error| error.to_string())?;
        }
        let content = toml::to_string_pretty(self).map_err(|error| error.to_string())?;
        write_private_atomic(&path, &content).map_err(|error| error.to_string())
    }
}

/// The durable run snapshot deliberately does not serialize `Profile`.
/// Operator-supplied extra options are retained only as a digest so a
/// password or token embedded in an expert option cannot enter SQLite or an
/// exported report.
#[derive(Serialize, Deserialize)]
struct RunPlanSnapshot {
    dry_run: bool,
    profile: RunProfileSnapshot,
}

#[derive(Serialize, Deserialize)]
struct RunProfileSnapshot {
    name: String,
    source_host: String,
    source_port: String,
    source_tls: String,
    source_ca_bundle: String,
    source_certificate_pin_sha256: String,
    allow_insecure_source_transport: bool,
    source_user: String,
    source_credential_id: String,
    destination_host: String,
    destination_user: String,
    destination_credential_id: String,
    destination_port: String,
    destination_tls: String,
    destination_ca_bundle: String,
    destination_certificate_pin_sha256: String,
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
    dovecot_checkpoint_sha256: Option<String>,
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

fn validate_certificate_pin(value: &str, label: &str) -> Result<(), String> {
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

fn plan_snapshot_sha256(snapshot: &str) -> String {
    format!("{:x}", Sha256::digest(snapshot.as_bytes()))
}

/// Produce a stable identity for one evidence result and the exact run plan
/// that produced it. This is an integrity reference for exported artifacts,
/// not a signature or a claim of independent message-level verification.
fn evidence_digest(run_id: &str, plan_snapshot: &str, evidence: &core::MailboxEvidence) -> String {
    let canonical = format!(
        "run_id={run_id}\nplan_snapshot_sha256={}\nsource_folders={}\ndestination_folders={}\nsource_messages={}\ndestination_messages={}\nsource_bytes={}\ndestination_bytes={}\nunmatched_messages={}\nfailed_messages={}\nauthoritative={}\n",
        plan_snapshot_sha256(plan_snapshot),
        evidence.source_folders,
        evidence.destination_folders,
        evidence.source_messages,
        evidence.destination_messages,
        evidence.source_bytes,
        evidence.destination_bytes,
        evidence.unmatched_messages,
        evidence.failed_messages,
        evidence.authoritative,
    );
    plan_snapshot_sha256(&canonical)
}

/// Add a deterministic, bundle-level integrity reference to a structured
/// report. The digest is calculated over the canonical JSON representation
/// without the digest field itself, so whitespace changes do not invalidate a
/// proof while any semantic report change does.
fn with_proof_digest(mut value: serde_json::Value) -> Result<serde_json::Value, String> {
    if !value.is_object() {
        return Err("Migration proof must be a JSON object.".into());
    }
    value
        .as_object_mut()
        .expect("object checked above")
        .remove("proof_digest");
    // Re-signing must produce the same digest as signing an unsigned report.
    // The prior signature is metadata, not part of the signed report payload.
    value
        .as_object_mut()
        .expect("object checked above")
        .remove("proof_signature");
    let canonical = serde_json::to_string(&value).map_err(|error| error.to_string())?;
    let digest = plan_snapshot_sha256(&canonical);
    value
        .as_object_mut()
        .expect("object checked above")
        .insert("proof_digest".into(), serde_json::Value::String(digest));
    Ok(value)
}

fn canonical_signed_proof_payload(value: &serde_json::Value) -> Result<String, String> {
    let mut unsigned = value.clone();
    unsigned
        .as_object_mut()
        .ok_or("Migration proof must be a JSON object.")?
        .remove("proof_signature");
    serde_json::to_string(&unsigned).map_err(|error| error.to_string())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode<const N: usize>(value: &str) -> Result<[u8; N], String> {
    if value.len() != N * 2 {
        return Err(format!("expected {} hexadecimal bytes", N));
    }
    let mut output = [0_u8; N];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(chunk).map_err(|_| "invalid hexadecimal text".to_owned())?;
        output[index] = u8::from_str_radix(text, 16)
            .map_err(|_| "invalid hexadecimal signature material".to_owned())?;
    }
    Ok(output)
}

fn sign_proof_file(
    path: &std::path::Path,
    signing_key_path: &std::path::Path,
    key_id: &str,
) -> Result<String, String> {
    reports::signing::sign_file(path, signing_key_path, key_id)
}

#[cfg(test)]
fn verify_proof_file(path: &std::path::Path) -> Result<String, String> {
    verify_proof_file_with_trust(path, None)
}

fn verify_proof_file_with_trust(
    path: &std::path::Path,
    trusted_public_key: Option<&str>,
) -> Result<String, String> {
    reports::signing::verify_file(path, trusted_public_key)
}

/// Persist only an opaque identity for a preflighted plan. The full
/// canonical fingerprint is used in memory for the live gate, but the
/// database only needs equality and should not retain generated arguments.
fn plan_fingerprint_digest(fingerprint: &str) -> String {
    plan_snapshot_sha256(fingerprint)
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

/// Decode a persisted batch row without ever substituting a default plan.
/// A missing or corrupt plan is durable-state corruption, not a request for a
/// new migration profile. Callers must surface the error and keep execution
/// disabled until the row is repaired or discarded deliberately.
fn decode_persisted_batch_profile(config: Option<&str>, job_id: &str) -> Result<Profile, String> {
    let config =
        config.ok_or_else(|| format!("Saved batch mailbox {job_id} has no migration plan"))?;
    toml::from_str(config)
        .map_err(|error| format!("Saved batch mailbox {job_id} is corrupt: {error}"))
}

fn decode_report_run_snapshot(snapshot: &str) -> Result<Option<RunPlanSnapshot>, String> {
    if snapshot.trim().is_empty() {
        return Ok(None);
    }
    toml::from_str(snapshot)
        .map(Some)
        .map_err(|error| format!("The evidence run plan snapshot is corrupt: {error}"))
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

fn append_dovecot_source_tls_policy(args: &mut Vec<String>, mode: &str, ca_bundle: &str) {
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

    fn path() -> Result<std::path::PathBuf, String> {
        dirs_next::config_dir()
            .map(|directory| directory.join("mailswiftsync/profile.toml"))
            .ok_or_else(|| {
                "Cannot determine a durable configuration directory; repair the OS profile or use an explicit state path.".to_owned()
            })
    }
    fn load() -> Result<Self, String> {
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
    fn save(&self) -> Result<(), String> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            restrict_directory_permissions(parent).map_err(|e| e.to_string())?;
        }
        let content = toml::to_string_pretty(&self.profile).map_err(|e| e.to_string())?;
        write_private_atomic(&path, &content).map_err(|e| e.to_string())
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
    /// Reload configured references immediately before live admission. The
    /// ordinary loader intentionally preserves a password typed into the
    /// current form; live promotion must instead use the current keyring
    /// value when a credential reference is authoritative.
    fn reload_configured_keyring_credentials(&mut self) -> Result<(), String> {
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
    fn credential_fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.source_password.as_bytes());
        digest.update([0]);
        digest.update(self.destination_password.as_bytes());
        format!("{:x}", digest.finalize())
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
    #[cfg(test)]
    fn args(&self, redact: bool) -> Vec<String> {
        self.args_with_throttle_divisor(redact, 1)
    }
    fn args_with_throttle_divisor(&self, redact: bool, throttle_divisor: usize) -> Vec<String> {
        self.args_with_throttle_divisor_and_mode(redact, throttle_divisor, self.dry_run)
    }
    fn args_with_throttle_divisor_and_mode(
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
    fn extra_options_valid(&self) -> Result<(), String> {
        engine::validate_extra_options(&self.profile.extra_options)
    }
    #[cfg(test)]
    fn prepared_command(&self) -> Result<PreparedCommand, String> {
        self.prepared_command_with_throttle_divisor(1)
    }
    #[cfg(test)]
    fn prepared_command_with_throttle_divisor(
        &self,
        throttle_divisor: usize,
    ) -> Result<PreparedCommand, String> {
        self.prepared_command_with_throttle_divisor_and_checkpoint(throttle_divisor, None)
    }
    fn prepared_command_with_throttle_divisor_and_checkpoint(
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
        let (executable, mut args) = self.command_with_checkpoint_and_mode(true, None, false);
        if self.engine() == core::Engine::ImapSync {
            remove_option(&mut args, "--password1");
            remove_option(&mut args, "--password2");
        }
        format!(
            "{}\n{}\ncredential-source1={}\ncredential-source2={}\ninsecure-source-transport-ack={}\nsource-ca-bundle={}\nsource-certificate-pin={}\ndestination-ca-bundle={}\ndestination-certificate-pin={}",
            executable,
            args.join("\u{1f}"),
            self.profile.source_credential_id.trim(),
            self.profile.destination_credential_id.trim(),
            self.profile.allow_insecure_source_transport,
            self.profile.source_ca_bundle.trim(),
            self.profile
                .source_certificate_pin_sha256
                .trim()
                .to_ascii_lowercase(),
            self.profile.destination_ca_bundle.trim(),
            self.profile
                .destination_certificate_pin_sha256
                .trim()
                .to_ascii_lowercase(),
        )
    }

    /// Serialize the launch configuration without session passwords or raw
    /// expert-option values. This is persisted with the run so historical
    /// reports do not depend on the currently edited form.
    #[cfg(test)]
    fn plan_snapshot(&self) -> String {
        self.plan_snapshot_with_checkpoint(None)
            .expect("test snapshot serialization")
    }

    /// Serialize the launch configuration and, when applicable, the identity
    /// of the previously committed Dovecot state supplied to this run. The
    /// state token itself stays out of durable reports; its digest is enough
    /// to prove which resume point was selected.
    fn plan_snapshot_with_checkpoint(&self, checkpoint: Option<&str>) -> Result<String, String> {
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
                source_ca_bundle: profile.source_ca_bundle.clone(),
                source_certificate_pin_sha256: profile.source_certificate_pin_sha256.clone(),
                allow_insecure_source_transport: profile.allow_insecure_source_transport,
                source_user: profile.source_user.clone(),
                source_credential_id: profile.source_credential_id.clone(),
                destination_host: profile.destination_host.clone(),
                destination_user: profile.destination_user.clone(),
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
                    .then(|| checkpoint.map(plan_snapshot_sha256))
                    .flatten(),
            },
        };
        toml::to_string(&snapshot)
            .map_err(|error| format!("could not serialize immutable run plan snapshot: {error}"))
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
        self.command_with_checkpoint(redact, None)
    }
    fn command_with_checkpoint(
        &self,
        redact: bool,
        checkpoint: Option<&str>,
    ) -> (String, Vec<String>) {
        self.command_with_checkpoint_and_mode(redact, checkpoint, self.dry_run)
    }
    fn command_with_checkpoint_and_mode(
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
    RunLine {
        run_id: String,
        job_id: String,
        text: String,
    },
    ProcessStarted(
        String,
        String,
        u32,
        Option<u64>,
        Option<u32>,
        Option<u32>,
        String,
        mpsc::SyncSender<Result<(), String>>,
    ),
    ProcessEnded {
        run_id: String,
        job_id: String,
    },
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
        credential_fingerprint: Option<String>,
    },
    BatchEvidence {
        job_id: String,
        child_run_id: String,
        evidence: core::MailboxEvidence,
    },
    Checkpoint {
        run_id: String,
        job_id: String,
        value: String,
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
    let _child_supervisor = match attach_child_supervisor(&child) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err(format!(
                "could not establish descendant process supervision for {executable}: {error}"
            ));
        }
    };
    let identity = process_identity(child.id());
    let (start_ticks, process_group, session_id) = identity
        .map(|(start, group, session)| (Some(start), Some(group), Some(session)))
        .unwrap_or((None, None, None));
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stdout pipe unavailable; child cancelled before supervision".into());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stderr pipe unavailable; child cancelled before supervision".into());
        }
    };
    let out_tx = tx.clone();
    let out_prefix = prefix.to_owned();
    let out_secrets = secrets.to_vec();
    let tail = Arc::new(Mutex::new(VecDeque::with_capacity(200)));
    let evidence = Arc::new(Mutex::new(
        verification::ImapsyncEvidenceAccumulator::default(),
    ));
    let dropped_diagnostics = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let out_tail = Arc::clone(&tail);
    let out_evidence = Arc::clone(&evidence);
    let dovecot_checkpoint = Arc::new(Mutex::new(None::<String>));
    let out_dovecot_checkpoint = Arc::clone(&dovecot_checkpoint);
    let out_dropped_diagnostics = Arc::clone(&dropped_diagnostics);
    let out_run_id = run_id.to_owned();
    let out_job_id = job_id.to_owned();
    let out_thread = thread::spawn(move || {
        for_each_lossy_line(stdout, |line| {
            let mut safe = line;
            for secret in &out_secrets {
                if !secret.is_empty() {
                    safe = safe.replace(secret, "[REDACTED]");
                }
            }
            record_process_tail(&out_tail, &safe);
            if let Ok(mut evidence) = out_evidence.lock() {
                evidence.observe(&safe);
            }
            if dovecot_exit_two_is_delta
                && let Some(candidate) = dovecot_state_candidate(&safe)
                && let Ok(mut checkpoint) = out_dovecot_checkpoint.lock()
            {
                *checkpoint = Some(candidate);
            }
            if out_tx
                .try_send(Event::RunLine {
                    run_id: out_run_id.clone(),
                    job_id: out_job_id.clone(),
                    text: format!("{out_prefix}{safe}"),
                })
                .is_err()
            {
                out_dropped_diagnostics.fetch_add(1, Ordering::Relaxed);
            }
        })
    });
    let err_tx = tx.clone();
    let err_prefix = prefix.to_owned();
    let err_secrets = secrets.to_vec();
    let err_tail = Arc::clone(&tail);
    let err_evidence = Arc::clone(&evidence);
    let err_dropped_diagnostics = Arc::clone(&dropped_diagnostics);
    let err_run_id = run_id.to_owned();
    let err_job_id = job_id.to_owned();
    let err_thread = thread::spawn(move || {
        for_each_lossy_line(stderr, |line| {
            let mut safe = line;
            for secret in &err_secrets {
                if !secret.is_empty() {
                    safe = safe.replace(secret, "[REDACTED]");
                }
            }
            record_process_tail(&err_tail, &safe);
            if let Ok(mut evidence) = err_evidence.lock() {
                evidence.observe(&safe);
            }
            if err_tx
                .try_send(Event::RunLine {
                    run_id: err_run_id.clone(),
                    job_id: err_job_id.clone(),
                    text: format!("{err_prefix}[stderr] {safe}"),
                })
                .is_err()
            {
                err_dropped_diagnostics.fetch_add(1, Ordering::Relaxed);
            }
        })
    });
    // Start both drainers before the reliable lifecycle send. If the
    // bounded event queue is temporarily full, this send may wait, but the
    // child pipes are already being drained and cannot deadlock the engine.
    let (registration_tx, registration_rx) = mpsc::sync_channel(1);
    let registration_deadline = std::time::Instant::now() + PROCESS_REGISTRATION_ACK_TIMEOUT;
    let mut process_started_event = Some(Event::ProcessStarted(
        run_id.to_owned(),
        job_id.to_owned(),
        child.id(),
        start_ticks,
        process_group,
        session_id,
        executable.to_owned(),
        registration_tx,
    ));
    let process_started = loop {
        if cancel.load(Ordering::Relaxed) {
            break false;
        }
        let remaining = registration_deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break false;
        }
        match tx.try_send(
            process_started_event
                .take()
                .expect("registration event exists"),
        ) {
            Ok(()) => break true,
            Err(mpsc::TrySendError::Full(event)) => {
                process_started_event = Some(event);
                thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => break false,
        }
    };
    let result = if process_started {
        let registration = loop {
            if cancel.load(Ordering::Relaxed) {
                break Err("cancelled before durable process registration".to_owned());
            }
            let remaining =
                registration_deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break Err("durable process registration acknowledgement timed out".to_owned());
            }
            match registration_rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err("durable process registration response was lost".to_owned());
                }
            }
        };
        if let Err(error) = registration {
            cancel.store(true, Ordering::Relaxed);
            let _ = wait_with_timeout(&mut child, timeout.min(Duration::from_secs(5)), cancel);
            Err(format!(
                "process registration failed; child cancelled: {error}"
            ))
        } else {
            match wait_with_timeout(&mut child, timeout, cancel) {
                Err(error) => Err(error.to_string()),
                Ok(ProcessOutcome {
                    cancelled: true, ..
                }) => Err("cancelled by operator".into()),
                Ok(ProcessOutcome {
                    timed_out: true, ..
                }) => Err("migration exceeded its configured execution timeout".into()),
                Ok(ProcessOutcome {
                    exit_code: Some(0), ..
                }) => Ok(StreamOutcome::Completed),
                Ok(ProcessOutcome {
                    exit_code: Some(2), ..
                }) if dovecot_exit_two_is_delta => Ok(StreamOutcome::DeltaRequired),
                Ok(ProcessOutcome { exit_code, .. }) => {
                    Err(format!("process exited with code {:?}", exit_code))
                }
            }
        }
    } else {
        // ProcessStarted is the reliable hand-off to the durable controller.
        // Continuing after a disconnected event channel would leave a live
        // child with no possible process registration or cancellation owner.
        cancel.store(true, Ordering::Relaxed);
        let _ = wait_with_timeout(&mut child, timeout.min(Duration::from_secs(5)), cancel);
        Err("process-start event channel disconnected; child cancelled before supervision".into())
    };
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
    let dropped = dropped_diagnostics.load(Ordering::Relaxed);
    if dropped > 0 {
        // This is the durable accounting marker for lossy UI streaming. The
        // reader threads have already joined, so waiting here cannot block a
        // child pipe; it ensures a saturated event queue cannot silently
        // erase the fact that diagnostics were omitted.
        let _ = tx.send(Event::RunLine {
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
            text: format!(
                "{prefix}[diagnostics] {dropped} output line(s) omitted because the operator event queue was full"
            ),
        });
    }
    // The process identity is only valid for this attempt. Clear it before
    // returning so a transient retry (or a crash during its backoff) cannot
    // leave an exited PID looking like an active orphan.
    let process_end_error = tx
        .send(Event::ProcessEnded {
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
        })
        .err()
        .map(|_| "process event channel disconnected while clearing process identity".to_owned());
    match result {
        Ok(outcome) if reader_error.is_none() => {
            if let Some(error) = process_end_error {
                return Err(error);
            }
            let imapsync_evidence = evidence
                .lock()
                .map_err(|_| "imapsync evidence collector was poisoned".to_owned())?;
            let imapsync_evidence = imapsync_evidence.evidence();
            let checkpoint = dovecot_checkpoint
                .lock()
                .map_err(|_| "Dovecot checkpoint collector was poisoned".to_owned())?
                .clone();
            if let Some(value) = &checkpoint {
                tx.send(Event::Checkpoint {
                    run_id: run_id.to_owned(),
                    job_id: job_id.to_owned(),
                    value: value.clone(),
                })
                .map_err(|_| {
                    "process event channel disconnected while delivering checkpoint".to_owned()
                })?;
            }
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
            let error = [
                process_error,
                reader_error,
                process_end_error.unwrap_or_default(),
            ]
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

/// Dovecot emits the stateful-sync resume value as a compact, single-line
/// token. The public contract does not require padded base64, so only the
/// documented shape (printable, non-whitespace, bounded single line) is
/// enforced here; the durable store applies the same safety bound.
fn dovecot_state_candidate(line: &str) -> Option<String> {
    let value = line.trim();
    if value.len() < 8 || value.len() > 4096 || value.chars().any(char::is_whitespace) {
        return None;
    }
    if value.bytes().all(|byte| byte.is_ascii_graphic())
        && !matches!(
            value.to_ascii_lowercase().as_str(),
            "success" | "successful" | "completed"
        )
    {
        Some(value.to_owned())
    } else {
        None
    }
}

fn record_process_tail(tail: &Mutex<VecDeque<String>>, line: &str) {
    if let Ok(mut tail) = tail.lock() {
        let line = truncate_utf8(line, MAX_DIAGNOSTIC_LINE_BYTES);
        let line_bytes = line.len();
        while tail.len() >= MAX_PROCESS_TAIL_LINES
            || tail.iter().map(String::len).sum::<usize>() + line_bytes > MAX_PROCESS_TAIL_BYTES
        {
            tail.pop_front();
        }
        tail.push_back(line);
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
    observer: Option<OutputObserver>,
) -> Result<(ProcessOutcome, Vec<String>, bool), String> {
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
    let _child_supervisor = match attach_child_supervisor(&child) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err(format!(
                "could not establish descendant process supervision for {executable}: {error}"
            ));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stdout pipe unavailable; child cancelled before supervision".into());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stderr pipe unavailable; child cancelled before supervision".into());
        }
    };
    let out_secrets = secrets.to_vec();
    let out_observer = observer.clone();
    let out_thread = thread::spawn(move || {
        collect_redacted_lines_with_callback(stdout, &out_secrets, |line| {
            if let Some(observer) = &out_observer {
                observer(line);
            }
        })
    });
    let err_secrets = secrets.to_vec();
    let err_observer = observer;
    let err_thread = thread::spawn(move || {
        collect_redacted_lines_with_callback(stderr, &err_secrets, |line| {
            if let Some(observer) = &err_observer {
                observer(line);
            }
        })
    });
    let status = wait_with_timeout(&mut child, timeout, cancel).map_err(|error| error.to_string());
    let stdout_lines = out_thread
        .join()
        .map_err(|_| "stdout reader thread panicked".to_owned())?
        .map_err(|error| format!("stdout reader failed: {error}"));
    let stderr_lines = err_thread
        .join()
        .map_err(|_| "stderr reader thread panicked".to_owned())?
        .map_err(|error| format!("stderr reader failed: {error}"));
    let stdout_output = stdout_lines?;
    let stderr_output = stderr_lines?;
    let capture_truncated = stdout_output.truncated || stderr_output.truncated;
    let mut lines = stdout_output.lines;
    lines.extend(
        stderr_output
            .lines
            .into_iter()
            .map(|line| format!("[stderr] {line}")),
    );
    let status = status?;
    if capture_truncated {
        lines.push(
            "[diagnostics truncated; semantic verification continued from the full stream]".into(),
        );
    }
    Ok((status, lines, capture_truncated))
}

/// Ask an engine for its version without passing credentials or mailbox
/// arguments. This is best-effort metadata: an old wrapper may not implement
/// `--version`, in which case the run explicitly remains unversioned.
fn probe_engine_version(executable: &str) -> Option<String> {
    let cancel = AtomicBool::new(false);
    let mut candidates = vec![(executable.to_owned(), vec!["--version".into()])];
    if std::path::Path::new(executable)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("doveadm"))
    {
        let sibling = std::path::Path::new(executable)
            .parent()
            .map(|parent| parent.join("dovecot"))
            .unwrap_or_else(|| std::path::PathBuf::from("dovecot"));
        candidates.push((
            sibling.to_string_lossy().into_owned(),
            vec!["--version".into()],
        ));
    }
    for (candidate, args) in candidates {
        let Ok((status, lines, _)) = run_capture_lines(
            &candidate,
            &args,
            &[],
            &cancel,
            &[],
            Duration::from_secs(5),
            None,
        ) else {
            continue;
        };
        if status.exit_code == Some(0)
            && let Some(line) = lines
                .into_iter()
                .map(|line| line.trim().to_owned())
                .find(|line| !line.is_empty() && line.len() <= 512)
        {
            return Some(line);
        }
    }
    None
}

fn run_dovecot_destination_preflight(
    commands: &[(String, Vec<String>)],
    tx: &mpsc::SyncSender<Event>,
    cancel: &AtomicBool,
    timeout: Duration,
    prefix: &str,
    run_id: &str,
    job_id: &str,
) -> Result<(), String> {
    for (index, (executable, args)) in commands.iter().enumerate() {
        let (status, lines, _) =
            run_capture_lines(executable, args, &[], cancel, &[], timeout, None)?;
        for line in lines {
            let _ = tx.send(Event::RunLine {
                run_id: run_id.to_owned(),
                job_id: job_id.to_owned(),
                text: format!("{prefix}[destination preflight/{}] {line}", index + 1),
            });
        }
        if status.exit_code != Some(0) {
            return Err(format!(
                "Dovecot destination preflight command {} exited with code {:?}",
                index + 1,
                status.exit_code
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_dovecot_verification(
    commands: &[(String, Vec<String>)],
    verification_env: &[(String, String)],
    secrets: &[String],
    tx: &mpsc::SyncSender<Event>,
    cancel: &AtomicBool,
    timeout: Duration,
    prefix: &str,
    run_id: &str,
    job_id: &str,
) -> Result<core::MailboxEvidence, String> {
    let mut reports = Vec::with_capacity(commands.len());
    for (index, (verify_exe, verify_args)) in commands.iter().enumerate() {
        let accumulator = Arc::new(Mutex::new(verification::DovecotStatusAccumulator::default()));
        let observer_accumulator = Arc::clone(&accumulator);
        let observer: OutputObserver = Arc::new(move |line| {
            if let Ok(mut accumulator) = observer_accumulator.lock() {
                accumulator.observe(line);
            }
        });
        let (status, report, truncated) = run_capture_lines(
            verify_exe,
            verify_args,
            if index == 0 { verification_env } else { &[] },
            cancel,
            secrets,
            timeout,
            Some(observer),
        )?;
        for line in &report {
            let _ = tx.send(Event::RunLine {
                run_id: run_id.to_owned(),
                job_id: job_id.to_owned(),
                text: format!("{prefix}[verification/{}] {line}", index + 1),
            });
        }
        if status.exit_code != Some(0) {
            return Err(format!(
                "Dovecot verification command {} exited with code {:?}",
                index + 1,
                status.exit_code
            ));
        }
        if truncated {
            let _ = tx.send(Event::RunLine {
                run_id: run_id.to_owned(),
                job_id: job_id.to_owned(),
                text: format!(
                    "{prefix}[verification/{}] diagnostic transcript truncated; semantic accumulator processed the complete stream",
                    index + 1
                ),
            });
        }
        let status = accumulator
            .lock()
            .map_err(|_| "Dovecot verification accumulator was poisoned".to_owned())?
            .clone();
        reports.push((report, status));
    }
    if reports.len() < 2 {
        return Err("Dovecot verification returned incomplete reports".into());
    }
    verification::dovecot_evidence_from_accumulators(&reports[0].1, &reports[1].1)
        .ok_or_else(|| "Dovecot status output was incomplete".into())
}

#[derive(Clone)]
struct BulkJob {
    label: String,
    form: Form,
    state: String,
}

#[derive(Clone, Copy)]
enum BulkStateSet {
    Failed,
    Attention,
    Unresolved,
}

impl BulkStateSet {
    fn matches(self, state: &str) -> bool {
        match self {
            Self::Failed => state == "failed",
            Self::Attention => state == "attention",
            Self::Unresolved => matches!(
                state,
                "failed" | "attention" | "cancelled" | "delta_required" | "verification_difference"
            ),
        }
    }
}

fn bulk_selection_value(
    jobs: &[BulkJob],
    selected_ids: &HashSet<String>,
    job_ids: &[String],
) -> serde_json::Value {
    let rows = jobs
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            selected_ids.is_empty()
                || job_ids
                    .get(*index)
                    .is_some_and(|job_id| selected_ids.contains(job_id))
        })
        .map(|(_, job)| {
            serde_json::json!({
                "label": job.label,
                "source_host": job.form.profile.source_host,
                "source_user": job.form.profile.source_user,
                "destination_host": job.form.profile.destination_host,
                "destination_user": job.form.profile.destination_user,
                "state": display_state_key(&job.state),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "format": "mailswiftsync-batch-selection",
        "version": 1,
        "selected_rows": rows,
        "note": "This handoff intentionally excludes passwords, keyring references, extra options, and engine credentials. It is a review/retry scope, not an executable migration plan."
    })
}

type BatchWorkItem = (usize, String, String, Option<String>, BulkJob);
type PendingDbEvent = (String, String, String, String);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum BulkRetryScope {
    #[default]
    Unresolved,
    FailedAttention,
    DeltaRequired,
    VerificationDifference,
    Automation,
    All,
}

#[derive(Debug, Clone)]
struct BulkConfirmationSummary {
    eligible_count: usize,
    deletion_enabled: bool,
    durable_state_error: Option<String>,
    concurrency: usize,
    scope: BulkRetryScope,
}

impl BulkRetryScope {
    fn includes(self, state: &str) -> bool {
        let Some(state) = core::MailboxState::parse(state) else {
            return false;
        };
        match self {
            // Operator-review states must never be pulled into unattended
            // retry by the default scope. They require an explicit choice
            // after the durable reason has been reviewed.
            Self::Unresolved => {
                !state.is_verified()
                    && !matches!(
                        state,
                        core::MailboxState::Attention | core::MailboxState::VerificationDifference
                    )
            }
            Self::FailedAttention => matches!(
                state,
                core::MailboxState::Failed | core::MailboxState::Attention
            ),
            Self::DeltaRequired => state == core::MailboxState::DeltaRequired,
            Self::VerificationDifference => state == core::MailboxState::VerificationDifference,
            Self::Automation => matches!(
                state,
                core::MailboxState::Queued
                    | core::MailboxState::Ready
                    | core::MailboxState::Completed
                    | core::MailboxState::DeltaRequired
                    | core::MailboxState::Cancelled
                    | core::MailboxState::Failed
            ),
            Self::All => true,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved (skip verified and accepted exceptions)",
            Self::FailedAttention => "Failed or Attention only",
            Self::DeltaRequired => "Delta required only",
            Self::VerificationDifference => "Verification differences only",
            Self::Automation => "Automation-safe retryable work",
            Self::All => "All rows (explicit re-run)",
        }
    }

    fn includes_automation(self, state: &str, reason: Option<core::AttentionReason>) -> bool {
        if self != Self::Automation {
            return self.includes(state);
        }
        match core::MailboxState::parse(state) {
            Some(
                core::MailboxState::Queued
                | core::MailboxState::Ready
                | core::MailboxState::Completed
                | core::MailboxState::DeltaRequired
                | core::MailboxState::Cancelled,
            ) => true,
            Some(core::MailboxState::Failed) => matches!(
                reason,
                Some(core::AttentionReason::TransportFailed)
                    | Some(core::AttentionReason::CapacityLimited)
            ),
            _ => false,
        }
    }
}

fn canonical_destination_identity(profile: &Profile) -> Result<String, String> {
    endpoint::canonical_destination_identity(
        &profile.destination_user,
        &profile.destination_host,
        effective_destination_tls(&profile.destination_tls),
        &profile.destination_port,
    )
    .map_err(|error| format!("Invalid destination endpoint: {error}"))
}

fn validate_bulk_import_file(path: &std::path::Path) -> Result<(), String> {
    let size = std::fs::metadata(path)
        .map_err(|error| format!("Could not inspect import file: {error}"))?
        .len();
    if size > MAX_BULK_IMPORT_BYTES {
        return Err(format!(
            "The import file is {} bytes; the limit is {} bytes.",
            size, MAX_BULK_IMPORT_BYTES
        ));
    }
    Ok(())
}

fn duplicate_bulk_destination(jobs: &[BulkJob]) -> Result<Option<String>, String> {
    let mut destinations = HashSet::new();
    for (index, job) in jobs.iter().enumerate() {
        let key = canonical_destination_identity(&job.form.profile)?;
        if !destinations.insert(key) {
            return Ok(Some(format!(
                "Mailbox {} targets a destination mailbox already used by another batch row; concurrent writes to one mailbox are blocked.",
                index + 1
            )));
        }
    }
    Ok(None)
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

fn preferred_project_id<'a>(
    active_run_project: Option<&'a str>,
    selected_project: Option<&'a str>,
    single_project: Option<&'a str>,
    batch_project: Option<&'a str>,
) -> Option<&'a str> {
    active_run_project
        .or(selected_project)
        .or(batch_project)
        .or(single_project)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceView {
    Overview,
    Plan,
    Mailboxes,
    Activity,
    Verification,
}
struct App {
    form: Form,
    output: VecDeque<String>,
    receiver: Option<Receiver<Event>>,
    status: String,
    preview: bool,
    bulk_jobs: Vec<BulkJob>,
    bulk_open: bool,
    settings_open: bool,
    bulk_search: String,
    bulk_state_filter: String,
    bulk_selected_ids: HashSet<String>,
    bulk_message: String,
    advanced_open: bool,
    engine_open: bool,
    store: core::StateStore,
    /// Held for the lifetime of the application. An advisory OS lock is
    /// released automatically if the process crashes, so a later instance
    /// can safely perform orphan recovery without killing a live sibling.
    _instance_lock: Option<InstanceLock>,
    /// Startup could not prove that every recorded process identity was
    /// gone or owned by this application. No new execution is allowed until
    /// the operator confirms the host has been checked.
    process_review_required: bool,
    persistence_available: bool,
    profile_available: bool,
    /// The project currently selected by the operator for views and exports.
    /// This is deliberately separate from `active_run`, which is execution
    /// ownership and must never be inferred from UI selection.
    selected_project_id: Option<String>,
    /// A selected project that is not the current editable execution context
    /// is a historical read-only view. Keeping this explicit prevents the
    /// header/project browser from implying that the visible plan is safe to
    /// mutate or execute for that project.
    workspace_read_only: bool,
    projects_open: bool,
    project_search: String,
    project_id: Option<String>,
    job_id: Option<String>,
    /// Process-local credential material used by the last successful dry
    /// preflight. Missing after restart intentionally requires revalidation.
    preflight_credential_fingerprint: Option<String>,
    run_id: Option<String>,
    active_run: Option<ActiveRunContext>,
    /// Non-secret plan values captured for the active execution. The egui
    /// form remains visible while a run is active, but edits must not mutate
    /// the plan presented to the operator or the next retry.
    locked_profile: Option<Profile>,
    locked_dry_run: Option<bool>,
    cancel_requested: Option<Arc<AtomicBool>>,
    bulk_project_id: Option<String>,
    bulk_job_ids: Vec<String>,
    bulk_job_index_by_id: HashMap<String, usize>,
    /// Process-local credential material from the last successful dry
    /// validation for each durable queue row. Restored queues start empty.
    bulk_preflight_credential_fingerprints: Vec<Option<String>>,
    preflight: Vec<(String, String, bool)>,
    capability_receiver:
        Option<Receiver<Result<(core::ServerCapabilities, core::ServerCapabilities), String>>>,
    /// Fresh authentication completed immediately before an IMAPS live run.
    /// Both digests are captured at probe launch and must still match when
    /// the run is admitted.
    live_auth_receiver: Option<Receiver<Result<LiveAuthProof, String>>>,
    live_auth_proof: Option<LiveAuthProof>,
    source_capabilities: Option<core::ServerCapabilities>,
    destination_capabilities: Option<core::ServerCapabilities>,
    live_confirm_open: bool,
    live_confirmed: bool,
    live_confirmation_plan: Option<String>,
    durability_error: bool,
    durability_recovery_pending: bool,
    stop_confirm_open: bool,
    keyring_open: bool,
    active_view: WorkspaceView,
    pending_evidence: Option<core::MailboxEvidence>,
    pending_batch_evidence: HashMap<String, core::MailboxEvidence>,
    pending_checkpoint: Option<String>,
    pending_batch_checkpoints: HashMap<String, String>,
    /// Execution diagnostics that could not yet be committed. These remain
    /// in memory and are retried before later terminal events are handled.
    pending_db_events: Vec<PendingDbEvent>,
    /// Events deferred because their durable predecessor could not be
    /// committed. Keeping them here prevents a child completion from being
    /// silently lost when SQLite is temporarily unavailable.
    deferred_events: VecDeque<Event>,
    run_started_at: Option<std::time::Instant>,
    dark_mode: bool,
    ui_scale: f32,
    bulk_live_confirm_open: bool,
    bulk_live_confirmed: bool,
    bulk_confirmation_summary: Option<BulkConfirmationSummary>,
    bulk_dry_run: bool,
    bulk_clear_confirm_open: bool,
    pending_bulk_import: Option<std::path::PathBuf>,
    bulk_import_receiver: Option<Receiver<Result<Vec<BulkJob>, String>>>,
    bulk_live_run: bool,
    /// Live retry scope defaults to unresolved rows and is process-local UI
    /// state; durable child/run IDs remain the execution identity.
    bulk_retry_scope: BulkRetryScope,
    bulk_source_keyring_apply: String,
    bulk_destination_keyring_apply: String,
    verification_exception_operator: String,
    verification_exception_reason: String,
    verification_search: String,
    verification_filter: String,
    activity_show_all: bool,
    activity_search: String,
    activity_status_filter: String,
    reopen_reason: String,
    /// Database-backed UI read model. Rendering consumes this cache instead
    /// of issuing SQLite queries on every egui repaint.
    ui_projects: Vec<core::ProjectListItem>,
    ui_runs: Vec<core::RunListItem>,
    ui_report: Option<core::ProjectReportSnapshot>,
    ui_project: Option<core::Project>,
    ui_jobs: Vec<core::MailboxJob>,
    ui_snapshot_project_id: Option<String>,
    ui_snapshot_refreshed_at: Option<std::time::Instant>,
}
impl Default for App {
    fn default() -> Self {
        let appearance = AppearancePreferences::load();
        let state_path_result = persistent_state_path();
        let path_error = state_path_result.as_ref().err().cloned();
        let state_path = state_path_result.ok();
        let state_directory_error =
            state_path
                .as_ref()
                .and_then(|path| path.parent())
                .and_then(|parent| {
                    std::fs::create_dir_all(parent)
                        .and_then(|_| restrict_directory_permissions(parent))
                        .err()
                        .map(|error| {
                            format!("Could not secure persistent state directory: {error}")
                        })
                });
        let instance_lock = state_path
            .as_ref()
            .map(|path| acquire_instance_lock(path.as_path()));
        let (store, mut persistence_warning) = match instance_lock.as_ref() {
            Some(Ok(_)) => match core::StateStore::open(
                state_path
                    .as_ref()
                    .expect("instance lock cannot exist without a state path"),
            ) {
                Ok(store) => (store, None),
                Err(error) => (
                    core::StateStore::in_memory().expect("SQLite memory store must be available"),
                    Some(format!("Persistent SQLite state unavailable: {error}")),
                ),
            },
            Some(Err(error)) => (
                core::StateStore::in_memory().expect("SQLite memory store must be available"),
                Some(format!("Persistent SQLite state unavailable: {error}")),
            ),
            None => (
                core::StateStore::in_memory().expect("SQLite memory store must be available"),
                Some(format!(
                    "Persistent SQLite state unavailable: {}",
                    path_error
                        .as_deref()
                        .unwrap_or("no durable state path is available")
                )),
            ),
        };
        if let Some(error) = state_directory_error {
            persistence_warning.get_or_insert(error);
        }
        let (recovered, orphaned, unverified_processes) = if persistence_warning.is_none() {
            let processes = match store.active_processes() {
                Ok(processes) => processes,
                Err(error) => {
                    persistence_warning = Some(format!(
                        "Persistent SQLite recovery unavailable; execution is blocked: {error}"
                    ));
                    Vec::new()
                }
            };
            if persistence_warning.is_some() {
                (0, 0, Vec::new())
            } else {
                let mut unverified = Vec::new();
                for process in &processes {
                    if process.pid > 0 && recorded_process_matches(process) {
                        terminate_recorded_process_group(process);
                    } else {
                        unverified.push(process.clone());
                    }
                }
                match store.recover_abandoned_jobs_preserving(&unverified) {
                    Ok(recovered) => (recovered, processes.len(), unverified),
                    Err(error) => {
                        persistence_warning = Some(format!(
                            "Persistent SQLite recovery failed; execution is blocked: {error}"
                        ));
                        (0, 0, Vec::new())
                    }
                }
            }
        } else {
            (0, 0, Vec::new())
        };
        // Only the lock owner may reconcile stale runtime secrets. If any
        // recorded child identity could not be verified, fail closed and
        // preserve age-only secret directories: an unverified process may
        // still depend on its passfile. The operator can review and clean it
        // up after confirming the process is gone.
        if persistence_warning.is_none() && unverified_processes.is_empty() {
            cleanup_stale_secret_directories(&secret_runtime_base());
        }
        let mut initial_output = persistence_warning.clone().map_or_else(
            || vec!["Ready. Start with Preflight against a test destination mailbox.".into()],
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
        let unverified_process_count = unverified_processes.len();
        if unverified_process_count > 0 {
            initial_output.push(format!(
                "{unverified_process_count} recorded process identity(ies) could not be verified and were not signalled; review the affected jobs before retrying."
            ));
            initial_output.push(
                "Stale secret cleanup was deferred because an unverified process may still need its passfile."
                    .into(),
            );
        }
        if recovered > 0 {
            initial_output.push(format!(
                "Recovered {recovered} interrupted job(s) into Attention for review."
            ));
        }
        let mut initial_output: VecDeque<String> = initial_output.into_iter().collect();
        let (mut form, profile_warning) = match Form::load() {
            Ok(form) => (form, None),
            Err(error) => (
                Form::default(),
                Some(format!(
                    "Saved migration profile is unavailable; execution is blocked: {error}"
                )),
            ),
        };
        if let Some(warning) = profile_warning.as_ref() {
            initial_output.push_back(warning.clone());
            initial_output.push_back(
                "History and reports remain available; repair the profile before execution.".into(),
            );
        }
        if let Some(warning) = persistence_warning.as_ref()
            && !initial_output.iter().any(|line| line == warning)
        {
            initial_output.push_front(warning.clone());
            initial_output.push_back(
                "WARNING: saved configuration must be repaired before execution.".into(),
            );
        }
        if form.profile.destination_tls.is_empty() {
            form.profile.destination_tls = default_destination_tls();
        }
        let restored_project = if persistence_warning.is_none() {
            match store.latest_project() {
                Ok(project) => project,
                Err(error) => {
                    persistence_warning = Some(format!(
                        "Persistent project restore failed; execution is blocked: {error}"
                    ));
                    None
                }
            }
        } else {
            None
        };
        let (project_id, job_id) = restored_project
            .as_ref()
            .filter(|project| {
                project.source_endpoint == form.profile.source_host
                    && project.destination_endpoint == form.profile.destination_host
            })
            .map(|project| {
                let job_id = match store.first_mailbox(&project.id) {
                    Ok(job_id) => job_id,
                    Err(error) => {
                        persistence_warning = Some(format!(
                            "Persistent mailbox restore failed; execution is blocked: {error}"
                        ));
                        None
                    }
                };
                if let Some(job) = &job_id {
                    match store.mailbox_identity(job) {
                        Ok(Some((source_user, destination_user, state))) => {
                            // Restore non-secret mailbox identity and make
                            // recovery state visible immediately. Passwords
                            // remain blank and must be entered again before a
                            // live run.
                            form.profile.source_user = source_user;
                            form.profile.destination_user = destination_user;
                            if state == "attention" {
                                form.dry_run = true;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            persistence_warning = Some(format!(
                                "Persistent mailbox identity restore failed; execution is blocked: {error}"
                            ));
                        }
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
            let jobs = match store.mailboxes(&project.id) {
                Ok(jobs) => jobs,
                Err(error) => {
                    persistence_warning = Some(format!(
                        "Persistent batch restore failed; execution is blocked: {error}"
                    ));
                    return None;
                }
            };
            for job in jobs {
                let profile = match decode_persisted_batch_profile(job.config.as_deref(), &job.id) {
                    Ok(profile) => profile,
                    Err(error) => {
                        persistence_warning = Some(format!("{error}; execution is blocked"));
                        return None;
                    }
                };
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
        if let Some(warning) = persistence_warning.as_ref()
            && !initial_output.iter().any(|line| line == warning)
        {
            initial_output.push_front(warning.clone());
            initial_output.push_back(
                "WARNING: durable state could not be restored; repair the ledger before execution."
                    .into(),
            );
        }
        let restored_bulk_preflight_credential_fingerprints = vec![None; restored_bulk_jobs.len()];
        let restored_bulk_job_index_by_id = restored_bulk_job_ids
            .iter()
            .enumerate()
            .map(|(index, job_id)| (job_id.clone(), index))
            .collect();
        Self {
            form,
            output: initial_output,
            receiver: None,
            status: persistence_warning
                .clone()
                .or_else(|| profile_warning.clone())
                .unwrap_or_else(|| "Idle".into()),
            preview: false,
            bulk_jobs: restored_bulk_jobs,
            bulk_open: false,
            settings_open: false,
            bulk_search: String::new(),
            bulk_state_filter: "all".into(),
            bulk_selected_ids: HashSet::new(),
            bulk_message: if restored_bulk_project_id.is_some() {
                "Restored durable batch queue; credentials must be entered again before validation."
                    .into()
            } else {
                "Import a CSV, XLS, or XLSX file to build a reviewable queue.".into()
            },
            advanced_open: false,
            // Do not interrupt first launch with a configuration dialog. The
            // conservative Auto engine is already selected and can be
            // changed from the migration plan when the endpoints are known.
            engine_open: false,
            store,
            _instance_lock: instance_lock.and_then(Result::ok),
            process_review_required: unverified_process_count > 0,
            persistence_available: persistence_warning.is_none(),
            profile_available: profile_warning.is_none(),
            selected_project_id: restored_bulk_project_id
                .clone()
                .or_else(|| project_id.clone()),
            workspace_read_only: false,
            projects_open: false,
            project_search: String::new(),
            project_id,
            job_id,
            preflight_credential_fingerprint: None,
            run_id: None,
            active_run: None,
            locked_profile: None,
            locked_dry_run: None,
            cancel_requested: None,
            bulk_project_id: restored_bulk_project_id,
            bulk_job_ids: restored_bulk_job_ids,
            bulk_job_index_by_id: restored_bulk_job_index_by_id,
            bulk_preflight_credential_fingerprints: restored_bulk_preflight_credential_fingerprints,
            preflight: Vec::new(),
            capability_receiver: None,
            live_auth_receiver: None,
            live_auth_proof: None,
            source_capabilities: None,
            destination_capabilities: None,
            live_confirm_open: false,
            live_confirmed: false,
            live_confirmation_plan: None,
            durability_error: false,
            durability_recovery_pending: false,
            stop_confirm_open: false,
            keyring_open: false,
            active_view: WorkspaceView::Overview,
            pending_evidence: None,
            pending_batch_evidence: HashMap::new(),
            pending_checkpoint: None,
            pending_batch_checkpoints: HashMap::new(),
            pending_db_events: Vec::new(),
            deferred_events: VecDeque::new(),
            run_started_at: None,
            // Migration windows are log-heavy and commonly run in dark,
            // low-glare operator environments.
            dark_mode: appearance.dark_mode,
            ui_scale: appearance.ui_scale,
            bulk_live_confirm_open: false,
            bulk_live_confirmed: false,
            bulk_confirmation_summary: None,
            bulk_dry_run: true,
            bulk_clear_confirm_open: false,
            pending_bulk_import: None,
            bulk_import_receiver: None,
            bulk_live_run: false,
            bulk_retry_scope: BulkRetryScope::default(),
            bulk_source_keyring_apply: String::new(),
            bulk_destination_keyring_apply: String::new(),
            verification_exception_operator: String::new(),
            verification_exception_reason: String::new(),
            verification_search: String::new(),
            verification_filter: "all".into(),
            activity_show_all: false,
            activity_search: String::new(),
            activity_status_filter: "all".into(),
            reopen_reason: String::new(),
            ui_projects: Vec::new(),
            ui_runs: Vec::new(),
            ui_report: None,
            ui_project: None,
            ui_jobs: Vec::new(),
            ui_snapshot_project_id: None,
            ui_snapshot_refreshed_at: None,
        }
    }
}

fn persistent_state_path() -> Result<PathBuf, String> {
    persistent_state_path_from(
        std::env::var_os("MAILSWIFTSYNC_STATE_PATH"),
        dirs_next::data_local_dir(),
    )
}

fn persistent_state_path_from(
    state_override: Option<OsString>,
    data_directory: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if let Some(path) = state_override {
        let path = PathBuf::from(path);
        if path.as_os_str().is_empty() {
            return Err("MAILSWIFTSYNC_STATE_PATH is set but empty.".into());
        }
        return Ok(path);
    }
    let data_directory = data_directory.ok_or_else(|| {
        "Cannot determine a durable state directory; set MAILSWIFTSYNC_STATE_PATH to an explicit ledger path.".to_owned()
    })?;
    Ok(data_directory.join("mailswiftsync/state.db"))
}

/// Restore a verified SQLite ledger without ever replacing the destination
/// with an unchecked or partially copied file. An existing destination is
/// retained as a uniquely named rollback artifact.
fn restore_ledger(
    backup: &std::path::Path,
    destination: &std::path::Path,
) -> Result<Option<PathBuf>, String> {
    if backup == destination {
        return Err("Restore source and destination must be different files.".into());
    }
    core::StateStore::open_readonly(backup)
        .map_err(|error| format!("restore source is not a valid current ledger: {error}"))?;
    let parent = destination
        .parent()
        .ok_or("Restore destination has no parent directory.")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("could not create restore directory: {error}"))?;
    restrict_directory_permissions(parent)
        .map_err(|error| format!("could not secure restore directory: {error}"))?;
    if !destination.exists() {
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{}", destination.display(), suffix));
            if sidecar.exists() {
                return Err(format!(
                    "restore destination has an orphaned SQLite sidecar {}; remove or recover it before restoring",
                    sidecar.display()
                ));
            }
        }
    }
    let temporary = parent.join(format!(
        ".{}.restore-{}.tmp",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state.db"),
        uuid::Uuid::new_v4()
    ));
    std::fs::copy(backup, &temporary)
        .map_err(|error| format!("could not copy restore source: {error}"))?;
    if let Err(error) = restrict_file_permissions(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("could not secure restored ledger: {error}"));
    }
    if let Err(error) = core::StateStore::open_readonly(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!(
            "copied restore ledger failed integrity/schema validation: {error}"
        ));
    }
    let previous = if destination.exists() {
        let path = parent.join(format!(
            "{}.pre-restore-{}.db",
            destination
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("state"),
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = std::fs::rename(destination, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("could not preserve existing destination: {error}"));
        }
        let mut moved_sidecars = Vec::new();
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{}", destination.display(), suffix));
            if !sidecar.exists() {
                continue;
            }
            let previous_sidecar = PathBuf::from(format!("{}{}", path.display(), suffix));
            if let Err(error) = std::fs::rename(&sidecar, &previous_sidecar) {
                for (original, preserved) in moved_sidecars.into_iter().rev() {
                    let _ = std::fs::rename(preserved, original);
                }
                let _ = std::fs::rename(&path, destination);
                let _ = std::fs::remove_file(&temporary);
                return Err(format!(
                    "could not preserve SQLite sidecar {sidecar:?}: {error}"
                ));
            }
            moved_sidecars.push((sidecar, previous_sidecar));
        }
        Some(path)
    } else {
        None
    };
    if let Err(error) = std::fs::rename(&temporary, destination) {
        if let Some(previous) = &previous {
            let _ = std::fs::rename(previous, destination);
            for suffix in ["-wal", "-shm"] {
                let preserved = PathBuf::from(format!("{}{}", previous.display(), suffix));
                let original = PathBuf::from(format!("{}{}", destination.display(), suffix));
                let _ = std::fs::rename(preserved, original);
            }
        }
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("could not install restored ledger: {error}"));
    }
    restrict_file_permissions(destination).map_err(|error| {
        format!("restored ledger was installed but could not be secured: {error}")
    })?;
    Ok(previous)
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

#[cfg(test)]
fn next_ui_scale(current: f32) -> f32 {
    const SCALES: [f32; 5] = [0.90, 1.00, 1.10, 1.25, 1.50];
    SCALES
        .iter()
        .copied()
        .find(|scale| *scale > current + f32::EPSILON)
        .unwrap_or(SCALES[0])
}

fn terminal_phase_advance_allowed(
    external_succeeded: bool,
    terminal_write_ok: bool,
    durability_error: bool,
) -> bool {
    external_succeeded && terminal_write_ok && !durability_error
}

fn recommended_next_action(
    phase: core::Phase,
    has_preflight: bool,
    attention_count: usize,
    running: bool,
) -> &'static str {
    if running {
        return "A migration is running — monitor Activity or use Stop migration if you need to halt it.";
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

fn endpoint_for_probe(host: &str, configured_port: &str) -> Result<String, String> {
    let host = host.trim();
    if configured_port.trim().is_empty() {
        return Ok(host.to_owned());
    }
    let port = configured_port
        .trim()
        .parse::<u16>()
        .map_err(|_| "invalid endpoint port".to_owned())?;
    if port == 0 {
        return Err("endpoint port must be between 1 and 65535".into());
    }
    let (host, _) = endpoint::parts(host, 993)?;
    if host.contains(':') {
        Ok(format!("[{host}]:{port}"))
    } else {
        Ok(format!("{host}:{port}"))
    }
}

/// Command previews and fingerprints must not silently turn malformed input
/// into a different endpoint. Validation rejects the sentinel before launch;
/// this helper keeps tuple-based command-generation APIs fail-closed too.
fn command_endpoint_parts(host: &str, default_port: u16) -> (String, u16) {
    endpoint::parts(host, default_port).unwrap_or_else(|_| ("<invalid-endpoint>".to_owned(), 0))
}

fn command_port(configured_port: &str, endpoint_port: u16) -> u16 {
    match configured_port.trim().parse::<u16>() {
        Ok(port) if port != 0 => port,
        _ if configured_port.trim().is_empty() => endpoint_port,
        _ => 0,
    }
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

fn read_imap_tagged<S: Read>(
    stream: &mut S,
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

fn read_imap_greeting<S: Read>(stream: &mut S, host: &str) -> Result<String, String> {
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
    Ok(greeting)
}

fn imap_command_succeeded(response: &str, tag: &str) -> bool {
    response
        .lines()
        .any(|line| line.starts_with(&format!("{tag} OK")))
}

fn probe_tls_capabilities_with_transport(
    host: &str,
    user: &str,
    password: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
) -> Result<core::ServerCapabilities, String> {
    let (server_name, port) = endpoint::parts(host, default_imap_port(transport))
        .map_err(|error| format!("Invalid IMAP host {host}: {error}"))?;
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
    let mut tcp = tcp.ok_or_else(|| {
        format!(
            "{host}: could not connect to any resolved address: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown connection error".into())
        )
    })?;
    tcp.set_read_timeout(Some(Duration::from_secs(8)))
        .map_err(|e| e.to_string())?;
    let mut roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if !ca_bundle.trim().is_empty() {
        let file = std::fs::File::open(ca_bundle.trim())
            .map_err(|error| format!("{host}: could not open additional CA bundle: {error}"))?;
        let mut reader = BufReader::new(file);
        let mut loaded = 0;
        for certificate in certs(&mut reader) {
            let certificate = certificate
                .map_err(|error| format!("{host}: invalid certificate in CA bundle: {error}"))?;
            roots
                .add(certificate)
                .map_err(|error| format!("{host}: could not add CA certificate: {error}"))?;
            loaded += 1;
        }
        if loaded == 0 {
            return Err(format!("{host}: CA bundle contained no PEM certificates"));
        }
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from(server_name.to_owned())
        .map_err(|e| format!("{host}: invalid TLS server name: {e}"))?;

    if transport == "starttls" {
        let mut response = String::new();
        let mut buffer = [0; 4096];
        let greeting = read_imap_greeting(&mut tcp, host)?;
        tcp.write_all(b"s001 CAPABILITY\r\n")
            .map_err(|e| e.to_string())?;
        read_imap_tagged(&mut tcp, "s001", &mut response, &mut buffer)?;
        if !imap_command_succeeded(&response, "s001")
            || !response.to_ascii_uppercase().contains("STARTTLS")
        {
            return Err(format!("{host}: server does not advertise STARTTLS"));
        }
        tcp.write_all(b"s002 STARTTLS\r\n")
            .map_err(|e| e.to_string())?;
        read_imap_tagged(&mut tcp, "s002", &mut response, &mut buffer)?;
        if !imap_command_succeeded(&response, "s002") {
            return Err(format!("{host}: STARTTLS negotiation failed"));
        }
        let connection = ClientConnection::new(Arc::new(config), name)
            .map_err(|e| format!("{host}: TLS configuration failed: {e}"))?;
        let mut stream = StreamOwned::new(connection, tcp);
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(|error| format!("{host}: TLS handshake failed: {error}"))?;
        verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
        return complete_authenticated_imap_probe(stream, host, user, password, greeting);
    }

    let connection = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| format!("{host}: TLS configuration failed: {e}"))?;
    let mut stream = StreamOwned::new(connection, tcp);
    let greeting = read_imap_greeting(&mut stream, host)?;
    verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
    complete_authenticated_imap_probe(stream, host, user, password, greeting)
}

fn verify_certificate_pin(
    stream: &StreamOwned<ClientConnection, TcpStream>,
    host: &str,
    expected: &str,
) -> Result<(), String> {
    if expected.trim().is_empty() {
        return Ok(());
    }
    let certificate = stream
        .conn
        .peer_certificates()
        .and_then(|certificates| certificates.first())
        .ok_or_else(|| format!("{host}: TLS peer did not provide a certificate"))?;
    let actual = format!("{:x}", Sha256::digest(certificate.as_ref()));
    if actual != expected.trim().to_ascii_lowercase() {
        return Err(format!(
            "{host}: TLS certificate SHA-256 pin mismatch (presented {actual})"
        ));
    }
    Ok(())
}

fn complete_authenticated_imap_probe<S: Read + Write>(
    mut stream: S,
    host: &str,
    user: &str,
    password: &str,
    greeting: String,
) -> Result<core::ServerCapabilities, String> {
    let mut response = String::new();
    let mut buffer = [0; 4096];
    stream
        .write_all(b"a001 CAPABILITY\r\n")
        .map_err(|e| e.to_string())?;
    read_imap_tagged(&mut stream, "a001", &mut response, &mut buffer)?;
    if !imap_command_succeeded(&response, "a001") {
        return Err(format!("{host}: pre-auth CAPABILITY failed"));
    }
    let preauth = greeting.contains("* PREAUTH");
    if !preauth {
        let quoted_password = Zeroizing::new(imap_quote(password)?);
        let login = format!(
            "a002 LOGIN {} {}\r\n",
            imap_quote(user)?,
            quoted_password.as_str()
        );
        let login = Zeroizing::new(login);
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
    if !list_response.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("* LIST ") || line.starts_with("* LIST\t")
    }) {
        return Err(format!(
            "{host}: folder inventory returned no untagged LIST records"
        ));
    }
    let caps = core::ServerCapabilities::parse_with_inventory(&post_auth_response, &list_response);
    if caps.values.is_empty() {
        return Err(format!(
            "{host}: server did not return a CAPABILITY response"
        ));
    }
    let mut caps = caps;
    if caps.supports("QUOTA") {
        let mut quota_response = String::new();
        stream
            .write_all(b"a006 GETQUOTAROOT \"\"\r\n")
            .map_err(|e| e.to_string())?;
        // A provider can advertise QUOTA while denying the root query. Keep
        // that result explicitly unknown; quota support is advisory and must
        // not turn an otherwise valid mailbox into a false capacity failure.
        if read_imap_tagged(&mut stream, "a006", &mut quota_response, &mut buffer).is_ok() {
            caps.record_quota_response(&quota_response);
        }
    }
    let _ = stream.write_all(b"a007 LOGOUT\r\n");
    Ok(caps)
}

/// Re-authenticate both encrypted endpoints immediately before a live imapsync
/// child is launched. Plain transport remains excluded because it has no TLS
/// boundary to validate and is already protected by the explicit transport
/// acknowledgement gate.
fn fresh_dual_imaps_authentication(form: &Form) -> Result<(), String> {
    if !fresh_imap_authentication_applies(form) {
        return Ok(());
    }
    let source = endpoint_for_probe(&form.profile.source_host, &form.profile.source_port)?;
    let destination = endpoint_for_probe(
        &form.profile.destination_host,
        &form.profile.destination_port,
    )?;
    let source_capabilities = probe_tls_capabilities_with_transport(
        &source,
        &form.profile.source_user,
        form.source_password.as_str(),
        &form.profile.source_tls,
        &form.profile.source_ca_bundle,
        &form.profile.source_certificate_pin_sha256,
    )?;
    if source_capabilities.quota_exceeded {
        return Err(
            "source mailbox quota is exhausted according to the authenticated IMAP quota response"
                .into(),
        );
    }
    let destination_capabilities = probe_tls_capabilities_with_transport(
        &destination,
        &form.profile.destination_user,
        form.destination_password.as_str(),
        &form.profile.destination_tls,
        &form.profile.destination_ca_bundle,
        &form.profile.destination_certificate_pin_sha256,
    )?;
    if destination_capabilities.quota_exceeded {
        return Err("destination mailbox quota is exhausted according to the authenticated IMAP quota response".into());
    }
    Ok(())
}

fn fresh_imap_authentication_applies(form: &Form) -> bool {
    !form.dry_run
        && form.engine() == core::Engine::ImapSync
        && form.profile.source_tls != "plain"
        && form.profile.destination_tls != "plain"
}

fn quota_summary(caps: &core::ServerCapabilities) -> &'static str {
    if !caps.supports("QUOTA") {
        "quota not advertised"
    } else if caps.quota_exceeded {
        "quota exceeded"
    } else if caps.quota_observed {
        "quota reported within limit"
    } else {
        "quota status unavailable; verify capacity with the provider"
    }
}

impl App {
    fn theme_colors(&self) -> ThemeColors {
        if self.dark_mode {
            ThemeColors::dark()
        } else {
            ThemeColors::light()
        }
    }

    /// Refresh the database-backed UI read model at a low frequency. egui may
    /// repaint many times per second while a process is producing output;
    /// those repaints must not turn into repeated SQLite reads.
    fn refresh_ui_snapshot(&mut self) {
        if self
            .ui_snapshot_refreshed_at
            .is_some_and(|at| at.elapsed() < Duration::from_millis(500))
        {
            return;
        }
        self.ui_snapshot_refreshed_at = Some(std::time::Instant::now());
        if let Ok(projects) = self.store.recent_projects(500) {
            self.ui_projects = projects;
        }
        let project_id = self.active_project_id().map(str::to_owned);
        if project_id != self.ui_snapshot_project_id {
            self.ui_snapshot_project_id = project_id.clone();
            self.ui_report = None;
            self.ui_runs.clear();
            self.ui_project = None;
            self.ui_jobs.clear();
        }
        if let Some(project_id) = project_id {
            if let Ok(project) = self.store.project(&project_id) {
                self.ui_project = project;
            }
            if let Ok(jobs) = self.store.mailboxes(&project_id) {
                self.ui_jobs = jobs;
            }
            if let Ok(Some(report)) = self.store.project_report_snapshot(&project_id) {
                self.ui_report = Some(report);
            }
            if let Ok(runs) = self
                .store
                .recent_run_list(&project_id, MAX_ACTIVITY_HISTORY_ROWS)
            {
                self.ui_runs = runs;
            }
        }
    }

    fn current_editable_project_id(&self) -> Option<&str> {
        self.active_run
            .as_ref()
            .map(|run| run.project_id.as_str())
            .or(self.bulk_project_id.as_deref())
            .or(self.project_id.as_deref())
    }

    fn select_workspace_project(&mut self, project_id: String) {
        if self.running() {
            self.status = "Project switching is disabled while a migration is running.".into();
            return;
        }
        let editable_id = self.current_editable_project_id().map(str::to_owned);
        self.workspace_read_only = editable_id.as_deref() != Some(project_id.as_str());
        self.selected_project_id = Some(project_id);
        self.active_view = WorkspaceView::Overview;
        if self.workspace_read_only {
            self.preflight.clear();
            self.source_capabilities = None;
            self.destination_capabilities = None;
            self.status = "Viewing a historical project read-only. Start a new migration to edit or execute a plan.".into();
        }
    }

    fn start_new_migration(&mut self) {
        if self.running() {
            self.status =
                "A migration is running; finish or stop it before starting a new workspace.".into();
            return;
        }
        self.selected_project_id = None;
        self.workspace_read_only = false;
        self.project_id = None;
        self.job_id = None;
        self.bulk_project_id = None;
        self.bulk_job_ids.clear();
        self.bulk_job_index_by_id.clear();
        self.bulk_selected_ids.clear();
        self.bulk_preflight_credential_fingerprints.clear();
        self.bulk_jobs.clear();
        self.preflight.clear();
        self.source_capabilities = None;
        self.destination_capabilities = None;
        self.live_auth_proof = None;
        self.live_confirmed = false;
        self.live_confirmation_plan = None;
        self.form = Form::default();
        self.active_view = WorkspaceView::Plan;
        self.status =
            "New migration workspace ready; configure the endpoints before preflight.".into();
    }

    fn active_project_id(&self) -> Option<&str> {
        preferred_project_id(
            self.active_run.as_ref().map(|run| run.project_id.as_str()),
            self.selected_project_id.as_deref(),
            self.bulk_project_id.as_deref(),
            self.project_id.as_deref(),
        )
    }

    fn start_capability_probe(&mut self) {
        let credential_load = if self.form.dry_run {
            self.form.load_configured_keyring_credentials()
        } else {
            self.form.reload_configured_keyring_credentials()
        };
        if let Err(error) = credential_load {
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
        if self.form.profile.source_tls == "plain" || self.form.profile.destination_tls == "plain" {
            self.status = "Authenticated capability discovery requires encrypted IMAP; plain transport remains blocked by the explicit cleartext acknowledgement gate.".into();
            return;
        }
        let source = match endpoint_for_probe(
            &self.form.profile.source_host,
            &self.form.profile.source_port,
        ) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.status = format!("Source readiness probe blocked: {error}");
                return;
            }
        };
        let destination = match endpoint_for_probe(
            &self.form.profile.destination_host,
            &self.form.profile.destination_port,
        ) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.status = format!("Destination readiness probe blocked: {error}");
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        self.capability_receiver = Some(rx);
        self.status = "Authenticating and inspecting IMAPS readiness…".into();
        let source_user = self.form.profile.source_user.clone();
        let source_password = self.form.source_password.clone();
        let destination_user = self.form.profile.destination_user.clone();
        let destination_password = self.form.destination_password.clone();
        let source_tls = self.form.profile.source_tls.clone();
        let destination_tls = self.form.profile.destination_tls.clone();
        let source_ca_bundle = self.form.profile.source_ca_bundle.clone();
        let source_certificate_pin_sha256 = self.form.profile.source_certificate_pin_sha256.clone();
        let destination_ca_bundle = self.form.profile.destination_ca_bundle.clone();
        let destination_certificate_pin_sha256 =
            self.form.profile.destination_certificate_pin_sha256.clone();
        thread::spawn(move || {
            let result = probe_tls_capabilities_with_transport(
                &source,
                &source_user,
                source_password.as_str(),
                &source_tls,
                &source_ca_bundle,
                &source_certificate_pin_sha256,
            )
            .and_then(|left| {
                probe_tls_capabilities_with_transport(
                    &destination,
                    &destination_user,
                    destination_password.as_str(),
                    &destination_tls,
                    &destination_ca_bundle,
                    &destination_certificate_pin_sha256,
                )
                .map(|right| (left, right))
            });
            let _ = tx.send(result);
        });
    }

    fn requires_live_imaps_auth_probe(&self) -> bool {
        fresh_imap_authentication_applies(&self.form)
    }

    fn start_live_imaps_auth_probe(
        &mut self,
        plan_fingerprint: String,
        credential_fingerprint: String,
    ) {
        if self.live_auth_receiver.is_some() {
            return;
        }
        let source = match endpoint_for_probe(
            &self.form.profile.source_host,
            &self.form.profile.source_port,
        ) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.status = format!("Live authentication probe blocked: {error}");
                return;
            }
        };
        let destination = match endpoint_for_probe(
            &self.form.profile.destination_host,
            &self.form.profile.destination_port,
        ) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.status = format!("Live authentication probe blocked: {error}");
                return;
            }
        };
        let source_user = self.form.profile.source_user.clone();
        let source_password = self.form.source_password.clone();
        let destination_user = self.form.profile.destination_user.clone();
        let destination_password = self.form.destination_password.clone();
        let source_tls = self.form.profile.source_tls.clone();
        let destination_tls = self.form.profile.destination_tls.clone();
        let source_ca_bundle = self.form.profile.source_ca_bundle.clone();
        let source_certificate_pin_sha256 = self.form.profile.source_certificate_pin_sha256.clone();
        let destination_ca_bundle = self.form.profile.destination_ca_bundle.clone();
        let destination_certificate_pin_sha256 =
            self.form.profile.destination_certificate_pin_sha256.clone();
        let (tx, rx) = mpsc::channel();
        self.live_auth_receiver = Some(rx);
        self.status = "Re-authenticating encrypted IMAP endpoints before live execution…".into();
        thread::spawn(move || {
            let result = probe_tls_capabilities_with_transport(
                &source,
                &source_user,
                source_password.as_str(),
                &source_tls,
                &source_ca_bundle,
                &source_certificate_pin_sha256,
            )
            .and_then(|_| {
                probe_tls_capabilities_with_transport(
                    &destination,
                    &destination_user,
                    destination_password.as_str(),
                    &destination_tls,
                    &destination_ca_bundle,
                    &destination_certificate_pin_sha256,
                )
                .map(|_| LiveAuthProof {
                    plan_fingerprint,
                    credential_fingerprint,
                })
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
                "Execution mode".into(),
                if self.form.dry_run {
                    "Preflight enabled — destination will not be intentionally changed".into()
                } else {
                    "Live migration enabled — destination may be changed".into()
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
                format!(
                    "{} · {} folder(s) discovered{} · {}",
                    caps.strategy().join(" · "),
                    caps.mailbox_count,
                    if caps.special_use_mailboxes > 0 {
                        format!(" · {} SPECIAL-USE folder(s)", caps.special_use_mailboxes)
                    } else {
                        String::new()
                    },
                    quota_summary(caps)
                ),
                caps.inventory_complete && !caps.quota_exceeded,
            ));
        }
        if let Some(caps) = &self.destination_capabilities {
            self.preflight.push((
                "Destination capabilities".into(),
                format!(
                    "{} · {} folder(s) discovered{} · {}",
                    caps.strategy().join(" · "),
                    caps.mailbox_count,
                    if caps.special_use_mailboxes > 0 {
                        format!(" · {} SPECIAL-USE folder(s)", caps.special_use_mailboxes)
                    } else {
                        String::new()
                    },
                    quota_summary(caps)
                ),
                caps.inventory_complete && !caps.quota_exceeded,
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
                self.selected_project_id = Some(project.id.clone());
                self.project_id = Some(project.id);
                self.job_id = Some(job);
                self.status = "Project created; ready for preflight review".into();
            }
            Err(e) => self.status = format!("Could not create project: {e}"),
        }
    }
    fn projects_dialog(&mut self, ctx: &egui::Context) {
        if !self.projects_open {
            return;
        }
        let mut open = self.projects_open;
        egui::Window::new("Projects")
            .open(&mut open)
            .default_width(760.0)
            .default_height(520.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.heading("Migration projects");
                ui.label(RichText::new("Select a durable project to make it the workspace for reports, mailboxes, activity, and verification.").color(MUTED));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Search");
                    ui.add(egui::TextEdit::singleline(&mut self.project_search)
                        .hint_text("project name, source, or destination")
                        .desired_width(320.0));
                });
                ui.add_space(8.0);
                let search = self.project_search.trim().to_ascii_lowercase();
                {
                    let projects = self.ui_projects.clone();
                        let visible = projects.into_iter().filter(|project| {
                            search.is_empty()
                                || project.name.to_ascii_lowercase().contains(&search)
                                || project.source_endpoint.to_ascii_lowercase().contains(&search)
                                || project.destination_endpoint.to_ascii_lowercase().contains(&search)
                        }).collect::<Vec<_>>();
                        ui.label(RichText::new(format!("{} project(s)", visible.len())).color(MUTED));
                        egui::ScrollArea::vertical()
                            .max_height(360.0)
                            .show(ui, |ui| {
                                egui::Grid::new("project_browser").striped(true).show(ui, |ui| {
                                    ui.strong("Project");
                                    ui.strong("Phase");
                                    ui.strong("Source");
                                    ui.strong("Destination");
                                    ui.end_row();
                                    for project in visible {
                                        let selected = self.selected_project_id.as_deref() == Some(project.id.as_str());
                                        if ui.selectable_label(selected, &project.name).clicked() {
                                            self.select_workspace_project(project.id.clone());
                                            self.projects_open = false;
                                        }
                                        ui.label(format_phase_name(project.phase));
                                        ui.label(&project.source_endpoint);
                                        ui.label(&project.destination_endpoint);
                                        ui.end_row();
                                    }
                                });
                            });
                    }
                ui.add_space(8.0);
                if ui.button("New migration plan").clicked() {
                    self.start_new_migration();
                    self.projects_open = false;
                }
            });
        self.projects_open = open && self.projects_open;
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = self.settings_open;
        let mut close_requested = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Operator settings");
                ui.label(RichText::new("Workspace tools are grouped here so the header stays focused on project and run status.").color(MUTED));
                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.heading("Appearance");
                    ui.horizontal(|ui| {
                        ui.label("Theme");
                        let label = if self.dark_mode { "Dark" } else { "Light" };
                        if ui.button(label).clicked() {
                            self.dark_mode = !self.dark_mode;
                            if let Err(error) = (AppearancePreferences { dark_mode: self.dark_mode, ui_scale: self.ui_scale }).save() {
                                self.status = format!("Could not save appearance preference: {error}");
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(format!("Interface size: {:.0}%", self.ui_scale * 100.0));
                        if ui.button("Decrease").clicked() {
                            self.ui_scale = (self.ui_scale - 0.10).max(0.90);
                        }
                        if ui.button("Increase").clicked() {
                            self.ui_scale = (self.ui_scale + 0.10).min(1.50);
                        }
                    });
                    if ui.button("Save appearance preferences").clicked()
                        && let Err(error) = (AppearancePreferences { dark_mode: self.dark_mode, ui_scale: self.ui_scale }).save()
                    {
                        self.status = format!("Could not save appearance preference: {error}");
                    }
                });
                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.heading("Migration configuration");
                    if ui.button("Project browser").clicked() {
                        self.projects_open = true;
                        close_requested = true;
                    }
                    if ui.button("Credentials").clicked() { self.keyring_open = true; }
                    if ui.button(format!("Engine: {}", self.form.engine().label())).clicked() { self.engine_open = true; }
                    if ui.button("Advanced options").clicked() { self.advanced_open = true; }
                    if ui.button("Preflight & readiness").clicked() {
                        self.active_view = WorkspaceView::Overview;
                        close_requested = true;
                        self.assess_plan();
                    }
                });
            });
        self.settings_open = open && !close_requested;
    }

    fn plan_completeness(&self) -> (usize, usize) {
        // This is deliberately limited to values the local form can prove.
        // Network authentication and server capability checks belong to the
        // explicit preflight assessment below, not to this counter.
        let checks = 4;
        let passed = [
            !self.form.profile.source_host.trim().is_empty(),
            !self.form.profile.destination_host.trim().is_empty(),
            !self.form.profile.source_user.trim().is_empty(),
            !self.form.profile.destination_user.trim().is_empty(),
        ]
        .into_iter()
        .filter(|ok| *ok)
        .count();
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
        let current = match self.active_project_id() {
            None => core::Phase::Discovery,
            Some(project_id) => match self.store.project(project_id) {
                Ok(Some(project)) => project.phase,
                Ok(None) => core::Phase::Discovery,
                Err(error) => {
                    ui.group(|ui| {
                        ui.label(
                            RichText::new("DURABLE LIFECYCLE UNAVAILABLE")
                                .strong()
                                .color(ALERT),
                        );
                        ui.label(format!(
                            "Could not read the selected project from SQLite: {error}"
                        ));
                        ui.label(
                            "Do not start or retry a migration until durable storage is available.",
                        );
                    });
                    return;
                }
            },
        };
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
        if self.process_review_required {
            ui.group(|ui| {
                ui.label(RichText::new("PROCESS OWNERSHIP REVIEW REQUIRED").strong().color(ALERT));
                ui.label("MailSwiftSync could not prove that a previously recorded migration process is gone. Do not start another migration until you have checked the host process list and confirmed no MailSwiftSync engine remains.");
                if ui.button("I confirmed no unverified migration process remains").clicked() {
                    match self.store.clear_active_processes_after_review() {
                        Ok(()) => {
                            self.process_review_required = false;
                            self.status = "Process review acknowledged; execution gates are available again.".into();
                        }
                        Err(error) => {
                            self.status = format!(
                                "Could not clear reviewed process identities: {error}"
                            );
                        }
                    }
                }
            });
        }
        if !self.workspace_read_only {
            self.source_transport_warning(ui);
        }
        if self.workspace_read_only {
            ui.group(|ui| {
                ui.label(RichText::new("HISTORICAL PROJECT · READ ONLY").strong().color(BLUE));
                ui.label("You are viewing durable history for this project. The editable migration plan and execution controls are detached until you start a new migration.");
                if ui.button("Start a new migration").clicked() {
                    self.start_new_migration();
                }
            });
            ui.add_space(8.0);
        }
        if self.active_view != WorkspaceView::Plan {
            match self.active_view {
                WorkspaceView::Overview => self.overview_view(ui),
                WorkspaceView::Mailboxes => self.mailbox_view(ui),
                WorkspaceView::Activity => self.activity_view(ui),
                WorkspaceView::Verification => self.verification_view(ui),
                WorkspaceView::Plan => {}
            }
            return;
        }
        let (passed, total) = self.plan_completeness();
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.heading("Migration workspace");
                ui.label(RichText::new(if self.form.dry_run { "PREFLIGHT" } else { "LIVE MIGRATION" })
                    .strong()
                    .color(if self.form.dry_run { TEAL } else { ALERT }));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{passed}/{total} configuration items complete"))
                            .strong()
                            .color(if passed == total { TEAL } else { ALERT }),
                    );
                });
            });
            ui.add_space(5.0);
            ui.label(RichText::new("Recommended next step: run preflight, review blockers, then select a small pilot mailbox.").color(MUTED));
        });
    }

    fn overview_readiness_controls(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("Preflight & readiness");
            ui.label(RichText::new("Plan completeness is separate from live network checks. Run the authenticated probe before live migration.").color(MUTED));
            ui.horizontal_wrapped(|ui| {
                let probe_enabled = self.capability_receiver.is_none()
                    && self.form.engine() != core::Engine::Dovecot
                    && !self.running()
                    && !self.workspace_read_only;
                if ui.add_enabled(probe_enabled, egui::Button::new("Run authenticated readiness probe")).clicked() {
                    self.start_capability_probe();
                }
                if ui.add_enabled(!self.running() && !self.workspace_read_only, egui::Button::new("Refresh assessment")).clicked() {
                    self.assess_plan();
                }
                if self.active_project_id().is_none()
                    && ui.add_enabled(!self.running() && !self.workspace_read_only, egui::Button::new("Create project from plan")).clicked()
                {
                    self.create_project();
                }
            });
            if self.form.engine() == core::Engine::Dovecot {
                ui.label(RichText::new("Dovecot preflight checks the configured imapc source; destination readiness still requires administrative review.").color(MUTED));
            }
            if self.preflight.is_empty() {
                ui.label("No preflight assessment has been recorded for the current plan.");
            } else {
                egui::Grid::new("overview_preflight_controls").striped(true).show(ui, |ui| {
                    ui.strong("Check");
                    ui.strong("Result");
                    ui.end_row();
                    for (name, detail, passed) in &self.preflight {
                        ui.label(RichText::new(if *passed { "✓" } else { "!" }).color(if *passed { TEAL } else { ALERT }));
                        ui.label(RichText::new(name).strong());
                        ui.label(detail);
                        ui.end_row();
                    }
                });
            }
        });
        if let Some(project_id) = self.active_project_id().map(str::to_owned)
            && let Ok(Some(project)) = self.store.project(&project_id)
            && project.phase == core::Phase::Complete
        {
            ui.add_space(10.0);
            ui.group(|ui| {
                ui.heading("Project controls");
                ui.label(RichText::new("This project is complete and read-only. Reopening requires an audit reason and returns it to Attention.").color(ALERT));
                ui.horizontal(|ui| {
                    ui.label("Reason");
                    ui.text_edit_singleline(&mut self.reopen_reason);
                    if ui.add_enabled(!self.running() && !self.reopen_reason.trim().is_empty(), egui::Button::new("Reopen project")).clicked() {
                        self.status = match self.store.reopen_project(&project.id, &self.reopen_reason) {
                            Ok(()) => {
                                self.reopen_reason.clear();
                                "Project reopened for documented review".into()
                            }
                            Err(error) => format!("Could not reopen project: {error}"),
                        };
                    }
                });
            });
        }
    }

    fn overview_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Migration overview");
        ui.label(
            RichText::new("A calm, evidence-led workspace for moving mailboxes safely.")
                .color(MUTED),
        );
        ui.add_space(16.0);
        let project = self.ui_project.clone();
        let phase = project
            .as_ref()
            .map(|value| value.phase)
            .unwrap_or(core::Phase::Discovery);
        let durable_jobs = self.ui_jobs.clone();
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
        self.overview_readiness_controls(ui);
        ui.add_space(14.0);
        if project.is_none() && self.bulk_jobs.is_empty() {
            ui.group(|ui| {
                ui.heading("Start your first migration");
                ui.label("MailSwiftSync guides every migration through a reviewable preflight before any destination changes are allowed.");
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    for (number, title, detail) in [
                        ("1", "Connect", "Enter source and destination endpoints."),
                        ("2", "Assess", "Run a dry preflight and review the plan."),
                        ("3", "Prove", "Migrate, verify, and export customer evidence."),
                    ] {
                        ui.group(|ui| {
                            ui.label(RichText::new(format!("{number}  {title}")).strong());
                            ui.label(RichText::new(detail).color(MUTED));
                        });
                    }
                });
                if ui.button("Configure first mailbox  →").clicked() {
                    self.active_view = WorkspaceView::Plan;
                }
                ui.label(RichText::new("For multiple mailboxes, use Batch after reviewing one representative pilot.").color(MUTED));
            });
            ui.add_space(14.0);
        }
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
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.group(|ui| {
                ui.label(RichText::new("PROJECT STATUS").size(11.0).color(MUTED));
                ui.heading(if project.is_some() {
                    "Project created"
                } else {
                    "No project yet"
                });
                ui.label(if project.is_some() {
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
                    let verified_count = counts.get("verified").copied().unwrap_or(0)
                        + counts.get("verified_with_exceptions").copied().unwrap_or(0);
                    ui.label(format!(
                        "{} ready · {} running · {} verified",
                        counts.get("ready").copied().unwrap_or(0),
                        counts.get("running").copied().unwrap_or(0),
                        verified_count,
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
                if ui
                    .add_enabled(
                        !self.workspace_read_only,
                        egui::Button::new("Open migration plan  →"),
                    )
                    .clicked()
                {
                    self.active_view = WorkspaceView::Plan;
                }
                if ui
                    .add_enabled(
                        !self.workspace_read_only,
                        egui::Button::new("Refresh preflight assessment"),
                    )
                    .clicked()
                {
                    self.assess_plan();
                }
                if ui
                    .add_enabled(
                        !self.workspace_read_only,
                        egui::Button::new("Import mailbox list"),
                    )
                    .clicked()
                {
                    self.bulk_open = true;
                }
            });
        });
        ui.add_space(14.0);
        ui.label(RichText::new("Safety contract").strong());
        ui.horizontal_wrapped(|ui| {
            for text in [
                "Preflight is the default",
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
        let colors = self.theme_colors();
        ui.heading("Mailboxes");
        ui.label(RichText::new("Review, filter, select, and operate on customer mailboxes without reopening the legacy queue window.").color(MUTED));
        ui.add_space(12.0);
        if self.workspace_read_only {
            let Some(project_id) = self.active_project_id().map(str::to_owned) else {
                ui.label("No historical project is selected.");
                return;
            };
            match self.store.mailboxes(&project_id) {
                Ok(jobs) => {
                    ui.label(
                        RichText::new(format!(
                            "{} durable mailbox record(s) · read-only",
                            jobs.len()
                        ))
                        .strong(),
                    );
                    egui::ScrollArea::vertical().max_height(520.0).show_rows(
                        ui,
                        32.0,
                        jobs.len(),
                        |ui, rows| {
                            egui::Grid::new("historical_mailboxes")
                                .striped(true)
                                .show(ui, |ui| {
                                    if rows.start == 0 {
                                        ui.strong("Source");
                                        ui.strong("Destination");
                                        ui.strong("State");
                                        ui.end_row();
                                    }
                                    for index in rows {
                                        let job = &jobs[index];
                                        ui.label(&job.source_mailbox);
                                        ui.label(&job.destination_mailbox);
                                        let (badge, color) = job_state_badge(&job.state, colors);
                                        ui.label(RichText::new(badge).color(color));
                                        ui.end_row();
                                    }
                                });
                        },
                    );
                }
                Err(error) => {
                    ui.label(
                        RichText::new(format!("Historical mailbox records unavailable: {error}"))
                            .color(ALERT),
                    );
                }
            }
            return;
        }
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
                if ui.button("Import / edit queue").clicked() {
                    self.bulk_open = true;
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Search");
                ui.add(
                    egui::TextEdit::singleline(&mut self.bulk_search)
                        .hint_text("mailbox, host, or user")
                        .desired_width(220.0),
                );
                egui::ComboBox::from_id_salt("mailbox_state_filter")
                    .selected_text(match self.bulk_state_filter.as_str() {
                        "imported" => "Imported",
                        "attention" => "Attention",
                        "failed" => "Failed",
                        "delta_required" => "Delta required",
                        "verified" | "verified_with_exceptions" => "Verified",
                        "ready" => "Ready",
                        _ => "All states",
                    })
                    .show_ui(ui, |ui| {
                        for (value, label) in [
                            ("all", "All states"),
                            ("imported", "Imported"),
                            ("ready", "Ready"),
                            ("attention", "Attention"),
                            ("failed", "Failed"),
                            ("delta_required", "Delta required"),
                            ("verified", "Verified"),
                            ("verified_with_exceptions", "Verified with exceptions"),
                        ] {
                            ui.selectable_value(&mut self.bulk_state_filter, value.into(), label);
                        }
                    });
                if ui.button("Select visible").clicked() {
                    for (index, job) in self.bulk_jobs.iter().enumerate() {
                        if self.mailbox_matches_filter(job)
                            && let Some(id) = self.bulk_job_ids.get(index)
                        {
                            self.bulk_selected_ids.insert(id.clone());
                        }
                    }
                }
                if ui.button("Clear selection").clicked() {
                    self.bulk_selected_ids.clear();
                }
            });
            let visible_indices = self
                .bulk_jobs
                .iter()
                .enumerate()
                .filter(|(_, job)| self.mailbox_matches_filter(job))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            ui.label(
                RichText::new(format!(
                    "{} visible · {} selected",
                    visible_indices.len(),
                    self.bulk_selected_ids.len()
                ))
                .color(MUTED),
            );
            let has_selection = !self.bulk_selected_ids.is_empty();
            let mut run_preflight = false;
            let mut run_live = false;
            let mut run_delta = false;
            let mut review_selected = false;
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Selected mailbox actions").strong());
                if ui
                    .add_enabled(
                        has_selection && !self.running(),
                        egui::Button::new("Run preflight"),
                    )
                    .clicked()
                {
                    run_preflight = true;
                }
                if ui
                    .add_enabled(
                        has_selection && !self.running(),
                        egui::Button::new(
                            RichText::new("Run live migration").color(Color32::WHITE),
                        )
                        .fill(ALERT),
                    )
                    .clicked()
                {
                    run_live = true;
                }
                if ui
                    .add_enabled(
                        has_selection && !self.running(),
                        egui::Button::new("Run final delta"),
                    )
                    .clicked()
                {
                    run_delta = true;
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new("Review verification"))
                    .clicked()
                {
                    review_selected = true;
                }
                if !has_selection {
                    ui.label(
                        RichText::new("Select one or more rows to enable actions.").color(MUTED),
                    );
                }
            });
            if run_preflight {
                self.form.dry_run = true;
                self.bulk_dry_run = true;
                self.bulk_retry_scope = BulkRetryScope::All;
                self.start_bulk();
            } else if run_live {
                self.form.dry_run = false;
                self.bulk_dry_run = false;
                self.bulk_retry_scope = BulkRetryScope::All;
                self.start_bulk();
            } else if run_delta {
                self.form.dry_run = false;
                self.bulk_dry_run = false;
                self.bulk_retry_scope = BulkRetryScope::DeltaRequired;
                self.start_bulk();
            } else if review_selected
                && let Some(job_id) = self
                    .bulk_job_ids
                    .iter()
                    .find(|id| self.bulk_selected_ids.contains(*id))
            {
                self.job_id = Some(job_id.clone());
                self.active_view = WorkspaceView::Verification;
            }
            ui.add_space(8.0);
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::auto())
                .column(Column::remainder())
                .column(Column::remainder())
                .column(Column::remainder())
                .column(Column::auto())
                .column(Column::remainder())
                .header(32.0, |mut header| {
                    for label in [
                        "",
                        "Mailbox",
                        "Source",
                        "Destination",
                        "State",
                        "Operator action",
                    ] {
                        header.col(|ui| {
                            ui.strong(label);
                        });
                    }
                })
                .body(|body| {
                    body.rows(42.0, visible_indices.len(), |mut row| {
                        let index = visible_indices[row.index()];
                        let job = &self.bulk_jobs[index];
                        let Some(job_id) = self.bulk_job_ids.get(index) else {
                            return;
                        };
                        row.col(|ui| {
                            let mut selected = self.bulk_selected_ids.contains(job_id);
                            if ui.checkbox(&mut selected, "").changed() {
                                if selected {
                                    self.bulk_selected_ids.insert(job_id.clone());
                                } else {
                                    self.bulk_selected_ids.remove(job_id);
                                }
                            }
                        });
                        row.col(|ui| {
                            ui.label(&job.label);
                        });
                        row.col(|ui| {
                            ui.label(format!(
                                "{}\n{}",
                                job.form.profile.source_host, job.form.profile.source_user
                            ));
                        });
                        row.col(|ui| {
                            ui.label(format!(
                                "{}\n{}",
                                job.form.profile.destination_host,
                                job.form.profile.destination_user
                            ));
                        });
                        row.col(|ui| {
                            let (badge, color) = job_state_badge(&job.state, colors);
                            ui.label(RichText::new(badge).color(color));
                        });
                        row.col(|ui| {
                            if job.state == "attention" {
                                match self.store.mailbox_attention_reason(job_id) {
                                    Ok(Some(reason)) => {
                                        ui.label(
                                            RichText::new(reason.recommended_action()).color(MUTED),
                                        );
                                    }
                                    Ok(None) => {
                                        ui.label(
                                            RichText::new("Inspect durable run detail")
                                                .color(MUTED),
                                        );
                                    }
                                    Err(_) => {
                                        ui.label(
                                            RichText::new("Attention reason unavailable")
                                                .color(ALERT),
                                        );
                                    }
                                }
                            }
                        });
                    });
                });
            ui.label(RichText::new("When a selection is present, batch actions apply only to selected rows. With no selection, the chosen retry scope applies to all matching rows.").color(MUTED));
        }
    }

    fn mailbox_matches_filter(&self, job: &BulkJob) -> bool {
        let state = job.state.to_ascii_lowercase().replace(' ', "_");
        if !self.bulk_state_filter.is_empty()
            && self.bulk_state_filter != "all"
            && state != self.bulk_state_filter
            && !(self.bulk_state_filter == "delta_required" && state.contains("delta"))
            && !(self.bulk_state_filter == "verification_difference"
                && state.contains("verification"))
        {
            return false;
        }
        let search = self.bulk_search.trim().to_ascii_lowercase();
        search.is_empty()
            || [
                job.label.as_str(),
                job.form.profile.source_host.as_str(),
                job.form.profile.source_user.as_str(),
                job.form.profile.destination_host.as_str(),
                job.form.profile.destination_user.as_str(),
            ]
            .iter()
            .any(|value| value.to_ascii_lowercase().contains(&search))
    }

    fn bulk_row_is_selected(&self, index: usize) -> bool {
        self.bulk_selected_ids.is_empty()
            || self
                .bulk_job_ids
                .get(index)
                .is_some_and(|job_id| self.bulk_selected_ids.contains(job_id))
    }

    fn select_bulk_state_set(&mut self, set: BulkStateSet) {
        self.bulk_selected_ids = self
            .bulk_jobs
            .iter()
            .enumerate()
            .filter(|(_, job)| set.matches(&display_state_key(&job.state)))
            .filter_map(|(index, _)| self.bulk_job_ids.get(index).cloned())
            .collect();
        self.bulk_state_filter = "all".into();
        self.bulk_message = format!(
            "Selected {} mailbox row(s) for focused review.",
            self.bulk_selected_ids.len()
        );
    }

    fn export_bulk_selection(&self) -> Result<(), String> {
        if self.bulk_jobs.is_empty() {
            return Err("The batch queue has no mailbox rows to export.".into());
        }
        let value =
            bulk_selection_value(&self.bulk_jobs, &self.bulk_selected_ids, &self.bulk_job_ids);
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-batch-selection.json")
            .save_file()
            .ok_or("Batch selection export cancelled.")?;
        let report = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
        write_private_atomic(&path, &report).map_err(|error| error.to_string())
    }

    fn activity_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Activity");
        ui.label(RichText::new("Live output is retained here for operator review. Durable run history remains available after restart.").color(MUTED));
        ui.add_space(12.0);
        if !self.workspace_read_only
            && let Some(job) = self.job_id.as_deref()
        {
            let state = self.ui_report.as_ref().and_then(|report| {
                report
                    .mailboxes
                    .iter()
                    .find(|mailbox| mailbox.job.id == job)
                    .map(|mailbox| mailbox.job.state.clone())
            });
            match state.as_deref() {
                Some(state) if needs_operator_review(state) => {
                    if ui.button("Prepare safe retry  →").clicked() {
                        self.form.dry_run = true;
                        self.live_confirmed = false;
                        self.active_view = WorkspaceView::Plan;
                        self.status = "Retry prepared as a dry preflight. Review the exact plan before any live run.".into();
                    }
                }
                Some(_) | None => {}
            }
        }
        ui.group(|ui| {
            ui.horizontal(|ui| {
                let running = self.running();
                ui.heading(if running {
                    "Run in progress"
                } else {
                    "No active run"
                });
                ui.label(
                    RichText::new(&self.status)
                        .color(status_color(&self.status, self.theme_colors())),
                );
                if ui.button("Copy output").clicked() {
                    ui.ctx()
                        .copy_text(self.output.iter().cloned().collect::<Vec<_>>().join("\n"));
                }
                if running && ui.button("Stop migration").clicked() {
                    self.stop_confirm_open = true;
                }
            });
            egui::ScrollArea::vertical()
                .hscroll(true)
                .stick_to_bottom(true)
                .max_height(420.0)
                .show_rows(ui, 20.0, self.output.len(), |ui, rows| {
                    for index in rows {
                        if let Some(line) = self.output.get(index) {
                            ui.add(
                                egui::Label::new(RichText::new(line).monospace().size(14.0))
                                    .wrap_mode(egui::TextWrapMode::Extend),
                            );
                        }
                    }
                });
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.heading("Durable run history");
            let history_label = if self.activity_show_all {
                "Show recent 20"
            } else {
                "Show up to 250 runs"
            };
            if ui.button(history_label).clicked() {
                self.activity_show_all = !self.activity_show_all;
            }
        });
        let Some(_project) = self.active_project_id().map(str::to_owned) else {
            ui.label(
                RichText::new("Create or restore a project to see durable runs.").color(MUTED),
            );
            return;
        };
        let run_limit = if self.activity_show_all {
            MAX_ACTIVITY_HISTORY_ROWS
        } else {
            20
        };
        ui.horizontal_wrapped(|ui| {
            ui.label("Filter history");
            ui.add(
                egui::TextEdit::singleline(&mut self.activity_search)
                    .hint_text("mailbox, phase, engine, run ID, or detail")
                    .desired_width(280.0),
            );
            egui::ComboBox::from_id_salt("activity_status_filter")
                .selected_text(match self.activity_status_filter.as_str() {
                    "errors" => "Errors and attention",
                    "running" => "Running",
                    "completed" => "Completed",
                    _ => "All statuses",
                })
                .show_ui(ui, |ui| {
                    for (value, label) in [
                        ("all", "All statuses"),
                        ("errors", "Errors and attention"),
                        ("running", "Running"),
                        ("completed", "Completed"),
                    ] {
                        ui.selectable_value(&mut self.activity_status_filter, value.into(), label);
                    }
                });
        });
        let runs = self
            .ui_runs
            .iter()
            .take(run_limit as usize)
            .cloned()
            .collect::<Vec<_>>();
        match runs {
            runs if runs.is_empty() => {
                ui.label(RichText::new("No durable runs recorded yet.").color(MUTED));
            }
            runs => {
                let search = self.activity_search.trim().to_ascii_lowercase();
                let visible = runs
                    .iter()
                    .enumerate()
                    .filter(|(_, run)| {
                        let status_match = match self.activity_status_filter.as_str() {
                            "errors" => run.status != "completed" && run.status != "running",
                            "running" => run.status == "running",
                            "completed" => run.status == "completed",
                            _ => true,
                        };
                        let mailbox = run
                            .destination_mailbox
                            .as_deref()
                            .or(run.source_mailbox.as_deref())
                            .unwrap_or("batch");
                        let text_match = search.is_empty()
                            || [
                                run.id.as_str(),
                                mailbox,
                                run.phase_at_start.as_str(),
                                run.engine.as_str(),
                                run.status.as_str(),
                                run.detail.as_str(),
                            ]
                            .iter()
                            .any(|value| value.to_ascii_lowercase().contains(&search));
                        status_match && text_match
                    })
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>();
                ui.label(
                    RichText::new(format!(
                        "{} visible of {} loaded",
                        visible.len(),
                        runs.len()
                    ))
                    .color(MUTED),
                );
                if self.activity_show_all && runs.len() == MAX_ACTIVITY_HISTORY_ROWS as usize {
                    ui.label(
                        RichText::new("Showing the newest 250 runs. Export the audit report for complete history.")
                            .color(MUTED),
                    );
                }
                egui::Grid::new("durable_run_history")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("Run");
                        ui.strong("Mailbox");
                        ui.strong("Stage");
                        ui.strong("Engine");
                        ui.strong("Status");
                        ui.strong("Started");
                        ui.strong("Finished");
                        ui.strong("Detail");
                        ui.end_row();
                    });
                egui::ScrollArea::vertical()
                    .id_salt("durable_run_history_rows")
                    .max_height(420.0)
                    .show_rows(ui, 32.0, visible.len(), |ui, rows| {
                        egui::Grid::new("durable_run_history_rows_grid")
                            .striped(true)
                            .show(ui, |ui| {
                                for row in rows {
                                    let index = visible[row];
                                    let run = &runs[index];
                                    ui.label(
                                        RichText::new(&run.id[..8.min(run.id.len())]).monospace(),
                                    );
                                    ui.label(
                                        run.destination_mailbox
                                            .as_deref()
                                            .or(run.source_mailbox.as_deref())
                                            .unwrap_or("Batch")
                                            .to_owned(),
                                    );
                                    ui.label(&run.phase_at_start);
                                    ui.label(&run.engine);
                                    ui.label(RichText::new(&run.status).color(
                                        if run.status == "completed" {
                                            TEAL
                                        } else if run.status == "running" {
                                            BLUE
                                        } else {
                                            ALERT
                                        },
                                    ));
                                    ui.label(&run.started_at);
                                    ui.label(run.finished_at.as_deref().unwrap_or("in progress"));
                                    ui.label(if run.detail.is_empty() {
                                        "—"
                                    } else {
                                        &run.detail
                                    });
                                    ui.end_row();
                                }
                            });
                    });
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
            .ok_or("The mailbox job has no durable state; evidence export is blocked.")?;
        let project_id = self
            .store
            .project_id_for_mailbox(job)
            .map_err(|e| e.to_string())?
            .ok_or("The mailbox job no longer belongs to a project.")?;
        if self.active_project_id() != Some(project_id.as_str()) {
            return Err(
                "The selected project does not contain this mailbox; select its project before exporting mailbox evidence."
                    .into(),
            );
        }
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
        // An empty snapshot is the only legacy case that can safely use the
        // project/mailbox fallback. A non-empty but malformed snapshot must
        // fail closed; otherwise a report could silently describe the
        // current project instead of the plan that produced the evidence.
        let snapshot_profile =
            decode_report_run_snapshot(&run.plan_snapshot)?.map(|run| run.profile);
        let source_endpoint = snapshot_profile
            .as_ref()
            .map(|profile| profile.source_host.as_str())
            .unwrap_or(project.source_endpoint.as_str());
        let destination_endpoint = snapshot_profile
            .as_ref()
            .map(|profile| profile.destination_host.as_str())
            .unwrap_or(project.destination_endpoint.as_str());
        let source_identity = snapshot_profile
            .as_ref()
            .map(|profile| profile.source_user.as_str())
            .unwrap_or(source_mailbox.as_str());
        let destination_identity = snapshot_profile
            .as_ref()
            .map(|profile| profile.destination_user.as_str())
            .unwrap_or(destination_mailbox.as_str());
        let identity_note = if snapshot_profile.is_some() {
            "run snapshot"
        } else {
            "durable project/mailbox fallback (legacy run snapshot unavailable)"
        };
        let evidence_reference = evidence_digest(&run.id, &run.plan_snapshot, &evidence);
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-verification.md")
            .save_file()
            .ok_or("Report export cancelled.")?;
        let mut report = format!(
            "# MailSwiftSync verification report\n\n- Project: {}\n- Source endpoint: {}\n- Destination endpoint: {}\n- Source mailbox: {}\n- Destination mailbox: {}\n- Identity source: {}\n- Engine: {}\n- Run ID: `{}`\n- Run status: `{}`\n- Started: `{}`\n- Finished: `{}`\n- Mailbox state: `{}`\n- Evidence level: `{}`\n- Evidence source: `{}`\n- Evidence digest: `{}`\n\n## Execution plan snapshot\n\nThe snapshot excludes session passwords and raw extra-option values. It retains an SHA-256 digest for expert-option identity without copying those values into the ledger or report.\n\n```toml\n{}\n```\n\n| Metric | Source | Destination |\n|---|---:|---:|\n| Folders | {} | {} |\n| Messages | {} | {} |\n| Virtual size | {} | {} |\n| Unmatched messages | {} | — |\n| Failed messages | {} | — |\n\nThis report distinguishes engine-confirmed output from aggregate reconciliation. Neither is independent message-level proof; provider-specific warnings and deeper verification require additional review.",
            markdown_escape(&project.name),
            markdown_escape(source_endpoint),
            markdown_escape(destination_endpoint),
            markdown_escape(source_identity),
            markdown_escape(destination_identity),
            identity_note,
            markdown_escape(&run.engine),
            run.id,
            run.status,
            run.started_at,
            run.finished_at.as_deref().unwrap_or("in progress"),
            state,
            evidence.evidence_level(),
            match evidence.evidence_scope() {
                core::EvidenceScope::EngineConfirmed => "engine-confirmed summary",
                core::EvidenceScope::AggregateReconciled => "aggregate mailbox totals",
            },
            evidence_reference,
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
        if let Some(index) = report.find("- Run ID:") {
            report.insert_str(
                index,
                &format!(
                    "- Project phase at run start: `{}`\n",
                    markdown_escape(&run.phase_at_start)
                ),
            );
        }
        write_private_atomic(&path, &report).map_err(|e| e.to_string())
    }

    fn export_project_report(&self) -> Result<(), String> {
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        let snapshot = self
            .store
            .project_report_snapshot(project_id)
            .map_err(|e| e.to_string())?
            .ok_or("The durable migration project no longer exists.")?;
        if snapshot.mailboxes.is_empty() {
            return Err("The project has no mailbox jobs to report.".into());
        }
        let project = snapshot.project.clone();
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-project-report.md")
            .save_file()
            .ok_or("Report export cancelled.")?;
        let verified = snapshot
            .mailboxes
            .iter()
            .filter(|mailbox| {
                matches!(
                    mailbox.job.state.as_str(),
                    "verified" | "verified_with_exceptions"
                )
            })
            .count();
        let attention = snapshot
            .mailboxes
            .iter()
            .filter(|mailbox| needs_operator_review(&mailbox.job.state))
            .count();
        let mut report = format!(
            "# MailSwiftSync project report\n\n- Project: {}\n- Project ID: `{}`\n- Source endpoint: {}\n- Destination endpoint: {}\n- Phase: `{:?}`\n- Mailboxes: {}\n- Verified: {}\n- Attention required: {}\n\n## Mailbox results\n\n| Source mailbox | Destination mailbox | State | Attention reason | Recommended action | Exception acceptance | Evidence run | Evidence | Evidence digest | Source messages | Destination messages | Unmatched | Failed |\n|---|---|---|---|---|---|---|---|---|---:|---:|---:|---:|\n",
            markdown_escape(&project.name),
            project.id,
            markdown_escape(&project.source_endpoint),
            markdown_escape(&project.destination_endpoint),
            project.phase,
            snapshot.mailboxes.len(),
            verified,
            attention,
        );
        for mailbox in snapshot.mailboxes {
            let job = mailbox.job;
            let attention_reason = mailbox.attention_reason;
            let acceptance = mailbox.acceptance;
            let acceptance_summary = acceptance
                .as_ref()
                .map(|value| format!("{}: {}", value.operator, value.reason))
                .unwrap_or_else(|| "—".into());
            let reason_label = attention_reason.map(|reason| reason.label()).unwrap_or("—");
            let recommended_action = attention_reason
                .map(|reason| reason.recommended_action())
                .unwrap_or("—");
            if let Some((evidence_run_id, evidence, plan_snapshot)) = mailbox.evidence {
                let plan_snapshot = plan_snapshot.ok_or("The evidence run no longer exists.")?;
                report.push_str(&format!(
                    "| {} | {} | `{}` | {} | {} | {} | `{}` | {} | `{}` | {} | {} | {} | {} |\n",
                    markdown_escape(&job.source_mailbox),
                    markdown_escape(&job.destination_mailbox),
                    job.state,
                    markdown_escape(reason_label),
                    markdown_escape(recommended_action),
                    markdown_escape(&acceptance_summary),
                    evidence_run_id,
                    evidence.evidence_level(),
                    evidence_digest(&evidence_run_id, &plan_snapshot, &evidence,),
                    evidence.source_messages,
                    evidence.destination_messages,
                    evidence.unmatched_messages,
                    evidence.failed_messages,
                ));
            } else {
                report.push_str(&format!(
                    "| {} | {} | `{}` | {} | {} | {} | — | missing | — | — | — | — | — |\n",
                    markdown_escape(&job.source_mailbox),
                    markdown_escape(&job.destination_mailbox),
                    job.state,
                    markdown_escape(reason_label),
                    markdown_escape(recommended_action),
                    markdown_escape(&acceptance_summary),
                ));
            }
        }
        report.push_str("\n## Recent runs\n\n| Run | Engine | Engine version | Phase at start | Status | Plan reference | Started | Finished | Detail |\n|---|---|---|---|---|---|---|---|---|\n");
        for report_run in snapshot.runs.iter().rev().take(20) {
            let run = &report_run.run;
            let engine_version = report_run
                .engine_version
                .clone()
                .unwrap_or_else(|| "unavailable".into());
            report.push_str(&format!(
                "| `{}` | {} | {} | `{}` | `{}` | `{}` | {} | {} | {} |\n",
                run.id,
                markdown_escape(&run.engine),
                markdown_escape(&engine_version),
                markdown_escape(&run.phase_at_start),
                run.status,
                plan_snapshot_sha256(&run.plan_snapshot),
                run.started_at,
                run.finished_at
                    .clone()
                    .unwrap_or_else(|| "in progress".into()),
                markdown_escape(if run.detail.is_empty() {
                    "—"
                } else {
                    &run.detail
                }),
            ));
        }
        report.push_str("\nEvidence levels describe what was actually established. Engine-confirmed output is not independent message-level reconciliation, and aggregate totals are not proof of message identity. Missing evidence or any state other than `verified` or `verified_with_exceptions` requires operator review before declaring the project complete.\n");
        write_private_atomic(&path, &report).map_err(|e| e.to_string())
    }

    fn export_project_json(&self) -> Result<(), String> {
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        let snapshot = self
            .store
            .project_report_snapshot(project_id)
            .map_err(|e| e.to_string())?
            .ok_or("The durable migration project no longer exists.")?;
        if snapshot.mailboxes.is_empty() {
            return Err("The project has no mailbox jobs to report.".into());
        }
        let project = snapshot.project;
        let mailboxes = snapshot
            .mailboxes
            .into_iter()
            .map(|mailbox| {
                let job = mailbox.job;
                let attention_reason = mailbox.attention_reason;
                match mailbox.evidence {
                    Some((evidence_run_id, evidence, plan_snapshot)) => {
                        let plan_snapshot = plan_snapshot.ok_or("The evidence run no longer exists.")?;
                        let digest = evidence_digest(
                            &evidence_run_id,
                            &plan_snapshot,
                            &evidence,
                        );
                        Ok(serde_json::json!({
                            "id": job.id,
                            "source_mailbox": job.source_mailbox,
                            "destination_mailbox": job.destination_mailbox,
                            "state": job.state,
                            "attention_reason": attention_reason.map(|reason| reason.as_str()),
                            "recommended_action": attention_reason.map(|reason| reason.recommended_action()),
                            "verification_acceptance": mailbox.acceptance,
                            "evidence": {
                                "run_id": evidence_run_id,
                                "scope": evidence.evidence_scope().label(),
                                "evidence_level": evidence.evidence_level(),
                                "authoritative": evidence.authoritative,
                                "evidence_digest": digest,
                                "source_folders": evidence.source_folders,
                                "destination_folders": evidence.destination_folders,
                                "source_messages": evidence.source_messages,
                                "destination_messages": evidence.destination_messages,
                                "source_bytes": evidence.source_bytes,
                                "destination_bytes": evidence.destination_bytes,
                                "unmatched_messages": evidence.unmatched_messages,
                                "failed_messages": evidence.failed_messages,
                            }
                        }))
                    }
                    None => Ok(serde_json::json!({
                        "id": job.id,
                        "source_mailbox": job.source_mailbox,
                        "destination_mailbox": job.destination_mailbox,
                        "state": job.state,
                        "attention_reason": attention_reason.map(|reason| reason.as_str()),
                        "recommended_action": attention_reason.map(|reason| reason.recommended_action()),
                        "verification_acceptance": mailbox.acceptance,
                        "evidence": null
                    })),
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        let run_values = snapshot
            .runs
            .into_iter()
            .map(|run| {
                let value = run.run;
                Ok(serde_json::json!({
                    "id": value.id,
                    "job_id": value.job_id,
                    "parent_run_id": value.parent_run_id,
                    "engine": value.engine,
                    "engine_version": run.engine_version,
                    "phase_at_start": value.phase_at_start,
                    "plan_snapshot_sha256": plan_snapshot_sha256(&value.plan_snapshot),
                    "status": value.status,
                    "started_at": value.started_at,
                    "finished_at": value.finished_at,
                    "detail": value.detail,
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let value = serde_json::json!({
            "format": "mailswiftsync-project-report",
            "format_version": 2,
            "application_version": env!("CARGO_PKG_VERSION"),
            "run_manifest_complete": true,
            "project": {
                "id": project.id,
                "name": project.name,
                "source_endpoint": project.source_endpoint,
                "destination_endpoint": project.destination_endpoint,
                "phase": format!("{:?}", project.phase),
            },
            "mailboxes": mailboxes,
            "runs": run_values,
            "note": "The run manifest contains every durable run for this project. Aggregate evidence is not message-level reconciliation; unresolved or missing evidence requires operator review."
        });
        let value = with_proof_digest(value)?;
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-project-report.json")
            .save_file()
            .ok_or("Report export cancelled.")?;
        let report = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        write_private_atomic(&path, &report).map_err(|e| e.to_string())
    }

    /// Export the customer-safe proof artifact. Unlike the operator JSON
    /// report, this intentionally omits project IDs, endpoints, plan
    /// snapshots, credential references, executable paths, and diagnostic
    /// details that may reveal internal topology. It retains every run's
    /// execution metadata and each mailbox's evidence digest so a customer
    /// or change record can verify what was established without receiving the
    /// forensic report.
    fn export_customer_proof(&self) -> Result<(), String> {
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-customer-proof.json")
            .save_file()
            .ok_or("Customer proof export cancelled.")?;
        self.export_customer_proof_to(&path)
    }

    fn export_customer_proof_to(&self, path: &std::path::Path) -> Result<(), String> {
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        reports::customer::export_from_store(&self.store, project_id, path)
    }

    fn export_support_bundle_dialog(&self) -> Result<(), String> {
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-support-bundle.json")
            .save_file()
            .ok_or("Support-bundle export cancelled.")?;
        let state_path = persistent_state_path()?;
        export_support_bundle(&state_path, &path)
    }

    fn export_project_health(&self) -> Result<(), String> {
        let project_id = self
            .active_project_id()
            .ok_or("No durable migration project is available yet.")?;
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-project-health.json")
            .save_file()
            .ok_or("Health export cancelled.")?;
        reports::operator::export_health(&self.store, project_id, &path)
    }

    fn verification_view(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        ui.heading("Verification");
        ui.label(RichText::new("Do not trust a completed process until the destination reconciles with the source.").color(MUTED));
        ui.add_space(12.0);
        ui.group(|ui| {
            ui.heading("Verification and audit report");
            ui.label(RichText::new("The transfer engine is only one part of the migration. This report is the operator-facing proof of what arrived and what still needs attention.").color(MUTED));
            if self.active_project_id().is_some() {
                if ui.button("Export project report…").clicked() {
                    let result = self.export_project_report();
                    self.report_export_result("Project report", result);
                }
                if ui.button("Export project JSON…").clicked() {
                    let result = self.export_project_json();
                    self.report_export_result("Project JSON", result);
                }
                if ui.button("Export customer proof JSON…").clicked() {
                    let result = self.export_customer_proof();
                    self.report_export_result("Customer proof", result);
                }
                if ui.button("Export support bundle…").clicked() {
                    let result = self.export_support_bundle_dialog();
                    self.report_export_result("Support bundle", result);
                }
                if ui.button("Export project health…").clicked() {
                    let result = self.export_project_health();
                    self.report_export_result("Project health export", result);
                }
            }
            let selected_project = self.active_project_id().map(str::to_owned);
            if selected_project.is_some() {
                match self.ui_report.clone() {
                    Some(snapshot) => {
                        let verified = snapshot
                            .mailboxes
                            .iter()
                            .filter(|mailbox| matches!(mailbox.job.state.as_str(), "verified" | "verified_with_exceptions"))
                            .count();
                        let review = snapshot
                            .mailboxes
                            .iter()
                            .filter(|mailbox| needs_operator_review(&mailbox.job.state))
                            .count();
                        ui.separator();
                        ui.heading("Mailbox evidence");
                        ui.label(format!(
                            "{verified} of {} verified · {review} require review",
                            snapshot.mailboxes.len()
                        ));
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Search");
                            ui.add(egui::TextEdit::singleline(&mut self.verification_search)
                                .hint_text("mailbox or destination")
                                .desired_width(220.0));
                            egui::ComboBox::from_id_salt("verification_result_filter")
                                .selected_text(match self.verification_filter.as_str() {
                                    "review" => "Needs review",
                                    "verified" => "Verified",
                                    "difference" => "Differences",
                                    _ => "All results",
                                })
                                .show_ui(ui, |ui| {
                                    for (value, label) in [("all", "All results"), ("review", "Needs review"), ("verified", "Verified"), ("difference", "Differences")] {
                                        ui.selectable_value(&mut self.verification_filter, value.into(), label);
                                    }
                                });
                        });
                        let search = self.verification_search.trim().to_ascii_lowercase();
                        let visible = snapshot
                            .mailboxes
                            .iter()
                            .enumerate()
                            .filter(|(_, mailbox)| {
                                let state = mailbox.job.state.as_str();
                                let result_match = match self.verification_filter.as_str() {
                                    "review" => needs_operator_review(state),
                                    "verified" => matches!(state, "verified" | "verified_with_exceptions"),
                                    "difference" => state == "verification_difference",
                                    _ => true,
                                };
                                let text_match = search.is_empty()
                                    || mailbox.job.source_mailbox.to_ascii_lowercase().contains(&search)
                                    || mailbox.job.destination_mailbox.to_ascii_lowercase().contains(&search);
                                result_match && text_match
                            })
                            .map(|(index, _)| index)
                            .collect::<Vec<_>>();
                        ui.label(RichText::new(format!("{} visible", visible.len())).color(MUTED));
                        egui::ScrollArea::vertical()
                            .id_salt("verification_mailbox_list")
                            .max_height(360.0)
                            .show_rows(ui, 32.0, visible.len(), |ui, rows| {
                                egui::Grid::new("verification_mailboxes")
                                    .striped(true)
                                    .min_col_width(140.0)
                                    .show(ui, |ui| {
                                        if rows.start == 0 {
                                            ui.strong("Mailbox");
                                            ui.strong("Evidence");
                                            ui.strong("Result");
                                            ui.end_row();
                                        }
                                        for row in rows {
                                            let mailbox = &snapshot.mailboxes[visible[row]];
                                            let evidence_label = mailbox
                                                .evidence
                                                .as_ref()
                                                .map(|(_, evidence, _)| evidence.evidence_level())
                                                .unwrap_or("No evidence");
                                            let (badge, color) = job_state_badge(&mailbox.job.state, colors);
                                            if ui
                                                .selectable_label(
                                                    self.job_id.as_deref() == Some(mailbox.job.id.as_str()),
                                                    &mailbox.job.destination_mailbox,
                                                )
                                                .clicked()
                                            {
                                                self.job_id = Some(mailbox.job.id.clone());
                                            }
                                            ui.label(evidence_label);
                                            ui.label(RichText::new(badge).color(color));
                                            ui.end_row();
                                        }
                                    });
                            });
                    }
                    None => {
                        ui.separator();
                        ui.label(RichText::new("The selected project no longer exists.").color(ALERT));
                    }
                }
            }
            let mut selection_error = None;
            let selected_job = self.job_id.clone().filter(|job| {
                match self.store.project_id_for_mailbox(job) {
                    Ok(project_id) => project_id == selected_project,
                    Err(error) => {
                        selection_error = Some(error.to_string());
                        false
                    }
                }
            });
            if let Some(job) = selected_job.as_deref() {
                match self.store.evidence(job) {
                    Ok(Some(evidence)) => {
                        ui.label("Durable mailbox reconciliation");
                        if ui.button("Export verification report…").clicked() {
                            let result = self.export_verification_report();
                            self.report_export_result("Verification report", result);
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
                match self.store.mailbox_state(job) {
                    Ok(Some(state)) if state == "verification_difference" => {
                        ui.separator();
                        ui.heading("Accept residual difference");
                        ui.label(RichText::new("This records an auditable exception; it does not change the underlying evidence or claim exact equality.").color(MUTED));
                        ui.horizontal(|ui| {
                            ui.label("Operator");
                            ui.add(egui::TextEdit::singleline(&mut self.verification_exception_operator).desired_width(220.0));
                        });
                        ui.add(egui::TextEdit::multiline(&mut self.verification_exception_reason)
                            .hint_text("Why is this difference acceptable? Include the change-ticket or customer approval reference.")
                            .desired_rows(3));
                        let can_accept = !self.verification_exception_operator.trim().is_empty()
                            && !self.verification_exception_reason.trim().is_empty();
                        if ui.add_enabled(can_accept, egui::Button::new("Accept and mark verified with exceptions")).clicked() {
                            match selected_project.as_deref() {
                                Some(project_id) => match self.store.accept_verification_difference(
                                    project_id,
                                    job,
                                    &self.verification_exception_operator,
                                    &self.verification_exception_reason,
                                ) {
                                    Ok(()) => {
                                        self.status = "Verification exception recorded durably".into();
                                        self.verification_exception_reason.clear();
                                    }
                                    Err(error) => self.status = format!("Could not accept verification exception: {error}"),
                                },
                                None => self.status = "No active project selected".into(),
                            }
                        }
                    }
                    Ok(Some(state)) if state == "verified_with_exceptions" => {
                        if let Ok(Some(acceptance)) = self.store.latest_verification_acceptance(job) {
                            ui.separator();
                            ui.label(RichText::new("Verified with exceptions").strong().color(Color32::from_rgb(218, 148, 48)));
                            ui.label(format!("Accepted by {} at {}: {}", acceptance.operator, acceptance.accepted_at, acceptance.reason));
                        }
                    }
                    Ok(Some(_)) | Ok(None) => {}
                    Err(error) => {
                        ui.label(RichText::new(format!("Could not read durable mailbox state: {error}")).color(ALERT));
                    }
                }
            } else if let Some(error) = selection_error {
                ui.label(
                    RichText::new(format!(
                        "The selected mailbox cannot be resolved in durable state: {error}"
                    ))
                    .color(ALERT),
                );
            } else if self.job_id.is_some() && selected_project.is_some() {
                ui.label(
                    RichText::new(
                        "The retained single-mailbox selection belongs to another project. Select a mailbox from the current project before viewing or exporting its evidence.",
                    )
                    .color(ALERT),
                );
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
            // Import only proves that the row is structurally valid. It has
            // not authenticated either endpoint or established a durable
            // preflight plan, so it must not present as live-ready.
            state: "imported".into(),
        })
    }
    fn apply_bulk_import_result(&mut self, result: Result<Vec<BulkJob>, String>) {
        match result {
            Ok(jobs) => {
                self.bulk_message = format!(
                    "Imported {} mailbox rows. Review them and run preflight before migration.",
                    jobs.len()
                );
                // A new file is a new durable batch scope. Never let a queue
                // replacement reuse the project/job IDs from an older file.
                if self.selected_project_id == self.bulk_project_id {
                    self.selected_project_id = None;
                }
                self.bulk_project_id = None;
                self.bulk_job_ids.clear();
                self.bulk_job_index_by_id.clear();
                self.bulk_retry_scope = BulkRetryScope::default();
                self.bulk_selected_ids.clear();
                self.bulk_preflight_credential_fingerprints = vec![None; jobs.len()];
                self.bulk_jobs = jobs;
            }
            Err(e) => self.bulk_message = e,
        }
    }

    fn begin_bulk_import(&mut self, path: std::path::PathBuf) {
        if self.bulk_import_receiver.is_some() {
            self.bulk_message = "A mailbox file is already being imported.".into();
            return;
        }
        let base = self.form.clone();
        let (sender, receiver) = mpsc::channel();
        self.bulk_message = format!(
            "Importing {} in the background…",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("mailbox file")
        );
        self.bulk_import_receiver = Some(receiver);
        thread::spawn(move || {
            let ext = path
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let result = if ext == "csv" {
                App::read_csv(&path, &base)
            } else if ext == "xls" || ext == "xlsx" {
                App::read_sheet(&path, &base)
            } else {
                Err("Choose a .csv, .xls, or .xlsx file.".into())
            };
            let _ = sender.send(result);
        });
    }

    fn import_bulk(&mut self, path: &std::path::Path) {
        self.begin_bulk_import(path.to_owned());
    }

    fn request_bulk_import(&mut self, path: std::path::PathBuf) {
        if self.bulk_jobs.is_empty() {
            self.import_bulk(&path);
        } else {
            self.pending_bulk_import = Some(path);
        }
    }

    fn clear_bulk_queue(&mut self) {
        self.bulk_jobs.clear();
        self.bulk_selected_ids.clear();
        if self.selected_project_id == self.bulk_project_id {
            self.selected_project_id = None;
        }
        self.bulk_project_id = None;
        self.bulk_job_ids.clear();
        self.bulk_job_index_by_id.clear();
        self.bulk_retry_scope = BulkRetryScope::default();
        self.bulk_preflight_credential_fingerprints.clear();
        self.bulk_message = "Queue cleared; its durable batch association was discarded.".into();
    }

    fn rebuild_bulk_job_index(&mut self) {
        self.bulk_job_index_by_id = self
            .bulk_job_ids
            .iter()
            .enumerate()
            .map(|(index, job_id)| (job_id.clone(), index))
            .collect();
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
        validate_bulk_import_file(path)?;
        let mut reader = csv::Reader::from_path(path).map_err(|e| e.to_string())?;
        let headers = reader
            .headers()
            .map_err(|e| e.to_string())?
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        Self::validate_headers(&headers, base)?;
        if headers.len() > MAX_BULK_IMPORT_COLUMNS {
            return Err(format!(
                "The file has too many columns; the limit is {MAX_BULK_IMPORT_COLUMNS}."
            ));
        }
        let mut jobs = Vec::new();
        for (index, record) in reader.records().enumerate() {
            if index >= MAX_BULK_IMPORT_ROWS {
                return Err(format!(
                    "The file exceeds the {MAX_BULK_IMPORT_ROWS}-row import limit."
                ));
            }
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
                .map(|(h, v)| {
                    if v.len() > MAX_BULK_IMPORT_CELL_BYTES {
                        return Err(format!(
                            "Row {} contains a cell larger than {} bytes.",
                            index + 2,
                            MAX_BULK_IMPORT_CELL_BYTES
                        ));
                    }
                    Ok((h.clone(), v.to_owned()))
                })
                .collect::<Result<HashMap<_, _>, String>>()?;
            jobs.push(Self::job_from_values(&values, base, index + 2)?);
        }
        if jobs.is_empty() {
            return Err("The file has no migration rows.".into());
        }
        Ok(jobs)
    }
    fn read_sheet(path: &std::path::Path, base: &Form) -> Result<Vec<BulkJob>, String> {
        validate_bulk_import_file(path)?;
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
        if headers.len() > MAX_BULK_IMPORT_COLUMNS {
            return Err(format!(
                "The worksheet has too many columns; the limit is {MAX_BULK_IMPORT_COLUMNS}."
            ));
        }
        Self::validate_headers(&headers, base)?;
        let mut jobs = Vec::new();
        for (index, row) in rows.enumerate() {
            if index >= MAX_BULK_IMPORT_ROWS {
                return Err(format!(
                    "The worksheet exceeds the {MAX_BULK_IMPORT_ROWS}-row import limit."
                ));
            }
            if row.iter().all(|cell| cell.to_string().trim().is_empty()) {
                continue;
            }
            if row.len() != headers.len() {
                return Err(format!(
                    "Row {} has {} values but the header has {} columns.",
                    index + 2,
                    row.len(),
                    headers.len()
                ));
            }
            let values = headers
                .iter()
                .zip(row.iter())
                .map(|(h, v)| {
                    let value = v.to_string();
                    if value.len() > MAX_BULK_IMPORT_CELL_BYTES {
                        return Err(format!(
                            "Row {} contains a cell larger than {} bytes.",
                            index + 2,
                            MAX_BULK_IMPORT_CELL_BYTES
                        ));
                    }
                    Ok((h.clone(), value))
                })
                .collect::<Result<HashMap<_, _>, String>>()?;
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
        if !self.profile_available {
            self.bulk_message =
                "Batch execution is blocked because the saved migration profile is unavailable; repair it before starting a queue."
                    .into();
            return;
        }
        if self.workspace_read_only {
            self.bulk_message =
                "This project is being viewed read-only. Start a new migration to execute a batch."
                    .into();
            return;
        }
        if self.process_review_required {
            self.bulk_message = "Execution is blocked until you confirm that no unverified migration process remains on this host.".into();
            return;
        }
        if self.bulk_jobs.is_empty() {
            self.bulk_message = "Import a file before starting the queue.".into();
            return;
        }
        let live = !self.bulk_dry_run;
        if live && !self.bulk_live_confirmed {
            self.bulk_live_confirm_open = true;
            self.bulk_confirmation_summary = None;
            return;
        }
        if live {
            self.bulk_live_confirmed = false;
            if self.bulk_project_id.is_none() || self.bulk_job_ids.len() != self.bulk_jobs.len() {
                self.bulk_message =
                    "Run a successful preflight for this queue before starting live migrations."
                        .into();
                return;
            }
        }
        if !self.persistence_available {
            self.bulk_message = "Batch execution requires durable SQLite storage.".into();
            return;
        }
        let durable_admissions = if self.bulk_project_id.is_some()
            && self.bulk_job_ids.len() == self.bulk_jobs.len()
        {
            let Some(project_id) = self.bulk_project_id.as_deref() else {
                self.bulk_message =
                    "Run a successful preflight for this queue before starting live migrations."
                        .into();
                return;
            };
            match self
                .store
                .batch_admission_states(project_id, &self.bulk_job_ids)
            {
                Ok(states) => states.into_iter().map(Some).collect::<Vec<_>>(),
                Err(error) => {
                    self.bulk_message = format!(
                        "Could not read durable batch admission state; batch was not started: {error}"
                    );
                    return;
                }
            }
        } else {
            vec![None; self.bulk_jobs.len()]
        };
        let durable_states = durable_admissions
            .iter()
            .map(|admission| admission.as_ref().map(|value| value.state.clone()))
            .collect::<Vec<_>>();
        let selected_indices = self
            .bulk_jobs
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                let selected = self.bulk_selected_ids.is_empty()
                    || self
                        .bulk_job_ids
                        .get(*index)
                        .is_some_and(|id| self.bulk_selected_ids.contains(id));
                selected
                    && durable_states
                        .get(*index)
                        .and_then(Option::as_deref)
                        .is_none_or(|state| self.bulk_retry_scope.includes(state))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if selected_indices.is_empty() {
            self.bulk_message = format!(
                "No mailboxes match the selected live retry scope: {}.",
                self.bulk_retry_scope.label()
            );
            return;
        }
        if live {
            for &index in &selected_indices {
                let job = &self.bulk_jobs[index];
                let state = durable_states[index].as_deref();
                let preflight = durable_admissions
                    .get(index)
                    .and_then(|admission| admission.as_ref())
                    .and_then(|admission| admission.preflight_plan.as_deref());
                if !matches!(
                    state,
                    Some(
                        "ready"
                            | "delta_required"
                            | "verification_difference"
                            | "failed"
                            | "attention"
                            | "cancelled"
                            | "completed"
                            | "verified"
                            | "verified_with_exceptions"
                    )
                ) || preflight
                    != Some(plan_fingerprint_digest(&job.form.plan_fingerprint()).as_str())
                {
                    self.bulk_message = format!(
                        "Mailbox {} is not ready for live execution. Re-run dry validation after reviewing its exact plan.",
                        index + 1
                    );
                    return;
                }
            }
        }
        let jobs = selected_indices
            .iter()
            .map(|&index| {
                let mut job = self.bulk_jobs[index].clone();
                job.form.dry_run = !live;
                job
            })
            .collect::<Vec<_>>();
        let mut jobs = jobs;
        for (selected_index, job) in jobs.iter_mut().enumerate() {
            let credential_load = if live {
                job.form.reload_configured_keyring_credentials()
            } else {
                job.form.load_configured_keyring_credentials()
            };
            if let Err(error) = credential_load {
                self.bulk_message = format!(
                    "Could not load credentials for mailbox {} (queue row {}): {error}",
                    selected_index + 1,
                    selected_indices[selected_index] + 1
                );
                return;
            }
            if live {
                let queue_index = selected_indices[selected_index];
                let current = job.form.credential_fingerprint();
                let expected = self
                    .bulk_preflight_credential_fingerprints
                    .get(queue_index)
                    .and_then(Option::as_deref);
                if expected != Some(current.as_str()) {
                    self.bulk_message = format!(
                        "Mailbox {} credentials changed or were not retained from dry validation. Run a new dry validation before live execution.",
                        queue_index + 1
                    );
                    return;
                }
            }
        }
        if let Some((selected_index, error)) = jobs
            .iter()
            .enumerate()
            .find_map(|(index, job)| job.form.validate().err().map(|error| (index, error)))
        {
            self.bulk_message = format!(
                "Mailbox {} is not ready for validation: {error}",
                selected_indices[selected_index] + 1
            );
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
        if live {
            match duplicate_bulk_destination(&jobs) {
                Ok(Some(error)) => {
                    self.bulk_message = error;
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    self.bulk_message = format!(
                        "Live batch blocked because a destination endpoint is invalid: {error}"
                    );
                    return;
                }
            }
        }
        let concurrency = self.form.profile.batch_concurrency.clamp(1, 16);
        if let Err(error) = validate_batch_throttle(&self.form.profile, concurrency) {
            self.bulk_message = error;
            return;
        }
        self.durability_error = false;
        self.durability_recovery_pending = false;
        self.pending_batch_evidence.clear();
        self.pending_batch_checkpoints.clear();
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
        let reusable_project = if let Some(project_id) = self.bulk_project_id.clone() {
            match self.store.mailboxes(&project_id) {
                Ok(stored) if durable_batch_matches_queue(&stored, &mailboxes) => Some(project_id),
                Ok(_) => None,
                Err(error) => {
                    self.bulk_message = format!(
                        "Could not inspect the existing durable batch; no new batch was created: {error}"
                    );
                    return;
                }
            }
        } else {
            None
        };
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
        self.selected_project_id = Some(project_id.clone());
        self.bulk_job_ids = job_ids;
        self.rebuild_bulk_job_index();
        // Resolve queue-row indices to the durable IDs of the project chosen
        // above. A freshly imported dry queue has no old IDs at all, and an
        // edited queue may have IDs from a different project; using those
        // pre-resolution IDs would panic or bind children to stale mailboxes.
        let selected_job_ids = selected_indices
            .iter()
            .map(|&index| self.bulk_job_ids[index].clone())
            .collect::<Vec<_>>();
        let queue_checkpoints = selected_indices
            .iter()
            .map(|index| {
                if live && self.bulk_jobs[*index].form.engine() == core::Engine::Dovecot {
                    durable_admissions
                        .get(*index)
                        .and_then(|admission| admission.as_ref())
                        .and_then(|admission| admission.checkpoint.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        self.bulk_live_run = live;
        let expected_plans = if live {
            jobs.iter()
                .map(|job| plan_fingerprint_digest(&job.form.plan_fingerprint()))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let batch_plan_fingerprints = jobs
            .iter()
            .map(|job| plan_fingerprint_digest(&job.form.plan_fingerprint()))
            .collect::<Vec<_>>();
        let plan_snapshot = jobs
            .iter()
            .zip(queue_checkpoints.iter())
            .map(|(job, checkpoint)| {
                job.form
                    .plan_snapshot_with_checkpoint(checkpoint.as_deref())
            })
            .collect::<Result<Vec<_>, _>>();
        let plan_snapshot = match plan_snapshot {
            Ok(snapshots) => snapshots.join("\n--- batch mailbox plan ---\n"),
            Err(error) => {
                self.bulk_message = error;
                return;
            }
        };
        let run_id = uuid::Uuid::new_v4().to_string();
        self.run_id = Some(run_id.clone());
        // Version metadata is a property of the executable, not the mailbox.
        // Probe each distinct path once so a large batch does not synchronously
        // spawn one --version process per row on the UI thread.
        let mut engine_versions = HashMap::<String, Option<String>>::new();
        let child_plans = jobs
            .iter()
            .zip(queue_checkpoints.iter())
            .map(|(job, checkpoint)| {
                let executable = match job.form.engine() {
                    core::Engine::Dovecot => &job.form.profile.doveadm_path,
                    core::Engine::Auto | core::Engine::ImapSync => &job.form.profile.imapsync_path,
                };
                let engine_version = engine_versions
                    .entry(executable.clone())
                    .or_insert_with(|| probe_engine_version(executable))
                    .clone();
                job.form
                    .plan_snapshot_with_checkpoint(checkpoint.as_deref())
                    .map(|plan_snapshot| core::BatchChildPlan {
                        engine: job.form.engine().label().to_owned(),
                        plan_snapshot,
                        engine_version,
                    })
            })
            .collect::<Result<Vec<_>, _>>();
        let child_plans = match child_plans {
            Ok(plans) => plans,
            Err(error) => {
                self.bulk_message = error;
                return;
            }
        };
        let child_run_ids = match self.store.begin_batch_run_with_children(
            &project_id,
            &selected_job_ids,
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
        self.locked_profile = Some(self.form.profile.clone());
        self.locked_dry_run = Some(self.form.dry_run);
        self.active_run = Some(ActiveRunContext {
            run_id: run_id.clone(),
            project_id: project_id.clone(),
            job_id: None,
            batch_job_ids: selected_job_ids.clone(),
            batch_child_indices: child_run_ids
                .iter()
                .enumerate()
                .map(|(index, id)| (id.clone(), index))
                .collect(),
            batch_child_run_ids: child_run_ids,
            batch_plan_fingerprints,
            kind: RunKind::Batch,
            dry_run: !live,
            engine: self.form.engine(),
            plan_fingerprint: String::new(),
            credential_fingerprint: String::new(),
        });
        for &index in &selected_indices {
            if let Some(job) = self.bulk_jobs.get_mut(index) {
                job.state = "Queued".into();
            }
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
        let queue_job_ids = selected_job_ids;
        let child_run_ids = self
            .active_run
            .as_ref()
            .map(|run| run.batch_child_run_ids.clone())
            .unwrap_or_default();
        let launch_limiter = Arc::new(ProcessLaunchLimiter::new(BATCH_PROCESS_STARTS_PER_SECOND));
        let batch_project_id = project_id.clone();
        let batch_run_id = run_id.clone();
        thread::spawn(move || {
            let failed = Arc::new(AtomicBool::new(false));
            let terminal_jobs = Arc::new(Mutex::new(HashSet::new()));
            // Keep only a small number of full job plans in flight.  In
            // particular, do not eagerly enqueue hundreds of Forms (which
            // may contain credential material) before workers have even
            // started.  The producer runs in this coordinator thread, so a
            // bounded queue applies backpressure without blocking the UI.
            let queue_capacity = concurrency.saturating_mul(2).max(1);
            let (job_tx, job_rx): (
                crossbeam_channel::Sender<BatchWorkItem>,
                crossbeam_channel::Receiver<BatchWorkItem>,
            ) = crossbeam_channel::bounded(queue_capacity);
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
                    while let Ok((index, job_id, child_run_id, checkpoint, job)) = job_rx.recv() {
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
                                credential_fingerprint: None,
                            });
                            if let Ok(mut terminal) = terminal_jobs.lock() {
                                terminal.insert(index);
                            }
                            continue;
                        }
                        let _ = tx.send(Event::RunLine {
                            run_id: child_run_id.clone(),
                            job_id: job_id.clone(),
                            text: format!("══ Job {}: {} ══", index + 1, job.label),
                        });
                        let mut completed = false;
                        let mut delta_required = false;
                        let mut claimed = false;
                        for attempt in 0..=retry_count {
                            if !launch_limiter.acquire(&cancel) {
                                break;
                            }
                            if live
                                && let Err(error) = fresh_dual_imaps_authentication(&job.form)
                            {
                                failed.store(true, Ordering::Relaxed);
                                let _ = tx.send(Event::RunLine {
                                    run_id: child_run_id.clone(),
                                    job_id: job_id.clone(),
                                    text: format!(
                                        "[{}] fresh live authentication failed before launch: {error}",
                                        index + 1
                                    ),
                                });
                                let _ = tx.send(Event::JobState {
                                    job_id: job_id.clone(),
                                    child_run_id: child_run_id.clone(),
                                    state: "Failed".into(),
                                });
                                let _ = tx.send(Event::JobFinished {
                                    job_id: job_id.clone(),
                                    child_run_id: child_run_id.clone(),
                                    state: "failed".into(),
                                    detail: classified_failure_detail(&format!(
                                        "fresh live authentication failed before launch: {error}"
                                    )),
                                    credential_fingerprint: None,
                                });
                                if let Ok(mut terminal) = terminal_jobs.lock() {
                                    terminal.insert(index);
                                }
                                break;
                            }
                            if !claimed {
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
                                    let cancelled =
                                        classify_failure(&error) == FailureClass::Cancellation;
                                    if !cancelled {
                                        failed.store(true, Ordering::Relaxed);
                                    }
                                    let _ = tx.send(Event::RunLine {
                                        run_id: child_run_id.clone(),
                                        job_id: job_id.clone(),
                                        text: format!("[{}] {}", index + 1, error),
                                    });
                                    let _ = tx.send(Event::JobState {
                                        job_id: job_id.clone(),
                                        child_run_id: child_run_id.clone(),
                                        state: if cancelled { "Cancelled" } else { "Failed" }.into(),
                                    });
                                    let _ = tx.send(Event::JobFinished {
                                        job_id: job_id.clone(),
                                        child_run_id: child_run_id.clone(),
                                        state: if cancelled { "cancelled" } else { "failed" }.into(),
                                        detail: classified_failure_detail(&error),
                                        credential_fingerprint: None,
                                    });
                                    if let Ok(mut terminal) = terminal_jobs.lock() {
                                        terminal.insert(index);
                                    }
                                    break;
                                }
                                claimed = true;
                            }
                            let _ = tx.send(Event::JobState {
                                job_id: job_id.clone(),
                                child_run_id: child_run_id.clone(),
                                state: "Running".into(),
                            });
                            if attempt > 0 {
                                let _ = tx.send(Event::RunLine {
                                    run_id: child_run_id.clone(),
                                    job_id: job_id.clone(),
                                    text: format!(
                                        "[{}] retry attempt {attempt}/{retry_count}",
                                        index + 1
                                    ),
                                });
                                let _ = tx.send(Event::JobState {
                                    job_id: job_id.clone(),
                                    child_run_id: child_run_id.clone(),
                                    state: "Running".into(),
                                });
                            }
                            let prepared = job
                                .form
                                .prepared_command_with_throttle_divisor_and_checkpoint(
                                    concurrency,
                                    checkpoint.as_deref(),
                                );
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
                                                &child_run_id,
                                                &job_id,
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
                                                &child_run_id,
                                                &job_id,
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
                                        let _ = tx.send(Event::RunLine {
                                            run_id: child_run_id.clone(),
                                            job_id: job_id.clone(),
                                            text: format!(
                                                "[{}] Dovecot reports an incomplete synchronization; another delta pass is required",
                                                index + 1
                                            ),
                                        });
                                        delta_required = true;
                                    }
                                    completed = true;
                                    break;
                                }
                                Err(error)
                                    if classify_failure(&error) != FailureClass::Cancellation
                                        && attempt < retry_count
                                        && is_transient_batch_error(&error) =>
                                {
                                    let _ = tx.send(Event::RunLine {
                                        run_id: child_run_id.clone(),
                                        job_id: job_id.clone(),
                                        text: format!(
                                            "[{}] [{}] transient failure; retrying: {error}",
                                            index + 1,
                                            classify_failure(&error).label()
                                        ),
                                    });
                                    let _ = tx.send(Event::JobState {
                                        job_id: job_id.clone(),
                                        child_run_id: child_run_id.clone(),
                                        state: "Retrying".into(),
                                    });
                                    let delay = transient_retry_delay(&error, attempt);
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
                                    let cancelled =
                                        classify_failure(&error) == FailureClass::Cancellation;
                                    if !cancelled {
                                        failed.store(true, Ordering::Relaxed);
                                    }
                                    let _ = tx.send(Event::RunLine {
                                        run_id: child_run_id.clone(),
                                        job_id: job_id.clone(),
                                        text: format!(
                                            "[{}] [{}] failed: {error}",
                                            index + 1,
                                            classify_failure(&error).label()
                                        ),
                                    });
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
                                        detail: classified_failure_detail(&error),
                                        credential_fingerprint: None,
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
                                credential_fingerprint: if job.form.dry_run {
                                    Some(job.form.credential_fingerprint())
                                } else {
                                    None
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
                                credential_fingerprint: None,
                            });
                            if let Ok(mut terminal) = terminal_jobs.lock() {
                                terminal.insert(index);
                            }
                        }
                    }
                }));
            }
            drop(job_rx);

            let mut enqueue_failed = false;
            for (index, job) in jobs.into_iter().enumerate() {
                let Some(job_id) = queue_job_ids.get(index).cloned() else {
                    failed.store(true, Ordering::Relaxed);
                    enqueue_failed = true;
                    break;
                };
                let Some(child_run_id) = child_run_ids.get(index).cloned() else {
                    failed.store(true, Ordering::Relaxed);
                    enqueue_failed = true;
                    break;
                };
                if job_tx
                    .send((
                        index,
                        job_id,
                        child_run_id,
                        queue_checkpoints.get(index).cloned().unwrap_or_default(),
                        job,
                    ))
                    .is_err()
                {
                    failed.store(true, Ordering::Relaxed);
                    enqueue_failed = true;
                    break;
                }
            }
            drop(job_tx);

            let mut worker_panicked = false;
            for worker in workers {
                if worker.join().is_err() {
                    worker_panicked = true;
                    failed.store(true, Ordering::Relaxed);
                }
            }
            if enqueue_failed {
                let _ = tx.send(Event::Line(
                    "Batch work queue disconnected before all jobs were admitted; unresolved jobs require review before retrying."
                        .into(),
                ));
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
                    let _ = tx.send(Event::RunLine {
                        run_id: child_run_id.clone(),
                        job_id: job_id.clone(),
                        text: format!(
                            "[{}] worker stopped unexpectedly; job moved to Attention",
                            index + 1
                        ),
                    });
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
                        credential_fingerprint: None,
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

    fn report_export_result(&mut self, artifact: &str, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.status = format!("{artifact} exported successfully.");
                push_visible_output(&mut self.output, self.status.clone());
            }
            Err(error) => {
                self.status = format!("{artifact} was not exported: {error}");
                push_visible_output(&mut self.output, format!("[export] {}", self.status));
            }
        }
    }

    fn start(&mut self) {
        if !self.profile_available {
            self.status =
                "Execution is blocked because the saved migration profile is unavailable; repair it before starting a migration."
                    .into();
            return;
        }
        if self.workspace_read_only {
            self.status =
                "This project is being viewed read-only. Start a new migration to execute a plan."
                    .into();
            return;
        }
        if self.process_review_required {
            self.status = "Execution is blocked until you confirm that no unverified migration process remains on this host.".into();
            return;
        }
        if !self.form.dry_run {
            let current_plan = plan_fingerprint_digest(&self.form.plan_fingerprint());
            if !self.live_confirmed
                || self.live_confirmation_plan.as_deref() != Some(current_plan.as_str())
            {
                self.live_confirmed = false;
                self.live_confirmation_plan = None;
                self.live_confirm_open = true;
                return;
            }
        }
        let credential_load = if self.form.dry_run {
            self.form.load_configured_keyring_credentials()
        } else {
            self.form.reload_configured_keyring_credentials()
        };
        if let Err(error) = credential_load {
            self.status = error;
            return;
        }
        if let Err(e) = self.form.validate() {
            self.status = e;
            return;
        }
        if let (Some(project_id), Some(job_id)) = (self.project_id.clone(), self.job_id.clone()) {
            let identity_matches = match (
                self.store.project(&project_id),
                self.store.mailbox_identity(&job_id),
            ) {
                (Ok(Some(project)), Ok(Some((source, destination, state)))) => {
                    let mailbox = core::MailboxJob {
                        id: job_id.clone(),
                        source_mailbox: source,
                        destination_mailbox: destination,
                        state,
                        config: None,
                    };
                    durable_single_identity_matches(&project, &mailbox, &self.form.profile)
                }
                (Err(error), _) | (_, Err(error)) => {
                    self.status = format!(
                        "Could not read the durable mailbox identity; migration was not started: {error}"
                    );
                    return;
                }
                (Ok(None), _) | (_, Ok(None)) => false,
            };
            if !identity_matches {
                if self.form.dry_run {
                    // A changed identity is a new durable plan. Keep the
                    // previous project history intact and create a fresh
                    // project/job below rather than attaching the run to the
                    // old mailbox record.
                    self.project_id = None;
                    self.job_id = None;
                    self.selected_project_id = None;
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
        if self.requires_live_imaps_auth_probe() {
            let plan_fingerprint = plan_fingerprint_digest(&self.form.plan_fingerprint());
            let credential_fingerprint = self.form.credential_fingerprint();
            if !self
                .live_auth_proof
                .as_ref()
                .is_some_and(|proof| proof.matches(&plan_fingerprint, &credential_fingerprint))
            {
                self.start_live_imaps_auth_probe(plan_fingerprint, credential_fingerprint);
                return;
            }
        }
        self.durability_error = false;
        self.durability_recovery_pending = false;
        if !self.form.dry_run {
            if !self.persistence_available {
                self.status =
                    "Live migration is disabled because durable SQLite storage is unavailable."
                        .into();
                return;
            }
            let preflight_ready = match self.project_id.as_deref() {
                Some(project_id) => match self.store.project(project_id) {
                    Ok(Some(project)) => matches!(
                        project.phase,
                        core::Phase::Preflight
                            | core::Phase::Pilot
                            | core::Phase::Seed
                            | core::Phase::CatchUp
                            | core::Phase::FinalDelta
                            | core::Phase::Verification
                    ),
                    Ok(None) => false,
                    Err(error) => {
                        self.status = format!(
                            "Could not read durable project readiness; migration was not started: {error}"
                        );
                        return;
                    }
                },
                None => false,
            };
            let expected_plan = plan_fingerprint_digest(&self.form.plan_fingerprint());
            let plan_matches = match self.job_id.as_deref() {
                Some(job) => match self.store.preflight_plan(job) {
                    Ok(Some(plan)) => plan == expected_plan,
                    Ok(None) => false,
                    Err(error) => {
                        self.status = format!(
                            "Could not read the durable preflight plan; migration was not started: {error}"
                        );
                        return;
                    }
                },
                None => false,
            };
            let current_credential_fingerprint = self.form.credential_fingerprint();
            let credentials_match = self.preflight_credential_fingerprint.as_deref()
                == Some(current_credential_fingerprint.as_str());
            let mailbox_ready = match self.job_id.as_deref() {
                Some(job) => match self.store.mailbox_state(job) {
                    Ok(Some(state)) => state == "ready" || state == "delta_required",
                    Ok(None) => false,
                    Err(error) => {
                        self.status = format!(
                            "Could not read durable mailbox readiness; migration was not started: {error}"
                        );
                        return;
                    }
                },
                None => false,
            };
            if !preflight_ready || !mailbox_ready || !plan_matches || !credentials_match {
                self.status = if credentials_match {
                    "Run a successful dry preflight for this exact mailbox plan before starting live migration.".into()
                } else {
                    "Credentials changed or were reloaded since preflight. Run a new dry preflight before starting live migration.".into()
                };
                return;
            }
        }
        if self.project_id.is_none() {
            match self.store.create_project_with_mailbox(
                &self.form.profile.name,
                &self.form.profile.source_host,
                &self.form.profile.destination_host,
                &self.form.profile.source_user,
                &self.form.profile.destination_user,
            ) {
                Ok((project, job)) => {
                    self.project_id = Some(project.id.clone());
                    self.selected_project_id = Some(project.id.clone());
                    self.job_id = Some(job);
                }
                Err(error) => {
                    self.status = format!("Could not create durable migration project: {error}");
                    return;
                }
            }
        }
        let plan_fingerprint = plan_fingerprint_digest(&self.form.plan_fingerprint());
        let credential_fingerprint = self.form.credential_fingerprint();
        let run_engine = self.form.engine();
        let run_dry_run = self.form.dry_run;
        let (run_project_id, run_job_id) = match (self.project_id.clone(), self.job_id.clone()) {
            (Some(project), Some(job)) => (project, job),
            _ => {
                self.status = "Could not start without a durable mailbox project.".into();
                return;
            }
        };
        let dovecot_checkpoint = if run_engine == core::Engine::Dovecot && !run_dry_run {
            match self.store.mailbox_checkpoint(&run_job_id) {
                Ok(checkpoint) => checkpoint,
                Err(error) => {
                    self.status = format!(
                        "Could not read the durable Dovecot checkpoint; migration was not started: {error}"
                    );
                    return;
                }
            }
        } else {
            None
        };
        let plan_snapshot = match self
            .form
            .plan_snapshot_with_checkpoint(dovecot_checkpoint.as_deref())
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        let prepared = match self
            .form
            .prepared_command_with_throttle_divisor_and_checkpoint(1, dovecot_checkpoint.as_deref())
        {
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
        let observed_engine_version = probe_engine_version(&exe);
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
        if let Some(version) = observed_engine_version
            && let Err(error) = self.store.record_engine_version(&run_id, &version)
        {
            cleanup_paths(&cleanup);
            let _ = self.store.finish_run_for_mailbox_with_checkpoint(
                &run_project_id,
                &run_job_id,
                &run_id,
                "failed",
                "attention",
                &format!("could not persist engine version metadata: {error}"),
                None,
            );
            self.status = format!("Could not persist engine version metadata: {error}");
            return;
        }
        self.locked_profile = Some(self.form.profile.clone());
        self.locked_dry_run = Some(self.form.dry_run);
        self.live_auth_proof = None;
        self.live_confirmed = false;
        self.live_confirmation_plan = None;
        self.pending_checkpoint = None;
        self.run_id = Some(run_id.clone());
        self.active_run = Some(ActiveRunContext {
            run_id: run_id.clone(),
            project_id: run_project_id,
            job_id: Some(run_job_id.clone()),
            batch_job_ids: Vec::new(),
            batch_child_run_ids: Vec::new(),
            batch_child_indices: HashMap::new(),
            batch_plan_fingerprints: Vec::new(),
            kind: RunKind::Single,
            dry_run: run_dry_run,
            engine: run_engine,
            plan_fingerprint,
            credential_fingerprint,
        });
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_requested = Some(cancel.clone());
        self.receiver = Some(rx);
        self.run_started_at = Some(std::time::Instant::now());
        self.status = if self.form.dry_run {
            "Preflight in progress".into()
        } else {
            "Sync in progress".into()
        };
        self.output = VecDeque::from([format!(
            "Starting {} with {}…",
            if self.form.dry_run {
                "preflight"
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
            let worker_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
                            &run_id,
                            &process_job_id,
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
                            &run_id,
                            &process_job_id,
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
            }));
            if worker_result.is_err() {
                let _ = tx.send(Event::Finished(Err(
                    "single-run worker panicked; migration requires operator review".into(),
                )));
            }
        });
    }
    fn poll(&mut self) {
        self.refresh_ui_snapshot();
        if let Some(receiver) = &self.bulk_import_receiver
            && let Ok(result) = receiver.try_recv()
        {
            self.bulk_import_receiver = None;
            self.apply_bulk_import_result(result);
        }
        let live_auth_result = self
            .live_auth_receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        if let Some(result) = live_auth_result {
            self.live_auth_receiver = None;
            match result {
                Ok(proof) => {
                    self.live_auth_proof = Some(proof);
                    self.status =
                        "Fresh IMAPS authentication passed; continuing live admission…".into();
                    self.start();
                }
                Err(error) => {
                    self.live_auth_proof = None;
                    self.status =
                        format!("Live authentication failed; migration was not started: {error}");
                }
            }
        }
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
        let mut pending_db_events = std::mem::take(&mut self.pending_db_events);
        let mut deferred_events = std::mem::take(&mut self.deferred_events);
        let mut durability_errors = Vec::new();
        let mut recovered_durability = false;
        let active_run = self.active_run.clone();
        if let Some(rx) = &self.receiver {
            let mut processed_events = 0;
            while processed_events < MAX_EVENTS_PER_FRAME {
                let event = if let Some(event) = deferred_events.pop_front() {
                    event
                } else {
                    match rx.try_recv() {
                        Ok(event) => event,
                        Err(_) => break,
                    }
                };
                processed_events += 1;
                match event {
                    Event::ClaimBatch {
                        project_id,
                        job_id,
                        parent_run_id,
                        child_run_id,
                        reply,
                    } => {
                        let result = if active_run.as_ref().is_some_and(|run| {
                            run.project_id == project_id
                                && run.owns_batch_child(&parent_run_id, &child_run_id, &job_id)
                        }) {
                            self.store
                                .claim_batch_mailbox_for_child(
                                    &project_id,
                                    &job_id,
                                    &parent_run_id,
                                    &child_run_id,
                                )
                                .map_err(|error| error.to_string())
                        } else {
                            Err(
                                "batch claim event does not belong to the active run context"
                                    .to_owned(),
                            )
                        };
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
                        reply,
                    ) => {
                        let checkpoint_run_id = process_run_id.clone();
                        let result = if active_run
                            .as_ref()
                            .is_some_and(|run| run.owns_process(&process_run_id, &job_id))
                        {
                            self.store
                                .register_process(&core::ActiveProcess {
                                    run_id: process_run_id,
                                    job_id,
                                    pid,
                                    start_ticks,
                                    process_group,
                                    session_id,
                                    executable,
                                })
                                .map_err(|error| error.to_string())
                        } else {
                            Err(
                                "process-start event does not belong to the active run context"
                                    .to_owned(),
                            )
                        };
                        if result.is_ok()
                            && active_run
                                .as_ref()
                                .is_some_and(|run| matches!(run.kind, RunKind::Batch))
                        {
                            // A new attempt has a new process and therefore
                            // must not inherit a checkpoint candidate emitted
                            // by a failed earlier attempt of the same child.
                            self.pending_batch_checkpoints.remove(&checkpoint_run_id);
                        }
                        let _ = reply.send(result.clone());
                        if let Err(error) = result {
                            durability_errors.push(format!(
                                "persist process identity failed; cancellation requested before an untracked engine can continue: {error}"
                            ));
                            // A migration must not continue when its process
                            // identity could not be durably registered. The
                            // runner will terminate the child through its
                            // normal cancellation path, while the terminal
                            // event records the durability failure.
                            if let Some(cancel) = &self.cancel_requested {
                                cancel.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                    Event::RunLine {
                        run_id,
                        job_id,
                        text,
                    } => {
                        let owns_line = active_run.as_ref().is_some_and(|run| {
                            if matches!(run.kind, RunKind::Batch) {
                                run.owns_batch_child(&run.run_id, &run_id, &job_id)
                            } else {
                                run.run_id == run_id
                                    && run.job_id.as_deref() == Some(job_id.as_str())
                            }
                        });
                        if owns_line {
                            let project_id = active_run
                                .as_ref()
                                .map(|run| run.project_id.clone())
                                .expect("owned output requires an active run");
                            pending_db_events.push((
                                project_id,
                                run_id,
                                "run_output".into(),
                                text.clone(),
                            ));
                        }
                        push_visible_output(&mut self.output, text);
                    }
                    Event::ProcessEnded { run_id, job_id } => {
                        if active_run
                            .as_ref()
                            .is_some_and(|run| run.owns_process(&run_id, &job_id))
                        {
                            if let Err(error) = self.store.clear_processes(&run_id) {
                                durability_errors.push(format!(
                                    "clear completed process identity for {run_id} failed: {error}"
                                ));
                            }
                        } else {
                            durability_errors.push(format!(
                                "ignored process-ended event for unknown run {run_id}"
                            ));
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
                            && let Some(index) = run.batch_child_index(&job_id, &child_run_id)
                        {
                            let bulk_index = self
                                .bulk_job_index_by_id
                                .get(&job_id)
                                .copied()
                                .unwrap_or(index);
                            if let Some(job) = self.bulk_jobs.get_mut(bulk_index) {
                                job.state = state.clone();
                            }
                            // JobState is deliberately presentation-only. The
                            // worker has already received an acknowledged
                            // ClaimBatch response before it can launch the
                            // process, and terminal state, evidence,
                            // checkpoint, and preflight data must commit
                            // together in the following JobFinished event.
                            // Persisting a terminal JobState here would leave
                            // a mailbox terminal while its child run remained
                            // running if that transaction later failed.
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
                        credential_fingerprint,
                    } => {
                        // Diagnostics are accepted only while a child is
                        // active/queued. Flush them before the terminal
                        // transaction so a fast worker cannot deliver
                        // RunLine(s) and JobFinished in one poll cycle and
                        // lose the child log to the terminal-state guard.
                        if !pending_db_events.is_empty() {
                            let batch = pending_db_events
                                .iter()
                                .map(|(_, run_id, kind, detail)| {
                                    (run_id.as_str(), kind.as_str(), detail.as_str())
                                })
                                .collect::<Vec<_>>();
                            match self.store.record_events_for_runs_batch(&batch) {
                                Ok(()) => {
                                    pending_db_events.clear();
                                    if self.durability_recovery_pending {
                                        recovered_durability = true;
                                    }
                                }
                                Err(error) => {
                                    self.durability_recovery_pending = true;
                                    durability_errors.push(format!(
                                        "persist child diagnostics before completion failed: {error}"
                                    ));
                                    // Leave the child running in the ledger,
                                    // retain both the diagnostics and the
                                    // completion event, and stop consuming
                                    // later events until the durable
                                    // predecessor can be committed.
                                    deferred_events.push_front(Event::JobFinished {
                                        job_id,
                                        child_run_id,
                                        state,
                                        detail,
                                        credential_fingerprint,
                                    });
                                    break;
                                }
                            }
                        }
                        if let Some(run) = active_run.as_ref()
                            && matches!(run.kind, RunKind::Batch)
                            && let Some(index) = run.batch_child_index(&job_id, &child_run_id)
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
                            let evidence = self.pending_batch_evidence.get(&child_run_id);
                            let final_state = evidence.map_or(state.clone(), |value| {
                                if value.is_exact_match() && state != "delta_required" {
                                    "verified".into()
                                } else if state == "delta_required" {
                                    "delta_required".into()
                                } else {
                                    "verification_difference".into()
                                }
                            });
                            let checkpoint = self
                                .pending_batch_checkpoints
                                .get(&child_run_id)
                                .filter(|_| run_status == "completed");
                            let preflight_plan = if run.dry_run && state == "ready" {
                                run.batch_plan_fingerprints.get(index).cloned()
                            } else {
                                None
                            };
                            let result = if let Some(value) = evidence.as_ref() {
                                self.store
                                    .finish_run_for_mailbox_with_evidence_and_preflight_plan_and_checkpoint(
                                        &run.project_id,
                                        &job_id,
                                        &child_run_id,
                                        run_status,
                                        &final_state,
                                        &detail,
                                        value,
                                        preflight_plan.as_deref(),
                                        checkpoint.map(String::as_str),
                                    )
                            } else {
                                self.store
                                    .finish_run_for_mailbox_with_preflight_plan_and_checkpoint(
                                        &run.project_id,
                                        &job_id,
                                        &child_run_id,
                                        run_status,
                                        &final_state,
                                        &detail,
                                        preflight_plan.as_deref(),
                                        checkpoint.map(String::as_str),
                                    )
                            };
                            let completion_persisted = result.is_ok();
                            // Durable state is authoritative. Do not show a
                            // terminal child state in the editable queue until
                            // the run/mailbox transaction has committed.
                            if completion_persisted
                                && let Some(bulk_index) =
                                    self.bulk_job_index_by_id.get(&job_id).copied()
                                && let Some(job) = self.bulk_jobs.get_mut(bulk_index)
                            {
                                job.state = display_job_state(&final_state).into();
                            }
                            if let Err(error) = result {
                                self.durability_recovery_pending = true;
                                durability_errors.push(format!(
                                    "persist child run {} completion failed: {error}",
                                    index + 1
                                ));
                                // Keep the completion event and its evidence
                                // inputs together until the terminal
                                // transaction succeeds. The next poll will
                                // retry this event after any pending
                                // diagnostics have been committed.
                                deferred_events.push_front(Event::JobFinished {
                                    job_id,
                                    child_run_id,
                                    state,
                                    detail,
                                    credential_fingerprint,
                                });
                                break;
                            } else {
                                if self.durability_recovery_pending {
                                    recovered_durability = true;
                                }
                                self.pending_batch_evidence.remove(&child_run_id);
                                self.pending_batch_checkpoints.remove(&child_run_id);
                            }
                            if completion_persisted
                                && !self.bulk_live_run
                                && state == "ready"
                                && let Some(fingerprint) = credential_fingerprint
                                && let Some(bulk_index) =
                                    self.bulk_job_index_by_id.get(&job_id).copied()
                                && let Some(saved) = self
                                    .bulk_preflight_credential_fingerprints
                                    .get_mut(bulk_index)
                            {
                                *saved = Some(fingerprint);
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
                            && run.batch_child_index(&job_id, &child_run_id).is_some()
                        {
                            self.pending_batch_evidence.insert(child_run_id, evidence);
                        } else {
                            durability_errors.push(format!(
                                "ignored evidence event for unknown child run {child_run_id}"
                            ));
                        }
                    }
                    Event::Checkpoint {
                        run_id,
                        job_id,
                        value,
                    } => {
                        if let Some(run) = active_run.as_ref()
                            && run.owns_process(&run_id, &job_id)
                        {
                            if matches!(run.kind, RunKind::Batch) {
                                self.pending_batch_checkpoints.insert(run_id, value);
                            } else if run.run_id == run_id {
                                self.pending_checkpoint = Some(value);
                            }
                        } else {
                            durability_errors.push(format!(
                                "ignored Dovecot checkpoint for unknown run {run_id}"
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
                .map(|(_, run_id, kind, detail)| (run_id.as_str(), kind.as_str(), detail.as_str()))
                .collect::<Vec<_>>();
            let result = active_run.as_ref().map_or_else(
                || Err(rusqlite::Error::InvalidQuery),
                |_| self.store.record_events_for_runs_batch(&batch),
            );
            if let Err(error) = result {
                self.durability_recovery_pending = true;
                durability_errors.push(format!(
                    "record execution events failed; diagnostics remain queued for retry: {error}"
                ));
                self.pending_db_events = pending_db_events.clone();
            } else {
                pending_db_events.clear();
                if self.durability_recovery_pending {
                    recovered_durability = true;
                }
            }
        }
        let cycle_had_durability_errors = !durability_errors.is_empty();
        for error in durability_errors {
            self.report_store_error("batch event persistence", Err(error));
        }
        self.deferred_events = deferred_events;
        if recovered_durability
            && self.durability_recovery_pending
            && !cycle_had_durability_errors
            && pending_db_events.is_empty()
            && self.deferred_events.is_empty()
        {
            self.durability_error = false;
            self.durability_recovery_pending = false;
        }
        if !pending_db_events.is_empty() || !self.deferred_events.is_empty() {
            // A parent Finished event must not finalize the batch while a
            // child completion or its audit trail is waiting on durable
            // storage. The retained events will be retried on the next poll.
            done = None;
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
                self.pending_evidence.clone().or_else(|| {
                    (run_context.engine == core::Engine::ImapSync)
                        .then(|| {
                            let output = self.output.iter().cloned().collect::<Vec<_>>();
                            verification::parse_imapsync_evidence(&output)
                        })
                        .flatten()
                })
            } else {
                self.pending_evidence.clone()
            };
            let terminal_checkpoint = if !was_bulk_run && succeeded {
                self.pending_checkpoint.clone()
            } else {
                None
            };
            if succeeded && !run_context.dry_run && terminal_evidence.is_none() {
                let result = self.store.record_event(
                    &run_context.project_id,
                    "verification_pending",
                    "completed transfer did not provide complete verification evidence",
                );
                self.report_store_error("record incomplete verification", result);
            }
            if !was_bulk_run && let Some(job) = &run_context.job_id {
                if succeeded && run_context.dry_run {
                    let result = self
                        .store
                        .set_preflight_plan(job, &run_context.plan_fingerprint);
                    self.report_store_error("record preflight plan", result);
                }
                let final_state =
                    if succeeded && run_context.dry_run {
                        "ready"
                    } else if !succeeded {
                        if r.as_ref().err().is_some_and(|error| {
                            classify_failure(error) == FailureClass::Verification
                        }) {
                            "attention"
                        } else if r.as_ref().err().is_some_and(|error| {
                            classify_failure(error) == FailureClass::Cancellation
                        }) {
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
            let mut retry_terminal_commit = false;
            {
                let project = &run_context.project_id;
                let run_id = &run_context.run_id;
                let run_status = if succeeded {
                    "completed"
                } else if r
                    .as_ref()
                    .err()
                    .is_some_and(|error| classify_failure(error) == FailureClass::Verification)
                {
                    "verification_failed"
                } else if r
                    .as_ref()
                    .err()
                    .is_some_and(|error| classify_failure(error) == FailureClass::Cancellation)
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
                            self.store
                                .finish_run_for_mailbox_with_evidence_and_checkpoint(
                                    project,
                                    job,
                                    run_id,
                                    run_status,
                                    state,
                                    &detail,
                                    evidence,
                                    terminal_checkpoint.as_deref(),
                                )
                        } else {
                            self.store.finish_run_for_mailbox_with_checkpoint(
                                project,
                                job,
                                run_id,
                                run_status,
                                state,
                                &detail,
                                terminal_checkpoint.as_deref(),
                            )
                        }
                    } else {
                        self.store.finish_run_for_mailbox_with_checkpoint(
                            project,
                            job,
                            run_id,
                            run_status,
                            state,
                            &detail,
                            terminal_checkpoint.as_deref(),
                        )
                    }
                } else {
                    self.store.finish_run(run_id, run_status, &detail)
                };
                let terminal_write_ok = match terminal_write {
                    Ok(()) => true,
                    Err(error) => {
                        self.durability_error = true;
                        self.durability_recovery_pending = true;
                        push_visible_output(
                            &mut self.output,
                            format!("[durability] Could not persist terminal state: {error}"),
                        );
                        retry_terminal_commit = true;
                        false
                    }
                };
                if terminal_write_ok && !was_bulk_run && run_context.dry_run {
                    self.preflight_credential_fingerprint =
                        Some(run_context.credential_fingerprint.clone());
                }
                if terminal_write_ok {
                    if self.durability_recovery_pending && !cycle_had_durability_errors {
                        self.durability_error = false;
                        self.durability_recovery_pending = false;
                    }
                    // These inputs belong to this terminal commit. Do not
                    // consume them before SQLite acknowledges the commit, or
                    // a retry would be unable to reproduce the same durable
                    // result.
                    self.pending_evidence = None;
                    self.pending_checkpoint = None;
                }
                // A terminal run commit is the boundary between an external
                // process result and durable control-plane state.  If that
                // commit failed, the database still owns a running run (or a
                // queued child), so do not append a terminal success/failure
                // event or advance the project phase.  Startup recovery must
                // reconcile it after the operator has restored the database.
                if was_bulk_run && terminal_write_ok {
                    let result = self.store.record_event(
                        project,
                        "run_finished",
                        if succeeded { "success" } else { "failure" },
                    );
                    self.report_store_error("record batch run completion", result);
                }
                // A successful engine result is not enough to advance the
                // lifecycle. Any earlier persistence failure in this event
                // cycle (for example, the preflight digest or evidence
                // event) leaves the durable result incomplete and must keep
                // the project in its current phase for recovery/review.
                if terminal_phase_advance_allowed(
                    succeeded,
                    terminal_write_ok,
                    self.durability_error,
                ) {
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
                } else if terminal_write_ok && !succeeded && !self.durability_error {
                    let result = self.store.transition(project, core::Phase::Attention);
                    self.report_store_error("move project to Attention", result);
                }
            }
            if retry_terminal_commit {
                // The external process is already gone, but the durable run
                // is still active. Keep ownership and retry the exact
                // terminal event on the next poll instead of forcing startup
                // recovery for a transient SQLite failure.
                self.deferred_events.push_front(Event::Finished(r));
                self.status =
                    "Migration result requires durable storage; retrying terminal commit".into();
                return;
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
            self.locked_profile = None;
            self.locked_dry_run = None;
            self.pending_batch_evidence.clear();
            // Keep the durable queue after completion so a validated batch
            // can be promoted to live execution, and failed/live jobs can be
            // deliberately retried or run through another delta pass.
            if !was_bulk_run {
                if self.selected_project_id == self.bulk_project_id {
                    self.selected_project_id = None;
                }
                self.bulk_project_id = None;
                self.bulk_job_ids.clear();
                self.bulk_job_index_by_id.clear();
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
                Some("verified_with_exceptions") => {
                    "Migration completed with accepted verification exceptions"
                }
                Some("delta_required") => "Migration completed; final delta or review required",
                Some("verification_difference") => {
                    "Migration completed; verification found differences requiring review"
                }
                _ => "Migration completed; verification requires operator review",
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn account(
        ui: &mut egui::Ui,
        title: &str,
        host: &mut String,
        user: &mut String,
        password: &mut String,
        password_required: bool,
        saved_credential: bool,
        color: Color32,
    ) {
        let editable = ui.ctx().data(|data| {
            data.get_temp::<bool>(egui::Id::new("plan_controls_enabled"))
                .unwrap_or(true)
        });
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
                ui.add_enabled(editable, egui::TextEdit::singleline(host));
            });
            inline_error(ui, "Server", host, true);
            ui.horizontal(|ui| {
                ui.label("User");
                ui.add_enabled(editable, egui::TextEdit::singleline(user));
            });
            inline_error(ui, "User", user, true);
            ui.horizontal(|ui| {
                ui.label("Password");
                let visibility_id = ui.make_persistent_id(title).with("password_visibility");
                let visible = ui
                    .ctx()
                    .data_mut(|data| data.get_temp::<bool>(visibility_id).unwrap_or(false));
                ui.add_enabled(
                    editable,
                    egui::TextEdit::singleline(password).password(!visible),
                );
                if ui.button(if visible { "Hide" } else { "Show" }).clicked() {
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(visibility_id, !visible));
                }
            });
            if saved_credential && password.is_empty() {
                ui.label(
                    RichText::new("Saved credential configured; session password not required.")
                        .color(TEAL)
                        .size(12.0),
                );
            } else {
                inline_error(
                    ui,
                    "Password",
                    password,
                    password_required && !saved_credential,
                );
            }
        });
    }
    fn preview(&mut self, ctx: &egui::Context) {
        if !self.preview {
            return;
        }
        egui::Window::new("Execution plan")
            .open(&mut self.preview)
            .default_width(670.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Passwords are redacted. This is an argument list for review, not a shell command to paste.",
                    )
                    .color(MUTED),
                );
                let (exe, args) = self.form.command(true);
                ui.label(RichText::new(format!("Executable: {exe}")).monospace());
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for (index, argument) in args.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("[{index:>3}]")).monospace().color(MUTED),
                                );
                                ui.label(RichText::new(argument).monospace());
                            });
                        }
                    });
            });
    }
    fn bulk_dialog(&mut self, ctx: &egui::Context) {
        let colors = self.theme_colors();
        if !self.bulk_open {
            return;
        }
        let mut open = self.bulk_open;
        egui::Window::new("Batch migration queue").open(&mut open).default_width(850.0).default_height(540.0).show(ctx, |ui| {
            ui.heading("Import → review → validate");
            ui.label(RichText::new(&self.bulk_message).color(MUTED));
            ui.add_space(8.0);
            let mut state_counts = HashMap::new();
            for job in &self.bulk_jobs {
                *state_counts.entry(display_state_key(&job.state)).or_insert(0_usize) += 1;
            }
            let count_state = |state: &str| state_counts.get(state).copied().unwrap_or(0);
            let unresolved_count = ["failed", "attention", "cancelled", "delta_required", "verification_difference"]
                .iter()
                .map(|state| count_state(state))
                .sum::<usize>();
            ui.group(|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(format!("{} total", self.bulk_jobs.len()));
                    ui.label(format!("{} ready", count_state("ready")));
                    ui.label(format!("{} running", count_state("running")));
                    ui.label(format!("{} verified", count_state("verified") + count_state("verified_with_exceptions")));
                    ui.label(RichText::new(format!("{} unresolved", unresolved_count)).color(if unresolved_count > 0 { ALERT } else { TEAL }));
                    if ui.button("Select unresolved").clicked() { self.select_bulk_state_set(BulkStateSet::Unresolved); }
                    if ui.button("Select failed").clicked() { self.select_bulk_state_set(BulkStateSet::Failed); }
                    if ui.button("Select attention").clicked() { self.select_bulk_state_set(BulkStateSet::Attention); }
                    if !self.bulk_selected_ids.is_empty() && ui.button("Clear selection").clicked() { self.bulk_selected_ids.clear(); }
                });
                ui.label(RichText::new("Focused selections apply to preflight and live scope controls below; live execution still requires matching preflight and confirmation.").size(11.0).color(MUTED));
            });
            ui.add_space(8.0);
            let previous_bulk_dry_run = self.bulk_dry_run;
            ui.horizontal(|ui| {
                ui.label("Batch mode");
                ui.selectable_value(&mut self.bulk_dry_run, true, "Preflight");
                ui.selectable_value(&mut self.bulk_dry_run, false, "Live migration");
            });
            if self.bulk_dry_run != previous_bulk_dry_run {
                self.bulk_live_confirmed = false;
                self.bulk_confirmation_summary = None;
            }
            let queue_editable = !self.running();
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.running() && self.bulk_import_receiver.is_none(), egui::Button::new("Import CSV / XLSX…")).clicked() && let Some(path) = rfd::FileDialog::new().add_filter("Migration lists", &["csv", "xls", "xlsx"]).pick_file() { self.request_bulk_import(path); }
                if ui.add_enabled(!self.running(), egui::Button::new("Clear queue")).clicked() {
                    if self.bulk_jobs.is_empty() {
                        self.clear_bulk_queue();
                    } else {
                        self.bulk_clear_confirm_open = true;
                    }
                }
                if ui.add_enabled(!self.running() && !self.bulk_jobs.is_empty(), egui::Button::new("Export selected set…")).clicked() {
                    self.bulk_message = match self.export_bulk_selection() {
                        Ok(()) => "Selected batch rows exported without credentials or engine options.".into(),
                        Err(error) => error,
                    };
                }
                let live_count = self
                    .bulk_jobs
                    .iter()
                    .filter(|job| self.bulk_retry_scope.includes(&display_state_key(&job.state)))
                    .count();
                let label = if self.bulk_dry_run {
                    format!("Run {} preflight checks", self.bulk_jobs.len())
                } else {
                    format!("Start {live_count} live migrations")
                };
                let can_start = !self.running()
                    && !self.bulk_jobs.is_empty()
                    && (self.bulk_dry_run || live_count > 0);
                if ui.add_enabled(can_start, egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(if self.bulk_dry_run { BLUE } else { ALERT })).clicked() { self.start_bulk(); }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label("Maximum concurrent workers");
                ui.add_enabled(
                    queue_editable,
                    egui::Slider::new(&mut self.form.profile.batch_concurrency, 1..=16),
                );
                ui.label(RichText::new("Applies to both preflight and live migration; bounded to 1–16 workers").size(11.0).color(MUTED));
            });
            ui.horizontal(|ui| {
                ui.label("Transient retries");
                ui.add_enabled(
                    queue_editable,
                    egui::Slider::new(&mut self.form.profile.batch_retry_count, 0..=3),
                );
                ui.label(RichText::new("auth/configuration failures are never retried").size(11.0).color(MUTED));
            });
            if !self.bulk_dry_run {
                ui.add_enabled_ui(queue_editable, |ui| {
                    egui::ComboBox::from_id_salt("bulk_retry_scope")
                        .selected_text(self.bulk_retry_scope.label())
                        .show_ui(ui, |ui| {
                            for scope in [
                                BulkRetryScope::Unresolved,
                                BulkRetryScope::FailedAttention,
                                BulkRetryScope::DeltaRequired,
                                BulkRetryScope::VerificationDifference,
                                BulkRetryScope::All,
                            ] {
                                ui.selectable_value(
                                    &mut self.bulk_retry_scope,
                                    scope,
                                    scope.label(),
                                );
                            }
                        });
                });
                ui.label(
                    RichText::new(format!(
                        "Live scope: {}. Verified rows run only with the explicit all-rows scope.",
                        self.bulk_retry_scope.label()
                    ))
                    .size(11.0)
                    .color(MUTED),
                );
            }
            ui.label(RichText::new("Passwordless queue credentials").strong());
            ui.label(RichText::new("Apply an existing OS-keyring reference to rows that do not already have a password or credential ID. The secret itself is never copied into the queue.").size(11.0).color(MUTED));
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
            egui::Grid::new("bulk_jobs")
                .striped(true)
                .min_col_width(120.0)
                .show(ui, |ui| {
                    ui.strong("#"); ui.strong("Migration"); ui.strong("Source"); ui.strong("Destination"); ui.strong("Source password"); ui.strong("Destination password"); ui.strong("Status"); ui.end_row();
                    let row_count = self.bulk_jobs.len();
                    egui::ScrollArea::vertical().show_rows(ui, 42.0, row_count, |ui, rows| {
                    for index in rows {
                        let job = &mut self.bulk_jobs[index];
                        ui.label((index + 1).to_string());
                        ui.label(&job.label);
                        ui.label(format!("{}\n{}", job.form.profile.source_host, job.form.profile.source_user));
                        ui.label(format!("{}\n{}", job.form.profile.destination_host, job.form.profile.destination_user));
                        ui.add_enabled(queue_editable, egui::TextEdit::singleline(&mut *job.form.source_password).password(true).desired_width(120.0));
                        if job.form.engine() == core::Engine::Dovecot { ui.label("Not required"); } else { ui.add_enabled(queue_editable, egui::TextEdit::singleline(&mut *job.form.destination_password).password(true).desired_width(120.0)); }
                        let (badge, color) = job_state_badge(&job.state, colors);
                        ui.label(RichText::new(badge).color(color));
                        ui.end_row();
                    }
                    });
                });
            ui.add_space(8.0); ui.label(RichText::new("Imported passwords are used only for this open queue. Saving a profile never saves them.").size(11.0).color(ALERT));
        });
        self.bulk_open = open;
    }
    fn bulk_clear_confirmation(&mut self, ctx: &egui::Context) {
        if !self.bulk_clear_confirm_open || self.running() {
            return;
        }
        let mut open = self.bulk_clear_confirm_open;
        let mut clear = false;
        let mut close_requested = false;
        egui::Window::new("Clear mailbox queue?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Discard the current queue?");
                ui.label(format!(
                    "This removes {} mailbox row(s), selection, in-memory passwords, and the durable batch association from this workspace.",
                    self.bulk_jobs.len()
                ));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Keep queue").clicked() {
                        close_requested = true;
                    }
                    if ui.button("Clear queue").clicked() {
                        clear = true;
                        close_requested = true;
                    }
                });
            });
        self.bulk_clear_confirm_open = open && !close_requested;
        if clear {
            self.clear_bulk_queue();
        }
    }
    fn bulk_import_confirmation(&mut self, ctx: &egui::Context) {
        if self.pending_bulk_import.is_none() || self.running() {
            return;
        }
        let path_label = self
            .pending_bulk_import
            .as_deref()
            .and_then(std::path::Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("the selected file")
            .to_owned();
        let mut open = true;
        let mut close_requested = false;
        let mut replace = false;
        egui::Window::new("Replace mailbox queue?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Replace the current queue?");
                ui.label(format!(
                    "Importing {path_label} will replace {} current mailbox row(s), selection, in-memory passwords, and the durable batch association.",
                    self.bulk_jobs.len()
                ));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Keep current queue").clicked() {
                        close_requested = true;
                    }
                    if ui.button("Replace queue").clicked() {
                        replace = true;
                        close_requested = true;
                    }
                });
            });
        if close_requested || !open {
            let path = self.pending_bulk_import.take();
            if replace && let Some(path) = path {
                self.import_bulk(&path);
            }
        }
    }
    fn bulk_live_confirmation(&mut self, ctx: &egui::Context) {
        if !self.bulk_live_confirm_open {
            return;
        }
        if self.bulk_confirmation_summary.is_none() {
            let mut durable_state_error = None;
            let eligible_indices = self
                .bulk_jobs
                .iter()
                .enumerate()
                .filter_map(|(index, _)| {
                    let job_id = self.bulk_job_ids.get(index)?;
                    let state = match self.store.mailbox_state(job_id) {
                        Ok(Some(state)) => state,
                        Ok(None) => return None,
                        Err(error) => {
                            durable_state_error = Some(format!(
                                "Could not read durable mailbox state for confirmation: {error}"
                            ));
                            return None;
                        }
                    };
                    (self.bulk_row_is_selected(index) && self.bulk_retry_scope.includes(&state))
                        .then_some(index)
                })
                .collect::<HashSet<_>>();
            let deletion_enabled = eligible_indices
                .iter()
                .any(|index| self.bulk_jobs[*index].form.profile.delete2);
            self.bulk_confirmation_summary = Some(BulkConfirmationSummary {
                eligible_count: eligible_indices.len(),
                deletion_enabled,
                durable_state_error,
                concurrency: self.form.profile.batch_concurrency.clamp(1, 16),
                scope: self.bulk_retry_scope,
            });
        }
        let summary = self
            .bulk_confirmation_summary
            .as_ref()
            .cloned()
            .expect("confirmation summary is initialized above");
        let mut open = self.bulk_live_confirm_open;
        let mut close = false;
        egui::Window::new("Confirm live batch migration")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading(RichText::new("This will change destination mailboxes").color(ALERT));
                ui.label(format!("{} mailboxes selected", summary.eligible_count));
                if let Some(error) = &summary.durable_state_error {
                    ui.label(RichText::new(error).color(ALERT));
                }
                ui.label(format!("Worker concurrency: {}", summary.concurrency));
                ui.label(
                    RichText::new(format!(
                        "Destination deletion: {}",
                        if summary.deletion_enabled {
                            "ENABLED ⚠"
                        } else {
                            "disabled"
                        }
                    ))
                    .color(if summary.deletion_enabled { ALERT } else { MUTED }),
                );
                ui.label(format!("Scope: {}.", summary.scope.label()));
                ui.label("Each mailbox must already have a matching successful preflight. Source mail is not deleted by default.");
                ui.label(RichText::new("Review the queue, concurrency, throttles, and exact plans before continuing.").color(MUTED));
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui
                        .add_enabled(
                            summary.durable_state_error.is_none() && summary.eligible_count > 0,
                            egui::Button::new(
                                RichText::new("I understand — start batch").color(Color32::WHITE),
                            )
                            .fill(ALERT),
                        )
                        .clicked()
                    {
                        close = true;
                        self.bulk_live_confirmed = true;
                        self.start_bulk();
                    }
                });
            });
        self.bulk_live_confirm_open = open && !close;
        if !self.bulk_live_confirm_open {
            self.bulk_confirmation_summary = None;
        }
    }
    fn advanced_dialog(&mut self, ctx: &egui::Context) {
        if !self.advanced_open {
            return;
        }
        let mut open = self.advanced_open;
        egui::Window::new("Advanced migration options").open(&mut open).default_width(620.0).show(ctx, |ui| {
            ui.label(RichText::new("These controls affect the imapsync fallback. Dovecot-native migrations use doveadm and server-side consistency rules.").color(MUTED));
            ui.add_space(8.0);
            let editable = !self.running();
            ui.add_enabled_ui(editable, |ui| {
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
                        ui.group(|ui| { ui.heading(RichText::new("Destructive destination option").color(ALERT)); ui.checkbox(&mut self.form.profile.delete2, "Delete destination messages missing from source  (--delete2)"); ui.label(RichText::new("Use only for an intentionally exact backup after a tested preflight. This can remove destination mail.").size(11.0).color(ALERT)); });
            });
            if !editable {
                ui.label(RichText::new("Advanced plan settings are locked while a migration is running.").color(MUTED));
            }
                ui.add_space(8.0); ui.label("The Extra imapsync options field accepts only the documented safe tuning and diagnostic allowlist. Connection, credential, TLS, destructive, logging, and unknown flags are rejected.");
        });
        self.advanced_open = open;
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
                });
                if !editable {
                    ui.label(RichText::new("Credential settings are locked while a migration is running.").color(MUTED));
                }
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
            let editable = !self.running();
            ui.add_enabled_ui(editable, |ui| {
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
                        "Local doveadm is supported. Remote execution remains disabled until a secret-safe broker is available.",
                    );
                    egui::ComboBox::from_id_salt("dovecot_execution")
                        .selected_text(match self.form.profile.dovecot_execution.as_str() {
                            "local" => "Local machine",
                            "ssh" => "Unavailable (secret broker required)",
                            _ => "Automatic (local-only inference)",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.form.profile.dovecot_execution,
                                "local".into(),
                                "Local machine",
                            );
                            ui.selectable_value(
                                &mut self.form.profile.dovecot_execution,
                                "automatic".into(),
                                "Automatic (local-only inference)",
                            );
                        });
                });
                ui.horizontal(|ui| { ui.label("doveadm"); ui.text_edit_singleline(&mut self.form.profile.doveadm_path); });
                ui.horizontal(|ui| { ui.label("Config"); ui.text_edit_singleline(&mut self.form.profile.dovecot_config); });
                ui.label(RichText::new("Remote Dovecot execution is unavailable until a secret-safe broker is implemented. Use local doveadm or imapsync.").size(11.0).color(ALERT));
                ui.label(RichText::new("Dry mode only lists the destination mailbox. A live run uses sync -1; enabling destination deletion switches to backup.").size(11.0).color(MUTED));
                }
            });
            if !editable {
                ui.label(RichText::new("Engine and execution settings are locked while a migration is running.").color(MUTED));
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
        egui::Window::new("Confirm live migration")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading(RichText::new("Destination changes require confirmation").color(ALERT));
                ui.label(format!(
                    "This will invoke {} with the current credentials and rules.",
                    self.form.engine().label()
                ));
                ui.add_space(8.0);
                ui.label(RichText::new(format!("Project: {}", self.form.profile.name)).strong());
                ui.label(format!(
                    "{}  →  {}",
                    self.form.profile.source_host, self.form.profile.destination_host
                ));
                let deletion_enabled = self.form.profile.delete2;
                ui.label("Source mail: not deleted by default");
                ui.label(
                    RichText::new(format!(
                        "Destination deletion: {}",
                        if deletion_enabled {
                            "ENABLED ⚠"
                        } else {
                            "disabled"
                        }
                    ))
                    .color(if deletion_enabled { ALERT } else { MUTED }),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_requested = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("I understand — start migration")
                                    .color(Color32::WHITE),
                            )
                            .fill(ALERT),
                        )
                        .clicked()
                    {
                        close_requested = true;
                        self.live_confirmed = true;
                        self.live_confirmation_plan =
                            Some(plan_fingerprint_digest(&self.form.plan_fingerprint()));
                        self.start();
                    }
                });
            });
        self.live_confirm_open = open && !close_requested;
    }

    fn stop_confirmation(&mut self, ctx: &egui::Context) {
        if !self.stop_confirm_open || !self.running() {
            return;
        }
        let mut open = self.stop_confirm_open;
        let mut close_requested = false;
        egui::Window::new("Stop migration?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading(RichText::new("The migration will stop where it is").color(ALERT));
                ui.label("The destination may be partially migrated. A later preflight, delta, or verification pass may be required before continuing.");
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Keep running").clicked() {
                        close_requested = true;
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("Stop migration").color(Color32::WHITE)).fill(ALERT))
                        .clicked()
                    {
                        if let Some(cancel) = &self.cancel_requested {
                            cancel.store(true, Ordering::Relaxed);
                        }
                        self.status = "Cancellation requested…".into();
                        close_requested = true;
                    }
                });
            });
        self.stop_confirm_open = open && !close_requested;
    }
}

fn markdown_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\n', " ")
}

fn push_visible_output(output: &mut VecDeque<String>, line: String) {
    let line = truncate_utf8(&line, MAX_DIAGNOSTIC_LINE_BYTES);
    while output.len() >= MAX_VISIBLE_OUTPUT_LINES
        || output.iter().map(String::len).sum::<usize>() + line.len() > MAX_VISIBLE_OUTPUT_BYTES
    {
        output.pop_front();
    }
    output.push_back(line);
}

fn truncate_utf8(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn display_job_state(state: &str) -> &'static str {
    match state {
        "imported" => "Imported",
        "queued" => "Queued",
        "preflight" => "Preflight",
        "ready" => "Ready",
        "running" => "Running",
        "retrying" => "Retrying",
        "delta_required" => "Delta required",
        "verification_difference" => "Verification difference",
        "completed" => "Completed",
        "verified_with_exceptions" => "Verified with exceptions",
        "verified" => "Verified",
        "failed" => "Failed",
        "cancelled" => "Cancelled",
        "attention" => "Attention",
        _ => "Unknown",
    }
}

fn display_state_key(state: &str) -> String {
    let normalized = state.to_ascii_lowercase().replace(' ', "_");
    core::MailboxState::parse(&normalized).map_or(normalized, |state| state.as_str().to_owned())
}

fn job_state_badge(state: &str, colors: ThemeColors) -> (&'static str, Color32) {
    match state {
        "imported" => ("○ Imported", colors.text_secondary),
        "verified_with_exceptions" => ("✓ Verified with exceptions", colors.warning),
        "verified" => ("✓ Verified", colors.success),
        "completed" => ("✓ Completed", colors.success),
        "running" => ("● Running", colors.info),
        "queued" => ("○ Queued", colors.text_secondary),
        "preflight" => ("◌ Preflight", colors.info),
        "retrying" => ("↻ Retrying", colors.warning),
        "ready" => ("○ Ready", colors.text_secondary),
        "delta_required" => ("↻ Delta required", colors.warning),
        "verification_difference" => ("≠ Verification difference", colors.warning),
        "attention" => ("! Attention", colors.warning),
        "failed" => ("× Failed", colors.danger),
        "cancelled" => ("× Cancelled", colors.danger),
        _ => ("? Unknown", colors.danger),
    }
}

fn needs_operator_review(state: &str) -> bool {
    core::MailboxState::parse(state).is_some_and(core::MailboxState::needs_operator_review)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatusSeverity {
    Info,
    Success,
    Warning,
    Error,
}

fn status_severity(status: &str) -> StatusSeverity {
    let normalized = status.to_ascii_lowercase();
    if [
        "failed",
        "could not",
        "error",
        "unavailable",
        "rejected",
        "requires durability",
        "not started",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        return StatusSeverity::Error;
    }
    if [
        "attention",
        "review",
        "cancellation",
        "cancelled",
        "warning",
        "blocked",
        "omitted",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        return StatusSeverity::Warning;
    }
    if [
        "completed",
        "passed",
        "saved",
        "acknowledged",
        "ready",
        "available",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        return StatusSeverity::Success;
    }
    StatusSeverity::Info
}

fn status_color(status: &str, colors: ThemeColors) -> Color32 {
    match status_severity(status) {
        StatusSeverity::Info => colors.info,
        StatusSeverity::Success => colors.success,
        StatusSeverity::Warning => colors.warning,
        StatusSeverity::Error => colors.danger,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailureClass {
    Cancellation,
    Authentication,
    Quota,
    Capacity,
    Transport,
    Configuration,
    Message,
    Verification,
    Unknown,
}

impl FailureClass {
    fn label(self) -> &'static str {
        match self {
            Self::Cancellation => "cancelled",
            Self::Authentication => "authentication",
            Self::Quota => "quota",
            Self::Capacity => "capacity",
            Self::Transport => "transport",
            Self::Configuration => "configuration",
            Self::Message => "message",
            Self::Verification => "verification",
            Self::Unknown => "unknown",
        }
    }

    fn attention_reason(self) -> core::AttentionReason {
        match self {
            Self::Cancellation => core::AttentionReason::Interrupted,
            Self::Authentication => core::AttentionReason::AuthenticationFailed,
            Self::Quota => core::AttentionReason::CapacityLimited,
            Self::Capacity => core::AttentionReason::CapacityLimited,
            Self::Transport => core::AttentionReason::TransportFailed,
            Self::Configuration => core::AttentionReason::ConfigurationInvalid,
            Self::Message => core::AttentionReason::MessageRejected,
            Self::Verification => core::AttentionReason::VerificationIncomplete,
            Self::Unknown => core::AttentionReason::Unknown,
        }
    }

    fn parse_label(label: &str) -> Option<Self> {
        Some(match label {
            "cancelled" => Self::Cancellation,
            "authentication" => Self::Authentication,
            "quota" => Self::Quota,
            "capacity" => Self::Capacity,
            "transport" => Self::Transport,
            "configuration" => Self::Configuration,
            "message" => Self::Message,
            "verification" => Self::Verification,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

fn project_health_state_counts(jobs: &[core::MailboxJob]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for job in jobs {
        *counts.entry(job.state.clone()).or_insert(0) += 1;
    }
    counts
}

fn classify_failure(error: &str) -> FailureClass {
    // Controller-generated failures carry a stable machine-readable class.
    // Prefer it over the diagnostic prose: retry and terminal-state policy
    // must not change because an engine happened to mention another keyword.
    if let Some(class) = error
        .strip_prefix("[attention_reason=")
        .and_then(|value| value.split_once("] [class="))
        .and_then(|(_, value)| value.split_once(']'))
        .and_then(|(label, _)| FailureClass::parse_label(label))
    {
        return class;
    }
    let error = error.to_ascii_lowercase();
    if ["cancelled", "canceled", "operator cancellation"]
        .iter()
        .any(|marker| error.contains(marker))
    {
        FailureClass::Cancellation
    } else if [
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
        "rate limit",
        "rate-limit",
        "throttl",
        "too many connections",
        "too many requests",
        "server busy",
        "temporarily overloaded",
        "429",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Capacity
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
        "verification",
        "evidence incomplete",
        "evidence unavailable",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Verification
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
    matches!(
        classify_failure(error),
        FailureClass::Transport | FailureClass::Capacity
    )
}

fn transient_retry_delay(error: &str, attempt: usize) -> Duration {
    let base_seconds = if classify_failure(error) == FailureClass::Capacity {
        5
    } else {
        1
    };
    let multiplier = 1_u64 << attempt.min(5);
    Duration::from_secs((base_seconds * multiplier).min(120))
}

fn classified_failure_detail(error: &str) -> String {
    let class = classify_failure(error);
    format!(
        "[attention_reason={}] [class={}] {error}",
        class.attention_reason().as_str(),
        class.label()
    )
}

fn write_private_atomic(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        restrict_file_permissions(&temporary)?;
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
        // Keep operator-facing tables, status text, and logs readable on a
        // migration workstation. This is a default scale, not a substitute
        // for a future persisted Appearance preference.
        ctx.set_zoom_factor(self.ui_scale);
        self.poll();
        let colors = self.theme_colors();
        let plan_controls_enabled = !self.running() && !self.workspace_read_only;
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("plan_controls_enabled"),
                plan_controls_enabled,
            );
        });
        let mut v = if self.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        v.panel_fill = colors.panel;
        v.window_fill = colors.window;
        v.widgets.active.bg_fill = colors.info;
        v.widgets.hovered.bg_fill = colors.selection;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colors.border);
        ctx.set_visuals(v);
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(colors.window)
                    .inner_margin(egui::Margin::symmetric(24, 15)),
            )
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("MAILSWIFTSYNC")
                            .strong()
                            .size(23.0)
                            .color(colors.text_primary),
                    );
                    ui.label(
                        RichText::new("mailbox migration control plane")
                            .italics()
                            .color(colors.link),
                    );
                    let projects = self.ui_projects.iter().take(8).cloned().collect::<Vec<_>>();
                    if !projects.is_empty() || self.selected_project_id.is_some() {
                        let selected_name = self
                            .selected_project_id
                            .as_deref()
                            .and_then(|id| projects.iter().find(|project| project.id == id))
                            .map(|project| project.name.clone())
                            .unwrap_or_else(|| "Select project".into());
                        ui.add_enabled_ui(!self.running(), |ui| {
                            egui::ComboBox::from_id_salt("project_switcher")
                                .selected_text(selected_name)
                                .width(180.0)
                                .show_ui(ui, |ui| {
                                    for project in projects {
                                        let selected = self.selected_project_id.as_deref()
                                            == Some(project.id.as_str());
                                        if ui
                                            .selectable_label(
                                                selected,
                                                format!(
                                                    "{} · {}",
                                                    project.name,
                                                    format_phase_name(project.phase)
                                                ),
                                            )
                                            .clicked()
                                        {
                                            self.select_workspace_project(project.id.clone());
                                        }
                                    }
                                });
                        });
                    }
                    if ui.button("⚙ Settings").clicked() {
                        self.settings_open = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.running() {
                            ui.add(egui::Spinner::new());
                            if let Some(started) = self.run_started_at {
                                ui.label(format!("elapsed {}", format_elapsed(started.elapsed())));
                            }
                        }
                        ui.label(
                            RichText::new(&self.status).color(status_color(&self.status, colors)),
                        );
                        ui.separator();
                        ui.label(
                            RichText::new(if self.workspace_read_only {
                                "HISTORICAL VIEW"
                            } else if self.form.dry_run {
                                "PREFLIGHT"
                            } else {
                                "LIVE MIGRATION"
                            })
                            .strong()
                            .color(if self.workspace_read_only {
                                colors.info
                            } else if self.form.dry_run {
                                colors.success
                            } else {
                                colors.danger
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
                    .fill(colors.panel)
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("WORKSPACE")
                        .size(11.0)
                        .strong()
                        .color(colors.text_secondary),
                );
                ui.add_space(6.0);
                for (view, label) in [
                    (WorkspaceView::Overview, "Overview"),
                    (WorkspaceView::Plan, "Migration plan"),
                    (WorkspaceView::Mailboxes, "Mailboxes"),
                    (WorkspaceView::Activity, "Activity"),
                    (WorkspaceView::Verification, "Verification"),
                ] {
                    let selected = self.active_view == view;
                    if ui
                        .add_sized(
                            [ui.available_width(), 32.0],
                            egui::Button::selectable(selected, RichText::new(label).strong()),
                        )
                        .clicked()
                    {
                        self.active_view = view;
                    }
                    ui.add_space(4.0);
                }
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(colors.background)
                    .inner_margin(egui::Margin::same(24)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.project_summary(ui);
                        if self.active_view == WorkspaceView::Plan {
                        ui.add_enabled_ui(plan_controls_enabled, |ui| {
                            ui.add_space(14.0);
                            ui.heading("Migration plan");
                            ui.label(RichText::new("Set up the connection, run preflight, then deliberately promote this project through each migration phase.").color(MUTED));
                            ui.add_space(14.0);
                            ui.horizontal(|ui| {
                                ui.label("Project name");
                                ui.text_edit_singleline(&mut self.form.profile.name);
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| if ui.button("Save non-secret profile").clicked() { self.status = match self.form.save() { Ok(()) => "Profile saved; passwords were not saved".into(), Err(e) => format!("Could not save profile: {e}") }; });
                            });
                            ui.add_space(10.0);
                            let destination_password_required =
                                self.form.engine() != core::Engine::Dovecot;
                            if ui.available_width() > 900.0 {
                                ui.columns(2, |c| {
                                    Self::account(&mut c[0], "01  SOURCE MAILBOX", &mut self.form.profile.source_host, &mut self.form.profile.source_user, &mut self.form.source_password, true, !self.form.profile.source_credential_id.trim().is_empty(), BLUE);
                                    Self::account(&mut c[1], "02  DESTINATION MAILBOX", &mut self.form.profile.destination_host, &mut self.form.profile.destination_user, &mut self.form.destination_password, destination_password_required, !self.form.profile.destination_credential_id.trim().is_empty(), TEAL);
                                });
                            } else {
                                Self::account(ui, "01  SOURCE MAILBOX", &mut self.form.profile.source_host, &mut self.form.profile.source_user, &mut self.form.source_password, true, !self.form.profile.source_credential_id.trim().is_empty(), BLUE);
                                ui.add_space(8.0);
                                Self::account(ui, "02  DESTINATION MAILBOX", &mut self.form.profile.destination_host, &mut self.form.profile.destination_user, &mut self.form.destination_password, destination_password_required, !self.form.profile.destination_credential_id.trim().is_empty(), TEAL);
                            }
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Source port");
                                ui.add(egui::TextEdit::singleline(&mut self.form.profile.source_port).desired_width(70.0));
                                ui.label("TLS");
                                egui::ComboBox::from_id_salt("source_tls").selected_text(&self.form.profile.source_tls).show_ui(ui, |ui| for mode in ["imaps", "starttls", "plain"] { ui.selectable_value(&mut self.form.profile.source_tls, mode.into(), mode); });
                                ui.separator();
                                ui.label("Destination port");
                                ui.add(egui::TextEdit::singleline(&mut self.form.profile.destination_port).desired_width(70.0));
                                ui.label("TLS");
                                egui::ComboBox::from_id_salt("destination_tls").selected_text(&self.form.profile.destination_tls).show_ui(ui, |ui| for mode in ["imaps", "starttls"] { ui.selectable_value(&mut self.form.profile.destination_tls, mode.into(), mode); });
                            });
                            ui.collapsing("Enterprise certificate trust (optional)", |ui| {
                                ui.label(RichText::new("Use a PEM CA bundle for private PKI, or pin the leaf certificate's SHA-256 fingerprint. Public/system roots remain enabled.").color(MUTED));
                                ui.horizontal(|ui| {
                                    ui.label("Source CA bundle");
                                    ui.add(egui::TextEdit::singleline(&mut self.form.profile.source_ca_bundle).desired_width(300.0).hint_text("/path/to/company-ca.pem"));
                                    ui.label("SHA-256 pin");
                                    ui.add(egui::TextEdit::singleline(&mut self.form.profile.source_certificate_pin_sha256).desired_width(300.0).hint_text("64 hex characters"));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Destination CA bundle");
                                    ui.add(egui::TextEdit::singleline(&mut self.form.profile.destination_ca_bundle).desired_width(300.0).hint_text("/path/to/company-ca.pem"));
                                    ui.label("SHA-256 pin");
                                    ui.add(egui::TextEdit::singleline(&mut self.form.profile.destination_certificate_pin_sha256).desired_width(300.0).hint_text("64 hex characters"));
                                });
                                ui.label(RichText::new("Pins are checked in the authenticated readiness probe; a mismatch blocks execution. Do not use a pin as a substitute for an approved CA unless your security policy explicitly permits it.").color(MUTED));
                            });
                            ui.add_space(14.0);
                            ui.group(|ui| {
                                ui.heading("03  SYNC RULES");
                                ui.label(RichText::new("Execution mode").strong());
                                ui.horizontal(|ui| {
                                    ui.selectable_value(&mut self.form.dry_run, true, "Preflight")
                                        .on_hover_text("Authenticate and validate the plan without intentionally changing the destination.");
                                    ui.selectable_value(&mut self.form.dry_run, false, "Live migration")
                                        .on_hover_text("Run the selected migration and allow destination changes.");
                                });
                                ui.label(RichText::new(if self.form.dry_run {
                                    "Preflight checks access and mapping without intentionally changing the destination."
                                } else {
                                    "Live migration is enabled; review the destination and deletion warning before starting."
                                }).color(if self.form.dry_run { MUTED } else { ALERT }));
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut self.form.profile.automap, "Map standard folders automatically");
                                    ui.checkbox(&mut self.form.profile.justfolders, "Folders only");
                                    ui.checkbox(&mut self.form.profile.addheader, "Add Message-ID header when needed");
                                });
                                ui.horizontal(|ui| { ui.label("Extra imapsync options"); ui.text_edit_singleline(&mut self.form.profile.extra_options); });
                                ui.horizontal(|ui| { ui.label("imapsync executable"); ui.text_edit_singleline(&mut self.form.profile.imapsync_path); });
                            });
                            if self.form.profile.delete2 {
                                ui.group(|ui| {
                                    ui.label(RichText::new("⚠ DESTINATION DELETION ENABLED").strong().color(ALERT));
                                    ui.label(RichText::new("Messages that exist only on the destination may be removed during live migration.").color(ALERT));
                                });
                            }
                        });
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui.button("Preview redacted command").clicked() { self.preview = true; }
                            if self.running() {
                                if ui.button("Stop migration").clicked() { self.stop_confirm_open = true; }
                            } else {
                                let label = if self.form.dry_run { "Run preflight  →" } else { "Start live migration  →" };
                                if ui.add_enabled(true, egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(if self.form.dry_run { BLUE } else { ALERT })).clicked() { self.start(); }
                            }
                            if !self.form.dry_run && !self.running() { ui.label(RichText::new("Live migration can add mail to the destination. Review readiness before continuing.").color(ALERT)); }
                        });
                        ui.add_space(14.0);
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.heading("Execution journal");
                                ui.label(RichText::new(if self.running() { "streaming output" } else { "waiting" }).color(MUTED));
                                if ui.button("Copy output").clicked() {
                                    ui.ctx().copy_text(self.output.iter().cloned().collect::<Vec<_>>().join("\n"));
                                }
                            });
                            egui::ScrollArea::vertical()
                                .hscroll(true)
                                .stick_to_bottom(true)
                                .max_height(180.0)
                                .show_rows(ui, 20.0, self.output.len(), |ui, rows| {
                                    for index in rows {
                                        if let Some(line) = self.output.get(index) {
                                            ui.add(egui::Label::new(RichText::new(line).monospace().size(14.0)).wrap_mode(egui::TextWrapMode::Extend));
                                        }
                                    }
                                });
                        });
                        ui.add_space(8.0);
                        ui.label(RichText::new("Passwords never enter the saved profile. The selected engine receives credentials only for the active process; local process visibility still matters.").size(11.0).color(MUTED));
                        }
                    });
            });
        self.preview(ctx);
        self.bulk_dialog(ctx);
        self.bulk_clear_confirmation(ctx);
        self.bulk_import_confirmation(ctx);
        self.bulk_live_confirmation(ctx);
        self.settings_dialog(ctx);
        self.projects_dialog(ctx);
        self.keyring_dialog(ctx);
        self.advanced_dialog(ctx);
        self.engine_dialog(ctx);
        self.live_confirmation(ctx);
        self.stop_confirmation(ctx);
        if self.running()
            || self.capability_receiver.is_some()
            || self.live_auth_receiver.is_some()
            || self.bulk_import_receiver.is_some()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

fn main() -> eframe::Result<()> {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let Some(command) = arguments.next() else {
        return eframe::run_native(
            "MailSwiftSync",
            eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_inner_size([1200.0, 820.0])
                    .with_min_inner_size([900.0, 640.0]),
                ..Default::default()
            },
            Box::new(|_| Ok(Box::<App>::default())),
        );
    };
    if matches!(command.to_str(), Some("help" | "--help" | "-h")) {
        print_cli_help();
        return Ok(());
    }
    if matches!(command.to_str(), Some("--version" | "-V" | "version")) {
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync --version");
            std::process::exit(2);
        }
        println!("MailSwiftSync {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if command == std::ffi::OsStr::new("verify") {
        let Some(path) = arguments.next() else {
            eprintln!("Usage: mailswiftsync verify <project-report.json> [trusted-public-key-hex]");
            std::process::exit(2);
        };
        let trusted_public_key = arguments.next();
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync verify <project-report.json> [trusted-public-key-hex]");
            std::process::exit(2);
        }
        let trusted_public_key = trusted_public_key
            .as_deref()
            .and_then(|value| value.to_str());
        match verify_proof_file_with_trust(std::path::Path::new(&path), trusted_public_key) {
            Ok(message) => {
                println!("{message}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Migration proof verification failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("sign") {
        let (Some(report), Some(signing_key), key_id) = (
            arguments.next(),
            arguments.next(),
            arguments.next().unwrap_or_else(|| "operator".into()),
        ) else {
            eprintln!(
                "Usage: mailswiftsync sign <project-report.json> <ed25519-pkcs8-key> [key-id]"
            );
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!(
                "Usage: mailswiftsync sign <project-report.json> <ed25519-pkcs8-key> [key-id]"
            );
            std::process::exit(2);
        }
        let report = std::path::PathBuf::from(report);
        let signing_key = std::path::PathBuf::from(signing_key);
        let key_id = key_id.to_string_lossy();
        match sign_proof_file(&report, &signing_key, &key_id) {
            Ok(message) => {
                println!("{message}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Migration proof signing failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("backup") {
        let (Some(source), Some(destination)) = (arguments.next(), arguments.next()) else {
            eprintln!("Usage: mailswiftsync backup <state.db> <backup.db>");
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync backup <state.db> <backup.db>");
            std::process::exit(2);
        }
        let source = std::path::PathBuf::from(source);
        let destination = std::path::PathBuf::from(destination);
        let _lock = match acquire_instance_lock(&source) {
            Ok(lock) => lock,
            Err(error) => {
                eprintln!("Ledger backup refused: {error}");
                std::process::exit(1);
            }
        };
        match core::StateStore::open(&source).and_then(|store| store.backup_to(&destination)) {
            Ok(()) => {
                println!("Created verified ledger backup: {}", destination.display());
                return Ok(());
            }
            Err(error) => {
                eprintln!("Ledger backup failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("restore") {
        let (Some(backup), Some(destination)) = (arguments.next(), arguments.next()) else {
            eprintln!("Usage: mailswiftsync restore <backup.db> <state.db>");
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync restore <backup.db> <state.db>");
            std::process::exit(2);
        }
        let backup = std::path::PathBuf::from(backup);
        let destination = std::path::PathBuf::from(destination);
        let _lock = match acquire_instance_lock(&destination) {
            Ok(lock) => lock,
            Err(error) => {
                eprintln!("Ledger restore refused: {error}");
                std::process::exit(1);
            }
        };
        match restore_ledger(&backup, &destination) {
            Ok(Some(previous)) => {
                println!(
                    "Restored verified ledger to {}; previous ledger preserved at {}.",
                    destination.display(),
                    previous.display()
                );
                return Ok(());
            }
            Ok(None) => {
                println!("Restored verified ledger to {}.", destination.display());
                return Ok(());
            }
            Err(error) => {
                eprintln!("Ledger restore failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("status") {
        let Some(state) = arguments.next() else {
            eprintln!("Usage: mailswiftsync status <state.db> [project-id]");
            std::process::exit(2);
        };
        let project_id = arguments.next();
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync status <state.db> [project-id]");
            std::process::exit(2);
        }
        let state = std::path::PathBuf::from(state);
        let project_id = match project_id.as_deref() {
            Some(value) => match value.to_str() {
                Some(value) => Some(value),
                None => {
                    eprintln!("Status refused: project ID must be valid UTF-8");
                    std::process::exit(2);
                }
            },
            None => None,
        };
        match headless_status(&state, project_id) {
            Ok(status) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&status)
                        .expect("headless status is always serializable")
                );
                return Ok(());
            }
            Err(error) => {
                eprintln!("Could not read migration status: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("recover") {
        let Some(state) = arguments.next() else {
            eprintln!("Usage: mailswiftsync recover <state.db>");
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync recover <state.db>");
            std::process::exit(2);
        }
        let state = std::path::PathBuf::from(state);
        let _lock = match acquire_instance_lock(&state) {
            Ok(lock) => lock,
            Err(error) => {
                eprintln!("Recovery refused: {error}");
                std::process::exit(1);
            }
        };
        match headless_recover(&state) {
            Ok(result) => {
                println!(
                    "Recovered {} job(s); preserved {} unverified process identity(ies).",
                    result.recovered_jobs, result.preserved_processes
                );
                if result.preserved_processes > 0 {
                    eprintln!(
                        "WARNING: process ownership could not be proven for every recorded engine; no unverified process was signalled."
                    );
                }
                return Ok(());
            }
            Err(error) => {
                eprintln!("Migration recovery failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("support-bundle") {
        let (Some(state), Some(output)) = (arguments.next(), arguments.next()) else {
            eprintln!("Usage: mailswiftsync support-bundle <state.db> <output.json>");
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync support-bundle <state.db> <output.json>");
            std::process::exit(2);
        }
        let state = std::path::PathBuf::from(state);
        let output = std::path::PathBuf::from(output);
        let _lock = match acquire_instance_lock(&state) {
            Ok(lock) => lock,
            Err(error) => {
                eprintln!("Support-bundle export refused: {error}");
                std::process::exit(1);
            }
        };
        match export_support_bundle(&state, &output) {
            Ok(()) => {
                println!("Created sanitized support bundle: {}", output.display());
                return Ok(());
            }
            Err(error) => {
                eprintln!("Support-bundle export failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("customer-proof") {
        let (Some(state), Some(output)) = (arguments.next(), arguments.next()) else {
            eprintln!("Usage: mailswiftsync customer-proof <state.db> <output.json>");
            std::process::exit(2);
        };
        let project_id = arguments.next();
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync customer-proof <state.db> <output.json> [project-id]");
            std::process::exit(2);
        }
        let state = std::path::PathBuf::from(state);
        let output = std::path::PathBuf::from(output);
        let store = match core::StateStore::open_readonly(&state) {
            Ok(store) => store,
            Err(error) => {
                eprintln!(
                    "Customer-proof export refused: durable SQLite state is unavailable: {error}"
                );
                std::process::exit(1);
            }
        };
        let project_id = match project_id {
            Some(project_id) => match project_id.to_str() {
                Some(project_id) => Some(project_id.to_owned()),
                None => {
                    eprintln!("Customer-proof export refused: project ID must be valid UTF-8");
                    std::process::exit(2);
                }
            },
            None => match store.latest_project() {
                Ok(project) => project.map(|project| project.id),
                Err(error) => {
                    eprintln!(
                        "Customer-proof export refused: could not select latest project: {error}"
                    );
                    std::process::exit(1);
                }
            },
        };
        let Some(project_id) = project_id else {
            eprintln!("Customer-proof export refused: no durable migration project is available");
            std::process::exit(1);
        };
        match reports::customer::export_from_store(&store, &project_id, &output) {
            Ok(()) => {
                println!("Created customer migration proof: {}", output.display());
                return Ok(());
            }
            Err(error) => {
                eprintln!("Customer-proof export failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("supervise") {
        let Some(state) = arguments.next() else {
            eprintln!("Usage: mailswiftsync supervise <state.db> [poll-seconds] [idle-polls]");
            std::process::exit(2);
        };
        let poll_seconds = match arguments.next() {
            Some(value) => value
                .to_str()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0),
            None => 30,
        };
        let idle_polls = match arguments.next() {
            Some(value) => value.to_str().and_then(|value| value.parse::<usize>().ok()),
            None => Some(1),
        };
        if arguments.next().is_some()
            || !(1..=3_600).contains(&poll_seconds)
            || idle_polls.is_none()
        {
            eprintln!(
                "Usage: mailswiftsync supervise <state.db> [poll-seconds 1..3600] [idle-polls; 0 means continuous]"
            );
            std::process::exit(2);
        }
        let state = std::path::PathBuf::from(state);
        match headless_supervise(
            &state,
            Duration::from_secs(poll_seconds),
            idle_polls.expect("idle-poll count was validated above"),
        ) {
            Ok(message) => {
                println!("{message}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Migration supervisor stopped: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("headless") {
        let (Some(state), Some(mode)) = (arguments.next(), arguments.next()) else {
            eprintln!(
                "Usage: mailswiftsync headless <state.db> preflight|live|batch-preflight|batch-live"
            );
            std::process::exit(2);
        };
        if arguments.next().is_some()
            || !matches!(
                mode.to_str(),
                Some("preflight" | "live" | "batch-preflight" | "batch-live")
            )
        {
            eprintln!(
                "Usage: mailswiftsync headless <state.db> preflight|live|batch-preflight|batch-live"
            );
            std::process::exit(2);
        }
        let state = std::path::PathBuf::from(state);
        let mode = mode.to_string_lossy();
        let result = match mode.as_ref() {
            "preflight" => headless_execute(&state, false),
            "live" => headless_execute(&state, true),
            "batch-preflight" => headless_batch_execute(&state, false),
            "batch-live" => headless_batch_execute(&state, true),
            _ => unreachable!("headless mode was validated above"),
        };
        match result {
            Ok(message) => {
                println!("{message}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Headless migration failed: {error}");
                std::process::exit(1);
            }
        }
    }
    eprintln!(
        "Unknown command. Run `mailswiftsync --help` for available commands; run without a command for the GUI."
    );
    std::process::exit(2);
}

fn print_cli_help() {
    println!(
        "MailSwiftSync — durable, evidence-first mailbox migration control plane\n\n\
Usage:\n  mailswiftsync                 Open the desktop controller\n  mailswiftsync <command>        Run a headless control-plane operation\n\n\
Commands:\n  verify <report> [trusted-key]  Verify report integrity and optional signer trust\n  sign <report> <key> [key-id]   Sign a customer proof with an Ed25519 key\n  backup <state> <backup>        Create an integrity-checked ledger backup\n  restore <backup> <state>       Restore a validated ledger and preserve rollback state\n  status <state> [project-id]    Emit secret-free JSON status\n  recover <state>                Recover interrupted work conservatively\n  support-bundle <state> <out>   Export a sanitized diagnostic bundle\n  customer-proof <state> <out>   Export customer-safe migration evidence\n  supervise <state> [poll] [n]   Run automation-safe supervision\n  headless <state> <mode>        Run preflight/live or batch-preflight/batch-live\n\n\
Options:\n  -h, --help                    Show this help\n  -V, --version                 Show the application version\n\n\
Headless live operations fail nonzero for unresolved verification, delta,\noperator-attention, or durability states."
    );
}

#[derive(Debug, Serialize)]
struct HeadlessStatus {
    schema_version: i64,
    active_processes: Vec<core::ActiveProcess>,
    projects: Vec<HeadlessProjectStatus>,
}

#[derive(Debug, Serialize)]
struct HeadlessProjectStatus {
    id: String,
    name: String,
    source_endpoint: String,
    destination_endpoint: String,
    phase: String,
    mailboxes: Vec<HeadlessMailboxStatus>,
}

#[derive(Debug, Serialize)]
struct HeadlessMailboxStatus {
    id: String,
    source_mailbox: String,
    destination_mailbox: String,
    state: String,
    attention_reason: Option<String>,
}

/// Build a support artifact from durable state without including anything
/// that could disclose credentials, mailbox content, or internal topology.
fn export_support_bundle(
    state_path: &std::path::Path,
    output_path: &std::path::Path,
) -> Result<(), String> {
    let store = core::StateStore::open_readonly(state_path).map_err(|error| error.to_string())?;
    let projects = store
        .recent_projects(1_000)
        .map_err(|error| error.to_string())?;
    let mut project_values = Vec::with_capacity(projects.len());
    for project in projects {
        let jobs = store
            .mailboxes(&project.id)
            .map_err(|error| error.to_string())?;
        let attention_reasons = store
            .mailbox_attention_reasons(&project.id)
            .map_err(|error| error.to_string())?;
        let mailbox_values = jobs
            .iter()
            .map(|job| {
                Ok(serde_json::json!({
                    "id": job.id,
                    "state": job.state,
                    "attention_reason": attention_reasons.get(&job.id).map(|reason| reason.as_str()),
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let runs = store
            .recent_run_list(&project.id, 100)
            .map_err(|error| error.to_string())?;
        let run_values = runs
            .into_iter()
            .map(|run| {
                Ok(serde_json::json!({
                    "run_id": run.id,
                    "job_id": run.job_id,
                    "parent_run_id": run.parent_run_id,
                    "engine": run.engine,
                    "engine_version": store.engine_version(&run.id)
                        .map_err(|error| error.to_string())?,
                    "phase_at_start": run.phase_at_start,
                    "status": run.status,
                    "started_at": run.started_at,
                    "finished_at": run.finished_at,
                    "has_detail": !run.detail.is_empty(),
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
        project_values.push(serde_json::json!({
            "project_id": project.id,
            "project_name": project.name,
            "phase": project.phase.as_str(),
            "mailbox_count": jobs.len(),
            "mailboxes": mailbox_values,
            "recent_runs": run_values,
        }));
    }
    let active_processes = store
        .active_processes()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|process| {
            serde_json::json!({
                "run_id": process.run_id,
                "job_id": process.job_id,
                "pid": process.pid,
            })
        })
        .collect::<Vec<_>>();
    let value = serde_json::json!({
        "format": "mailswiftsync-support-bundle",
        "format_version": 1,
        "application_version": env!("CARGO_PKG_VERSION"),
        "schema_version": core::CURRENT_SCHEMA_VERSION,
        "platform": {
            "os": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
        },
        "active_processes": active_processes,
        "projects": project_values,
        "redaction": {
            "endpoints": "excluded",
            "credentials": "excluded",
            "plan_snapshots": "excluded",
            "command_paths": "excluded",
            "mail_content": "excluded",
            "diagnostic_text": "excluded",
        },
        "note": "This bundle is intended for support and incident triage. It contains durable state classifications and run metadata, not a forensic log or migration proof."
    });
    let report = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    write_private_atomic(output_path, &report).map_err(|error| error.to_string())
}

#[derive(Debug, Serialize)]
struct HeadlessRecoveryResult {
    recovered_jobs: usize,
    preserved_processes: usize,
}

fn headless_status(
    state_path: &std::path::Path,
    selected_project_id: Option<&str>,
) -> Result<HeadlessStatus, String> {
    let store = core::StateStore::open_readonly(state_path).map_err(|error| error.to_string())?;
    let projects = if let Some(project_id) = selected_project_id {
        store
            .project(project_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        store
            .recent_projects(1_000)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|project| core::Project {
                id: project.id,
                name: project.name,
                source_endpoint: project.source_endpoint,
                destination_endpoint: project.destination_endpoint,
                phase: project.phase,
            })
            .collect::<Vec<_>>()
    };
    let mut result = Vec::with_capacity(projects.len());
    for project in projects {
        let attention_reasons = store
            .mailbox_attention_reasons(&project.id)
            .map_err(|error| error.to_string())?;
        let mut mailboxes = Vec::new();
        for mailbox in store
            .mailboxes(&project.id)
            .map_err(|error| error.to_string())?
        {
            mailboxes.push(HeadlessMailboxStatus {
                attention_reason: attention_reasons
                    .get(&mailbox.id)
                    .map(|reason| reason.as_str().to_owned()),
                id: mailbox.id,
                source_mailbox: mailbox.source_mailbox,
                destination_mailbox: mailbox.destination_mailbox,
                state: mailbox.state,
            });
        }
        result.push(HeadlessProjectStatus {
            id: project.id,
            name: project.name,
            source_endpoint: project.source_endpoint,
            destination_endpoint: project.destination_endpoint,
            phase: project.phase.as_str().to_owned(),
            mailboxes,
        });
    }
    let active_processes = store
        .active_processes()
        .map_err(|error| error.to_string())?;
    Ok(HeadlessStatus {
        schema_version: core::CURRENT_SCHEMA_VERSION,
        active_processes,
        projects: result,
    })
}

fn headless_recover(state_path: &std::path::Path) -> Result<HeadlessRecoveryResult, String> {
    let store = core::StateStore::open(state_path).map_err(|error| error.to_string())?;
    let processes = store
        .active_processes()
        .map_err(|error| error.to_string())?;
    let mut unverified = Vec::new();
    for process in &processes {
        if process.pid > 0 && recorded_process_matches(process) {
            terminate_recorded_process_group(process);
        } else {
            unverified.push(process.clone());
        }
    }
    let recovered_jobs = store
        .recover_abandoned_jobs_preserving(&unverified)
        .map_err(|error| error.to_string())?;
    if unverified.is_empty() {
        cleanup_stale_secret_directories(&secret_runtime_base());
    }
    Ok(HeadlessRecoveryResult {
        recovered_jobs,
        preserved_processes: unverified.len(),
    })
}

/// Run the existing durable controller without constructing an egui window.
/// A live invocation always performs a fresh dry preflight first, so this
/// path cannot promote credentials or a plan left over from another process.
fn headless_execute(state_path: &std::path::Path, live: bool) -> Result<String, String> {
    // App::default owns the lock and performs the same startup recovery as
    // the GUI. The environment override is read only during construction.
    unsafe { std::env::set_var("MAILSWIFTSYNC_STATE_PATH", state_path) };
    let mut app = App::default();
    if !app.persistence_available {
        return Err("durable SQLite state is unavailable; execution is blocked".into());
    }
    if !app.profile_available {
        return Err("saved migration profile is unavailable; repair it before execution".into());
    }
    if app.process_review_required {
        return Err(
            "recorded process ownership could not be verified; run `recover` and review the host before retrying"
                .into(),
        );
    }
    if !app.bulk_job_ids.is_empty() {
        return Err(
            "headless single-mailbox execution refuses a restored batch queue; use the GUI for batch admission or an explicit batch supervisor"
                .into(),
        );
    }

    app.form.dry_run = true;
    app.start();
    wait_for_headless_controller(&mut app)?;
    let project_id = app
        .project_id
        .as_deref()
        .ok_or_else(|| format!("preflight did not create a durable mailbox: {}", app.status))?
        .to_owned();
    let job_id = app
        .job_id
        .as_deref()
        .ok_or_else(|| format!("preflight did not create a durable mailbox: {}", app.status))?
        .to_owned();
    let mailbox_count = app
        .store
        .mailboxes(&project_id)
        .map_err(|error| error.to_string())?
        .len();
    if mailbox_count != 1 {
        return Err(format!(
            "headless execution requires exactly one mailbox; durable project contains {mailbox_count}"
        ));
    }
    let preflight_state = app
        .store
        .mailbox_state(&job_id)
        .map_err(|error| error.to_string())?;
    if preflight_state.as_deref() != Some("ready") {
        return Err(format!(
            "preflight did not produce a runnable mailbox (state={:?}, status={})",
            preflight_state, app.status
        ));
    }
    if !live {
        return Ok(format!(
            "Headless preflight completed for project {project_id}, mailbox {job_id}."
        ));
    }

    // This is the explicit command-line live confirmation. The plan and
    // credential fingerprint were captured by the just-completed preflight;
    // start() still revalidates both before launching an engine.
    app.form.dry_run = false;
    app.live_confirmed = true;
    app.live_confirmation_plan = Some(plan_fingerprint_digest(&app.form.plan_fingerprint()));
    app.start();
    wait_for_headless_controller(&mut app)?;
    let final_state = app
        .store
        .mailbox_state(&job_id)
        .map_err(|error| error.to_string())?;
    if !matches!(
        final_state.as_deref(),
        Some("verified") | Some("verified_with_exceptions")
    ) {
        let exit_hint = match final_state.as_deref() {
            Some("delta_required") => "delta required (exit status 3)",
            Some("verification_difference") => "verification difference (exit status 4)",
            Some("attention") => "operator attention (exit status 5)",
            _ => "unresolved (exit status 1)",
        };
        return Err(format!(
            "live migration did not reach a verified terminal state: {exit_hint}; state={:?}, status={}",
            final_state, app.status
        ));
    }
    Ok(format!(
        "Headless live migration completed for project {project_id}, mailbox {job_id}; state={}.",
        final_state.unwrap_or_default()
    ))
}

/// Drive the existing durable batch worker without a GUI. Only queues already
/// restored from the ledger are accepted; importing or silently reconstructing
/// a partial batch in automation would make the scope ambiguous.
fn headless_batch_execute(state_path: &std::path::Path, live: bool) -> Result<String, String> {
    unsafe { std::env::set_var("MAILSWIFTSYNC_STATE_PATH", state_path) };
    let mut app = App::default();
    if !app.persistence_available {
        return Err("durable SQLite state is unavailable; batch execution is blocked".into());
    }
    if app.process_review_required {
        return Err(
            "recorded process ownership could not be verified; run `recover` and review the host before retrying"
                .into(),
        );
    }
    if app.bulk_jobs.is_empty() {
        return Err(
            "no durable batch queue was restored; import and validate the batch in the GUI before using headless batch execution"
                .into(),
        );
    }
    if app.bulk_job_ids.len() != app.bulk_jobs.len() || app.bulk_project_id.is_none() {
        return Err(
            "the restored batch queue has no complete durable identity; refusing ambiguous headless admission"
                .into(),
        );
    }
    app.bulk_retry_scope = BulkRetryScope::Automation;
    app.bulk_selected_ids = app
        .bulk_job_ids
        .iter()
        .filter_map(|job_id| {
            let state = app.store.mailbox_state(job_id).ok().flatten()?;
            let reason = app.store.mailbox_attention_reason(job_id).ok().flatten();
            app.bulk_retry_scope
                .includes_automation(&state, reason)
                .then_some(job_id.clone())
        })
        .collect();
    if app.bulk_selected_ids.is_empty() {
        return Err(
            "no automation-safe batch work is queued; operator-review and verification-difference rows were not retried"
                .into(),
        );
    }
    let (form_dry_run, bulk_dry_run) = headless_batch_mode(false);
    app.form.dry_run = form_dry_run;
    app.bulk_dry_run = bulk_dry_run;
    app.start_bulk();
    wait_for_headless_controller(&mut app)?;
    let project_id = app
        .bulk_project_id
        .as_deref()
        .ok_or_else(|| {
            format!(
                "batch preflight lost its durable project: {}",
                app.bulk_message
            )
        })?
        .to_owned();
    let job_ids = app
        .bulk_job_ids
        .iter()
        .filter(|job_id| app.bulk_selected_ids.contains(*job_id))
        .cloned()
        .collect::<Vec<_>>();
    let states = app
        .store
        .batch_admission_states(&project_id, &job_ids)
        .map_err(|error| error.to_string())?;
    if states.iter().any(|state| state.state != "ready") {
        return Err(format!(
            "batch preflight did not make every mailbox runnable (states: {})",
            states
                .iter()
                .map(|state| format!("{}={}", state.job_id, state.state))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !live {
        return Ok(format!(
            "Headless batch preflight completed for project {project_id}; {} mailbox(es) are ready.",
            job_ids.len()
        ));
    }

    let (form_dry_run, bulk_dry_run) = headless_batch_mode(true);
    app.form.dry_run = form_dry_run;
    app.bulk_dry_run = bulk_dry_run;
    app.bulk_live_confirmed = true;
    app.start_bulk();
    wait_for_headless_controller(&mut app)?;
    let final_mailboxes = app
        .store
        .mailboxes(&project_id)
        .map_err(|error| error.to_string())?;
    let unresolved = final_mailboxes
        .iter()
        .filter(|mailbox| app.bulk_selected_ids.contains(&mailbox.id))
        .filter(|mailbox| {
            matches!(
                mailbox.state.as_str(),
                "failed"
                    | "attention"
                    | "cancelled"
                    | "queued"
                    | "claimed"
                    | "running"
                    | "delta_required"
                    | "verification_difference"
            )
        })
        .map(|mailbox| format!("{}={}", mailbox.id, mailbox.state))
        .collect::<Vec<_>>();
    if !unresolved.is_empty() {
        return Err(format!(
            "headless batch live run left unresolved mailbox(es): {}",
            unresolved.join(", ")
        ));
    }
    Ok(format!(
        "Headless batch live migration completed for project {project_id}; {} mailbox(es) reached verified terminal states.",
        job_ids.len()
    ))
}

/// Keep the legacy single-mailbox form flag and the batch controller mode in
/// sync at the headless boundary. Batch execution is governed by
/// `bulk_dry_run`; changing only `form.dry_run` can rerun preflight and make a
/// requested live invocation report ready rows as successfully migrated.
fn headless_batch_mode(live: bool) -> (bool, bool) {
    let dry_run = !live;
    (dry_run, dry_run)
}

/// Foreground supervisor for an already admitted durable batch. The loop is
/// intentionally conservative: it only selects automation-safe rows and
/// leaves Attention/verification-difference rows for an operator. A zero
/// idle-poll limit keeps watching for work; a non-zero limit makes a one-shot
/// maintenance-window invocation terminate after the requested quiet period.
fn headless_supervise(
    state_path: &std::path::Path,
    poll_interval: Duration,
    max_idle_polls: usize,
) -> Result<String, String> {
    let mut idle_polls = 0_usize;
    let mut completed_passes = 0_usize;
    loop {
        let status = headless_status(state_path, None)?;
        let actionable = status
            .projects
            .iter()
            .filter(|project| {
                project.source_endpoint == "batch" && project.destination_endpoint == "batch"
            })
            .flat_map(|project| project.mailboxes.iter())
            .any(|mailbox| {
                BulkRetryScope::Automation.includes_automation(
                    &mailbox.state,
                    mailbox
                        .attention_reason
                        .as_deref()
                        .and_then(core::AttentionReason::parse),
                )
            });
        if !actionable {
            idle_polls = idle_polls.saturating_add(1);
            if max_idle_polls != 0 && idle_polls >= max_idle_polls {
                return Ok(format!(
                    "Migration supervisor stopped after {idle_polls} idle poll(s); completed {completed_passes} batch pass(es). Operator-review rows were left untouched."
                ));
            }
            thread::sleep(poll_interval);
            continue;
        }
        idle_polls = 0;
        match headless_batch_execute(state_path, true) {
            Ok(message) => {
                completed_passes = completed_passes.saturating_add(1);
                eprintln!("{message}");
            }
            Err(error)
                if error.contains("Another MailSwiftSync instance holds the project database")
                    || error.contains("durable SQLite state is unavailable") =>
            {
                eprintln!("Migration supervisor waiting for durable controller ownership: {error}");
                thread::sleep(poll_interval);
            }
            Err(error) => return Err(error),
        }
    }
}

fn wait_for_headless_controller(app: &mut App) -> Result<(), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(7 * 24 * 60 * 60);
    while app.running() || app.capability_receiver.is_some() || app.live_auth_receiver.is_some() {
        app.poll();
        if std::time::Instant::now() >= deadline {
            if let Some(cancel) = &app.cancel_requested {
                cancel.store(true, Ordering::Relaxed);
            }
            return Err("headless controller wait exceeded seven days".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
    if app.durability_error || app.durability_recovery_pending {
        return Err(format!(
            "durable terminal state was not confirmed: {}",
            app.status
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

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
        assert!(args.windows(2).any(|pair| pair == ["-s", ""]));
        assert!(!args.iter().any(|arg| arg == "secret"));
    }

    #[test]
    fn live_dovecot_plan_uses_previous_checkpoint() {
        let mut form = dovecot_form();
        form.dry_run = false;
        let (_, args) = form.command_with_checkpoint(true, Some("AQAAAHm4+Jk="));
        assert!(args.windows(2).any(|pair| pair == ["-s", "AQAAAHm4+Jk="]));
    }

    #[test]
    fn dovecot_state_candidate_accepts_state_and_rejects_diagnostics() {
        assert_eq!(
            dovecot_state_candidate("AQAAAHm4+Jk="),
            Some("AQAAAHm4+Jk=".into())
        );
        assert!(dovecot_state_candidate("sync completed successfully").is_none());
        assert_eq!(
            dovecot_state_candidate("AQAAAHm4+Jk"),
            Some("AQAAAHm4+Jk".into())
        );
        assert_eq!(
            dovecot_state_candidate("state-token_v2"),
            Some("state-token_v2".into())
        );
        assert!(dovecot_state_candidate("completed").is_none());
        assert!(dovecot_state_candidate(" short ").is_none());
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
    fn report_snapshot_decode_only_allows_empty_legacy_snapshots() {
        assert!(decode_report_run_snapshot("  \n").unwrap().is_none());
        match decode_report_run_snapshot("not a run snapshot") {
            Ok(_) => panic!("malformed report snapshot was accepted"),
            Err(error) => assert!(error.contains("plan snapshot is corrupt")),
        }
    }

    #[test]
    fn evidence_digest_binds_run_plan_and_evidence_values() {
        let evidence = core::MailboxEvidence {
            source_messages: 10,
            destination_messages: 10,
            source_bytes: 100,
            destination_bytes: 100,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 2,
            destination_folders: 2,
            authoritative: true,
        };
        let first = evidence_digest("run-one", "snapshot-one", &evidence);
        assert_eq!(first, evidence_digest("run-one", "snapshot-one", &evidence));
        assert_ne!(first, evidence_digest("run-two", "snapshot-one", &evidence));
        assert_ne!(first, evidence_digest("run-one", "snapshot-two", &evidence));
        let mut changed = evidence.clone();
        changed.destination_messages = 9;
        assert_ne!(first, evidence_digest("run-one", "snapshot-one", &changed));
    }

    #[test]
    fn migration_proof_digest_detects_semantic_tampering() {
        let original = serde_json::json!({
            "format": "mailswiftsync-project-report",
            "format_version": 1,
            "project": { "name": "Example migration" },
            "mailboxes": [],
            "runs": []
        });
        let proof = with_proof_digest(original).unwrap();
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-proof-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("proof.json");
        std::fs::write(&path, serde_json::to_string_pretty(&proof).unwrap()).unwrap();
        assert!(verify_proof_file(&path).unwrap().contains("verified"));

        let mut tampered = proof;
        tampered["project"]["name"] = "Altered migration".into();
        std::fs::write(&path, serde_json::to_string_pretty(&tampered).unwrap()).unwrap();
        assert!(verify_proof_file(&path).is_err());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn customer_proof_format_is_digest_verifiable() {
        let proof = with_proof_digest(serde_json::json!({
            "format": "mailswiftsync-customer-proof",
            "format_version": 1,
            "project": { "name": "Customer migration", "phase": "Verification" },
            "mailboxes": [],
            "runs": [],
            "note": "customer-safe"
        }))
        .unwrap();
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-customer-proof-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("proof.json");
        std::fs::write(&path, serde_json::to_string_pretty(&proof).unwrap()).unwrap();
        assert!(verify_proof_file(&path).unwrap().contains("verified"));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn support_bundle_excludes_topology_and_diagnostic_material() {
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-support-bundle-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let state = directory.join("state.db");
        let output = directory.join("support.json");
        let store = core::StateStore::open(&state).unwrap();
        let project = store
            .create_project("Support fixture", "source.internal", "destination.internal")
            .unwrap();
        store
            .add_mailbox(&project.id, "alice@example.test", "alice@example.test")
            .unwrap();
        drop(store);

        export_support_bundle(&state, &output).unwrap();
        let text = std::fs::read_to_string(&output).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["format"], "mailswiftsync-support-bundle");
        assert!(!text.contains("source.internal"));
        assert!(!text.contains("destination.internal"));
        assert_eq!(value["redaction"]["credentials"], "excluded");
        assert_eq!(value["redaction"]["diagnostic_text"], "excluded");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn signed_migration_proof_requires_valid_signature_and_trust_pin() {
        let original = serde_json::json!({
            "format": "mailswiftsync-project-report",
            "format_version": 2,
            "project": { "name": "Signed migration" },
            "mailboxes": [],
            "runs": []
        });
        let proof = with_proof_digest(original).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-signed-proof-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("proof.json");
        let key_path = directory.join("signing-key.pk8");
        std::fs::write(&path, serde_json::to_string_pretty(&proof).unwrap()).unwrap();
        let key = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .unwrap();
        std::fs::write(&key_path, key.as_ref()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&key_path).unwrap().permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(&key_path, permissions).unwrap();
        }
        sign_proof_file(&path, &key_path, "test-key").unwrap();
        // Re-signing is a supported repair/rotation workflow. The previous
        // signature must not become part of the newly calculated digest.
        sign_proof_file(&path, &key_path, "test-key-rotated").unwrap();
        let signed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let public_key = signed["proof_signature"]["public_key"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            verify_proof_file_with_trust(&path, Some(&public_key))
                .unwrap()
                .contains("signature valid")
        );
        assert!(verify_proof_file_with_trust(&path, Some(&"00".repeat(32))).is_err());
        let mut tampered = signed;
        tampered["project"]["name"] = "Altered migration".into();
        std::fs::write(&path, serde_json::to_string_pretty(&tampered).unwrap()).unwrap();
        assert!(verify_proof_file(&path).is_err());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn preflight_plan_storage_uses_an_opaque_digest() {
        let fingerprint = "imapsync\n--timeout\u{1f}30\ncredential-source1=source-id";
        let digest = plan_fingerprint_digest(fingerprint);
        assert_eq!(digest.len(), 64);
        assert_ne!(digest, fingerprint);
        assert!(!digest.contains("imapsync"));
        assert!(!digest.contains("source-id"));
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
    fn plan_snapshot_records_checkpoint_identity_without_checkpoint_value() {
        let mut form = dovecot_form();
        form.dry_run = false;
        let checkpoint = "AQAAAHm4+Jk=";
        let snapshot = form
            .plan_snapshot_with_checkpoint(Some(checkpoint))
            .unwrap();
        let digest = plan_snapshot_sha256(checkpoint);
        assert!(snapshot.contains("dovecot_checkpoint_sha256"));
        assert!(snapshot.contains(&digest));
        assert!(!snapshot.contains(checkpoint));
    }

    #[test]
    fn dry_or_non_dovecot_snapshots_do_not_claim_checkpoint_input() {
        let mut dovecot = dovecot_form();
        assert!(
            !dovecot
                .plan_snapshot_with_checkpoint(Some("AQAAAHm4+Jk="))
                .unwrap()
                .contains("dovecot_checkpoint_sha256")
        );

        dovecot.dry_run = false;
        dovecot.profile.engine = core::Engine::ImapSync;
        assert!(
            !dovecot
                .plan_snapshot_with_checkpoint(Some("AQAAAHm4+Jk="))
                .unwrap()
                .contains("dovecot_checkpoint_sha256")
        );
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
    fn credential_fingerprint_changes_without_exposing_secret_material() {
        let mut first = dovecot_form();
        first.source_password = Zeroizing::new("source-one".into());
        first.destination_password = Zeroizing::new("destination-one".into());
        let mut second = first.clone();
        second.destination_password = Zeroizing::new("destination-two".into());

        assert_ne!(
            first.credential_fingerprint(),
            second.credential_fingerprint()
        );
        assert!(!first.credential_fingerprint().contains("source-one"));
        assert!(!first.credential_fingerprint().contains("destination-one"));
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
    fn remote_dovecot_execution_is_rejected_without_secret_broker() {
        let mut form = dovecot_form();
        form.profile.dovecot_ssh_user = "migration".into();
        assert!(
            form.prepared_command()
                .err()
                .is_some_and(|error| error.contains("not available"))
        );
        form.profile.allow_remote_password_in_argv = true;
        assert!(
            form.prepared_command()
                .err()
                .is_some_and(|error| error.contains("not available"))
        );
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
    fn validation_rejects_zero_ports_with_leading_zeroes() {
        let mut form = dovecot_form();
        for value in ["0", "00", "000"] {
            form.profile.source_port = value.into();
            assert!(form.validate().unwrap_err().contains("Source IMAP port"));
            form.profile.source_port.clear();
            form.profile.destination_port = value.into();
            assert!(
                form.validate()
                    .unwrap_err()
                    .contains("Destination IMAP port")
            );
            form.profile.destination_port.clear();
        }
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
        form.profile.extra_options = "--nofoldersizes --timeout 30".into();
        assert!(form.validate().is_ok());
        form.profile.extra_options = "--custom-helper /tmp/helper".into();
        let error = form.validate().unwrap_err();
        assert!(error.contains("safe imapsync option allowlist"));
        form.profile.extra_options = "--timeout".into();
        assert!(form.validate().unwrap_err().contains("requires a value"));
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
        assert_eq!(job.state, "imported");
        assert_eq!(
            job_state_badge(&job.state, ThemeColors::dark()).0,
            "○ Imported"
        );
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
    fn bulk_import_rejects_oversized_files_before_parsing() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-limit-{}",
            uuid::Uuid::new_v4()
        ));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_BULK_IMPORT_BYTES + 1).unwrap();
        let error = validate_bulk_import_file(&path).unwrap_err();
        assert!(error.contains("import file"));
        assert!(error.contains("limit"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn batch_selection_export_excludes_secret_material() {
        let mut form = Form::default();
        form.profile.source_host = "source.example".into();
        form.profile.source_user = "source@example".into();
        form.profile.destination_host = "destination.example".into();
        form.profile.destination_user = "destination@example".into();
        form.source_password = "source-secret".to_owned().into();
        form.destination_password = "destination-secret".to_owned().into();
        form.profile.source_credential_id = "source-key".into();
        let jobs = vec![BulkJob {
            label: "mailbox".into(),
            form,
            state: "failed".into(),
        }];
        let value = bulk_selection_value(&jobs, &HashSet::new(), &[]);
        let text = serde_json::to_string(&value).unwrap();
        assert!(text.contains("source@example"));
        assert!(text.contains("failed"));
        assert!(!text.contains("source-secret"));
        assert!(!text.contains("destination-secret"));
        assert!(!text.contains("source-key"));
        assert!(!text.contains("extra_options"));
    }

    #[test]
    fn corrupt_or_missing_persisted_batch_plan_fails_closed() {
        let missing = match decode_persisted_batch_profile(None, "job-1") {
            Ok(_) => panic!("missing persisted plan must be rejected"),
            Err(error) => error,
        };
        assert!(missing.contains("job-1"));
        assert!(missing.contains("no migration plan"));

        let corrupt = match decode_persisted_batch_profile(Some("not = valid = toml"), "job-2") {
            Ok(_) => panic!("corrupt persisted plan must be rejected"),
            Err(error) => error,
        };
        assert!(corrupt.contains("job-2"));
        assert!(corrupt.contains("is corrupt"));
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
    fn ledger_restore_validates_copy_and_preserves_previous_state() {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-restore-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let source = directory.join("backup.db");
        let destination = directory.join("state.db");
        core::StateStore::in_memory()
            .unwrap()
            .backup_to(&source)
            .unwrap();

        assert!(restore_ledger(&source, &destination).unwrap().is_none());
        core::StateStore::open_readonly(&destination).unwrap();
        std::fs::remove_file(&destination).unwrap();
        let orphaned_sidecar = PathBuf::from(format!("{}-wal", destination.display()));
        std::fs::write(&orphaned_sidecar, b"orphaned sqlite sidecar").unwrap();
        assert!(restore_ledger(&source, &destination).is_err());
        std::fs::remove_file(orphaned_sidecar).unwrap();
        assert!(restore_ledger(&source, &destination).unwrap().is_none());

        core::StateStore::in_memory()
            .unwrap()
            .backup_to(&destination.with_extension("replacement.db"))
            .unwrap();
        for suffix in ["-wal", "-shm"] {
            std::fs::write(
                format!("{}{}", destination.display(), suffix),
                b"old sqlite sidecar",
            )
            .unwrap();
        }
        let previous = restore_ledger(&source, &destination).unwrap().unwrap();
        core::StateStore::open_readonly(&previous).unwrap();
        core::StateStore::open_readonly(&destination).unwrap();
        for suffix in ["-wal", "-shm"] {
            assert!(std::path::Path::new(&format!("{}{}", previous.display(), suffix)).exists());
            assert!(
                !std::path::Path::new(&format!("{}{}", destination.display(), suffix)).exists()
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn batch_retry_classifier_excludes_authentication_failures() {
        assert!(is_transient_batch_error("connection reset by peer"));
        assert!(is_transient_batch_error("operation timed out"));
        assert!(is_transient_batch_error("server returned 429 rate limit"));
        assert!(!is_transient_batch_error("mailbox is full: OVERQUOTA"));
        assert!(!is_transient_batch_error("IMAP authentication failed"));
        assert!(!is_transient_batch_error(
            "invalid destination configuration"
        ));
    }

    #[test]
    fn capacity_retries_back_off_longer_than_transport_retries() {
        assert_eq!(
            classify_failure("too many connections"),
            FailureClass::Capacity
        );
        assert_eq!(
            transient_retry_delay("connection reset by peer", 0),
            Duration::from_secs(1)
        );
        assert_eq!(
            transient_retry_delay("server busy", 0),
            Duration::from_secs(5)
        );
        assert_eq!(
            transient_retry_delay("server busy", 99),
            Duration::from_secs(120)
        );
        assert_eq!(
            classified_failure_detail("too many requests"),
            "[attention_reason=capacity_limited] [class=capacity] too many requests"
        );
    }

    #[test]
    fn batch_retry_scopes_select_only_the_intended_durable_states() {
        let states = [
            "ready",
            "failed",
            "attention",
            "delta_required",
            "verification_difference",
            "completed",
            "verified",
            "verified_with_exceptions",
        ];
        assert_eq!(
            states
                .iter()
                .filter(|state| BulkRetryScope::Unresolved.includes(state))
                .copied()
                .collect::<Vec<_>>(),
            vec!["ready", "failed", "delta_required", "completed",]
        );
        assert_eq!(
            states
                .iter()
                .filter(|state| BulkRetryScope::FailedAttention.includes(state))
                .copied()
                .collect::<Vec<_>>(),
            vec!["failed", "attention"]
        );
        assert_eq!(
            states
                .iter()
                .filter(|state| BulkRetryScope::DeltaRequired.includes(state))
                .copied()
                .collect::<Vec<_>>(),
            vec!["delta_required"]
        );
        assert_eq!(
            states
                .iter()
                .filter(|state| BulkRetryScope::VerificationDifference.includes(state))
                .copied()
                .collect::<Vec<_>>(),
            vec!["verification_difference"]
        );
        assert_eq!(
            states
                .iter()
                .filter(|state| BulkRetryScope::All.includes(state))
                .count(),
            states.len()
        );
    }

    #[test]
    fn failure_taxonomy_keeps_operator_actions_distinct() {
        assert_eq!(
            classify_failure("cancelled by operator before launch"),
            FailureClass::Cancellation
        );
        assert_eq!(
            classify_failure("verification evidence is incomplete"),
            FailureClass::Verification
        );
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
        assert_eq!(
            classified_failure_detail("OVERQUOTA"),
            "[attention_reason=capacity_limited] [class=quota] OVERQUOTA"
        );
        assert_eq!(
            classify_failure(
                "[attention_reason=transport_failed] [class=transport] authentication failed"
            ),
            FailureClass::Transport
        );
        assert_eq!(
            classify_failure("[attention_reason=unknown] [class=not-a-class] quota exceeded"),
            FailureClass::Quota
        );
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
        assert!(duplicate_bulk_destination(&jobs).unwrap().is_some());
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
        assert!(duplicate_bulk_destination(&jobs).unwrap().is_some());
    }

    #[test]
    fn duplicate_destination_identity_uses_transport_and_explicit_port() {
        let mut first = Form::default();
        first.profile.destination_host = "mail.example".into();
        first.profile.destination_user = "user@example".into();
        first.profile.destination_tls = "starttls".into();
        let mut second = first.clone();
        second.profile.destination_port = "1993".into();
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
        assert!(duplicate_bulk_destination(&jobs).unwrap().is_none());
        assert_eq!(
            canonical_destination_identity(&jobs[0].form.profile).unwrap(),
            "endpoint:mail.example:143:user@example"
        );
    }

    #[test]
    fn duplicate_destination_identity_fails_closed_on_malformed_endpoint() {
        let mut form = Form::default();
        form.profile.destination_host = "mail.example:not-a-port".into();
        form.profile.destination_user = "user@example".into();
        let jobs = vec![BulkJob {
            label: "invalid".into(),
            form,
            state: "Ready".into(),
        }];
        assert!(duplicate_bulk_destination(&jobs).is_err());
    }

    #[test]
    fn duplicate_destination_identity_canonicalizes_hosts_but_preserves_mailbox_case() {
        let mut first = Form::default();
        first.profile.destination_host = "MAIL.EXAMPLE.".into();
        first.profile.destination_user = "User@example".into();
        let mut second = first.clone();
        second.profile.destination_host = "mail.example".into();
        second.profile.destination_user = "user@example".into();
        assert_ne!(
            canonical_destination_identity(&first.profile).unwrap(),
            canonical_destination_identity(&second.profile).unwrap()
        );
        assert_eq!(
            canonical_destination_identity(&first.profile).unwrap(),
            "endpoint:mail.example:993:User@example"
        );
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
    fn active_run_project_takes_precedence_over_loaded_projects() {
        assert_eq!(
            preferred_project_id(
                Some("active-batch"),
                Some("selected"),
                Some("single"),
                Some("batch"),
            ),
            Some("active-batch")
        );
        assert_eq!(
            preferred_project_id(None, Some("selected"), Some("single"), Some("batch")),
            Some("selected")
        );
        assert_eq!(
            preferred_project_id(None, None, Some("single"), Some("batch")),
            Some("batch")
        );
    }

    #[test]
    fn active_run_context_rejects_foreign_process_events() {
        let context = ActiveRunContext {
            run_id: "parent".into(),
            project_id: "project".into(),
            job_id: None,
            batch_job_ids: vec!["job-a".into(), "job-b".into()],
            batch_child_run_ids: vec!["child-a".into(), "child-b".into()],
            batch_child_indices: HashMap::from([("child-a".into(), 0), ("child-b".into(), 1)]),
            batch_plan_fingerprints: vec!["plan-a".into(), "plan-b".into()],
            kind: RunKind::Batch,
            dry_run: true,
            engine: core::Engine::ImapSync,
            plan_fingerprint: String::new(),
            credential_fingerprint: String::new(),
        };
        assert!(context.owns_process("child-a", "job-a"));
        assert!(context.owns_process("child-b", "job-b"));
        assert!(context.owns_batch_child("parent", "child-a", "job-a"));
        assert!(!context.owns_process("child-a", "job-b"));
        assert!(!context.owns_process("foreign-child", "job-a"));
        assert!(!context.owns_process("parent", "job-a"));
        assert!(!context.owns_batch_child("other-parent", "child-a", "job-a"));
        assert!(!context.owns_batch_child("parent", "child-a", "job-b"));
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

    #[test]
    fn diagnostic_buffers_truncate_utf8_and_bound_bytes() {
        let mut output = VecDeque::new();
        push_visible_output(&mut output, "é".repeat(MAX_DIAGNOSTIC_LINE_BYTES + 1));
        assert!(output.front().unwrap().len() <= MAX_DIAGNOSTIC_LINE_BYTES);
        assert!(
            output
                .front()
                .unwrap()
                .is_char_boundary(output.front().unwrap().len())
        );

        let tail = Mutex::new(VecDeque::new());
        for _ in 0..100 {
            record_process_tail(&tail, &"x".repeat(MAX_DIAGNOSTIC_LINE_BYTES));
        }
        let tail = tail.lock().unwrap();
        assert!(tail.iter().map(String::len).sum::<usize>() <= MAX_PROCESS_TAIL_BYTES);
        assert!(tail.len() <= MAX_PROCESS_TAIL_LINES);
    }

    #[cfg(unix)]
    #[test]
    fn live_dovecot_exit_code_two_is_a_delta_outcome() {
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let acknowledger = thread::spawn(move || {
            while let Ok(event) = rx.recv() {
                if let Event::ProcessStarted(_, _, _, _, _, _, _, reply) = event {
                    let _ = reply.send(Ok(()));
                }
            }
        });
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
        drop(tx);
        acknowledger.join().unwrap();
        assert_eq!(outcome.outcome, StreamOutcome::DeltaRequired);
    }

    #[cfg(unix)]
    #[test]
    fn streaming_captures_bounded_imapsync_evidence_without_full_log() {
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let acknowledger = thread::spawn(move || {
            while let Ok(event) = rx.recv() {
                if let Event::ProcessStarted(_, _, _, _, _, _, _, reply) = event {
                    let _ = reply.send(Ok(()));
                }
            }
        });
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
        drop(tx);
        acknowledger.join().unwrap();
        assert_eq!(result.outcome, StreamOutcome::Completed);
        let evidence = result.imapsync_evidence.unwrap();
        assert_eq!(evidence.source_messages, 7);
        assert_eq!(evidence.destination_messages, 7);
        assert!(evidence.authoritative);
    }

    #[cfg(unix)]
    #[test]
    fn dry_dovecot_exit_code_two_is_not_a_delta_outcome() {
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let acknowledger = thread::spawn(move || {
            while let Ok(event) = rx.recv() {
                if let Event::ProcessStarted(_, _, _, _, _, _, _, reply) = event {
                    let _ = reply.send(Ok(()));
                }
            }
        });
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
        drop(tx);
        acknowledger.join().unwrap();
        assert!(outcome.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn disconnected_process_event_channel_cancels_child_before_returning() {
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        drop(rx);
        let cancel = AtomicBool::new(false);
        let outcome = run_streaming(
            "/bin/sh",
            &["-c".into(), "sleep 30".into()],
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

        let error = outcome.unwrap_err();
        assert!(error.contains("event channel disconnected"));
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[cfg(unix)]
    #[test]
    fn rejected_process_registration_cancels_child_before_returning() {
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let acknowledger = thread::spawn(move || {
            if let Ok(Event::ProcessStarted(_, _, _, _, _, _, _, reply)) = rx.recv() {
                let _ = reply.send(Err("synthetic durable registration rejection".into()));
            }
        });
        let cancel = AtomicBool::new(false);
        let outcome = run_streaming(
            "/bin/sh",
            &["-c".into(), "sleep 30".into()],
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
        drop(tx);
        acknowledger.join().unwrap();
        assert!(
            outcome
                .unwrap_err()
                .contains("process registration failed; child cancelled")
        );
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

    #[cfg(target_os = "macos")]
    #[test]
    fn recorded_macos_process_identity_can_be_validated_and_terminated() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        let (start_ticks, process_group, session_id) = process_identity(pid).unwrap();
        let process = core::ActiveProcess {
            run_id: "run-macos-identity".into(),
            job_id: "job-macos-identity".into(),
            pid,
            start_ticks: Some(start_ticks),
            process_group: Some(process_group),
            session_id: Some(session_id),
            executable: "sh".into(),
        };
        assert!(recorded_process_matches(&process));
        terminate_recorded_process_group(&process);
        let status = child.wait().unwrap();
        assert!(!status.success());
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
            endpoint::parts("mail.example:8143", 993).unwrap(),
            ("mail.example".into(), 8143)
        );
        assert_eq!(
            endpoint::parts("[2001:db8::1]:993", 143).unwrap(),
            ("2001:db8::1".into(), 993)
        );
        assert!(endpoint::parts("mail.example:0", 993).is_err());
        assert!(endpoint::parts("[2001:db8::1]garbage", 993).is_err());
    }

    #[test]
    fn command_endpoint_helpers_never_reuse_invalid_input() {
        assert_eq!(
            command_endpoint_parts("mail.example:0", 993),
            ("<invalid-endpoint>".into(), 0)
        );
        assert_eq!(command_port("000", 993), 0);
        assert_eq!(command_port("", 993), 993);
    }

    #[test]
    fn validation_rejects_malformed_embedded_endpoint_ports() {
        let mut form = dovecot_form();
        form.profile.source_host = "source.example:not-a-port".into();
        assert!(
            form.validate()
                .unwrap_err()
                .contains("Source IMAP host is not a valid endpoint")
        );
        form.profile.source_host = "source.example".into();
        form.profile.destination_host = "destination.example:bad".into();
        assert!(
            form.validate()
                .unwrap_err()
                .contains("Destination IMAP host is not a valid endpoint")
        );
    }

    #[test]
    fn probe_endpoint_preserves_explicit_ipv6_and_port() {
        assert_eq!(
            endpoint_for_probe("[2001:db8::1]", "993").unwrap(),
            "[2001:db8::1]:993"
        );
        assert_eq!(
            endpoint_for_probe("mail.example", "").unwrap(),
            "mail.example"
        );
        assert!(endpoint_for_probe("mail.example", "0").is_err());
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
        form.profile.source_tls = "starttls".into();
        form.profile.destination_tls = "starttls".into();
        let args = form.args(true);
        assert!(args.contains(&"--tls1".into()));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sslargs1", "SSL_verify_mode=1"])
        );
        assert!(args.windows(2).any(|pair| pair == ["--port2", "143"]));
        assert!(args.contains(&"--tls2".into()));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sslargs2", "SSL_verify_mode=1"])
        );
        assert!(!args.iter().any(|arg| arg.starts_with("--tlsargs")));
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
        let fallback = credentials::secret_runtime_base_from(Some(PathBuf::from("")));
        let temp_dir = std::env::temp_dir();
        assert_eq!(fallback.parent(), Some(temp_dir.as_path()));
        #[cfg(unix)]
        assert!(
            fallback
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("mailswiftsync-runtime-"))
        );
        #[cfg(not(unix))]
        assert_eq!(
            fallback.file_name().and_then(|name| name.to_str()),
            Some("mailswiftsync-runtime")
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

        terminate_process_group_by_pid(pid);

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
        let (start_ticks, process_group, session_id) = process_identity(pid).unwrap();
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
        terminate_recorded_process_group(&process);
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
            "A migration is running — monitor Activity or use Stop migration if you need to halt it."
        );
    }

    #[test]
    fn lifecycle_cannot_advance_when_terminal_durability_is_uncertain() {
        assert!(terminal_phase_advance_allowed(true, true, false));
        assert!(!terminal_phase_advance_allowed(true, false, false));
        assert!(!terminal_phase_advance_allowed(true, true, true));
        assert!(!terminal_phase_advance_allowed(false, true, false));
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
        assert!(
            !args
                .iter()
                .any(|arg| arg == "ssl_client_require_valid_cert=yes")
        );
    }

    #[test]
    fn dovecot_encrypted_source_forces_certificate_validation() {
        let mut form = dovecot_form();
        form.profile.source_tls = "starttls".into();
        form.profile.dovecot_config = "/etc/dovecot/custom.conf".into();
        form.profile.source_ca_bundle = "/etc/company-ca.pem".into();
        let (_, args) = form.command(true);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ssl_client_require_valid_cert=yes"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ssl_client_ca_file=/etc/company-ca.pem"])
        );
    }

    #[test]
    fn imapsync_trust_bundle_is_explicit_and_verification_stays_enabled() {
        let mut form = Form::default();
        form.profile.source_ca_bundle = "/etc/company ca.pem".into();
        form.profile.destination_ca_bundle = "/opt/customer trust/ca.pem".into();
        let args = form.args(true);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sslargs1", "SSL_verify_mode=1"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sslargs1", "SSL_ca_file=/etc/company ca.pem"])
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("SSL_verify_mode=1 SSL_ca_file="))
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sslargs2", "SSL_verify_mode=1"])
        );
        assert!(
            args.windows(2)
                .any(|pair| { pair == ["--sslargs2", "SSL_ca_file=/opt/customer trust/ca.pem"] })
        );
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
    fn trust_settings_change_the_preflight_fingerprint() {
        let form = Form::default();
        let original = form.plan_fingerprint();
        let mut changed = form.clone();
        changed.profile.source_ca_bundle = "/etc/company-ca.pem".into();
        assert_ne!(original, changed.plan_fingerprint());
        changed.profile.source_ca_bundle.clear();
        changed.profile.destination_certificate_pin_sha256 = "ab".repeat(32);
        assert_ne!(original, changed.plan_fingerprint());
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
    fn imapsync_parser_ignores_unrelated_detected_lines() {
        let lines = [
            "Host1 Nb folders: 1 folders".into(),
            "Host2 Nb folders: 1 folders".into(),
            "Host1 Nb messages: 2 messages".into(),
            "Host2 Nb messages: 2 messages".into(),
            "Host1 Total size: 100 bytes".into(),
            "Host2 Total size: 100 bytes".into(),
            "Detected 17 folders during namespace discovery".into(),
            "The sync looks good".into(),
        ];
        let evidence = verification::parse_imapsync_evidence(&lines).unwrap();
        assert_eq!(evidence.failed_messages, 0);
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

    #[test]
    fn live_auth_proof_requires_matching_plan_and_credentials() {
        let proof = LiveAuthProof {
            plan_fingerprint: "plan-a".into(),
            credential_fingerprint: "credentials-a".into(),
        };

        assert!(proof.matches("plan-a", "credentials-a"));
        assert!(!proof.matches("plan-b", "credentials-a"));
        assert!(!proof.matches("plan-a", "credentials-b"));
    }

    #[test]
    fn fresh_imap_authentication_applies_to_encrypted_transports() {
        let form = dovecot_form();
        assert!(!fresh_imap_authentication_applies(&form));

        let mut form = Form::default();
        form.profile.source_tls = "starttls".into();
        form.profile.destination_tls = "imaps".into();
        form.dry_run = false;
        assert!(fresh_imap_authentication_applies(&form));
        form.profile.destination_tls = "starttls".into();
        assert!(fresh_imap_authentication_applies(&form));
        form.profile.source_tls = "plain".into();
        assert!(!fresh_imap_authentication_applies(&form));
    }

    #[test]
    fn ui_scale_cycles_through_readable_operator_presets() {
        assert_eq!(next_ui_scale(0.90), 1.00);
        assert_eq!(next_ui_scale(1.10), 1.25);
        assert_eq!(next_ui_scale(1.50), 0.90);
    }

    #[test]
    fn semantic_theme_text_colors_meet_normal_text_contrast_target() {
        for colors in [ThemeColors::dark(), ThemeColors::light()] {
            for foreground in [
                colors.text_primary,
                colors.text_secondary,
                colors.info,
                colors.success,
                colors.warning,
                colors.danger,
                colors.link,
            ] {
                assert!(
                    contrast_ratio(foreground, colors.background) >= 4.5,
                    "foreground {:?} does not meet contrast target",
                    foreground
                );
            }
        }
    }

    #[test]
    fn displayed_batch_states_map_to_durable_retry_keys() {
        assert_eq!(display_state_key("Failed"), "failed");
        assert_eq!(display_state_key("Delta required"), "delta_required");
        assert_eq!(
            display_state_key("Verification difference"),
            "verification_difference"
        );
    }

    #[test]
    fn headless_batch_mode_binds_form_and_controller_modes() {
        assert_eq!(headless_batch_mode(false), (true, true));
        assert_eq!(headless_batch_mode(true), (false, false));
    }

    #[test]
    fn status_severity_uses_operational_meaning_not_only_failed_prefixes() {
        assert_eq!(
            status_severity("Migration completed and verified"),
            StatusSeverity::Success
        );
        assert_eq!(
            status_severity("Migration result requires durability review"),
            StatusSeverity::Error
        );
        assert_eq!(
            status_severity("Cancellation requested…"),
            StatusSeverity::Warning
        );
        assert_eq!(
            status_severity("Fresh authentication passed; continuing"),
            StatusSeverity::Success
        );
        assert_eq!(status_severity("Dry run in progress"), StatusSeverity::Info);
    }

    #[test]
    fn mailbox_state_badges_are_semantic_and_not_uniform() {
        assert_eq!(
            job_state_badge("verified", ThemeColors::dark()).0,
            "✓ Verified"
        );
        assert_eq!(job_state_badge("failed", ThemeColors::dark()).0, "× Failed");
        assert_ne!(
            job_state_badge("verified", ThemeColors::dark()).1,
            job_state_badge("failed", ThemeColors::dark()).1
        );
        assert_eq!(
            job_state_badge("retrying", ThemeColors::dark()).0,
            "↻ Retrying"
        );
    }

    #[test]
    fn headless_status_is_secret_free_and_reports_durable_mailboxes() {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-headless-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let state = directory.join("state.db");
        let db = core::StateStore::open(&state).unwrap();
        let project = db
            .create_project_with_mailbox(
                "headless-status",
                "source.example",
                "destination.example",
                "source-user",
                "destination-user",
            )
            .unwrap()
            .0;
        drop(db);

        // Read-only status must remain available while a controller owns the
        // exclusive application lock.
        let _writer_lock = acquire_instance_lock(&state).unwrap();
        let status = headless_status(&state, Some(&project.id)).unwrap();
        assert_eq!(status.schema_version, core::CURRENT_SCHEMA_VERSION);
        assert_eq!(status.projects.len(), 1);
        assert_eq!(status.projects[0].mailboxes.len(), 1);
        assert_eq!(status.projects[0].mailboxes[0].state, "queued");
        let serialized = serde_json::to_string(&status).unwrap();
        assert!(!serialized.contains("password"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn durable_state_path_never_falls_back_to_temporary_storage() {
        assert_eq!(
            persistent_state_path_from(
                Some(OsString::from("/var/lib/mailswiftsync/state.db")),
                None,
            )
            .unwrap(),
            PathBuf::from("/var/lib/mailswiftsync/state.db")
        );
        assert_eq!(
            persistent_state_path_from(None, Some(PathBuf::from("/home/operator/.local/share")))
                .unwrap(),
            PathBuf::from("/home/operator/.local/share/mailswiftsync/state.db")
        );
        let missing_directory = persistent_state_path_from(None, None).unwrap_err();
        assert!(missing_directory.contains("durable state directory"));
        let empty_override = persistent_state_path_from(Some(OsString::new()), None).unwrap_err();
        assert!(empty_override.contains("set but empty"));
    }
}
