//! Fleet API: a long-running HTTP listener that serves cached
//! `fleet-status` JSON and fleet-wide Prometheus metrics to monitoring and
//! PSA tooling, plus wave listing and, for named operators only, wave
//! approval. Reads never take an instance lock; `supervise` remains the
//! per-ledger worker.

mod controls;
mod http;
mod tls;

use super::*;
use crate::headless::FleetStatus;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

const SERVE_USAGE: &str = "Usage: mailswiftsync serve <directory> --token-file <file> [--operators-file <file>] [--listen 127.0.0.1:9464] [--tls-cert <pem> --tls-key <pem>] [--refresh-seconds 5..3600] [--allow-non-loopback]";
const DEFAULT_LISTEN: &str = "127.0.0.1:9464";
const DEFAULT_REFRESH_SECONDS: u64 = 30;
const MIN_TOKEN_BYTES: usize = 32;
const MAX_REQUEST_HEAD_BYTES: usize = 8 * 1024;
const MAX_CONCURRENT_CONNECTIONS: usize = 16;
/// Over-ceiling connections waiting for their 503.
const MAX_QUEUED_REFUSALS: usize = 4 * MAX_CONCURRENT_CONNECTIONS;
/// How long a refused connection may take to read its 503 and close.
const BUSY_LINGER: Duration = Duration::from_secs(1);
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ServeArguments {
    pub(super) root: PathBuf,
    pub(super) token_file: PathBuf,
    pub(super) operators_file: Option<PathBuf>,
    pub(super) listen: SocketAddr,
    pub(super) tls: Option<TlsFiles>,
    pub(super) refresh_seconds: u64,
    pub(super) allow_non_loopback: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct TlsFiles {
    pub(super) certificate: PathBuf,
    pub(super) key: PathBuf,
}

pub(super) fn parse_serve_arguments<I>(arguments: I) -> Result<ServeArguments, &'static str>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter();
    let mut root = None;
    let mut token_file = None;
    let mut operators_file = None;
    let mut listen = None;
    let mut tls_certificate = None;
    let mut tls_key = None;
    let mut refresh_seconds = None;
    let mut allow_non_loopback = false;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--token-file") if token_file.is_none() => {
                token_file = Some(PathBuf::from(arguments.next().ok_or(SERVE_USAGE)?));
            }
            Some("--operators-file") if operators_file.is_none() => {
                operators_file = Some(PathBuf::from(arguments.next().ok_or(SERVE_USAGE)?));
            }
            Some("--listen") if listen.is_none() => {
                let value = arguments.next().ok_or(SERVE_USAGE)?;
                listen = Some(
                    value
                        .to_str()
                        .and_then(|value| value.parse::<SocketAddr>().ok())
                        .ok_or("--listen needs an IP address and port, such as 127.0.0.1:9464")?,
                );
            }
            Some("--tls-cert") if tls_certificate.is_none() => {
                tls_certificate = Some(PathBuf::from(arguments.next().ok_or(SERVE_USAGE)?));
            }
            Some("--tls-key") if tls_key.is_none() => {
                tls_key = Some(PathBuf::from(arguments.next().ok_or(SERVE_USAGE)?));
            }
            Some("--refresh-seconds") if refresh_seconds.is_none() => {
                let value = arguments.next().ok_or(SERVE_USAGE)?;
                refresh_seconds = Some(
                    value
                        .to_str()
                        .and_then(|value| value.parse::<u64>().ok())
                        .filter(|seconds| (5..=3600).contains(seconds))
                        .ok_or("--refresh-seconds must be between 5 and 3600")?,
                );
            }
            Some("--allow-non-loopback") if !allow_non_loopback => allow_non_loopback = true,
            Some(flag) if flag.starts_with("--") => return Err(SERVE_USAGE),
            _ if root.is_none() => root = Some(PathBuf::from(argument)),
            _ => return Err(SERVE_USAGE),
        }
    }
    let tls = match (tls_certificate, tls_key) {
        (Some(certificate), Some(key)) => Some(TlsFiles { certificate, key }),
        (None, None) => None,
        _ => return Err("--tls-cert and --tls-key must be given together"),
    };
    let listen = listen.unwrap_or_else(|| DEFAULT_LISTEN.parse().expect("valid default"));
    // With native TLS a network listener is safe to bind directly; plain
    // HTTP beyond loopback still needs the explicit acknowledgement.
    if !listen.ip().is_loopback() && tls.is_none() && !allow_non_loopback {
        return Err(
            "without --tls-cert/--tls-key the fleet API speaks plain HTTP, so a non-loopback --listen would expose the bearer token and customer metadata on the network; enable TLS, or put a TLS reverse proxy in front and pass --allow-non-loopback to acknowledge",
        );
    }
    Ok(ServeArguments {
        root: root.ok_or(SERVE_USAGE)?,
        token_file: token_file
            .ok_or("--token-file is required; the fleet API never runs unauthenticated")?,
        operators_file,
        listen,
        tls,
        refresh_seconds: refresh_seconds.unwrap_or(DEFAULT_REFRESH_SECONDS),
        allow_non_loopback,
    })
}

pub(super) fn serve_command(arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let serve = match parse_serve_arguments(arguments) {
        Ok(serve) => serve,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let token = match read_secret_file(&serve.token_file).and_then(validated_token) {
        Ok(token) => token,
        Err(error) => {
            eprintln!("Fleet API token refused: {error}");
            std::process::exit(2);
        }
    };
    let operators = match &serve.operators_file {
        None => Vec::new(),
        Some(path) => match read_secret_file(path)
            .and_then(|contents| controls::parse_operators(contents.as_str(), &token))
        {
            Ok(operators) => operators,
            Err(error) => {
                eprintln!("Fleet API operators file refused: {error}");
                std::process::exit(2);
            }
        },
    };
    let tls = match &serve.tls {
        None => None,
        Some(files) => match tls::server_config(&files.certificate, &files.key) {
            Ok(config) => Some(config),
            Err(error) => {
                eprintln!("Fleet API TLS configuration refused: {error}");
                std::process::exit(2);
            }
        },
    };
    if let Err(error) = run_fleet_api(&serve, token, operators, tls) {
        eprintln!("Fleet API stopped: {error}");
        std::process::exit(1);
    }
    Ok(())
}

fn validated_token(token: crate::credentials::SecretString) -> Result<[u8; 32], String> {
    let value = token.as_str();
    if value.len() < MIN_TOKEN_BYTES {
        return Err(format!(
            "the token file must hold at least {MIN_TOKEN_BYTES} characters; generate one with `openssl rand -hex 32`"
        ));
    }
    if !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("the token must be printable ASCII without spaces".into());
    }
    Ok(token_digest(value.as_bytes()))
}

/// Tokens are compared by SHA-256 digest so neither their bytes nor their
/// length leak through comparison timing.
fn token_digest(token: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(token).into()
}

/// Everything a request handler needs; shared by every connection thread.
struct ApiContext {
    root: PathBuf,
    read_token: [u8; 32],
    operators: Vec<controls::Operator>,
    snapshot: RwLock<Arc<FleetSnapshot>>,
    /// Present when the listener terminates TLS itself.
    tls: Option<Arc<rustls::ServerConfig>>,
    refresh: RefreshHealth,
}

