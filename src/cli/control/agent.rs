//! `mailswiftsync control worker`: the agent that executes control-plane
//! leases on the host that holds the mailbox credentials.
//!
//! The control plane never sees credentials or the local plans themselves.
//! Approval is bound to local execution through a plan document: `worker
//! plan` exports the ledger project ID and a SHA-256 of each durable batch
//! row's effective plan, keyed by job ID. The operator stores that document
//! as the project plan, so a second person approves exactly those digests.
//! Before running a leased mailbox, the agent recomputes the row's digest
//! and refuses (reporting a failed assignment) unless it still matches.
//!
//! A run renews its lease in the background. If renewal is refused, the
//! lease is gone (expired, revoked, reassigned, or the plan approval
//! changed): the agent cancels the local run and reports nothing, because a
//! fenced completion would be refused anyway.

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub(super) const PLAN_FORMAT: &str = "mailswiftsync.worker-plan";
pub(super) const PLAN_FORMAT_VERSION: u64 = 1;
const DEFAULT_POLL_SECONDS: u64 = 30;
const MAX_POLL_SECONDS: u64 = 3_600;
const MAX_ERROR_BACKOFF: Duration = Duration::from_secs(300);
const MAX_DETAIL_BYTES: usize = 4_096;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

pub(super) const USAGE: &str = "Usage:
  mailswiftsync control worker plan <state.db> [--project <ledger-project-id>]
  mailswiftsync control worker enroll --server <url> --enrollment-token-file <file> --credential-out <file> [--capability <name>]... [--ca-cert <pem>]
  mailswiftsync control worker run <state.db> --server <url> --credential-file <file> [--ca-cert <pem>] [--lease-seconds 300] [--poll-seconds 30] [--once] [--acknowledge-destination-loss]";

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Command {
    Plan {
        state: PathBuf,
        project: Option<String>,
    },
    Enroll {
        server: url::Url,
        token_file: PathBuf,
        credential_out: PathBuf,
        capabilities: Vec<String>,
        ca_cert: Option<PathBuf>,
    },
    Run {
        state: PathBuf,
        server: url::Url,
        credential_file: PathBuf,
        ca_cert: Option<PathBuf>,
        lease_seconds: i64,
        poll: Duration,
        once: bool,
        acknowledge_destination_loss: bool,
    },
}

pub(super) fn parse(arguments: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut arguments = arguments.peekable();
    let action = arguments.next().ok_or(USAGE)?;
    let mut state = None;
    let mut options = std::collections::BTreeMap::<String, String>::new();
    let mut capabilities = Vec::new();
    let mut once = false;
    let mut acknowledge_destination_loss = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--once" if !once => once = true,
            "--acknowledge-destination-loss" if !acknowledge_destination_loss => {
                acknowledge_destination_loss = true;
            }
            "--capability" => capabilities.push(arguments.next().ok_or(USAGE)?),
            flag @ ("--project"
            | "--server"
            | "--enrollment-token-file"
            | "--credential-out"
            | "--credential-file"
            | "--ca-cert"
            | "--lease-seconds"
            | "--poll-seconds") => {
                let value = arguments.next().ok_or(USAGE)?;
                if options.insert(flag.to_owned(), value).is_some() {
                    return Err(format!("{flag} was given more than once"));
                }
            }
            flag if flag.starts_with("--") => {
                return Err(format!("unknown or repeated option {flag}\n{USAGE}"));
            }
            _ if state.is_none() => state = Some(PathBuf::from(argument)),
            _ => return Err(USAGE.into()),
        }
    }
    let allowed = |names: &[&str], flags_ok: bool| {
        let options_ok = options.keys().all(|key| names.contains(&key.as_str()));
        let flags_used = once || acknowledge_destination_loss || !capabilities.is_empty();
        (options_ok && (flags_ok || !flags_used))
            .then_some(())
            .ok_or_else(|| {
                format!("an option does not apply to `control worker {action}`\n{USAGE}")
            })
    };
    match action.as_str() {
        "plan" => {
            allowed(&["--project"], false)?;
            Ok(Command::Plan {
                state: state.ok_or(USAGE)?,
                project: options.remove("--project"),
            })
        }
        "enroll" => {
            if state.is_some() || once || acknowledge_destination_loss {
                return Err(USAGE.into());
            }
            allowed(
                &[
                    "--server",
                    "--enrollment-token-file",
                    "--credential-out",
                    "--ca-cert",
                ],
                true,
            )?;
            Ok(Command::Enroll {
                server: server_url(&options.remove("--server").ok_or(USAGE)?)?,
                token_file: options
                    .remove("--enrollment-token-file")
                    .ok_or(USAGE)?
                    .into(),
                credential_out: options.remove("--credential-out").ok_or(USAGE)?.into(),
                capabilities,
                ca_cert: options.remove("--ca-cert").map(PathBuf::from),
            })
        }
        "run" => {
            if !capabilities.is_empty() {
                return Err(USAGE.into());
            }
            allowed(
                &[
                    "--server",
                    "--credential-file",
                    "--ca-cert",
                    "--lease-seconds",
                    "--poll-seconds",
                ],
                true,
            )?;
            let lease_seconds = match options.remove("--lease-seconds") {
                None => super::workers::DEFAULT_LEASE_SECONDS,
                Some(value) => value
                    .parse::<i64>()
                    .ok()
                    .filter(|seconds| (30..=super::workers::MAX_LEASE_SECONDS).contains(seconds))
                    .ok_or_else(|| {
                        format!(
                            "--lease-seconds must be 30 to {}",
                            super::workers::MAX_LEASE_SECONDS
                        )
                    })?,
            };
            let poll_seconds = match options.remove("--poll-seconds") {
                None => DEFAULT_POLL_SECONDS,
                Some(value) => value
                    .parse::<u64>()
                    .ok()
                    .filter(|seconds| (1..=MAX_POLL_SECONDS).contains(seconds))
                    .ok_or_else(|| format!("--poll-seconds must be 1 to {MAX_POLL_SECONDS}"))?,
            };
            Ok(Command::Run {
                state: state.ok_or(USAGE)?,
                server: server_url(&options.remove("--server").ok_or(USAGE)?)?,
                credential_file: options.remove("--credential-file").ok_or(USAGE)?.into(),
                ca_cert: options.remove("--ca-cert").map(PathBuf::from),
                lease_seconds,
                poll: Duration::from_secs(poll_seconds),
                once,
                acknowledge_destination_loss,
            })
        }
        _ => Err(USAGE.into()),
    }
}

