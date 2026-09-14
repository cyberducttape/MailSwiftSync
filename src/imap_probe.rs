use crate::core;
use crate::credentials::SecretString;
use crate::imap_protocol::{
    advertises_capability, atom_eq, is_tagged_response, is_untagged_response,
};
use crate::oauth::{read_auth_continuation, read_auth_result};
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) fn endpoint_for_probe(host: &str, configured_port: &str) -> Result<String, String> {
    let host = host.trim();
    if configured_port.trim().is_empty() {
        return Ok(host.to_owned());
    }
    let port = configured_port
        .trim()
        .parse::<u16>()
        .map_err(|_| "invalid endpoint port".to_owned())?;
    if port == 0 {
        return Err("endpoint port must be between 1 and 65535".into());
    }
    let (host, _) = crate::endpoint::parts(host, 993)?;
    if host.contains(':') {
        Ok(format!("[{host}]:{port}"))
    } else {
        Ok(format!("{host}:{port}"))
    }
}

/// Command previews and fingerprints must not silently turn malformed input
/// into a different endpoint. Validation rejects the sentinel before launch;
/// this helper keeps tuple-based command-generation APIs fail-closed too.
pub(crate) fn command_endpoint_parts(host: &str, default_port: u16) -> (String, u16) {
    crate::endpoint::parts(host, default_port)
        .unwrap_or_else(|_| ("<invalid-endpoint>".to_owned(), 0))
}

pub(crate) fn command_port(configured_port: &str, endpoint_port: u16) -> u16 {
    match configured_port.trim().parse::<u16>() {
        Ok(port) if port != 0 => port,
        _ if configured_port.trim().is_empty() => endpoint_port,
        _ => 0,
    }
}

pub(crate) fn imap_quote(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err("IMAP quoted value cannot contain control characters".into());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn read_imap_tagged<S: Read>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
) -> Result<(), String> {
    loop {
        let count = stream.read(buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err(format!("IMAP connection closed before {tag} completed"));
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..count]));
        if response.lines().any(|line| is_tagged_response(line, tag)) {
            return Ok(());
        }
        if response.len() > 1_048_576 {
            return Err("IMAP preflight response exceeded 1 MiB".into());
        }
    }
}

const MAX_IMAP_LIST_LINE_BYTES: usize = 64 * 1024;
const MAX_IMAP_LIST_LITERAL_BYTES: usize = 1024 * 1024;
const MAX_IMAP_LIST_MAILBOXES: usize = 100_000;
const MAX_IMAP_LIST_DURATION: Duration = Duration::from_secs(60);

#[derive(Debug, Default, PartialEq, Eq)]
struct ListInventorySummary {
    mailbox_count: usize,
    special_use_mailboxes: usize,
}

/// Consume an authenticated LIST response without retaining the complete
/// inventory. LIST may contain literals and arbitrarily many folders, so the
/// safety limits apply to individual records, literals, processing time, and
/// mailbox count rather than to one growing response string. There is
/// intentionally no aggregate byte ceiling: a legitimate folder-heavy
/// account may exceed several megabytes while still remaining bounded in
/// memory by the record and count limits.
fn read_imap_list_response<S: Read>(
    stream: &mut S,
    tag: &str,
    buffer: &mut [u8; 4096],
) -> Result<ListInventorySummary, String> {
    let mut line = Vec::new();
    let started = Instant::now();
    let mut literal_remaining = 0_usize;
    let mut literal_separator_remaining = 0_u8;
    let mut summary = ListInventorySummary::default();
    loop {
        let count = stream.read(buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            return Err(format!("IMAP connection closed before {tag} completed"));
        }
        if started.elapsed() > MAX_IMAP_LIST_DURATION {
            return Err("IMAP LIST response exceeded the 60-second processing limit".into());
        }
        let mut offset = 0;
        while offset < count {
            if literal_remaining > 0 {
                let consumed = literal_remaining.min(count - offset);
                literal_remaining -= consumed;
                offset += consumed;
                if literal_remaining == 0 {
                    literal_separator_remaining = 2;
                }
                continue;
            }
            if literal_separator_remaining > 0 {
                let consumed = literal_separator_remaining.min((count - offset) as u8);
                literal_separator_remaining -= consumed;
                offset += consumed as usize;
                continue;
            }
            let byte = buffer[offset];
            offset += 1;
            line.push(byte);
            if line.len() > MAX_IMAP_LIST_LINE_BYTES {
                return Err("IMAP LIST response record exceeded 64 KiB".into());
            }
            if byte != b'\n' {
                continue;
            }
            let text = String::from_utf8_lossy(&line);
            let text = text.trim_end_matches(['\r', '\n']);
            if is_untagged_response(text, "LIST") {
                summary.mailbox_count = summary.mailbox_count.saturating_add(1);
                if summary.mailbox_count > MAX_IMAP_LIST_MAILBOXES {
                    return Err(format!(
                        "IMAP LIST response exceeded the {MAX_IMAP_LIST_MAILBOXES}-mailbox limit"
                    ));
                }
                if crate::imap_protocol::list_has_attribute(text, r"\ALL")
                    || crate::imap_protocol::list_has_attribute(text, r"\ARCHIVE")
                    || crate::imap_protocol::list_has_attribute(text, r"\DRAFTS")
                    || crate::imap_protocol::list_has_attribute(text, r"\FLAGGED")
                    || crate::imap_protocol::list_has_attribute(text, r"\JUNK")
                    || crate::imap_protocol::list_has_attribute(text, r"\SENT")
                    || crate::imap_protocol::list_has_attribute(text, r"\TRASH")
                {
                    summary.special_use_mailboxes = summary.special_use_mailboxes.saturating_add(1);
                }
            }
            if is_tagged_response(text, tag) {
                let status = text.split_whitespace().nth(1);
                if !status.is_some_and(|status| atom_eq(status, "OK")) {
                    return Err(format!("IMAP LIST command {tag} failed"));
                }
                return Ok(summary);
            }
            if let Some(literal_size) = list_literal_size(text)
                && literal_size > 0
            {
                if literal_size > MAX_IMAP_LIST_LITERAL_BYTES {
                    return Err("IMAP LIST literal exceeded 1 MiB".into());
                }
                literal_remaining = literal_size;
            }
            line.clear();
        }
    }
}

