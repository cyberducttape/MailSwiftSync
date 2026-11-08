//! Listener for the control-plane API: bounded HTTP/1.1, one request per
//! connection, optional native TLS, and a fixed connection ceiling.
//!
//! Unlike the read-only fleet API, control requests carry JSON bodies.
//! Only `Content-Length` framing is accepted (no chunked encoding), bodies
//! are capped, and the whole request must arrive within one deadline.

use super::api::{self, Request, Response};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Room for the largest plan the store accepts (measured compact), sent
/// pretty-printed inside its `{plan, revision}` envelope.
pub(super) const MAX_BODY_BYTES: usize = 2 * super::store::MAX_PLAN_BYTES;
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_CONNECTIONS: usize = 32;

pub(super) fn serve(
    listener: TcpListener,
    context: Arc<api::Context>,
    tls: Option<Arc<rustls::ServerConfig>>,
) -> Result<(), String> {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_CONCURRENT_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let context = Arc::clone(&context);
        let tls = tls.clone();
        let slots = Arc::clone(&active);
        let spawned = std::thread::Builder::new()
            .name("control-plane-connection".into())
            .spawn(move || {
                handle_connection(stream, &context, tls);
                slots.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::AcqRel);
        }
    }
    Err("the listening socket closed".into())
}

fn handle_connection(
    mut stream: TcpStream,
    context: &api::Context,
    tls: Option<Arc<rustls::ServerConfig>>,
) {
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let peer = stream.peer_addr().ok().map(|address| address.ip());
    match tls {
        None => exchange(&mut stream, context, peer),
        Some(config) => {
            let Ok(connection) = rustls::ServerConnection::new(config) else {
                return;
            };
            let mut tls = rustls::StreamOwned::new(connection, stream);
            exchange(&mut tls, context, peer);
            tls.conn.send_close_notify();
            let _ = tls.flush();
            stream = tls.sock;
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Deadline-bounded reads over a TCP stream, whether or not TLS wraps it.
trait SetDeadline {
    fn set_remaining(&mut self, remaining: Duration);
}

impl SetDeadline for TcpStream {
    fn set_remaining(&mut self, remaining: Duration) {
        let _ = self.set_read_timeout(Some(remaining.max(Duration::from_millis(1))));
    }
}

impl SetDeadline for rustls::StreamOwned<rustls::ServerConnection, TcpStream> {
    fn set_remaining(&mut self, remaining: Duration) {
        self.sock.set_remaining(remaining);
    }
}

fn exchange<S: Read + Write + SetDeadline>(
    stream: &mut S,
    context: &api::Context,
    peer: Option<std::net::IpAddr>,
) {
    let response = match read_request(stream, Instant::now() + REQUEST_DEADLINE) {
        Ok(mut request) => {
            request.peer = peer;
            api::handle(context, &request)
        }
        Err(response) => response,
    };
    let _ = write_response(stream, &response);
}

fn read_request<S: Read + SetDeadline>(
    stream: &mut S,
    deadline: Instant,
) -> Result<Request, Response> {
    let timeout = || Response::error(408, "Request Timeout", "the request did not arrive in time");
    let mut data = Vec::with_capacity(2048);
    let mut buffer = [0_u8; 4096];
    let head_end = loop {
        if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        if data.len() > MAX_HEAD_BYTES {
            return Err(Response::error(
                431,
                "Request Header Fields Too Large",
                "the request head is too large",
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(timeout());
        }
        stream.set_remaining(remaining);
        match stream.read(&mut buffer) {
            Ok(0) => return Err(Response::error(400, "Bad Request", "incomplete request")),
            Ok(count) => data.extend_from_slice(&buffer[..count]),
            Err(_) => return Err(timeout()),
        }
    };
    if head_end > MAX_HEAD_BYTES {
        return Err(Response::error(
            431,
            "Request Header Fields Too Large",
            "the request head is too large",
        ));
    }
    let head = crate::cli::serve::http::parse_request_head(&data[..head_end])
        .map_err(|_| Response::error(400, "Bad Request", "malformed request"))?;
    let header = |name: &str| {
        head.headers
            .iter()
            .filter(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>()
    };
    for singleton in ["authorization", "host", "content-length", "content-type"] {
        if header(singleton).len() > 1 {
            return Err(Response::error(
                400,
                "Bad Request",
                "ambiguous repeated header",
            ));
        }
    }
    if !header("transfer-encoding").is_empty() {
        return Err(Response::error(
            411,
            "Length Required",
            "send the body with Content-Length",
        ));
    }
    let length = match header("content-length").first() {
        None => 0,
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|_| value.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or_else(|| Response::error(400, "Bad Request", "invalid Content-Length"))?,
    };
    if length > MAX_BODY_BYTES {
        return Err(Response::error(
            413,
            "Content Too Large",
            "the request body is too large",
        ));
    }
    let mut body = data.split_off(head_end);
    while body.len() < length {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(timeout());
        }
        stream.set_remaining(remaining);
        match stream.read(&mut buffer) {
            Ok(0) => {
                return Err(Response::error(
                    400,
                    "Bad Request",
                    "incomplete request body",
                ));
            }
            Ok(count) => body.extend_from_slice(&buffer[..count]),
            Err(_) => return Err(timeout()),
        }
    }
    if body.len() != length {
        return Err(Response::error(
            400,
            "Bad Request",
            "unexpected data after the request body",
        ));
    }
    let authorization = header("authorization")
        .first()
        .map(|value| (*value).to_owned());
    let content_type = header("content-type")
        .first()
        .map(|value| (*value).to_owned());
    Ok(Request {
        method: head.method,
        target: head.target,
        authorization,
        content_type,
        body,
        peer: None,
    })
}

fn write_response(stream: &mut impl Write, response: &Response) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nCross-Origin-Resource-Policy: same-origin\r\nConnection: close\r\n",
        response.status,
        response.reason,
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(response.body.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Script(Cursor<Vec<u8>>);
    impl Read for Script {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(buffer)
        }
    }
    impl SetDeadline for Script {
        fn set_remaining(&mut self, _: Duration) {}
    }

    fn read(raw: &[u8]) -> Result<Vec<u8>, u16> {
        read_request(
            &mut Script(Cursor::new(raw.to_vec())),
            Instant::now() + Duration::from_secs(5),
        )
        .map(|request| request.body)
        .map_err(|response| response.status)
    }

    #[test]
    fn bodies_need_exact_content_length_framing() {
        let request =
            read(b"POST /v1/auth/login HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\n{}")
                .unwrap();
        assert_eq!(request, b"{}");
        assert_eq!(
            read(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"),
            Err(411)
        );
        assert_eq!(
            read(b"POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab"),
            Err(400)
        );
        assert_eq!(
            read(b"POST / HTTP/1.1\r\nContent-Length: +2\r\n\r\nab"),
            Err(400)
        );
        assert_eq!(
            read(b"POST / HTTP/1.1\r\nContent-Length: 1\r\n\r\nab"),
            Err(400)
        );
        assert_eq!(
            read(b"POST / HTTP/1.1\r\nContent-Length: 4\r\n\r\nab"),
            Err(400)
        );
        let oversized = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        assert_eq!(read(oversized.as_bytes()), Err(413));
        assert_eq!(
            read(b"GET / HTTP/1.1\r\nAuthorization: Bearer a\r\nAuthorization: Bearer b\r\n\r\n"),
            Err(400)
        );
    }

    #[test]
    fn the_largest_storable_plan_fits_in_a_request() {
        use super::super::store::{MAX_PLAN_BYTES, test_plan, validate_plan};
        let mut plan = test_plan("ledger");
        let mut length = serde_json::to_string(&plan).unwrap().len();
        let mailboxes = plan["mailboxes"].as_object_mut().unwrap();
        for index in 0.. {
            let key = format!("job-{index:06}");
            let value = serde_json::json!({ "label": format!("Mailbox {index}"), "plan_sha256": "a".repeat(64) });
            // `"key":value,` in compact form.
            let added = key.len() + serde_json::to_string(&value).unwrap().len() + 4;
            if length + added > MAX_PLAN_BYTES {
                break;
            }
            length += added;
            mailboxes.insert(key, value);
        }
        validate_plan(&plan).unwrap();
        let compact = serde_json::to_string(&plan).unwrap().len();
        assert!(compact > MAX_PLAN_BYTES - 200, "{compact}");
        let body =
            serde_json::to_string_pretty(&serde_json::json!({ "plan": plan, "revision": 1 }))
                .unwrap();
        let raw = format!(
            "PUT /v1/tenants/t/projects/p/plan HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        assert_eq!(read(raw.as_bytes()).map(|body| body.len()), Ok(body.len()));
    }

    /// Opt-in: `MAILSWIFTSYNC_BENCH_DIR=target cargo test --release
    /// control_plane_concurrency_benchmark -- --ignored --nocapture`. Audited reads over loopback HTTP against an
    /// on-disk WAL database; every request also commits an audit entry, so
    /// this measures the single serialized connection under write load.
    #[test]
    #[ignore = "benchmark"]
    fn control_plane_concurrency_benchmark() {
        use super::super::store::{Actor, Store, test_plan};
        // Point MAILSWIFTSYNC_BENCH_DIR at real disk: on tmpfs, fsync is free.
        let directory = std::env::var_os("MAILSWIFTSYNC_BENCH_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("mss-control-bench-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let store = Store::open(&directory.join("control.db")).unwrap();
        let password = super::super::authn::test_secret();
        let admin = store
            .bootstrap_administrator("admin@msp.example", "Admin", &password)
            .unwrap();
        let actor = Actor {
            request_id: "bench",
            label: format!("user:{admin}"),
        };
        let mut tenants = Vec::new();
        for index in 0..20 {
            let tenant = store
                .create_tenant(&format!("Tenant {index}"), &actor)
                .unwrap();
            for project in 0..5 {
                store
                    .create_project(
                        &tenant.id,
                        &format!("Move {project}"),
                        &test_plan("ledger"),
                        &actor,
                    )
                    .unwrap();
            }
            tenants.push(tenant.id);
        }
        let (token, _) = store
            .login("admin@msp.example", &password, "bench", None, false)
            .unwrap();
        let token = token.to_string();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let context = Arc::new(api::Context { store, oidc: None });
        std::thread::spawn(move || serve(listener, context, None));

        for clients in [1_usize, 8, 32] {
            let deadline = Instant::now() + Duration::from_secs(5);
            let latencies = std::thread::scope(|scope| {
                let workers = (0..clients)
                    .map(|client| {
                        let token = &token;
                        let tenants = &tenants;
                        scope.spawn(move || {
                            let http = reqwest::blocking::Client::new();
                            let mut latencies = Vec::new();
                            let mut refused = Vec::new();
                            let mut index = client;
                            while Instant::now() < deadline {
                                let tenant = &tenants[index % tenants.len()];
                                index += 1;
                                let started = Instant::now();
                                match http
                                    .get(format!("http://{address}/v1/tenants/{tenant}/projects"))
                                    .bearer_auth(token)
                                    .send()
                                {
                                    Ok(response) if response.status() == 200 => {
                                        latencies.push(started.elapsed());
                                    }
                                    Ok(response) => refused.push(response.status().to_string()),
                                    Err(error) => refused.push(error.to_string()),
                                }
                            }
                            (latencies, refused)
                        })
                    })
                    .collect::<Vec<_>>();
                let mut latencies = Vec::new();
                let mut refused = Vec::new();
                for worker in workers {
                    let (mut ok, mut failed) = worker.join().unwrap();
                    latencies.append(&mut ok);
                    refused.append(&mut failed);
                }
                (latencies, refused)
            });
            let (latencies, refused) = latencies;
            if let Some(first) = refused.first() {
                println!(
                    "control-plane clients={clients} refused={} first={first}",
                    refused.len()
                );
            }
            let mut sorted = latencies;
            sorted.sort();
            let percentile = |p: usize| sorted[(sorted.len() * p / 100).min(sorted.len() - 1)];
            println!(
                "control-plane clients={clients} requests={} per_second={:.0} p50_ms={:.1} p99_ms={:.1}",
                sorted.len(),
                sorted.len() as f64 / 5.0,
                percentile(50).as_secs_f64() * 1000.0,
                percentile(99).as_secs_f64() * 1000.0,
            );
        }
        let _ = std::fs::remove_dir_all(directory);
    }
}