/// The control-plane base URL. Bearer credentials cross it, so plain HTTP
/// is accepted only to a loopback address.
fn server_url(value: &str) -> Result<url::Url, String> {
    let mut url =
        url::Url::parse(value).map_err(|_| format!("--server is not a valid URL: {value}"))?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        "http" => {
            return Err(
                "--server must use https: worker credentials would cross the network in plain text"
                    .into(),
            );
        }
        _ => return Err("--server must be an http(s) URL".into()),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("--server must not embed credentials".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("--server must not carry a query or fragment".into());
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

pub(super) fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Plan { state, project } => {
            let plan = export_plan(&state, project.as_deref())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&plan).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        Command::Enroll {
            server,
            token_file,
            credential_out,
            capabilities,
            ca_cert,
        } => {
            let token = crate::credentials::read_secret_file(&token_file)
                .map_err(|error| format!("enrollment token file refused: {error}"))?;
            let client = HttpControl::new(server, token.as_str().trim(), ca_cert.as_deref())?;
            let (worker_id, credential) = client.enroll(&capabilities)?;
            crate::credentials::write_secret_file(&credential_out, &credential).map_err(
                |error| {
                    format!(
                        "could not create {} (it must not exist yet): {error}",
                        credential_out.display()
                    )
                },
            )?;
            println!(
                "Enrolled worker {worker_id}; its credential is in {}",
                credential_out.display()
            );
            Ok(())
        }
        Command::Run {
            state,
            server,
            credential_file,
            ca_cert,
            lease_seconds,
            poll,
            once,
            acknowledge_destination_loss,
        } => {
            let credential = crate::credentials::read_secret_file(&credential_file)
                .map_err(|error| format!("worker credential file refused: {error}"))?;
            let credential = credential.as_str().trim();
            if super::authn::TokenKind::of(credential) != Some(super::authn::TokenKind::Worker) {
                return Err("the credential file does not hold a worker credential".into());
            }
            let control = HttpControl::new(server, credential, ca_cert.as_deref())?;
            let executor = LedgerExecutor {
                state,
                acknowledge_destination_loss,
            };
            let config = AgentConfig::new(lease_seconds);
            run_loop(&control, &executor, &config, poll, once)
        }
    }
}

/// Digest of one durable batch row's effective plan: its (shared or own)
/// configuration and its per-row overrides, length-prefixed so the two
/// fields cannot be shifted into each other.
pub(super) fn row_plan_sha256(config: Option<&str>, overrides: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"mailswiftsync-worker-plan-v1\0");
    for field in [config, overrides] {
        match field {
            None => hasher.update([0]),
            Some(text) => {
                hasher.update([1]);
                hasher.update((text.len() as u64).to_be_bytes());
                hasher.update(text.as_bytes());
            }
        }
    }
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// The plan document for one ledger project's durable batch.
pub(super) fn export_plan(state: &Path, project: Option<&str>) -> Result<Value, String> {
    let store = crate::core::StateStore::open_readonly(state)
        .map_err(|error| format!("could not open {}: {error}", state.display()))?;
    let project = match project {
        Some(id) => store.project(id),
        None => store.latest_project(),
    }
    .map_err(|error| error.to_string())?
    .ok_or("the ledger has no such project")?;
    let mut mailboxes = serde_json::Map::new();
    store.queue_plan_scan(&project.id, |row| {
        mailboxes.insert(
            row.id,
            json!({
                "label": row.label,
                "plan_sha256": row_plan_sha256(row.config.as_deref(), row.row_overrides.as_deref()),
            }),
        );
        Ok(())
    })?;
    if mailboxes.is_empty() {
        return Err(format!(
            "project {} has no durable batch mailboxes; import and validate a batch first",
            project.id
        ));
    }
    Ok(json!({
        "format": PLAN_FORMAT,
        "format_version": PLAN_FORMAT_VERSION,
        "ledger_project_id": project.id,
        "ledger_project_name": project.name,
        "mailboxes": mailboxes,
    }))
}

