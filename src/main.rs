#[allow(dead_code)] // The control-plane API is consumed by the next orchestration UI layer.
mod core;

use calamine::{Reader, open_workbook_auto};
use eframe::{
    egui,
    egui::{Color32, RichText, Stroke},
};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::Duration,
};

const NAVY: Color32 = Color32::from_rgb(17, 26, 43);
const BLUE: Color32 = Color32::from_rgb(45, 113, 205);
const TEAL: Color32 = Color32::from_rgb(24, 158, 166);
const SKY: Color32 = Color32::from_rgb(235, 243, 252);
const MUTED: Color32 = Color32::from_rgb(103, 119, 139);
const ALERT: Color32 = Color32::from_rgb(193, 74, 61);

#[derive(Clone, Default, Serialize, Deserialize)]
struct Profile {
    name: String,
    source_host: String,
    #[serde(default)]
    source_port: String,
    #[serde(default = "default_source_tls")]
    source_tls: String,
    source_user: String,
    destination_host: String,
    destination_user: String,
    imapsync_path: String,
    #[serde(default)]
    engine: core::Engine,
    #[serde(default = "default_doveadm_path")]
    doveadm_path: String,
    #[serde(default = "default_ssh_path")]
    ssh_path: String,
    #[serde(default)]
    dovecot_ssh_user: String,
    #[serde(default)]
    dovecot_config: String,
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

fn default_doveadm_path() -> String {
    "doveadm".into()
}
fn default_ssh_path() -> String {
    "ssh".into()
}
fn default_source_tls() -> String {
    "imaps".into()
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
    source_password: String,
    destination_password: String,
    dry_run: bool,
}
impl Default for Form {
    fn default() -> Self {
        Self {
            profile: Profile {
                name: "New migration".into(),
                imapsync_path: "imapsync".into(),
                source_tls: default_source_tls(),
                doveadm_path: default_doveadm_path(),
                ssh_path: default_ssh_path(),
                automap: true,
                ..Default::default()
            },
            source_password: String::new(),
            destination_password: String::new(),
            dry_run: true,
        }
    }
}
impl Form {
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
        if let Err(error) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.to_string());
        }
        Ok(())
    }
    fn validate(&self) -> Result<(), String> {
        let mut required = vec![
            ("Source IMAP host", &self.profile.source_host),
            ("Source username", &self.profile.source_user),
            ("Destination IMAP host", &self.profile.destination_host),
            ("Destination username", &self.profile.destination_user),
            ("Source password", &self.source_password),
        ];
        if self.engine() != core::Engine::Dovecot {
            required.push(("Destination password", &self.destination_password));
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
        for (label, value) in required {
            if value.trim().is_empty() {
                return Err(format!("{label} is required."));
            }
        }
        self.extra_options_valid()?;
        Ok(())
    }
    fn args(&self, redact: bool) -> Vec<String> {
        let p1 = if redact {
            "••••••••"
        } else {
            &self.source_password
        };
        let p2 = if redact {
            "••••••••"
        } else {
            &self.destination_password
        };
        let (source_host, endpoint_port) = endpoint_parts(&self.profile.source_host, 993)
            .unwrap_or_else(|_| (self.profile.source_host.clone(), 993));
        let source_port = self.profile.source_port.trim();
        let mut a = vec![
            "--host1".into(),
            source_host,
            "--user1".into(),
            self.profile.source_user.clone(),
            "--password1".into(),
            p1.into(),
            "--host2".into(),
            self.profile.destination_host.clone(),
            "--user2".into(),
            self.profile.destination_user.clone(),
            "--password2".into(),
            p2.into(),
        ];
        a.extend([
            "--port1".into(),
            if source_port.is_empty() {
                endpoint_port.to_string()
            } else {
                source_port.into()
            },
        ]);
        if self.profile.source_tls == "plain" {
            a.push("--nossl1".into());
        }
        if self.profile.source_tls == "starttls" {
            a.push("--tls1".into());
        }
        if self.profile.automap {
            a.push("--automap".into());
        }
        if self.profile.addheader {
            a.push("--addheader".into());
        }
        if self.profile.justfolders {
            a.push("--justfolders".into());
        }
        for (enabled, flag) in [
            (self.profile.sync_internaldates, "--syncinternaldates"),
            (self.profile.useuid, "--useuid"),
            (self.profile.usecache, "--usecache"),
            (self.profile.fastio1, "--fastio1"),
            (self.profile.fastio2, "--fastio2"),
            (self.profile.allowsizemismatch, "--allowsizemismatch"),
            (self.profile.delete2, "--delete2"),
        ] {
            if enabled {
                a.push(flag.into());
            }
        }
        if self.dry_run {
            a.push("--dry".into());
        }
        if let Ok(extra) = parse_shell_words(&self.profile.extra_options) {
            a.extend(extra);
        }
        a
    }
    fn extra_options_valid(&self) -> Result<(), String> {
        parse_shell_words(&self.profile.extra_options)
            .map(|_| ())
            .map_err(|error| format!("Extra options: {error}"))
    }
    fn prepared_command(&self) -> Result<PreparedCommand, String> {
        if self.engine() == core::Engine::Dovecot {
            let (executable, args) = self.command(false);
            return Ok(PreparedCommand {
                executable,
                args,
                cleanup: Vec::new(),
                env: Vec::new(),
            });
        }
        let mut args = self.args(false);
        remove_option(&mut args, "--password1");
        remove_option(&mut args, "--password2");
        Ok(PreparedCommand {
            executable: self.profile.imapsync_path.clone(),
            args,
            cleanup: Vec::new(),
            env: vec![
                ("IMAPSYNC_PASSWORD1".into(), self.source_password.clone()),
                (
                    "IMAPSYNC_PASSWORD2".into(),
                    self.destination_password.clone(),
                ),
            ],
        })
    }
    fn engine(&self) -> core::Engine {
        match self.profile.engine {
            // The desktop cannot safely infer the destination's mail stack
            // from a hostname or from a locally installed executable.
            core::Engine::Auto => core::Engine::ImapSync,
            selected => selected,
        }
    }
    fn command(&self, redact: bool) -> (String, Vec<String>) {
        if self.engine() != core::Engine::Dovecot {
            return (self.profile.imapsync_path.clone(), self.args(redact));
        }
        let password = if redact {
            "••••••••"
        } else {
            &self.source_password
        };
        let (source_host, endpoint_port) = endpoint_parts(&self.profile.source_host, 993)
            .unwrap_or_else(|_| (self.profile.source_host.clone(), 993));
        let source_port = self
            .profile
            .source_port
            .parse::<u16>()
            .unwrap_or(endpoint_port);
        let mut args = Vec::new();
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
        if self.profile.dovecot_ssh_user.trim().is_empty()
            && ["localhost", "127.0.0.1", "::1"].contains(&self.profile.destination_host.trim())
        {
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
    fn dovecot_verification_commands(&self, redact: bool) -> Vec<(String, Vec<String>)> {
        if self.engine() != core::Engine::Dovecot {
            return Vec::new();
        }
        let password = if redact {
            "••••••••"
        } else {
            &self.source_password
        };
        let (source_host, endpoint_port) = endpoint_parts(&self.profile.source_host, 993)
            .unwrap_or_else(|_| (self.profile.source_host.clone(), 993));
        let source_port = self
            .profile
            .source_port
            .parse::<u16>()
            .unwrap_or(endpoint_port);
        let mut source = Vec::new();
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

fn wait_with_timeout(
    child: &mut Child,
    timeout: Duration,
    cancel: &AtomicBool,
) -> std::io::Result<ExitStatus> {
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if cancel.load(Ordering::Relaxed) {
            terminate_process_group(child);
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled by operator",
            ));
        }
        if started.elapsed() >= timeout {
            terminate_process_group(child);
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "migration exceeded the one-day execution limit",
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Each migration gets its own session so cancellation cannot leave a
        // shell/wrapper descendant running against the destination.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

fn terminate_process_group(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = -(child.id() as libc::pid_t);
        unsafe {
            let _ = libc::kill(process_group, libc::SIGTERM);
        }
        thread::sleep(Duration::from_millis(100));
        unsafe {
            let _ = libc::kill(process_group, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = child.kill();
}

fn number_after(line: &str, marker: &str) -> Option<u64> {
    line.split_once(marker)?
        .1
        .split_whitespace()
        .find_map(|token| {
            token
                .trim_matches(|character: char| !character.is_ascii_digit())
                .parse()
                .ok()
        })
}

/// Extracts the stable summary fields emitted by imapsync. We intentionally
/// require both hosts and all three dimensions before writing evidence; a
/// partial log must never look like a successful zero-message migration.
fn parse_imapsync_evidence(lines: &[String]) -> Option<core::MailboxEvidence> {
    let last = |marker: &str| {
        lines
            .iter()
            .rev()
            .find_map(|line| number_after(line, marker))
    };
    let source_folders = last("Host1 Nb folders:")?;
    let destination_folders = last("Host2 Nb folders:")?;
    let source_messages = last("Host1 Nb messages:")?;
    let destination_messages = last("Host2 Nb messages:")?;
    let source_bytes = last("Host1 Total size:")?;
    let destination_bytes = last("Host2 Total size:")?;
    let failed_messages = last("Detected ").unwrap_or(0);
    let matched = lines
        .iter()
        .any(|line| line.contains("The sync looks good"));
    Some(core::MailboxEvidence {
        source_messages,
        destination_messages,
        source_bytes,
        destination_bytes,
        unmatched_messages: if matched { 0 } else { 1 },
        failed_messages,
        source_folders,
        destination_folders,
        authoritative: matched,
    })
}

fn parse_dovecot_status(lines: &[String]) -> Option<(u64, u64, u64)> {
    let mut folders = 0;
    let mut messages = 0;
    let mut bytes = 0;
    for line in lines {
        let message_count = line
            .split_whitespace()
            .find_map(|token| token.strip_prefix("messages=")?.parse::<u64>().ok());
        let virtual_size = line
            .split_whitespace()
            .find_map(|token| token.strip_prefix("vsize=")?.parse::<u64>().ok());
        if let (Some(message_count), Some(virtual_size)) = (message_count, virtual_size) {
            folders += 1;
            messages += message_count;
            bytes += virtual_size;
        }
    }
    (folders > 0).then_some((folders, messages, bytes))
}

fn parse_dovecot_evidence(
    source: &[String],
    destination: &[String],
) -> Option<core::MailboxEvidence> {
    let (source_folders, source_messages, source_bytes) = parse_dovecot_status(source)?;
    let (destination_folders, destination_messages, destination_bytes) =
        parse_dovecot_status(destination)?;
    Some(core::MailboxEvidence {
        source_messages,
        destination_messages,
        source_bytes,
        destination_bytes,
        unmatched_messages: 0,
        failed_messages: 0,
        source_folders,
        destination_folders,
        authoritative: false,
    })
}

enum Event {
    Line(String),
    JobState(usize, String),
    Evidence(core::MailboxEvidence),
    VerificationFailed(String),
    Finished(Result<(), String>),
}

fn run_streaming(
    executable: &str,
    args: &[String],
    env: &[(String, String)],
    tx: &mpsc::Sender<Event>,
    prefix: &str,
    cancel: &AtomicBool,
    secrets: &[String],
) -> Result<(), String> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {executable}: {error}"))?;
    let stdout = child.stdout.take().ok_or("stdout pipe unavailable")?;
    let stderr = child.stderr.take().ok_or("stderr pipe unavailable")?;
    let out_tx = tx.clone();
    let out_prefix = prefix.to_owned();
    let out_secrets = secrets.to_vec();
    let out_thread = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let mut safe = line;
            for secret in &out_secrets {
                if !secret.is_empty() {
                    safe = safe.replace(secret, "[REDACTED]");
                }
            }
            let _ = out_tx.send(Event::Line(format!("{out_prefix}{safe}")));
        }
    });
    let err_tx = tx.clone();
    let err_prefix = prefix.to_owned();
    let err_secrets = secrets.to_vec();
    let err_thread = thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let mut safe = line;
            for secret in &err_secrets {
                if !secret.is_empty() {
                    safe = safe.replace(secret, "[REDACTED]");
                }
            }
            let _ = err_tx.send(Event::Line(format!("{err_prefix}[stderr] {safe}")));
        }
    });
    let result = wait_with_timeout(&mut child, Duration::from_secs(24 * 60 * 60), cancel)
        .map_err(|error| error.to_string())
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(format!("process exited with {status}"))
            }
        });
    let _ = out_thread.join();
    let _ = err_thread.join();
    result
}

