//! Restart state machine for durable verification staging. Each test
//! reproduces what a killed controller leaves in the stage, changes the
//! server between runs, and drives the real fetch path against a scripted
//! IMAP server. The invariant under test: after a scan the staged folder
//! equals the server's current folder exactly, whatever happened between
//! runs. Completed folders and committed metadata prefixes require an
//! unchanged tracked MODSEQ; untracked scans are refetched.
use super::*;
use crate::core::{
    ExtractedMessage, ExtractedMessages, FolderSnapshot, MailboxMessageKey, MessageMetadataStage,
    StagedMessageSide,
};
use std::io::{self, Read, Write};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const SIDE: StagedMessageSide = StagedMessageSide::Destination;
const PAGE: u64 = MESSAGE_FETCH_PAGE_SIZE;

/// Minimal scripted IMAP server for one folder: answers SELECT, sequence
/// number FETCH enumeration, and UID FETCH (metadata and optional BODY[])
/// from an in-memory
/// UID set, recording every UID it was asked to fetch. Messages listed in
/// `flags` carry a FLAGS item; SELECT advertises `permanent_flags` if set.
pub(super) struct FolderServer {
    uids: Vec<u64>,
    uidvalidity: u64,
    uidnext: u64,
    fetched: Vec<u64>,
    fetch_commands: usize,
    flags_only_fetch_commands: usize,
    uid_enumeration_commands: usize,
    pub(super) flags: std::collections::HashMap<u64, String>,
    change_flags_after_metadata_fetch: Option<(u64, String)>,
    pub(super) condstore: bool,
    pub(super) unsolicited_flags: bool,
    modseq_unavailable: bool,
    modseq: u64,
    pub(super) permanent_flags: Option<String>,
    input: Vec<u8>,
    output: Vec<u8>,
    /// Bytes of `output` already read; dropped once all are consumed so a
    /// large reply is not shifted on every read.
    output_read: usize,
    commands: Vec<String>,
}

impl FolderServer {
    pub(super) fn new(uids: &[u64], uidvalidity: u64, uidnext: u64) -> Self {
        Self {
            uids: uids.to_vec(),
            uidvalidity,
            uidnext,
            fetched: Vec::new(),
            fetch_commands: 0,
            flags_only_fetch_commands: 0,
            uid_enumeration_commands: 0,
            flags: std::collections::HashMap::new(),
            change_flags_after_metadata_fetch: None,
            condstore: false,
            unsolicited_flags: false,
            modseq_unavailable: false,
            modseq: 1,
            permanent_flags: None,
            input: Vec::new(),
            output: Vec::new(),
            output_read: 0,
            commands: Vec::new(),
        }
    }

    fn snapshot(&self) -> FolderSnapshot {
        FolderSnapshot {
            uidvalidity: self.uidvalidity,
            uidnext: self.uidnext,
            exists: self.uids.len() as u64,
            highest_modseq: (self.condstore && !self.modseq_unavailable).then_some(self.modseq),
        }
    }

