use crate::core;
use crate::credentials::SecretString;
use crate::imap_protocol::{
    advertises_capability, atom_eq, is_tagged_response, is_untagged_response,
};
use crate::oauth::{read_auth_continuation_with_deadline, read_auth_result_with_deadline};
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::{Duration, Instant},
};

#[path = "imap_probe/fetch_parser.rs"]
mod fetch_parser;
use fetch_parser::parse_message_fetch_body_hashes_response_bytes;
use fetch_parser::parse_message_fetch_metadata_response_bytes_with_mailbox;
#[cfg(test)]
use fetch_parser::{parse_message_fetch_metadata_response_bytes, parse_message_id_header};

const DNS_RESOLVER_WORKERS: usize = 4;
const DNS_RESOLVER_QUEUE: usize = 32;
const MAX_DNS_ADDRESSES: usize = 64;
const BUDGETED_IMAP_IO_SLICE: Duration = Duration::from_millis(250);

struct DnsResolverRequest {
    address: String,
    result: mpsc::Sender<std::io::Result<Vec<SocketAddr>>>,
}

struct DnsResolverPool {
    requests: mpsc::SyncSender<DnsResolverRequest>,
}

static DNS_RESOLVER_POOL: OnceLock<Result<DnsResolverPool, String>> = OnceLock::new();

impl DnsResolverPool {
    fn new() -> Result<Self, String> {
        let (requests, receiver) = mpsc::sync_channel::<DnsResolverRequest>(DNS_RESOLVER_QUEUE);
        let receiver = Arc::new(Mutex::new(receiver));
        for worker_number in 0..DNS_RESOLVER_WORKERS {
            let receiver = Arc::clone(&receiver);
            std::thread::Builder::new()
                .name(format!("mailswiftsync-dns-{worker_number}"))
                .spawn(move || {
                    loop {
                        let request = match receiver.lock() {
                            Ok(receiver) => receiver.recv(),
                            Err(_) => return,
                        };
                        let Ok(request) = request else {
                            return;
                        };
                        let result = request
                            .address
                            .to_socket_addrs()
                            .and_then(collect_dns_addresses);
                        let _ = request.result.send(result);
                    }
                })
                .map_err(|error| format!("could not start DNS resolver worker: {error}"))?;
        }
        Ok(Self { requests })
    }

    fn resolve(
        &self,
        address: String,
    ) -> Result<mpsc::Receiver<std::io::Result<Vec<SocketAddr>>>, String> {
        let (result, receiver) = mpsc::channel();
        self.requests
            .try_send(DnsResolverRequest { address, result })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "DNS resolver queue is full".to_owned(),
                mpsc::TrySendError::Disconnected(_) => "DNS resolver pool stopped".to_owned(),
            })?;
        Ok(receiver)
    }
}

fn collect_dns_addresses<I>(mut addresses: I) -> std::io::Result<Vec<SocketAddr>>
where
    I: Iterator<Item = SocketAddr>,
{
    let mut collected = Vec::with_capacity(MAX_DNS_ADDRESSES);
    for _ in 0..MAX_DNS_ADDRESSES {
        let Some(address) = addresses.next() else {
            return Ok(collected);
        };
        collected.push(address);
    }
    if addresses.next().is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("DNS response exceeded the {MAX_DNS_ADDRESSES}-address limit"),
        ));
    }
    Ok(collected)
}

fn dns_resolver_pool() -> Result<&'static DnsResolverPool, String> {
    DNS_RESOLVER_POOL
        .get_or_init(|| DnsResolverPool::new().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(Clone::clone)
}

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

/// Incrementally locate a tagged completion line without interpreting bytes
/// inside IMAP literals as protocol framing. The scanner owns only offsets and
/// literal state; it never rescans bytes already consumed, so a growing FETCH
/// response is processed in O(n) time across all socket reads.
struct TaggedResponseScanner<'a> {
    tag: &'a [u8],
    cursor: usize,
    line_start: usize,
    literal_remaining: usize,
}

impl<'a> TaggedResponseScanner<'a> {
    fn new(tag: &'a str) -> Self {
        Self {
            tag: tag.as_bytes(),
            cursor: 0,
            line_start: 0,
            literal_remaining: 0,
        }
    }

