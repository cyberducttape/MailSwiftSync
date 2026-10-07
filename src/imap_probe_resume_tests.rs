//! Restart state machine for durable verification staging. Each test
//! reproduces what a killed controller leaves in the stage, changes the
//! server between runs, and drives the real fetch path against a scripted
//! IMAP server. The invariant under test: after a scan the staged folder
//! equals the server's current folder exactly, whatever happened between
//! runs, and unchanged pages are not fetched again.
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

/// Minimal scripted IMAP server for one folder: answers SELECT, UID
/// SEARCH, and UID FETCH (metadata and optional BODY[]) from an in-memory
/// UID set, recording every UID it was asked to fetch. Messages listed in
/// `flags` carry a FLAGS item; SELECT advertises `permanent_flags` if set.
pub(super) struct FolderServer {
    uids: Vec<u64>,
    uidvalidity: u64,
    uidnext: u64,
    fetched: Vec<u64>,
    fetch_commands: usize,
    pub(super) flags: std::collections::HashMap<u64, String>,
    pub(super) permanent_flags: Option<String>,
    input: Vec<u8>,
    output: Vec<u8>,
}

impl FolderServer {
    pub(super) fn new(uids: &[u64], uidvalidity: u64, uidnext: u64) -> Self {
        Self {
            uids: uids.to_vec(),
            uidvalidity,
            uidnext,
            fetched: Vec::new(),
            fetch_commands: 0,
            flags: std::collections::HashMap::new(),
            permanent_flags: None,
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
            let permanent = self
                .permanent_flags
                .as_ref()
                .map(|flags| format!("* OK [PERMANENTFLAGS ({flags})] ok\r\n"))
                .unwrap_or_default();
            format!(
                "* {} EXISTS\r\n{permanent}* OK [UIDVALIDITY {}] ok\r\n* OK [UIDNEXT {}] ok\r\n{tag} OK SELECT completed\r\n",
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
                let flags = self
                    .flags
                    .get(&uid)
                    .map(|flags| format!("FLAGS ({flags}) "))
                    .unwrap_or_default();
                reply.push_str(&format!(
                        "* {sequence} FETCH (UID {uid} {flags}RFC822.SIZE 10 INTERNALDATE \"01-Jan-2024 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}}}\r\n{header}{body})\r\n",
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

fn scan_with(stage: &mut MessageMetadataStage, server: &mut FolderServer, body_hash: bool) -> u64 {
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