    fn respond(&mut self, line: &str) {
        self.commands.push(line.to_owned());
        let (tag, command) = line.split_once(' ').unwrap();
        let reply = if command.starts_with("SELECT ") || command.starts_with("EXAMINE ") {
            let modseq = if self.condstore && command.ends_with("(CONDSTORE)") {
                if self.modseq_unavailable {
                    "* OK [NOMODSEQ] unsupported\r\n".to_owned()
                } else {
                    format!("* OK [HIGHESTMODSEQ {}] current\r\n", self.modseq)
                }
            } else {
                String::new()
            };
            let permanent = self
                .permanent_flags
                .as_ref()
                .map(|flags| format!("* OK [PERMANENTFLAGS ({flags})] ok\r\n"))
                .unwrap_or_default();
            format!(
                "* {} EXISTS\r\n{permanent}* OK [UIDVALIDITY {}] ok\r\n* OK [UIDNEXT {}] ok\r\n{modseq}{tag} OK SELECT completed\r\n",
                self.uids.len(),
                self.uidvalidity,
                self.uidnext
            )
        } else if let Some(rest) = command.strip_prefix("FETCH ") {
            let set = rest.split_whitespace().next().unwrap();
            assert!(rest.ends_with("(UID)"));
            self.uid_enumeration_commands += 1;
            let (low, high) = set.split_once(':').unwrap();
            let low = low.parse::<u64>().unwrap();
            let high = high.parse::<u64>().unwrap();
            let mut reply = String::new();
            for sequence in low..=high {
                if let Some(uid) = self.uids.get(sequence.saturating_sub(1) as usize) {
                    reply.push_str(&format!("* {sequence} FETCH (UID {uid})\r\n"));
                }
            }
            reply.push_str(&format!("{tag} OK FETCH completed\r\n"));
            reply
        } else if command == "LOGOUT" {
            format!("* BYE logging out\r\n{tag} OK LOGOUT completed\r\n")
        } else if let Some(rest) = command.strip_prefix("UID FETCH ") {
            let set = rest.split_whitespace().next().unwrap();
            let with_body = rest.contains("BODY.PEEK[]");
            let flags_only = rest.ends_with("(UID FLAGS)");
            let requests_flags = rest.contains("FLAGS");
            if flags_only {
                self.flags_only_fetch_commands += 1;
            }
            if !flags_only {
                self.fetch_commands += 1;
            }
            let mut reply = String::new();
            let requested = set.split(',').flat_map(|item| {
                let (low, high) = item.split_once(':').unwrap_or((item, item));
                low.parse::<u64>().unwrap()..=high.parse::<u64>().unwrap()
            });
            for uid in requested {
                if !flags_only {
                    self.fetched.push(uid);
                }
                let sequence = self
                    .uids
                    .binary_search(&uid)
                    .ok()
                    .or_else(|| self.uids.iter().position(|value| *value == uid))
                    .unwrap()
                    + 1;
                let header = format!("Message-ID: <{uid}@example.test>\r\n\r\n");
                let body = if with_body {
                    let body = format!("body of {uid}");
                    format!(" BODY[] {{{}}}\r\n{body}", body.len())
                } else {
                    String::new()
                };
                let flags = if requests_flags || self.unsolicited_flags {
                    self.flags
                        .get(&uid)
                        .map(|flags| format!("FLAGS ({flags}) "))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                if flags_only {
                    reply.push_str(&format!("* {sequence} FETCH (UID {uid} {flags})\r\n"));
                    continue;
                }
                reply.push_str(&format!(
                    "* {sequence} FETCH (UID {uid} {flags}RFC822.SIZE 10 INTERNALDATE \"01-Jan-2024 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}}}\r\n{header}{body})\r\n",
                    header.len()
                ));
            }
            if !flags_only && let Some((uid, flags)) = self.change_flags_after_metadata_fetch.take()
            {
                self.flags.insert(uid, flags);
                self.modseq = self.modseq.saturating_add(1);
            }
            reply.push_str(&format!("{tag} OK FETCH completed\r\n"));
            reply
        } else {
            panic!("unexpected command {line}");
        };
        self.output.extend_from_slice(reply.as_bytes());
    }
}

#[test]
fn post_destination_source_revalidation_accepts_stable_cursor_and_rejects_modseq_drift() {
    let cancelled = AtomicBool::new(false);
    let budget = MessageFetchBudget::new(Duration::from_secs(10), &cancelled);
    let mut server = FolderServer::new(&[1, 2], 3, 42);
    server.condstore = true;
    server.modseq = 12;
    let expected = HashMap::from([("INBOX".to_owned(), server.snapshot())]);
    revalidate_source_selected_folders(
        &mut server,
        "imap.example.test",
        &budget,
        &expected,
        true,
        "source",
    )
    .unwrap();
    assert!(
        server
            .commands
            .iter()
            .any(|command| { command.contains("EXAMINE \"INBOX\" (CONDSTORE)") })
    );

    let mut changed_all = FolderServer::new(&[1, 2, 3], 30, 420);
    changed_all.condstore = true;
    changed_all.modseq = 130;
    let error = revalidate_source_selected_folders(
        &mut changed_all,
        "imap.example.test",
        &budget,
        &expected,
        true,
        "source",
    )
    .unwrap_err();
    assert!(
        error.contains("changed fields: UIDVALIDITY, UIDNEXT, EXISTS, HIGHESTMODSEQ"),
        "{error}"
    );
    for raw_cursor_pair in ["3->30", "42->420", "2->3", "Some(12)->Some(130)"] {
        assert!(
            !error.contains(raw_cursor_pair),
            "raw cursor pair {raw_cursor_pair} leaked in {error}"
        );
    }

    let mut changed = FolderServer::new(&[1, 2], 3, 42);
    changed.condstore = true;
    changed.modseq = 13;
    let error = revalidate_source_selected_folders(
        &mut changed,
        "imap.example.test",
        &budget,
        &expected,
        true,
        "source",
    )
    .unwrap_err();
    assert!(error.contains("source folder INBOX changed"), "{error}");
    assert!(error.contains("HIGHESTMODSEQ"), "{error}");
    assert!(
        !error.contains("12"),
        "snapshot values must not be logged: {error}"
    );
    assert!(
        !error.contains("13"),
        "snapshot values must not be logged: {error}"
    );

    let mut changed_destination = FolderServer::new(&[1, 2], 3, 42);
    changed_destination.condstore = true;
    changed_destination.modseq = 13;
    let error = revalidate_source_selected_folders(
        &mut changed_destination,
        "imap.example.test",
        &budget,
        &expected,
        true,
        "destination",
    )
    .unwrap_err();
    assert!(
        error.contains("destination folder INBOX changed"),
        "{error}"
    );
    assert!(error.contains("HIGHESTMODSEQ"), "{error}");
    assert!(
        !error.contains("12"),
        "snapshot values must not be logged: {error}"
    );
    assert!(
        !error.contains("13"),
        "snapshot values must not be logged: {error}"
    );
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
        let unread = &self.output[self.output_read..];
        let count = buffer.len().min(unread.len());
        buffer[..count].copy_from_slice(&unread[..count]);
        self.output_read += count;
        if self.output_read == self.output.len() {
            self.output.clear();
            self.output_read = 0;
        }
        Ok(count)
    }
}

