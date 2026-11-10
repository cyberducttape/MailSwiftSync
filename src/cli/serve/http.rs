//! Strict, bounded parsing for the fleet API's single HTTP/1 request head,
//! and the connection limits both HTTP listeners share.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A socket whose reads and writes share absolute deadlines.
///
/// A socket timeout bounds one system call, and rustls makes many inside one
/// `read` while it assembles a handshake flight or a record. A peer sending
/// a byte at a time would keep that `read` from ever returning, so the
/// deadline has to be enforced beneath TLS.
pub(crate) struct DeadlineStream {
    stream: TcpStream,
    read_deadline: Instant,
    write_deadline: Instant,
}

impl DeadlineStream {
    /// `request_time` covers the handshake and the whole request.
    pub(crate) fn new(stream: TcpStream, request_time: Duration) -> Self {
        let deadline = Instant::now() + request_time;
        Self {
            stream,
            read_deadline: deadline,
            write_deadline: deadline,
        }
    }

    pub(crate) fn into_inner(self) -> TcpStream {
        self.stream
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(remaining)
}

impl Read for DeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream
            .set_read_timeout(Some(remaining(self.read_deadline)?))?;
        self.stream.read(buffer)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream
            .set_write_timeout(Some(remaining(self.write_deadline)?))?;
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// A stream that can start the clock for writing its response.
pub(crate) trait BeginResponse {
    fn begin_response(&mut self, allowed: Duration);
}

impl BeginResponse for DeadlineStream {
    fn begin_response(&mut self, allowed: Duration) {
        self.write_deadline = Instant::now() + allowed;
    }
}

impl BeginResponse for rustls::StreamOwned<rustls::ServerConnection, DeadlineStream> {
    fn begin_response(&mut self, allowed: Duration) {
        self.sock.begin_response(allowed);
    }
}

/// One admitted connection. Dropping it frees the slot, including when the
/// connection's thread panics, so panics cannot use up the ceiling.
pub(crate) struct ConnectionSlot(Arc<AtomicUsize>);

impl ConnectionSlot {
    pub(crate) fn acquire(active: &Arc<AtomicUsize>, ceiling: usize) -> Option<Self> {
        active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |running| {
                (running < ceiling).then_some(running + 1)
            })
            .ok()
            .map(|_| Self(Arc::clone(active)))
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Answer a connection over the ceiling, then linger until the peer closes.
/// Closing a socket whose request has not been read sends a reset (always on
/// Windows), and the reset can destroy the answer before the peer reads it,
/// so the request is drained after the write side is shut. The linger is
/// bounded in time and bytes.
pub(crate) fn refuse_and_linger(
    mut stream: TcpStream,
    linger: Duration,
    max_drain: usize,
    answer: impl FnOnce(&mut TcpStream) -> io::Result<()>,
) {
    let deadline = Instant::now() + linger;
    let _ = stream.set_write_timeout(Some(linger));
    if answer(&mut stream).is_err() {
        return;
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut discard = [0_u8; 4096];
    let mut drained = 0;
    while drained < max_drain {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            break;
        }
        match stream.read(&mut discard) {
            Ok(0) | Err(_) => break,
            Ok(read) => drained += read,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RequestHead {
    pub(crate) method: String,
    pub(crate) target: String,
    pub(crate) headers: Vec<(String, String)>,
}

pub(crate) fn parse_request_head(bytes: &[u8]) -> Result<RequestHead, String> {
    const MAX_HEADERS: usize = 64;
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);
    match request
        .parse(bytes)
        .map_err(|error| format!("invalid HTTP request: {error}"))?
    {
        httparse::Status::Complete(consumed) if consumed == bytes.len() => {}
        httparse::Status::Complete(_) => {
            return Err("unexpected data after HTTP request head".into());
        }
        httparse::Status::Partial => return Err("incomplete HTTP request head".into()),
    }

    let method = request.method.ok_or("HTTP method is missing")?;
    let target = request.path.ok_or("HTTP request target is missing")?;
    match request.version.ok_or("HTTP version is missing")? {
        // This listener speaks HTTP/1.1 and closes every connection after a
        // single request. Accept HTTP/1.0 clients as a compatible subset, but
        // never reinterpret a future HTTP/1.x version as HTTP/1.1.
        0 | 1 => {}
        _ => return Err("unsupported HTTP minor version".into()),
    }
    let headers = request
        .headers
        .iter()
        .map(|header| {
            let name = header.name.to_owned();
            let value = std::str::from_utf8(header.value)
                .map_err(|_| "HTTP header value is not valid UTF-8".to_owned())?
                .trim()
                .to_owned();
            Ok((name, value))
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(RequestHead {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_complete_request_head_and_preserves_repeated_headers() {
        let parsed =
            parse_request_head(b"GET /metrics HTTP/1.1\r\nX-Test: first\r\nX-Test: second\r\n\r\n")
                .unwrap();
        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.target, "/metrics");
        assert_eq!(parsed.headers.len(), 2);
    }

    #[test]
    fn rejects_malformed_or_incomplete_requests() {
        for input in [
            &b"GET /metrics HTTP/1.1\r\nBad Header: x\r\n\r\n"[..],
            &b"GET /metrics HTTP/1.1\r\nX-Test: x\r\n"[..],
            &b"GET /metrics HTTP/1.1\r\n\r\nbody"[..],
            &b"GET /metrics HTTP/1.1\r\nX-Test: \xff\r\n\r\n"[..],
        ] {
            assert!(parse_request_head(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn refuses_requests_exceeding_the_header_count_bound() {
        let mut request = b"GET /metrics HTTP/1.1\r\n".to_vec();
        for index in 0..65 {
            request.extend_from_slice(format!("X-{index}: v\r\n").as_bytes());
        }
        request.extend_from_slice(b"\r\n");
        assert!(parse_request_head(&request).is_err());
    }

    #[test]
    fn accepts_only_supported_http_one_minor_versions() {
        for version in ["HTTP/1.0", "HTTP/1.1"] {
            let request = format!("GET /healthz {version}\r\n\r\n");
            assert!(parse_request_head(request.as_bytes()).is_ok(), "{version}");
        }

        for version in ["HTTP/1.2", "HTTP/1.9", "HTTP/1.255"] {
            let request = format!("GET /healthz {version}\r\n\r\n");
            assert!(parse_request_head(request.as_bytes()).is_err(), "{version}");
        }
    }

    #[test]
    fn a_connection_slot_is_freed_when_its_thread_panics() {
        let active = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            let slot = ConnectionSlot::acquire(&active, 1).expect("a free slot");
            assert!(ConnectionSlot::acquire(&active, 1).is_none());
            let result = std::thread::Builder::new()
                .name("slot-panic-test".into())
                .spawn(move || {
                    let _slot = slot;
                    std::panic::resume_unwind(Box::new("connection failed"));
                })
                .unwrap()
                .join();
            assert!(result.is_err());
            assert_eq!(active.load(Ordering::Acquire), 0);
        }
    }

    #[test]
    fn a_trickling_peer_cannot_outlast_the_read_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let mut stream = DeadlineStream::new(accepted, Duration::from_millis(200));
        let trickle = std::thread::spawn(move || {
            for _ in 0..200 {
                if peer.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let started = Instant::now();
        let mut buffer = [0_u8; 16];
        let mut received = 0;
        let error = loop {
            match stream.read(&mut buffer) {
                Ok(count) => received += count,
                Err(error) => break error,
            }
        };
        assert!(received > 0, "the peer's bytes arrived before the deadline");
        // The socket's own timeout reports WouldBlock on Unix.
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            "{error:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(stream);
        let _ = trickle.join();
    }
}