/// Scan outcomes over the process lifetime, for readiness and metrics; the
/// snapshot alone only describes the latest scan.
#[derive(Default)]
struct RefreshHealth {
    interval_seconds: AtomicU64,
    last_success_at: AtomicU64,
    consecutive_failures: AtomicU64,
    failures_total: AtomicU64,
}

impl RefreshHealth {
    fn new(interval: Duration) -> Self {
        let health = Self::default();
        health
            .interval_seconds
            .store(interval.as_secs(), Ordering::Relaxed);
        health
    }

    fn record(&self, snapshot: &FleetSnapshot) {
        if snapshot.rendered.is_ok() {
            self.last_success_at
                .store(snapshot.generated_at, Ordering::Relaxed);
            self.consecutive_failures.store(0, Ordering::Relaxed);
        } else {
            self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
            self.failures_total.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

impl ApiContext {
    fn current_snapshot(&self) -> Arc<FleetSnapshot> {
        Arc::clone(
            &self
                .snapshot
                .read()
                .unwrap_or_else(|poison| poison.into_inner()),
        )
    }
}

fn run_fleet_api(
    serve: &ServeArguments,
    read_token: [u8; 32],
    operators: Vec<controls::Operator>,
    tls: Option<Arc<rustls::ServerConfig>>,
) -> Result<(), String> {
    // Refuse to start on a root that cannot be scanned at all, so a typo in
    // a service unit fails loudly instead of serving errors forever.
    let first = FleetSnapshot::capture(&serve.root);
    if let Err(error) = &first.rendered {
        return Err(format!("could not scan {}: {error}", serve.root.display()));
    }
    let refresh = RefreshHealth::new(Duration::from_secs(serve.refresh_seconds));
    refresh.record(&first);
    let context = Arc::new(ApiContext {
        root: serve.root.clone(),
        read_token,
        operators,
        snapshot: RwLock::new(Arc::new(first)),
        tls,
        refresh,
    });
    let listener = TcpListener::bind(serve.listen)
        .map_err(|error| format!("could not listen on {}: {error}", serve.listen))?;
    eprintln!(
        "Serving fleet status for {} on {}://{} (refresh every {}s); {}",
        serve.root.display(),
        if context.tls.is_some() {
            "https"
        } else {
            "http"
        },
        listener.local_addr().unwrap_or(serve.listen),
        serve.refresh_seconds,
        match context.operators.len() {
            0 => "read-only, no operators configured".to_owned(),
            count => format!("wave approval enabled for {count} named operator(s)"),
        }
    );
    {
        let context = Arc::clone(&context);
        let interval = Duration::from_secs(serve.refresh_seconds);
        std::thread::Builder::new()
            .name("fleet-api-refresh".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(interval);
                    let next = FleetSnapshot::capture(&context.root);
                    if let Err(error) = &next.rendered {
                        eprintln!("Fleet status refresh failed: {error}");
                    }
                    context.refresh.record(&next);
                    *context
                        .snapshot
                        .write()
                        .unwrap_or_else(|poison| poison.into_inner()) = Arc::new(next);
                }
            })
            .map_err(|error| format!("could not start the refresh thread: {error}"))?;
    }
    accept_connections(listener, context)
}

fn accept_connections(listener: TcpListener, context: Arc<ApiContext>) -> Result<(), String> {
    let active = Arc::new(AtomicUsize::new(0));
    let mut last_accept_error_log: Option<std::time::Instant> = None;
    // Over-ceiling connections are answered on one helper thread, so a slow
    // peer's lingering close never stalls the accept loop. A refusal that
    // does not fit the queue is closed without an answer.
    let (refusals, refusal_queue) = std::sync::mpsc::sync_channel::<TcpStream>(MAX_QUEUED_REFUSALS);
    let refusals = std::thread::Builder::new()
        .name("fleet-api-refusals".into())
        .spawn(move || refusal_queue.into_iter().for_each(refuse_busy))
        .is_ok()
        .then_some(refusals);
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                // Usually descriptor exhaustion or a reset during accept.
                // Report it at most once a minute, and back off briefly so
                // a persistent failure cannot spin a CPU.
                if last_accept_error_log
                    .is_none_or(|logged| logged.elapsed() >= Duration::from_secs(60))
                {
                    eprintln!("Fleet API could not accept a connection: {error}");
                    last_accept_error_log = Some(std::time::Instant::now());
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_CONCURRENT_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            // A plaintext 503 is meaningless to a TLS client; just close.
            if context.tls.is_none()
                && let Some(refusals) = &refusals
            {
                let _ = refusals.try_send(stream);
            }
            continue;
        }
        let context = Arc::clone(&context);
        let connection_slots = Arc::clone(&active);
        let spawned = std::thread::Builder::new()
            .name("fleet-api-connection".into())
            .spawn(move || {
                handle_connection(stream, &context);
                connection_slots.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::AcqRel);
        }
    }
    Err("the listening socket closed".into())
}

