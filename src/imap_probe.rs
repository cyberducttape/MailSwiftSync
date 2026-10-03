use crate::core;
use crate::credentials::SecretString;
use crate::imap_protocol::{advertises_capability, atom_eq, is_untagged_response};
use crate::oauth::{read_auth_continuation_with_deadline, read_auth_result_with_deadline};
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    net::TcpStream,
    sync::Arc,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

#[path = "imap_probe/fetch_parser.rs"]
mod fetch_parser;
mod folders;
mod list_parser;
mod literal_framing;
#[path = "imap_probe/namespace.rs"]
mod namespace_parser;
mod resolver;
use fetch_parser::parse_message_fetch_body_hashes_response_bytes;
use fetch_parser::parse_message_fetch_metadata_response_bytes_with_mailbox;
#[cfg(test)]
use fetch_parser::{parse_message_fetch_metadata_response_bytes, parse_message_id_header};
pub(crate) use folders::MailboxDescriptor;
#[cfg(test)]
use folders::{ListInventorySummary, read_imap_list_response_with_mailboxes, record_list_entry};
use folders::{
    authenticated_list_command, read_imap_list_response, read_imap_list_response_with_details,
};
#[cfg(test)]
use list_parser::tokens as parse_list_tokens;
#[cfg(test)]
use list_parser::{delimiter as parse_list_delimiter, mailbox_name as parse_list_mailbox_name};
use literal_framing::TaggedResponseScanner;
use resolver::{connect_racing, resolve_dns_with_deadline};

pub(crate) fn internal_dns_resolver_main(arguments: &[std::ffi::OsString]) -> i32 {
    resolver::internal_dns_resolver_main(arguments)
}

const BUDGETED_IMAP_IO_SLICE: Duration = Duration::from_millis(250);
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

fn write_imap_command<S: Write>(
    stream: &mut S,
    bytes: &[u8],
    budget: Option<&MessageFetchBudget<'_>>,
    operation: &str,
) -> Result<(), String> {
    let Some(budget) = budget else {
        return stream
            .write_all(bytes)
            .map_err(|error| format!("{operation}: {error}"));
    };
    let mut offset = 0;
    while offset < bytes.len() {
        budget.check()?;
        match stream.write(&bytes[offset..]) {
            Ok(0) => return Err(format!("{operation}: IMAP connection closed while writing")),
            Ok(written) => offset = offset.saturating_add(written),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(error) => return Err(format!("{operation}: {error}")),
        }
    }
    Ok(())
}

fn read_imap_tagged_with_limit<S: Read>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
    max_bytes: usize,
) -> Result<(), String> {
    let mut raw_response = Vec::new();
    let mut scanner = TaggedResponseScanner::new(tag);
    let deadline = Instant::now() + MAX_IMAP_COMMAND_DURATION;
    loop {
        let count = read_with_deadline(stream, buffer, deadline, None)?;
        if count == 0 {
            return Err(format!("IMAP connection closed before {tag} completed"));
        }
        raw_response.extend_from_slice(&buffer[..count]);
        if raw_response.len() > max_bytes {
            return Err(format!(
                "IMAP response for {tag} exceeded the {max_bytes}-byte safety limit"
            ));
        }
        if scanner.scan(&raw_response) {
            response.clear();
            response.push_str(&String::from_utf8_lossy(&raw_response));
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
    let mut scanner = TaggedResponseScanner::new(tag);
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
        if scanner.scan(&raw_response) {
            response.clear();
            response.push_str(&String::from_utf8_lossy(&raw_response));
            return Ok(());
        }
    }
}

fn read_imap_tagged_with_optional_budget<S: Read>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
    budget: Option<&MessageFetchBudget<'_>>,
) -> Result<(), String> {
    match budget {
        Some(budget) => {
            read_imap_tagged_with_budget(stream, tag, response, buffer, 1_048_576, budget)
        }
        None => read_imap_tagged(stream, tag, response, buffer),
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
    let mut scanner = TaggedResponseScanner::new(tag);
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
        if scanner.scan(&response) {
            return Ok(response);
        }
    }
}

/// Locate a tagged completion line without interpreting bytes inside IMAP
/// literals as protocol framing. Literal payloads are arbitrary octets and
/// may contain lines that look exactly like the command tag.
#[cfg(test)]
fn tagged_response_outside_literals(response: &[u8], tag: &str) -> bool {
    TaggedResponseScanner::new(tag).scan(response)
}

const MAX_IMAP_LIST_INVENTORY_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAP_COMMAND_DURATION: Duration = Duration::from_secs(15);
const MAX_MESSAGE_FETCH_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// Initial metadata FETCH page; `FetchPagePlanner` adapts it per folder.
const MESSAGE_FETCH_PAGE_SIZE: u64 = 128;
const MAX_UID_SET_BYTES: usize = 7_000;
const MESSAGE_UID_SEARCH_WINDOW_SIZE: u64 = 10_000;
const MAX_MESSAGE_FETCH_RECORDS: usize = 1_000_000;
pub(crate) const MAX_BODY_HASH_MESSAGES_PER_ENDPOINT: usize = 100_000;
// This is a fail-closed bound for one fetched page's transient Rust state. Live
// metadata is staged into SQLite immediately after parsing, so the bound is
// released after each staged page rather than accumulating with account size.
const MAX_ESTIMATED_FETCHED_STATE_BYTES: usize = 256 * 1024 * 1024;
const MAX_MAILBOX_STABILITY_ATTEMPTS: usize = 2;

fn body_hash_message_limit_exceeded(existing: usize, incoming: usize) -> bool {
    existing.saturating_add(incoming) > MAX_BODY_HASH_MESSAGES_PER_ENDPOINT
}

pub(crate) struct MessageFetchBudget<'a> {
    pub(super) deadline: Instant,
    pub(super) cancel: &'a AtomicBool,
}

/// Shared admission accounting for transient fetched pages on both sides of
/// one live verification. The staged sink releases each page after SQLite
/// insertion, while the shared budget still prevents either side from
/// bypassing the transient-state ceiling.
pub(crate) struct MessageStateBudget {
    estimated_bytes: AtomicUsize,
}

impl MessageStateBudget {
    pub(crate) fn new() -> Self {
        Self {
            estimated_bytes: AtomicUsize::new(0),
        }
    }

    fn reserve(&self, bytes: usize) -> Result<(), String> {
        let reserved =
            self.estimated_bytes
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    let next = current.checked_add(bytes)?;
                    (next <= MAX_ESTIMATED_FETCHED_STATE_BYTES).then_some(next)
                });
        if reserved.is_err() {
            return Err(format!(
                "account pair exceeded the estimated {MAX_ESTIMATED_FETCHED_STATE_BYTES}-byte fetched-state admission budget"
            ));
        }
        Ok(())
    }

    fn release(&self, bytes: usize) {
        self.estimated_bytes.fetch_sub(bytes, Ordering::AcqRel);
    }
}

struct StateReservation<'a> {
    budget: &'a MessageStateBudget,
    bytes: usize,
    committed: bool,
}

impl<'a> StateReservation<'a> {
    fn new(budget: &'a MessageStateBudget) -> Self {
        Self {
            budget,
            bytes: 0,
            committed: false,
        }
    }

    fn reserve(&mut self, bytes: usize) -> Result<(), String> {
        self.budget.reserve(bytes)?;
        self.bytes = self.bytes.saturating_add(bytes);
        Ok(())
    }

    fn release(&mut self, bytes: usize) {
        let released = bytes.min(self.bytes);
        self.bytes -= released;
        self.budget.release(released);
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for StateReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.budget.release(self.bytes);
        }
    }
}

/// Shared admission budget for explicit RFC822 body hashing. The budget is
/// consumed across both accounts and is intentionally not released after a
/// page: every downloaded byte is part of the run's resource ceiling, even
/// when a moving mailbox later causes that folder to be retried.
pub(crate) struct BodyHashBudget {
    max_total_bytes: usize,
    consumed_bytes: AtomicUsize,
}

impl BodyHashBudget {
    pub(crate) fn new(max_total_bytes: usize) -> Self {
        Self {
            max_total_bytes,
            consumed_bytes: AtomicUsize::new(0),
        }
    }

    fn reserve(&self, bytes: usize) -> Result<(), String> {
        let reserved =
            self.consumed_bytes
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    let next = current.checked_add(bytes)?;
                    (next <= self.max_total_bytes).then_some(next)
                });
        if reserved.is_err() {
            return Err(format!(
                "RFC822 body hashing exceeded the configured {}-byte total bound",
                self.max_total_bytes
            ));
        }
        Ok(())
    }
}

pub(crate) struct BodyHashOptions<'a> {
    pub(crate) max_body_bytes: usize,
    pub(crate) budget: &'a BodyHashBudget,
}

pub(crate) struct FetchedAccountSummary {
    pub(crate) mailboxes: HashSet<String>,
    pub(crate) mailbox_details: Vec<MailboxDescriptor>,
    pub(crate) total_exists: u64,
}

trait MessageSink {
    fn insert_batch(&mut self, messages: &crate::core::ExtractedMessages) -> Result<(), String>;
    fn insert_batch_with_fingerprints(
        &mut self,
        messages: &crate::core::ExtractedMessages,
        fingerprints: &HashMap<crate::core::MailboxMessageKey, String>,
    ) -> Result<(), String> {
        if fingerprints.is_empty() {
            self.insert_batch(messages)
        } else {
            Err("body fingerprints are not supported by this message sink".to_owned())
        }
    }
    fn rollback_mailbox(&mut self, mailbox: &str) -> Result<(), String>;
    fn len(&self) -> usize;
    /// Rows currently staged for one folder generation. Compared against the
    /// server's EXISTS after a scan so rows kept from an interrupted run can
    /// never stand in for messages that were expunged in the meantime.
    fn mailbox_len(&self, mailbox: &str, uidvalidity: u64) -> Result<u64, String>;
    fn resume_after_uid(
        &mut self,
        _mailbox: &str,
        _snapshot: crate::core::FolderSnapshot,
    ) -> Result<Option<u64>, String> {
        Ok(None)
    }
    fn checkpoint_page(
        &mut self,
        _mailbox: &str,
        _snapshot: crate::core::FolderSnapshot,
        _last_uid: u64,
    ) -> Result<(), String> {
        Ok(())
    }
    fn complete_mailbox(
        &mut self,
        _mailbox: &str,
        _snapshot: crate::core::FolderSnapshot,
        _last_uid: u64,
    ) -> Result<(), String> {
        Ok(())
    }
}

struct StageMessageSink<'a> {
    stage: &'a mut crate::core::MessageMetadataStage,
    side: crate::core::StagedMessageSide,
    count: usize,
}

