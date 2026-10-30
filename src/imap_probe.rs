use crate::core;
use crate::imap_protocol::{advertises_capability, atom_eq, is_untagged_response};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

mod auth;
mod fetch_pages;
#[path = "imap_probe/fetch_parser.rs"]
mod fetch_parser;
mod folders;
mod list_parser;
mod literal_framing;
mod message_census;
#[path = "imap_probe/namespace.rs"]
mod namespace_parser;
mod protocol;
mod resolver;
mod tls;
#[path = "imap_probe/uid.rs"]
mod uid;
use auth::{authenticate_imap_stream, reconnect_and_authenticate_imap_stream};
#[cfg(test)]
use fetch_pages::parse_uid_search_response;
use fetch_pages::{
    FetchPagePlanner, encode_uid_page, parse_uid_fetch_response, validate_fetch_page_coverage,
};
use fetch_parser::canonical_flag_set;
use fetch_parser::parse_message_fetch_body_hashes_response_bytes;
use fetch_parser::parse_message_fetch_metadata_response_bytes_with_mailbox;
#[cfg(test)]
use fetch_parser::{parse_message_fetch_metadata_response_bytes, parse_message_id_header};
pub(crate) use folders::MailboxDescriptor;
#[cfg(test)]
use folders::{
    ListInventorySummary, read_imap_list_response, read_imap_list_response_with_mailboxes,
    record_list_entry,
};
use folders::{authenticated_list_command, read_imap_list_response_with_details};
#[cfg(test)]
use list_parser::tokens as parse_list_tokens;
#[cfg(test)]
use list_parser::{delimiter as parse_list_delimiter, mailbox_name as parse_list_mailbox_name};
#[cfg(test)]
use protocol::{TaggedResponseScanner, tagged_response_outside_literals};
use protocol::{
    read_imap_tagged_bytes_with_budget, read_imap_tagged_with_budget,
    read_imap_tagged_with_optional_budget, write_imap_command,
};

const TRANSIENT_IO_RETRY_BACKOFF: Duration = Duration::from_millis(5);

/// Avoid a tight spin when a stream reports WouldBlock without sleeping.
/// Callers retain their original absolute deadline and cancellation checks.
pub(crate) fn wait_for_io_retry(deadline: Instant, cancel: Option<&AtomicBool>) {
    if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
        return;
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if !remaining.is_zero() {
        std::thread::sleep(remaining.min(TRANSIENT_IO_RETRY_BACKOFF));
    }
}
use resolver::connect_racing;
use tls::connect_tls_stream_with_budget;

pub(crate) fn internal_dns_resolver_main(arguments: &[std::ffi::OsString]) -> i32 {
    resolver::internal_dns_resolver_main(arguments)
}