fn handle_connection(mut stream: TcpStream, context: &ApiContext) {
    // Socket timeouts also bound the TLS handshake, which happens inside
    // the first read.
    let _ = stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(RESPONSE_WRITE_TIMEOUT));
    match &context.tls {
        None => {
            serve_request(&mut stream, context);
        }
        Some(config) => {
            let Ok(connection) = rustls::ServerConnection::new(Arc::clone(config)) else {
                return;
            };
            let mut tls = rustls::StreamOwned::new(connection, stream);
            // A failed handshake (plain HTTP sent to the TLS port, an
            // untrusted client) surfaces as a read error and gets no reply.
            if serve_request(&mut tls, context) {
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
            stream = tls.sock;
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Answer one request on a plain or TLS stream; false when nothing could be
/// exchanged at all (for example a failed TLS handshake).
fn serve_request(stream: &mut (impl Read + Write), context: &ApiContext) -> bool {
    // After a failed handshake the write below fails too, so no plaintext
    // error ever goes to a TLS port and the caller skips close_notify.
    let response = match read_request_head(stream) {
        Ok(head) => respond(&head, context),
        Err(response) => response,
    };
    let head_only = response.head_only;
    write_response(stream, &response, head_only).is_ok()
}

fn read_request_head(stream: &mut impl Read) -> Result<Vec<u8>, Response> {
    let mut head = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    // The socket timeout bounds each read; this bounds the whole head, so a
    // client trickling bytes cannot hold one of the few connection slots.
    let deadline = std::time::Instant::now() + REQUEST_READ_TIMEOUT;
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(Response::text(
                408,
                "Request Timeout",
                "request timed out\n",
            ));
        }
        let read = stream
            .read(&mut buffer)
            .map_err(|_| Response::text(408, "Request Timeout", "request timed out\n"))?;
        if read == 0 {
            return Err(Response::text(400, "Bad Request", "incomplete request\n"));
        }
        head.extend_from_slice(&buffer[..read]);
        if let Some(end) = head.windows(4).position(|window| window == b"\r\n\r\n") {
            // The limit applies to the head itself, including one whose
            // terminator arrived in the read that crossed the limit.
            if end + 4 > MAX_REQUEST_HEAD_BYTES {
                return Err(Response::text(
                    431,
                    "Request Header Fields Too Large",
                    "request head too large\n",
                ));
            }
            // This server handles exactly one request and never reads a body.
            // Do not truncate bytes that happened to arrive in the same read:
            // they could be a body sent despite zero/absent framing or a
            // pipelined second request whose interpretation differs upstream.
            if head.len() != end + 4 {
                return Err(Response::text(
                    400,
                    "Bad Request",
                    "unexpected data after request head\n",
                ));
            }
            break;
        }
        if head.len() > MAX_REQUEST_HEAD_BYTES {
            return Err(Response::text(
                431,
                "Request Header Fields Too Large",
                "request head too large\n",
            ));
        }
    }
    Ok(head)
}

/// One immutable refresh result shared by every request until the next
/// refresh, so polling cost is bounded by the refresh interval instead of
/// the request rate.
struct FleetSnapshot {
    generated_at: u64,
    rendered: Result<RenderedFleet, String>,
}

struct RenderedFleet {
    json: String,
    metrics: Result<String, String>,
}

impl FleetSnapshot {
    fn capture(root: &std::path::Path) -> Self {
        let generated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let rendered = fleet_status(root).and_then(|status| {
            let json = serde_json::to_string_pretty(&status)
                .map_err(|error| format!("could not serialize fleet status: {error}"))?;
            let metrics = render_fleet_metrics(&status, generated_at);
            Ok(RenderedFleet { json, metrics })
        });
        Self {
            generated_at,
            rendered,
        }
    }
}

fn render_fleet_metrics(status: &FleetStatus, generated_at: u64) -> Result<String, String> {
    let summaries = status
        .ledgers
        .iter()
        .map(|ledger| &ledger.summary)
        .collect::<Vec<_>>();
    let mut output = render_metrics_across(&summaries)?;
    let _ = writeln!(
        output,
        "# HELP mailswiftsync_fleet_ledgers Ledger files found under the served directory, by whether they could be summarized.\n# TYPE mailswiftsync_fleet_ledgers gauge\nmailswiftsync_fleet_ledgers{{state=\"readable\"}} {}\nmailswiftsync_fleet_ledgers{{state=\"unreadable\"}} {}",
        status.ledger_count,
        status.unreadable.len()
    );
    let _ = writeln!(
        output,
        "# HELP mailswiftsync_fleet_snapshot_timestamp_seconds Unix time of the ledger scan this response was rendered from.\n# TYPE mailswiftsync_fleet_snapshot_timestamp_seconds gauge\nmailswiftsync_fleet_snapshot_timestamp_seconds {generated_at}"
    );
    Ok(output)
}

#[derive(Debug)]
struct Response {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    extra_headers: Vec<(&'static str, String)>,
    body: String,
    head_only: bool,
}

impl Response {
    fn text(status: u16, reason: &'static str, body: &str) -> Self {
        Self {
            status,
            reason,
            content_type: "text/plain; charset=utf-8",
            extra_headers: Vec::new(),
            body: body.to_owned(),
            head_only: false,
        }
    }

    fn json(status: u16, reason: &'static str, body: String) -> Self {
        Self {
            content_type: "application/json",
            body,
            ..Self::text(status, reason, "")
        }
    }

    fn json_error(status: u16, reason: &'static str, error: &str) -> Self {
        Self {
            content_type: "application/json",
            body: serde_json::json!({ "error": error }).to_string(),
            ..Self::text(status, reason, "")
        }
    }
}

/// Answer a connection over the ceiling with a 503, then linger until the
/// peer closes. Closing a socket whose request has not been read sends a
/// reset (always on Windows), and the reset can destroy the 503 before the
/// peer reads it, so the request is drained after the write side is shut.
/// The linger is bounded in time and bytes.
fn refuse_busy(mut stream: TcpStream) {
    let deadline = std::time::Instant::now() + BUSY_LINGER;
    let _ = stream.set_write_timeout(Some(BUSY_LINGER));
    if write_response(&mut stream, &busy_response(), false).is_err() {
        return;
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut discard = [0_u8; 4096];
    let mut drained = 0;
    while drained < MAX_REQUEST_HEAD_BYTES {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            break;
        }
        match stream.read(&mut discard) {
            Ok(0) | Err(_) => break,
            Ok(read) => drained += read,
        }
    }
}

fn busy_response() -> Response {
    let mut response = Response::text(503, "Service Unavailable", "too many connections\n");
    response.extra_headers.push(("Retry-After", "1".into()));
    response
}

/// Who a request's bearer token identifies.
#[derive(Debug, PartialEq, Eq)]
enum Caller<'a> {
    Anonymous,
    Reader,
    Operator(&'a str),
}

impl Caller<'_> {
    fn may_read(&self) -> bool {
        !matches!(self, Self::Anonymous)
    }
}

/// The routes this API serves and the methods each accepts.
fn allowed_methods(segments: &[&str]) -> Option<&'static str> {
    match segments {
        ["healthz"]
        | ["readyz"]
        | ["metrics"]
        | ["v1", "fleet"]
        | ["v1", "projects", _, "waves"] => Some("GET, HEAD"),
        ["v1", "waves", _, "approve"]
        | ["v1", "projects", _, "hold"]
        | ["v1", "projects", _, "resume"] => Some("POST"),
        _ => None,
    }
}

/// Reject duplicate headers whose interpretation can affect authentication,
/// authority, or message framing. Although this server closes every
/// connection after one request, accepting conflicting values here can make
/// authorization differ from a reverse proxy's interpretation of the same
/// request.
fn has_ambiguous_security_headers(headers: &[(String, String)]) -> bool {
    const SINGLETON_HEADERS: [&str; 4] = [
        "authorization",
        "host",
        "content-length",
        "transfer-encoding",
    ];
    let mut seen = [false; SINGLETON_HEADERS.len()];
    let mut content_length = false;
    let mut transfer_encoding = false;
    for (name, _) in headers {
        if name.eq_ignore_ascii_case("content-length") {
            content_length = true;
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            transfer_encoding = true;
        }
        if let Some(index) = SINGLETON_HEADERS
            .iter()
            .position(|singleton| name.eq_ignore_ascii_case(singleton))
        {
            if seen[index] {
                return true;
            }
            seen[index] = true;
        }
    }

    // Reject request-smuggling framing combinations even though the API
    // intentionally does not consume request bodies.
    content_length && transfer_encoding
}