impl MessageSink for StageMessageSink<'_> {
    fn insert_batch(&mut self, messages: &crate::core::ExtractedMessages) -> Result<(), String> {
        self.stage.insert_messages(self.side, messages)?;
        self.count = self.count.saturating_add(messages.len());
        Ok(())
    }

    fn insert_batch_with_fingerprints(
        &mut self,
        messages: &crate::core::ExtractedMessages,
        fingerprints: &HashMap<crate::core::MailboxMessageKey, String>,
    ) -> Result<(), String> {
        self.stage
            .insert_messages_with_fingerprints(self.side, messages, fingerprints)?;
        self.count = self.count.saturating_add(messages.len());
        Ok(())
    }

    fn rollback_mailbox(&mut self, mailbox: &str) -> Result<(), String> {
        self.stage.delete_mailbox(self.side, mailbox)?;
        self.count = self
            .stage
            .count(self.side)
            .map_err(|error| error.to_string())? as usize;
        Ok(())
    }

    fn len(&self) -> usize {
        self.count
    }

    fn mailbox_len(&self, mailbox: &str, uidvalidity: u64) -> Result<u64, String> {
        self.stage
            .count_mailbox(self.side, mailbox, uidvalidity)
            .map_err(|error| error.to_string())
    }

    fn resume_after_uid(
        &mut self,
        mailbox: &str,
        snapshot: crate::core::FolderSnapshot,
    ) -> Result<Option<u64>, String> {
        let cursor = self.stage.resume_mailbox(self.side, mailbox, snapshot)?;
        self.count = self
            .stage
            .count(self.side)
            .map_err(|error| error.to_string())? as usize;
        Ok(cursor)
    }

    fn checkpoint_page(
        &mut self,
        mailbox: &str,
        snapshot: crate::core::FolderSnapshot,
        last_uid: u64,
    ) -> Result<(), String> {
        self.stage
            .checkpoint_page(self.side, mailbox, snapshot, last_uid)
    }

    fn complete_mailbox(
        &mut self,
        mailbox: &str,
        snapshot: crate::core::FolderSnapshot,
        last_uid: u64,
    ) -> Result<(), String> {
        self.stage
            .complete_mailbox(self.side, mailbox, snapshot, last_uid)
    }
}

impl<'a> MessageFetchBudget<'a> {
    pub(crate) fn new(timeout: Duration, cancel: &'a AtomicBool) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            cancel,
        }
    }

    pub(super) fn check(&self) -> Result<(), String> {
        if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("message-level verification cancelled by operator".into());
        }
        if Instant::now() >= self.deadline {
            return Err("message-level verification exceeded its execution deadline".into());
        }
        Ok(())
    }
}

