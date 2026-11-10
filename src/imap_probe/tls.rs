//! Certificate-validated IMAP transport setup and TLS pin enforcement.

use super::*;
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use sha2::{Digest, Sha256};
use std::{net::TcpStream, sync::Arc};

const BUDGETED_IMAP_IO_SLICE: Duration = Duration::from_millis(250);
const UNBUDGETED_SESSION_LIMIT: Duration = Duration::from_secs(30);

/// The socket beneath TLS, refusing I/O once the session's deadline passes.
/// Socket timeouts bound one system call, and rustls makes many inside one
/// `read` while it assembles a record, so a server sending a byte at a time
/// could otherwise hold that `read` past every deadline checked around it.
pub(super) struct DeadlineSocket {
    tcp: TcpStream,
    deadline: Instant,
}

impl DeadlineSocket {
    fn new(tcp: TcpStream, deadline: Instant) -> Self {
        Self { tcp, deadline }
    }

    fn check(&self) -> std::io::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "IMAP session deadline exceeded",
            ));
        }
        Ok(())
    }
}

impl std::io::Read for DeadlineSocket {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.check()?;
        self.tcp.read(buffer)
    }
}

impl std::io::Write for DeadlineSocket {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.check()?;
        self.tcp.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.tcp.flush()
    }
}

pub(super) fn connect_tls_stream_with_budget(
    host: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
    budget: &MessageFetchBudget<'_>,
) -> Result<(StreamOwned<ClientConnection, DeadlineSocket>, String), String> {
    connect_tls_stream_inner(
        host,
        transport,
        ca_bundle,
        certificate_pin_sha256,
        Some(budget),
    )
}

pub(super) fn connect_tls_stream_inner(
    host: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
    budget: Option<&MessageFetchBudget<'_>>,
) -> Result<(StreamOwned<ClientConnection, DeadlineSocket>, String), String> {
    let session_deadline = budget.map_or_else(
        || Instant::now() + UNBUDGETED_SESSION_LIMIT,
        |budget| budget.deadline,
    );
    let (server_name, port) = crate::endpoint::parts(host, crate::default_imap_port(transport))
        .map_err(|error| format!("Invalid IMAP host {host}: {error}"))?;
    let address = if server_name.contains(':') {
        format!("[{server_name}]:{port}")
    } else {
        format!("{server_name}:{port}")
    };
    let dns_deadline = budget
        .map(|budget| budget.deadline.min(Instant::now() + Duration::from_secs(8)))
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(8));
    let cancelled =
        || budget.is_some_and(|budget| budget.cancel.load(std::sync::atomic::Ordering::Relaxed));
    let sockets = resolve_dns_with_deadline(&address, dns_deadline, &cancelled)
        .map_err(|error| format!("{host}: {error}"))?;
    if sockets.is_empty() {
        return Err(format!("{host}: no address found"));
    }
    let connect_deadline = budget
        .map(|budget| budget.deadline.min(Instant::now() + Duration::from_secs(8)))
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(8));
    let mut tcp = connect_racing(sockets, connect_deadline, &cancelled)
        .map_err(|error| format!("{host}: {error}"))?;
    let io_timeout = budget
        .map(|budget| {
            budget
                .deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(8))
        })
        .unwrap_or_else(|| Duration::from_secs(8));
    if io_timeout.is_zero() {
        return Err(format!("{host}: connection deadline exceeded"));
    }
    tcp.set_read_timeout(Some(io_timeout))
        .map_err(|error| error.to_string())?;
    tcp.set_write_timeout(Some(io_timeout))
        .map_err(|error| error.to_string())?;

    let mut roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if !ca_bundle.trim().is_empty() {
        let certificates = CertificateDer::pem_file_iter(ca_bundle.trim())
            .map_err(|error| format!("{host}: could not open additional CA bundle: {error}"))?;
        let mut loaded = 0;
        for certificate in certificates {
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
    let name = ServerName::try_from(server_name)
        .map_err(|error| format!("{host}: invalid TLS server name: {error}"))?;

    if transport == "starttls" {
        let mut response = String::new();
        let mut buffer = [0; 4096];
        let greeting = read_imap_greeting(&mut tcp, host, budget)?;
        write_imap_command(
            &mut tcp,
            b"s001 CAPABILITY\r\n",
            budget,
            "could not write CAPABILITY",
        )?;
        read_imap_tagged_with_optional_budget(
            &mut tcp,
            "s001",
            &mut response,
            &mut buffer,
            budget,
        )?;
        if !imap_command_succeeded(&response, "s001")
            || !advertises_capability(&response, "STARTTLS")
        {
            return Err(format!("{host}: server does not advertise STARTTLS"));
        }
        write_imap_command(
            &mut tcp,
            b"s002 STARTTLS\r\n",
            budget,
            "could not write STARTTLS",
        )?;
        read_imap_tagged_with_optional_budget(
            &mut tcp,
            "s002",
            &mut response,
            &mut buffer,
            budget,
        )?;
        if !imap_command_succeeded(&response, "s002") {
            return Err(format!("{host}: STARTTLS negotiation failed"));
        }
        let connection = ClientConnection::new(Arc::new(config), name)
            .map_err(|error| format!("{host}: TLS configuration failed: {error}"))?;
        let mut stream = StreamOwned::new(connection, DeadlineSocket::new(tcp, session_deadline));
        complete_tls_handshake(&mut stream, host, budget)?;
        refresh_socket_timeout(&stream.sock.tcp, budget)
            .map_err(|error| format!("{host}: could not set TLS I/O timeout: {error}"))?;
        verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
        return Ok((stream, greeting));
    }

    let connection = ClientConnection::new(Arc::new(config), name)
        .map_err(|error| format!("{host}: TLS configuration failed: {error}"))?;
    let mut stream = StreamOwned::new(connection, DeadlineSocket::new(tcp, session_deadline));
    refresh_socket_timeout(&stream.sock.tcp, budget)
        .map_err(|error| format!("{host}: could not set TLS I/O timeout: {error}"))?;
    let greeting = read_imap_greeting(&mut stream, host, budget)?;
    verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
    Ok((stream, greeting))
}

fn complete_tls_handshake(
    stream: &mut StreamOwned<ClientConnection, DeadlineSocket>,
    host: &str,
    budget: Option<&MessageFetchBudget<'_>>,
) -> Result<(), String> {
    let deadline = budget
        .map(|budget| budget.deadline)
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(8))
        .min(Instant::now() + Duration::from_secs(8));
    complete_tls_handshake_until(stream, host, budget, deadline)
}