fn respond<T: AsRef<[u8]> + ?Sized>(head: &T, context: &ApiContext) -> Response {
    let request = match http::parse_request_head(head.as_ref()) {
        Ok(request) => request,
        Err(_) => return Response::text(400, "Bad Request", "malformed request\n"),
    };
    if has_ambiguous_security_headers(&request.headers) {
        return Response::text(400, "Bad Request", "ambiguous security headers\n");
    }
    let headers = request
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let method = request.method.as_str();
    let target = request.target.as_str();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let segments = path
        .strip_prefix('/')
        .unwrap_or(path)
        .split('/')
        .collect::<Vec<_>>();
    let Some(allowed) = allowed_methods(&segments) else {
        return Response::text(404, "Not Found", "not found\n");
    };
    if !allowed.split(", ").any(|candidate| candidate == method) {
        let mut response = Response::text(405, "Method Not Allowed", "method not allowed\n");
        response.extra_headers.push(("Allow", allowed.into()));
        return response;
    }
    let caller = identify(&headers, context);
    let snapshot = context.current_snapshot();
    let mut response = match segments.as_slice() {
        ["healthz"] => Response::text(200, "OK", "ok\n"),
        ["readyz"] => readiness(&snapshot, &context.refresh),
        _ if !caller.may_read() => unauthorized(),
        ["v1", "fleet"] => match &snapshot.rendered {
            Ok(rendered) => Response::json(200, "OK", rendered.json.clone()),
            Err(error) => Response::json_error(503, "Service Unavailable", error),
        },
        ["metrics"] => match snapshot.rendered.as_ref().map(|rendered| &rendered.metrics) {
            Ok(Ok(metrics)) => Response {
                content_type: "text/plain; version=0.0.4; charset=utf-8",
                body: format!("{metrics}{}", refresh_metrics(&context.refresh)),
                ..Response::text(200, "OK", "")
            },
            Ok(Err(error)) | Err(error) => {
                Response::text(503, "Service Unavailable", &format!("{error}\n"))
            }
        },
        ["v1", "projects", project_id, "waves"] if controls::valid_id(project_id) => {
            controls::project_waves(&context.root, project_id)
        }
        ["v1", "waves", wave_id, "approve"] if controls::valid_id(wave_id) => {
            let Caller::Operator(operator) = caller else {
                return Response::json_error(
                    403,
                    "Forbidden",
                    "approving a wave needs a named operator token",
                );
            };
            if let Some(refusal) = refuse_request_body(&headers) {
                return refusal;
            }
            controls::approve_wave(&context.root, wave_id, operator)
        }
        ["v1", "projects", project_id, action @ ("hold" | "resume")]
            if controls::valid_id(project_id) =>
        {
            let Caller::Operator(operator) = caller else {
                return Response::json_error(
                    403,
                    "Forbidden",
                    "holding or resuming a project needs a named operator token",
                );
            };
            if let Some(refusal) = refuse_request_body(&headers) {
                return refusal;
            }
            if *action == "hold" {
                let reason = url::form_urlencoded::parse(query.as_bytes())
                    .find(|(name, _)| name == "reason")
                    .map(|(_, value)| value.into_owned());
                let Some(reason) = reason else {
                    return Response::json_error(
                        400,
                        "Bad Request",
                        "a hold needs a reason query parameter, for example ?reason=customer%20change%20freeze",
                    );
                };
                controls::hold_project(&context.root, project_id, operator, &reason)
            } else {
                controls::resume_project(&context.root, project_id, operator)
            }
        }
        _ => Response::json_error(400, "Bad Request", "malformed identifier"),
    };
    response.extra_headers.push((
        "X-MailSwiftSync-Snapshot-Generated-At",
        snapshot.generated_at.to_string(),
    ));
    response.head_only = method == "HEAD";
    response
}

/// Ready only when the latest scan succeeded and is recent. Unauthenticated
/// like `/healthz`, so the body carries counts and ages but never a path or
/// an error message.
fn readiness(snapshot: &FleetSnapshot, refresh: &RefreshHealth) -> Response {
    let interval = refresh.interval_seconds.load(Ordering::Relaxed).max(1);
    let age = unix_now().saturating_sub(snapshot.generated_at);
    let fresh = age <= interval.saturating_mul(3);
    let ready = snapshot.rendered.is_ok() && fresh;
    let body = serde_json::json!({
        "ready": ready,
        "latest_scan_succeeded": snapshot.rendered.is_ok(),
        "snapshot_age_seconds": age,
        "consecutive_refresh_failures": refresh.consecutive_failures.load(Ordering::Relaxed),
    })
    .to_string();
    if ready {
        Response::json(200, "OK", body)
    } else {
        Response::json(503, "Service Unavailable", body)
    }
}

fn refresh_metrics(refresh: &RefreshHealth) -> String {
    format!(
        "# HELP mailswiftsync_fleet_refresh_failures_total Ledger scans that failed since the fleet API started.\n# TYPE mailswiftsync_fleet_refresh_failures_total counter\nmailswiftsync_fleet_refresh_failures_total {}\n# HELP mailswiftsync_fleet_last_successful_scan_timestamp_seconds Unix time of the latest ledger scan that succeeded.\n# TYPE mailswiftsync_fleet_last_successful_scan_timestamp_seconds gauge\nmailswiftsync_fleet_last_successful_scan_timestamp_seconds {}\n",
        refresh.failures_total.load(Ordering::Relaxed),
        refresh.last_success_at.load(Ordering::Relaxed),
    )
}

fn unauthorized() -> Response {
    let mut response = Response::text(401, "Unauthorized", "bearer token required\n");
    response
        .extra_headers
        .push(("WWW-Authenticate", "Bearer realm=\"mailswiftsync\"".into()));
    response
}

/// Control requests carry everything in the path; a body is never read, so
/// one is refused instead of being silently ignored.
fn refuse_request_body(headers: &[(&str, &str)]) -> Option<Response> {
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("transfer-encoding")
            || (name.eq_ignore_ascii_case("content-length") && *value != "0")
        {
            return Some(Response::json_error(
                413,
                "Content Too Large",
                "control requests take no body",
            ));
        }
    }
    None
}

/// Match the bearer token against every configured credential without an
/// early exit, so timing does not reveal which kind of token was close.
fn identify<'a>(headers: &[(&str, &str)], context: &'a ApiContext) -> Caller<'a> {
    let Some(credential) = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .and_then(|(_, value)| {
            let (scheme, credential) = value.split_once(' ')?;
            scheme
                .eq_ignore_ascii_case("bearer")
                .then(|| credential.trim())
        })
    else {
        return Caller::Anonymous;
    };
    let digest = token_digest(credential.as_bytes());
    let reader = crate::oauth_redirect::constant_time_eq(&digest, &context.read_token);
    let mut operator = None;
    for candidate in &context.operators {
        if crate::oauth_redirect::constant_time_eq(&digest, &candidate.digest) {
            operator = Some(candidate.name.as_str());
        }
    }
    match (operator, reader) {
        (Some(name), _) => Caller::Operator(name),
        (None, true) => Caller::Reader,
        (None, false) => Caller::Anonymous,
    }
}