/// A socket read timeout is only a progress hint; it is not the same as the
/// LIST operation's total deadline. TLS/read adapters can return `TimedOut`
/// while waiting for the rest of a record, so retry transient reads until the
/// bounded LIST deadline expires instead of allowing discovery to stall.
fn read_with_deadline<S: Read>(
    stream: &mut S,
    buffer: &mut [u8],
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> Result<usize, String> {
    loop {
        if cancel.is_some_and(|cancel| cancel.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err("IMAP read cancelled by operator".into());
        }
        if Instant::now() >= deadline {
            return Err("IMAP read deadline exceeded".into());
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

fn read_imap_greeting<S: Read>(
    stream: &mut S,
    host: &str,
    budget: Option<&MessageFetchBudget<'_>>,
) -> Result<String, String> {
    let mut response = String::new();
    let mut buffer = [0; 4096];
    let deadline = budget
        .map(|budget| budget.deadline.min(Instant::now() + Duration::from_secs(8)))
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(8));
    loop {
        let count = read_with_deadline(
            stream,
            &mut buffer,
            deadline,
            budget.map(|budget| budget.cancel),
        )?;
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

/// Whether the server's tagged completion for `tag` is OK. The completion is
/// located by the literal-aware framer, never by scanning raw lines.
pub(crate) fn imap_command_succeeded(response: impl AsRef<[u8]>, tag: &str) -> bool {
    literal_framing::tagged_completion(response.as_ref(), tag).is_some_and(|line| {
        String::from_utf8_lossy(line)
            .split_whitespace()
            .nth(1)
            .is_some_and(|status| atom_eq(status, "OK"))
    })
}

fn imap_command_failure(
    response: impl AsRef<[u8]>,
    tag: &str,
    operation: &str,
    host: &str,
) -> String {
    let detail = literal_framing::tagged_completion(response.as_ref(), tag)
        .map(String::from_utf8_lossy)
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
    connect_tls_stream_inner(host, transport, ca_bundle, certificate_pin_sha256, None)
}

fn connect_tls_stream_with_budget(
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
        .map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(io_timeout))
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
    let name = ServerName::try_from(server_name)
        .map_err(|e| format!("{host}: invalid TLS server name: {e}"))?;

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
            .map_err(|e| format!("{host}: TLS configuration failed: {e}"))?;
        let mut stream = StreamOwned::new(connection, tcp);
        complete_tls_handshake(&mut stream, host, budget)?;
        refresh_socket_timeout(&stream.sock, budget)
            .map_err(|e| format!("{host}: could not set TLS I/O timeout: {e}"))?;
        verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
        return Ok((stream, greeting));
    }

    let connection = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| format!("{host}: TLS configuration failed: {e}"))?;
    let mut stream = StreamOwned::new(connection, tcp);
    refresh_socket_timeout(&stream.sock, budget)
        .map_err(|e| format!("{host}: could not set TLS I/O timeout: {e}"))?;
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
        authenticate_imap_stream(stream, host, user, credential, auth_method, greeting, None)?;
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
    let digest = Sha256::digest(certificate.as_ref());
    let actual = digest
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>();
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
    budget: Option<&MessageFetchBudget<'_>>,
) -> Result<(S, String), String> {
    let mut response = String::new();
    let mut buffer = [0; 4096];
    write_imap_command(
        &mut stream,
        b"a001 CAPABILITY\r\n",
        budget,
        "could not write pre-auth CAPABILITY",
    )?;
    read_imap_tagged_with_optional_budget(&mut stream, "a001", &mut response, &mut buffer, budget)?;
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
            write_imap_command(
                &mut stream,
                b"a002 AUTHENTICATE XOAUTH2\r\n",
                budget,
                "could not write XOAUTH2 authentication command",
            )?;
            response.clear();
            let (deadline, cancel) = budget
                .map(|budget| (Some(budget.deadline), Some(budget.cancel)))
                .unwrap_or((None, None));
            read_auth_continuation_with_deadline(
                &mut stream,
                "a002",
                &mut response,
                &mut buffer,
                deadline,
                cancel,
            )?;
            write_imap_command(
                &mut stream,
                encoded.as_bytes(),
                budget,
                "could not write XOAUTH2 payload",
            )?;
            write_imap_command(
                &mut stream,
                b"\r\n",
                budget,
                "could not finish XOAUTH2 payload",
            )?;
            response.clear();
            read_auth_result_with_deadline(
                &mut stream,
                "a002",
                &mut response,
                &mut buffer,
                deadline,
                cancel,
            )?;
        } else {
            let quoted_password = SecretString::new(imap_quote(credential)?);
            let login = format!(
                "a002 LOGIN {} {}\r\n",
                imap_quote(user)?,
                quoted_password.as_str()
            );
            let login = SecretString::new(login);
            write_imap_command(
                &mut stream,
                login.as_bytes(),
                budget,
                "could not write IMAP authentication",
            )?;
            read_imap_tagged_with_optional_budget(
                &mut stream,
                "a002",
                &mut response,
                &mut buffer,
                budget,
            )?;
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
    write_imap_command(
        &mut stream,
        b"a003 CAPABILITY\r\n",
        budget,
        "could not write post-auth CAPABILITY",
    )?;
    let mut post_auth_response = String::new();
    read_imap_tagged_with_optional_budget(
        &mut stream,
        "a003",
        &mut post_auth_response,
        &mut buffer,
        budget,
    )?;
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
        authenticate_imap_stream(stream, host, user, credential, auth_method, greeting, None)?;
    let mut buffer = [0; 4096];
    stream
        .write_all(b"a004 NAMESPACE\r\n")
        .map_err(|e| e.to_string())?;
    let mut namespace_response = String::new();
    read_imap_tagged(&mut stream, "a004", &mut namespace_response, &mut buffer)?;
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
    caps.namespace = namespace_parser::parse(&namespace_response);
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

/// Which side of a migration an endpoint belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MailSide {
    Source,
    Destination,
}

/// A fresh authentication probe failure and the side that produced it, so
/// the controller can attribute provider throttling to the right endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidedProbeError {
    pub(crate) side: MailSide,
    pub(crate) message: String,
}

pub(crate) fn fresh_dual_imaps_authentication(form: &crate::Form) -> Result<(), SidedProbeError> {
    if !fresh_imap_authentication_applies(form) {
        return Ok(());
    }
    let sided = |side| move |message| SidedProbeError { side, message };
    let source = endpoint_for_probe(&form.profile.source_host, &form.profile.source_port)
        .map_err(sided(MailSide::Source))?;
    let destination = endpoint_for_probe(
        &form.profile.destination_host,
        &form.profile.destination_port,
    )
    .map_err(sided(MailSide::Destination))?;
    probe_tls_authentication_with_transport(
        &source,
        &form.profile.source_user,
        form.source_password.as_str(),
        &form.profile.source_auth,
        &form.profile.source_tls,
        &form.profile.source_ca_bundle,
        &form.profile.source_certificate_pin_sha256,
    )
    .map_err(sided(MailSide::Source))?;
    probe_tls_authentication_with_transport(
        &destination,
        &form.profile.destination_user,
        form.destination_password.as_str(),
        &form.profile.destination_auth,
        &form.profile.destination_tls,
        &form.profile.destination_ca_bundle,
        &form.profile.destination_certificate_pin_sha256,
    )
    .map_err(sided(MailSide::Destination))?;
    Ok(())
}

/// Fetch bounded message metadata for one mailbox over an authenticated TLS
/// IMAP session. This is deliberately independent of the transfer engine:
/// imapsync output contains local UIDs and counters, not a portable message
/// identity. A fresh source and destination read therefore provides the
/// evidence used by the message reconciler instead of trusting engine prose.
#[allow(clippy::too_many_arguments)]
/// Fetch messages from a single mailbox using an existing authenticated stream.
/// This allows connection reuse across multiple folders instead of opening a new
/// connection for each one. Returns early on error without poisoning the stream state.
#[derive(Debug)]
enum MailboxFetchError {
    Changed(String),
    Control(String),
    SessionFatal(String),
    Folder(String),
}

impl From<String> for MailboxFetchError {
    fn from(error: String) -> Self {
        classify_mailbox_fetch_error(error)
    }
}

impl From<&str> for MailboxFetchError {
    fn from(error: &str) -> Self {
        classify_mailbox_fetch_error(error.to_owned())
    }
}

fn classify_mailbox_fetch_error(error: String) -> MailboxFetchError {
    let lower = error.to_ascii_lowercase();
    if lower.contains("cancelled")
        || lower.contains("execution deadline")
        || lower.contains("deadline exceeded")
    {
        MailboxFetchError::Control(error)
    } else if [
        "connection closed",
        "connection reset",
        "broken pipe",
        "timed out",
        "timeout",
        "tls",
        "could not read",
        "could not write",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        MailboxFetchError::SessionFatal(error)
    } else {
        MailboxFetchError::Folder(error)
    }
}

fn fetch_mailbox_with_existing_stream<S: Read + Write, C: MessageSink>(
    stream: &mut S,
    host: &str,
    mailbox: &str,
    budget: &MessageFetchBudget<'_>,
    state_budget: &MessageStateBudget,
    body_hash: Option<&BodyHashOptions<'_>>,
    sink: &mut C,
) -> Result<u64, MailboxFetchError> {
    budget.check()?;
    if mailbox.trim().is_empty() {
        return Err("message-level verification requires a non-empty mailbox".into());
    }
    let quoted_mailbox = imap_quote(mailbox)?;
    let select = format!("v001 SELECT {quoted_mailbox}\r\n");
    write_imap_command(
        stream,
        select.as_bytes(),
        Some(budget),
        &format!("{host}: could not select mailbox"),
    )?;
    let mut response = String::new();
    let mut buffer = [0; 4096];
    read_imap_tagged_with_budget(
        stream,
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
        )
        .into());
    }
    let (start_exists, start_uidvalidity, uidnext) =
        parse_selected_mailbox(&response, host, mailbox)?;
    let start_uidvalidity = start_uidvalidity.ok_or_else(|| {
        format!(
            "{host}: SELECT {mailbox} did not return UIDVALIDITY; mutation detection is unavailable"
        )
    })?;
    let uidnext = uidnext.ok_or_else(|| {
        format!("{host}: SELECT {mailbox} did not return UIDNEXT; bounded UID enumeration is unavailable")
    })?;
    let mut state_reservation = StateReservation::new(state_budget);
    let mut page_number = 0usize;
    let mailbox_context: std::sync::Arc<str> = std::sync::Arc::from(mailbox);
    let snapshot = crate::core::FolderSnapshot {
        uidvalidity: start_uidvalidity,
        uidnext,
        exists: start_exists,
    };
    let resume_after_uid = sink.resume_after_uid(mailbox, snapshot)?;
    let searched_uid_count = enumerate_uid_pages(
        stream,
        host,
        mailbox,
        uidnext,
        &mut response,
        &mut buffer,
        budget,
        resume_after_uid,
        if body_hash.is_some() {
            FetchPagePlanner::body()
        } else {
            FetchPagePlanner::metadata()
        },
        |stream, buffer, uid_page, uid_set| {
            budget.check()?;
            if body_hash.is_some() && body_hash_message_limit_exceeded(sink.len(), uid_page.len()) {
                return Err(format!(
                    "{host}: body-hash proof is limited to {MAX_BODY_HASH_MESSAGES_PER_ENDPOINT} messages per endpoint; use metadata-only verification above that envelope"
                ));
            }
            let tag = format!("v{:03}", page_number + 3);
            page_number = page_number.saturating_add(1);
            let body_field = body_hash.map_or("", |_| " BODY.PEEK[]");
            let command = format!(
                "{tag} UID FETCH {uid_set} (UID RFC822.SIZE INTERNALDATE BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)]{body_field})\r\n"
            );
            write_imap_command(
                stream,
                command.as_bytes(),
                Some(budget),
                &format!("{host}: could not fetch mailbox metadata"),
            )?;
            let raw_response = read_imap_tagged_bytes_with_budget(
                stream,
                &tag,
                buffer,
                MAX_MESSAGE_FETCH_RESPONSE_BYTES,
                budget,
            )?;
            let response_bytes = raw_response.len();
            // Check the raw bytes: a lossy UTF-8 copy would shift literal
            // lengths for 8-bit message bodies and break literal framing.
            if !imap_command_succeeded(&raw_response, &tag) {
                return Err(imap_command_failure(
                    &raw_response,
                    &tag,
                    "FETCH mailbox metadata",
                    host,
                ));
            }
            let page_messages = parse_message_fetch_metadata_response_bytes_with_mailbox(
                &raw_response,
                std::sync::Arc::clone(&mailbox_context),
                Some(start_uidvalidity),
            )?;
            validate_fetch_page_coverage(&page_messages, uid_page, host, mailbox)?;
            let body_page = body_hash
                .map(|options| {
                    parse_message_fetch_body_hashes_response_bytes(
                        &raw_response,
                        mailbox,
                        Some(start_uidvalidity),
                        options.max_body_bytes,
                    )
                })
                .transpose()?;
            if let (Some(options), Some(body_page)) = (body_hash, body_page.as_ref()) {
                options.budget.reserve(body_page.total_bytes)?;
                if body_page.fingerprints.len() != page_messages.len()
                    || body_page.fingerprints.keys().collect::<HashSet<_>>()
                        != page_messages.keys().collect::<HashSet<_>>()
                {
                    return Err(format!(
                        "{host}: folder {mailbox}: body FETCH coverage mismatch (metadata {}, body {})",
                        page_messages.len(),
                        body_page.fingerprints.len()
                    ));
                }
            }
            for (key, message) in &page_messages {
                let fingerprint = body_page
                    .as_ref()
                    .and_then(|page| page.fingerprints.get(key));
                state_reservation.reserve(estimated_message_record_bytes(
                    key,
                    message,
                    fingerprint,
                ))?;
            }
            if let Some(body_page) = body_page.as_ref() {
                sink.insert_batch_with_fingerprints(&page_messages, &body_page.fingerprints)?;
            } else {
                sink.insert_batch(&page_messages)?;
            }
            // The live sink stages the page into SQLite. Do not retain the
            // transient Rust estimate after durable staging succeeds.
            state_reservation.release(
                page_messages
                    .iter()
                    .map(|(key, message)| {
                        estimated_message_record_bytes(
                            key,
                            message,
                            body_page
                                .as_ref()
                                .and_then(|page| page.fingerprints.get(key)),
                        )
                    })
                    .sum(),
            );
            if sink.len() > MAX_MESSAGE_FETCH_RECORDS {
                return Err(format!(
                    "{host}: folder {mailbox}: message count exceeds {MAX_MESSAGE_FETCH_RECORDS}"
                ));
            }
            if let Some(last_uid) = uid_page.last().copied() {
                sink.checkpoint_page(mailbox, snapshot, last_uid)?;
            }
            Ok(response_bytes)
        },
    )?;
    if searched_uid_count != start_exists {
        return Err(format!(
            "{host}: folder {mailbox}: SEARCH coverage mismatch (EXISTS {start_exists}, validated UIDs {searched_uid_count})",
        )
        .into());
    }
    // Pages staged before an interruption are not refetched on resume. Any
    // message expunged since then leaves a stale row behind, so the staged
    // folder must hold exactly the server's current message count. A
    // mismatch discards the folder's stage and rescans it from the start.
    let staged = sink.mailbox_len(mailbox, start_uidvalidity)?;
    if staged != start_exists {
        return Err(MailboxFetchError::Changed(format!(
            "{host}: folder {mailbox}: staged rows ({staged}) differ from EXISTS {start_exists}; rescan required"
        )));
    }

    // Re-SELECT after the bounded scan. If the folder changed while it was
    // being read, discard the scan so a moving mailbox cannot be reported as
    // missing, extra, or modified mail. The account-level caller retries this
    // folder once before marking verification incomplete.
    let end_tag = format!("v{:03}", page_number + 3);
    let end_select = format!("{end_tag} SELECT {quoted_mailbox}\r\n");
    write_imap_command(
        stream,
        end_select.as_bytes(),
        Some(budget),
        &format!("{host}: could not re-select mailbox"),
    )?;
    response.clear();
    read_imap_tagged_with_budget(
        stream,
        &end_tag,
        &mut response,
        &mut buffer,
        1_048_576,
        budget,
    )?;
    if !imap_command_succeeded(&response, &end_tag) {
        return Err(imap_command_failure(
            &response,
            &end_tag,
            "re-select mailbox for mutation check",
            host,
        )
        .into());
    }
    let (end_exists, end_uidvalidity, end_uidnext) =
        parse_selected_mailbox(&response, host, mailbox)?;
    if end_uidvalidity != Some(start_uidvalidity)
        || end_uidnext != Some(uidnext)
        || end_exists != start_exists
    {
        return Err(MailboxFetchError::Changed(format!(
            "{host}: mailbox {mailbox} changed during verification (UIDVALIDITY {start_uidvalidity:?}->{end_uidvalidity:?}, UIDNEXT {uidnext}->{end_uidnext:?}, message count {start_exists}->{end_exists}); retry required"
        )));
    }
    let last_uid = uidnext.saturating_sub(1);
    sink.complete_mailbox(mailbox, snapshot, last_uid)?;
    state_reservation.commit();
    // This path is metadata-only by design. BODY[] hashing belongs to the
    // separate content-verification adapter and is not populated here.
    Ok(start_exists)
}

fn fetch_mailbox_with_stability_retry<S: Read + Write, C: MessageSink>(
    stream: &mut S,
    host: &str,
    mailbox: &str,
    budget: &MessageFetchBudget<'_>,
    state_budget: &MessageStateBudget,
    body_hash: Option<&BodyHashOptions<'_>>,
    sink: &mut C,
) -> Result<u64, MailboxFetchError> {
    let mut last_error = None;
    for attempt in 0..MAX_MAILBOX_STABILITY_ATTEMPTS {
        match fetch_mailbox_with_existing_stream(
            stream,
            host,
            mailbox,
            budget,
            state_budget,
            body_hash,
            sink,
        ) {
            Ok(result) => return Ok(result),
            Err(MailboxFetchError::Changed(error)) => {
                if let Err(rollback) = sink.rollback_mailbox(mailbox) {
                    return Err(MailboxFetchError::Folder(rollback));
                }
                last_error = Some(error);
                if attempt + 1 < MAX_MAILBOX_STABILITY_ATTEMPTS {
                    continue;
                }
            }
            Err(error @ MailboxFetchError::Control(_))
            | Err(error @ MailboxFetchError::SessionFatal(_))
            | Err(error @ MailboxFetchError::Folder(_)) => {
                if let Err(rollback) = sink.rollback_mailbox(mailbox) {
                    return Err(MailboxFetchError::Folder(rollback));
                }
                return Err(error);
            }
        }
    }
    Err(MailboxFetchError::Changed(last_error.unwrap_or_else(
        || format!("{host}: mailbox {mailbox} stability check failed"),
    )))
}

