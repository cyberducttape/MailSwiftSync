//! Certificate-validated IMAP transport setup and TLS pin enforcement.

use super::*;
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use sha2::{Digest, Sha256};
use std::{net::TcpStream, sync::Arc};

const BUDGETED_IMAP_IO_SLICE: Duration = Duration::from_millis(250);

/// Open an IMAP connection and establish a certificate-validated TLS session.
pub(crate) fn connect_tls_stream(
    host: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
) -> Result<(StreamOwned<ClientConnection, TcpStream>, String), String> {
    connect_tls_stream_inner(host, transport, ca_bundle, certificate_pin_sha256, None)
}

pub(super) fn connect_tls_stream_with_budget(
    host: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
    budget: &MessageFetchBudget<'_>,
) -> Result<(StreamOwned<ClientConnection, TcpStream>, String), String> {
    connect_tls_stream_inner(
        host,
        transport,
        ca_bundle,
        certificate_pin_sha256,
        Some(budget),
    )
}

fn connect_tls_stream_inner(
    host: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
    budget: Option<&MessageFetchBudget<'_>>,
) -> Result<(StreamOwned<ClientConnection, TcpStream>, String), String> {
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
        let mut stream = StreamOwned::new(connection, tcp);
        complete_tls_handshake(&mut stream, host, budget)?;
        refresh_socket_timeout(&stream.sock, budget)
            .map_err(|error| format!("{host}: could not set TLS I/O timeout: {error}"))?;
        verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
        return Ok((stream, greeting));
    }

    let connection = ClientConnection::new(Arc::new(config), name)
        .map_err(|error| format!("{host}: TLS configuration failed: {error}"))?;
    let mut stream = StreamOwned::new(connection, tcp);
    refresh_socket_timeout(&stream.sock, budget)
        .map_err(|error| format!("{host}: could not set TLS I/O timeout: {error}"))?;
    let greeting = read_imap_greeting(&mut stream, host, budget)?;
    verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
    Ok((stream, greeting))
}

fn complete_tls_handshake(
    stream: &mut StreamOwned<ClientConnection, TcpStream>,
    host: &str,
    budget: Option<&MessageFetchBudget<'_>>,
) -> Result<(), String> {
    loop {
        if let Some(budget) = budget {
            budget.check()?;
            refresh_socket_timeout(&stream.sock, Some(budget))
                .map_err(|error| format!("{host}: could not set TLS handshake timeout: {error}"))?;
        }
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
                continue;
            }
            Err(error) => return Err(format!("{host}: TLS handshake failed: {error}")),
        }
    }
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
    use super::certificate_matches_pin;

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
}