struct DisconnectedStream;

enum SessionStream {
    Disconnected,
    Folder(Box<FolderServer>),
}

impl Write for DisconnectedStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for DisconnectedStream {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "scripted connection reset",
        ))
    }
}

impl Write for SessionStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Disconnected => Ok(bytes.len()),
            Self::Folder(server) => server.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Disconnected => Ok(()),
            Self::Folder(server) => server.flush(),
        }
    }
}

impl Read for SessionStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Disconnected => DisconnectedStream.read(buffer),
            Self::Folder(server) => server.read(buffer),
        }
    }
}

fn scan_with(
    stage: &mut MessageMetadataStage,
    server: &mut FolderServer,
    body_hash: bool,
    condstore_enabled: bool,
) -> u64 {
    let cancelled = AtomicBool::new(false);
    let budget = MessageFetchBudget::new(Duration::from_secs(10), &cancelled);
    let state_budget = MessageStateBudget::new();
    let body_budget = BodyHashBudget::new(1 << 30);
    let body_options = BodyHashOptions {
        max_body_bytes: 1 << 20,
        budget: &body_budget,
    };
    let mut sink = StageMessageSink {
        stage,
        side: SIDE,
        count: 0,
        mailbox_counts: HashMap::new(),
        active_mailbox: None,
    };
    fetch_mailbox_with_stability_retry(
        server,
        "imap.example.test",
        "INBOX",
        &budget,
        &state_budget,
        body_hash.then_some(&body_options),
        condstore_enabled,
        &mut sink,
    )
    .unwrap_or_else(|error| panic!("scan failed: {error:?}"))
}

fn scan(stage: &mut MessageMetadataStage, server: &mut FolderServer) -> u64 {
    scan_with(stage, server, false, false)
}

fn scan_with_session_reconnect<S, R>(
    stage: &mut MessageMetadataStage,
    stream: &mut S,
    condstore_enabled: &mut bool,
    reconnect: &mut R,
) -> Result<u64, MailboxFetchError>
where
    S: Read + Write,
    R: FnMut() -> Result<(S, bool), String>,
{
    let cancelled = AtomicBool::new(false);
    let budget = MessageFetchBudget::new(Duration::from_secs(10), &cancelled);
    let state_budget = MessageStateBudget::new();
    let count = stage.count(SIDE).unwrap() as usize;
    let mut sink = StageMessageSink {
        stage,
        side: SIDE,
        count,
        mailbox_counts: HashMap::new(),
        active_mailbox: None,
    };
    let mut consecutive_reconnects = 0;
    fetch_mailbox_with_session_reconnect(
        stream,
        "imap.example.test",
        "INBOX",
        &budget,
        &state_budget,
        None,
        condstore_enabled,
        &mut consecutive_reconnects,
        reconnect,
        &mut sink,
    )
}

#[test]
fn fatal_session_reauthenticates_and_retries_the_failed_folder() {
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    let mut stream = SessionStream::Disconnected;
    let mut condstore_enabled = false;
    let mut reconnect_calls = 0;
    let mut reconnect = || {
        reconnect_calls += 1;
        let mut server = FolderServer::new(&[1], 9, 2);
        server.condstore = true;
        server.flags.insert(1, "\\Seen $ClientTag".into());
        Ok((SessionStream::Folder(Box::new(server)), true))
    };

    assert_eq!(
        scan_with_session_reconnect(
            &mut stage,
            &mut stream,
            &mut condstore_enabled,
            &mut reconnect
        )
        .unwrap(),
        1
    );
    assert_eq!(reconnect_calls, 1);
    assert!(condstore_enabled, "capabilities must refresh after re-auth");
    let SessionStream::Folder(server) = stream else {
        panic!("reconnected session was not installed");
    };
    assert_eq!(server.fetch_commands, 1);
    let message = stage
        .all_messages(SIDE)
        .unwrap()
        .remove(&MailboxMessageKey::with_uidvalidity("INBOX", 9, "1"))
        .unwrap();
    let flags = message.flags.unwrap();
    assert!(flags.contains("\\Seen"));
    assert!(flags.contains("$clienttag"));
}

#[test]
fn reconnect_authentication_failure_keeps_the_folder_unverified() {
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    let mut stream = SessionStream::Disconnected;
    let mut condstore_enabled = false;
    let mut reconnect_calls = 0;
    let mut reconnect = || {
        reconnect_calls += 1;
        Err("scripted authentication rejected".to_owned())
    };

    let result = scan_with_session_reconnect(
        &mut stage,
        &mut stream,
        &mut condstore_enabled,
        &mut reconnect,
    );
    let Err(MailboxFetchError::SessionFatal(error)) = result else {
        panic!("reconnect authentication failure must remain a fatal session error");
    };
    assert!(error.contains("reconnect attempt 1 failed"));
    assert!(error.contains("authentication rejected"));
    assert_eq!(reconnect_calls, 1);
    assert_eq!(stage.count(SIDE).unwrap(), 0);
}

