use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::Arc;

use chrono::{DateTime, FixedOffset};
use rusqlite::{OptionalExtension, params};

use super::message_extraction::ExtractedMessages;
use super::{
    evidence::VerificationOutcome,
    message_extraction::{ExtractedMessage, MailboxMessageKey},
    message_staging::{
        MessageMetadataStage, StagedMessage, StagedMessageSide, staged_message_from_row,
        staged_message_pair_from_row,
    },
};

/// Conservative admission budget for the combined source/destination state
/// used by reconciliation. This is an estimate, not a process-wide peak RSS
/// guarantee: hash tables, indexes, classification sets, and mismatch
/// evidence can have allocator overhead beyond the per-record estimate.
const MAX_ESTIMATED_VERIFIER_STATE_BYTES: usize = 256 * 1024 * 1024;
// Each fetched record participates in several borrowed indexes and
// classification sets during reconciliation. This is deliberately
// conservative: it accounts for hash-table entries, references, and
// temporary membership bookkeeping that are not represented by the record
// estimate itself.
const ESTIMATED_RECONCILIATION_INDEX_BYTES_PER_RECORD: usize = 128;
/// Bound the owned mismatch evidence retained before the durable SQLite
/// transaction. This is separate from fetched-state admission because a
/// mismatch-heavy account owns additional strings for every detail row.
const MAX_ESTIMATED_MISMATCH_DETAIL_BYTES: usize = 64 * 1024 * 1024;
mod fingerprints;
mod flags;
mod mismatch;
mod reconciliation;
mod staged;
mod summary;
pub use mismatch::*;
use reconciliation::*;
pub use summary::*;

fn build_uid_folder_index(
    messages: &ExtractedMessages,
) -> HashMap<(Option<u64>, &str, &str), &MailboxMessageKey> {
    messages
        .keys()
        .map(|key| {
            (
                (key.uidvalidity, key.uid.as_str(), key.mailbox.as_ref()),
                key,
            )
        })
        .collect()
}

/// Core message verification engine.
pub struct MessageVerification;

type Pass1Result<'a> = Result<
    (
        Vec<MessageMismatch>,
        HashSet<&'a MailboxMessageKey>,
        HashSet<&'a MailboxMessageKey>,
        Vec<(&'a MailboxMessageKey, &'a MailboxMessageKey)>,
    ),
    String,
>;

/// A validated metadata reconciliation, including the exact source and
/// destination pairs it established. Content verification enriches these
/// pairs; it never forms pairings of its own.
struct MetadataReconciliation<'a> {
    mismatches: Vec<MessageMismatch>,
    summary: VerificationSummary,
    membership: VerificationMembership<'a>,
    /// Message-ID, expected folder, and exact metadata agree.
    clean_pairs: Vec<(&'a MailboxMessageKey, &'a MailboxMessageKey)>,
}

type ReconciliationPassResult<'a> = Result<
    (
        Vec<MessageMismatch>,
        HashSet<&'a MailboxMessageKey>,
        HashSet<&'a MailboxMessageKey>,
    ),
    String,
>;

impl MessageVerification {}

#[cfg(test)]
#[path = "message_verification/tests.rs"]
mod tests;