    fn scan(&mut self, response: &[u8]) -> bool {
        while self.cursor < response.len() {
            if self.literal_remaining > 0 {
                let consumed = self
                    .literal_remaining
                    .min(response.len().saturating_sub(self.cursor));
                self.cursor += consumed;
                self.literal_remaining -= consumed;
                if self.literal_remaining == 0 {
                    // Literal bytes are payload, not part of the following
                    // protocol line. Multiple literals may occur in one
                    // FETCH response line, so resume line accounting here.
                    self.line_start = self.cursor;
                }
                continue;
            }

            if response[self.cursor] != b'\r' {
                self.cursor += 1;
                continue;
            }
            if self.cursor + 1 >= response.len() {
                // Keep a trailing CR for the next socket read, when the LF
                // completing this line may arrive in a separate chunk.
                break;
            }
            if response[self.cursor + 1] != b'\n' {
                self.cursor += 1;
                continue;
            }

            let line_end = self.cursor;
            let line = &response[self.line_start..line_end];
            if line.starts_with(self.tag)
                && line
                    .get(self.tag.len())
                    .is_some_and(|byte| byte.is_ascii_whitespace())
            {
                return true;
            }
            self.literal_remaining = parse_imap_literal_length(line).unwrap_or(0);
            self.cursor += 2;
            self.line_start = self.cursor;
        }
        false
    }
}

fn parse_imap_literal_length(line: &[u8]) -> Option<usize> {
    line.strip_suffix(b"}")
        .and_then(|line| line.iter().rposition(|byte| *byte == b'{'))
        .and_then(|start| std::str::from_utf8(&line[start + 1..line.len() - 1]).ok())
        .and_then(|length| {
            length
                .strip_suffix('+')
                .unwrap_or(length)
                .parse::<usize>()
                .ok()
        })
}

/// Locate a tagged completion line without interpreting bytes inside IMAP
/// literals as protocol framing. Literal payloads are arbitrary octets and
/// may contain lines that look exactly like the command tag.
#[cfg(test)]
fn tagged_response_outside_literals(response: &[u8], tag: &str) -> bool {
    TaggedResponseScanner::new(tag).scan(response)
}

const MAX_IMAP_LIST_LINE_BYTES: usize = 64 * 1024;
const MAX_IMAP_LIST_LITERAL_BYTES: usize = 1024 * 1024;
const MAX_IMAP_LIST_MAILBOXES: usize = 100_000;
const MAX_IMAP_LIST_INVENTORY_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAP_LIST_DURATION: Duration = Duration::from_secs(60);
const MAX_IMAP_COMMAND_DURATION: Duration = Duration::from_secs(15);
const MAX_MESSAGE_FETCH_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MESSAGE_FETCH_PAGE_SIZE: u64 = 32;
const MESSAGE_UID_SEARCH_WINDOW_SIZE: u64 = 10_000;
const MAX_MESSAGE_FETCH_RECORDS: usize = 1_000_000;
// This is a fail-closed bound for one fetched page's transient Rust state. Live
// metadata is staged into SQLite immediately after parsing, so the bound is
// released after each staged page rather than accumulating with account size.
const MAX_ESTIMATED_FETCHED_STATE_BYTES: usize = 256 * 1024 * 1024;
const MAX_MAILBOX_STABILITY_ATTEMPTS: usize = 2;

pub(crate) struct MessageFetchBudget<'a> {
    deadline: Instant,
    cancel: &'a AtomicBool,
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
        self.stage.resume_mailbox(self.side, mailbox, snapshot)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MailboxDescriptor {
    pub(crate) wire_name: String,
    pub(crate) delimiter: Option<String>,
    pub(crate) special_use: Vec<String>,
    pub(crate) selectable: bool,
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
    read_imap_list_response_inner(stream, tag, buffer, None, None)
}

#[cfg(test)]
fn read_imap_list_response_with_mailboxes<S: Read>(
    stream: &mut S,
    tag: &str,
    buffer: &mut [u8; 4096],
    mailboxes: &mut Vec<String>,
) -> Result<ListInventorySummary, String> {
    let mut details = Vec::new();
    let summary = read_imap_list_response_inner(stream, tag, buffer, Some(&mut details), None)?;
    mailboxes.extend(
        details
            .into_iter()
            .filter(|mailbox| mailbox.selectable)
            .map(|mailbox| mailbox.wire_name),
    );
    Ok(summary)
}