/// Enumerate bounded UID windows and hand each fetch-sized page to the caller.
/// No response contains the entire mailbox UID set, and no all-mailbox UID
/// vector is retained between windows.
#[allow(clippy::too_many_arguments)]
fn enumerate_uid_pages<S: Read + Write, F>(
    stream: &mut S,
    host: &str,
    mailbox: &str,
    uidnext: u64,
    response: &mut String,
    buffer: &mut [u8; 4096],
    budget: &MessageFetchBudget<'_>,
    resume_after_uid: Option<u64>,
    mut planner: FetchPagePlanner,
    mut consume_page: F,
) -> Result<u64, String>
where
    F: FnMut(&mut S, &mut [u8; 4096], &[u64], &str) -> Result<usize, String>,
{
    let mut window_start = 1_u64;
    let mut searched_uid_count = 0_u64;
    let mut search_number = 0usize;
    while window_start < uidnext {
        budget.check()?;
        let window_end = window_start
            .saturating_add(MESSAGE_UID_SEARCH_WINDOW_SIZE - 1)
            .min(uidnext.saturating_sub(1));
        let search_tag = format!("s{:03}", search_number + 2);
        search_number = search_number.saturating_add(1);
        let search_command = format!("{search_tag} UID SEARCH UID {window_start}:{window_end}\r\n");
        write_imap_command(
            stream,
            search_command.as_bytes(),
            Some(budget),
            &format!("{host}: could not search mailbox UIDs"),
        )?;
        response.clear();
        read_imap_tagged_with_budget(stream, &search_tag, response, buffer, 1_048_576, budget)?;
        if !imap_command_succeeded(response.as_str(), &search_tag) {
            return Err(imap_command_failure(
                response.as_str(),
                &search_tag,
                "SEARCH mailbox UID window",
                host,
            ));
        }
        let mut uids = parse_uid_search_response(response, host, mailbox)?;
        uids.sort_unstable();
        if uids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(format!(
                "{host}: folder {mailbox}: SEARCH returned duplicate UIDs in window {window_start}:{window_end}"
            ));
        }
        if uids
            .iter()
            .any(|uid| *uid < window_start || *uid > window_end)
        {
            return Err(format!(
                "{host}: folder {mailbox}: SEARCH returned a UID outside window {window_start}:{window_end}"
            ));
        }
        searched_uid_count = searched_uid_count.saturating_add(uids.len() as u64);
        // Skip exactly the UIDs already staged before an interruption. Page
        // sizes adapt, so page boundaries are not stable across runs and
        // cannot be used to decide what was already fetched.
        let mut offset =
            resume_after_uid.map_or(0, |resume| uids.partition_point(|uid| *uid <= resume));
        while offset < uids.len() {
            let (count, uid_set) = encode_uid_page(&uids[offset..], planner.size());
            let uid_page = &uids[offset..offset + count];
            let response_bytes = consume_page(stream, buffer, uid_page, &uid_set)?;
            planner.observe(uid_page.len(), response_bytes);
            offset += count;
        }
        window_start = window_end.saturating_add(1);
    }
    Ok(searched_uid_count)
}

/// Adaptive UID FETCH page sizing.
///
/// A fixed small page turns a large mailbox into thousands of round trips,
/// which dominates on providers tens of milliseconds away. Pages are instead
/// sized from the observed response bytes per message so each response lands
/// near a byte budget: growth is at most 2x per page, contraction is
/// immediate, and both stay within fixed bounds. Body pages use a separate,
/// smaller envelope because one message may approach the per-message bound.
#[derive(Debug, Clone)]
struct FetchPagePlanner {
    size: usize,
    min: usize,
    max: usize,
    target_response_bytes: usize,
}

impl FetchPagePlanner {
    fn metadata() -> Self {
        Self {
            size: MESSAGE_FETCH_PAGE_SIZE as usize,
            min: 32,
            max: 1024,
            target_response_bytes: 1024 * 1024,
        }
    }

    fn body() -> Self {
        Self {
            size: 8,
            min: 1,
            max: 64,
            target_response_bytes: MAX_MESSAGE_FETCH_RESPONSE_BYTES / 4,
        }
    }

    fn size(&self) -> usize {
        self.size
    }

    fn observe(&mut self, messages: usize, response_bytes: usize) {
        if messages == 0 {
            return;
        }
        let per_message = (response_bytes / messages).max(1);
        let ideal = self.target_response_bytes / per_message;
        self.size = ideal
            .min(self.size.saturating_mul(2))
            .clamp(self.min, self.max);
    }
}

/// Encode the longest prefix of sorted, unique `uids` (up to `max_count`) as
/// an IMAP sequence set, compressing consecutive runs (`1:500,502,504:900`).
/// The set is kept under `MAX_UID_SET_BYTES` so the command line stays within
/// the 8192-octet client limit RFC 7162 recommends; at least one UID is
/// always taken.
fn encode_uid_page(uids: &[u64], max_count: usize) -> (usize, String) {
    use std::fmt::Write as _;
    let mut set = String::new();
    let mut count = 0;
    while count < uids.len().min(max_count.max(1)) {
        let start = uids[count];
        let mut end_index = count;
        while end_index + 1 < uids.len().min(max_count.max(1))
            && uids[end_index + 1] == uids[end_index] + 1
        {
            end_index += 1;
        }
        let mut item = String::new();
        if end_index == count {
            let _ = write!(item, "{start}");
        } else {
            let _ = write!(item, "{start}:{}", uids[end_index]);
        }
        let separator = usize::from(!set.is_empty());
        if count > 0 && set.len() + separator + item.len() > MAX_UID_SET_BYTES {
            break;
        }
        if separator == 1 {
            set.push(',');
        }
        set.push_str(&item);
        count = end_index + 1;
    }
    (count, set)
}

fn parse_uid_search_response(
    response: &str,
    host: &str,
    mailbox: &str,
) -> Result<Vec<u64>, String> {
    let line = response
        .lines()
        .find(|line| is_untagged_response(line, "SEARCH"))
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
    Ok(uids)
}

fn validate_fetch_page_coverage(
    messages: &crate::core::ExtractedMessages,
    requested_uids: &[u64],
    host: &str,
    mailbox: &str,
) -> Result<(), String> {
    let parsed_uids = messages
        .keys()
        .map(|key| {
            if key.mailbox.as_ref() != mailbox {
                return Err(format!(
                    "{host}: folder {mailbox}: FETCH returned a record for mailbox {}",
                    key.mailbox
                ));
            }
            key.uid.parse::<u64>().map_err(|_| {
                format!(
                    "{host}: folder {mailbox}: FETCH returned invalid UID {}",
                    key.uid
                )
            })
        })
        .collect::<Result<HashSet<_>, _>>()?;
    let requested_uids = requested_uids.iter().copied().collect::<HashSet<_>>();
    if parsed_uids != requested_uids {
        return Err(format!(
            "{host}: folder {mailbox}: FETCH coverage mismatch (requested {}, parsed {})",
            requested_uids.len(),
            parsed_uids.len()
        ));
    }
    Ok(())
}