fn write_response(
    stream: &mut impl Write,
    response: &Response,
    head_only: bool,
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nCross-Origin-Resource-Policy: same-origin\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        response.status,
        response.reason,
        response.content_type,
        response.body.len()
    );
    for (name, value) in &response.extra_headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if !head_only {
        stream.write_all(response.body.as_bytes())?;
    }
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const OPERATOR: &str = "operator-0123456789abcdef0123456789";

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn snapshot() -> FleetSnapshot {
        FleetSnapshot {
            generated_at: 1_700_000_000,
            rendered: Ok(RenderedFleet {
                json: "{\"format\":\"mailswiftsync-fleet-status\"}".into(),
                metrics: Ok("mailswiftsync_mailbox_jobs{state=\"total\"} 0\n".into()),
            }),
        }
    }

    fn context_at(root: PathBuf, snapshot: FleetSnapshot) -> ApiContext {
        ApiContext {
            root,
            read_token: token_digest(TOKEN.as_bytes()),
            operators: controls::parse_operators(
                &format!("{OPERATOR} Alice Example"),
                &token_digest(TOKEN.as_bytes()),
            )
            .unwrap(),
            snapshot: RwLock::new(Arc::new(snapshot)),
            tls: None,
            refresh: RefreshHealth::new(Duration::from_secs(30)),
        }
    }

    fn context(snapshot: FleetSnapshot) -> ApiContext {
        context_at(
            std::env::temp_dir().join("mailswiftsync-serve-unused"),
            snapshot,
        )
    }

    fn request(path: &str, authorization: Option<&str>) -> String {
        request_with("GET", path, authorization)
    }

    fn request_with(method: &str, path: &str, authorization: Option<&str>) -> String {
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: localhost");
        if let Some(authorization) = authorization {
            head.push_str("\r\nAuthorization: ");
            head.push_str(authorization);
        }
        head.push_str("\r\n\r\n");
        head
    }

    fn private_root() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("mailswiftsync-serve-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("shard")).unwrap();
        #[cfg(unix)]
        for directory in [root.clone(), root.join("shard")] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        root
    }

    /// A ledger with one project holding one unapproved wave.
    fn ledger_with_wave(root: &std::path::Path) -> (PathBuf, String, String) {
        let state = root.join("shard").join("state.db");
        let store = crate::core::StateStore::open(&state).unwrap();
        let (project, job_id) = store
            .create_project_with_mailbox("shard", "source.example", "destination.example", "a", "a")
            .unwrap();
        let wave = store
            .create_wave(
                &project.id,
                &crate::core::WaveSettings {
                    name: "Pilot".into(),
                    ..Default::default()
                },
                &[job_id],
            )
            .unwrap();
        (state, project.id, wave.id)
    }

    #[test]
    fn arguments_default_to_loopback_and_require_a_token_file() {
        assert_eq!(
            parse_serve_arguments(args(&["/srv/ledgers", "--token-file", "/etc/token"])),
            Ok(ServeArguments {
                root: "/srv/ledgers".into(),
                token_file: "/etc/token".into(),
                operators_file: None,
                listen: DEFAULT_LISTEN.parse().unwrap(),
                tls: None,
                refresh_seconds: DEFAULT_REFRESH_SECONDS,
                allow_non_loopback: false,
            })
        );
        assert_eq!(
            parse_serve_arguments(args(&[
                "/srv",
                "--token-file",
                "t",
                "--operators-file",
                "/etc/operators"
            ]))
            .unwrap()
            .operators_file,
            Some("/etc/operators".into())
        );
        assert!(parse_serve_arguments(args(&["/srv/ledgers"])).is_err());
        assert!(parse_serve_arguments(args(&["--token-file", "/etc/token"])).is_err());
    }

    #[test]
    fn non_loopback_listening_needs_an_explicit_acknowledgement() {
        let base = [
            "/srv",
            "--token-file",
            "/etc/token",
            "--listen",
            "0.0.0.0:9464",
        ];
        assert!(parse_serve_arguments(args(&base)).is_err());
        let mut acknowledged = base.to_vec();
        acknowledged.push("--allow-non-loopback");
        let parsed = parse_serve_arguments(args(&acknowledged)).unwrap();
        assert_eq!(parsed.listen, "0.0.0.0:9464".parse().unwrap());
        assert!(parsed.allow_non_loopback);
    }

    #[test]
    fn tls_needs_both_files_and_makes_a_network_listener_acceptable() {
        let base = ["/srv", "--token-file", "t"];
        let with = |extra: &[&str]| {
            let mut values = base.to_vec();
            values.extend_from_slice(extra);
            parse_serve_arguments(args(&values))
        };
        assert!(with(&["--tls-cert", "cert.pem"]).is_err());
        assert!(with(&["--tls-key", "key.pem"]).is_err());
        let parsed = with(&[
            "--tls-cert",
            "cert.pem",
            "--tls-key",
            "key.pem",
            "--listen",
            "0.0.0.0:9464",
        ])
        .unwrap();
        assert_eq!(
            parsed.tls,
            Some(TlsFiles {
                certificate: "cert.pem".into(),
                key: "key.pem".into(),
            })
        );
        assert!(!parsed.allow_non_loopback);
        assert!(with(&["--tls-cert", "a", "--tls-cert", "b", "--tls-key", "k"]).is_err());
    }

    /// A throwaway self-signed `localhost` certificate, or `None` where the
    /// `openssl` command is unavailable.
    #[cfg(unix)]
    fn generated_certificate(directory: &std::path::Path) -> Option<(PathBuf, PathBuf)> {
        let certificate = directory.join("cert.pem");
        let key = directory.join("key.pem");
        let status = std::process::Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
            ])
            .args(["-nodes", "-days", "2", "-subj", "/CN=localhost"])
            .args(["-addext", "subjectAltName=DNS:localhost"])
            // A leaf, not a CA: webpki refuses a CA certificate as a server.
            .args(["-addext", "basicConstraints=critical,CA:FALSE"])
            .args(["-addext", "extendedKeyUsage=serverAuth"])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&certificate)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        Some((certificate, key))
    }

    #[cfg(unix)]
    #[test]
    fn a_tls_listener_serves_requests_and_never_answers_plaintext() {
        let root = private_root();
        let Some((certificate, key)) = generated_certificate(&root) else {
            eprintln!("openssl unavailable; skipping the live TLS test");
            return;
        };
        let config = tls::server_config(&certificate, &key).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut context = context(snapshot());
        context.tls = Some(config);
        let shared = Arc::new(context);
        std::thread::spawn(move || accept_connections(listener, shared));

        let mut roots = rustls::RootCertStore::empty();
        let pem = std::fs::read(&certificate).unwrap();
        {
            use rustls::pki_types::pem::PemObject;
            for certificate in rustls::pki_types::CertificateDer::pem_slice_iter(&pem) {
                roots.add(certificate.unwrap()).unwrap();
            }
        }
        let client = Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let connection =
            rustls::ClientConnection::new(client, "localhost".try_into().unwrap()).unwrap();
        let mut tls_stream =
            rustls::StreamOwned::new(connection, TcpStream::connect(address).unwrap());
        tls_stream
            .write_all(
                format!("GET /metrics HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\n\r\n")
                    .as_bytes(),
            )
            .unwrap();
        let mut response = String::new();
        let _ = tls_stream.read_to_string(&mut response);
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
        assert!(response.contains("mailswiftsync_mailbox_jobs"));

        // Plain HTTP to the TLS port must not get a plaintext answer.
        let mut plain = TcpStream::connect(address).unwrap();
        plain
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        plain.write_all(b"GET /healthz HTTP/1.1\r\n\r\n").unwrap();
        let mut bytes = Vec::new();
        let _ = plain.read_to_end(&mut bytes);
        assert!(!String::from_utf8_lossy(&bytes).contains("HTTP/1.1"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn tls_configuration_refuses_unusable_files() {
        let root = private_root();
        let Some((certificate, key)) = generated_certificate(&root) else {
            return;
        };
        let missing = root.join("missing.pem");
        assert!(tls::server_config(&missing, &key).is_err());
        assert!(tls::server_config(&certificate, &missing).is_err());
        // The certificate is not a key, and an empty file holds no chain.
        let empty = root.join("empty.pem");
        std::fs::write(&empty, b"").unwrap();
        assert!(tls::server_config(&empty, &key).is_err());
        // A key readable by others is refused like any secret file.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            tls::server_config(&certificate, &key)
                .unwrap_err()
                .contains("owner-only")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn arguments_reject_malformed_values_duplicates_and_extras() {
        for values in [
            vec!["/srv", "--token-file", "t", "--refresh-seconds", "4"],
            vec!["/srv", "--token-file", "t", "--refresh-seconds", "3601"],
            vec!["/srv", "--token-file", "t", "--listen", "localhost"],
            vec!["/srv", "--token-file", "t", "--token-file", "u"],
            vec![
                "/srv",
                "--token-file",
                "t",
                "--operators-file",
                "o",
                "--operators-file",
                "p",
            ],
            vec!["/srv", "--token-file", "t", "--operators-file"],
            vec!["/srv", "--token-file", "t", "--unknown"],
            vec!["/srv", "/other", "--token-file", "t"],
            vec!["/srv", "--token-file"],
        ] {
            assert!(parse_serve_arguments(args(&values)).is_err(), "{values:?}");
        }
    }

    #[test]
    fn short_or_non_printable_tokens_are_refused() {
        let check = |value: &str| validated_token(value.to_owned().into());
        assert!(check("short").is_err());
        assert!(check(&format!("{TOKEN} with spaces")).is_err());
        assert_eq!(check(TOKEN).unwrap(), token_digest(TOKEN.as_bytes()));
    }

    #[test]
    fn data_endpoints_require_the_bearer_token_but_health_does_not() {
        let context = context(snapshot());
        assert_eq!(respond(&request("/healthz", None), &context).status, 200);
        for path in ["/v1/fleet", "/metrics"] {
            for authorization in [None, Some("Bearer wrong-token"), Some("Basic abc")] {
                let response = respond(&request(path, authorization), &context);
                assert_eq!(response.status, 401, "{path} {authorization:?}");
                assert!(!response.body.contains("mailswiftsync"));
            }
            for token in [TOKEN, OPERATOR] {
                let response = respond(&request(path, Some(&format!("bearer {token}"))), &context);
                assert_eq!(response.status, 200, "{path}");
            }
        }
        let fleet = respond(
            &request("/v1/fleet?pretty", Some(&format!("Bearer {TOKEN}"))),
            &context,
        );
        assert_eq!(fleet.content_type, "application/json");
        assert!(fleet.body.contains("mailswiftsync-fleet-status"));
    }

    #[test]
    fn tokens_identify_readers_and_named_operators() {
        let context = context(snapshot());
        let identify_with = |value: &str| identify(&[("Authorization", value)], &context);
        assert_eq!(identify_with(&format!("Bearer {TOKEN}")), Caller::Reader);
        assert_eq!(
            identify_with(&format!("Bearer {OPERATOR}")),
            Caller::Operator("Alice Example")
        );
        assert_eq!(identify_with("Bearer nope"), Caller::Anonymous);
        assert_eq!(identify(&[], &context), Caller::Anonymous);
    }

    #[test]
    fn routes_enforce_methods_and_reject_malformed_requests() {
        let context = context(snapshot());
        let post = respond("POST /v1/fleet HTTP/1.1\r\n\r\n", &context);
        assert_eq!(post.status, 405);
        assert!(post.extra_headers.contains(&("Allow", "GET, HEAD".into())));
        let get_approve = respond("GET /v1/waves/abc/approve HTTP/1.1\r\n\r\n", &context);
        assert_eq!(get_approve.status, 405);
        assert!(
            get_approve
                .extra_headers
                .contains(&("Allow", "POST".into()))
        );
        assert_eq!(respond("GET /v1/fleet\r\n\r\n", &context).status, 400);
        assert_eq!(respond("GET / HTTP/2\r\n\r\n", &context).status, 400);
        assert_eq!(respond("GET /other HTTP/1.1\r\n\r\n", &context).status, 404);
        assert_eq!(
            respond("GET /v1/fleet/extra HTTP/1.1\r\n\r\n", &context).status,
            404
        );
        let head = respond("HEAD /healthz HTTP/1.1\r\n\r\n", &context);
        assert!(head.head_only);
        let malformed = respond(
            &request(
                "/v1/projects/a%2F..%2Fb/waves",
                Some(&format!("Bearer {TOKEN}")),
            ),
            &context,
        );
        assert_eq!(malformed.status, 400);
    }

    #[test]
    fn rejects_duplicate_auth_authority_and_framing_headers() {
        let context = context(snapshot());
        let duplicate_authorization = format!(
            "GET /v1/fleet HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\nAuthorization: Bearer {OPERATOR}\r\n\r\n"
        );
        let duplicate_host = "GET /healthz HTTP/1.1\r\nHost: localhost\r\nHost: other\r\n\r\n";
        let duplicate_content_length = "POST /v1/projects/project/hold HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nContent-Length: 1\r\n\r\n";
        let conflicting_framing = "POST /v1/projects/project/hold HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\n\r\n";

        for request in [
            duplicate_authorization.as_str(),
            duplicate_host,
            duplicate_content_length,
            conflicting_framing,
        ] {
            assert_eq!(
                respond(request, &context).status,
                400,
                "request with ambiguous security headers was accepted: {request:?}"
            );
        }
    }

    #[test]
    fn approval_needs_an_operator_token_and_no_body() {
        let context = context(snapshot());
        let path = "/v1/waves/8f1c2e9a/approve";
        assert_eq!(
            respond(&request_with("POST", path, None), &context).status,
            401
        );
        let reader = respond(
            &request_with("POST", path, Some(&format!("Bearer {TOKEN}"))),
            &context,
        );
        assert_eq!(reader.status, 403);
        let with_body = format!(
            "{}\r\nContent-Length: 12\r\n\r\n",
            request_with("POST", path, Some(&format!("Bearer {OPERATOR}")))
                .trim_end_matches("\r\n\r\n")
        );
        assert_eq!(respond(&with_body, &context).status, 413);
        let chunked = format!(
            "{}\r\nTransfer-Encoding: chunked\r\n\r\n",
            request_with("POST", path, Some(&format!("Bearer {OPERATOR}")))
                .trim_end_matches("\r\n\r\n")
        );
        assert_eq!(respond(&chunked, &context).status, 413);
    }

    #[test]
    fn an_operator_approves_a_wave_once_under_their_own_name() {
        let root = private_root();
        let (state, project_id, wave_id) = ledger_with_wave(&root);
        let context = context_at(root.clone(), snapshot());
        let operator = Some(format!("Bearer {OPERATOR}"));
        let approve = format!("/v1/waves/{wave_id}/approve");

        let waves = respond(
            &request(
                &format!("/v1/projects/{project_id}/waves"),
                Some(&format!("Bearer {TOKEN}")),
            ),
            &context,
        );
        assert_eq!(waves.status, 200, "{}", waves.body);
        assert!(
            waves.body.contains("\"status\": \"draft\""),
            "{}",
            waves.body
        );

        let approved = respond(
            &request_with("POST", &approve, operator.as_deref()),
            &context,
        );
        assert_eq!(approved.status, 200, "{}", approved.body);
        assert!(
            approved
                .body
                .contains("\"approved_by\":\"Alice Example (fleet API)\"")
        );
        let store = crate::core::StateStore::open_readonly(&state).unwrap();
        assert_eq!(
            store
                .wave(&wave_id)
                .unwrap()
                .unwrap()
                .approved_by
                .as_deref(),
            Some("Alice Example (fleet API)")
        );
        drop(store);

        let again = respond(
            &request_with("POST", &approve, operator.as_deref()),
            &context,
        );
        assert_eq!(again.status, 409);
        assert!(again.body.contains("already approved by Alice Example"));
        let unknown = respond(
            &request_with(
                "POST",
                "/v1/waves/missing-wave/approve",
                operator.as_deref(),
            ),
            &context,
        );
        assert_eq!(unknown.status, 404);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_operator_holds_and_resumes_a_project_even_while_the_ledger_is_in_use() {
        let root = private_root();
        let (state, project_id, _) = ledger_with_wave(&root);
        let context = context_at(root.clone(), snapshot());
        let operator = Some(format!("Bearer {OPERATOR}"));
        let hold = format!("/v1/projects/{project_id}/hold?reason=customer%20change%20freeze");
        let resume = format!("/v1/projects/{project_id}/resume");

        let reader = respond(
            &request_with("POST", &hold, Some(&format!("Bearer {TOKEN}"))),
            &context,
        );
        assert_eq!(reader.status, 403);
        let no_reason = respond(
            &request_with(
                "POST",
                &format!("/v1/projects/{project_id}/hold"),
                operator.as_deref(),
            ),
            &context,
        );
        assert_eq!(no_reason.status, 400);

        // A supervised pass or the GUI owns the ledger; the hold still lands.
        let lock = crate::process::acquire_instance_lock(&state).unwrap();
        let held = respond(&request_with("POST", &hold, operator.as_deref()), &context);
        assert_eq!(held.status, 200, "{}", held.body);
        assert!(
            held.body
                .contains("\"held_by\":\"Alice Example (fleet API)\"")
        );
        assert!(held.body.contains("customer change freeze"));
        let store = crate::core::StateStore::open_readonly(&state).unwrap();
        assert_eq!(
            store
                .project_hold(&project_id)
                .unwrap()
                .map(|hold| hold.reason),
            Some("customer change freeze".to_owned())
        );
        drop(store);
        let again = respond(&request_with("POST", &hold, operator.as_deref()), &context);
        assert_eq!(again.status, 409);

        let resumed = respond(
            &request_with("POST", &resume, operator.as_deref()),
            &context,
        );
        assert_eq!(resumed.status, 200, "{}", resumed.body);
        assert!(resumed.body.contains("\"state\":\"resumed\""));
        assert_eq!(
            respond(
                &request_with("POST", &resume, operator.as_deref()),
                &context
            )
            .status,
            409
        );
        drop(lock);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn approval_waits_for_the_ledger_lock_and_refuses_ambiguous_copies() {
        let root = private_root();
        let (state, _, wave_id) = ledger_with_wave(&root);
        let context = context_at(root.clone(), snapshot());
        let operator = Some(format!("Bearer {OPERATOR}"));
        let approve = format!("/v1/waves/{wave_id}/approve");

        let lock = crate::process::acquire_instance_lock(&state).unwrap();
        let busy = respond(
            &request_with("POST", &approve, operator.as_deref()),
            &context,
        );
        assert_eq!(busy.status, 409, "{}", busy.body);
        assert!(busy.extra_headers.contains(&("Retry-After", "60".into())));
        drop(lock);

        let store = crate::core::StateStore::open_readonly(&state).unwrap();
        store.backup_to(&root.join("copy.db")).unwrap();
        assert!(store.wave(&wave_id).unwrap().unwrap().approved_by.is_none());
        drop(store);
        let ambiguous = respond(
            &request_with("POST", &approve, operator.as_deref()),
            &context,
        );
        assert_eq!(ambiguous.status, 409);
        assert!(ambiguous.body.contains("more than one ledger"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_failed_refresh_is_reported_rather_than_serving_stale_data() {
        let failed = context(FleetSnapshot {
            generated_at: 1,
            rendered: Err("could not read /srv: permission denied".into()),
        });
        let authorization = format!("Bearer {TOKEN}");
        for path in ["/v1/fleet", "/metrics"] {
            let response = respond(&request(path, Some(&authorization)), &failed);
            assert_eq!(response.status, 503, "{path}");
            assert!(response.body.contains("permission denied"));
        }
        let truncated = context(FleetSnapshot {
            generated_at: 1,
            rendered: Ok(RenderedFleet {
                json: "{}".into(),
                metrics: Err("truncated".into()),
            }),
        });
        let metrics = respond(&request("/metrics", Some(&authorization)), &truncated);
        assert_eq!(metrics.status, 503);
        let fleet = respond(&request("/v1/fleet", Some(&authorization)), &truncated);
        assert_eq!(fleet.status, 200);
    }

    #[test]
    fn readiness_requires_a_recent_successful_scan_and_leaks_no_detail() {
        let fresh = FleetSnapshot {
            generated_at: unix_now(),
            ..snapshot()
        };
        let context = context(fresh);
        context.refresh.record(&context.current_snapshot());
        let ready = respond("GET /readyz HTTP/1.1\r\n\r\n", &context);
        assert_eq!(ready.status, 200, "{}", ready.body);
        assert!(ready.body.contains("\"ready\":true"));

        let failed = FleetSnapshot {
            generated_at: unix_now(),
            rendered: Err("could not read /srv/customer-a: permission denied".into()),
        };
        context.refresh.record(&failed);
        *context.snapshot.write().unwrap() = Arc::new(failed);
        let not_ready = respond("GET /readyz HTTP/1.1\r\n\r\n", &context);
        assert_eq!(not_ready.status, 503);
        assert!(
            not_ready
                .body
                .contains("\"consecutive_refresh_failures\":1")
        );
        assert!(!not_ready.body.contains("customer-a"));
        // Liveness stays independent of scan health.
        assert_eq!(
            respond("GET /healthz HTTP/1.1\r\n\r\n", &context).status,
            200
        );

        // A snapshot older than three refresh intervals is stale even if it
        // succeeded, for example when the refresh thread is stuck.
        *context.snapshot.write().unwrap() = Arc::new(FleetSnapshot {
            generated_at: unix_now() - 120,
            ..snapshot()
        });
        assert_eq!(
            respond("GET /readyz HTTP/1.1\r\n\r\n", &context).status,
            503
        );
    }

    #[test]
    fn metrics_report_refresh_failures_and_the_last_successful_scan() {
        let context = context(snapshot());
        context.refresh.record(&snapshot());
        context.refresh.record(&FleetSnapshot {
            generated_at: 1,
            rendered: Err("unreadable".into()),
        });
        let metrics = respond(
            &request("/metrics", Some(&format!("Bearer {TOKEN}"))),
            &context,
        );
        assert_eq!(metrics.status, 200);
        assert!(
            metrics
                .body
                .contains("mailswiftsync_fleet_refresh_failures_total 1")
        );
        assert!(
            metrics
                .body
                .contains("mailswiftsync_fleet_last_successful_scan_timestamp_seconds 1700000000")
        );
    }

    #[test]
    fn a_refused_client_reads_its_503_even_when_its_request_arrives_late() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            // The request lands after the 503 was written: closing the
            // socket with it unread would reset the connection.
            std::thread::sleep(Duration::from_millis(200));
            stream
                .write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            std::thread::sleep(Duration::from_millis(100));
            let mut response = String::new();
            stream.read_to_string(&mut response).map(|_| response)
        });
        let (stream, _) = listener.accept().unwrap();
        refuse_busy(stream);
        let response = client.join().unwrap().expect("the 503 was reset away");
        assert!(response.starts_with("HTTP/1.1 503"), "{response}");
        assert!(response.contains("Retry-After: 1"), "{response}");
    }

    #[test]
    fn concurrent_clients_beyond_the_ceiling_get_answers_and_the_listener_survives() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let shared = Arc::new(context(snapshot()));
        std::thread::spawn(move || accept_connections(listener, shared));
        let exchange = move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .write_all(
                    format!("GET /metrics HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\n\r\n")
                        .as_bytes(),
                )
                .unwrap();
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            response
        };
        let clients = (0..48)
            .map(|_| std::thread::spawn(exchange))
            .collect::<Vec<_>>();
        let mut served = 0;
        for client in clients {
            let response = client.join().unwrap();
            if response.starts_with("HTTP/1.1 200 OK") {
                served += 1;
            } else {
                assert!(
                    response.starts_with("HTTP/1.1 503") && response.contains("Retry-After: 1"),
                    "{response}"
                );
            }
        }
        assert!(served >= 16, "only {served} requests served");
        // Every slot was released: a later request is served normally.
        assert!(exchange().starts_with("HTTP/1.1 200 OK"));
    }

    #[test]
    fn oversized_request_heads_are_refused() {
        let mut oversized = std::io::Cursor::new(vec![b'a'; MAX_REQUEST_HEAD_BYTES + 2048]);
        assert_eq!(read_request_head(&mut oversized).unwrap_err().status, 431);
        let mut truncated = std::io::Cursor::new(b"GET / HTTP/1.1\r\n".to_vec());
        assert_eq!(read_request_head(&mut truncated).unwrap_err().status, 400);
        // A head that crosses the limit inside the final read is refused
        // even though its terminator arrives in that same read.
        let mut crossing = format!(
            "GET / HTTP/1.1\r\nX-Pad: {}",
            "a".repeat(MAX_REQUEST_HEAD_BYTES - 100)
        )
        .into_bytes();
        crossing.extend_from_slice(&[b'b'; 200]);
        crossing.extend_from_slice(b"\r\n\r\n");
        assert!(crossing.len() > MAX_REQUEST_HEAD_BYTES);
        assert!(crossing.len() < MAX_REQUEST_HEAD_BYTES + 1024);
        assert_eq!(
            read_request_head(&mut std::io::Cursor::new(crossing))
                .unwrap_err()
                .status,
            431
        );
        let mut exact = format!(
            "GET / HTTP/1.1\r\nX-Pad: {}",
            "a".repeat(MAX_REQUEST_HEAD_BYTES - 27)
        )
        .into_bytes();
        exact.extend_from_slice(b"\r\n\r\n");
        assert_eq!(exact.len(), MAX_REQUEST_HEAD_BYTES);
        assert!(read_request_head(&mut std::io::Cursor::new(exact)).is_ok());
        let mut complete = std::io::Cursor::new(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n".to_vec());
        assert_eq!(
            read_request_head(&mut complete).unwrap(),
            b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"
        );
    }

    #[test]
    fn refuses_bytes_buffered_after_the_request_head() {
        for request in [
            &b"POST /v1/projects/p/hold?reason=x HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\nbody"[..],
            &b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\nGET /metrics HTTP/1.1\r\nHost: x\r\n\r\n"[..],
        ] {
            let error = read_request_head(&mut std::io::Cursor::new(request.to_vec()))
                .expect_err("buffered trailing bytes must not be silently discarded");
            assert_eq!(error.status, 400);
        }
    }

    #[test]
    fn fleet_metrics_add_ledger_counts_and_snapshot_time() {
        let root = private_root();
        let store = crate::core::StateStore::open(root.join("shard").join("state.db")).unwrap();
        store
            .create_project_with_mailbox("shard", "source.example", "destination.example", "a", "a")
            .unwrap();
        drop(store);
        std::fs::write(root.join("junk.db"), b"not a database").unwrap();

        let snapshot = FleetSnapshot::capture(&root);
        let rendered = snapshot.rendered.as_ref().unwrap();
        let metrics = rendered.metrics.as_ref().unwrap();
        assert!(metrics.contains("mailswiftsync_mailbox_jobs{state=\"total\"} 1"));
        assert!(metrics.contains("mailswiftsync_fleet_ledgers{state=\"readable\"} 1"));
        assert!(metrics.contains("mailswiftsync_fleet_ledgers{state=\"unreadable\"} 1"));
        assert!(metrics.contains(&format!(
            "mailswiftsync_fleet_snapshot_timestamp_seconds {}",
            snapshot.generated_at
        )));
        assert!(!metrics.contains("source.example"));
        assert!(rendered.json.contains("\"ledger_count\": 1"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_live_listener_serves_health_and_authorized_status() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let shared = Arc::new(context(snapshot()));
        std::thread::spawn(move || accept_connections(listener, shared));

        let exchange = |request: &str| {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        let health = exchange("GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(
            health.starts_with("HTTP/1.1 200 OK\r\n"),
            "health endpoint must return HTTP 200"
        );
        assert!(health.contains("Cache-Control: no-store"));
        assert!(health.contains("Cross-Origin-Resource-Policy: same-origin"));
        assert!(health.ends_with("ok\n"));
        let unauthorized = exchange("GET /metrics HTTP/1.1\r\n\r\n");
        assert!(
            unauthorized.starts_with("HTTP/1.1 401"),
            "unauthorized endpoint must return HTTP 401"
        );
        let metrics = exchange(&format!(
            "GET /metrics HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"
        ));
        assert!(
            metrics.starts_with("HTTP/1.1 200 OK\r\n"),
            "metrics endpoint must return HTTP 200"
        );
        assert!(metrics.contains("mailswiftsync_mailbox_jobs"));
        let head = exchange("HEAD /healthz HTTP/1.1\r\n\r\n");
        assert!(head.contains("Content-Length: 3\r\n"));
        assert!(head.ends_with("\r\n\r\n"), "HEAD response must omit a body");
    }
}
