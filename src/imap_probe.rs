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
    collections::{HashMap, HashSet},
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    sync::atomic::AtomicBool,
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
    read_imap_tagged_with_limit(stream, tag, response, buffer, 1_048_576)
}

fn read_imap_tagged_with_limit<S: Read>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
    max_bytes: usize,
) -> Result<(), String> {
    let mut raw_response = Vec::new();
    loop {
        let count = stream.read(buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err(format!("IMAP connection closed before {tag} completed"));
        }
        raw_response.extend_from_slice(&buffer[..count]);
        if raw_response.len() > max_bytes {
            return Err(format!(
                "IMAP response for {tag} exceeded the {max_bytes}-byte safety limit"
            ));
        }
        response.clear();
        response.push_str(&String::from_utf8_lossy(&raw_response));
        if tagged_response_outside_literals(&raw_response, tag) {
            return Ok(());
        }
    }
}

fn read_imap_tagged_with_budget<S: Read>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
    max_bytes: usize,
    budget: &MessageFetchBudget<'_>,
) -> Result<(), String> {
    let mut raw_response = Vec::new();
    let mut no_progress_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        budget.check()?;
        if Instant::now() >= no_progress_deadline {
            return Err(format!("IMAP response for {tag} stalled for 15 seconds"));
        }
        match stream.read(buffer) {
            Ok(0) => return Err(format!("IMAP connection closed before {tag} completed")),
            Ok(count) => {
                no_progress_deadline = Instant::now() + Duration::from_secs(15);
                raw_response.extend_from_slice(&buffer[..count]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.to_string()),
        }
        if raw_response.len() > max_bytes {
            return Err(format!(
                "IMAP response for {tag} exceeded the {max_bytes}-byte safety limit"
            ));
        }
        response.clear();
        response.push_str(&String::from_utf8_lossy(&raw_response));
        if tagged_response_outside_literals(&raw_response, tag) {
            return Ok(());
        }
    }
}

fn read_imap_tagged_bytes_with_budget<S: Read>(
    stream: &mut S,
    tag: &str,
    buffer: &mut [u8; 4096],
    max_bytes: usize,
    budget: &MessageFetchBudget<'_>,
) -> Result<Vec<u8>, String> {
    let mut response = Vec::new();
    let mut no_progress_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        budget.check()?;
        if Instant::now() >= no_progress_deadline {
            return Err(format!("IMAP response for {tag} stalled for 15 seconds"));
        }
        match stream.read(buffer) {
            Ok(0) => return Err(format!("IMAP connection closed before {tag} completed")),
            Ok(count) => {
                no_progress_deadline = Instant::now() + Duration::from_secs(15);
                response.extend_from_slice(&buffer[..count]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.to_string()),
        }
        if response.len() > max_bytes {
            return Err(format!(
                "IMAP response for {tag} exceeded the {max_bytes}-byte safety limit"
            ));
        }
        if tagged_response_outside_literals(&response, tag) {
            return Ok(response);
        }
    }
}

/// Locate a tagged completion line without interpreting bytes inside IMAP
/// literals as protocol framing. Literal payloads are arbitrary octets and
/// may contain lines that look exactly like the command tag.
fn tagged_response_outside_literals(response: &[u8], tag: &str) -> bool {
    let tag = tag.as_bytes();
    let mut offset = 0;
    while offset < response.len() {
        let Some(relative_end) = response[offset..]
            .windows(2)
            .position(|pair| pair == b"\r\n")
        else {
            return false;
        };
        let line_end = offset + relative_end;
        let line = &response[offset..line_end];
        if line.starts_with(tag)
            && line
                .get(tag.len())
                .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            return true;
        }
        let literal_length = line
            .strip_suffix(b"}")
            .and_then(|line| line.iter().rposition(|byte| *byte == b'{'))
            .and_then(|start| std::str::from_utf8(&line[start + 1..line.len() - 1]).ok())
            .and_then(|length| {
                length
                    .strip_suffix('+')
                    .unwrap_or(length)
                    .parse::<usize>()
                    .ok()
            });
        offset = line_end + 2;
        if let Some(length) = literal_length {
            if response.len().saturating_sub(offset) < length {
                return false;
            }
            offset += length;
        }
    }
    false
}

const MAX_IMAP_LIST_LINE_BYTES: usize = 64 * 1024;
const MAX_IMAP_LIST_LITERAL_BYTES: usize = 1024 * 1024;
const MAX_IMAP_LIST_MAILBOXES: usize = 100_000;
const MAX_IMAP_LIST_DURATION: Duration = Duration::from_secs(60);
const MAX_MESSAGE_FETCH_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MESSAGE_FETCH_PAGE_SIZE: u64 = 32;
const MAX_MESSAGE_FETCH_RECORDS: usize = 1_000_000;
// The current verifier intentionally remains an in-memory implementation. This
// estimate covers fetched records, not the full peak of both account maps and
// every reconciliation index. It is therefore a fail-closed admission guard,
// not a process-wide memory guarantee. SQLite-backed streaming reconciliation
// is required before very large MSP migrations can be production-supported.
const MAX_ESTIMATED_MESSAGE_STATE_BYTES: usize = 256 * 1024 * 1024;

