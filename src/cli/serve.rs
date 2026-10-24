//! Read-only fleet API: a long-running HTTP listener that serves cached
//! `fleet-status` JSON and fleet-wide Prometheus metrics to monitoring and
//! PSA tooling. It never takes an instance lock and exposes no control
//! actions; `supervise` remains the per-ledger worker.

use super::*;
use crate::headless::FleetStatus;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

const SERVE_USAGE: &str = "Usage: mailswiftsync serve <directory> --token-file <file> [--listen 127.0.0.1:9464] [--refresh-seconds 5..3600] [--allow-non-loopback]";
const DEFAULT_LISTEN: &str = "127.0.0.1:9464";
const DEFAULT_REFRESH_SECONDS: u64 = 30;
const MIN_TOKEN_BYTES: usize = 32;
const MAX_REQUEST_HEAD_BYTES: usize = 8 * 1024;
const MAX_CONCURRENT_CONNECTIONS: usize = 16;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ServeArguments {
    pub(super) root: PathBuf,
    pub(super) token_file: PathBuf,
    pub(super) listen: SocketAddr,
    pub(super) refresh_seconds: u64,
    pub(super) allow_non_loopback: bool,
}

pub(super) fn parse_serve_arguments<I>(arguments: I) -> Result<ServeArguments, &'static str>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter();
    let mut root = None;
    let mut token_file = None;
    let mut listen = None;
    let mut refresh_seconds = None;
    let mut allow_non_loopback = false;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--token-file") if token_file.is_none() => {
                token_file = Some(PathBuf::from(arguments.next().ok_or(SERVE_USAGE)?));
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
    let listen = listen.unwrap_or_else(|| DEFAULT_LISTEN.parse().expect("valid default"));
    if !listen.ip().is_loopback() && !allow_non_loopback {
        return Err(
            "the fleet API speaks plain HTTP, so a non-loopback --listen would expose the bearer token and customer metadata on the network; put a TLS reverse proxy in front and pass --allow-non-loopback to acknowledge",
        );
    }
    Ok(ServeArguments {
        root: root.ok_or(SERVE_USAGE)?,
        token_file: token_file
            .ok_or("--token-file is required; the fleet API never runs unauthenticated")?,
        listen,
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
    if let Err(error) = run_fleet_api(&serve, &token) {
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

fn run_fleet_api(serve: &ServeArguments, token: &[u8; 32]) -> Result<(), String> {
    // Refuse to start on a root that cannot be scanned at all, so a typo in
    // a service unit fails loudly instead of serving errors forever.
    let first = FleetSnapshot::capture(&serve.root);
    if let Err(error) = &first.rendered {
        return Err(format!("could not scan {}: {error}", serve.root.display()));
    }
    let snapshot = Arc::new(RwLock::new(Arc::new(first)));
    let listener = TcpListener::bind(serve.listen)
        .map_err(|error| format!("could not listen on {}: {error}", serve.listen))?;
    eprintln!(
        "Serving read-only fleet status for {} on http://{} (refresh every {}s)",
        serve.root.display(),
        listener.local_addr().unwrap_or(serve.listen),
        serve.refresh_seconds
    );
    {
        let snapshot = Arc::clone(&snapshot);
        let root = serve.root.clone();
        let interval = Duration::from_secs(serve.refresh_seconds);
        std::thread::Builder::new()
            .name("fleet-api-refresh".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(interval);
                    let next = FleetSnapshot::capture(&root);
                    if let Err(error) = &next.rendered {
                        eprintln!("Fleet status refresh failed: {error}");
                    }
                    *snapshot
                        .write()
                        .unwrap_or_else(|poison| poison.into_inner()) = Arc::new(next);
                }
            })
            .map_err(|error| format!("could not start the refresh thread: {error}"))?;
    }
    accept_connections(listener, snapshot, *token)
}

fn accept_connections(
    listener: TcpListener,
    snapshot: Arc<RwLock<Arc<FleetSnapshot>>>,
    token: [u8; 32],
) -> Result<(), String> {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_CONCURRENT_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            let _ = stream.set_write_timeout(Some(REQUEST_READ_TIMEOUT));
            let _ = write_response(&mut stream, &busy_response(), false);
            continue;
        }
        let current = Arc::clone(&snapshot.read().unwrap_or_else(|poison| poison.into_inner()));
        let connection_slots = Arc::clone(&active);
        let spawned = std::thread::Builder::new()
            .name("fleet-api-connection".into())
            .spawn(move || {
                handle_connection(stream, &current, &token);
                connection_slots.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::AcqRel);
        }
    }
    Err("the listening socket closed".into())
}