/// Reconcile an entire IMAP account by enumerating selectable folders first.
/// Opens a single authenticated connection and reuses it for all folders.
/// Any per-folder failure invalidates the complete account result. Failed
/// folder names are collected only to produce one bounded diagnostic error;
/// successful results therefore always contain every selectable folder.
/// The folder count and every message record remain bounded by the LIST/FETCH limits.
#[allow(clippy::too_many_arguments)]
fn fetch_tls_account_messages_with_sink<C: MessageSink>(
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
    budget: &MessageFetchBudget<'_>,
    state_budget: &MessageStateBudget,
    body_hash: Option<&BodyHashOptions<'_>>,
    excluded_mailboxes: &HashSet<String>,
    sink: &mut C,
) -> Result<FetchedAccountSummary, String> {
    budget.check()?;
    if transport == "plain" {
        return Err(
            "message-level verification requires TLS; refusing to inspect a plain IMAP session"
                .into(),
        );
    }
    let (stream, greeting) =
        connect_tls_stream_with_budget(host, transport, ca_bundle, certificate_pin_sha256, budget)?;
    let (mut stream, post_auth_response) = authenticate_imap_stream(
        stream,
        host,
        user,
        credential,
        auth_method,
        greeting,
        Some(budget),
    )?;
    write_imap_command(
        &mut stream,
        authenticated_list_command(advertises_capability(&post_auth_response, "SPECIAL-USE")),
        Some(budget),
        &format!("{host}: could not enumerate folders"),
    )?;
    let mut buffer = [0; 4096];
    let mut mailbox_details = Vec::new();
    let summary = read_imap_list_response_with_details(
        &mut stream,
        "a005",
        &mut buffer,
        &mut mailbox_details,
        budget,
    )?;
    let mut mailboxes = mailbox_details
        .iter()
        .filter(|mailbox| mailbox.selectable)
        .map(|mailbox| mailbox.wire_name.clone())
        .collect::<Vec<_>>();
    if summary.mailbox_count == 0 || mailboxes.is_empty() {
        let _ = stream.write_all(b"a999 LOGOUT\r\n");
        return Err(format!(
            "{host}: folder inventory did not expose selectable mailbox names"
        ));
    }
    if summary.selectable_mailbox_count != mailboxes.len() {
        let _ = stream.write_all(b"a999 LOGOUT\r\n");
        return Err(format!(
            "{host}: folder inventory included literal or otherwise unparseable mailbox names"
        ));
    }
    mailbox_details.retain(|mailbox| !excluded_mailboxes.contains(&mailbox.wire_name));
    mailboxes.retain(|mailbox| !excluded_mailboxes.contains(mailbox));
    if mailboxes.is_empty() {
        let _ = stream.write_all(b"a999 LOGOUT\r\n");
        return Err(format!(
            "{host}: folder mapping excludes every selectable mailbox; independent verification has no source scope"
        ));
    }
    mailboxes.sort();
    mailboxes.dedup();
    let mailbox_inventory = mailboxes.iter().cloned().collect::<HashSet<_>>();
    let mut total_exists = 0_u64;
    let mut incomplete_folders = HashMap::new();
    let mut incomplete_folder_count = 0_usize;

    // Process all folders using the same authenticated connection (performance optimization).
    // This avoids opening 200 separate TLS connections for a 200-folder account.
    for mailbox in mailboxes {
        budget.check()?;
        match fetch_mailbox_with_stability_retry(
            &mut stream,
            host,
            &mailbox,
            budget,
            state_budget,
            body_hash,
            sink,
        ) {
            Ok(folder_exists) => {
                total_exists = total_exists.saturating_add(folder_exists);
                if sink.len() > MAX_MESSAGE_FETCH_RECORDS {
                    let _ = stream.write_all(b"a999 LOGOUT\r\n");
                    return Err(format!(
                        "{host}: account exceeded the {MAX_MESSAGE_FETCH_RECORDS}-message verification limit"
                    ));
                }
            }
            Err(MailboxFetchError::Control(error)) => {
                let _ = stream.write_all(b"a999 LOGOUT\r\n");
                return Err(error);
            }
            Err(MailboxFetchError::SessionFatal(error)) => {
                // Retain the folder-specific cause so the all-or-nothing
                // account failure identifies unstable folders. Keep only a
                // bounded sample: the total count preserves completeness of
                // the operator signal without retaining unbounded error text.
                incomplete_folder_count = incomplete_folder_count.saturating_add(1);
                if incomplete_folders.len() < MAX_FOLDER_FAILURE_DETAILS {
                    incomplete_folders.insert(mailbox, error);
                }
                break;
            }
            Err(MailboxFetchError::Folder(error)) => {
                incomplete_folder_count = incomplete_folder_count.saturating_add(1);
                if incomplete_folders.len() < MAX_FOLDER_FAILURE_DETAILS {
                    incomplete_folders.insert(mailbox, error);
                }
            }
            Err(MailboxFetchError::Changed(error)) => {
                incomplete_folder_count = incomplete_folder_count.saturating_add(1);
                if incomplete_folders.len() < MAX_FOLDER_FAILURE_DETAILS {
                    incomplete_folders.insert(mailbox, error);
                }
            }
        }
    }
    let _ = stream.write_all(b"a999 LOGOUT\r\n");
    if !incomplete_folders.is_empty() {
        return Err(format_folder_failures(
            host,
            &incomplete_folders,
            incomplete_folder_count,
        ));
    }
    Ok(FetchedAccountSummary {
        mailboxes: mailbox_inventory,
        mailbox_details,
        total_exists,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fetch_tls_account_messages_to_stage_with_body_hashes(
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
    budget: &MessageFetchBudget<'_>,
    state_budget: &MessageStateBudget,
    body_hash: Option<&BodyHashOptions<'_>>,
    stage: &mut crate::core::MessageMetadataStage,
    side: crate::core::StagedMessageSide,
) -> Result<FetchedAccountSummary, String> {
    fetch_tls_account_messages_to_stage_with_body_hashes_excluding(
        host,
        user,
        credential,
        auth_method,
        transport,
        ca_bundle,
        certificate_pin_sha256,
        budget,
        state_budget,
        body_hash,
        &HashSet::new(),
        stage,
        side,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fetch_tls_account_messages_to_stage_with_body_hashes_excluding(
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
    budget: &MessageFetchBudget<'_>,
    state_budget: &MessageStateBudget,
    body_hash: Option<&BodyHashOptions<'_>>,
    excluded_mailboxes: &HashSet<String>,
    stage: &mut crate::core::MessageMetadataStage,
    side: crate::core::StagedMessageSide,
) -> Result<FetchedAccountSummary, String> {
    let count = stage.count(side).map_err(|error| error.to_string())? as usize;
    if body_hash.is_some() && count > MAX_BODY_HASH_MESSAGES_PER_ENDPOINT {
        return Err(format!(
            "{host}: body-hash proof is limited to {MAX_BODY_HASH_MESSAGES_PER_ENDPOINT} messages per endpoint; use metadata-only verification above that envelope"
        ));
    }
    let mut sink = StageMessageSink { stage, side, count };
    fetch_tls_account_messages_with_sink(
        host,
        user,
        credential,
        auth_method,
        transport,
        ca_bundle,
        certificate_pin_sha256,
        budget,
        state_budget,
        body_hash,
        excluded_mailboxes,
        &mut sink,
    )
}

const MAX_FOLDER_FAILURE_DETAILS: usize = 16;

fn format_folder_failures(host: &str, failures: &HashMap<String, String>, total: usize) -> String {
    const MAX_REASON_CHARS: usize = 240;
    let mut folders = failures.keys().cloned().collect::<Vec<_>>();
    folders.sort();
    let details = folders
        .into_iter()
        .take(MAX_FOLDER_FAILURE_DETAILS)
        .map(|folder| {
            let reason = failures
                .get(&folder)
                .map(String::as_str)
                .unwrap_or("unknown failure");
            let reason = reason
                .chars()
                .map(|character| {
                    if character.is_control() {
                        ' '
                    } else {
                        character
                    }
                })
                .take(MAX_REASON_CHARS)
                .collect::<String>();
            let folder = folder
                .chars()
                .map(|character| {
                    if character.is_control() {
                        ' '
                    } else {
                        character
                    }
                })
                .take(MAX_REASON_CHARS)
                .collect::<String>();
            format!("{folder}: {reason}")
        })
        .collect::<Vec<_>>();
    let omitted = total.saturating_sub(details.len());
    if omitted == 0 {
        format!(
            "{host}: verification did not obtain stable metadata for folders: {}",
            details.join("; ")
        )
    } else {
        format!(
            "{host}: verification did not obtain stable metadata for folders: {}; (+{omitted} more)",
            details.join("; ")
        )
    }
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
) -> Result<(u64, Option<u64>, Option<u64>), String> {
    let mut exists = None;
    let mut uidvalidity = None;
    let mut uidnext = None;
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
        if let Some(index) = fields.iter().position(|field| {
            field
                .trim_matches(['[', ']'])
                .eq_ignore_ascii_case("UIDNEXT")
        }) && let Some(value) = fields.get(index + 1)
        {
            uidnext = value.trim_matches(['[', ']']).parse::<u64>().ok();
        }
    }
    let exists = exists.ok_or_else(|| {
        format!("{host}: SELECT {mailbox} did not return an untagged EXISTS count")
    })?;
    Ok((exists, uidvalidity, uidnext))
}

pub(crate) fn fresh_imap_authentication_applies(form: &crate::Form) -> bool {
    !form.dry_run
        && form.engine() == core::Engine::ImapSync
        && form.profile.source_tls != "plain"
        && form.profile.destination_tls != "plain"
}

#[cfg(test)]
mod tests {
    use super::fetch_parser::parse_message_fetch_body_hashes_response_bytes;
    use super::{
        ListInventorySummary, MAX_ESTIMATED_FETCHED_STATE_BYTES, MAX_IMAP_LIST_INVENTORY_BYTES,
        MailboxFetchError, MessageFetchBudget, MessageStateBudget, StateReservation,
        TaggedResponseScanner, authenticated_list_command, classify_mailbox_fetch_error,
        format_folder_failures, parse_list_delimiter, parse_list_mailbox_name,
        parse_message_fetch_metadata_response_bytes, parse_message_id_header,
        read_imap_list_response, read_imap_list_response_with_mailboxes, read_with_deadline,
        record_list_entry, tagged_response_outside_literals, write_imap_command,
    };
    use std::collections::HashMap;
    use std::io::{self, Cursor, Read, Write};
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

    struct TimeoutThenWrite {
        timed_out: bool,
        data: Vec<u8>,
    }

    impl Write for TimeoutThenWrite {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.timed_out {
                self.timed_out = true;
                return Err(io::Error::new(io::ErrorKind::TimedOut, "synthetic timeout"));
            }
            self.data.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn budgeted_write_retries_transient_socket_timeout() {
        let cancelled = AtomicBool::new(false);
        let budget = MessageFetchBudget::new(Duration::from_secs(1), &cancelled);
        let mut stream = TimeoutThenWrite {
            timed_out: false,
            data: Vec::new(),
        };
        write_imap_command(&mut stream, b"command\r\n", Some(&budget), "write command").unwrap();
        assert_eq!(stream.data, b"command\r\n");
    }

    #[test]
    fn budgeted_write_honors_cancellation_before_writing() {
        let cancelled = AtomicBool::new(true);
        let budget = MessageFetchBudget::new(Duration::from_secs(1), &cancelled);
        let mut stream = TimeoutThenWrite {
            timed_out: false,
            data: Vec::new(),
        };
        let error = write_imap_command(&mut stream, b"command\r\n", Some(&budget), "write command")
            .unwrap_err();
        assert!(error.contains("cancelled"));
        assert!(stream.data.is_empty());
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
    fn parser_property_inputs_never_panic_and_list_unicode_round_trips() {
        let names = ["Café", "受信箱", "📬", r#"Café\"quoted"/受信箱📬"#];
        for name in names {
            let quoted = name.replace('\\', "\\\\").replace('"', "\\\"");
            let parsed = super::parse_list_tokens(&format!("() \"/\" \"{quoted}\""));
            assert_eq!(parsed.unwrap().last().map(String::as_str), Some(name));
        }

        // Deterministic arbitrary-byte property corpus. This complements unit
        // examples by exercising malformed UTF-8 and framing at many offsets.
        let mut state = 0x4d53_5753_u32;
        for length in 0..512 {
            let mut bytes = Vec::with_capacity(length);
            for _ in 0..length {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                bytes.push(state as u8);
            }
            let text = String::from_utf8_lossy(&bytes);
            let _ = super::parse_list_tokens(&text);
            let _ = super::parse_message_fetch_metadata_response_bytes(
                &bytes,
                "property-test",
                Some(1),
            );
            let _ = tagged_response_outside_literals(&bytes, "v001");
        }
    }

    #[test]
    fn tagged_scanner_handles_split_lines_literals_and_large_payloads_incrementally() {
        let payload = format!(
            "{}v009 NO this is still literal data\r\n{}",
            "x".repeat(512 * 1024),
            "y".repeat(512 * 1024)
        );
        let response = format!(
            "* 1 FETCH (BODY[] {{{}}}\r\n{} )\r\nv009 OK FETCH completed\r\n",
            payload.len(),
            payload
        );
        let mut scanner = TaggedResponseScanner::new("v009");
        let mut accumulated = Vec::new();
        for chunk in response.as_bytes().chunks(137) {
            accumulated.extend_from_slice(chunk);
            let found = scanner.scan(&accumulated);
            if accumulated.len() < response.len() {
                assert!(!found);
            } else {
                assert!(found);
            }
        }
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
    fn list_read_deadline_honors_cancellation_before_waiting_for_data() {
        let cancel = AtomicBool::new(true);
        let mut stream = Cursor::new(Vec::<u8>::new());
        let mut buffer = [0_u8; 16];
        let error = read_with_deadline(
            &mut stream,
            &mut buffer,
            std::time::Instant::now() + Duration::from_secs(30),
            Some(&cancel),
        )
        .unwrap_err();
        assert!(error.contains("cancelled by operator"));
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
    fn list_inventory_descriptor_memory_is_aggregate_bounded() {
        let line = format!(
            "* LIST (\\HasNoChildren) \"/\" \"{}\"",
            "x".repeat(16 * 1024)
        );
        let mut summary = ListInventorySummary::default();
        let mut details = Vec::new();
        let mut inventory_bytes = 0;
        let mut error: Option<String> = None;
        for _ in 0..=MAX_IMAP_LIST_INVENTORY_BYTES / (16 * 1024) {
            if let Err(value) = record_list_entry(
                &line,
                None,
                &mut summary,
                &mut Some(&mut details),
                &mut inventory_bytes,
            ) {
                error = Some(value);
                break;
            }
        }
        assert!(
            error
                .expect("aggregate inventory limit must reject oversized descriptor population")
                .contains("inventory exceeded")
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
        assert_eq!(
            parse_list_mailbox_name(r#"* LIST (\HasNoChildren) "/" "a\"b folder""#),
            Some("a\"b folder".into())
        );
    }

    #[test]
    fn list_mailbox_parser_preserves_utf8_and_quoted_escapes() {
        assert_eq!(
            parse_list_mailbox_name(r#"* LIST (\HasNoChildren) "/" "Café""#),
            Some("Café".into())
        );
        assert_eq!(
            parse_list_mailbox_name(r#"* LIST (\HasNoChildren) "/" "郵件 📬""#),
            Some("郵件 📬".into())
        );
        assert_eq!(
            parse_list_mailbox_name(r#"* LIST (\HasNoChildren) "/" "Café 郵件 📬 a\\b\"c""#),
            Some("Café 郵件 📬 a\\b\"c".into())
        );
    }

    #[test]
    fn list_parser_extracts_delimiter_after_attributes() {
        assert_eq!(
            parse_list_delimiter(r#"* LIST (\HasNoChildren \Sent) "/" "Sent Items""#),
            Some("/".into())
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
    fn list_parser_decodes_literal_mailbox_names() {
        let response =
            b"* LIST (\\HasNoChildren) \"/\" {10}\r\nSent Items\r\na005 OK LIST completed\r\n";
        let mut stream = Cursor::new(response);
        let mut buffer = [0_u8; 4096];
        let mut mailboxes = Vec::new();
        super::read_imap_list_response_with_mailboxes(
            &mut stream,
            "a005",
            &mut buffer,
            &mut mailboxes,
        )
        .unwrap();
        assert_eq!(mailboxes, ["Sent Items"]);
    }

    #[test]
    fn fetch_parser_extracts_message_metadata_and_uidvalidity() {
        let response = b"* 1 FETCH (UID 5 RFC822.SIZE 100 INTERNALDATE \"01-Jan-2024 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] {31}\r\nMessage-ID: <a@example.com>\r\n\r\n)\r\n\
                       * 2 FETCH (UID 9 RFC822.SIZE 200 INTERNALDATE \"02-Jan-2024 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] NIL)\r\n\
                       v002 OK FETCH completed\r\n";
        let messages =
            parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77)).unwrap();
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
    fn message_id_header_parser_unfolds_continuations() {
        assert_eq!(
            parse_message_id_header("Message-ID:\r\n <abc@example.com>\r\n"),
            Some("<abc@example.com>".into())
        );
    }

    #[test]
    fn folder_failure_details_are_bounded_and_preserved() {
        let failures = (0..17)
            .map(|index| (format!("folder-{index:02}"), format!("failure-{index}")))
            .collect::<HashMap<_, _>>();
        let detail = format_folder_failures("imap.example", &failures, failures.len());
        assert!(detail.contains("folder-00: failure-0"));
        assert!(detail.contains("(+1 more)"));
    }

    #[test]
    fn mailbox_failures_have_typed_control_and_session_categories() {
        assert!(matches!(
            classify_mailbox_fetch_error("message-level verification cancelled by operator".into()),
            MailboxFetchError::Control(_)
        ));
        assert!(matches!(
            classify_mailbox_fetch_error("imap connection reset by peer".into()),
            MailboxFetchError::SessionFatal(_)
        ));
        assert!(matches!(
            classify_mailbox_fetch_error("IMAP SELECT returned NO".into()),
            MailboxFetchError::Folder(_)
        ));
    }

    #[test]
    fn uid_search_parser_uses_actual_sparse_uids_not_exists_count() {
        let response = "* sEaRcH 100 104 109\r\nv002 OK SEARCH completed\r\n";
        assert_eq!(
            super::parse_uid_search_response(response, "imap.example", "INBOX").unwrap(),
            vec![100, 104, 109]
        );
    }

    #[test]
    fn estimated_record_bytes_include_body_fingerprint_storage() {
        let key = crate::core::MailboxMessageKey::new("INBOX", "900001");
        let message = crate::core::ExtractedMessage {
            message_id: Some("<message@example.com>".into()),
            uid: Some("900001".into()),
            size_bytes: Some(42),
            internal_date: Some("01-Jan-2026 00:00:00 +0000".into()),
        };
        let without_fingerprint = super::estimated_message_record_bytes(&key, &message, None);
        let with_fingerprint =
            super::estimated_message_record_bytes(&key, &message, Some(&"a".repeat(64)));
        assert_eq!(with_fingerprint - without_fingerprint, 64);
    }

    #[test]
    fn body_hash_message_envelope_is_enforced_before_the_next_fetch_page() {
        assert!(!super::body_hash_message_limit_exceeded(
            super::MAX_BODY_HASH_MESSAGES_PER_ENDPOINT - 32,
            32
        ));
        assert!(super::body_hash_message_limit_exceeded(
            super::MAX_BODY_HASH_MESSAGES_PER_ENDPOINT - 31,
            32
        ));
        assert!(super::body_hash_message_limit_exceeded(usize::MAX, 1));
    }

    #[test]
    fn shared_state_budget_rejects_combined_account_overflow() {
        let budget = MessageStateBudget::new();
        budget
            .reserve(MAX_ESTIMATED_FETCHED_STATE_BYTES - 1)
            .unwrap();
        assert!(budget.reserve(2).is_err());
    }

    #[test]
    fn staged_page_reservation_can_be_released_after_insert() {
        let budget = MessageStateBudget::new();
        let mut reservation = StateReservation::new(&budget);
        reservation.reserve(1024).unwrap();
        reservation.release(1024);
        assert!(budget.reserve(MAX_ESTIMATED_FETCHED_STATE_BYTES).is_ok());
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
    fn uid_search_parser_preserves_duplicates_for_coverage_validation() {
        assert_eq!(
            super::parse_uid_search_response(
                "* SEARCH 100 100 104\r\nv002 OK SEARCH completed\r\n",
                "imap.example",
                "INBOX"
            )
            .unwrap(),
            vec![100, 100, 104]
        );
    }

    #[test]
    fn selected_mailbox_extracts_uidnext_for_bounded_enumeration() {
        let response = "* 2 EXISTS\r\n* OK [UIDVALIDITY 77] ready\r\n* OK [UIDNEXT 900001] next\r\na001 OK SELECT completed\r\n";
        assert_eq!(
            super::parse_selected_mailbox(response, "imap.example", "INBOX").unwrap(),
            (2, Some(77), Some(900001))
        );
    }

    #[test]
    fn raw_fetch_parser_preserves_framing_around_invalid_literal_bytes() {
        let response = b"* 1 FETCH (UID 100 RFC822.SIZE 17 INTERNALDATE \"01-Jan-2026 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] {31}\r\nMessage-ID: <raw@example.com>\r\nBODY[] {17}\r\n\xff\x00v002 OK fake\r\n)\r\nv002 OK FETCH completed\r\n";
        let messages =
            super::parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77))
                .unwrap();
        let key = crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "100");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[&key].message_id.as_deref(),
            Some("<raw@example.com>")
        );
    }

    #[test]
    fn body_fetch_parser_hashes_exact_bounded_literal_bytes() {
        let response =
            b"* 1 FETCH (UID 100 BODY.PEEK[] {5+}\r\nhello)\r\nv002 OK FETCH completed\r\n";
        let fingerprints =
            parse_message_fetch_body_hashes_response_bytes(response, "INBOX", Some(77), 5).unwrap();
        let key = crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "100");
        assert_eq!(
            fingerprints.fingerprints.get(&key).map(String::as_str),
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Build a two-message metadata+body FETCH response whose literals carry
    /// arbitrary payloads, using the synchronizing or non-synchronizing form.
    fn fetch_response_with_bodies(bodies: &[&[u8]], non_sync: bool) -> Vec<u8> {
        let plus = if non_sync { "+" } else { "" };
        let mut response = Vec::new();
        for (index, body) in bodies.iter().enumerate() {
            let header = format!("Message-ID: <m{index}@example.com>\r\n\r\n");
            response.extend_from_slice(
                format!(
                    "* {} FETCH (UID {} RFC822.SIZE {} INTERNALDATE \"01-Jan-2026 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}{plus}}}\r\n{header} BODY[] {{{}{plus}}}\r\n",
                    index + 1,
                    100 + index,
                    body.len(),
                    header.len(),
                    body.len()
                )
                .as_bytes(),
            );
            response.extend_from_slice(body);
            response.extend_from_slice(b")\r\n");
        }
        response.extend_from_slice(b"v002 OK FETCH completed\r\n");
        response
    }

    fn assert_bodies_parse_exactly(bodies: &[&[u8]], non_sync: bool) {
        let response = fetch_response_with_bodies(bodies, non_sync);
        let messages =
            super::parse_message_fetch_metadata_response_bytes(&response, "INBOX", Some(77))
                .unwrap();
        let hashes =
            parse_message_fetch_body_hashes_response_bytes(&response, "INBOX", Some(77), 1 << 20)
                .unwrap();
        assert_eq!(messages.len(), bodies.len());
        assert_eq!(hashes.fingerprints.len(), bodies.len());
        for (index, body) in bodies.iter().enumerate() {
            let key = crate::core::MailboxMessageKey::with_uidvalidity(
                "INBOX",
                77,
                (100 + index).to_string(),
            );
            assert_eq!(
                messages[&key].message_id.as_deref(),
                Some(format!("<m{index}@example.com>").as_str())
            );
            assert_eq!(messages[&key].size_bytes, Some(body.len() as u64));
            assert_eq!(hashes.fingerprints[&key], sha256_hex(body));
        }
    }

    #[test]
    fn tagged_completion_inside_a_literal_cannot_mask_a_failure() {
        let response = b"* 1 FETCH (UID 1 BODY[] {21}\r\nv004 OK all good\r\n.\r\n)\r\nv004 NO [LIMIT] too many\r\n";
        assert!(!super::imap_command_succeeded(response, "v004"));
        assert!(
            super::imap_command_failure(response, "v004", "FETCH", "host")
                .ends_with("v004 NO [LIMIT] too many")
        );
        let ok = b"* 1 FETCH (UID 1 BODY[] {20}\r\nv004 NO not really\r\n)\r\nv004 OK done\r\n";
        assert!(super::imap_command_succeeded(ok, "v004"));
        // Broken literal framing never counts as success.
        assert!(!super::imap_command_succeeded(
            b"* 1 FETCH (BODY[] {99}\r\nv004 OK x\r\n",
            "v004"
        ));
    }

    #[test]
    fn uid_pages_compress_runs_and_respect_count_and_byte_limits() {
        assert_eq!(
            super::encode_uid_page(&[1, 2, 3, 5, 7, 8, 9, 20], 100),
            (8, "1:3,5,7:9,20".to_owned())
        );
        assert_eq!(
            super::encode_uid_page(&[1, 2, 3, 4], 2),
            (2, "1:2".to_owned())
        );
        assert_eq!(super::encode_uid_page(&[4, 9], 0), (1, "4".to_owned()));
        let sparse = (0..5_000_u64)
            .map(|index| 4_000_000_000 + index * 2)
            .collect::<Vec<_>>();
        let (count, set) = super::encode_uid_page(&sparse, 5_000);
        assert!(count > 0 && count < sparse.len());
        assert!(set.len() <= super::MAX_UID_SET_BYTES, "{}", set.len());
        assert_eq!(set.split(',').count(), count);
    }

    #[test]
    fn fetch_page_planner_grows_gradually_and_contracts_immediately() {
        let mut planner = super::FetchPagePlanner::metadata();
        let start = planner.size();
        planner.observe(start, start * 200);
        assert_eq!(planner.size(), start * 2, "growth is capped at 2x per page");
        for _ in 0..10 {
            planner.observe(planner.size(), planner.size() * 200);
        }
        assert_eq!(planner.size(), 1024, "growth stops at the maximum");
        planner.observe(1024, 1024 * 100_000);
        assert_eq!(
            planner.size(),
            32,
            "large responses contract to the minimum at once"
        );

        let mut body = super::FetchPagePlanner::body();
        body.observe(8, 8 * 40 * 1024 * 1024);
        assert_eq!(body.size(), 1, "one large message per body page");
    }

    #[test]
    fn body_literals_that_look_like_protocol_do_not_split_fetch_records() {
        let adversarial: &[u8] = b"Subject: test\r\n\r\nHello\r\n* 2 FETCH (UID 999 BODY[] {3}\r\nabc)\r\nv002 OK FETCH completed\r\n* BYE\r\n)\r\nBODY[HEADER.FIELDS (MESSAGE-ID)] {5}\r\nUID 4242 \r\n";
        for non_sync in [false, true] {
            assert_bodies_parse_exactly(&[adversarial, b"plain body\r\n"], non_sync);
        }
    }

    #[test]
    fn arbitrary_literal_payloads_never_alter_the_fetch_parse() {
        // Deterministic property test: protocol-looking fragments spliced at
        // arbitrary byte offsets and line boundaries, with random filler and
        // bare CR/LF bytes, must leave UIDs, Message-IDs, and digests intact.
        const FRAGMENTS: &[&[u8]] = &[
            b"* 2 FETCH (",
            b"* 2 FETCH (UID 7 BODY[] {4}\r\n",
            b"v002 OK FETCH completed\r\n",
            b"A001 OK",
            b"* BYE\r\n",
            b")\r\n",
            b"{12}\r\n",
            b"{3+}\r\n",
            b"~{2}\r\n",
            b"BODY[] {1}\r\n",
            b"\r\n",
            b"\r",
            b"\n",
        ];
        let mut state = 0x4649_5845_u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for _ in 0..400 {
            let mut bodies = Vec::new();
            for _ in 0..2 {
                let mut body = Vec::new();
                for _ in 0..(next() % 12) {
                    if next() % 2 == 0 {
                        body.extend_from_slice(FRAGMENTS[next() as usize % FRAGMENTS.len()]);
                    } else {
                        for _ in 0..(next() % 9) {
                            body.push(next() as u8);
                        }
                    }
                }
                bodies.push(body);
            }
            let bodies = bodies.iter().map(Vec::as_slice).collect::<Vec<_>>();
            assert_bodies_parse_exactly(&bodies, next() % 2 == 0);
        }
    }

    #[test]
    fn fetch_items_after_a_literal_are_found_in_protocol_text_only() {
        let response = b"* 1 FETCH (BODY[] {14}\r\nUID 1 BODY[] x UID 7 RFC822.SIZE 14)\r\nv002 OK FETCH completed\r\n";
        let hashes =
            parse_message_fetch_body_hashes_response_bytes(response, "INBOX", Some(77), 64)
                .unwrap();
        let key = crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "7");
        assert_eq!(hashes.fingerprints[&key], sha256_hex(b"UID 1 BODY[] x"));
    }

    #[test]
    fn fetch_parser_fails_closed_on_truncated_or_literal8_framing() {
        let truncated = b"* 1 FETCH (UID 1 BODY[] {50}\r\nshort)\r\nv002 OK FETCH completed\r\n";
        assert!(
            parse_message_fetch_body_hashes_response_bytes(truncated, "INBOX", Some(77), 64)
                .unwrap_err()
                .contains("response ended first")
        );
        let binary = b"* 1 FETCH (UID 1 BINARY[] ~{2}\r\nhi)\r\nv002 OK FETCH completed\r\n";
        assert!(
            super::parse_message_fetch_metadata_response_bytes(binary, "INBOX", Some(77))
                .unwrap_err()
                .contains("literal8")
        );
    }

    #[test]
    fn body_fetch_parser_fails_closed_on_missing_or_oversized_body() {
        let missing = b"* 1 FETCH (UID 100 RFC822.SIZE 5)\r\nv002 OK FETCH completed\r\n";
        let error = parse_message_fetch_body_hashes_response_bytes(missing, "INBOX", Some(77), 5)
            .unwrap_err();
        assert!(error.contains("omitted BODY[] literal"));

        let oversized = b"* 1 FETCH (UID 100 BODY[] {6}\r\nhello!)\r\nv002 OK FETCH completed\r\n";
        let error = parse_message_fetch_body_hashes_response_bytes(oversized, "INBOX", Some(77), 5)
            .unwrap_err();
        assert!(error.contains("exceeded the 5-byte body-hash bound"));

        // A bare `}\r\n` after BODY[] used to make a malformed literal
        // length slice from index 1 to index 0 and panic under fuzzing.
        let malformed_literal = b"* 2 FETCH (UID 1 BODY[] }\r\n)\r\n";
        let error =
            parse_message_fetch_body_hashes_response_bytes(malformed_literal, "INBOX", Some(77), 5)
                .unwrap_err();
        assert!(error.contains("omitted BODY[] literal"));
    }

    #[test]
    fn metadata_fetch_parser_accepts_mixed_case_atoms() {
        let response = b"* 1 fEtCh (uId 100 rFc822.sIzE 17 iNtErNaLDate \"01-Jan-2026 00:00:00 +0000\" bOdY[HeAdEr.FiElDs (MeSsAgE-Id)] NIL)\r\nv002 OK FETCH completed\r\n";
        let messages =
            super::parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77))
                .unwrap();
        assert_eq!(messages.len(), 1);
        let key = crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "100");
        assert_eq!(messages[&key].size_bytes, Some(17));
    }

    #[test]
    fn metadata_fetch_parser_rejects_duplicate_uids() {
        let response = b"* 1 FETCH (UID 100 RFC822.SIZE 17)\r\n* 2 FETCH (UID 100 RFC822.SIZE 17)\r\nv002 OK FETCH completed\r\n";
        assert!(
            super::parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77))
                .unwrap_err()
                .contains("duplicate FETCH UID")
        );
    }

    #[test]
    fn fetch_page_coverage_rejects_missing_and_unexpected_uids() {
        let response = b"* 1 FETCH (UID 100 RFC822.SIZE 17)\r\n* 2 FETCH (UID 101 RFC822.SIZE 17)\r\nv002 OK FETCH completed\r\n";
        let messages =
            super::parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77))
                .unwrap();
        assert!(
            super::validate_fetch_page_coverage(&messages, &[100, 102], "host", "INBOX")
                .unwrap_err()
                .contains("FETCH coverage mismatch")
        );
    }

    #[test]
    fn message_fetch_budget_honors_operator_cancellation() {
        let cancelled = AtomicBool::new(true);
        let budget = MessageFetchBudget::new(Duration::from_secs(60), &cancelled);
        assert!(budget.check().unwrap_err().contains("cancelled"));
    }
}

#[cfg(test)]
mod resume_tests {
    //! Restart state machine for durable verification staging. Each test
    //! reproduces what a killed controller leaves in the stage, changes the
    //! server between runs, and drives the real fetch path against a scripted
    //! IMAP server. The invariant under test: after a scan the staged folder
    //! equals the server's current folder exactly, whatever happened between
    //! runs, and unchanged pages are not fetched again.
    use super::*;
    use crate::core::{
        ExtractedMessage, ExtractedMessages, FolderSnapshot, MailboxMessageKey,
        MessageMetadataStage, StagedMessageSide,
    };
    use std::io::{self, Read, Write};
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    const SIDE: StagedMessageSide = StagedMessageSide::Destination;
    const PAGE: u64 = MESSAGE_FETCH_PAGE_SIZE;

    /// Minimal scripted IMAP server for one folder: answers SELECT, UID
    /// SEARCH, and UID FETCH (metadata and optional BODY[]) from an in-memory
    /// UID set, recording every UID it was asked to fetch.
    struct FolderServer {
        uids: Vec<u64>,
        uidvalidity: u64,
        uidnext: u64,
        fetched: Vec<u64>,
        fetch_commands: usize,
        input: Vec<u8>,
        output: Vec<u8>,
    }

    impl FolderServer {
        fn new(uids: &[u64], uidvalidity: u64, uidnext: u64) -> Self {
            Self {
                uids: uids.to_vec(),
                uidvalidity,
                uidnext,
                fetched: Vec::new(),
                fetch_commands: 0,
                input: Vec::new(),
                output: Vec::new(),
            }
        }

        fn snapshot(&self) -> FolderSnapshot {
            FolderSnapshot {
                uidvalidity: self.uidvalidity,
                uidnext: self.uidnext,
                exists: self.uids.len() as u64,
            }
        }

        fn respond(&mut self, line: &str) {
            let (tag, command) = line.split_once(' ').unwrap();
            let reply = if command.starts_with("SELECT ") {
                format!(
                    "* {} EXISTS\r\n* OK [UIDVALIDITY {}] ok\r\n* OK [UIDNEXT {}] ok\r\n{tag} OK SELECT completed\r\n",
                    self.uids.len(),
                    self.uidvalidity,
                    self.uidnext
                )
            } else if let Some(range) = command.strip_prefix("UID SEARCH UID ") {
                let (low, high) = range.split_once(':').unwrap();
                let (low, high) = (low.parse::<u64>().unwrap(), high.parse::<u64>().unwrap());
                let found = self
                    .uids
                    .iter()
                    .filter(|uid| (low..=high).contains(*uid))
                    .map(u64::to_string)
                    .collect::<Vec<_>>();
                format!(
                    "* SEARCH {}\r\n{tag} OK SEARCH completed\r\n",
                    found.join(" ")
                )
            } else if let Some(rest) = command.strip_prefix("UID FETCH ") {
                let set = rest.split_whitespace().next().unwrap();
                let with_body = rest.contains("BODY.PEEK[]");
                self.fetch_commands += 1;
                let mut reply = String::new();
                let requested = set.split(',').flat_map(|item| {
                    let (low, high) = item.split_once(':').unwrap_or((item, item));
                    low.parse::<u64>().unwrap()..=high.parse::<u64>().unwrap()
                });
                for uid in requested {
                    self.fetched.push(uid);
                    let sequence = self.uids.iter().position(|value| *value == uid).unwrap() + 1;
                    let header = format!("Message-ID: <{uid}@example.test>\r\n\r\n");
                    let body = if with_body {
                        let body = format!("body of {uid}");
                        format!(" BODY[] {{{}}}\r\n{body}", body.len())
                    } else {
                        String::new()
                    };
                    reply.push_str(&format!(
                        "* {sequence} FETCH (UID {uid} RFC822.SIZE 10 INTERNALDATE \"01-Jan-2024 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}}}\r\n{header}{body})\r\n",
                        header.len()
                    ));
                }
                reply.push_str(&format!("{tag} OK FETCH completed\r\n"));
                reply
            } else {
                panic!("unexpected command {line}");
            };
            self.output.extend_from_slice(reply.as_bytes());
        }
    }

    impl Write for FolderServer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.input.extend_from_slice(bytes);
            while let Some(end) = self.input.windows(2).position(|pair| pair == b"\r\n") {
                let line = String::from_utf8(self.input.drain(..end + 2).collect()).unwrap();
                self.respond(line.trim_end());
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Read for FolderServer {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = buffer.len().min(self.output.len());
            buffer[..count].copy_from_slice(&self.output[..count]);
            self.output.drain(..count);
            Ok(count)
        }
    }

    fn scan_with(
        stage: &mut MessageMetadataStage,
        server: &mut FolderServer,
        body_hash: bool,
    ) -> u64 {
        let cancelled = AtomicBool::new(false);
        let budget = MessageFetchBudget::new(Duration::from_secs(10), &cancelled);
        let state_budget = MessageStateBudget::new();
        let body_budget = BodyHashBudget::new(1 << 30);
        let body_options = BodyHashOptions {
            max_body_bytes: 1 << 20,
            budget: &body_budget,
        };
        let count = stage.count(SIDE).unwrap() as usize;
        let mut sink = StageMessageSink {
            stage,
            side: SIDE,
            count,
        };
        fetch_mailbox_with_stability_retry(
            server,
            "imap.example.test",
            "INBOX",
            &budget,
            &state_budget,
            body_hash.then_some(&body_options),
            &mut sink,
        )
        .unwrap_or_else(|error| panic!("scan failed: {error:?}"))
    }

    fn scan(stage: &mut MessageMetadataStage, server: &mut FolderServer) -> u64 {
        scan_with(stage, server, false)
    }

    fn staged_uids(stage: &MessageMetadataStage) -> Vec<u64> {
        let mut uids = stage
            .all_messages(SIDE)
            .unwrap()
            .keys()
            .map(|key| key.uid.parse::<u64>().unwrap())
            .collect::<Vec<_>>();
        uids.sort_unstable();
        uids
    }

    fn staged_page(uidvalidity: u64, uids: impl IntoIterator<Item = u64>) -> ExtractedMessages {
        uids.into_iter()
            .map(|uid| {
                (
                    MailboxMessageKey::with_uidvalidity("INBOX", uidvalidity, uid.to_string()),
                    ExtractedMessage {
                        message_id: Some(format!("<{uid}@example.test>")),
                        uid: Some(uid.to_string()),
                        size_bytes: Some(10),
                        internal_date: Some("01-Jan-2024 00:00:00 +0000".into()),
                    },
                )
            })
            .collect()
    }

    /// Leave the stage exactly as a controller killed mid-folder would:
    /// the snapshot is recorded, `staged` pages are committed, and the
    /// cursor reaches `checkpoint` (none if the crash preceded it).
    fn interrupted(
        server: &FolderServer,
        staged: impl IntoIterator<Item = u64>,
        checkpoint: Option<u64>,
    ) -> MessageMetadataStage {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        let snapshot = server.snapshot();
        assert_eq!(stage.resume_mailbox(SIDE, "INBOX", snapshot).unwrap(), None);
        stage
            .insert_messages(SIDE, &staged_page(snapshot.uidvalidity, staged))
            .unwrap();
        if let Some(last_uid) = checkpoint {
            stage
                .checkpoint_page(SIDE, "INBOX", snapshot, last_uid)
                .unwrap();
        }
        stage
    }

    fn folder(count: u64) -> Vec<u64> {
        (1..=count).collect()
    }

    #[test]
    fn large_folder_inventory_uses_few_adaptive_fetch_round_trips() {
        // Contiguous and sparse UID layouts: each UID is fetched exactly once
        // and pages grow well past the old fixed 32-UID size.
        for uids in [
            folder(5_000),
            (1..=5_000).map(|uid| uid * 2).collect::<Vec<_>>(),
        ] {
            let uidnext = uids.last().unwrap() + 1;
            let mut server = FolderServer::new(&uids, 9, uidnext);
            let mut stage = MessageMetadataStage::open_in_memory().unwrap();
            assert_eq!(scan(&mut stage, &mut server), uids.len() as u64);
            let mut fetched = server.fetched.clone();
            fetched.sort_unstable();
            assert!(fetched == uids, "every UID fetched exactly once");
            assert!(
                server.fetch_commands <= 12,
                "adaptive FETCH round-trip bound exceeded"
            );
            debug_assert!(server.fetch_commands <= 12);
        }
    }

    #[test]
    fn unchanged_resume_fetches_only_pages_after_the_cursor() {
        let uids = folder(PAGE * 2 + 5);
        let before = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        let mut stage = interrupted(&before, 1..=PAGE, Some(PAGE));
        let mut server = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        assert_eq!(scan(&mut stage, &mut server), uids.len() as u64);
        assert!(server.fetched.iter().all(|uid| *uid > PAGE));
        assert_eq!(staged_uids(&stage), uids);
    }

    #[test]
    fn append_between_runs_rescans_from_zero() {
        let uids = folder(PAGE * 2 + 5);
        let before = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        let mut stage = interrupted(&before, 1..=PAGE, Some(PAGE));
        let current = folder(PAGE * 2 + 6);
        let mut server = FolderServer::new(&current, 9, PAGE * 2 + 7);
        scan(&mut stage, &mut server);
        assert!(server.fetched.contains(&1));
        assert_eq!(staged_uids(&stage), current);
    }

    #[test]
    fn expunge_between_runs_drops_the_staged_row() {
        let uids = folder(PAGE * 2 + 5);
        let before = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        let mut stage = interrupted(&before, 1..=PAGE, Some(PAGE));
        let current = uids
            .iter()
            .copied()
            .filter(|uid| *uid != 5)
            .collect::<Vec<_>>();
        let mut server = FolderServer::new(&current, 9, PAGE * 2 + 6);
        scan(&mut stage, &mut server);
        assert_eq!(staged_uids(&stage), current);
    }

    #[test]
    fn expunge_and_append_with_unchanged_exists_rescans_from_zero() {
        let uids = folder(PAGE * 2 + 5);
        let before = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        let mut stage = interrupted(&before, 1..=PAGE, Some(PAGE));
        let current = (1..=PAGE * 2 + 6)
            .filter(|uid| *uid != 5)
            .collect::<Vec<_>>();
        assert_eq!(current.len(), uids.len());
        let mut server = FolderServer::new(&current, 9, PAGE * 2 + 7);
        scan(&mut stage, &mut server);
        assert_eq!(staged_uids(&stage), current);
    }

    #[test]
    fn changed_uidvalidity_discards_the_old_generation() {
        let uids = folder(PAGE + 3);
        let before = FolderServer::new(&uids, 9, PAGE + 4);
        let mut stage = interrupted(&before, 1..=PAGE, Some(PAGE));
        let mut server = FolderServer::new(&uids, 10, PAGE + 4);
        scan(&mut stage, &mut server);
        let messages = stage.all_messages(SIDE).unwrap();
        assert_eq!(messages.len(), uids.len());
        assert!(messages.keys().all(|key| key.uidvalidity == Some(10)));
    }

    #[test]
    fn crash_after_page_insert_before_cursor_refetches_that_page_once() {
        let uids = folder(PAGE * 2 + 5);
        let before = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        let mut stage = interrupted(&before, 1..=PAGE, None);
        let mut server = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        scan(&mut stage, &mut server);
        assert!(server.fetched == uids, "every UID fetched exactly once");
        assert_eq!(staged_uids(&stage), uids);
    }

    #[test]
    fn completed_folder_restart_reuses_an_unchanged_stage() {
        let uids = folder(PAGE + 3);
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        scan(&mut stage, &mut FolderServer::new(&uids, 9, PAGE + 4));
        let mut server = FolderServer::new(&uids, 9, PAGE + 4);
        assert_eq!(scan(&mut stage, &mut server), uids.len() as u64);
        assert!(server.fetched.is_empty());
        assert_eq!(staged_uids(&stage), uids);
    }

    #[test]
    fn resumed_scan_of_a_completed_folder_fetches_new_arrivals() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        assert_eq!(
            scan(&mut stage, &mut FolderServer::new(&[1, 2, 3], 9, 4)),
            3
        );
        // One message replaced by another keeps EXISTS unchanged.
        assert_eq!(
            scan(&mut stage, &mut FolderServer::new(&[1, 3, 4], 9, 5)),
            3
        );
        assert_eq!(staged_uids(&stage), vec![1, 3, 4]);
    }

    #[test]
    fn resume_after_expunge_and_delivery_with_equal_exists_rescans_the_folder() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        let original = (1..=32).collect::<Vec<_>>();
        assert_eq!(
            scan(&mut stage, &mut FolderServer::new(&original, 42, 33)),
            32
        );
        // Between runs UID 10 is expunged and UID 33 delivered: EXISTS is
        // still 32, so only the persisted snapshot distinguishes the folders.
        let current = (1..=33).filter(|uid| *uid != 10).collect::<Vec<_>>();
        assert_eq!(
            scan(&mut stage, &mut FolderServer::new(&current, 42, 34)),
            32
        );
        assert_eq!(staged_uids(&stage), current);
    }

    #[test]
    fn body_hash_resume_restores_fingerprints_for_partially_staged_pages() {
        let uids = folder(PAGE * 2 + 5);
        let before = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        // The crash landed after page rows committed but before their
        // fingerprints and the cursor were written.
        let mut stage = interrupted(&before, 1..=PAGE, None);
        assert!(stage.content_fingerprints(SIDE).is_empty());
        let mut server = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        scan_with(&mut stage, &mut server, true);
        assert_eq!(staged_uids(&stage), uids);
        assert_eq!(stage.content_fingerprints(SIDE).len(), uids.len());
    }

    #[test]
    fn body_hash_resume_keeps_fingerprints_of_checkpointed_pages() {
        let uids = folder(PAGE * 2 + 5);
        let mut first = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        scan_with(&mut stage, &mut first, true);
        let fingerprints = stage.content_fingerprints(SIDE);
        assert_eq!(fingerprints.len(), uids.len());
        let mut server = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        scan_with(&mut stage, &mut server, true);
        assert!(server.fetched.is_empty());
        assert_eq!(stage.content_fingerprints(SIDE), fingerprints);
    }
}
