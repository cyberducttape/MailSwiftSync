//! Connection limits both HTTP listeners share. Separate from the request
//! parser, which the fuzz targets include without the TLS stack.

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

#[cfg(test)]
mod tests {
    use super::*;

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