pub(crate) struct MessageFetchBudget<'a> {
    deadline: Instant,
    cancel: &'a AtomicBool,
}

pub(crate) struct FetchedAccountMessages {
    pub(crate) mailboxes: HashSet<String>,
    pub(crate) messages: crate::core::ExtractedMessages,
    pub(crate) content_fingerprints: HashMap<crate::core::MailboxMessageKey, String>,
}

impl<'a> MessageFetchBudget<'a> {
    pub(crate) fn new(timeout: Duration, cancel: &'a AtomicBool) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            cancel,
        }
    }

    fn check(&self) -> Result<(), String> {
        if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("message-level verification cancelled by operator".into());
        }
        if Instant::now() >= self.deadline {
            return Err("message-level verification exceeded its execution deadline".into());
        }
        Ok(())
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ListInventorySummary {
    mailbox_count: usize,
    special_use_mailboxes: usize,
    selectable_mailbox_count: usize,
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
    read_imap_list_response_inner(stream, tag, buffer, None)
}

fn read_imap_list_response_with_mailboxes<S: Read>(
    stream: &mut S,
    tag: &str,
    buffer: &mut [u8; 4096],
    mailboxes: &mut Vec<String>,
) -> Result<ListInventorySummary, String> {
    read_imap_list_response_inner(stream, tag, buffer, Some(mailboxes))
}