fn complete_tls_handshake_until(
    stream: &mut StreamOwned<ClientConnection, DeadlineSocket>,
    host: &str,
    budget: Option<&MessageFetchBudget<'_>>,
    deadline: Instant,
) -> Result<(), String> {
    loop {
        if Instant::now() >= deadline {
            return Err(format!("{host}: TLS handshake deadline exceeded"));
        }
        if let Some(budget) = budget {
            budget.check()?;
        }
        refresh_handshake_socket_timeout(&stream.sock.tcp, deadline, budget.is_some())
            .map_err(|error| format!("{host}: could not set TLS handshake timeout: {error}"))?;
        match stream.conn.complete_io(&mut stream.sock) {
            Ok(_) if !stream.conn.is_handshaking() => return Ok(()),
            Ok(_) => continue,
            Err(error)
                if budget.is_some()
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
            {
                super::wait_for_io_retry(deadline, budget.map(|budget| budget.cancel));
            }
            Err(error) => return Err(format!("{host}: TLS handshake failed: {error}")),
        }
    }
}

fn refresh_handshake_socket_timeout(
    stream: &TcpStream,
    deadline: Instant,
    budgeted: bool,
) -> std::io::Result<()> {
    let slice = if budgeted {
        BUDGETED_IMAP_IO_SLICE
    } else {
        Duration::from_secs(8)
    };
    let timeout = deadline
        .saturating_duration_since(Instant::now())
        .min(slice);
    if timeout.is_zero() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "TLS handshake deadline exceeded",
        ));
    }
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(())
}

fn refresh_socket_timeout(
    stream: &TcpStream,
    budget: Option<&MessageFetchBudget<'_>>,
) -> std::io::Result<()> {
    let timeout = budget
        .map(|budget| {
            budget
                .deadline
                .saturating_duration_since(Instant::now())
                .min(BUDGETED_IMAP_IO_SLICE)
        })
        .unwrap_or_else(|| Duration::from_secs(8));
    if timeout.is_zero() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "message fetch deadline exceeded",
        ));
    }
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(())
}