fn list_literal_size(line: &str) -> Option<usize> {
    let end = line.strip_suffix('}')?;
    let start = end.rfind('{')? + 1;
    end[start..].parse().ok()
}

fn authenticated_list_command(request_special_use: bool) -> &'static [u8] {
    if request_special_use {
        b"a005 LIST \"\" \"*\" RETURN (SPECIAL-USE)\r\n"
    } else {
        b"a005 LIST \"\" \"*\"\r\n"
    }
}

fn read_imap_greeting<S: Read>(stream: &mut S, host: &str) -> Result<String, String> {
    let mut response = String::new();
    let mut buffer = [0; 4096];
    loop {
        let count = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..count]));
        if response.contains("\r\n") || response.len() > 65_536 {
            break;
        }
    }
    if !is_untagged_response(&response, "OK") && !is_untagged_response(&response, "PREAUTH") {
        return Err(format!("{host}: server greeting was missing or invalid"));
    }
    Ok(response)
}

pub(crate) fn imap_command_succeeded(response: &str, tag: &str) -> bool {
    response.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some(tag) && fields.next().is_some_and(|status| atom_eq(status, "OK"))
    })
}

pub(crate) fn probe_tls_capabilities_with_transport(
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
) -> Result<crate::core::ServerCapabilities, String> {
    let (server_name, port) = crate::endpoint::parts(host, crate::default_imap_port(transport))
        .map_err(|error| format!("Invalid IMAP host {host}: {error}"))?;
    let address = if server_name.contains(':') {
        format!("[{server_name}]:{port}")
    } else {
        format!("{server_name}:{port}")
    };
    let sockets = address
        .to_socket_addrs()
        .map_err(|error| format!("{host}: {error}"))?
        .collect::<Vec<_>>();
    if sockets.is_empty() {
        return Err(format!("{host}: no address found"));
    }
    let mut last_error = None;
    let mut tcp = None;
    for socket in sockets {
        match TcpStream::connect_timeout(&socket, Duration::from_secs(8)) {
            Ok(stream) => {
                tcp = Some(stream);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let mut tcp = tcp.ok_or_else(|| {
        format!(
            "{host}: could not connect to any resolved address: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown connection error".into())
        )
    })?;
    tcp.set_read_timeout(Some(Duration::from_secs(8)))
        .map_err(|e| e.to_string())?;
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
    let name = ServerName::try_from(server_name.to_owned())
        .map_err(|e| format!("{host}: invalid TLS server name: {e}"))?;

    if transport == "starttls" {
        let mut response = String::new();
        let mut buffer = [0; 4096];
        let greeting = read_imap_greeting(&mut tcp, host)?;
        tcp.write_all(b"s001 CAPABILITY\r\n")
            .map_err(|e| e.to_string())?;
        read_imap_tagged(&mut tcp, "s001", &mut response, &mut buffer)?;
        if !imap_command_succeeded(&response, "s001")
            || !advertises_capability(&response, "STARTTLS")
        {
            return Err(format!("{host}: server does not advertise STARTTLS"));
        }
        tcp.write_all(b"s002 STARTTLS\r\n")
            .map_err(|e| e.to_string())?;
        read_imap_tagged(&mut tcp, "s002", &mut response, &mut buffer)?;
        if !imap_command_succeeded(&response, "s002") {
            return Err(format!("{host}: STARTTLS negotiation failed"));
        }
        let connection = ClientConnection::new(Arc::new(config), name)
            .map_err(|e| format!("{host}: TLS configuration failed: {e}"))?;
        let mut stream = StreamOwned::new(connection, tcp);
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(|error| format!("{host}: TLS handshake failed: {error}"))?;
        verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
        return complete_authenticated_imap_probe(
            stream,
            host,
            user,
            credential,
            auth_method,
            greeting,
        );
    }

    let connection = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| format!("{host}: TLS configuration failed: {e}"))?;
    let mut stream = StreamOwned::new(connection, tcp);
    let greeting = read_imap_greeting(&mut stream, host)?;
    verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
    complete_authenticated_imap_probe(stream, host, user, credential, auth_method, greeting)
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
    let actual = format!("{:x}", Sha256::digest(certificate.as_ref()));
    if actual != expected.trim().to_ascii_lowercase() {
        return Err(format!(
            "{host}: TLS certificate SHA-256 pin mismatch (presented {actual})"
        ));
    }
    Ok(())
}

fn complete_authenticated_imap_probe<S: Read + Write>(
    mut stream: S,
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    greeting: String,
) -> Result<crate::core::ServerCapabilities, String> {
    let mut response = String::new();
    let mut buffer = [0; 4096];
    stream
        .write_all(b"a001 CAPABILITY\r\n")
        .map_err(|e| e.to_string())?;
    read_imap_tagged(&mut stream, "a001", &mut response, &mut buffer)?;
    if !imap_command_succeeded(&response, "a001") {
        return Err(format!("{host}: pre-auth CAPABILITY failed"));
    }
    let preauth = greeting
        .lines()
        .any(|line| is_untagged_response(line, "PREAUTH"));
    if !preauth {
        if auth_method == "oauth2" {
            let encoded = crate::oauth::xoauth2_payload(user, credential);
            stream
                .write_all(b"a002 AUTHENTICATE XOAUTH2\r\n")
                .map_err(|e| e.to_string())?;
            response.clear();
            read_auth_continuation(&mut stream, "a002", &mut response, &mut buffer)?;
            stream
                .write_all(encoded.as_bytes())
                .and_then(|_| stream.write_all(b"\r\n"))
                .map_err(|e| e.to_string())?;
            response.clear();
            read_auth_result(&mut stream, "a002", &mut response, &mut buffer)?;
        } else {
            let quoted_password = SecretString::new(imap_quote(credential)?);
            let login = format!(
                "a002 LOGIN {} {}\r\n",
                imap_quote(user)?,
                quoted_password.as_str()
            );
            let login = SecretString::new(login);
            stream
                .write_all(login.as_bytes())
                .map_err(|e| e.to_string())?;
            read_imap_tagged(&mut stream, "a002", &mut response, &mut buffer)?;
        }
        if !imap_command_succeeded(&response, "a002") {
            return Err(format!("{host}: IMAP authentication failed"));
        }
    }
    // RFC 9051 permits capabilities to change after authentication, so the
    // post-auth response is the one used for readiness decisions.
    stream
        .write_all(b"a003 CAPABILITY\r\n")
        .map_err(|e| e.to_string())?;
    let mut post_auth_response = String::new();
    read_imap_tagged(&mut stream, "a003", &mut post_auth_response, &mut buffer)?;
    if !imap_command_succeeded(&post_auth_response, "a003") {
        return Err(format!("{host}: post-auth CAPABILITY failed"));
    }
    stream
        .write_all(b"a004 NAMESPACE\r\n")
        .map_err(|e| e.to_string())?;
    let mut _namespace_response = String::new();
    read_imap_tagged(&mut stream, "a004", &mut _namespace_response, &mut buffer)?;
    // NAMESPACE is useful for mapping, but not required by IMAP or by every
    // usable migration endpoint. LIST remains the authoritative inventory
    // gate; callers may surface this response as a compatibility warning.
    stream
        .write_all(authenticated_list_command(advertises_capability(
            &post_auth_response,
            "SPECIAL-USE",
        )))
        .map_err(|e| e.to_string())?;
    let list_summary = read_imap_list_response(&mut stream, "a005", &mut buffer)
        .map_err(|error| format!("{host}: folder inventory failed: {error}"))?;
    if list_summary.mailbox_count == 0 {
        return Err(format!(
            "{host}: folder inventory returned no untagged LIST records"
        ));
    }
    let caps = crate::core::ServerCapabilities::from_inventory_summary(
        &post_auth_response,
        list_summary.mailbox_count,
        list_summary.special_use_mailboxes,
    );
    if caps.values.is_empty() {
        return Err(format!(
            "{host}: server did not return a CAPABILITY response"
        ));
    }
    let mut caps = caps;
    if caps.supports("QUOTA") {
        let mut quota_response = String::new();
        stream
            .write_all(b"a006 GETQUOTAROOT \"\"\r\n")
            .map_err(|e| e.to_string())?;
        // A provider can advertise QUOTA while denying the root query. Keep
        // that result explicitly unknown; quota support is advisory and must
        // not turn an otherwise valid mailbox into a false capacity failure.
        if read_imap_tagged(&mut stream, "a006", &mut quota_response, &mut buffer).is_ok() {
            caps.record_quota_response(&quota_response);
        }
    }
    let _ = stream.write_all(b"a007 LOGOUT\r\n");
    Ok(caps)
}

pub(crate) fn fresh_dual_imaps_authentication(form: &crate::Form) -> Result<(), String> {
    if !fresh_imap_authentication_applies(form) {
        return Ok(());
    }
    let source = endpoint_for_probe(&form.profile.source_host, &form.profile.source_port)?;
    let destination = endpoint_for_probe(
        &form.profile.destination_host,
        &form.profile.destination_port,
    )?;
    let source_capabilities = probe_tls_capabilities_with_transport(
        &source,
        &form.profile.source_user,
        form.source_password.as_str(),
        &form.profile.source_auth,
        &form.profile.source_tls,
        &form.profile.source_ca_bundle,
        &form.profile.source_certificate_pin_sha256,
    )?;
    if source_capabilities.quota_exceeded {
        return Err(
            "source mailbox quota is exhausted according to the authenticated IMAP quota response"
                .into(),
        );
    }
    let destination_capabilities = probe_tls_capabilities_with_transport(
        &destination,
        &form.profile.destination_user,
        form.destination_password.as_str(),
        &form.profile.destination_auth,
        &form.profile.destination_tls,
        &form.profile.destination_ca_bundle,
        &form.profile.destination_certificate_pin_sha256,
    )?;
    if destination_capabilities.quota_exceeded {
        return Err("destination mailbox quota is exhausted according to the authenticated IMAP quota response".into());
    }
    Ok(())
}

pub(crate) fn fresh_imap_authentication_applies(form: &crate::Form) -> bool {
    !form.dry_run
        && form.engine() == core::Engine::ImapSync
        && form.profile.source_tls != "plain"
        && form.profile.destination_tls != "plain"
}

#[cfg(test)]
mod tests {
    use super::{authenticated_list_command, read_imap_list_response};
    use std::io::Cursor;

    #[test]
    fn list_request_asks_for_rfc6154_attributes_when_supported() {
        assert_eq!(
            authenticated_list_command(true),
            b"a005 LIST \"\" \"*\" RETURN (SPECIAL-USE)\r\n"
        );
        assert_eq!(
            authenticated_list_command(false),
            b"a005 LIST \"\" \"*\"\r\n"
        );
    }

    #[test]
    fn list_response_streaming_parser_does_not_retain_a_megabyte_string() {
        let mut response = String::new();
        for index in 0..20_000 {
            response.push_str(&format!(
                "* LIST (\\HasNoChildren) \"/\" \"folder-{index:0>900}\"\r\n"
            ));
        }
        response.push_str("a005 OK LIST completed\r\n");
        assert!(response.len() > 1_048_576);
        let mut stream = Cursor::new(response.into_bytes());
        let mut buffer = [0_u8; 4096];
        let summary = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap();
        assert_eq!(summary.mailbox_count, 20_000);
        assert_eq!(summary.special_use_mailboxes, 0);
    }

    #[test]
    fn list_response_streaming_parser_skips_bounded_literals() {
        let response =
            b"* LIST (\\HasNoChildren) \"/\" {11}\r\n* LIST fake\r\na005 OK LIST completed\r\n";
        let mut stream = Cursor::new(response);
        let mut buffer = [0_u8; 4096];
        let summary = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap();
        assert_eq!(summary.mailbox_count, 1);
    }

    #[test]
    fn list_response_streaming_parser_rejects_oversized_records_and_literals() {
        let oversized_line = format!("* LIST ({})\r\na005 OK\r\n", "x".repeat(65 * 1024));
        let mut line_stream = Cursor::new(oversized_line.into_bytes());
        let mut buffer = [0_u8; 4096];
        assert!(
            read_imap_list_response(&mut line_stream, "a005", &mut buffer)
                .unwrap_err()
                .contains("record exceeded")
        );

        let mut literal_stream =
            Cursor::new(b"* LIST (\\HasNoChildren) \"/\" {1048577}\r\na005 OK\r\n");
        assert!(
            read_imap_list_response(&mut literal_stream, "a005", &mut buffer)
                .unwrap_err()
                .contains("literal exceeded")
        );
    }
}