fn read_imap_list_response_inner<S: Read>(
    stream: &mut S,
    tag: &str,
    buffer: &mut [u8; 4096],
    mut mailboxes: Option<&mut Vec<String>>,
) -> Result<ListInventorySummary, String> {
    let mut line = Vec::new();
    let started = Instant::now();
    let mut last_read = Instant::now();
    let mut literal_remaining = 0_usize;
    let mut literal_separator_remaining = 0_u8;
    let mut summary = ListInventorySummary::default();
    const MAX_INTER_READ_STALL: Duration = Duration::from_secs(15);
    let deadline = started + MAX_IMAP_LIST_DURATION;
    loop {
        if last_read.elapsed() > MAX_INTER_READ_STALL {
            return Err("IMAP LIST response stalled (no data received for 15 seconds)".into());
        }
        let read_deadline = (last_read + MAX_INTER_READ_STALL).min(deadline);
        let count = read_with_deadline(stream, buffer, read_deadline)?;
        last_read = Instant::now();
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
                if !crate::imap_protocol::list_has_attribute(text, r"\NOSELECT")
                    && let Some(mailboxes) = mailboxes.as_deref_mut()
                    && let Some(mailbox) = parse_list_mailbox_name(text)
                {
                    mailboxes.push(mailbox);
                    summary.selectable_mailbox_count =
                        summary.selectable_mailbox_count.saturating_add(1);
                }
            }
            if is_tagged_response(text, tag) {
                let status = text.split_whitespace().nth(1);
                if !status.is_some_and(|status| atom_eq(status, "OK")) {
                    let detail = text
                        .chars()
                        .map(|character| {
                            if character.is_control() {
                                ' '
                            } else {
                                character
                            }
                        })
                        .take(512)
                        .collect::<String>();
                    return Err(format!("IMAP LIST command {tag} failed: {detail}"));
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

/// Parse the final mailbox-name atom from a normal, non-literal LIST record.
/// Literal mailbox names are intentionally not guessed: the caller receives
/// the inventory count but message verification fails closed if a provider
/// requires literal decoding that this bounded parser cannot retain.
fn parse_list_mailbox_name(line: &str) -> Option<String> {
    let mut tokens = Vec::new();
    let mut chars = line.split_whitespace().peekable();
    while let Some(token) = chars.next() {
        if token.starts_with('"') {
            let mut value = token.to_owned();
            while !value.ends_with('"') {
                value.push(' ');
                value.push_str(chars.next()?);
            }
            let value = value
                .strip_prefix('"')?
                .strip_suffix('"')?
                .replace("\\\\", "\\")
                .replace("\\\"", "\"");
            tokens.push(value);
        } else {
            tokens.push(token.to_owned());
        }
    }
    tokens
        .last()
        .filter(|value| !value.starts_with('{'))
        .cloned()
}

/// A socket read timeout is only a progress hint; it is not the same as the
/// LIST operation's total deadline. TLS/read adapters can return `TimedOut`
/// while waiting for the rest of a record, so retry transient reads until the
/// bounded LIST deadline expires instead of allowing discovery to stall.
fn read_with_deadline<S: Read>(
    stream: &mut S,
    buffer: &mut [u8],
    deadline: Instant,
) -> Result<usize, String> {
    loop {
        if Instant::now() >= deadline {
            return Err("IMAP LIST response stalled (no data received for 15 seconds)".into());
        }
        match stream.read(buffer) {
            Ok(count) => return Ok(count),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.to_string()),
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

fn imap_command_failure(response: &str, tag: &str, operation: &str, host: &str) -> String {
    let detail = response
        .lines()
        .find(|line| is_tagged_response(line, tag))
        .map(|line| {
            line.chars()
                .map(|character| {
                    if character.is_control() {
                        ' '
                    } else {
                        character
                    }
                })
                .take(512)
                .collect::<String>()
        })
        .filter(|line| !line.is_empty());
    match detail {
        Some(detail) => format!("{host}: {operation} failed: {detail}"),
        None => format!("{host}: {operation} failed"),
    }
}

/// Open and TLS-secure a fresh IMAP connection (implicit IMAPS or STARTTLS,
/// per `transport`) and read the server greeting. This is the connection
/// setup shared by the readiness probe and any other caller that needs a
/// live, certificate-validated IMAP session (for example, independent
/// message-level reconciliation): both need the identical TLS/STARTTLS/
/// certificate-pin handling, and must not diverge into two implementations
/// of trust-critical connection logic.
pub(crate) fn connect_tls_stream(
    host: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
) -> Result<(StreamOwned<ClientConnection, TcpStream>, String), String> {
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
        stream
            .sock
            .set_read_timeout(Some(Duration::from_secs(8)))
            .map_err(|e| format!("{host}: could not set TLS read timeout: {e}"))?;
        verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
        return Ok((stream, greeting));
    }

    let connection = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| format!("{host}: TLS configuration failed: {e}"))?;
    let mut stream = StreamOwned::new(connection, tcp);
    stream
        .sock
        .set_read_timeout(Some(Duration::from_secs(8)))
        .map_err(|e| format!("{host}: could not set TLS read timeout: {e}"))?;
    let greeting = read_imap_greeting(&mut stream, host)?;
    verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
    Ok((stream, greeting))
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
    let (stream, greeting) =
        connect_tls_stream(host, transport, ca_bundle, certificate_pin_sha256)?;
    complete_authenticated_imap_probe(stream, host, user, credential, auth_method, greeting)
}

/// Authenticate an already TLS-protected connection and perform only the
/// post-authentication readiness checks needed immediately before a live
/// engine launch. Folder discovery, namespace analysis, and quota collection
/// belong to the comprehensive preflight probe below, not this hot path.
pub(crate) fn probe_tls_authentication_with_transport(
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
) -> Result<(), String> {
    let (stream, greeting) =
        connect_tls_stream(host, transport, ca_bundle, certificate_pin_sha256)?;
    let (mut stream, _) =
        authenticate_imap_stream(stream, host, user, credential, auth_method, greeting)?;
    let mut response = String::new();
    let mut buffer = [0; 4096];
    stream
        .write_all(b"a004 NOOP\r\n")
        .map_err(|e| e.to_string())?;
    read_imap_tagged(&mut stream, "a004", &mut response, &mut buffer)?;
    if !imap_command_succeeded(&response, "a004") {
        return Err(imap_command_failure(
            &response,
            "a004",
            "post-auth NOOP",
            host,
        ));
    }
    let _ = stream.write_all(b"a005 LOGOUT\r\n");
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
    let actual = format!("{:x}", Sha256::digest(certificate.as_ref()));
    if actual != expected.trim().to_ascii_lowercase() {
        return Err(format!("{host}: TLS certificate SHA-256 pin mismatch"));
    }
    Ok(())
}

fn authenticate_imap_stream<S: Read + Write>(
    mut stream: S,
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    greeting: String,
) -> Result<(S, String), String> {
    let mut response = String::new();
    let mut buffer = [0; 4096];
    stream
        .write_all(b"a001 CAPABILITY\r\n")
        .map_err(|e| e.to_string())?;
    read_imap_tagged(&mut stream, "a001", &mut response, &mut buffer)?;
    if !imap_command_succeeded(&response, "a001") {
        return Err(imap_command_failure(
            &response,
            "a001",
            "pre-auth CAPABILITY",
            host,
        ));
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
            return Err(imap_command_failure(
                &response,
                "a002",
                "IMAP authentication",
                host,
            ));
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
        return Err(imap_command_failure(
            &post_auth_response,
            "a003",
            "post-auth CAPABILITY",
            host,
        ));
    }
    Ok((stream, post_auth_response))
}

fn complete_authenticated_imap_probe<S: Read + Write>(
    stream: S,
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    greeting: String,
) -> Result<crate::core::ServerCapabilities, String> {
    let (mut stream, post_auth_response) =
        authenticate_imap_stream(stream, host, user, credential, auth_method, greeting)?;
    let mut buffer = [0; 4096];
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
    probe_tls_authentication_with_transport(
        &source,
        &form.profile.source_user,
        form.source_password.as_str(),
        &form.profile.source_auth,
        &form.profile.source_tls,
        &form.profile.source_ca_bundle,
        &form.profile.source_certificate_pin_sha256,
    )?;
    probe_tls_authentication_with_transport(
        &destination,
        &form.profile.destination_user,
        form.destination_password.as_str(),
        &form.profile.destination_auth,
        &form.profile.destination_tls,
        &form.profile.destination_ca_bundle,
        &form.profile.destination_certificate_pin_sha256,
    )?;
    Ok(())
}

/// Fetch bounded message metadata for one mailbox over an authenticated TLS
/// IMAP session. This is deliberately independent of the transfer engine:
/// imapsync output contains local UIDs and counters, not a portable message
/// identity. A fresh source and destination read therefore provides the
/// evidence used by the message reconciler instead of trusting engine prose.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fetch_tls_mailbox_messages(
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
    budget: &MessageFetchBudget<'_>,
    mailbox: &str,
) -> Result<
    (
        crate::core::ExtractedMessages,
        HashMap<crate::core::MailboxMessageKey, String>,
    ),
    String,
> {
    budget.check()?;
    if transport == "plain" {
        return Err(
            "message-level verification requires TLS; refusing to inspect a plain IMAP session"
                .into(),
        );
    }
    if mailbox.trim().is_empty() {
        return Err("message-level verification requires a non-empty mailbox".into());
    }
    let (stream, greeting) =
        connect_tls_stream(host, transport, ca_bundle, certificate_pin_sha256)?;
    let (mut stream, _) =
        authenticate_imap_stream(stream, host, user, credential, auth_method, greeting)?;
    let quoted_mailbox = imap_quote(mailbox)?;
    let select = format!("v001 SELECT {quoted_mailbox}\r\n");
    stream
        .write_all(select.as_bytes())
        .map_err(|error| format!("{host}: could not select mailbox: {error}"))?;
    let mut response = String::new();
    let mut buffer = [0; 4096];
    read_imap_tagged_with_budget(
        &mut stream,
        "v001",
        &mut response,
        &mut buffer,
        1_048_576,
        budget,
    )?;
    if !imap_command_succeeded(&response, "v001") {
        return Err(imap_command_failure(
            &response,
            "v001",
            "SELECT mailbox for message verification",
            host,
        ));
    }
    let (_exists, uidvalidity) = parse_selected_mailbox(&response, host, mailbox)?;
    // EXISTS is a message count, not a UID upper bound.  UIDs can start at
    // 100 (or contain arbitrary gaps after expunges), so UID FETCH 1:EXISTS
    // can silently fetch nothing.  Enumerate the actual UID set first.
    let search_tag = "v002";
    stream
        .write_all(format!("{search_tag} UID SEARCH ALL\r\n").as_bytes())
        .map_err(|error| format!("{host}: could not search mailbox UIDs: {error}"))?;
    response.clear();
    read_imap_tagged_with_budget(
        &mut stream,
        search_tag,
        &mut response,
        &mut buffer,
        1_048_576,
        budget,
    )?;
    if !imap_command_succeeded(&response, search_tag) {
        return Err(imap_command_failure(
            &response,
            search_tag,
            "SEARCH mailbox UIDs",
            host,
        ));
    }
    let uids = parse_uid_search_response(&response, host, mailbox)?;
    let mut messages = HashMap::new();
    let mut content_fingerprints = HashMap::new();
    let mut estimated_state_bytes = 0usize;
    for (page, uid_page) in uids.chunks(MESSAGE_FETCH_PAGE_SIZE as usize).enumerate() {
        budget.check()?;
        let tag = format!("v{:03}", page + 3);
        let uid_set = uid_page
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let command = format!(
            "{tag} UID FETCH {uid_set} (UID RFC822.SIZE INTERNALDATE BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])\r\n"
        );
        stream
            .write_all(command.as_bytes())
            .map_err(|error| format!("{host}: could not fetch mailbox metadata: {error}"))?;
        let raw_response = read_imap_tagged_bytes_with_budget(
            &mut stream,
            &tag,
            &mut buffer,
            MAX_MESSAGE_FETCH_RESPONSE_BYTES,
            budget,
        )?;
        let response = String::from_utf8_lossy(&raw_response);
        if !imap_command_succeeded(&response, &tag) {
            return Err(imap_command_failure(
                &response,
                &tag,
                "FETCH mailbox metadata",
                host,
            ));
        }
        let (page_messages, page_fingerprints) =
            parse_message_fetch_response_bytes(&raw_response, mailbox, uidvalidity)?;
        for (key, message) in page_messages {
            estimated_state_bytes = estimated_state_bytes
                .saturating_add(estimated_message_record_bytes(&key, &message, None));
            if messages.insert(key, message).is_some() {
                return Err(format!(
                    "{host}: mailbox {mailbox} returned a duplicate UID during message verification"
                ));
            }
            if messages.len() > MAX_MESSAGE_FETCH_RECORDS {
                return Err(format!(
                    "{host}: mailbox {mailbox} exceeded the {MAX_MESSAGE_FETCH_RECORDS}-message verification limit"
                ));
            }
            if estimated_state_bytes > MAX_ESTIMATED_MESSAGE_STATE_BYTES {
                return Err(format!(
                    "{host}: mailbox {mailbox} exceeded the estimated {MAX_ESTIMATED_MESSAGE_STATE_BYTES}-byte verification memory budget"
                ));
            }
        }
        for (key, fingerprint) in page_fingerprints {
            if messages.contains_key(&key) {
                // The key was inserted above; fingerprints are optional only
                // for servers that return NIL BODY[] and otherwise must be
                // one-to-one with the fetched message records.
                content_fingerprints.insert(key, fingerprint);
            }
        }
    }
    if messages.len() != uids.len() {
        return Err(format!(
            "{host}: mailbox {mailbox} changed or returned an incomplete FETCH response (searched {} UIDs, fetched {})",
            uids.len(),
            messages.len()
        ));
    }
    let _ = stream.write_all(b"v999 LOGOUT\r\n");
    Ok((messages, content_fingerprints))
}

fn parse_uid_search_response(
    response: &str,
    host: &str,
    mailbox: &str,
) -> Result<Vec<u64>, String> {
    let line = response
        .lines()
        .find(|line| line.starts_with("* SEARCH"))
        .ok_or_else(|| format!("{host}: SEARCH {mailbox} did not return a UID list"))?;
    let mut uids = line
        .split_whitespace()
        .skip(2)
        .map(|uid| {
            uid.parse::<u64>()
                .map_err(|_| format!("{host}: SEARCH {mailbox} returned invalid UID {uid}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    uids.sort_unstable();
    uids.dedup();
    Ok(uids)
}

/// Reconcile an entire IMAP account by enumerating selectable folders first.
/// The account-level operation deliberately opens a fresh authenticated
/// session per folder so one provider's SELECT/FETCH failure cannot leave a
/// partially trusted connection state affecting the next folder. The folder
/// count and every message record remain bounded by the LIST/FETCH limits.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fetch_tls_account_messages(
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
    budget: &MessageFetchBudget<'_>,
) -> Result<FetchedAccountMessages, String> {
    budget.check()?;
    if transport == "plain" {
        return Err(
            "message-level verification requires TLS; refusing to inspect a plain IMAP session"
                .into(),
        );
    }
    let (stream, greeting) =
        connect_tls_stream(host, transport, ca_bundle, certificate_pin_sha256)?;
    let (mut stream, post_auth_response) =
        authenticate_imap_stream(stream, host, user, credential, auth_method, greeting)?;
    stream
        .write_all(authenticated_list_command(advertises_capability(
            &post_auth_response,
            "SPECIAL-USE",
        )))
        .map_err(|error| format!("{host}: could not enumerate folders: {error}"))?;
    let mut buffer = [0; 4096];
    let mut mailboxes = Vec::new();
    let summary =
        read_imap_list_response_with_mailboxes(&mut stream, "a005", &mut buffer, &mut mailboxes)?;
    let _ = stream.write_all(b"a999 LOGOUT\r\n");
    if summary.mailbox_count == 0 || mailboxes.is_empty() {
        return Err(format!(
            "{host}: folder inventory did not expose selectable mailbox names"
        ));
    }
    if summary.selectable_mailbox_count != mailboxes.len() {
        return Err(format!(
            "{host}: folder inventory included literal or otherwise unparseable mailbox names"
        ));
    }
    mailboxes.sort();
    mailboxes.dedup();
    let mailbox_inventory = mailboxes.iter().cloned().collect::<HashSet<_>>();
    let mut all_messages = HashMap::new();
    let mut all_fingerprints = HashMap::new();
    let mut estimated_state_bytes = 0usize;
    for mailbox in mailboxes {
        budget.check()?;
        let (folder_messages, folder_fingerprints) = fetch_tls_mailbox_messages(
            host,
            user,
            credential,
            auth_method,
            transport,
            ca_bundle,
            certificate_pin_sha256,
            budget,
            &mailbox,
        )?;
        for (key, message) in folder_messages {
            estimated_state_bytes = estimated_state_bytes.saturating_add(
                estimated_message_record_bytes(&key, &message, all_fingerprints.get(&key)),
            );
            if all_messages.insert(key, message).is_some() {
                return Err(format!(
                    "{host}: folder inventory produced duplicate message identity"
                ));
            }
            if all_messages.len() > MAX_MESSAGE_FETCH_RECORDS {
                return Err(format!(
                    "{host}: account exceeded the {MAX_MESSAGE_FETCH_RECORDS}-message verification limit"
                ));
            }
            if estimated_state_bytes > MAX_ESTIMATED_MESSAGE_STATE_BYTES {
                return Err(format!(
                    "{host}: account exceeded the estimated {MAX_ESTIMATED_MESSAGE_STATE_BYTES}-byte verification memory budget"
                ));
            }
        }
        all_fingerprints.extend(folder_fingerprints);
    }
    Ok(FetchedAccountMessages {
        mailboxes: mailbox_inventory,
        messages: all_messages,
        content_fingerprints: all_fingerprints,
    })
}

fn estimated_message_record_bytes(
    key: &crate::core::MailboxMessageKey,
    message: &crate::core::ExtractedMessage,
    fingerprint: Option<&String>,
) -> usize {
    512usize
        .saturating_add(key.mailbox.len())
        .saturating_add(key.uid.len())
        .saturating_add(message.message_id.as_deref().map_or(0, str::len))
        .saturating_add(message.internal_date.as_deref().map_or(0, str::len))
        .saturating_add(fingerprint.map_or(0, String::len))
}

fn parse_selected_mailbox(
    response: &str,
    host: &str,
    mailbox: &str,
) -> Result<(u64, Option<u64>), String> {
    let mut exists = None;
    let mut uidvalidity = None;
    for line in response.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() >= 3 && fields[0] == "*" && fields[2].eq_ignore_ascii_case("EXISTS") {
            exists = fields[1].parse::<u64>().ok();
        }
        if let Some(index) = fields.iter().position(|field| {
            field
                .trim_matches(['[', ']'])
                .eq_ignore_ascii_case("UIDVALIDITY")
        }) && let Some(value) = fields.get(index + 1)
        {
            uidvalidity = value.trim_matches(['[', ']']).parse::<u64>().ok();
        }
    }
    let exists = exists.ok_or_else(|| {
        format!("{host}: SELECT {mailbox} did not return an untagged EXISTS count")
    })?;
    Ok((exists, uidvalidity))
}

#[allow(dead_code)]
fn parse_message_fetch_response(
    response: &str,
    mailbox: &str,
    uidvalidity: Option<u64>,
) -> Result<crate::core::ExtractedMessages, String> {
    Ok(parse_message_fetch_response_with_fingerprints(response, mailbox, uidvalidity)?.0)
}

fn parse_message_fetch_response_with_fingerprints(
    response: &str,
    mailbox: &str,
    uidvalidity: Option<u64>,
) -> Result<
    (
        crate::core::ExtractedMessages,
        HashMap<crate::core::MailboxMessageKey, String>,
    ),
    String,
> {
    let starts = fetch_record_starts(response);
    let mut messages = HashMap::new();
    let mut content_fingerprints = HashMap::new();
    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(response.len());
        let record = &response[start..end];
        let first_line_end = record
            .find("\r\n")
            .ok_or_else(|| "IMAP FETCH record had no line terminator".to_owned())?;
        let first_line = &record[..first_line_end];
        let uid = fetch_number(first_line, "UID")
            .ok_or_else(|| "IMAP FETCH record omitted UID".to_owned())?;
        let size_bytes = fetch_number(first_line, "RFC822.SIZE");
        let internal_date = fetch_quoted(first_line, "INTERNALDATE");
        let message_id = fetch_message_id(record).filter(|value| !value.is_empty());
        let uid = uid.to_string();
        let key = match uidvalidity {
            Some(value) => {
                crate::core::MailboxMessageKey::with_uidvalidity(mailbox, value, uid.clone())
            }
            None => crate::core::MailboxMessageKey::new(mailbox, uid.clone()),
        };
        messages.insert(
            key.clone(),
            crate::core::ExtractedMessage {
                message_id,
                uid: Some(uid),
                size_bytes,
                internal_date,
            },
        );
        if let Some(fingerprint) = fetch_content_fingerprint(record) {
            content_fingerprints.insert(key, fingerprint);
        }
    }
    Ok((messages, content_fingerprints))
}

fn parse_message_fetch_response_bytes(
    response: &[u8],
    mailbox: &str,
    uidvalidity: Option<u64>,
) -> Result<
    (
        crate::core::ExtractedMessages,
        HashMap<crate::core::MailboxMessageKey, String>,
    ),
    String,
> {
    let starts = fetch_record_starts_bytes(response);
    let mut messages = HashMap::new();
    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(response.len());
        let record = &response[start..end];
        let first_line_end = record
            .windows(2)
            .position(|pair| pair == b"\r\n")
            .ok_or_else(|| "IMAP FETCH record had no line terminator".to_owned())?;
        let first_line = String::from_utf8_lossy(&record[..first_line_end]);
        let uid = fetch_number(&first_line, "UID")
            .ok_or_else(|| "IMAP FETCH record omitted UID".to_owned())?
            .to_string();
        let key = match uidvalidity {
            Some(value) => {
                crate::core::MailboxMessageKey::with_uidvalidity(mailbox, value, uid.clone())
            }
            None => crate::core::MailboxMessageKey::new(mailbox, uid.clone()),
        };
        messages.insert(
            key,
            crate::core::ExtractedMessage {
                message_id: fetch_message_id_bytes(record).filter(|value| !value.is_empty()),
                uid: Some(uid),
                size_bytes: fetch_number(&first_line, "RFC822.SIZE"),
                internal_date: fetch_quoted(&first_line, "INTERNALDATE"),
            },
        );
    }
    Ok((messages, HashMap::new()))
}

/// Locate untagged FETCH record boundaries while skipping every IMAP literal.
/// A raw message body is arbitrary octets and can itself contain lines that
/// look like `* n FETCH`; treating those bytes as protocol framing would
/// silently hash or report the wrong message.
fn fetch_record_starts(response: &str) -> Vec<usize> {
    let bytes = response.as_bytes();
    let mut starts = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let Some(line_end) = response[offset..].find("\r\n") else {
            break;
        };
        let absolute_end = offset + line_end;
        let line = &response[offset..absolute_end];
        if (offset == 0 || bytes[offset - 1] == b'\n')
            && line.starts_with("* ")
            && line.contains(" FETCH (")
        {
            starts.push(offset);
        }
        let next_line = absolute_end + 2;
        if let Some(literal_size) = imap_literal_size(line) {
            offset = next_line.saturating_add(literal_size);
        } else {
            offset = next_line;
        }
    }
    starts
}

fn fetch_record_starts_bytes(response: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut offset = 0;
    while offset < response.len() {
        let Some(relative_end) = response[offset..]
            .windows(2)
            .position(|pair| pair == b"\r\n")
        else {
            break;
        };
        let line_end = offset + relative_end;
        let line = &response[offset..line_end];
        if line.starts_with(b"* ") && line.windows(7).any(|part| part == b" FETCH ") {
            starts.push(offset);
        }
        offset = line_end + 2;
        if let Some(literal_size) = imap_literal_size_bytes(line) {
            offset = offset.saturating_add(literal_size);
        }
    }
    starts
}

fn imap_literal_size(line: &str) -> Option<usize> {
    let close = line.strip_suffix('}')?;
    let open = close.rfind('{')?;
    close[open + 1..].parse::<usize>().ok()
}

fn imap_literal_size_bytes(line: &[u8]) -> Option<usize> {
    let line = line.strip_suffix(b"}")?;
    let start = line.iter().rposition(|byte| *byte == b'{')?;
    std::str::from_utf8(&line[start + 1..]).ok()?.parse().ok()
}

fn fetch_content_fingerprint(record: &str) -> Option<String> {
    let marker_start = record.find("BODY[]")? + "BODY[]".len();
    let literal = record[marker_start..].trim_start();
    if literal.starts_with("NIL") {
        return None;
    }
    let open = literal.find('{')?;
    let close = literal[open..].find("}\r\n")? + open;
    let size = literal[open + 1..close].parse::<usize>().ok()?;
    let body_offset = marker_start + record[marker_start..].find("\r\n")? + 2;
    let body = record
        .as_bytes()
        .get(body_offset..body_offset.checked_add(size)?)?;
    Some(format!("{:x}", Sha256::digest(body)))
}

fn fetch_number(line: &str, field: &str) -> Option<u64> {
    let start = line.find(field)? + field.len();
    let value = line[start..].trim_start();
    let end = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    (end > 0).then(|| value[..end].parse().ok()).flatten()
}

fn fetch_quoted(line: &str, field: &str) -> Option<String> {
    let start = line.find(field)? + field.len();
    let value = line[start..].trim_start().strip_prefix('"')?;
    let end = value.find('"')?;
    Some(value[..end].to_owned())
}

fn fetch_message_id(record: &str) -> Option<String> {
    let marker = "BODY[HEADER.FIELDS (MESSAGE-ID)]";
    let marker_start = record.find(marker)? + marker.len();
    let literal = record[marker_start..].trim_start();
    if literal.starts_with("NIL") {
        return None;
    }
    let literal_size_end = literal.find("}\r\n")?;
    let literal_size = literal.strip_prefix('{')?[..literal_size_end - 1]
        .parse::<usize>()
        .ok()?;
    let body_start = marker_start + record[marker_start..].find("\r\n")? + 2;
    let body = record
        .as_bytes()
        .get(body_start..body_start + literal_size)?;
    let body = String::from_utf8_lossy(body);
    body.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("message-id").then(|| {
            value
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_owned()
        })
    })
}