fn verify_certificate_pin(
    stream: &StreamOwned<ClientConnection, DeadlineSocket>,
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
    if !certificate_matches_pin(certificate.as_ref(), expected) {
        return Err(format!("{host}: TLS certificate SHA-256 pin mismatch"));
    }
    Ok(())
}

fn certificate_matches_pin(certificate: &[u8], expected: &str) -> bool {
    let digest = Sha256::digest(certificate);
    let actual = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    actual == expected.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{certificate_matches_pin, complete_tls_handshake_until};
    use crate::imap_probe::MessageFetchBudget;
    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn certificate_pin_matches_exact_der_digest_case_insensitively() {
        let digest = "03d66dd08835c1ca3f128cceacd1f31ac94163096b20f445ae84285bc0832d72";
        assert!(certificate_matches_pin(b"certificate", digest));
        assert!(certificate_matches_pin(
            b"certificate",
            &format!("  {}  ", digest.to_ascii_uppercase())
        ));
        assert!(!certificate_matches_pin(
            b"certificate",
            "13d66dd08835c1ca3f128cceacd1f31ac94163096b20f445ae84285bc0832d72"
        ));
    }

    #[test]
    fn a_server_trickling_tls_bytes_cannot_outlast_the_session_deadline() {
        use std::io::Write as _;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            // A record header announcing a large handshake record, then its
            // body one byte at a time.
            let _ = peer.write_all(&[0x16, 0x03, 0x03, 0x40, 0x00]);
            for _ in 0..300 {
                if peer.write_all(&[0x02]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let config = ClientConfig::builder()
            .with_root_certificates(RootCertStore::empty())
            .with_no_client_auth();
        let name = ServerName::try_from("example.com").unwrap();
        let connection = ClientConnection::new(Arc::new(config), name).unwrap();
        let start = Instant::now();
        let mut stream = StreamOwned::new(
            connection,
            super::DeadlineSocket::new(socket, start + Duration::from_millis(250)),
        );
        let mut byte = [0_u8; 1];
        assert!(std::io::Read::read(&mut stream, &mut byte).is_err());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the read was bounded only per byte"
        );
        drop(stream);
        let _ = server.join();
    }

    #[test]
    fn unbudgeted_tls_handshake_obeys_an_absolute_deadline_during_partial_io() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(200));
        });
        let socket = TcpStream::connect(address).unwrap();
        let config = ClientConfig::builder()
            .with_root_certificates(RootCertStore::empty())
            .with_no_client_auth();
        let name = ServerName::try_from("example.com").unwrap();
        let connection = ClientConnection::new(Arc::new(config), name).unwrap();
        let mut stream = StreamOwned::new(
            connection,
            super::DeadlineSocket::new(socket, Instant::now() + Duration::from_secs(10)),
        );
        let start = Instant::now();
        let result = complete_tls_handshake_until(
            &mut stream,
            "example.com",
            None,
            start + Duration::from_millis(40),
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_millis(150));
        server.join().unwrap();
    }

    #[test]
    fn probe_tls_handshake_obeys_total_budget_without_verification_limit_classification() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(200));
        });
        let socket = TcpStream::connect(address).unwrap();
        let config = ClientConfig::builder()
            .with_root_certificates(RootCertStore::empty())
            .with_no_client_auth();
        let name = ServerName::try_from("example.com").unwrap();
        let connection = ClientConnection::new(Arc::new(config), name).unwrap();
        let mut stream = StreamOwned::new(
            connection,
            super::DeadlineSocket::new(socket, Instant::now() + Duration::from_secs(10)),
        );
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let budget = MessageFetchBudget::for_probe(Duration::from_millis(40), &cancel);
        let start = Instant::now();

        let error = complete_tls_handshake_until(
            &mut stream,
            "example.com",
            Some(&budget),
            budget.deadline,
        )
        .unwrap_err();

        assert!(error.contains("TLS handshake deadline exceeded"));
        assert_eq!(crate::core::VerificationLimit::from_detail(&error), None);
        assert!(start.elapsed() < Duration::from_millis(150));
        server.join().unwrap();
    }
}