/// Resolve a hostname through the bounded resolver used by network probes.
/// Webhook delivery reuses this path so policy checks inspect the same bounded
/// address set that is pinned into its HTTP client.
pub(crate) fn resolve_dns_with_deadline(
    address: &str,
    deadline: std::time::Instant,
    cancelled: &dyn Fn() -> bool,
) -> std::io::Result<Vec<std::net::SocketAddr>> {
    resolver::resolve_dns_with_deadline(address, deadline, cancelled)
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

const MAX_IMAP_LIST_INVENTORY_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAP_COMMAND_DURATION: Duration = Duration::from_secs(15);
const TLS_CAPABILITY_PROBE_BUDGET: Duration = Duration::from_secs(90);
const TLS_READINESS_PROBE_BUDGET: Duration = Duration::from_secs(45);
const MAX_MESSAGE_FETCH_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// Initial metadata FETCH page; `FetchPagePlanner` adapts it per folder.
const MESSAGE_FETCH_PAGE_SIZE: u64 = 128;
const MAX_UID_SET_BYTES: usize = 7_000;
/// Sequence-number FETCH pages scale with the number of messages in a
/// mailbox, not with its historical UIDNEXT after years of expunges.
const MESSAGE_UID_ENUMERATION_PAGE_SIZE: u64 = 512;
pub(crate) const MAX_MESSAGE_FETCH_RECORDS: usize = 1_000_000;
pub(crate) const MAX_BODY_HASH_MESSAGES_PER_ENDPOINT: usize = 100_000;
// This is a fail-closed bound for one fetched page's transient Rust state. Live
// metadata is staged into SQLite immediately after parsing, so the bound is
// released after each staged page rather than accumulating with account size.
const MAX_ESTIMATED_FETCHED_STATE_BYTES: usize = 256 * 1024 * 1024;
const MAX_MAILBOX_STABILITY_ATTEMPTS: usize = 2;

fn body_hash_message_limit_exceeded(existing: usize, incoming: usize) -> bool {
    existing.saturating_add(incoming) > MAX_BODY_HASH_MESSAGES_PER_ENDPOINT
}

fn message_fetch_limit_exceeded(existing: usize, incoming: usize) -> bool {
    existing
        .checked_add(incoming)
        .is_none_or(|total| total > MAX_MESSAGE_FETCH_RECORDS)
}

pub(crate) struct MessageFetchBudget<'a> {
    pub(super) deadline: Instant,
    pub(super) cancel: &'a AtomicBool,
    verification_limits: bool,
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
            return Err(crate::core::VerificationLimit::FetchedState.tag(format!(
                "account pair exceeded the estimated {MAX_ESTIMATED_FETCHED_STATE_BYTES}-byte fetched-state admission budget"
            )));
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
            return Err(
                crate::core::VerificationLimit::BodyHashTotalSize.tag(format!(
                    "RFC822 body hashing exceeded the configured {}-byte total bound",
                    self.max_total_bytes
                )),
            );
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
    fn insert_page_and_checkpoint(
        &mut self,
        mailbox: &str,
        snapshot: crate::core::FolderSnapshot,
        last_uid: u64,
        messages: &crate::core::ExtractedMessages,
        fingerprints: &HashMap<crate::core::MailboxMessageKey, String>,
    ) -> Result<(), String> {
        self.insert_batch_with_fingerprints(messages, fingerprints)?;
        self.checkpoint_page(mailbox, snapshot, last_uid)
    }
    fn rollback_mailbox(&mut self, mailbox: &str) -> Result<(), String>;
    fn len(&self) -> usize;
    /// A durable SQLite stage can reconcile body hashes without constructing
    /// mailbox-sized in-memory indexes. Ephemeral sinks retain the stricter
    /// forensic cap because their downstream reconciler is memory-backed.
    fn has_durable_stage(&self) -> bool {
        false
    }
    /// Rows currently staged for one folder generation. Compared against the
    /// server's EXISTS after a scan to require complete coverage of the
    /// current SELECT snapshot.
    fn mailbox_len(&self, mailbox: &str, uidvalidity: u64) -> Result<u64, String>;
    fn begin_mailbox_scan(
        &mut self,
        _mailbox: &str,
        _snapshot: crate::core::FolderSnapshot,
    ) -> Result<(), String> {
        Ok(())
    }
    /// Reuse a completed folder from a durable stage only when its original
    /// snapshot can be proven unchanged. Ephemeral sinks always rescan.
    fn reuse_completed_mailbox(
        &mut self,
        _mailbox: &str,
        _snapshot: crate::core::FolderSnapshot,
    ) -> Result<bool, String> {
        Ok(false)
    }
    fn resume_mailbox_prefix(
        &mut self,
        _mailbox: &str,
        _snapshot: crate::core::FolderSnapshot,
    ) -> Result<Option<(u64, u64)>, String> {
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
    /// Record the canonical PERMANENTFLAGS a folder's SELECT advertised
    /// (`None` when the server sent none), so flag verification can tell a
    /// flag the destination cannot store from one that was lost.
    fn record_permanent_flags(
        &mut self,
        _mailbox: &str,
        _permanent_flags: Option<&str>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn retain_mailboxes(&mut self, _mailboxes: &HashSet<String>) -> Result<(), String> {
        Ok(())
    }
}

struct StageMessageSink<'a> {
    stage: &'a mut crate::core::MessageMetadataStage,
    side: crate::core::StagedMessageSide,
    count: usize,
    mailbox_counts: HashMap<String, usize>,
    active_mailbox: Option<String>,
}

impl MessageSink for StageMessageSink<'_> {
    fn insert_batch(&mut self, messages: &crate::core::ExtractedMessages) -> Result<(), String> {
        self.stage.insert_messages(self.side, messages)?;
        self.add_current_attempt_messages(messages.len())?;
        Ok(())
    }

    fn insert_batch_with_fingerprints(
        &mut self,
        messages: &crate::core::ExtractedMessages,
        fingerprints: &HashMap<crate::core::MailboxMessageKey, String>,
    ) -> Result<(), String> {
        self.stage
            .insert_messages_with_fingerprints(self.side, messages, fingerprints)?;
        self.add_current_attempt_messages(messages.len())?;
        Ok(())
    }

    fn insert_page_and_checkpoint(
        &mut self,
        mailbox: &str,
        snapshot: crate::core::FolderSnapshot,
        last_uid: u64,
        messages: &crate::core::ExtractedMessages,
        fingerprints: &HashMap<crate::core::MailboxMessageKey, String>,
    ) -> Result<(), String> {
        self.stage.insert_page_with_fingerprints(
            self.side,
            messages,
            fingerprints,
            Some((mailbox, snapshot, last_uid)),
        )?;
        self.add_current_attempt_messages(messages.len())
    }

    fn rollback_mailbox(&mut self, mailbox: &str) -> Result<(), String> {
        self.stage.delete_mailbox(self.side, mailbox)?;
        self.reset_mailbox_count(mailbox);
        if self.active_mailbox.as_deref() == Some(mailbox) {
            self.active_mailbox = None;
        }
        Ok(())
    }

    fn retain_mailboxes(&mut self, mailboxes: &HashSet<String>) -> Result<(), String> {
        self.stage.retain_mailboxes(self.side, mailboxes)?;
        self.count = 0;
        self.mailbox_counts.clear();
        self.active_mailbox = None;
        Ok(())
    }

    fn len(&self) -> usize {
        self.count
    }

    fn has_durable_stage(&self) -> bool {
        self.stage.is_durable()
    }

    fn mailbox_len(&self, mailbox: &str, uidvalidity: u64) -> Result<u64, String> {
        self.stage
            .count_mailbox(self.side, mailbox, uidvalidity)
            .map_err(|error| error.to_string())
    }

    fn record_permanent_flags(
        &mut self,
        mailbox: &str,
        permanent_flags: Option<&str>,
    ) -> Result<(), String> {
        self.stage
            .record_permanent_flags(self.side, mailbox, permanent_flags)
    }

    fn begin_mailbox_scan(
        &mut self,
        mailbox: &str,
        snapshot: crate::core::FolderSnapshot,
    ) -> Result<(), String> {
        self.stage
            .begin_mailbox_scan(self.side, mailbox, snapshot)?;
        self.reset_mailbox_count(mailbox);
        self.active_mailbox = Some(mailbox.to_owned());
        Ok(())
    }

    fn reuse_completed_mailbox(
        &mut self,
        mailbox: &str,
        snapshot: crate::core::FolderSnapshot,
    ) -> Result<bool, String> {
        let Some(count) = self
            .stage
            .reusable_completed_mailbox(self.side, mailbox, snapshot)?
        else {
            return Ok(false);
        };
        let count = usize::try_from(count)
            .map_err(|_| "reused verification folder count exceeds this platform".to_owned())?;
        if message_fetch_limit_exceeded(self.count, count) {
            return Err(crate::core::VerificationLimit::MessageCount.tag(format!(
                "account would exceed the {MAX_MESSAGE_FETCH_RECORDS}-message verification limit while reusing a completed folder"
            )));
        }
        self.reset_mailbox_count(mailbox);
        self.active_mailbox = Some(mailbox.to_owned());
        self.add_current_attempt_messages(count)?;
        Ok(true)
    }

    fn resume_mailbox_prefix(
        &mut self,
        mailbox: &str,
        snapshot: crate::core::FolderSnapshot,
    ) -> Result<Option<(u64, u64)>, String> {
        let Some((count, last_uid)) = self
            .stage
            .resumable_mailbox_prefix(self.side, mailbox, snapshot)?
        else {
            return Ok(None);
        };
        let count = usize::try_from(count)
            .map_err(|_| "resumed verification prefix exceeds this platform".to_owned())?;
        if message_fetch_limit_exceeded(self.count, count) {
            return Err(crate::core::VerificationLimit::MessageCount.tag(format!(
                "account would exceed the {MAX_MESSAGE_FETCH_RECORDS}-message verification limit while resuming a folder"
            )));
        }
        self.reset_mailbox_count(mailbox);
        self.active_mailbox = Some(mailbox.to_owned());
        self.add_current_attempt_messages(count)?;
        Ok(Some((
            u64::try_from(count).map_err(|error| error.to_string())?,
            last_uid,
        )))
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

impl StageMessageSink<'_> {
    fn add_current_attempt_messages(&mut self, count: usize) -> Result<(), String> {
        let mailbox = self
            .active_mailbox
            .clone()
            .ok_or_else(|| "message page arrived before a folder scan began".to_owned())?;
        let total = self
            .count
            .checked_add(count)
            .ok_or_else(|| "verification message count overflowed".to_owned())?;
        let mailbox_count = self
            .mailbox_counts
            .get(&mailbox)
            .copied()
            .unwrap_or_default()
            .checked_add(count)
            .ok_or_else(|| "verification folder message count overflowed".to_owned())?;
        self.count = total;
        self.mailbox_counts.insert(mailbox, mailbox_count);
        Ok(())
    }

    fn reset_mailbox_count(&mut self, mailbox: &str) {
        if let Some(previous) = self.mailbox_counts.remove(mailbox) {
            self.count = self.count.saturating_sub(previous);
        }
        self.mailbox_counts.insert(mailbox.to_owned(), 0);
    }
}

impl<'a> MessageFetchBudget<'a> {
    pub(crate) fn new(timeout: Duration, cancel: &'a AtomicBool) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            cancel,
            verification_limits: true,
        }
    }

    pub(crate) fn for_probe(timeout: Duration, cancel: &'a AtomicBool) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            cancel,
            verification_limits: false,
        }
    }

    pub(super) fn limit_error(
        &self,
        limit: crate::core::VerificationLimit,
        detail: String,
    ) -> String {
        if self.verification_limits {
            limit.tag(detail)
        } else {
            detail
        }
    }

    pub(super) fn check(&self) -> Result<(), String> {
        if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(if self.verification_limits {
                "message-level verification cancelled by operator"
            } else {
                "IMAP probe cancelled by operator"
            }
            .into());
        }
        if Instant::now() >= self.deadline {
            return Err(self.limit_error(
                crate::core::VerificationLimit::Deadline,
                if self.verification_limits {
                    "message-level verification exceeded its execution deadline".into()
                } else {
                    "IMAP probe exceeded its total execution deadline".into()
                },
            ));
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
                wait_for_io_retry(deadline, cancel);
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

pub(crate) fn probe_tls_capabilities_with_transport(
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
) -> Result<crate::core::ServerCapabilities, String> {
    let cancel = AtomicBool::new(false);
    let budget = MessageFetchBudget::for_probe(TLS_CAPABILITY_PROBE_BUDGET, &cancel);
    let (stream, greeting) = connect_tls_stream_with_budget(
        host,
        transport,
        ca_bundle,
        certificate_pin_sha256,
        &budget,
    )?;
    complete_authenticated_imap_probe(
        stream,
        host,
        user,
        credential,
        auth_method,
        greeting,
        &budget,
    )
}

/// Authenticate an already TLS-protected connection and perform only the
/// post-authentication readiness checks needed immediately before a live
/// engine launch. Full folder discovery and namespace analysis remain in the
/// comprehensive preflight probe; the separate live-readiness check observes
/// only the destination's INBOX quota root when the server advertises QUOTA.
pub(crate) fn probe_tls_authentication_with_transport(
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
) -> Result<(), String> {
    probe_tls_readiness(
        TlsProbeRequest {
            host,
            user,
            credential,
            auth_method,
            transport,
            ca_bundle,
            certificate_pin_sha256,
        },
        false,
    )
    .map(|_| ())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DestinationQuotaObservation {
    /// QUOTA is unavailable, the root query failed, or usable headroom was
    /// not reported. This never implies that capacity is sufficient.
    Unknown,
    /// The INBOX quota root reported valid usage below its limit. Other quota
    /// roots and projected transfer headroom remain unknown.
    NotExhausted,
    /// A valid INBOX quota-root resource reports usage at or above its limit.
    Exhausted,
}

impl DestinationQuotaObservation {
    pub(crate) fn is_exhausted(self) -> bool {
        self == Self::Exhausted
    }
}

/// Freshly authenticate and perform the bounded pre-launch endpoint checks.
/// When requested, observe only the quota root associated with INBOX. A
/// successful observation below its limit is not treated as proof of projected
/// or account-wide capacity.
pub(crate) fn probe_tls_live_readiness_with_transport(
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
) -> Result<DestinationQuotaObservation, String> {
    probe_tls_readiness(
        TlsProbeRequest {
            host,
            user,
            credential,
            auth_method,
            transport,
            ca_bundle,
            certificate_pin_sha256,
        },
        true,
    )
}

struct TlsProbeRequest<'a> {
    host: &'a str,
    user: &'a str,
    credential: &'a str,
    auth_method: &'a str,
    transport: &'a str,
    ca_bundle: &'a str,
    certificate_pin_sha256: &'a str,
}

fn probe_tls_readiness(
    request: TlsProbeRequest<'_>,
    check_inbox_quota: bool,
) -> Result<DestinationQuotaObservation, String> {
    let cancel = AtomicBool::new(false);
    let budget = MessageFetchBudget::for_probe(TLS_READINESS_PROBE_BUDGET, &cancel);
    let (stream, greeting) = connect_tls_stream_with_budget(
        request.host,
        request.transport,
        request.ca_bundle,
        request.certificate_pin_sha256,
        &budget,
    )?;
    let (mut stream, post_auth_capabilities) = authenticate_imap_stream(
        stream,
        request.host,
        request.user,
        request.credential,
        request.auth_method,
        greeting,
        Some(&budget),
    )?;
    live_readiness_on_authenticated_stream(
        &mut stream,
        request.host,
        &post_auth_capabilities,
        check_inbox_quota,
        Some(&budget),
    )
}

fn live_readiness_on_authenticated_stream<S: Read + Write>(
    stream: &mut S,
    host: &str,
    post_auth_capabilities: &str,
    check_inbox_quota: bool,
    budget: Option<&MessageFetchBudget<'_>>,
) -> Result<DestinationQuotaObservation, String> {
    let mut response = String::new();
    let mut buffer = [0; 4096];
    write_imap_command(stream, b"a004 NOOP\r\n", budget, "could not write NOOP")?;
    read_imap_tagged_with_optional_budget(stream, "a004", &mut response, &mut buffer, budget)?;
    if !imap_command_succeeded(&response, "a004") {
        return Err(imap_command_failure(
            &response,
            "a004",
            "post-auth NOOP",
            host,
        ));
    }
    let quota_observation =
        if check_inbox_quota && advertises_capability(post_auth_capabilities, "QUOTA") {
            let mut quota_response = String::new();
            let quota_result = write_imap_command(
                stream,
                QUOTA_PROBE_COMMAND,
                budget,
                "could not write quota probe",
            )
            .and_then(|()| {
                read_imap_tagged_with_optional_budget(
                    stream,
                    "a006",
                    &mut quota_response,
                    &mut buffer,
                    budget,
                )
            });
            if quota_result.is_ok() {
                inbox_quota_observation(post_auth_capabilities, Some(&quota_response))
            } else {
                if let Some(budget) = budget {
                    budget.check()?;
                }
                DestinationQuotaObservation::Unknown
            }
        } else {
            DestinationQuotaObservation::Unknown
        };
    let _ = write_imap_command(stream, b"a007 LOGOUT\r\n", budget, "could not write LOGOUT");
    Ok(quota_observation)
}

fn complete_authenticated_imap_probe<S: Read + Write>(
    stream: S,
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    greeting: String,
    budget: &MessageFetchBudget<'_>,
) -> Result<crate::core::ServerCapabilities, String> {
    let (mut stream, post_auth_response) = authenticate_imap_stream(
        stream,
        host,
        user,
        credential,
        auth_method,
        greeting,
        Some(budget),
    )?;
    let mut buffer = [0; 4096];
    write_imap_command(
        &mut stream,
        b"a004 NAMESPACE\r\n",
        Some(budget),
        "could not write NAMESPACE",
    )?;
    let mut namespace_response = String::new();
    read_imap_tagged_with_optional_budget(
        &mut stream,
        "a004",
        &mut namespace_response,
        &mut buffer,
        Some(budget),
    )?;
    write_imap_command(
        &mut stream,
        authenticated_list_command(
            "a005",
            advertises_capability(&post_auth_response, "SPECIAL-USE"),
        )
        .as_bytes(),
        Some(budget),
        "could not write LIST",
    )?;
    let mut mailbox_details = Vec::new();
    let list_summary = read_imap_list_response_with_details(
        &mut stream,
        "a005",
        &mut buffer,
        &mut mailbox_details,
        budget,
    )
    .map_err(|error| format!("{host}: folder inventory failed: {error}"))?;
    if list_summary.mailbox_count == 0 {
        return Err(format!(
            "{host}: folder inventory returned no untagged LIST records"
        ));
    }
    // Count messages now so verification capacity is known before a run,
    // leaving the rest of the probe budget for the quota check and LOGOUT.
    let selectable = mailbox_details
        .iter()
        .filter(|mailbox| mailbox.selectable)
        .map(|mailbox| mailbox.wire_name.as_str())
        .collect::<Vec<_>>();
    let census_deadline = (Instant::now() + message_census::CENSUS_TIME_BUDGET).min(
        budget
            .deadline
            .checked_sub(Duration::from_secs(15))
            .unwrap_or_else(Instant::now),
    );
    let message_census = message_census::count_account_messages(
        &mut stream,
        host,
        &selectable,
        advertises_capability(&post_auth_response, "STATUS=SIZE"),
        budget,
        census_deadline,
    )?;
    drop(selectable);
    drop(mailbox_details);
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
    caps.message_census = message_census;
    if caps.supports("QUOTA") {
        let mut quota_response = String::new();
        write_imap_command(
            &mut stream,
            QUOTA_PROBE_COMMAND,
            Some(budget),
            "could not write quota probe",
        )?;
        // A provider can advertise QUOTA while denying the root query. Keep
        // that result explicitly unknown; quota support is advisory and must
        // not turn an otherwise valid mailbox into a false capacity failure.
        // GETQUOTAROOT requires a mailbox name; the empty-root example belongs
        // to GETQUOTA. INBOX is mandatory, and this observation is scoped to
        // the quota roots returned for INBOX rather than all account folders.
        if read_imap_tagged_with_optional_budget(
            &mut stream,
            "a006",
            &mut quota_response,
            &mut buffer,
            Some(budget),
        )
        .is_ok()
            && quota_response_has_successful_root(&quota_response, "a006", "INBOX")
        {
            caps.record_quota_response(&quota_response);
        } else {
            budget.check()?;
        }
    }
    let _ = write_imap_command(
        &mut stream,
        b"a007 LOGOUT\r\n",
        Some(budget),
        "could not write LOGOUT",
    );
    Ok(caps)
}

const QUOTA_PROBE_COMMAND: &[u8] = b"a006 GETQUOTAROOT INBOX\r\n";

fn quota_response_has_successful_root(response: &str, tag: &str, expected: &str) -> bool {
    imap_command_succeeded(response, tag)
        && response.lines().any(|line| {
            if !crate::imap_protocol::is_untagged_response(line, "QUOTAROOT") {
                return false;
            }
            let mut fields = line.split_whitespace();
            fields.next(); // '*'
            fields.next(); // 'QUOTAROOT'
            fields
                .next()
                .is_some_and(|mailbox| mailbox.trim_matches('"').eq_ignore_ascii_case(expected))
        })
}

fn inbox_quota_observation(
    capabilities_response: &str,
    quota_response: Option<&str>,
) -> DestinationQuotaObservation {
    if !advertises_capability(capabilities_response, "QUOTA") {
        return DestinationQuotaObservation::Unknown;
    }
    let Some(response) = quota_response
        .filter(|response| quota_response_has_successful_root(response, "a006", "INBOX"))
    else {
        return DestinationQuotaObservation::Unknown;
    };
    let mut capabilities =
        crate::core::ServerCapabilities::from_inventory_summary(capabilities_response, 1, 0);
    capabilities.record_quota_response(response);
    if !capabilities.quota_observed {
        DestinationQuotaObservation::Unknown
    } else if capabilities.quota_exceeded {
        DestinationQuotaObservation::Exhausted
    } else {
        DestinationQuotaObservation::NotExhausted
    }
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

/// List one account's selectable folders with their RFC 6154 special-use
/// roles, for proposing explicit folder mappings while setting up a plan.
#[allow(clippy::too_many_arguments)]
pub(crate) fn list_account_folders_with_transport(
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
) -> Result<Vec<MailboxDescriptor>, String> {
    let cancel = AtomicBool::new(false);
    let budget = MessageFetchBudget::for_probe(TLS_CAPABILITY_PROBE_BUDGET, &cancel);
    let (stream, greeting) = connect_tls_stream_with_budget(
        host,
        transport,
        ca_bundle,
        certificate_pin_sha256,
        &budget,
    )?;
    let (mut stream, post_auth_response) = authenticate_imap_stream(
        stream,
        host,
        user,
        credential,
        auth_method,
        greeting,
        Some(&budget),
    )?;
    list_folders_on_authenticated_stream(&mut stream, host, &post_auth_response, &budget)
}

fn list_folders_on_authenticated_stream<S: Read + Write>(
    stream: &mut S,
    host: &str,
    post_auth_response: &str,
    budget: &MessageFetchBudget<'_>,
) -> Result<Vec<MailboxDescriptor>, String> {
    write_imap_command(
        stream,
        authenticated_list_command(
            "f001",
            advertises_capability(post_auth_response, "SPECIAL-USE"),
        )
        .as_bytes(),
        Some(budget),
        &format!("{host}: could not enumerate folders"),
    )?;
    let mut buffer = [0; 4096];
    let mut folders = Vec::new();
    read_imap_list_response_with_details(stream, "f001", &mut buffer, &mut folders, budget)?;
    let _ = stream.write_all(b"f999 LOGOUT\r\n");
    folders.retain(|folder| folder.selectable);
    Ok(folders)
}

/// List the source and destination folders of a plan, naming the side that
/// failed.
pub(crate) fn list_both_account_folders(
    form: &crate::Form,
) -> Result<(Vec<MailboxDescriptor>, Vec<MailboxDescriptor>), SidedProbeError> {
    let sided = |side| move |message| SidedProbeError { side, message };
    let source = endpoint_for_probe(&form.profile.source_host, &form.profile.source_port)
        .map_err(sided(MailSide::Source))?;
    let destination = endpoint_for_probe(
        &form.profile.destination_host,
        &form.profile.destination_port,
    )
    .map_err(sided(MailSide::Destination))?;
    let source_folders = list_account_folders_with_transport(
        &source,
        &form.profile.source_user,
        form.source_password.as_str(),
        &form.profile.source_auth,
        &form.profile.source_tls,
        &form.profile.source_ca_bundle,
        &form.profile.source_certificate_pin_sha256,
    )
    .map_err(sided(MailSide::Source))?;
    let destination_folders = list_account_folders_with_transport(
        &destination,
        &form.profile.destination_user,
        form.destination_password.as_str(),
        &form.profile.destination_auth,
        &form.profile.destination_tls,
        &form.profile.destination_ca_bundle,
        &form.profile.destination_certificate_pin_sha256,
    )
    .map_err(sided(MailSide::Destination))?;
    Ok((source_folders, destination_folders))
}

/// Probe one account of a plan for its capabilities and message census.
pub(crate) fn probe_account_capabilities(
    form: &crate::Form,
    side: MailSide,
) -> Result<crate::core::ServerCapabilities, String> {
    let profile = &form.profile;
    let (host, port, user, password, auth, tls, ca_bundle, pin) = match side {
        MailSide::Source => (
            &profile.source_host,
            &profile.source_port,
            &profile.source_user,
            &form.source_password,
            &profile.source_auth,
            &profile.source_tls,
            &profile.source_ca_bundle,
            &profile.source_certificate_pin_sha256,
        ),
        MailSide::Destination => (
            &profile.destination_host,
            &profile.destination_port,
            &profile.destination_user,
            &form.destination_password,
            &profile.destination_auth,
            &profile.destination_tls,
            &profile.destination_ca_bundle,
            &profile.destination_certificate_pin_sha256,
        ),
    };
    probe_tls_capabilities_with_transport(
        &endpoint_for_probe(host, port)?,
        user,
        password.as_str(),
        auth,
        tls,
        ca_bundle,
        pin,
    )
}

pub(crate) fn fresh_dual_imaps_readiness(form: &crate::Form) -> Result<(), SidedProbeError> {
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
    let destination_quota = probe_tls_live_readiness_with_transport(
        &destination,
        &form.profile.destination_user,
        form.destination_password.as_str(),
        &form.profile.destination_auth,
        &form.profile.destination_tls,
        &form.profile.destination_ca_bundle,
        &form.profile.destination_certificate_pin_sha256,
    )
    .map_err(sided(MailSide::Destination))?;
    if destination_quota.is_exhausted() {
        return Err(sided(MailSide::Destination)(
            "destination INBOX-root quota reports exhausted capacity; live migration was not started"
                .into(),
        ));
    }
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

#[allow(clippy::too_many_arguments)]
fn fetch_mailbox_with_existing_stream<S: Read + Write, C: MessageSink>(
    stream: &mut S,
    host: &str,
    mailbox: &str,
    budget: &MessageFetchBudget<'_>,
    state_budget: &MessageStateBudget,
    body_hash: Option<&BodyHashOptions<'_>>,
    condstore_enabled: bool,
    sink: &mut C,
) -> Result<u64, MailboxFetchError> {
    budget.check()?;
    if mailbox.trim().is_empty() {
        return Err("message-level verification requires a non-empty mailbox".into());
    }
    let quoted_mailbox = imap_quote(mailbox)?;
    let select_parameter = if condstore_enabled {
        " (CONDSTORE)"
    } else {
        ""
    };
    let select = format!("v001 SELECT {quoted_mailbox}{select_parameter}\r\n");
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
    let start_modseq = if condstore_enabled {
        Some(parse_select_modseq(&response, host, mailbox)?)
    } else {
        None
    };
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
        highest_modseq: start_modseq.and_then(|modseq| match modseq {
            ModseqSnapshot::Tracked(value) => Some(value),
            ModseqSnapshot::Unsupported => None,
        }),
    };
    // Durable metadata from a prior attempt is eligible for reuse only when
    // the server provides a tracked MODSEQ and the complete SELECT snapshot
    // still matches. Body-hash scans never reuse old pages.
    let reused_completed_scan =
        body_hash.is_none() && sink.reuse_completed_mailbox(mailbox, snapshot)?;
    let resumed_prefix = if !reused_completed_scan && body_hash.is_none() {
        sink.resume_mailbox_prefix(mailbox, snapshot)?
    } else {
        None
    };
    if !reused_completed_scan && resumed_prefix.is_none() {
        sink.begin_mailbox_scan(mailbox, snapshot)?;
    }
    sink.record_permanent_flags(mailbox, parse_permanent_flags(&response).as_deref())?;
    let enumerated_uid_count = if reused_completed_scan {
        start_exists
    } else {
        enumerate_uid_pages(
            stream,
            host,
            mailbox,
            start_exists,
            resumed_prefix,
            &mut response,
            &mut buffer,
            budget,
            if body_hash.is_some() {
                FetchPagePlanner::body()
            } else {
                FetchPagePlanner::metadata()
            },
            |stream, buffer, uid_page, uid_set| {
                budget.check()?;
                if message_fetch_limit_exceeded(sink.len(), uid_page.len()) {
                    return Err(crate::core::VerificationLimit::MessageCount.tag(format!(
                    "{host}: account would exceed the {MAX_MESSAGE_FETCH_RECORDS}-message verification limit"
                )));
                }
                if body_hash.is_some()
                    && !sink.has_durable_stage()
                    && body_hash_message_limit_exceeded(sink.len(), uid_page.len())
                {
                    return Err(crate::core::VerificationLimit::BodyHashMessageCount.tag(format!(
                    "{host}: ephemeral body-hash proof is limited to {MAX_BODY_HASH_MESSAGES_PER_ENDPOINT} messages per endpoint; use a durable SQLite-backed verification run or metadata-only verification"
                )));
                }
                let tag = format!("v{:03}", page_number + 3);
                page_number = page_number.saturating_add(1);
                let body_field = body_hash.map_or("", |_| " BODY.PEEK[]");
                // A flag observation is suitable for fidelity evidence only when
                // the selected mailbox supplies a tracked MODSEQ that can detect
                // concurrent flag changes through the end-of-scan snapshot.
                // Re-reading FLAGS without MODSEQ only narrows the race window; it
                // cannot establish that the observation remained stable.
                let flags_field = if matches!(start_modseq, Some(ModseqSnapshot::Tracked(_))) {
                    " FLAGS"
                } else {
                    ""
                };
                let command = format!(
                    // RFC 3501 exposes system flags and custom keyword atoms
                    // through FLAGS; KEYWORDS is not a portable FETCH data item.
                    "{tag} UID FETCH {uid_set} (UID{flags_field} RFC822.SIZE INTERNALDATE BODY.PEEK[HEADER.FIELDS (MESSAGE-ID FROM TO CC SUBJECT DATE CONTENT-TYPE)]{body_field})\r\n"
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
                let mut page_messages = parse_message_fetch_metadata_response_bytes_with_mailbox(
                    &raw_response,
                    std::sync::Arc::clone(&mailbox_context),
                    Some(start_uidvalidity),
                )?;
                validate_fetch_page_coverage(&page_messages, uid_page, host, mailbox)?;
                if !matches!(start_modseq, Some(ModseqSnapshot::Tracked(_))) {
                    // Servers may send unsolicited FETCH updates. Never let an
                    // untracked FLAGS item leak into the evidence merely because
                    // it appeared alongside a requested metadata response.
                    page_messages
                        .values_mut()
                        .for_each(|message| message.flags = None);
                }
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
                if let Some(last_uid) = uid_page.last().copied() {
                    if let Some(body_page) = body_page.as_ref() {
                        sink.insert_page_and_checkpoint(
                            mailbox,
                            snapshot,
                            last_uid,
                            &page_messages,
                            &body_page.fingerprints,
                        )?;
                    } else {
                        sink.insert_page_and_checkpoint(
                            mailbox,
                            snapshot,
                            last_uid,
                            &page_messages,
                            &HashMap::new(),
                        )?;
                    }
                } else if let Some(body_page) = body_page.as_ref() {
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
                Ok(response_bytes)
            },
        )?
    };
    if enumerated_uid_count != start_exists {
        return Err(format!(
            "{host}: folder {mailbox}: UID enumeration coverage mismatch (EXISTS {start_exists}, validated UIDs {enumerated_uid_count})",
        )
        .into());
    }
    // Require complete coverage of the selected snapshot. Previously staged
    // rows count only when the durable sink proved a completed, identical,
    // MODSEQ-tracked snapshot; all other paths invalidated and refetched them.
    let staged = sink.mailbox_len(mailbox, start_uidvalidity)?;
    if staged != start_exists {
        return Err(MailboxFetchError::Changed(format!(
            "{host}: folder {mailbox}: staged rows ({staged}) differ from EXISTS {start_exists}; rescan required"
        )));
    }

    // Re-SELECT after a fresh scan or reused snapshot. Any intervening change
    // invalidates the proof; the account-level caller retries this folder
    // once before marking verification incomplete.
    let end_tag = format!("v{:03}", page_number + 3);
    let end_select = format!("{end_tag} SELECT {quoted_mailbox}{select_parameter}\r\n");
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
    let end_modseq = if condstore_enabled {
        Some(parse_select_modseq(&response, host, mailbox)?)
    } else {
        None
    };
    if end_uidvalidity != Some(start_uidvalidity)
        || end_uidnext != Some(uidnext)
        || end_exists != start_exists
        || (start_modseq.is_some() && end_modseq != start_modseq)
    {
        return Err(MailboxFetchError::Changed(format!(
            "{host}: mailbox {mailbox} changed during verification (UIDVALIDITY {start_uidvalidity:?}->{end_uidvalidity:?}, UIDNEXT {uidnext}->{end_uidnext:?}, message count {start_exists}->{end_exists}, HIGHESTMODSEQ {start_modseq:?}->{end_modseq:?}); retry required"
        )));
    }
    let last_uid = uidnext.saturating_sub(1);
    if !reused_completed_scan {
        sink.complete_mailbox(mailbox, snapshot, last_uid)?;
    }
    state_reservation.commit();
    // This path is metadata-only by design. BODY[] hashing belongs to the
    // separate content-verification adapter and is not populated here.
    Ok(start_exists)
}

#[allow(clippy::too_many_arguments)]
fn fetch_mailbox_with_stability_retry<S: Read + Write, C: MessageSink>(
    stream: &mut S,
    host: &str,
    mailbox: &str,
    budget: &MessageFetchBudget<'_>,
    state_budget: &MessageStateBudget,
    body_hash: Option<&BodyHashOptions<'_>>,
    condstore_enabled: bool,
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
            condstore_enabled,
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

#[allow(clippy::too_many_arguments)]
fn fetch_mailbox_with_session_reconnect<S, C, R>(
    stream: &mut S,
    host: &str,
    mailbox: &str,
    budget: &MessageFetchBudget<'_>,
    state_budget: &MessageStateBudget,
    body_hash: Option<&BodyHashOptions<'_>>,
    condstore_enabled: &mut bool,
    consecutive_reconnects: &mut usize,
    reconnect: &mut R,
    sink: &mut C,
) -> Result<u64, MailboxFetchError>
where
    S: Read + Write,
    C: MessageSink,
    R: FnMut() -> Result<(S, bool), String>,
{
    const MAX_CONSECUTIVE_SESSION_RECONNECTS: usize = 3;

    loop {
        match fetch_mailbox_with_stability_retry(
            stream,
            host,
            mailbox,
            budget,
            state_budget,
            body_hash,
            *condstore_enabled,
            sink,
        ) {
            Err(MailboxFetchError::SessionFatal(error))
                if *consecutive_reconnects < MAX_CONSECUTIVE_SESSION_RECONNECTS =>
            {
                *consecutive_reconnects = consecutive_reconnects.saturating_add(1);
                match reconnect() {
                    Ok((new_stream, new_condstore_enabled)) => {
                        *stream = new_stream;
                        *condstore_enabled = new_condstore_enabled;
                    }
                    Err(reconnect_error) => {
                        return Err(MailboxFetchError::SessionFatal(format!(
                            "{error}; reconnect attempt {consecutive_reconnects} failed: {reconnect_error}"
                        )));
                    }
                }
            }
            result => return result,
        }
    }
}

/// Enumerate bounded sequence-number pages and hand each fetch-sized UID page
/// to the caller. Sequence numbers are dense over the current SELECT snapshot,
/// so command count scales with EXISTS rather than historical UIDNEXT. No
/// response contains the entire mailbox UID set, and no all-mailbox UID vector
/// is retained between pages.
#[allow(clippy::too_many_arguments)]
fn enumerate_uid_pages<S: Read + Write, F>(
    stream: &mut S,
    host: &str,
    mailbox: &str,
    exists: u64,
    resume_prefix: Option<(u64, u64)>,
    response: &mut String,
    buffer: &mut [u8; 4096],
    budget: &MessageFetchBudget<'_>,
    mut planner: FetchPagePlanner,
    mut consume_page: F,
) -> Result<u64, String>
where
    F: FnMut(&mut S, &mut [u8; 4096], &[u64], &str) -> Result<usize, String>,
{
    let (mut sequence_start, mut enumerated_uid_count, mut last_enumerated_uid) =
        match resume_prefix {
            Some((count, last_uid)) if count > 0 && count <= exists => (
                count
                    .checked_add(1)
                    .ok_or_else(|| format!("{host}: verification resume sequence overflowed"))?,
                count,
                Some(last_uid),
            ),
            Some(_) => {
                return Err(format!(
                    "{host}: {mailbox} has an invalid persisted verification prefix"
                ));
            }
            None => (1, 0, None),
        };
    let mut search_number = 0usize;
    while sequence_start <= exists {
        budget.check()?;
        let sequence_end = sequence_start
            .saturating_add(MESSAGE_UID_ENUMERATION_PAGE_SIZE - 1)
            .min(exists);
        let search_tag = format!("s{:03}", search_number + 2);
        search_number = search_number.saturating_add(1);
        // FETCH takes a sequence-number set.  Prefixing this with UID would
        // make the range a historical UID set, defeating EXISTS-bounded
        // enumeration for sparse mailboxes.
        let search_command =
            format!("{search_tag} FETCH {sequence_start}:{sequence_end} (UID)\r\n");
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
                "FETCH mailbox sequence page",
                host,
            ));
        }
        let uids = parse_uid_fetch_response(response, host, mailbox, sequence_start, sequence_end)?;
        if let (Some(previous), Some(first)) = (last_enumerated_uid, uids.first())
            && *first <= previous
        {
            return Err(format!(
                "{host}: FETCH {mailbox} UID order changed across sequence pages"
            ));
        }
        last_enumerated_uid = uids.last().copied().or(last_enumerated_uid);
        enumerated_uid_count = enumerated_uid_count
            .checked_add(uids.len() as u64)
            .ok_or_else(|| format!("{host}: enumerated UID count overflowed"))?;
        // Fresh scans begin at the first extant message. A resumable prefix
        // starts at the next sequence only after the caller validated the
        // persisted page/cursor pair against a tracked unchanged snapshot.
        let mut offset = 0;
        while offset < uids.len() {
            let (count, uid_set) = encode_uid_page(&uids[offset..], planner.size());
            let uid_page = &uids[offset..offset + count];
            let response_bytes = consume_page(stream, buffer, uid_page, &uid_set)?;
            planner.observe(uid_page.len(), response_bytes);
            offset += count;
        }
        sequence_start = sequence_end.saturating_add(1);
    }
    Ok(enumerated_uid_count)
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
    reconnect_credential: &mut dyn FnMut() -> Result<crate::credentials::SecretString, String>,
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
    let mut condstore_enabled = advertises_capability(&post_auth_response, "CONDSTORE");
    write_imap_command(
        &mut stream,
        authenticated_list_command(
            "a005",
            advertises_capability(&post_auth_response, "SPECIAL-USE"),
        )
        .as_bytes(),
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
    )
    .map_err(tag_folder_inventory_limit)?;
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
    sink.retain_mailboxes(&mailbox_inventory)?;
    let mut total_exists = 0_u64;
    let mut incomplete_folders = HashMap::new();
    let mut incomplete_folder_count = 0_usize;
    let mut consecutive_session_reconnects = 0_usize;

    // Process folders on one authenticated connection when possible. A
    // session-fatal transport error gets a bounded reconnect/re-authentication
    // attempt and retries the failed folder; the folder's own stability
    // checks still decide whether its staged evidence is usable. The budget
    // is consecutive rather than account-global: a provider may legitimately
    // close a long-lived session once per folder, but repeated failures on the
    // same recovery path still fail closed. The verification deadline remains
    // the total bound on all reconnect work.
    for mailbox in mailboxes {
        budget.check()?;
        let mut reconnect = || {
            let (new_stream, new_post_auth_response) = reconnect_and_authenticate_imap_stream(
                host,
                user,
                auth_method,
                Some(budget),
                reconnect_credential,
                || {
                    connect_tls_stream_with_budget(
                        host,
                        transport,
                        ca_bundle,
                        certificate_pin_sha256,
                        budget,
                    )
                },
            )?;
            Ok((
                new_stream,
                advertises_capability(&new_post_auth_response, "CONDSTORE"),
            ))
        };
        let result = fetch_mailbox_with_session_reconnect(
            &mut stream,
            host,
            &mailbox,
            budget,
            state_budget,
            body_hash,
            &mut condstore_enabled,
            &mut consecutive_session_reconnects,
            &mut reconnect,
            sink,
        );
        match result {
            Ok(folder_exists) => {
                consecutive_session_reconnects = 0;
                total_exists = total_exists.saturating_add(folder_exists);
                if sink.len() > MAX_MESSAGE_FETCH_RECORDS {
                    let _ = stream.write_all(b"a999 LOGOUT\r\n");
                    return Err(crate::core::VerificationLimit::MessageCount.tag(format!(
                        "{host}: account exceeded the {MAX_MESSAGE_FETCH_RECORDS}-message verification limit"
                    )));
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
pub(crate) fn fetch_tls_account_messages_to_stage_with_body_hashes_excluding_refresh(
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
    reconnect_credential: &mut dyn FnMut() -> Result<crate::credentials::SecretString, String>,
    stage: &mut crate::core::MessageMetadataStage,
    side: crate::core::StagedMessageSide,
) -> Result<FetchedAccountSummary, String> {
    let mut sink = StageMessageSink {
        stage,
        side,
        count: 0,
        mailbox_counts: HashMap::new(),
        active_mailbox: None,
    };
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
        reconnect_credential,
        &mut sink,
    )
}

/// Re-SELECT every endpoint folder after its inventory has finished. Any
/// folder-set or mailbox cursor drift invalidates verification. Sequential
/// endpoint rechecks narrow stale-evidence windows but do not create an atomic
/// account-wide snapshot or replace operator-enforced quiescence.
pub(crate) struct TlsAccountRevalidation<'a> {
    pub endpoint_label: &'a str,
    pub host: &'a str,
    pub user: &'a str,
    pub credential: &'a str,
    pub auth_method: &'a str,
    pub transport: &'a str,
    pub ca_bundle: &'a str,
    pub certificate_pin_sha256: &'a str,
    pub budget: &'a MessageFetchBudget<'a>,
    pub expected_mailboxes: &'a HashSet<String>,
    pub excluded_mailboxes: &'a HashSet<String>,
    pub expected_snapshots: &'a HashMap<String, crate::core::FolderSnapshot>,
}

pub(crate) fn revalidate_tls_account_snapshots(
    request: TlsAccountRevalidation<'_>,
) -> Result<HashMap<String, String>, String> {
    let TlsAccountRevalidation {
        endpoint_label,
        host,
        user,
        credential,
        auth_method,
        transport,
        ca_bundle,
        certificate_pin_sha256,
        budget,
        expected_mailboxes,
        excluded_mailboxes,
        expected_snapshots,
    } = request;
    budget.check()?;
    if transport == "plain" {
        return Err(format!(
            "{endpoint_label} snapshot revalidation requires TLS"
        ));
    }
    if expected_snapshots.len() != expected_mailboxes.len()
        || expected_mailboxes
            .iter()
            .any(|mailbox| !expected_snapshots.contains_key(mailbox))
    {
        return Err(format!(
            "{endpoint_label} snapshot revalidation lacks a complete folder baseline"
        ));
    }
    let (stream, greeting) =
        connect_tls_stream_with_budget(host, transport, ca_bundle, certificate_pin_sha256, budget)?;
    let (mut stream, capabilities) = authenticate_imap_stream(
        stream,
        host,
        user,
        credential,
        auth_method,
        greeting,
        Some(budget),
    )?;
    let condstore_enabled = advertises_capability(&capabilities, "CONDSTORE");
    let mut buffer = [0; 4096];
    stream
        .write_all(
            authenticated_list_command("r000", advertises_capability(&capabilities, "SPECIAL-USE"))
                .as_bytes(),
        )
        .map_err(|error| format!("{host}: could not re-list {endpoint_label} folders: {error}"))?;
    let mut descriptors = Vec::new();
    let inventory = read_imap_list_response_with_details(
        &mut stream,
        "r000",
        &mut buffer,
        &mut descriptors,
        budget,
    )
    .map_err(tag_folder_inventory_limit)?;
    if inventory.selectable_mailbox_count
        != descriptors.iter().filter(|item| item.selectable).count()
    {
        return Err(format!(
            "{host}: {endpoint_label} revalidation returned an incomplete folder inventory"
        ));
    }
    let current_mailboxes = descriptors
        .iter()
        .filter(|item| item.selectable && !excluded_mailboxes.contains(&item.wire_name))
        .map(|item| item.wire_name.clone())
        .collect::<HashSet<_>>();
    if &current_mailboxes != expected_mailboxes {
        return Err(format!(
            "{host}: {endpoint_label} folder inventory changed after its scan; refusing exact verification"
        ));
    }

    revalidate_source_selected_folders(
        &mut stream,
        host,
        budget,
        expected_snapshots,
        condstore_enabled,
        endpoint_label,
    )
}

fn revalidate_source_selected_folders<S: Read + Write>(
    stream: &mut S,
    host: &str,
    budget: &MessageFetchBudget<'_>,
    expected_snapshots: &HashMap<String, crate::core::FolderSnapshot>,
    condstore_enabled: bool,
    endpoint_label: &str,
) -> Result<HashMap<String, String>, String> {
    let mut mailboxes = expected_snapshots.keys().collect::<Vec<_>>();
    mailboxes.sort_unstable();
    let mut buffer = [0; 4096];
    let mut response = String::new();
    let mut observations = HashMap::with_capacity(mailboxes.len());
    for (index, mailbox) in mailboxes.into_iter().enumerate() {
        budget.check()?;
        let quoted_mailbox = imap_quote(mailbox)?;
        let select_parameter = if condstore_enabled {
            " (CONDSTORE)"
        } else {
            ""
        };
        let tag = format!("r{:06}", index + 1);
        let command = format!("{tag} EXAMINE {quoted_mailbox}{select_parameter}\r\n");
        write_imap_command(
            stream,
            command.as_bytes(),
            Some(budget),
            &format!("{host}: could not re-SELECT {endpoint_label} folder"),
        )?;
        response.clear();
        read_imap_tagged_with_budget(stream, &tag, &mut response, &mut buffer, 1_048_576, budget)?;
        if !imap_command_succeeded(&response, &tag) {
            return Err(imap_command_failure(
                &response,
                &tag,
                &format!("{endpoint_label} snapshot revalidation"),
                host,
            ));
        }
        let (exists, uidvalidity, uidnext) = parse_selected_mailbox(&response, host, mailbox)?;
        let uidvalidity = uidvalidity.ok_or_else(|| {
            format!("{host}: {endpoint_label} revalidation for {mailbox} omitted UIDVALIDITY")
        })?;
        let uidnext = uidnext.ok_or_else(|| {
            format!("{host}: {endpoint_label} revalidation for {mailbox} omitted UIDNEXT")
        })?;
        let expected = expected_snapshots
            .get(mailbox)
            .ok_or_else(|| format!("{host}: {endpoint_label} baseline missing folder {mailbox}"))?;
        let current_modseq = if condstore_enabled {
            match parse_select_modseq(&response, host, mailbox)? {
                ModseqSnapshot::Tracked(value) => Some(value),
                ModseqSnapshot::Unsupported => None,
            }
        } else {
            None
        };
        let current = crate::core::FolderSnapshot {
            uidvalidity,
            uidnext,
            exists,
            highest_modseq: current_modseq,
        };
        if !source_snapshot_matches(expected, &current) {
            let _ = stream.write_all(b"r999999 LOGOUT\r\n");
            let changed_fields = [
                (expected.uidvalidity != current.uidvalidity, "UIDVALIDITY"),
                (expected.uidnext != current.uidnext, "UIDNEXT"),
                (expected.exists != current.exists, "EXISTS"),
                (
                    expected.highest_modseq != current.highest_modseq,
                    "HIGHESTMODSEQ",
                ),
            ]
            .into_iter()
            .filter_map(|(changed, field)| changed.then_some(field))
            .collect::<Vec<_>>()
            .join(", ");
            return Err(format!(
                "{host}: {endpoint_label} folder {mailbox} changed after its verification scan (changed fields: {changed_fields}); rerun the final delta and verification",
            ));
        }
        observations.insert(
            mailbox.clone(),
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        );
    }
    let _ = stream.write_all(b"r999999 LOGOUT\r\n");
    Ok(observations)
}

pub(super) fn source_snapshot_matches(
    expected: &crate::core::FolderSnapshot,
    current: &crate::core::FolderSnapshot,
) -> bool {
    expected.uidvalidity == current.uidvalidity
        && expected.uidnext == current.uidnext
        && expected.exists == current.exists
        // A missing baseline watermark cannot prove that mutable flags stayed
        // unchanged. If watermark availability changes between the scan and
        // revalidation, invalidate the snapshot and require a fresh pass
        // rather than silently treating the new capability as equivalent.
        && expected.highest_modseq == current.highest_modseq
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
    let message = if omitted == 0 {
        format!(
            "{host}: verification did not obtain stable metadata for folders: {}",
            details.join("; ")
        )
    } else {
        format!(
            "{host}: verification did not obtain stable metadata for folders: {}; (+{omitted} more)",
            details.join("; ")
        )
    };
    // A folder that stopped at a safety limit cannot be completed by
    // retrying, so the account result carries that limit's tag even when
    // the folder's detail is truncated or omitted above.
    let mut limits = failures
        .iter()
        .filter_map(|(folder, reason)| {
            crate::core::VerificationLimit::from_detail(reason).map(|limit| (folder, limit))
        })
        .collect::<Vec<_>>();
    limits.sort_by(|left, right| left.0.cmp(right.0));
    match limits.first() {
        Some((_, limit))
            if crate::core::VerificationLimit::from_detail(&message) != Some(*limit) =>
        {
            limit.tag(message)
        }
        _ => message,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModseqSnapshot {
    Tracked(u64),
    Unsupported,
}

/// CONDSTORE requires one of HIGHESTMODSEQ or NOMODSEQ on every successful
/// SELECT/EXAMINE after negotiation. Treat malformed or missing state as a
/// folder failure rather than silently using the weaker snapshot.
fn parse_select_modseq(
    response: &str,
    host: &str,
    mailbox: &str,
) -> Result<ModseqSnapshot, String> {
    let mut highest = None;
    let mut no_modseq = false;
    for line in response.lines() {
        let Some(response_text) = untagged_ok_response_text(line) else {
            continue;
        };
        let Some(code_text) = response_text.strip_prefix('[') else {
            continue;
        };
        let Some((code_text, trailing)) = code_text.split_once(']') else {
            if code_text
                .split_ascii_whitespace()
                .next()
                .is_some_and(|code| {
                    code.eq_ignore_ascii_case("HIGHESTMODSEQ")
                        || code.eq_ignore_ascii_case("NOMODSEQ")
                })
            {
                return Err(format!(
                    "{host}: SELECT {mailbox} returned a malformed MODSEQ response code"
                ));
            }
            continue;
        };
        if !trailing.is_empty() && !trailing.starts_with(' ') {
            return Err(format!(
                "{host}: SELECT {mailbox} returned malformed data after a response code"
            ));
        }
        let mut fields = code_text.split(' ');
        let Some(code) = fields.next() else {
            continue;
        };
        if code.eq_ignore_ascii_case("HIGHESTMODSEQ") {
            let value = fields
                .next()
                .filter(|value| {
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                })
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0 && *value <= i64::MAX as u64)
                .ok_or_else(|| {
                    format!("{host}: SELECT {mailbox} returned invalid HIGHESTMODSEQ")
                })?;
            if fields.next().is_some() {
                return Err(format!(
                    "{host}: SELECT {mailbox} returned malformed HIGHESTMODSEQ response code"
                ));
            }
            if highest.replace(value).is_some() {
                return Err(format!(
                    "{host}: SELECT {mailbox} returned duplicate HIGHESTMODSEQ codes"
                ));
            }
        } else if code.eq_ignore_ascii_case("NOMODSEQ") {
            if fields.next().is_some() {
                return Err(format!(
                    "{host}: SELECT {mailbox} returned malformed NOMODSEQ response code"
                ));
            }
            if no_modseq {
                return Err(format!(
                    "{host}: SELECT {mailbox} returned duplicate NOMODSEQ codes"
                ));
            }
            no_modseq = true;
        }
    }
    match (highest, no_modseq) {
        (Some(value), false) => Ok(ModseqSnapshot::Tracked(value)),
        (None, true) => Ok(ModseqSnapshot::Unsupported),
        _ => Err(format!(
            "{host}: SELECT {mailbox} did not return exactly one HIGHESTMODSEQ or NOMODSEQ response code"
        )),
    }
}

fn untagged_ok_response_text(line: &str) -> Option<&str> {
    let response = line.strip_prefix("* ")?;
    let (status, text) = response.split_once(' ')?;
    status.eq_ignore_ascii_case("OK").then_some(text)
}

/// LIST parsing is shared with preflight discovery, so its inventory bounds
/// are tagged as verification limits only where verification reads LIST.
fn tag_folder_inventory_limit(error: String) -> String {
    if error.contains("LIST inventory exceeded") || error.contains("-mailbox limit") {
        crate::core::VerificationLimit::FolderInventory.tag(error)
    } else {
        error
    }
}

/// Extract the canonical `PERMANENTFLAGS` set from a SELECT response. RFC
/// 9051 says a client should assume every flag can be stored when the code
/// is absent, so `None` grants no exception during flag verification.
fn parse_permanent_flags(response: &str) -> Option<String> {
    response.lines().find_map(|line| {
        let upper = line.to_ascii_uppercase();
        let start = upper.find("[PERMANENTFLAGS (")? + "[PERMANENTFLAGS (".len();
        let end = start + line[start..].find(')')?;
        Some(canonical_flag_set(line[start..end].split_whitespace()))
    })
}

pub(crate) fn fresh_imap_authentication_applies(form: &crate::Form) -> bool {
    !form.dry_run
        && form.engine() == core::Engine::ImapSync
        && form.profile.source_tls != "plain"
        && form.profile.destination_tls != "plain"
}

#[cfg(test)]
#[path = "imap_probe_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "imap_probe_resume_tests.rs"]
mod resume_tests;

#[cfg(test)]
#[path = "imap_probe_flag_tests.rs"]
mod flag_tests;