fn run_capture_lines(
    executable: &str,
    args: &[String],
    cancel: &AtomicBool,
    secrets: &[String],
) -> Result<(ExitStatus, Vec<String>), String> {
    let mut command = Command::new(executable);
    command
        .args(args)
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
    let status = wait_with_timeout(&mut child, Duration::from_secs(60 * 60), cancel)
        .map_err(|error| error.to_string())?;
    let mut lines = out_thread
        .join()
        .map_err(|_| "stdout reader failed".to_owned())?;
    lines.extend(
        err_thread
            .join()
            .map_err(|_| "stderr reader failed".to_owned())?
            .into_iter()
            .map(|line| format!("[stderr] {line}")),
    );
    Ok((status, lines))
}

fn collect_redacted_lines<R: Read>(reader: R, secrets: &[String]) -> Vec<String> {
    BufReader::new(reader)
        .lines()
        .map_while(Result::ok)
        .map(|mut line| {
            for secret in secrets {
                if !secret.is_empty() {
                    line = line.replace(secret, "[REDACTED]");
                }
            }
            line
        })
        .collect()
}
#[derive(Clone)]
struct BulkJob {
    label: String,
    form: Form,
    state: String,
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
    output: Vec<String>,
    receiver: Option<Receiver<Event>>,
    status: String,
    preview: bool,
    bulk_jobs: Vec<BulkJob>,
    bulk_open: bool,
    bulk_message: String,
    advanced_open: bool,
    engine_open: bool,
    store: core::StateStore,
    persistence_available: bool,
    project_id: Option<String>,
    job_id: Option<String>,
    run_id: Option<String>,
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
    active_view: WorkspaceView,
}
impl Default for App {
    fn default() -> Self {
        let state_path = dirs_next::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("mailswiftsync/state.db");
        if let Some(parent) = state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let (store, persistence_warning) = match core::StateStore::open(&state_path) {
            Ok(store) => (store, None),
            Err(error) => (
                core::StateStore::in_memory().expect("SQLite memory store must be available"),
                Some(format!("Persistent SQLite state unavailable: {error}")),
            ),
        };
        let recovered = if persistence_warning.is_none() {
            store.recover_abandoned_jobs().unwrap_or(0)
        } else {
            0
        };
        let mut initial_output = persistence_warning.clone().map_or_else(
            || vec!["Ready. Start with a dry run against a test destination mailbox.".into()],
            |warning| {
                vec![
                    warning.clone(),
                    "WARNING: this session is not durable.".into(),
                ]
            },
        );
        if recovered > 0 {
            initial_output.push(format!(
                "Recovered {recovered} interrupted job(s) into Attention for review."
            ));
        }
        let form = Form::load();
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
                (
                    Some(project.id.clone()),
                    store.first_mailbox(&project.id).ok().flatten(),
                )
            })
            .unwrap_or((None, None));
        Self {
            form,
            output: initial_output,
            receiver: None,
            status: persistence_warning.clone().unwrap_or_else(|| "Idle".into()),
            preview: false,
            bulk_jobs: Vec::new(),
            bulk_open: false,
            bulk_message: "Import a CSV, XLS, or XLSX file to build a reviewable queue.".into(),
            advanced_open: false,
            engine_open: true,
            store,
            persistence_available: persistence_warning.is_none(),
            project_id,
            job_id,
            run_id: None,
            cancel_requested: None,
            bulk_project_id: None,
            bulk_job_ids: Vec::new(),
            cockpit_open: false,
            preflight: Vec::new(),
            capability_receiver: None,
            source_capabilities: None,
            destination_capabilities: None,
            live_confirm_open: false,
            live_confirmed: false,
            active_view: WorkspaceView::Overview,
        }
    }
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