fn handle_connection(mut stream: TcpStream, snapshot: &FleetSnapshot, token: &[u8; 32]) {
    let _ = stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(RESPONSE_WRITE_TIMEOUT));
    let response = match read_request_head(&mut stream) {
        Ok(head) => respond(&head, snapshot, token),
        Err(response) => response,
    };
    let head_only = response.head_only;
    let _ = write_response(&mut stream, &response, head_only);
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn read_request_head(stream: &mut impl Read) -> Result<String, Response> {
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
            head.truncate(end);
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
    String::from_utf8(head).map_err(|_| Response::text(400, "Bad Request", "malformed request\n"))
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

    fn json_error(status: u16, reason: &'static str, error: &str) -> Self {
        Self {
            content_type: "application/json",
            body: serde_json::json!({ "error": error }).to_string(),
            ..Self::text(status, reason, "")
        }
    }
}

fn busy_response() -> Response {
    let mut response = Response::text(503, "Service Unavailable", "too many connections\n");
    response.extra_headers.push(("Retry-After", "1".into()));
    response
}

fn respond(head: &str, snapshot: &FleetSnapshot, token: &[u8; 32]) -> Response {
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version), None) = (
        request_line.next(),
        request_line.next(),
        request_line.next(),
        request_line.next(),
    ) else {
        return Response::text(400, "Bad Request", "malformed request line\n");
    };
    if !version.starts_with("HTTP/1.") {
        return Response::text(505, "HTTP Version Not Supported", "HTTP/1.x only\n");
    }
    let head_only = method == "HEAD";
    if method != "GET" && !head_only {
        let mut response =
            Response::text(405, "Method Not Allowed", "the fleet API is read-only\n");
        response.extra_headers.push(("Allow", "GET, HEAD".into()));
        return response;
    }
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    let authorized = authorized(lines, token);
    let mut response = match path {
        "/healthz" => Response::text(200, "OK", "ok\n"),
        "/v1/fleet" | "/metrics" if !authorized => {
            let mut response = Response::text(401, "Unauthorized", "bearer token required\n");
            response
                .extra_headers
                .push(("WWW-Authenticate", "Bearer realm=\"mailswiftsync\"".into()));
            response
        }
        "/v1/fleet" => match &snapshot.rendered {
            Ok(rendered) => Response {
                content_type: "application/json",
                body: rendered.json.clone(),
                ..Response::text(200, "OK", "")
            },
            Err(error) => Response::json_error(503, "Service Unavailable", error),
        },
        "/metrics" => match snapshot.rendered.as_ref().map(|rendered| &rendered.metrics) {
            Ok(Ok(metrics)) => Response {
                content_type: "text/plain; version=0.0.4; charset=utf-8",
                body: metrics.clone(),
                ..Response::text(200, "OK", "")
            },
            Ok(Err(error)) | Err(error) => {
                Response::text(503, "Service Unavailable", &format!("{error}\n"))
            }
        },
        _ => Response::text(404, "Not Found", "not found\n"),
    };
    response.extra_headers.push((
        "X-MailSwiftSync-Snapshot-Generated-At",
        snapshot.generated_at.to_string(),
    ));
    response.head_only = head_only;
    response
}

fn authorized<'a>(headers: impl Iterator<Item = &'a str>, token: &[u8; 32]) -> bool {
    headers
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("authorization"))
        .filter_map(|(_, value)| {
            let (scheme, credential) = value.trim().split_once(' ')?;
            scheme
                .eq_ignore_ascii_case("bearer")
                .then(|| credential.trim())
        })
        .any(|credential| {
            crate::oauth_redirect::constant_time_eq(&token_digest(credential.as_bytes()), token)
        })
}