/// What the approved plan says about one leased mailbox.
fn approved_digest<'a>(plan: &'a Value, job_id: &str) -> Result<(&'a str, &'a str), String> {
    if plan.get("format").and_then(Value::as_str) != Some(PLAN_FORMAT)
        || plan.get("format_version").and_then(Value::as_u64) != Some(PLAN_FORMAT_VERSION)
    {
        return Err(format!(
            "the approved plan is not a {PLAN_FORMAT} v{PLAN_FORMAT_VERSION} document; export it with `control worker plan`"
        ));
    }
    let project = plan
        .get("ledger_project_id")
        .and_then(Value::as_str)
        .ok_or("the approved plan names no ledger project")?;
    let digest = plan
        .get("mailboxes")
        .and_then(|mailboxes| mailboxes.get(job_id))
        .and_then(|mailbox| mailbox.get("plan_sha256"))
        .and_then(Value::as_str)
        .ok_or("the approved plan does not include this mailbox")?;
    Ok((project, digest))
}

/// One leased assignment, as the service sends it.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Leased {
    pub(super) assignment_id: String,
    pub(super) fencing_token: i64,
    pub(super) plan_revision: i64,
    pub(super) plan: Value,
    pub(super) mailbox: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ControlError {
    /// 409: this worker no longer holds the lease.
    LeaseLost,
    /// 401: the worker credential was refused (revoked or unknown).
    Unauthorized,
    Other(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LeaseLost => formatter.write_str("the lease is no longer held"),
            Self::Unauthorized => formatter
                .write_str("the control plane refused the worker credential (revoked or unknown)"),
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

/// The worker endpoints of the control plane.
pub(super) trait Control: Sync {
    fn lease(&self, seconds: i64) -> Result<Option<Leased>, ControlError>;
    fn renew(&self, assignment: &str, fencing_token: i64, seconds: i64)
    -> Result<(), ControlError>;
    fn complete(
        &self,
        assignment: &str,
        fencing_token: i64,
        outcome: &str,
        result: &Value,
    ) -> Result<(), ControlError>;
}

/// Local execution against the worker's ledger.
pub(super) trait Executor: Sync {
    /// The current plan digest of a durable batch row, or `None` if the row
    /// is not in that project.
    fn plan_sha256(&self, project: &str, job_id: &str) -> Result<Option<String>, String>;
    /// Run one row to a verified terminal state. Raising `stop` cancels it.
    fn execute(&self, project: &str, job_id: &str, stop: &AtomicBool) -> Result<String, String>;
}

struct LedgerExecutor {
    state: PathBuf,
    acknowledge_destination_loss: bool,
}

impl Executor for LedgerExecutor {
    fn plan_sha256(&self, project: &str, job_id: &str) -> Result<Option<String>, String> {
        let store = crate::core::StateStore::open_readonly(&self.state)
            .map_err(|error| format!("could not open the ledger: {error}"))?;
        let rows = store
            .queue_plans(project, &[job_id.to_owned()])
            .map_err(|error| format!("could not read the batch plan: {error}"))?;
        Ok(rows
            .first()
            .map(|row| row_plan_sha256(row.config.as_deref(), row.row_overrides.as_deref())))
    }

    fn execute(&self, project: &str, job_id: &str, stop: &AtomicBool) -> Result<String, String> {
        crate::headless::headless_worker_execute(
            &self.state,
            project,
            job_id,
            self.acknowledge_destination_loss,
            stop,
        )
    }
}

pub(super) struct AgentConfig {
    lease_seconds: i64,
    renew_every: Duration,
    completion_retries: u32,
    completion_backoff: Duration,
}

impl AgentConfig {
    pub(super) fn new(lease_seconds: i64) -> Self {
        Self {
            lease_seconds,
            // Three chances to renew before the lease runs out.
            renew_every: Duration::from_secs((lease_seconds / 3).max(1) as u64),
            completion_retries: 3,
            completion_backoff: Duration::from_secs(2),
        }
    }
}

/// What happened to one lease attempt.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Handled {
    Idle,
    Completed {
        assignment: String,
    },
    Failed {
        assignment: String,
        reason: String,
    },
    /// The lease was lost mid-run; the local run was cancelled and nothing
    /// was reported.
    LeaseLost {
        assignment: String,
    },
}

/// Lease one assignment, run it, and report the outcome.
pub(super) fn handle_one(
    control: &impl Control,
    executor: &impl Executor,
    config: &AgentConfig,
) -> Result<Handled, ControlError> {
    // The service starts the lease when it handles the request, so the
    // local clock starts before sending it. The local deadline can then only
    // fall before the service's, never after.
    let requested = Instant::now();
    let Some(lease) = control.lease(config.lease_seconds)? else {
        return Ok(Handled::Idle);
    };
    let base = |detail: &str| {
        json!({
            "job_id": lease.mailbox,
            "plan_revision": lease.plan_revision,
            "detail": crate::truncate_utf8(detail, MAX_DETAIL_BYTES),
        })
    };
    // A transient failure here would discard a finished run's result and
    // leave the mailbox to run again after the lease expires, so retry a
    // few times; fencing makes a repeated completion harmless.
    let finish = |outcome: &str, detail: &str| -> Result<(), ControlError> {
        let result = base(detail);
        let mut attempt = 0;
        loop {
            match control.complete(&lease.assignment_id, lease.fencing_token, outcome, &result) {
                Err(ControlError::Other(message)) if attempt < config.completion_retries => {
                    attempt += 1;
                    eprintln!(
                        "Worker: could not report assignment {} ({message}); retrying",
                        lease.assignment_id
                    );
                    std::thread::sleep(config.completion_backoff * attempt);
                }
                answer => return answer,
            }
        }
    };
    let lost = || Handled::LeaseLost {
        assignment: lease.assignment_id.clone(),
    };
    let project = match check_binding(executor, &lease) {
        Ok(project) => project,
        Err(reason) => {
            return match finish("failed", &format!("refused before starting: {reason}")) {
                Ok(()) => Ok(Handled::Failed {
                    assignment: lease.assignment_id.clone(),
                    reason,
                }),
                Err(ControlError::LeaseLost) => Ok(lost()),
                Err(error) => Err(error),
            };
        }
    };

    let stop = AtomicBool::new(false);
    let lease_lost = AtomicBool::new(false);
    let (done, finished) = crossbeam_channel::bounded::<()>(0);
    let outcome = std::thread::scope(|scope| {
        scope.spawn(|| {
            keep_renewing(
                control,
                config,
                &lease,
                requested,
                &finished,
                &stop,
                &lease_lost,
            )
        });
        let outcome = executor.execute(&project, &lease.mailbox, &stop);
        drop(done);
        outcome
    });
    if lease_lost.load(Ordering::SeqCst) {
        return Ok(lost());
    }
    let reported = match &outcome {
        Ok(detail) => finish("completed", detail),
        Err(detail) => finish("failed", detail),
    };
    match reported {
        Ok(()) => Ok(match outcome {
            Ok(_) => Handled::Completed {
                assignment: lease.assignment_id.clone(),
            },
            Err(reason) => Handled::Failed {
                assignment: lease.assignment_id.clone(),
                reason,
            },
        }),
        Err(ControlError::LeaseLost) => Ok(lost()),
        Err(error) => Err(error),
    }
}

/// The ledger project to run, once the leased plan is proven to be the one
/// approved for this worker's local row.
fn check_binding(executor: &impl Executor, lease: &Leased) -> Result<String, String> {
    let (project, approved) = approved_digest(&lease.plan, &lease.mailbox)?;
    let local = executor
        .plan_sha256(project, &lease.mailbox)?
        .ok_or_else(|| {
            format!(
                "this worker's ledger has no batch mailbox {} in project {project}",
                lease.mailbox
            )
        })?;
    if local != approved {
        return Err(
            "the local plan for this mailbox differs from the approved plan; export the plan again and have it re-approved"
                .into(),
        );
    }
    Ok(project.to_owned())
}

/// Renew the lease until the run finishes. A refused renewal, or failing to
/// renew before the lease would have expired, raises `stop`.
fn keep_renewing(
    control: &impl Control,
    config: &AgentConfig,
    lease: &Leased,
    requested: Instant,
    finished: &crossbeam_channel::Receiver<()>,
    stop: &AtomicBool,
    lease_lost: &AtomicBool,
) {
    // Measured locally from when the lease was requested, so neither clock
    // skew nor network delay can extend it past the service's deadline.
    let mut expires = requested + Duration::from_secs(config.lease_seconds as u64);
    let give_up = |reason: &str| {
        eprintln!(
            "Worker: stopping assignment {}: {reason}",
            lease.assignment_id
        );
        lease_lost.store(true, Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
    };
    loop {
        match finished.recv_timeout(config.renew_every) {
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }
        let attempted = Instant::now();
        match control.renew(
            &lease.assignment_id,
            lease.fencing_token,
            config.lease_seconds,
        ) {
            Ok(()) => {
                expires = attempted + Duration::from_secs(config.lease_seconds as u64);
            }
            Err(error @ (ControlError::LeaseLost | ControlError::Unauthorized)) => {
                return give_up(&error.to_string());
            }
            Err(ControlError::Other(message)) => {
                eprintln!(
                    "Worker: could not renew assignment {} ({message}); retrying",
                    lease.assignment_id
                );
                if Instant::now() + config.renew_every >= expires {
                    return give_up("the lease could not be renewed before it expired");
                }
            }
        }
    }
}

fn run_loop(
    control: &impl Control,
    executor: &impl Executor,
    config: &AgentConfig,
    poll: Duration,
    once: bool,
) -> Result<(), String> {
    let mut failures = 0_u32;
    loop {
        let wait = match handle_one(control, executor, config) {
            Ok(handled) => {
                failures = 0;
                match &handled {
                    Handled::Idle => {}
                    Handled::Completed { assignment } => {
                        eprintln!("Worker: assignment {assignment} completed");
                    }
                    Handled::Failed { assignment, reason } => {
                        eprintln!("Worker: assignment {assignment} failed: {reason}");
                    }
                    Handled::LeaseLost { assignment } => {
                        eprintln!(
                            "Worker: lost the lease on assignment {assignment}; the local run was cancelled and not reported"
                        );
                    }
                }
                if once {
                    return Ok(());
                }
                match handled {
                    Handled::Idle => poll,
                    _ => Duration::ZERO,
                }
            }
            Err(ControlError::Unauthorized) => {
                return Err(ControlError::Unauthorized.to_string());
            }
            Err(error) => {
                if once {
                    return Err(error.to_string());
                }
                failures = failures.saturating_add(1);
                let backoff = poll
                    .saturating_mul(1 << failures.min(8))
                    .min(MAX_ERROR_BACKOFF);
                eprintln!(
                    "Worker: control plane request failed ({error}); retrying in {}s",
                    backoff.as_secs()
                );
                backoff
            }
        };
        std::thread::sleep(wait);
    }
}

/// The control plane over HTTP(S).
struct HttpControl {
    client: reqwest::blocking::Client,
    base: url::Url,
    bearer: Zeroizing<String>,
}

impl HttpControl {
    fn new(base: url::Url, bearer: &str, ca_cert: Option<&Path>) -> Result<Self, String> {
        let mut builder = reqwest::blocking::Client::builder()
            .use_rustls_tls()
            // The bearer credential goes only to the configured service:
            // no ambient proxy and no redirects.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(HTTP_TIMEOUT)
            .timeout(HTTP_TIMEOUT)
            .user_agent(concat!("MailSwiftSync-worker/", env!("CARGO_PKG_VERSION")));
        if let Some(path) = ca_cert {
            let pem = std::fs::read(path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?;
            let certificate = reqwest::Certificate::from_pem(&pem)
                .map_err(|error| format!("{} is not a PEM certificate: {error}", path.display()))?;
            builder = builder.add_root_certificate(certificate);
        }
        Ok(Self {
            client: builder
                .build()
                .map_err(|error| format!("could not prepare the HTTP client: {error}"))?,
            base,
            bearer: Zeroizing::new(bearer.to_owned()),
        })
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, ControlError> {
        let url = self
            .base
            .join(path)
            .map_err(|error| ControlError::Other(error.to_string()))?;
        let response = self
            .client
            .post(url)
            .bearer_auth(self.bearer.as_str())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .map_err(|error| ControlError::Other(format!("request failed: {error}")))?;
        let status = response.status().as_u16();
        // Responses are small JSON documents; bound the read regardless.
        let mut text = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(response, MAX_RESPONSE_BYTES),
            &mut text,
        )
        .map_err(|error| ControlError::Other(format!("could not read the response: {error}")))?;
        let body: Value = serde_json::from_slice(&text).unwrap_or(Value::Null);
        match status {
            200..=299 => Ok(body),
            401 => Err(ControlError::Unauthorized),
            409 => Err(ControlError::LeaseLost),
            _ => Err(ControlError::Other(format!(
                "the control plane answered {status}: {}",
                body.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("no detail")
            ))),
        }
    }

    fn enroll(&self, capabilities: &[String]) -> Result<(String, Zeroizing<String>), String> {
        let body = match self.post("v1/worker/enroll", &json!({ "capabilities": capabilities })) {
            Ok(body) => body,
            Err(ControlError::Unauthorized) => {
                return Err(
                    "the enrollment token was refused (already used, expired, or unknown)".into(),
                );
            }
            Err(error) => return Err(error.to_string()),
        };
        let credential = body
            .get("credential")
            .and_then(Value::as_str)
            .ok_or("the control plane returned no worker credential")?;
        let worker = body
            .pointer("/worker/id")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)");
        Ok((worker.to_owned(), Zeroizing::new(credential.to_owned())))
    }
}

impl Control for HttpControl {
    fn lease(&self, seconds: i64) -> Result<Option<Leased>, ControlError> {
        let body = self.post("v1/worker/lease", &json!({ "lease_seconds": seconds }))?;
        match body.get("lease") {
            None | Some(Value::Null) => Ok(None),
            Some(lease) => serde_json::from_value(lease.clone())
                .map(Some)
                .map_err(|error| ControlError::Other(format!("malformed lease: {error}"))),
        }
    }

    fn renew(
        &self,
        assignment: &str,
        fencing_token: i64,
        seconds: i64,
    ) -> Result<(), ControlError> {
        self.post(
            &format!("v1/worker/assignments/{assignment}/renew"),
            &json!({ "fencing_token": fencing_token, "lease_seconds": seconds }),
        )
        .map(drop)
    }

    fn complete(
        &self,
        assignment: &str,
        fencing_token: i64,
        outcome: &str,
        result: &Value,
    ) -> Result<(), ControlError> {
        self.post(
            &format!("v1/worker/assignments/{assignment}/complete"),
            &json!({ "fencing_token": fencing_token, "outcome": outcome, "result": result }),
        )
        .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::{Actor, Store};
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    fn strings(values: &[&str]) -> impl Iterator<Item = String> {
        values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn parses_worker_commands_and_refuses_plain_http_off_loopback() {
        assert_eq!(
            parse(strings(&["plan", "s.db"])).unwrap(),
            Command::Plan {
                state: "s.db".into(),
                project: None
            }
        );
        let Command::Run {
            server,
            lease_seconds,
            poll,
            once,
            ..
        } = parse(strings(&[
            "run",
            "s.db",
            "--server",
            "http://127.0.0.1:9470",
            "--credential-file",
            "c",
            "--once",
        ]))
        .unwrap()
        else {
            panic!("expected run");
        };
        assert_eq!(server.as_str(), "http://127.0.0.1:9470/");
        assert_eq!(lease_seconds, super::super::workers::DEFAULT_LEASE_SECONDS);
        assert_eq!(poll, Duration::from_secs(DEFAULT_POLL_SECONDS));
        assert!(once);
        assert!(
            parse(strings(&[
                "run",
                "s.db",
                "--server",
                "https://control.msp.example/base",
                "--credential-file",
                "c",
            ]))
            .is_ok()
        );
        for refused in [
            &[
                "run",
                "s.db",
                "--server",
                "http://control.msp.example",
                "--credential-file",
                "c",
            ][..],
            &[
                "run",
                "s.db",
                "--server",
                "https://u:p@control.msp.example",
                "--credential-file",
                "c",
            ],
            &[
                "run",
                "s.db",
                "--server",
                "ftp://127.0.0.1",
                "--credential-file",
                "c",
            ],
            &[
                "run",
                "s.db",
                "--server",
                "https://h.example",
                "--credential-file",
                "c",
                "--lease-seconds",
                "10",
            ],
            &[
                "run",
                "s.db",
                "--server",
                "https://h.example",
                "--credential-file",
                "c",
                "--capability",
                "x",
            ],
            &["run", "s.db", "--credential-file", "c"],
            &["plan", "s.db", "--once"],
            &["plan", "s.db", "--server", "https://h.example"],
            &[
                "enroll",
                "s.db",
                "--server",
                "https://h.example",
                "--enrollment-token-file",
                "t",
                "--credential-out",
                "c",
            ],
            &["bogus", "s.db"],
        ] {
            assert!(parse(strings(refused)).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn row_digests_cover_both_fields_unambiguously() {
        let digests = [
            row_plan_sha256(Some("ab"), Some("c")),
            row_plan_sha256(Some("a"), Some("bc")),
            row_plan_sha256(Some("abc"), None),
            row_plan_sha256(Some("abc"), Some("")),
            row_plan_sha256(None, Some("abc")),
        ];
        let distinct = digests.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(distinct.len(), digests.len());
        assert_eq!(digests[0], row_plan_sha256(Some("ab"), Some("c")));
        assert_eq!(digests[0].len(), 64);
    }

    fn batch_ledger() -> (PathBuf, String, Vec<String>) {
        let directory = std::env::temp_dir().join(format!("mss-agent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = directory.join("state.db");
        let store = crate::core::StateStore::open(&path).unwrap();
        let rows = (0..2)
            .map(|index| {
                let user = format!("user{index}@example.test");
                let config =
                    format!("destination_host = \"new.example\"\ndestination_user = \"{user}\"\n");
                crate::core::QueueInsert {
                    source_mailbox: user.clone(),
                    destination_mailbox: user.clone(),
                    destination_identity: crate::core::destination_identity_from_parts(
                        &user,
                        &user,
                        "new.example",
                        "",
                        "",
                    ),
                    config,
                    batch_plan_config: None,
                    row_overrides: None,
                    facts: crate::core::QueueRowFacts {
                        label: format!("Row {index}"),
                        ..Default::default()
                    },
                }
            })
            .collect::<Vec<_>>();
        let (project, ids) = store
            .create_batch_queue("queue", "old.example", "new.example", &rows)
            .unwrap();
        (path, project.id, ids)
    }

    #[test]
    fn plan_documents_bind_each_local_row_and_detect_changes() {
        let (path, project, ids) = batch_ledger();
        let plan = export_plan(&path, None).unwrap();
        assert_eq!(plan["format"], PLAN_FORMAT);
        assert_eq!(plan["ledger_project_id"], project.as_str());
        assert_eq!(plan["mailboxes"][&ids[1]]["label"], "Row 1");
        // The control plane accepts it: no credential-like field names.
        super::super::store::validate_plan(&plan).unwrap();
        assert_eq!(export_plan(&path, Some(&project)).unwrap(), plan);
        assert!(export_plan(&path, Some("missing")).is_err());

        let executor = LedgerExecutor {
            state: path.clone(),
            acknowledge_destination_loss: false,
        };
        let lease = |mailbox: &str, plan: Value| Leased {
            assignment_id: "asg_1".into(),
            fencing_token: 1,
            plan_revision: 1,
            plan,
            mailbox: mailbox.into(),
        };
        assert_eq!(
            check_binding(&executor, &lease(&ids[0], plan.clone())).unwrap(),
            project
        );
        assert!(check_binding(&executor, &lease("not-a-row", plan.clone())).is_err());
        let mut other_project = plan.clone();
        other_project["ledger_project_id"] = json!("elsewhere");
        assert!(check_binding(&executor, &lease(&ids[0], other_project)).is_err());
        let mut old_format = plan.clone();
        old_format["format_version"] = json!(0);
        assert!(check_binding(&executor, &lease(&ids[0], old_format)).is_err());

        // A local plan edit after approval is refused.
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute(
                "UPDATE mailbox_jobs SET row_overrides='changed' WHERE id=?1",
                [&ids[0]],
            )
            .unwrap();
        let refused = check_binding(&executor, &lease(&ids[0], plan.clone())).unwrap_err();
        assert!(
            refused.contains("differs from the approved plan"),
            "{refused}"
        );
        assert!(check_binding(&executor, &lease(&ids[1], plan)).is_ok());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A scripted control plane recording what the agent sent.
    #[derive(Default)]
    struct FakeControl {
        lease: Mutex<Option<Leased>>,
        renew_answer: Mutex<Option<ControlError>>,
        renewals: AtomicUsize,
        completions: Mutex<Vec<(String, Value)>>,
        failing_completions: AtomicUsize,
    }

    impl Control for FakeControl {
        fn lease(&self, _: i64) -> Result<Option<Leased>, ControlError> {
            Ok(self.lease.lock().unwrap().take())
        }
        fn renew(&self, _: &str, _: i64, _: i64) -> Result<(), ControlError> {
            self.renewals.fetch_add(1, Ordering::SeqCst);
            match self.renew_answer.lock().unwrap().take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
        fn complete(
            &self,
            _: &str,
            _: i64,
            outcome: &str,
            result: &Value,
        ) -> Result<(), ControlError> {
            if self
                .failing_completions
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
            {
                return Err(ControlError::Other("connection reset".into()));
            }
            self.completions
                .lock()
                .unwrap()
                .push((outcome.to_owned(), result.clone()));
            Ok(())
        }
    }

    /// Runs until stopped (or returns at once), recording what it ran.
    struct FakeExecutor {
        digest: Option<String>,
        runs: AtomicUsize,
        wait_for_stop: bool,
        outcome: Result<String, String>,
    }

    impl Executor for FakeExecutor {
        fn plan_sha256(&self, _: &str, _: &str) -> Result<Option<String>, String> {
            Ok(self.digest.clone())
        }
        fn execute(&self, _: &str, _: &str, stop: &AtomicBool) -> Result<String, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            if self.wait_for_stop {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !stop.load(Ordering::SeqCst) {
                    assert!(Instant::now() < deadline, "the agent never raised stop");
                    std::thread::sleep(Duration::from_millis(5));
                }
                return Err(crate::headless::STOPPED_BEFORE_COMPLETION.into());
            }
            self.outcome.clone()
        }
    }

    fn approved_plan(digest: &str) -> Value {
        json!({
            "format": PLAN_FORMAT,
            "format_version": PLAN_FORMAT_VERSION,
            "ledger_project_id": "local-project",
            "mailboxes": { "job-1": { "plan_sha256": digest } },
        })
    }

    fn leased(digest: &str) -> Leased {
        Leased {
            assignment_id: "asg_1".into(),
            fencing_token: 7,
            plan_revision: 2,
            plan: approved_plan(digest),
            mailbox: "job-1".into(),
        }
    }

    fn fast() -> AgentConfig {
        AgentConfig {
            lease_seconds: 300,
            renew_every: Duration::from_millis(10),
            completion_retries: 3,
            completion_backoff: Duration::from_millis(1),
        }
    }

    #[test]
    fn a_matching_assignment_runs_and_reports_its_outcome() {
        for (outcome, reported) in [
            (Ok::<_, String>("verified".to_owned()), "completed"),
            (Err("engine failed".to_owned()), "failed"),
        ] {
            let control = FakeControl::default();
            *control.lease.lock().unwrap() = Some(leased("d1"));
            let executor = FakeExecutor {
                digest: Some("d1".into()),
                runs: AtomicUsize::new(0),
                wait_for_stop: false,
                outcome,
            };
            let handled = handle_one(&control, &executor, &fast()).unwrap();
            assert!(matches!(
                (&handled, reported),
                (Handled::Completed { .. }, "completed") | (Handled::Failed { .. }, "failed")
            ));
            assert_eq!(executor.runs.load(Ordering::SeqCst), 1);
            let completions = control.completions.lock().unwrap();
            assert_eq!(completions.len(), 1);
            assert_eq!(completions[0].0, reported);
            assert_eq!(completions[0].1["job_id"], "job-1");
            assert_eq!(completions[0].1["plan_revision"], 2);
        }
        let control = FakeControl::default();
        let idle = FakeExecutor {
            digest: None,
            runs: AtomicUsize::new(0),
            wait_for_stop: false,
            outcome: Ok(String::new()),
        };
        assert_eq!(handle_one(&control, &idle, &fast()).unwrap(), Handled::Idle);
    }

    #[test]
    fn a_transient_reporting_failure_is_retried() {
        let control = FakeControl::default();
        *control.lease.lock().unwrap() = Some(leased("d1"));
        control.failing_completions.store(2, Ordering::SeqCst);
        let executor = FakeExecutor {
            digest: Some("d1".into()),
            runs: AtomicUsize::new(0),
            wait_for_stop: false,
            outcome: Ok("verified".into()),
        };
        assert!(matches!(
            handle_one(&control, &executor, &fast()).unwrap(),
            Handled::Completed { .. }
        ));
        assert_eq!(control.completions.lock().unwrap().len(), 1);

        *control.lease.lock().unwrap() = Some(leased("d1"));
        control.failing_completions.store(10, Ordering::SeqCst);
        assert!(matches!(
            handle_one(&control, &executor, &fast()),
            Err(ControlError::Other(_))
        ));
    }

    #[test]
    fn a_plan_mismatch_fails_the_assignment_without_running_anything() {
        for local in [Some("other".to_owned()), None] {
            let control = FakeControl::default();
            *control.lease.lock().unwrap() = Some(leased("approved"));
            let executor = FakeExecutor {
                digest: local,
                runs: AtomicUsize::new(0),
                wait_for_stop: false,
                outcome: Ok(String::new()),
            };
            assert!(matches!(
                handle_one(&control, &executor, &fast()).unwrap(),
                Handled::Failed { .. }
            ));
            assert_eq!(executor.runs.load(Ordering::SeqCst), 0);
            let completions = control.completions.lock().unwrap();
            assert_eq!(completions[0].0, "failed");
            assert!(
                completions[0].1["detail"]
                    .as_str()
                    .unwrap()
                    .starts_with("refused before starting")
            );
        }
    }

    #[test]
    fn losing_the_lease_stops_the_run_and_reports_nothing() {
        for refusal in [ControlError::LeaseLost, ControlError::Unauthorized] {
            let control = FakeControl::default();
            *control.lease.lock().unwrap() = Some(leased("d1"));
            *control.renew_answer.lock().unwrap() = Some(refusal);
            let executor = FakeExecutor {
                digest: Some("d1".into()),
                runs: AtomicUsize::new(0),
                wait_for_stop: true,
                outcome: Ok(String::new()),
            };
            assert_eq!(
                handle_one(&control, &executor, &fast()).unwrap(),
                Handled::LeaseLost {
                    assignment: "asg_1".into()
                }
            );
            assert!(control.completions.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn unreachable_renewals_stop_the_run_before_the_lease_expires() {
        struct Unreachable(FakeControl);
        impl Control for Unreachable {
            fn lease(&self, seconds: i64) -> Result<Option<Leased>, ControlError> {
                self.0.lease(seconds)
            }
            fn renew(&self, _: &str, _: i64, _: i64) -> Result<(), ControlError> {
                Err(ControlError::Other("connection refused".into()))
            }
            fn complete(&self, a: &str, f: i64, o: &str, r: &Value) -> Result<(), ControlError> {
                self.0.complete(a, f, o, r)
            }
        }
        let control = Unreachable(FakeControl::default());
        *control.0.lease.lock().unwrap() = Some(leased("d1"));
        let executor = FakeExecutor {
            digest: Some("d1".into()),
            runs: AtomicUsize::new(0),
            wait_for_stop: true,
            outcome: Ok(String::new()),
        };
        // A 1 s lease renewed every 0.3 s: the run must stop within it.
        let config = AgentConfig {
            lease_seconds: 1,
            renew_every: Duration::from_millis(300),
            ..fast()
        };
        let started = Instant::now();
        assert!(matches!(
            handle_one(&control, &executor, &config).unwrap(),
            Handled::LeaseLost { .. }
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(control.0.completions.lock().unwrap().is_empty());
    }

    #[test]
    fn the_agent_completes_a_real_lease_over_http() {
        let store = Store::in_memory();
        let password = super::super::authn::test_secret();
        let admin_id = store
            .bootstrap_administrator("admin@msp.example", "Admin", &password)
            .unwrap();
        let admin = Actor {
            request_id: "test",
            label: format!("user:{admin_id}"),
        };
        let editor = Actor {
            request_id: "test",
            label: "user:usr_editor".into(),
        };
        let tenant = store.create_tenant("Acme", &admin).unwrap().id;
        let project = store
            .create_project(&tenant, "Move", &approved_plan("d1"), &editor)
            .unwrap();
        store
            .approve_project(
                &tenant,
                &project.id,
                project.plan_revision,
                &admin_id,
                &admin,
            )
            .unwrap();
        store
            .create_assignments(&tenant, &project.id, &["job-1".into()], &admin)
            .unwrap();
        let (enrollment, _) = store
            .create_enrollment(&tenant, "agent-1", 10, &admin)
            .unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let context = std::sync::Arc::new(super::super::api::Context { store, oidc: None });
        let server_context = std::sync::Arc::clone(&context);
        std::thread::spawn(move || super::super::server::serve(listener, server_context, None));
        let base = server_url(&format!("http://{address}")).unwrap();

        let (_, credential) = HttpControl::new(base.clone(), &enrollment, None)
            .unwrap()
            .enroll(&["imapsync".into()])
            .unwrap();
        let control = HttpControl::new(base.clone(), &credential, None).unwrap();
        let executor = FakeExecutor {
            digest: Some("d1".into()),
            runs: AtomicUsize::new(0),
            wait_for_stop: false,
            outcome: Ok("verified".into()),
        };
        assert!(matches!(
            handle_one(&control, &executor, &fast()).unwrap(),
            Handled::Completed { .. }
        ));
        assert_eq!(
            handle_one(&control, &executor, &fast()).unwrap(),
            Handled::Idle
        );
        let [assignment] = &context.store.assignments(&tenant, &project.id).unwrap()[..] else {
            panic!("one assignment");
        };
        assert_eq!(assignment.state, "completed");
        assert_eq!(assignment.result.as_ref().unwrap()["detail"], "verified");

        // A user credential is not a worker credential.
        let refused = HttpControl::new(base, "mss_w_unknown", None).unwrap();
        assert!(matches!(
            refused.lease(300),
            Err(ControlError::Unauthorized)
        ));
    }
}