fn probe_tls_capabilities(host: &str) -> Result<core::ServerCapabilities, String> {
    let (server_name, port) =
        endpoint_parts(host, 993).map_err(|error| format!("Invalid IMAP host {host}: {error}"))?;
    let address = if server_name.contains(':') {
        format!("[{server_name}]:{port}")
    } else {
        format!("{server_name}:{port}")
    };
    let socket = address
        .to_socket_addrs()
        .map_err(|error| format!("{host}: {error}"))?
        .next()
        .ok_or_else(|| format!("{host}: no address found"))?;
    let tcp = TcpStream::connect_timeout(&socket, Duration::from_secs(8))
        .map_err(|e| format!("{host}: {e}"))?;
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
    stream
        .write_all(b"a001 CAPABILITY\r\n")
        .map_err(|e| e.to_string())?;
    let mut response = String::new();
    let mut buffer = [0; 4096];
    loop {
        let count = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..count]));
        if response.to_ascii_lowercase().contains("a001 ok") || response.len() > 65_536 {
            break;
        }
    }
    let caps = core::ServerCapabilities::parse(&response);
    if caps.values.is_empty() {
        return Err(format!(
            "{host}: server did not return a CAPABILITY response"
        ));
    }
    Ok(caps)
}

impl App {
    fn start_capability_probe(&mut self) {
        let source = self.form.profile.source_host.trim().to_owned();
        let destination = self.form.profile.destination_host.trim().to_owned();
        if source.is_empty() || destination.is_empty() {
            self.status = "Enter both IMAP hosts before capability discovery.".into();
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.capability_receiver = Some(rx);
        self.status = "Discovering TLS capabilities…".into();
        thread::spawn(move || {
            let result = probe_tls_capabilities(&source)
                .and_then(|left| probe_tls_capabilities(&destination).map(|right| (left, right)));
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
    }
    fn create_project(&mut self) {
        self.assess_plan();
        if self.form.profile.source_host.trim().is_empty()
            || self.form.profile.destination_host.trim().is_empty()
        {
            self.status = "Enter source and destination hosts before creating a project.".into();
            return;
        }
        match self.store.create_project(
            &self.form.profile.name,
            &self.form.profile.source_host,
            &self.form.profile.destination_host,
        ) {
            Ok(project) => match self.store.add_mailbox(
                &project.id,
                &self.form.profile.source_user,
                &self.form.profile.destination_user,
            ) {
                Ok(job) => {
                    self.project_id = Some(project.id);
                    self.job_id = Some(job);
                    self.status = "Project created; ready for preflight review".into();
                }
                Err(e) => self.status = format!("Could not create mailbox job: {e}"),
            },
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
            if ui.add_enabled(self.capability_receiver.is_none(), egui::Button::new("Discover server capabilities over verified TLS")).clicked() { self.start_capability_probe(); }
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

    fn project_summary(&mut self, ui: &mut egui::Ui) {
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
        let (passed, total) = self.readiness_score();
        let percent = (passed * 100 / total.max(1)) as u8;
        ui.label(
            RichText::new(format!("Readiness: {percent}%"))
                .strong()
                .color(if percent >= 80 { TEAL } else { ALERT }),
        );
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
                ui.heading(if self.bulk_jobs.is_empty() {
                    "1 configured".to_owned()
                } else {
                    format!("{} queued", self.bulk_jobs.len())
                });
                ui.label("Use Mailboxes to review scope before running anything.");
            });
            ui.group(|ui| {
                ui.label(RichText::new("EVIDENCE").size(11.0).color(MUTED));
                ui.heading("Not started");
                ui.label("Verification becomes available after a completed run.");
            });
        });
        ui.add_space(16.0);
        ui.group(|ui| {
            ui.heading("Recommended next step");
            ui.label("Run a preflight before any live migration. It checks endpoints, TLS capabilities, safety settings, and the current project scope.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Open migration plan  →").clicked() { self.active_view = WorkspaceView::Plan; }
                if ui.button("Open Project Cockpit").clicked() { self.cockpit_open = true; self.assess_plan(); }
                if ui.button("Import mailbox list").clicked() { self.bulk_open = true; }
            });
        });
        ui.add_space(14.0);
        ui.label(RichText::new("Safety contract").strong());
        ui.horizontal_wrapped(|ui| {
            for text in [
                "Simulation is the default",
                "Saved profiles exclude passwords",
                "TLS verification is enabled",
                "Source mail is read-only by default",
            ] {
                ui.label(RichText::new(format!("✓ {text}")).color(TEAL));
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

    fn activity_view(&self, ui: &mut egui::Ui) {
        ui.heading("Activity");
        ui.label(RichText::new("Live output is retained here for operator review. Detailed diagnostics stay attached to the run.").color(MUTED));
        ui.add_space(12.0);
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
    }

    fn export_verification_report(&self) -> Result<(), String> {
        let job = self
            .job_id
            .as_deref()
            .ok_or("No mailbox evidence is available yet.")?;
        let evidence = self
            .store
            .evidence(job)
            .map_err(|e| e.to_string())?
            .ok_or("No mailbox evidence is available yet.")?;
        let path = rfd::FileDialog::new()
            .set_file_name("mailswiftsync-verification.md")
            .save_file()
            .ok_or("Report export cancelled.")?;
        let report = format!(
            "# MailSwiftSync verification report\n\n- Project: {}\n- Source: {}\n- Destination: {}\n- Engine: {}\n- Confidence: {}%\n\n| Metric | Source | Destination |\n|---|---:|---:|\n| Folders | {} | {} |\n| Messages | {} | {} |\n| Virtual size | {} | {} |\n| Unmatched messages | {} | — |\n| Failed messages | {} | — |\n\nThis report contains aggregate evidence. Message-level reconciliation and provider-specific warnings require additional verification.",
            self.form.profile.name,
            self.form.profile.source_host,
            self.form.profile.destination_host,
            self.form.engine().label(),
            evidence.confidence_percent(),
            evidence.source_folders,
            evidence.destination_folders,
            evidence.source_messages,
            evidence.destination_messages,
            evidence.source_bytes,
            evidence.destination_bytes,
            evidence.unmatched_messages,
            evidence.failed_messages
        );
        std::fs::write(path, report).map_err(|e| e.to_string())
    }

    fn verification_view(&self, ui: &mut egui::Ui) {
        ui.heading("Verification");
        ui.label(RichText::new("Do not trust a completed process until the destination reconciles with the source.").color(MUTED));
        ui.add_space(12.0);
        ui.group(|ui| {
            ui.heading("Verification and audit report");
            ui.label(RichText::new("The transfer engine is only one part of the migration. This report is the operator-facing proof of what arrived and what still needs attention.").color(MUTED));
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
                            ("Confidence", format!("{}%", evidence.confidence_percent())),
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
        form.source_password = get("source_password");
        form.profile.destination_host = get("destination_host");
        form.profile.destination_user = get("destination_user");
        form.destination_password = get("destination_password");
        if let Some(value) = values.get("extra_options") {
            form.profile.extra_options = value.clone();
        }
        form.validate().map_err(|e| format!("Row {row}: {e}"))?;
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
                self.bulk_jobs = jobs;
            }
            Err(e) => self.bulk_message = e,
        }
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
    fn validate_headers(headers: &[String], base: &Form) -> Result<(), String> {
        let mut seen = HashSet::new();
        for header in headers {
            if header.is_empty() || !seen.insert(header.clone()) {
                return Err(
                    "The migration file contains an empty or duplicate column header.".into(),
                );
            }
        }
        let mut required = vec![
            "source_host",
            "source_user",
            "source_password",
            "destination_host",
            "destination_user",
        ];
        if base.engine() != core::Engine::Dovecot {
            required.push("destination_password");
        }
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
        if !self.form.dry_run {
            self.bulk_message = "Batch validation is always non-mutating. Re-enable Dry run before starting the queue.".into();
            return;
        }
        let jobs = self.bulk_jobs.clone();
        if jobs.iter().any(|job| !job.form.dry_run) {
            self.bulk_message = "One or more queued jobs were imported in live mode. Re-import them with Dry run enabled.".into();
            return;
        }
        if !self.persistence_available {
            self.bulk_message = "Batch validation requires durable SQLite storage.".into();
            return;
        }
        let project = match self
            .store
            .create_project("Batch validation", "batch", "batch")
        {
            Ok(project) => project,
            Err(error) => {
                self.bulk_message = format!("Could not create durable batch: {error}");
                return;
            }
        };
        let mut job_ids = Vec::with_capacity(jobs.len());
        for job in &jobs {
            match self.store.add_mailbox(
                &project.id,
                &job.form.profile.source_user,
                &job.form.profile.destination_user,
            ) {
                Ok(id) => job_ids.push(id),
                Err(error) => {
                    self.bulk_message = format!("Could not create durable batch job: {error}");
                    return;
                }
            }
        }
        self.bulk_project_id = Some(project.id.clone());
        self.bulk_job_ids = job_ids;
        for job in &mut self.bulk_jobs {
            job.state = "Queued".into();
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_requested = Some(cancel.clone());
        self.receiver = Some(rx);
        self.status = format!("Batch validation: {} jobs", jobs.len());
        self.output.clear();
        thread::spawn(move || {
            let mut failed = false;
            for (index, job) in jobs.into_iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(Event::JobState(index, "Cancelled".into()));
                    continue;
                }
                let _ = tx.send(Event::Line(format!(
                    "══ Job {}: {} ══",
                    index + 1,
                    job.label
                )));
                let _ = tx.send(Event::JobState(index, "Running".into()));
                let prepared = job.form.prepared_command();
                let result = match prepared {
                    Ok(command) => {
                        let result = run_streaming(
                            &command.executable,
                            &command.args,
                            &command.env,
                            &tx,
                            &format!("[{}] ", index + 1),
                            &cancel,
                            &[
                                job.form.source_password.clone(),
                                job.form.destination_password.clone(),
                            ],
                        );
                        for path in command.cleanup {
                            let _ = std::fs::remove_file(path);
                        }
                        result
                    }
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    let cancelled = error.contains("cancelled");
                    failed = true;
                    let _ = tx.send(Event::Line(format!("[{}] failed: {error}", index + 1)));
                    let _ = tx.send(Event::JobState(
                        index,
                        if cancelled { "Cancelled" } else { "Failed" }.into(),
                    ));
                    if cancelled {
                        continue;
                    }
                } else {
                    let _ = tx.send(Event::JobState(index, "Completed".into()));
                }
            }
            let _ = tx.send(Event::Finished(if cancel.load(Ordering::Relaxed) {
                Err("batch cancelled".into())
            } else if failed {
                Err("one or more batch jobs failed".into())
            } else {
                Ok(())
            }));
        });
    }
    fn running(&self) -> bool {
        self.receiver.is_some()
    }
    fn redact_output(&self, line: &str) -> String {
        let mut safe = line.to_owned();
        for secret in [&self.form.source_password, &self.form.destination_password] {
            if !secret.is_empty() {
                safe = safe.replace(secret, "[REDACTED]");
            }
        }
        safe
    }
    fn start(&mut self) {
        if !self.form.dry_run && !self.live_confirmed {
            self.live_confirm_open = true;
            return;
        }
        if !self.form.dry_run {
            self.live_confirmed = false;
        }
        if let Err(e) = self.form.validate() {
            self.status = e;
            return;
        }
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
                    project.phase != core::Phase::Discovery
                        && project.phase != core::Phase::Attention
                });
            if !preflight_ready || self.job_id.is_none() {
                self.status = "Run and pass a dry preflight for this project before starting a live migration.".into();
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
        let run_id = uuid::Uuid::new_v4().to_string();
        self.run_id = Some(run_id.clone());
        if let Some(job) = &self.job_id {
            let _ = self.store.set_mailbox_state(job, "running");
        }
        if let Some(project) = &self.project_id {
            let _ = self.store.start_run(
                project,
                self.job_id.as_deref(),
                &run_id,
                self.form.engine().label(),
            );
            let _ = self.store.record_event(
                project,
                "run_started",
                &format!("{} ({run_id})", self.form.engine().label()),
            );
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_requested = Some(cancel.clone());
        self.receiver = Some(rx);
        self.status = if self.form.dry_run {
            "Dry run in progress".into()
        } else {
            "Sync in progress".into()
        };
        self.output = vec![format!(
            "Starting {} with {}…",
            if self.form.dry_run {
                "safe dry run"
            } else {
                "synchronization"
            },
            self.form.engine().label()
        )];
        let engine_name = self.form.engine().label().to_owned();
        let verification = if !self.form.dry_run && self.form.engine() == core::Engine::Dovecot {
            self.form.dovecot_verification_commands(false)
        } else {
            Vec::new()
        };
        let verification_secret = self.form.source_password.clone();
        thread::spawn(move || {
            let mut command = Command::new(&exe);
            command
                .args(&args)
                .envs(prepared_env.iter().map(|(key, value)| (key, value)))
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            configure_process_group(&mut command);
            let mut child = match command.spawn() {
                Ok(c) => c,
                Err(e) => {
                    for path in cleanup {
                        let _ = std::fs::remove_file(path);
                    }
                    let _ = tx.send(Event::Finished(Err(format!("Could not start {exe}: {e}"))));
                    return;
                }
            };
            let out = child.stdout.take().expect("piped");
            let err = child.stderr.take().expect("piped");
            let a = tx.clone();
            let t1 = thread::spawn(move || {
                for l in BufReader::new(out).lines().map_while(Result::ok) {
                    let _ = a.send(Event::Line(l));
                }
            });
            let b = tx.clone();
            let t2 = thread::spawn(move || {
                for l in BufReader::new(err).lines().map_while(Result::ok) {
                    let _ = b.send(Event::Line(format!("[stderr] {l}")));
                }
            });
            let mut result =
                wait_with_timeout(&mut child, Duration::from_secs(24 * 60 * 60), &cancel)
                    .map_err(|e| e.to_string())
                    .and_then(|s| {
                        if s.success() {
                            Ok(())
                        } else {
                            Err(format!("{engine_name} exited with {s}"))
                        }
                    });
            let _ = t1.join();
            let _ = t2.join();
            if result.is_ok() && !verification.is_empty() {
                let mut reports = Vec::new();
                for (index, (verify_exe, verify_args)) in verification.iter().enumerate() {
                    match run_capture_lines(
                        verify_exe,
                        verify_args,
                        &cancel,
                        std::slice::from_ref(&verification_secret),
                    ) {
                        Ok((status, report)) => {
                            for line in &report {
                                let _ = tx.send(Event::Line(format!(
                                    "[verification/{}] {line}",
                                    index + 1
                                )));
                            }
                            if status.success() {
                                reports.push(report);
                            } else {
                                let _ = tx.send(Event::VerificationFailed(format!(
                                    "verification command {} exited with {}",
                                    index + 1,
                                    status
                                )));
                                result =
                                    Err("migration completed; Dovecot verification failed".into());
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = tx.send(Event::VerificationFailed(format!(
                                "could not start verification command {}: {error}",
                                index + 1
                            )));
                            result =
                                Err("migration completed; Dovecot verification could not start"
                                    .into());
                            break;
                        }
                    }
                }
                if result.is_ok() {
                    if let Some(evidence) = parse_dovecot_evidence(&reports[0], &reports[1]) {
                        let _ = tx.send(Event::Evidence(evidence));
                    } else {
                        let _ = tx.send(Event::VerificationFailed(
                            "Dovecot status output was incomplete".into(),
                        ));
                    }
                }
            }
            for path in cleanup {
                let _ = std::fs::remove_file(path);
            }
            let _ = tx.send(Event::Finished(result));
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
        if let Some(rx) = &self.receiver {
            while let Ok(event) = rx.try_recv() {
                match event {
                    Event::Line(s) => {
                        let safe = self.redact_output(&s);
                        if let Some(project) = &self.project_id {
                            let _ = self.store.record_event(project, "run_output", &safe);
                        }
                        self.output.push(safe);
                    }
                    Event::JobState(index, state) => {
                        if let Some(job) = self.bulk_jobs.get_mut(index) {
                            job.state = state.clone();
                        }
                        if let Some(job_id) = self.bulk_job_ids.get(index) {
                            let durable_state = match state.as_str() {
                                "Running" => "running",
                                "Completed" => "completed",
                                "Failed" => "failed",
                                "Cancelled" => "cancelled",
                                "Queued" => "queued",
                                _ => "attention",
                            };
                            let _ = self.store.set_mailbox_state(job_id, durable_state);
                        }
                    }
                    Event::Evidence(evidence) => {
                        if let Some(job) = &self.job_id {
                            let _ = self.store.record_evidence_for_run(
                                job,
                                self.run_id.as_deref().unwrap_or("unknown"),
                                &evidence,
                            );
                            let state = if evidence.confidence_percent() == 100 {
                                "verified"
                            } else {
                                "delta_required"
                            };
                            let _ = self.store.set_mailbox_state(job, state);
                        }
                        if let Some(project) = &self.project_id {
                            let _ = self.store.record_event(
                                project,
                                "verification_evidence",
                                &format!("{}% confidence", evidence.confidence_percent()),
                            );
                        }
                    }
                    Event::VerificationFailed(detail) => {
                        let safe = self.redact_output(&detail);
                        self.output.push(format!("[verification] {safe}"));
                        if let Some(project) = &self.project_id {
                            let _ = self
                                .store
                                .record_event(project, "verification_pending", &safe);
                        }
                    }
                    Event::Finished(r) => done = Some(r),
                }
            }
        }
        if let Some(r) = done {
            let succeeded = r.is_ok();
            if succeeded && !self.form.dry_run && self.form.engine() == core::Engine::ImapSync {
                if let Some(evidence) = parse_imapsync_evidence(&self.output) {
                    if let Some(job) = &self.job_id {
                        let _ = self.store.record_evidence_for_run(
                            job,
                            self.run_id.as_deref().unwrap_or("unknown"),
                            &evidence,
                        );
                        let state = if evidence.confidence_percent() == 100 {
                            "verified"
                        } else {
                            "delta_required"
                        };
                        let _ = self.store.set_mailbox_state(job, state);
                    }
                    if let Some(project) = &self.project_id {
                        let _ = self.store.record_event(
                            project,
                            "verification_evidence",
                            &format!("{}% confidence", evidence.confidence_percent()),
                        );
                    }
                } else if let Some(project) = &self.project_id {
                    let _ = self.store.record_event(
                        project,
                        "verification_pending",
                        "imapsync summary was incomplete",
                    );
                }
            } else if succeeded
                && !self.form.dry_run
                && self.form.engine() == core::Engine::Dovecot
                && self
                    .store
                    .evidence(self.job_id.as_deref().unwrap_or(""))
                    .ok()
                    .flatten()
                    .is_none()
                && let Some(project) = &self.project_id
            {
                let _ = self.store.record_event(
                    project,
                    "verification_pending",
                    "doveadm completed; mailbox reconciliation was incomplete",
                );
            }
            if let Some(job) = &self.job_id {
                let final_state = if succeeded && self.form.dry_run {
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
                } else if let Ok(Some(evidence)) = self.store.evidence(job) {
                    if evidence.confidence_percent() == 100 {
                        "verified"
                    } else {
                        "delta_required"
                    }
                } else if !self.form.dry_run {
                    "attention"
                } else {
                    "completed"
                };
                let _ = self.store.set_mailbox_state(job, final_state);
            }
            if let Some(project) = &self.project_id {
                if let Some(run_id) = &self.run_id {
                    let run_status = if succeeded {
                        "completed"
                    } else if r
                        .as_ref()
                        .err()
                        .is_some_and(|error| error.contains("verification"))
                    {
                        "verification_failed"
                    } else {
                        "failed"
                    };
                    let _ = self.store.finish_run(
                        run_id,
                        run_status,
                        if succeeded {
                            ""
                        } else {
                            r.as_ref().err().map(String::as_str).unwrap_or("run failed")
                        },
                    );
                }
                let _ = self.store.record_event(
                    project,
                    "run_finished",
                    if succeeded { "success" } else { "failure" },
                );
                if succeeded {
                    let _ = self.store.transition(
                        project,
                        if self.form.dry_run {
                            core::Phase::Preflight
                        } else {
                            core::Phase::Verification
                        },
                    );
                } else {
                    let _ = self.store.transition(project, core::Phase::Attention);
                }
            }
            self.status = match r {
                Ok(()) if self.form.dry_run => "Preflight completed successfully".into(),
                Ok(()) => "Completed successfully".into(),
                Err(e) => format!("Failed: {e}"),
            };
            self.receiver = None;
            self.cancel_requested = None;
            self.run_id = None;
            if self.bulk_project_id.is_some() {
                self.bulk_project_id = None;
                self.bulk_job_ids.clear();
            }
            self.live_confirmed = false;
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
        ui.group(|ui| {
            ui.heading(RichText::new(title).color(color));
            ui.label(RichText::new("IMAP connection").size(11.0).color(MUTED));
            ui.horizontal(|ui| {
                ui.label("Server");
                ui.text_edit_singleline(host);
            });
            ui.horizontal(|ui| {
                ui.label("User");
                ui.text_edit_singleline(user);
            });
            ui.horizontal(|ui| {
                ui.label("Password");
                ui.add(egui::TextEdit::singleline(password).password(true));
            });
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
                if ui.button("Import CSV / XLSX…").clicked() && let Some(path) = rfd::FileDialog::new().add_filter("Migration lists", &["csv", "xls", "xlsx"]).pick_file() { self.import_bulk(&path); }
                if ui.button("Clear queue").clicked() { self.bulk_jobs.clear(); self.bulk_message = "Queue cleared.".into(); }
                let label = format!("Run {} dry validations", self.bulk_jobs.len());
                if ui.add_enabled(!self.running() && !self.bulk_jobs.is_empty(), egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(BLUE)).clicked() { self.start_bulk(); }
            });
            ui.add_space(10.0);
            ui.label(RichText::new("Required columns: source_host, source_user, source_password, destination_host, destination_user, destination_password. Optional: name, extra_options.").size(11.0).color(MUTED));
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("bulk_jobs").striped(true).min_col_width(120.0).show(ui, |ui| {
                    ui.strong("#"); ui.strong("Migration"); ui.strong("Source"); ui.strong("Destination"); ui.strong("Status"); ui.end_row();
                    for (index, job) in self.bulk_jobs.iter().enumerate() { ui.label((index + 1).to_string()); ui.label(&job.label); ui.label(format!("{}\n{}", job.form.profile.source_host, job.form.profile.source_user)); ui.label(format!("{}\n{}", job.form.profile.destination_host, job.form.profile.destination_user)); ui.label(RichText::new(&job.state).color(TEAL)); ui.end_row(); }
                });
            });
            ui.add_space(8.0); ui.label(RichText::new("Imported passwords are used only for this open queue. Saving a profile never saves them.").size(11.0).color(ALERT));
        });
        self.bulk_open = open;
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
            ui.group(|ui| { ui.heading("Performance"); ui.checkbox(&mut self.form.profile.fastio1, "Fast I/O for source  (--fastio1)"); ui.checkbox(&mut self.form.profile.fastio2, "Fast I/O for destination  (--fastio2)"); });
            ui.add_space(8.0);
            ui.group(|ui| { ui.heading(RichText::new("Destructive destination option").color(ALERT)); ui.checkbox(&mut self.form.profile.delete2, "Delete destination messages missing from source  (--delete2)"); ui.label(RichText::new("Use only for an intentionally exact backup after a tested dry run. This can remove destination mail.").size(11.0).color(ALERT)); });
            ui.add_space(8.0); ui.label("For any other documented flag, use the Extra imapsync options field in the migration plan. Each option is passed as separate whitespace-delimited arguments.");
        });
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
                ui.horizontal(|ui| { ui.label("doveadm"); ui.text_edit_singleline(&mut self.form.profile.doveadm_path); });
                ui.horizontal(|ui| { ui.label("SSH executable"); ui.text_edit_singleline(&mut self.form.profile.ssh_path); });
                ui.horizontal(|ui| { ui.label("SSH user (optional)"); ui.text_edit_singleline(&mut self.form.profile.dovecot_ssh_user); });
                ui.horizontal(|ui| { ui.label("Config"); ui.text_edit_singleline(&mut self.form.profile.dovecot_config); });
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
impl eframe::App for App {
    #[allow(clippy::possible_missing_else, clippy::collapsible_if)]
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        let mut v = egui::Visuals::light();
        v.panel_fill = SKY;
        v.window_fill = Color32::WHITE;
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
                ui.separator();
                ui.label(RichText::new("TOOLS").size(11.0).strong().color(MUTED));
                if ui.button("Batch queue").clicked() {
                    self.bulk_open = true;
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
            });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(SKY).inner_margin(egui::Margin::same(24))).show(ctx, |ui| { self.project_summary(ui); ui.add_space(14.0); ui.heading("Migration plan"); ui.label(RichText::new("Set up the connection, run preflight, then deliberately promote this project through each migration phase.").color(MUTED)); ui.add_space(14.0); ui.horizontal(|ui| { ui.label("Project name"); ui.text_edit_singleline(&mut self.form.profile.name); ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| if ui.button("Save non-secret profile").clicked() { self.status = match self.form.save() { Ok(()) => "Profile saved; passwords were not saved".into(), Err(e) => format!("Could not save profile: {e}") }; }); }); ui.add_space(10.0); ui.columns(2, |c| { Self::account(&mut c[0], "01  SOURCE MAILBOX", &mut self.form.profile.source_host, &mut self.form.profile.source_user, &mut self.form.source_password, BLUE); Self::account(&mut c[1], "02  DESTINATION MAILBOX", &mut self.form.profile.destination_host, &mut self.form.profile.destination_user, &mut self.form.destination_password, TEAL); }); ui.horizontal(|ui| { ui.label("Source port"); ui.add(egui::TextEdit::singleline(&mut self.form.profile.source_port).desired_width(90.0)); ui.label("TLS"); egui::ComboBox::from_id_salt("source_tls").selected_text(&self.form.profile.source_tls).show_ui(ui, |ui| { for mode in ["imaps", "starttls", "plain"] { ui.selectable_value(&mut self.form.profile.source_tls, mode.into(), mode); } }); }); ui.add_space(14.0); ui.group(|ui| { ui.heading("03  SYNC RULES"); ui.checkbox(&mut self.form.dry_run, "Simulation mode — validate access and mapping without changing the destination"); ui.horizontal(|ui| { ui.checkbox(&mut self.form.profile.automap, "Map standard folders automatically"); ui.checkbox(&mut self.form.profile.justfolders, "Folders only"); ui.checkbox(&mut self.form.profile.addheader, "Add Message-ID header when needed"); }); ui.horizontal(|ui| { ui.label("Extra imapsync options"); ui.text_edit_singleline(&mut self.form.profile.extra_options); }); ui.horizontal(|ui| { ui.label("imapsync executable"); ui.text_edit_singleline(&mut self.form.profile.imapsync_path); }); }); ui.add_space(14.0); ui.horizontal(|ui| { if ui.button("Preview safe command").clicked() { self.preview = true; } if self.running() { if ui.button("Cancel running process").clicked() { if let Some(cancel) = &self.cancel_requested { cancel.store(true, Ordering::Relaxed); self.status = "Cancellation requested…".into(); } } } else { let label = if self.form.dry_run { "Run preflight simulation  →" } else { "Start live migration  →" }; if ui.add_enabled(true, egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(if self.form.dry_run { BLUE } else { ALERT })).clicked() { self.start(); } } if !self.form.dry_run && !self.running() { ui.label(RichText::new("Live mode can add mail to the destination. Review Project Cockpit first.").color(ALERT)); } }); ui.add_space(14.0); ui.group(|ui| { ui.horizontal(|ui| { ui.heading("Execution journal"); ui.label(RichText::new(if self.running() { "streaming output" } else { "waiting" }).color(MUTED)); }); egui::ScrollArea::vertical().stick_to_bottom(true).max_height(180.0).show(ui, |ui| for line in &self.output { ui.label(RichText::new(line).monospace().size(12.0)); }); }); ui.add_space(8.0); ui.label(RichText::new("Passwords never enter the saved profile. The selected engine receives credentials only for the active process; local process visibility still matters.").size(11.0).color(MUTED)); });
        self.preview(ctx);
        self.bulk_dialog(ctx);
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
        form.source_password = "secret".into();
        form.destination_password = "unused".into();
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
        assert!(!args.iter().any(|arg| arg == "secret"));
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
    fn remote_arguments_are_shell_quoted() {
        assert_eq!(shell_quote("plain-value"), "plain-value");
        assert_eq!(shell_quote("pa ss'word"), "'pa ss'\\''word'");
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
    fn dovecot_plain_tls_maps_to_dovecot_no() {
        let mut form = dovecot_form();
        form.profile.source_tls = "plain".into();
        let (_, args) = form.command(true);
        assert!(args.iter().any(|arg| arg == "imapc_ssl=no"));
    }

    #[test]
    fn imapsync_runtime_plan_uses_child_environment_credentials() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        let prepared = form.prepared_command().unwrap();
        assert!(!prepared.args.contains(&"--password1".into()));
        assert!(!prepared.args.contains(&"--password2".into()));
        assert_eq!(
            prepared.env[0],
            ("IMAPSYNC_PASSWORD1".into(), "secret".into())
        );
        assert_eq!(
            prepared.env[1],
            ("IMAPSYNC_PASSWORD2".into(), "unused".into())
        );
        assert!(!prepared.args.iter().any(|arg| arg == "secret"));
        assert!(prepared.cleanup.is_empty());
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
        let evidence = parse_imapsync_evidence(&lines).unwrap();
        assert_eq!(evidence.confidence_percent(), 100);
        assert_eq!(evidence.source_messages, 42);
    }

    #[test]
    fn incomplete_imapsync_summary_is_not_evidence() {
        assert!(parse_imapsync_evidence(&["Detected 0 errors".into()]).is_none());
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
        let evidence = parse_dovecot_evidence(&source, &destination).unwrap();
        assert_eq!(evidence.source_folders, 2);
        assert_eq!(evidence.source_messages, 12);
        assert_eq!(evidence.confidence_percent(), 85);
    }
}