fn fetch_message_id_bytes(record: &[u8]) -> Option<String> {
    let marker = b"BODY[HEADER.FIELDS (MESSAGE-ID)]";
    let marker_start = record
        .windows(marker.len())
        .position(|part| part == marker)?
        + marker.len();
    let literal = record[marker_start..].strip_prefix(b" ")?;
    let literal_end = literal.windows(3).position(|part| part == b"}\r\n")?;
    let size = std::str::from_utf8(&literal[1..literal_end])
        .ok()?
        .parse::<usize>()
        .ok()?;
    let body_start = marker_start
        + record[marker_start..]
            .windows(2)
            .position(|part| part == b"\r\n")?
        + 2;
    let body = record.get(body_start..body_start + size)?;
    String::from_utf8_lossy(body).lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("message-id").then(|| {
            value
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_owned()
        })
    })
}

pub(crate) fn fresh_imap_authentication_applies(form: &crate::Form) -> bool {
    !form.dry_run
        && form.engine() == core::Engine::ImapSync
        && form.profile.source_tls != "plain"
        && form.profile.destination_tls != "plain"
}

#[cfg(test)]
mod tests {
    use super::{
        MessageFetchBudget, authenticated_list_command, parse_list_mailbox_name,
        parse_message_fetch_response, parse_message_fetch_response_with_fingerprints,
        read_imap_list_response, read_imap_list_response_with_mailboxes,
        tagged_response_outside_literals,
    };
    use std::io::{self, Cursor, Read};
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    struct TimeoutThenData {
        timed_out: bool,
        data: Cursor<Vec<u8>>,
    }

