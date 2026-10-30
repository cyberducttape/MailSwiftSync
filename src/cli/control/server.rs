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
pub(super) const MAX_BODY_BYTES: usize = 64 * 1024;
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
}