fn read_imap_list_response_with_details<S: Read>(
    stream: &mut S,
    tag: &str,
    buffer: &mut [u8; 4096],
    mailboxes: &mut Vec<MailboxDescriptor>,
    budget: &MessageFetchBudget<'_>,
) -> Result<ListInventorySummary, String> {
    read_imap_list_response_inner(stream, tag, buffer, Some(mailboxes), Some(budget))
}

fn read_imap_list_response_inner<S: Read>(
    stream: &mut S,
    tag: &str,
    buffer: &mut [u8; 4096],
    mut mailbox_details: Option<&mut Vec<MailboxDescriptor>>,
    budget: Option<&MessageFetchBudget<'_>>,
) -> Result<ListInventorySummary, String> {
    let mut line = Vec::new();
    let started = Instant::now();
    let mut last_read = Instant::now();
    let mut literal_remaining = 0_usize;
    let mut literal_separator_remaining = 0_u8;
    let mut literal_header: Option<String> = None;
    let mut literal_value = Vec::new();
    let mut summary = ListInventorySummary::default();
    let mut inventory_bytes = 0_usize;
    const MAX_INTER_READ_STALL: Duration = Duration::from_secs(15);
    let deadline = started + MAX_IMAP_LIST_DURATION;
    loop {
        if let Some(budget) = budget {
            budget.check()?;
        }
        if last_read.elapsed() > MAX_INTER_READ_STALL {
            return Err("IMAP LIST response stalled (no data received for 15 seconds)".into());
        }
        let budget_deadline = budget.map_or(deadline, |budget| budget.deadline);
        let read_deadline = (last_read + MAX_INTER_READ_STALL)
            .min(deadline)
            .min(budget_deadline);
        let count = read_with_deadline(
            stream,
            buffer,
            read_deadline,
            budget.map(|budget| budget.cancel),
        )?;
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
                literal_value.extend_from_slice(&buffer[offset..offset + consumed]);
                literal_remaining -= consumed;
                offset += consumed;
                if literal_remaining == 0 {
                    if let Some(header) = literal_header.take() {
                        record_list_entry(
                            &header,
                            Some(&literal_value),
                            &mut summary,
                            &mut mailbox_details,
                            &mut inventory_bytes,
                        )?;
                    }
                    literal_value.clear();
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
            let literal_size = list_literal_size(text);
            if is_untagged_response(text, "LIST") && literal_size.is_none() {
                record_list_entry(
                    text,
                    None,
                    &mut summary,
                    &mut mailbox_details,
                    &mut inventory_bytes,
                )?;
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
                if is_untagged_response(text, "LIST") {
                    literal_header = Some(text.to_owned());
                    literal_value.clear();
                }
                literal_remaining = literal_size;
            }
            line.clear();
        }
    }
}

/// Parse the final mailbox-name atom from a normal, non-literal LIST record.
/// Literal names are handled by `record_list_entry` after the bounded literal
/// bytes have been collected; this function deliberately handles only the
/// quoted/atom form.
fn record_list_entry(
    line: &str,
    literal_name: Option<&[u8]>,
    summary: &mut ListInventorySummary,
    mailbox_details: &mut Option<&mut Vec<MailboxDescriptor>>,
    inventory_bytes: &mut usize,
) -> Result<(), String> {
    if !is_untagged_response(line, "LIST") {
        return Ok(());
    }
    summary.mailbox_count = summary.mailbox_count.saturating_add(1);
    if summary.mailbox_count > MAX_IMAP_LIST_MAILBOXES {
        return Err(format!(
            "IMAP LIST response exceeded the {MAX_IMAP_LIST_MAILBOXES}-mailbox limit"
        ));
    }
    let special_use = list_special_use(line);
    if !special_use.is_empty() {
        summary.special_use_mailboxes = summary.special_use_mailboxes.saturating_add(1);
    }
    let selectable = !crate::imap_protocol::list_has_attribute(line, r"\NOSELECT");
    if selectable {
        summary.selectable_mailbox_count = summary.selectable_mailbox_count.saturating_add(1);
    }
    if let Some(details) = mailbox_details.as_deref_mut() {
        let wire_name = match literal_name {
            Some(bytes) => String::from_utf8(bytes.to_vec())
                .map_err(|_| "IMAP LIST mailbox literal was not valid UTF-8".to_owned())?,
            None => parse_list_mailbox_name(line)
                .ok_or_else(|| "IMAP LIST mailbox name was missing or malformed".to_owned())?,
        };
        let descriptor_bytes = wire_name
            .len()
            .saturating_add(parse_list_delimiter(line).as_ref().map_or(0, String::len))
            .saturating_add(special_use.iter().map(String::len).sum::<usize>())
            // Account for the descriptor/vector and string allocation
            // overhead in addition to the retained character data.
            .saturating_add(256);
        *inventory_bytes = inventory_bytes.saturating_add(descriptor_bytes);
        if *inventory_bytes > MAX_IMAP_LIST_INVENTORY_BYTES {
            return Err(format!(
                "IMAP LIST inventory exceeded the {MAX_IMAP_LIST_INVENTORY_BYTES}-byte limit"
            ));
        }
        details.push(MailboxDescriptor {
            wire_name,
            delimiter: parse_list_delimiter(line),
            special_use,
            selectable,
        });
    }
    Ok(())
}