fn write_response(
    stream: &mut impl Write,
    response: &Response,
    head_only: bool,
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
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

    fn request(path: &str, authorization: Option<&str>) -> String {
        let mut head = format!("GET {path} HTTP/1.1\r\nHost: localhost");
        if let Some(authorization) = authorization {
            head.push_str("\r\nAuthorization: ");
            head.push_str(authorization);
        }
        head
    }

    #[test]
    fn arguments_default_to_loopback_and_require_a_token_file() {
        assert_eq!(
            parse_serve_arguments(args(&["/srv/ledgers", "--token-file", "/etc/token"])),
            Ok(ServeArguments {
                root: "/srv/ledgers".into(),
                token_file: "/etc/token".into(),
                listen: DEFAULT_LISTEN.parse().unwrap(),
                refresh_seconds: DEFAULT_REFRESH_SECONDS,
                allow_non_loopback: false,
            })
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
    fn arguments_reject_malformed_values_duplicates_and_extras() {
        for values in [
            vec!["/srv", "--token-file", "t", "--refresh-seconds", "4"],
            vec!["/srv", "--token-file", "t", "--refresh-seconds", "3601"],
            vec!["/srv", "--token-file", "t", "--listen", "localhost"],
            vec!["/srv", "--token-file", "t", "--token-file", "u"],
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
        let token = token_digest(TOKEN.as_bytes());
        let snapshot = snapshot();
        assert_eq!(
            respond(&request("/healthz", None), &snapshot, &token).status,
            200
        );
        for path in ["/v1/fleet", "/metrics"] {
            for authorization in [None, Some("Bearer wrong-token"), Some("Basic abc")] {
                let response = respond(&request(path, authorization), &snapshot, &token);
                assert_eq!(response.status, 401, "{path} {authorization:?}");
                assert!(!response.body.contains("mailswiftsync"));
            }
            let response = respond(
                &request(path, Some(&format!("bearer {TOKEN}"))),
                &snapshot,
                &token,
            );
            assert_eq!(response.status, 200, "{path}");
        }
        let fleet = respond(
            &request("/v1/fleet?pretty", Some(&format!("Bearer {TOKEN}"))),
            &snapshot,
            &token,
        );
        assert_eq!(fleet.content_type, "application/json");
        assert!(fleet.body.contains("mailswiftsync-fleet-status"));
    }

    #[test]
    fn the_api_is_read_only_and_rejects_malformed_requests() {
        let token = token_digest(TOKEN.as_bytes());
        let snapshot = snapshot();
        let post = respond("POST /v1/fleet HTTP/1.1", &snapshot, &token);
        assert_eq!(post.status, 405);
        assert!(post.extra_headers.contains(&("Allow", "GET, HEAD".into())));
        assert_eq!(respond("GET /v1/fleet", &snapshot, &token).status, 400);
        assert_eq!(respond("GET / HTTP/2", &snapshot, &token).status, 505);
        assert_eq!(
            respond("GET /other HTTP/1.1", &snapshot, &token).status,
            404
        );
        let head = respond("HEAD /healthz HTTP/1.1", &snapshot, &token);
        assert!(head.head_only);
    }

    #[test]
    fn a_failed_refresh_is_reported_rather_than_serving_stale_data() {
        let token = token_digest(TOKEN.as_bytes());
        let failed = FleetSnapshot {
            generated_at: 1,
            rendered: Err("could not read /srv: permission denied".into()),
        };
        let authorization = format!("Bearer {TOKEN}");
        for path in ["/v1/fleet", "/metrics"] {
            let response = respond(&request(path, Some(&authorization)), &failed, &token);
            assert_eq!(response.status, 503, "{path}");
            assert!(response.body.contains("permission denied"));
        }
        let truncated = FleetSnapshot {
            generated_at: 1,
            rendered: Ok(RenderedFleet {
                json: "{}".into(),
                metrics: Err("truncated".into()),
            }),
        };
        let metrics = respond(
            &request("/metrics", Some(&authorization)),
            &truncated,
            &token,
        );
        assert_eq!(metrics.status, 503);
        let fleet = respond(
            &request("/v1/fleet", Some(&authorization)),
            &truncated,
            &token,
        );
        assert_eq!(fleet.status, 200);
    }

    #[test]
    fn oversized_request_heads_are_refused() {
        let mut oversized = std::io::Cursor::new(vec![b'a'; MAX_REQUEST_HEAD_BYTES + 2048]);
        assert_eq!(read_request_head(&mut oversized).unwrap_err().status, 431);
        let mut truncated = std::io::Cursor::new(b"GET / HTTP/1.1\r\n".to_vec());
        assert_eq!(read_request_head(&mut truncated).unwrap_err().status, 400);
        let mut complete = std::io::Cursor::new(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n".to_vec());
        assert_eq!(
            read_request_head(&mut complete).unwrap(),
            "GET / HTTP/1.1\r\nHost: x"
        );
    }

    #[test]
    fn fleet_metrics_add_ledger_counts_and_snapshot_time() {
        let root =
            std::env::temp_dir().join(format!("mailswiftsync-serve-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("shard")).unwrap();
        #[cfg(unix)]
        for directory in [root.clone(), root.join("shard")] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
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
        let shared = Arc::new(RwLock::new(Arc::new(snapshot())));
        let token = token_digest(TOKEN.as_bytes());
        std::thread::spawn(move || accept_connections(listener, shared, token));

        let exchange = |request: &str| {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        let health = exchange("GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(health.starts_with("HTTP/1.1 200 OK\r\n"), "{health}");
        assert!(health.contains("Cache-Control: no-store"));
        assert!(health.ends_with("ok\n"));
        let unauthorized = exchange("GET /metrics HTTP/1.1\r\n\r\n");
        assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");
        let metrics = exchange(&format!(
            "GET /metrics HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"
        ));
        assert!(metrics.starts_with("HTTP/1.1 200 OK\r\n"), "{metrics}");
        assert!(metrics.contains("mailswiftsync_mailbox_jobs"));
        let head = exchange("HEAD /healthz HTTP/1.1\r\n\r\n");
        assert!(head.contains("Content-Length: 3\r\n"));
        assert!(head.ends_with("\r\n\r\n"), "{head}");
    }
}