#[test]
fn session_reconnect_attempts_are_bounded() {
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    let mut stream = SessionStream::Disconnected;
    let mut condstore_enabled = false;
    let mut reconnect_calls = 0;
    let mut reconnect = || {
        reconnect_calls += 1;
        Ok((SessionStream::Disconnected, false))
    };

    assert!(matches!(
        scan_with_session_reconnect(
            &mut stage,
            &mut stream,
            &mut condstore_enabled,
            &mut reconnect
        ),
        Err(MailboxFetchError::SessionFatal(_))
    ));
    assert_eq!(reconnect_calls, 3);
    assert_eq!(stage.count(SIDE).unwrap(), 0);
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
                    flags: None,
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
    stage.begin_mailbox_scan(SIDE, "INBOX", snapshot).unwrap();
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
fn sparse_historical_uid_space_does_not_create_empty_search_windows() {
    let mut server = FolderServer::new(&[4_294_967_295], 9, 4_294_967_296);
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    assert_eq!(scan(&mut stage, &mut server), 1);
    assert_eq!(server.uid_enumeration_commands, 1);
    assert_eq!(staged_uids(&stage), vec![4_294_967_295]);
}

#[test]
fn unchanged_resume_rescans_pages_but_does_not_claim_untracked_flags() {
    let uids = folder(PAGE * 2 + 5);
    let before = FolderServer::new(&uids, 9, PAGE * 2 + 6);
    let mut stage = interrupted(&before, 1..=PAGE, Some(PAGE));
    let mut server = FolderServer::new(&uids, 9, PAGE * 2 + 6);
    // Only FLAGS change between attempts; UIDVALIDITY, UIDNEXT, and EXISTS
    // remain identical. A stale staged value must not survive the restart.
    let snapshot = server.snapshot();
    let mut old_page = staged_page(snapshot.uidvalidity, 1..=PAGE);
    old_page
        .values_mut()
        .for_each(|message| message.flags = Some(String::new()));
    stage.insert_messages(SIDE, &old_page).unwrap();
    server.flags.insert(1, "\\Seen".into());

    assert_eq!(scan(&mut stage, &mut server), uids.len() as u64);
    assert_eq!(server.fetched, uids);
    assert_eq!(staged_uids(&stage), uids);
    let message = stage
        .all_messages(SIDE)
        .unwrap()
        .into_iter()
        .find(|(key, _)| key.uid == "1")
        .unwrap()
        .1;
    assert_eq!(message.flags, None);
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
fn completed_folder_restart_rescans_an_unchanged_stage_for_flags() {
    let uids = folder(PAGE + 3);
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    scan(&mut stage, &mut FolderServer::new(&uids, 9, PAGE + 4));
    let mut server = FolderServer::new(&uids, 9, PAGE + 4);
    assert_eq!(scan(&mut stage, &mut server), uids.len() as u64);
    assert_eq!(server.fetched, uids);
    assert_eq!(staged_uids(&stage), uids);
}

#[test]
fn durable_completed_folder_reuses_only_an_identical_tracked_snapshot() {
    let directory = crate::credentials::create_secret_directory().unwrap();
    let path = directory.join("verification.sqlite");
    let uids = vec![1, 2, 3];
    let mut first = FolderServer::new(&uids, 9, 4);
    first.condstore = true;
    first.modseq = 12;
    first.flags.insert(1, "\\Seen $ClientTag".into());
    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    assert_eq!(scan_with(&mut stage, &mut first, false, true), 3);
    assert_eq!(first.fetched, uids);
    drop(stage);

    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_verification_attempt().unwrap();
    let mut resumed = FolderServer::new(&uids, 9, 4);
    resumed.condstore = true;
    resumed.modseq = 12;
    resumed.flags.insert(1, "\\Seen $ClientTag".into());
    assert_eq!(scan_with(&mut stage, &mut resumed, false, true), 3);
    assert!(resumed.fetched.is_empty());
    assert_eq!(resumed.fetch_commands, 0);
    assert_eq!(resumed.uid_enumeration_commands, 0);
    assert_eq!(staged_uids(&stage), uids);
    let message = stage
        .all_messages(SIDE)
        .unwrap()
        .into_iter()
        .find(|(key, _)| key.uid == "1")
        .unwrap()
        .1;
    assert_eq!(message.flags.as_deref(), Some("$clienttag \\Seen"));
    stage.finish().unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn durable_completed_folder_with_modseq_drift_is_refetched() {
    let directory = crate::credentials::create_secret_directory().unwrap();
    let path = directory.join("verification.sqlite");
    let uids = vec![1, 2, 3];
    let mut first = FolderServer::new(&uids, 9, 4);
    first.condstore = true;
    first.modseq = 12;
    first.flags.insert(1, String::new());
    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    assert_eq!(scan_with(&mut stage, &mut first, false, true), 3);
    drop(stage);

    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_verification_attempt().unwrap();
    let mut resumed = FolderServer::new(&uids, 9, 4);
    resumed.condstore = true;
    resumed.modseq = 13;
    resumed.flags.insert(1, "\\Seen".into());
    assert_eq!(scan_with(&mut stage, &mut resumed, false, true), 3);
    assert_eq!(resumed.fetched, uids);
    let message = stage
        .all_messages(SIDE)
        .unwrap()
        .into_iter()
        .find(|(key, _)| key.uid == "1")
        .unwrap()
        .1;
    assert_eq!(message.flags.as_deref(), Some("\\Seen"));
    stage.finish().unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn durable_completed_folder_without_modseq_is_refetched() {
    let directory = crate::credentials::create_secret_directory().unwrap();
    let path = directory.join("verification.sqlite");
    let uids = vec![1, 2, 3];
    let mut first = FolderServer::new(&uids, 9, 4);
    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    assert_eq!(scan(&mut stage, &mut first), 3);
    drop(stage);

    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_verification_attempt().unwrap();
    let mut resumed = FolderServer::new(&uids, 9, 4);
    assert_eq!(scan(&mut stage, &mut resumed), 3);
    assert_eq!(resumed.fetched, uids);
    assert_eq!(resumed.uid_enumeration_commands, 1);
    stage.finish().unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn durable_partial_folder_resumes_at_the_next_sequence_after_snapshot_validation() {
    let directory = crate::credentials::create_secret_directory().unwrap();
    let path = directory.join("verification.sqlite");
    let uids = (1..=10).collect::<Vec<_>>();
    let mut original_server = FolderServer::new(&uids, 9, 11);
    original_server.condstore = true;
    original_server.modseq = 22;
    let snapshot = original_server.snapshot();
    let mut first_page = staged_page(9, 1..=3);
    first_page
        .values_mut()
        .for_each(|message| message.flags = Some("$clienttag \\Seen".into()));
    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_mailbox_scan(SIDE, "INBOX", snapshot).unwrap();
    stage
        .insert_page_with_fingerprints(
            SIDE,
            &first_page,
            &HashMap::new(),
            Some(("INBOX", snapshot, 3)),
        )
        .unwrap();
    drop(stage);

    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_verification_attempt().unwrap();
    let mut resumed = FolderServer::new(&uids, 9, 11);
    resumed.condstore = true;
    resumed.modseq = 22;
    resumed.flags.insert(1, "$clienttag \\Seen".into());
    assert_eq!(scan_with(&mut stage, &mut resumed, false, true), 10);
    assert_eq!(resumed.fetched, (4..=10).collect::<Vec<_>>());
    assert_eq!(resumed.uid_enumeration_commands, 1);
    assert!(
        resumed
            .commands
            .iter()
            .any(|command| { command.contains("FETCH 4:10 (UID)") })
    );
    assert!(
        !resumed
            .commands
            .iter()
            .any(|command| command.contains("FETCH 1:10 (UID)"))
    );
    assert_eq!(staged_uids(&stage), uids);
    let messages = stage.all_messages(SIDE).unwrap();
    for uid in 1..=3 {
        let message = messages
            .iter()
            .find(|(key, _)| key.uid == uid.to_string())
            .unwrap()
            .1;
        assert_eq!(message.flags.as_deref(), Some("$clienttag \\Seen"));
    }
    stage.finish().unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn durable_partial_folder_resumes_by_sequence_with_sparse_uids() {
    let directory = crate::credentials::create_secret_directory().unwrap();
    let path = directory.join("verification.sqlite");
    let uids = vec![3, 80, 500, 2_000, 50_000];
    let mut original_server = FolderServer::new(&uids, 9, 50_001);
    original_server.condstore = true;
    original_server.modseq = 22;
    let snapshot = original_server.snapshot();
    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_mailbox_scan(SIDE, "INBOX", snapshot).unwrap();
    stage
        .insert_page_with_fingerprints(
            SIDE,
            &staged_page(9, [3, 80, 500]),
            &HashMap::new(),
            Some(("INBOX", snapshot, 500)),
        )
        .unwrap();
    drop(stage);

    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_verification_attempt().unwrap();
    let mut resumed = FolderServer::new(&uids, 9, 50_001);
    resumed.condstore = true;
    resumed.modseq = 22;
    assert_eq!(
        scan_with(&mut stage, &mut resumed, false, true),
        uids.len() as u64
    );
    assert_eq!(resumed.fetched, [2_000, 50_000]);
    assert!(
        resumed
            .commands
            .iter()
            .any(|command| { command.contains("FETCH 4:5 (UID)") })
    );
    assert_eq!(staged_uids(&stage), uids);
    stage.finish().unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn durable_partial_folder_with_inconsistent_cursor_is_refetched_from_start() {
    let directory = crate::credentials::create_secret_directory().unwrap();
    let path = directory.join("verification.sqlite");
    let uids = vec![3, 80, 500, 2_000, 50_000];
    let mut original_server = FolderServer::new(&uids, 9, 50_001);
    original_server.condstore = true;
    original_server.modseq = 22;
    let snapshot = original_server.snapshot();
    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_mailbox_scan(SIDE, "INBOX", snapshot).unwrap();
    // Simulate a stage whose committed cursor does not describe its page.
    // Normal writes commit these atomically; resume must still fail closed if
    // the persisted state is inconsistent.
    stage
        .insert_page_with_fingerprints(
            SIDE,
            &staged_page(9, [3, 80, 500]),
            &HashMap::new(),
            Some(("INBOX", snapshot, 499)),
        )
        .unwrap();
    drop(stage);

    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_verification_attempt().unwrap();
    let mut resumed = FolderServer::new(&uids, 9, 50_001);
    resumed.condstore = true;
    resumed.modseq = 22;
    assert_eq!(
        scan_with(&mut stage, &mut resumed, false, true),
        uids.len() as u64
    );
    assert_eq!(resumed.fetched, uids);
    assert!(
        resumed
            .commands
            .iter()
            .any(|command| command.contains("FETCH 1:5 (UID)"))
    );
    assert_eq!(staged_uids(&stage), uids);
    stage.finish().unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn durable_partial_folder_with_modseq_drift_restarts_from_the_first_sequence() {
    let directory = crate::credentials::create_secret_directory().unwrap();
    let path = directory.join("verification.sqlite");
    let uids = (1..=10).collect::<Vec<_>>();
    let mut original_server = FolderServer::new(&uids, 9, 11);
    original_server.condstore = true;
    original_server.modseq = 22;
    let snapshot = original_server.snapshot();
    let mut first_page = staged_page(9, 1..=3);
    first_page
        .values_mut()
        .for_each(|message| message.flags = Some(String::new()));
    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_mailbox_scan(SIDE, "INBOX", snapshot).unwrap();
    stage
        .insert_page_with_fingerprints(
            SIDE,
            &first_page,
            &HashMap::new(),
            Some(("INBOX", snapshot, 3)),
        )
        .unwrap();
    drop(stage);

    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_verification_attempt().unwrap();
    let mut resumed = FolderServer::new(&uids, 9, 11);
    resumed.condstore = true;
    resumed.modseq = 23;
    resumed.flags.insert(1, "\\Seen".into());
    assert_eq!(scan_with(&mut stage, &mut resumed, false, true), 10);
    assert_eq!(resumed.fetched, uids);
    assert!(
        resumed
            .commands
            .iter()
            .any(|command| { command.contains("FETCH 1:10 (UID)") })
    );
    let messages = stage.all_messages(SIDE).unwrap();
    let first_message = messages.iter().find(|(key, _)| key.uid == "1").unwrap().1;
    assert_eq!(first_message.flags.as_deref(), Some("\\Seen"));
    stage.finish().unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn durable_partial_folder_without_modseq_restarts_from_the_first_sequence() {
    let directory = crate::credentials::create_secret_directory().unwrap();
    let path = directory.join("verification.sqlite");
    let uids = (1..=10).collect::<Vec<_>>();
    let original_server = FolderServer::new(&uids, 9, 11);
    let snapshot = original_server.snapshot();
    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_mailbox_scan(SIDE, "INBOX", snapshot).unwrap();
    stage
        .insert_page_with_fingerprints(
            SIDE,
            &staged_page(9, 1..=3),
            &HashMap::new(),
            Some(("INBOX", snapshot, 3)),
        )
        .unwrap();
    drop(stage);

    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_verification_attempt().unwrap();
    let mut resumed = FolderServer::new(&uids, 9, 11);
    assert_eq!(scan(&mut stage, &mut resumed), 10);
    assert_eq!(resumed.fetched, uids);
    assert!(
        resumed
            .commands
            .iter()
            .any(|command| command.contains("FETCH 1:10 (UID)"))
    );
    stage.finish().unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn durable_body_hashes_are_never_reused_after_restart() {
    let directory = crate::credentials::create_secret_directory().unwrap();
    let path = directory.join("verification.sqlite");
    let uids = vec![1, 2, 3];
    let mut first = FolderServer::new(&uids, 9, 4);
    first.condstore = true;
    first.modseq = 12;
    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    assert_eq!(scan_with(&mut stage, &mut first, true, true), 3);
    let original_fingerprints = stage.content_fingerprints(SIDE);
    assert_eq!(original_fingerprints.len(), uids.len());
    drop(stage);

    let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
    stage.begin_verification_attempt().unwrap();
    assert!(stage.content_fingerprints(SIDE).is_empty());
    let mut resumed = FolderServer::new(&uids, 9, 4);
    resumed.condstore = true;
    resumed.modseq = 12;
    assert_eq!(scan_with(&mut stage, &mut resumed, true, true), 3);
    assert_eq!(resumed.fetched, uids);
    assert_eq!(stage.content_fingerprints(SIDE), original_fingerprints);
    stage.finish().unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn flags_are_not_certified_without_condstore_after_resume() {
    let uids = vec![1, 2, 3];
    let mut first = FolderServer::new(&uids, 9, 4);
    first.flags.insert(1, "".into());
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    scan(&mut stage, &mut first);

    // UIDVALIDITY, UIDNEXT, and EXISTS are unchanged; only FLAGS changed.
    let mut resumed = FolderServer::new(&uids, 9, 4);
    resumed.flags.insert(1, "\\Seen $ClientTag".into());
    resumed.unsolicited_flags = true;
    scan(&mut stage, &mut resumed);

    assert_eq!(resumed.fetched, uids);
    assert_eq!(resumed.flags_only_fetch_commands, 0);
    let messages = stage.all_messages(SIDE).unwrap();
    let message = messages
        .iter()
        .find(|(key, _)| key.uid == "1")
        .map(|(_, message)| message)
        .unwrap();
    assert_eq!(message.flags, None);
}

#[test]
fn flags_mutation_during_scan_discards_pass_and_retries_with_fresh_flags() {
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    let mut server = FolderServer::new(&[1], 7, 2);
    server.condstore = true;
    server.flags.insert(1, "".into());
    server.change_flags_after_metadata_fetch = Some((1, "\\Seen $ClientTag".into()));

    scan_with(&mut stage, &mut server, false, true);

    let messages = stage.all_messages(SIDE).unwrap();
    let message = messages
        .iter()
        .find(|(key, _)| key.uid == "1")
        .map(|(_, message)| message)
        .unwrap();
    let flags = message.flags.as_deref().unwrap();
    assert!(flags.contains("\\Seen"));
    assert!(flags.contains("$clienttag"));
    assert_eq!(
        server.fetch_commands, 2,
        "changed MODSEQ retries the folder"
    );
    assert_eq!(server.flags_only_fetch_commands, 0);
}

#[test]
fn condstore_modseq_detects_flag_mutation_without_refetching_every_uid() {
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    let mut server = FolderServer::new(&[1], 7, 2);
    server.condstore = true;
    server.flags.insert(1, "".into());
    server.change_flags_after_metadata_fetch = Some((1, "\\Seen $ClientTag".into()));

    scan_with(&mut stage, &mut server, false, true);

    let message = stage
        .all_messages(SIDE)
        .unwrap()
        .into_iter()
        .find(|(key, _)| key.uid == "1")
        .map(|(_, message)| message)
        .unwrap();
    let flags = message.flags.as_deref().unwrap();
    assert!(flags.contains("\\Seen"));
    assert!(flags.contains("$clienttag"));
    assert_eq!(server.fetch_commands, 2, "modseq change retries the folder");
    assert_eq!(server.flags_only_fetch_commands, 0);
}

#[test]
fn condstore_nomodseq_does_not_claim_flag_fidelity() {
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    let mut server = FolderServer::new(&[1], 7, 2);
    server.condstore = true;
    server.modseq_unavailable = true;
    server.flags.insert(1, "".into());
    scan_with(&mut stage, &mut server, false, true);

    let message = stage
        .all_messages(SIDE)
        .unwrap()
        .into_iter()
        .find(|(key, _)| key.uid == "1")
        .map(|(_, message)| message)
        .unwrap();
    assert_eq!(message.flags, None);
    assert_eq!(server.flags_only_fetch_commands, 0);
}

#[test]
fn condstore_select_snapshot_requires_one_valid_modseq_code() {
    assert_eq!(
        parse_select_modseq(
            "* OK [HIGHESTMODSEQ 42] current\r\na001 OK selected\r\n",
            "imap.example.test",
            "INBOX"
        )
        .unwrap(),
        ModseqSnapshot::Tracked(42)
    );
    assert_eq!(
        parse_select_modseq(
            "* OK [NOMODSEQ] unavailable\r\na001 OK selected\r\n",
            "imap.example.test",
            "INBOX"
        )
        .unwrap(),
        ModseqSnapshot::Unsupported
    );
    for response in [
        "a001 OK selected\r\n",
        "* OK [HIGHESTMODSEQ 0] invalid\r\na001 OK selected\r\n",
        "* OK [HIGHESTMODSEQ +42] invalid\r\na001 OK selected\r\n",
        "* OK HIGHESTMODSEQ 42\r\na001 OK selected\r\n",
        "* 1 FETCH (HIGHESTMODSEQ 42)\r\na001 OK selected\r\n",
        "a001 OK [HIGHESTMODSEQ 42] selected\r\n",
        "* OK [HIGHESTMODSEQ 42 extra] invalid\r\na001 OK selected\r\n",
        "* OK [HIGHESTMODSEQ 42]text\r\na001 OK selected\r\n",
        "* OK [HIGHESTMODSEQ 42] current\r\n* OK [NOMODSEQ] conflicting\r\n",
        "* OK [HIGHESTMODSEQ 42] current\r\n* OK [HIGHESTMODSEQ 43] duplicate\r\n",
        "* OK [NOMODSEQ] unavailable\r\n* OK [NOMODSEQ] duplicate\r\n",
    ] {
        assert!(
            parse_select_modseq(response, "imap.example.test", "INBOX").is_err(),
            "accepted invalid response: {response:?}"
        );
    }
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
fn body_hash_rescan_replaces_partially_staged_pages_and_fingerprints() {
    let uids = folder(PAGE * 2 + 5);
    let before = FolderServer::new(&uids, 9, PAGE * 2 + 6);
    // Simulate a crash after page rows committed but before fingerprints and
    // the cursor were written. Starting a new scan discards all old rows.
    let mut stage = interrupted(&before, 1..=PAGE, None);
    assert!(stage.content_fingerprints(SIDE).is_empty());
    let mut server = FolderServer::new(&uids, 9, PAGE * 2 + 6);
    scan_with(&mut stage, &mut server, true, false);
    assert_eq!(staged_uids(&stage), uids);
    assert_eq!(stage.content_fingerprints(SIDE).len(), uids.len());
}

#[test]
fn body_hash_rescan_refetches_checkpointed_pages_and_fingerprints() {
    let uids = folder(PAGE * 2 + 5);
    let mut first = FolderServer::new(&uids, 9, PAGE * 2 + 6);
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    scan_with(&mut stage, &mut first, true, false);
    let fingerprints = stage.content_fingerprints(SIDE);
    assert_eq!(fingerprints.len(), uids.len());
    let mut server = FolderServer::new(&uids, 9, PAGE * 2 + 6);
    scan_with(&mut stage, &mut server, true, false);
    assert_eq!(server.fetched, uids);
    assert_eq!(stage.content_fingerprints(SIDE), fingerprints);
}

/// Stages one account side from a scripted folder through the real fetch path.
fn scan_side(
    stage: &mut MessageMetadataStage,
    side: StagedMessageSide,
    server: &mut FolderServer,
) -> Result<u64, String> {
    let cancelled = AtomicBool::new(false);
    let budget = MessageFetchBudget::new(Duration::from_secs(3_600), &cancelled);
    let state_budget = MessageStateBudget::new();
    let mut sink = StageMessageSink {
        stage,
        side,
        count: 0,
        mailbox_counts: HashMap::new(),
        active_mailbox: None,
    };
    fetch_mailbox_with_stability_retry(
        server,
        "imap.example.test",
        "INBOX",
        &budget,
        &state_budget,
        None,
        false,
        &mut sink,
    )
    .map_err(|error| format!("{error:?}"))
}

/// Above the ephemeral 1,000,000-message limit, a durable stage fetches and
/// reconciles every message of both accounts through the real IMAP fetch
/// path, and an ephemeral stage still refuses. Opt-in because it stages two
/// million-plus-row accounts; run it with
/// `cargo test --release --bin mailswiftsync above_the_ephemeral_limit -- --ignored`.
#[test]
#[ignore = "opt-in above-limit verification proof (about 2 GB of temporary stage)"]
fn a_durable_stage_verifies_an_account_above_the_ephemeral_limit() {
    let messages = std::env::var("MAILSWIFTSYNC_ABOVE_LIMIT_MESSAGES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1_000_100);
    assert!(messages > MAX_MESSAGE_FETCH_RECORDS as u64);
    let directory = std::env::temp_dir().join(format!("mss-above-limit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let started = std::time::Instant::now();
    let mut stage =
        MessageMetadataStage::open_durable(directory.join("stage.sqlite"), "above-limit").unwrap();
    for side in [StagedMessageSide::Source, StagedMessageSide::Destination] {
        let mut server = FolderServer::new(&folder(messages), 9, messages + 1);
        assert_eq!(scan_side(&mut stage, side, &mut server), Ok(messages));
        assert_eq!(stage.count(side).unwrap(), messages);
    }
    let staged = started.elapsed();
    let (mismatches, summary) = crate::core::MessageVerification::reconcile_stage(
        "job-above-limit",
        "run-above-limit",
        &stage,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(mismatches, 0);
    assert_eq!(summary.total_source, messages);
    assert_eq!(summary.total_destination, messages);
    // Exact coverage: every source message has its exact metadata pair.
    assert_eq!(summary.metadata_matches, messages);
    // Print only the generated count and timings; the assertions above
    // carry the coverage result, so nothing derived from the reconcile
    // summary reaches the log.
    let staged_ms = staged.as_millis();
    let total_ms = started.elapsed().as_millis();
    println!("above-limit messages_per_side={messages} staged_ms={staged_ms} total_ms={total_ms}");
    drop(stage);
    std::fs::remove_dir_all(&directory).unwrap();

    // The same account on an ephemeral stage still stops at the old limit.
    let mut ephemeral = MessageMetadataStage::open_in_memory().unwrap();
    let mut server = FolderServer::new(&folder(messages), 9, messages + 1);
    let error = scan_side(&mut ephemeral, StagedMessageSide::Source, &mut server).unwrap_err();
    assert!(
        error.contains("verification_limit=message_count"),
        "{error}"
    );
    assert!(error.contains("1000000-message"), "{error}");
}