fn parse_list_mailbox_name(line: &str) -> Option<String> {
    let tokens = parse_list_tokens(line)?;
    tokens
        .last()
        .filter(|value| !value.starts_with('{'))
        .cloned()
}

fn parse_list_delimiter(line: &str) -> Option<String> {
    let tokens = parse_list_tokens(line)?;
    let mut index = 2;
    if tokens.get(index)?.starts_with('(') {
        while !tokens.get(index)?.ends_with(')') {
            index += 1;
        }
        index += 1;
    }
    let delimiter = tokens.get(index)?;
    (!delimiter.is_empty()).then(|| delimiter.to_owned())
}

/// Tokenize the bounded, non-literal portion of an IMAP LIST response.
/// Quoted strings may contain UTF-8, whitespace, and escaped quote/backslash
/// bytes; decode the complete token only after processing quoted pairs.
fn parse_list_tokens(line: &str) -> Option<Vec<String>> {
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
            offset += 1;
        }
        if offset == bytes.len() {
            break;
        }
        if bytes[offset] == b'"' {
            offset += 1;
            let mut value = Vec::new();
            let mut closed = false;
            while offset < bytes.len() {
                match bytes[offset] {
                    b'\\' => {
                        offset += 1;
                        value.push(*bytes.get(offset)?);
                        offset += 1;
                    }
                    b'"' => {
                        offset += 1;
                        closed = true;
                        break;
                    }
                    byte => {
                        value.push(byte);
                        offset += 1;
                    }
                }
            }
            if !closed {
                return None;
            }
            tokens.push(String::from_utf8(value).ok()?);
        } else {
            let start = offset;
            while bytes
                .get(offset)
                .is_some_and(|byte| !byte.is_ascii_whitespace())
            {
                offset += 1;
            }
            tokens.push(std::str::from_utf8(&bytes[start..offset]).ok()?.to_owned());
        }
    }
    Some(tokens)
}