    impl Read for TimeoutThenData {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.timed_out {
                self.timed_out = true;
                return Err(io::Error::new(io::ErrorKind::TimedOut, "synthetic timeout"));
            }
            self.data.read(buffer)
        }
    }

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
    fn tagged_reader_ignores_command_tag_inside_literal_payload() {
        let payload = "message text\r\nv002 OK this is body text\r\nand it is still literal payload bytes\r\n";
        let response = format!(
            "* 1 FETCH (UID 100 BODY[] {{{}}}\r\n{} )\r\nv002 OK FETCH completed\r\n",
            payload.len(),
            payload
        );
        assert!(tagged_response_outside_literals(
            response.as_bytes(),
            "v002"
        ));

        let without_completion = response
            .strip_suffix("v002 OK FETCH completed\r\n")
            .unwrap();
        assert!(!tagged_response_outside_literals(
            without_completion.as_bytes(),
            "v002"
        ));
    }

    #[test]
    fn list_failure_preserves_bounded_provider_status_for_classification() {
        let mut stream = Cursor::new(b"a005 NO [UNAVAILABLE] Server busy\r\n");
        let mut buffer = [0_u8; 4096];

        let error = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap_err();

        assert!(error.contains("[UNAVAILABLE] Server busy"));
        assert!(error.len() < 600);
    }

    #[test]
    fn list_discovery_retries_transient_read_timeout_without_losing_the_deadline() {
        let mut stream = TimeoutThenData {
            timed_out: false,
            data: Cursor::new(
                b"* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\na005 OK LIST completed\r\n".to_vec(),
            ),
        };
        let mut buffer = [0_u8; 4096];
        let summary = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap();
        assert_eq!(summary.mailbox_count, 1);
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

    #[test]
    fn list_mailbox_parser_preserves_quoted_spaces_and_escaped_names() {
        assert_eq!(
            parse_list_mailbox_name(r#"* LIST (\HasNoChildren) "/" "Sent Items""#),
            Some("Sent Items".into())
        );
        assert_eq!(
            parse_list_mailbox_name(r#"* LIST (\HasNoChildren) "/" "a\\b\"c""#),
            Some("a\\b\"c".into())
        );
    }

    #[test]
    fn list_parser_collects_selectable_mailboxes_without_retaining_default_inventory() {
        let response = b"* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n\
                       * LIST (\\Noselect) \"/\" \"Archive\"\r\n\
                       a005 OK LIST completed\r\n";
        let mut stream = Cursor::new(response);
        let mut buffer = [0_u8; 4096];
        let mut mailboxes = Vec::new();
        let summary = read_imap_list_response_with_mailboxes(
            &mut stream,
            "a005",
            &mut buffer,
            &mut mailboxes,
        )
        .unwrap();
        assert_eq!(summary.mailbox_count, 2);
        assert_eq!(summary.selectable_mailbox_count, 1);
        assert_eq!(mailboxes, ["INBOX"]);
    }

    #[test]
    fn fetch_parser_extracts_message_metadata_and_uidvalidity() {
        let response = "* 1 FETCH (UID 5 RFC822.SIZE 100 INTERNALDATE \"01-Jan-2024 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] {31}\r\nMessage-ID: <a@example.com>\r\n\r\n)\r\n\
                       * 2 FETCH (UID 9 RFC822.SIZE 200 INTERNALDATE \"02-Jan-2024 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] NIL)\r\n\
                       v002 OK FETCH completed\r\n";
        let messages = parse_message_fetch_response(response, "INBOX", Some(77)).unwrap();
        assert_eq!(messages.len(), 2);
        let first = &messages[&crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "5")];
        assert_eq!(first.message_id.as_deref(), Some("<a@example.com>"));
        assert_eq!(first.size_bytes, Some(100));
        assert_eq!(
            first.internal_date.as_deref(),
            Some("01-Jan-2024 00:00:00 +0000")
        );
        let second = &messages[&crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "9")];
        assert_eq!(second.message_id, None);
    }

    #[test]
    fn uid_search_parser_uses_actual_sparse_uids_not_exists_count() {
        let response = "* SEARCH 100 104 109\r\nv002 OK SEARCH completed\r\n";
        assert_eq!(
            super::parse_uid_search_response(response, "imap.example", "INBOX").unwrap(),
            vec![100, 104, 109]
        );
    }

    #[test]
    fn uid_search_parser_rejects_missing_or_invalid_uid_lists() {
        assert!(
            super::parse_uid_search_response(
                "v002 OK SEARCH completed\r\n",
                "imap.example",
                "INBOX"
            )
            .is_err()
        );
        assert!(
            super::parse_uid_search_response("* SEARCH 100 nope\r\n", "imap.example", "INBOX")
                .is_err()
        );
    }

    #[test]
    fn fetch_parser_hashes_bounded_full_message_literals() {
        let response = concat!(
            "* 1 FETCH (UID 9 RFC822.SIZE 5 INTERNALDATE \"01-Jan-2026 00:00:00 +0000\" ",
            "BODY[HEADER.FIELDS (MESSAGE-ID)] {21}\r\n",
            "Message-ID: <a@b>\r\n\r\n",
            "BODY[] {5}\r\n",
            "abcde\r\n)\r\n",
            "v002 OK FETCH completed\r\n"
        );
        let (messages, fingerprints) =
            parse_message_fetch_response_with_fingerprints(response, "INBOX", Some(77)).unwrap();
        let key = crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "9");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            fingerprints[&key],
            "36bbe50ed96841d10443bcb670d6554f0a34b761be67ec9c4a8ad2c0c44ca42c"
        );
    }

    #[test]
    fn raw_fetch_parser_preserves_framing_around_invalid_literal_bytes() {
        let response = b"* 1 FETCH (UID 100 RFC822.SIZE 17 INTERNALDATE \"01-Jan-2026 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] {31}\r\nMessage-ID: <raw@example.com>\r\nBODY[] {17}\r\n\xff\x00v002 OK fake\r\n)\r\nv002 OK FETCH completed\r\n";
        let (messages, fingerprints) =
            super::parse_message_fetch_response_bytes(response, "INBOX", Some(77)).unwrap();
        let key = crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "100");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[&key].message_id.as_deref(),
            Some("<raw@example.com>")
        );
        assert!(fingerprints.is_empty());
    }

    #[test]
    fn message_fetch_budget_honors_operator_cancellation() {
        let cancelled = AtomicBool::new(true);
        let budget = MessageFetchBudget::new(Duration::from_secs(60), &cancelled);
        assert!(budget.check().unwrap_err().contains("cancelled"));
    }
}