fn list_special_use(line: &str) -> Vec<String> {
    [
        (r"\ALL", "all"),
        (r"\ARCHIVE", "archive"),
        (r"\DRAFTS", "drafts"),
        (r"\JUNK", "junk"),
        (r"\SENT", "sent"),
        (r"\TRASH", "trash"),
    ]
    .into_iter()
    .filter(|(attribute, _)| crate::imap_protocol::list_has_attribute(line, attribute))
    .map(|(_, kind)| kind.to_owned())
    .collect()
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
    let resolver = dns_resolver_pool()?.resolve(address)?;
    let dns_deadline = budget
        .map(|budget| budget.deadline.min(Instant::now() + Duration::from_secs(8)))
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(8));
    let sockets = loop {
        if budget.is_some_and(|budget| budget.cancel.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err(format!("{host}: DNS resolution cancelled"));
        }
        let remaining = dns_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("{host}: DNS resolution timed out"));
        }
        match resolver.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(result) => break result.map_err(|error| format!("{host}: {error}"))?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!("{host}: DNS resolver stopped unexpectedly"));
            }
        }
    };
    if sockets.is_empty() {
        return Err(format!("{host}: no address found"));
    }
    let mut last_error = None;
    let mut tcp = None;
    let connect_deadline = budget
        .map(|budget| budget.deadline.min(Instant::now() + Duration::from_secs(8)))
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(8));
    for socket in sockets {
        if budget.is_some_and(|budget| budget.cancel.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err(format!("{host}: connection cancelled"));
        }
        let remaining = connect_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(&socket, remaining) {
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
    let name = ServerName::try_from(server_name.to_owned())
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
        |stream, buffer, uid_page| {
            budget.check()?;
            let tag = format!("v{:03}", page_number + 3);
            page_number = page_number.saturating_add(1);
            let uid_set = uid_page
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
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
            let response = String::from_utf8_lossy(&raw_response);
            if !imap_command_succeeded(&response, &tag) {
                return Err(imap_command_failure(
                    &response,
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
            Ok(())
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
    mut consume_page: F,
) -> Result<u64, String>
where
    F: FnMut(&mut S, &mut [u8; 4096], &[u64]) -> Result<(), String>,
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
        if !imap_command_succeeded(response, &search_tag) {
            return Err(imap_command_failure(
                response,
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
        for uid_page in uids.chunks(MESSAGE_FETCH_PAGE_SIZE as usize) {
            if resume_after_uid
                .is_none_or(|resume| uid_page.last().copied().is_none_or(|last| last > resume))
            {
                consume_page(stream, buffer, uid_page)?;
            }
        }
        window_start = window_end.saturating_add(1);
    }
    Ok(searched_uid_count)
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
    let count = stage.count(side).map_err(|error| error.to_string())? as usize;
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
        ListInventorySummary, MAX_DNS_ADDRESSES, MAX_ESTIMATED_FETCHED_STATE_BYTES,
        MAX_IMAP_LIST_INVENTORY_BYTES, MailboxFetchError, MessageFetchBudget, MessageStateBudget,
        StateReservation, TaggedResponseScanner, authenticated_list_command,
        classify_mailbox_fetch_error, dns_resolver_pool, format_folder_failures,
        parse_list_delimiter, parse_list_mailbox_name, parse_message_fetch_metadata_response_bytes,
        parse_message_id_header, read_imap_list_response, read_imap_list_response_with_mailboxes,
        read_with_deadline, record_list_entry, tagged_response_outside_literals,
        write_imap_command,
    };
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    use std::{
        io::{self, Cursor, Read, Write},
        net::SocketAddr,
    };

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
    fn dns_resolution_uses_the_bounded_shared_pool() {
        let receiver = dns_resolver_pool()
            .unwrap()
            .resolve("127.0.0.1:993".to_owned())
            .unwrap();
        let addresses = receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(addresses.iter().any(|address| address.ip().is_loopback()));
    }

    #[test]
    fn dns_address_collection_rejects_excessive_responses() {
        let addresses =
            (0..=MAX_DNS_ADDRESSES).map(|port| SocketAddr::from(([127, 0, 0, 1], port as u16)));
        let error = super::collect_dns_addresses(addresses).unwrap_err();
        assert!(error.to_string().contains("address limit"));
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
                let mut reply = String::new();
                for uid in set.split(',').map(|uid| uid.parse::<u64>().unwrap()) {
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
    fn unchanged_resume_fetches_only_pages_after_the_cursor() {
        let uids = folder(PAGE * 2 + 5);
        let before = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        let mut stage = interrupted(&before, 1..=PAGE, Some(PAGE));
        let mut server = FolderServer::new(&uids, 9, PAGE * 2 + 6);
        assert_eq!(scan(&mut stage, &mut server), uids.len() as u64);
        assert!(
            server.fetched.iter().all(|uid| *uid > PAGE),
            "{:?}",
            server.fetched
        );
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
        assert_eq!(server.fetched, uids, "every UID fetched exactly once");
        assert_eq!(staged_uids(&stage), uids);
    }

    #[test]
    fn completed_folder_restart_reuses_an_unchanged_stage() {
        let uids = folder(PAGE + 3);
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        scan(&mut stage, &mut FolderServer::new(&uids, 9, PAGE + 4));
        let mut server = FolderServer::new(&uids, 9, PAGE + 4);
        assert_eq!(scan(&mut stage, &mut server), uids.len() as u64);
        assert!(server.fetched.is_empty(), "{:?}", server.fetched);
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
