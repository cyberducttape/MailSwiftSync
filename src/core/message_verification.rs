#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::Arc;

use chrono::{DateTime, FixedOffset};
use rusqlite::{OptionalExtension, params};

use super::{
    evidence::VerificationOutcome,
    message_extraction::{ExtractedMessage, ExtractedMessages, MailboxMessageKey},
    message_staging::{
        MessageMetadataStage, StagedMessage, StagedMessageSide, staged_message_from_row,
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

/// Mismatch classifications currently supported by the executable verifier.
///
/// Exact and date/size matches are successful match outcomes represented in
/// `VerificationSummary`, not mismatch records. Content-hash and folder-aware
/// classifications are intentionally not represented here until extraction
/// supplies those fields and the live path persists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MismatchType {
    /// Message-ID matches but available metadata differs.
    MessageIdOnly,
    /// The message is present with matching metadata, but not in its expected
    /// destination folder.
    PresentWrongFolder,
    /// Present in source, absent in destination
    Missing,
    /// Present in destination, absent in source
    Extra,
    /// Multiple instances of same message-ID in destination
    Duplicated,
}

impl MismatchType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MessageIdOnly => "message_id_only",
            Self::PresentWrongFolder => "message_present_wrong_folder",
            Self::Missing => "missing",
            Self::Extra => "extra",
            Self::Duplicated => "duplicated",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "message_id_only" => Self::MessageIdOnly,
            "message_present_wrong_folder" => Self::PresentWrongFolder,
            "missing" => Self::Missing,
            "extra" => Self::Extra,
            "duplicated" => Self::Duplicated,
            _ => return None,
        })
    }
}

/// Mismatch record to persist in the database.
#[derive(Debug, Clone)]
pub struct MessageMismatch {
    pub id: String,
    pub job_id: Arc<str>,
    pub run_id: Arc<str>,
    pub mismatch_type: MismatchType,
    pub source_folder: Option<String>,
    pub destination_folder: Option<String>,
    pub source_uidvalidity: Option<u64>,
    pub destination_uidvalidity: Option<u64>,
    pub source_uid: Option<String>,
    pub dest_uid: Option<String>,
    pub source_message_id: Option<String>,
    pub dest_message_id: Option<String>,
    pub source_size_bytes: Option<u64>,
    pub dest_size_bytes: Option<u64>,
    pub source_date: Option<String>,
    pub dest_date: Option<String>,
    pub source_fingerprint: Option<String>,
    pub destination_fingerprint: Option<String>,
}

/// Identity ownership produced by reconciliation. Every input key must occur
/// in exactly one source or destination classification.
#[derive(Debug, Clone, Default)]
pub struct VerificationMembership<'a> {
    pub matched_source: HashSet<&'a MailboxMessageKey>,
    pub matched_destination: HashSet<&'a MailboxMessageKey>,
    pub probable_source: HashSet<&'a MailboxMessageKey>,
    pub probable_destination: HashSet<&'a MailboxMessageKey>,
    pub missing_source: HashSet<&'a MailboxMessageKey>,
    pub extra_destination: HashSet<&'a MailboxMessageKey>,
    pub duplicated_destination: HashSet<&'a MailboxMessageKey>,
    pub changed_source: HashSet<&'a MailboxMessageKey>,
    pub changed_destination: HashSet<&'a MailboxMessageKey>,
}

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

fn mismatch_source_key<'a>(
    mismatch: &MessageMismatch,
    index: &HashMap<(Option<u64>, &str, &str), &'a MailboxMessageKey>,
) -> Option<&'a MailboxMessageKey> {
    let (uid, folder) = mismatch
        .source_uid
        .as_ref()
        .zip(mismatch.source_folder.as_ref())?;
    index
        .get(&(mismatch.source_uidvalidity, uid.as_str(), folder.as_str()))
        .copied()
}

fn mismatch_destination_key<'a>(
    mismatch: &MessageMismatch,
    index: &HashMap<(Option<u64>, &str, &str), &'a MailboxMessageKey>,
) -> Option<&'a MailboxMessageKey> {
    let (uid, folder) = mismatch
        .dest_uid
        .as_ref()
        .zip(mismatch.destination_folder.as_ref())?;
    index
        .get(&(
            mismatch.destination_uidvalidity,
            uid.as_str(),
            folder.as_str(),
        ))
        .copied()
}

fn estimated_verifier_record_bytes(key: &MailboxMessageKey, message: &ExtractedMessage) -> usize {
    512usize
        .saturating_add(key.mailbox.len())
        .saturating_add(key.uid.len())
        .saturating_add(message.message_id.as_deref().map_or(0, str::len))
        .saturating_add(message.internal_date.as_deref().map_or(0, str::len))
}

fn estimated_verifier_state_bytes(
    source_messages: &ExtractedMessages,
    dest_messages: &ExtractedMessages,
) -> usize {
    let record_bytes = source_messages
        .iter()
        .chain(dest_messages)
        .map(|(key, message)| estimated_verifier_record_bytes(key, message))
        .fold(0usize, |total, record| {
            total.saturating_add(record.saturating_mul(2))
        });
    let record_count = source_messages.len().saturating_add(dest_messages.len());
    record_bytes.saturating_add(
        record_count.saturating_mul(ESTIMATED_RECONCILIATION_INDEX_BYTES_PER_RECORD),
    )
}

fn enforce_verifier_state_budget(estimated_state_bytes: usize) -> Result<(), String> {
    if estimated_state_bytes > MAX_ESTIMATED_VERIFIER_STATE_BYTES {
        return Err(format!(
            "verification state exceeds the estimated {}-byte aggregate verifier budget",
            MAX_ESTIMATED_VERIFIER_STATE_BYTES
        ));
    }
    Ok(())
}

fn estimated_mismatch_bytes(mismatch: &MessageMismatch) -> usize {
    384usize
        .saturating_add(mismatch.id.len())
        .saturating_add(mismatch.job_id.len())
        .saturating_add(mismatch.run_id.len())
        .saturating_add(mismatch.source_folder.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.destination_folder.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.source_uid.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.dest_uid.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.source_message_id.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.dest_message_id.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.source_date.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.dest_date.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.source_fingerprint.as_deref().map_or(0, str::len))
        .saturating_add(
            mismatch
                .destination_fingerprint
                .as_deref()
                .map_or(0, str::len),
        )
}

fn append_mismatch_with_budget(
    mismatches: &mut Vec<MessageMismatch>,
    mismatch: MessageMismatch,
    estimated_bytes: &mut usize,
) -> Result<(), String> {
    *estimated_bytes = estimated_bytes.saturating_add(estimated_mismatch_bytes(&mismatch));
    if *estimated_bytes > MAX_ESTIMATED_MISMATCH_DETAIL_BYTES {
        return Err(format!(
            "verification mismatch detail exceeds the estimated {}-byte evidence budget",
            MAX_ESTIMATED_MISMATCH_DETAIL_BYTES
        ));
    }
    mismatches.push(mismatch);
    Ok(())
}

/// Core message verification engine.
pub struct MessageVerification;

type ReconciliationPassResult<'a> = Result<
    (
        Vec<MessageMismatch>,
        HashSet<&'a MailboxMessageKey>,
        HashSet<&'a MailboxMessageKey>,
    ),
    String,
>;

impl MessageVerification {
    /// Detect mismatches between source and destination message sets.
    ///
    /// IMAP UIDs are mailbox-local and are deliberately never used as the
    /// identity across the two inputs. Messages are matched first by a unique
    /// RFC Message-ID, then by a unique internal-date/size fingerprint. A
    /// UID is retained only as diagnostic context in the result.
    #[cfg(test)]
    pub fn detect_mismatches(
        job_id: &str,
        run_id: &str,
        source_messages: &ExtractedMessages,
        dest_messages: &ExtractedMessages,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        Self::detect_mismatches_with_folder_mapping(
            job_id,
            run_id,
            source_messages,
            dest_messages,
            &HashMap::new(),
        )
    }

    /// Reconcile metadata first, then classify same-identity messages whose
    /// independently fetched RFC822 bytes differ. Content hashes are only
    /// authoritative when both sides supplied a bounded SHA-256 fingerprint;
    /// missing fingerprints preserve the existing metadata-only result.
    #[cfg(test)]
    pub fn detect_mismatches_with_content_fingerprints(
        job_id: &str,
        run_id: &str,
        source_messages: &ExtractedMessages,
        dest_messages: &ExtractedMessages,
        source_fingerprints: &HashMap<MailboxMessageKey, String>,
        dest_fingerprints: &HashMap<MailboxMessageKey, String>,
        folder_mapping: &HashMap<String, String>,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        let job_context: Arc<str> = Arc::from(job_id);
        let run_context: Arc<str> = Arc::from(run_id);
        let (mut mismatches, mut summary) = Self::detect_mismatches_with_folder_mapping(
            job_id,
            run_id,
            source_messages,
            dest_messages,
            folder_mapping,
        )?;
        // A stable content fingerprint is a valid fallback identity when
        // Message-ID is absent. Only unique hashes are eligible, and folder
        // placement still has to agree with the migration mapping.
        let source_by_fingerprint = unique_fingerprint_index(source_fingerprints);
        let dest_by_fingerprint = unique_fingerprint_index(dest_fingerprints);
        let source_index = build_uid_folder_index(source_messages);
        let dest_index = build_uid_folder_index(dest_messages);
        let mut mismatch_by_source = HashMap::<MailboxMessageKey, HashSet<usize>>::new();
        let mut mismatch_by_destination = HashMap::<MailboxMessageKey, HashSet<usize>>::new();
        for (index, mismatch) in mismatches.iter().enumerate() {
            if let Some(key) = mismatch_source_key(mismatch, &source_index) {
                mismatch_by_source
                    .entry(key.clone())
                    .or_default()
                    .insert(index);
            }
            if let Some(key) = mismatch_destination_key(mismatch, &dest_index) {
                mismatch_by_destination
                    .entry(key.clone())
                    .or_default()
                    .insert(index);
            }
        }
        let mut resolved_mismatches = HashSet::new();
        for (fingerprint, source_key) in source_by_fingerprint {
            let Some(dest_key) = dest_by_fingerprint.get(&fingerprint).copied() else {
                continue;
            };
            if expected_destination_folder(source_key, folder_mapping) != dest_key.mailbox.as_ref()
            {
                continue;
            }
            let source_mismatches = mismatch_by_source
                .get(source_key)
                .cloned()
                .unwrap_or_default();
            let destination_mismatches = mismatch_by_destination
                .get(dest_key)
                .cloned()
                .unwrap_or_default();
            let source_present = !source_mismatches.is_empty();
            let dest_present = !destination_mismatches.is_empty();
            if !source_present || !dest_present {
                continue;
            }
            // A content match cannot erase a stronger semantic mismatch such
            // as a same-ID metadata change or a wrong-folder result. Only the
            // Missing+Extra pair created by identity uncertainty is eligible
            // for resolution by a unique content fingerprint.
            let has_other_mismatch = source_mismatches.iter().any(|index| {
                destination_mismatches.contains(index)
                    && !matches!(
                        mismatches[*index].mismatch_type,
                        MismatchType::Missing | MismatchType::Extra
                    )
            });
            if has_other_mismatch {
                continue;
            }
            for index in source_mismatches.into_iter().chain(destination_mismatches) {
                if matches!(
                    mismatches[index].mismatch_type,
                    MismatchType::Missing | MismatchType::Extra
                ) {
                    resolved_mismatches.insert(index);
                }
            }
            summary.metadata_matches += 1;
            summary.probable_matches = summary.probable_matches.saturating_sub(1);
            summary.missing_count = summary.missing_count.saturating_sub(1);
            summary.extra_count = summary.extra_count.saturating_sub(1);
        }
        if !resolved_mismatches.is_empty() {
            mismatches = mismatches
                .into_iter()
                .enumerate()
                .filter_map(|(index, mismatch)| {
                    (!resolved_mismatches.contains(&index)).then_some(mismatch)
                })
                .collect();
        }
        let dest_by_message_id = index_by_message_id(dest_messages);
        let mut destination_match_indexes = HashMap::new();
        for (message_id, dest_keys) in &dest_by_message_id {
            let mut destinations_by_metadata =
                HashMap::<MetadataFingerprint<'_>, VecDeque<&MailboxMessageKey>>::new();
            let mut destinations_by_content =
                HashMap::<(MetadataFingerprint<'_>, &str), VecDeque<&MailboxMessageKey>>::new();
            for dest_key in dest_keys {
                let Some(destination_metadata) = metadata_fingerprint(&dest_messages[dest_key])
                else {
                    continue;
                };
                destinations_by_metadata
                    .entry(destination_metadata)
                    .or_default()
                    .push_back(*dest_key);
                if let Some(destination_fingerprint) = dest_fingerprints.get(*dest_key) {
                    destinations_by_content
                        .entry((destination_metadata, destination_fingerprint.as_str()))
                        .or_default()
                        .push_back(*dest_key);
                }
            }
            destination_match_indexes.insert(
                *message_id,
                (destinations_by_metadata, destinations_by_content),
            );
        }
        let mut estimated_bytes = mismatches
            .iter()
            .map(estimated_mismatch_bytes)
            .sum::<usize>();
        if estimated_bytes > MAX_ESTIMATED_MISMATCH_DETAIL_BYTES {
            return Err(format!(
                "verification mismatch detail exceeds the estimated {}-byte evidence budget",
                MAX_ESTIMATED_MISMATCH_DETAIL_BYTES
            ));
        }
        let mut used_dest = HashSet::new();
        for source_key in sorted_keys(&source_messages.keys().collect()) {
            let Some(source_message_id) = source_messages[source_key]
                .message_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let Some(source_fingerprint) = source_fingerprints.get(source_key) else {
                continue;
            };
            let Some(source_metadata) = metadata_fingerprint(&source_messages[source_key]) else {
                continue;
            };
            let Some((destinations_by_metadata, destinations_by_content)) =
                destination_match_indexes.get_mut(source_message_id)
            else {
                continue;
            };
            let dest_key = pop_unused_destination(
                destinations_by_content.get_mut(&(source_metadata, source_fingerprint.as_str())),
                &used_dest,
            )
            .or_else(|| {
                pop_unused_destination(
                    destinations_by_metadata.get_mut(&source_metadata),
                    &used_dest,
                )
            });
            let Some(dest_key) = dest_key else { continue };
            used_dest.insert(dest_key);
            let destination_fingerprint = &dest_fingerprints[dest_key];
            if source_fingerprint == destination_fingerprint {
                continue;
            }
            let mut mismatch = make_mismatch(
                &job_context,
                &run_context,
                MismatchType::MessageIdOnly,
                Some(source_key),
                Some(dest_key),
                Some(&source_messages[source_key]),
                Some(&dest_messages[dest_key]),
            );
            mismatch.source_fingerprint = Some(source_fingerprint.clone());
            mismatch.destination_fingerprint = Some(destination_fingerprint.clone());
            append_mismatch_with_budget(&mut mismatches, mismatch, &mut estimated_bytes)?;
            summary.metadata_matches = summary.metadata_matches.saturating_sub(1);
            summary.changed_count = summary.changed_count.saturating_add(1);
        }
        Ok((mismatches, summary))
    }

    /// Detect mismatches while enforcing the expected source-to-destination
    /// folder mapping. An absent mapping entry means the source folder is
    /// expected to retain its name. Provider-specific label semantics belong
    /// in the mapping supplied by the caller, not in this generic verifier.
    /// Reconciliation Pass 1: Message-ID + exact metadata matching.
    /// Returns (mismatches created, source keys matched, dest keys matched).
    fn pass_1_message_id_exact_metadata<'a>(
        job_id: &Arc<str>,
        run_id: &Arc<str>,
        source_messages: &'a ExtractedMessages,
        dest_messages: &'a ExtractedMessages,
        folder_mapping: &'a HashMap<String, String>,
        source_by_message_id: &HashMap<&'a str, Vec<&'a MailboxMessageKey>>,
        dest_by_message_id: &HashMap<&'a str, Vec<&'a MailboxMessageKey>>,
    ) -> ReconciliationPassResult<'a> {
        let mut mismatches = Vec::new();
        let mut estimated_bytes = 0usize;
        let mut matched_source = HashSet::new();
        let mut matched_dest = HashSet::new();

        let mut message_ids: Vec<_> = source_by_message_id
            .keys()
            .filter(|id| dest_by_message_id.contains_key(*id))
            .copied()
            .collect();
        message_ids.sort_unstable();

        for message_id in message_ids {
            let source_uids = &source_by_message_id[message_id];
            let dest_uids = &dest_by_message_id[message_id];

            let mut destination_by_metadata =
                HashMap::<(&'a str, MetadataFingerprint<'a>), VecDeque<&MailboxMessageKey>>::new();
            for dest_key in dest_uids {
                if let Some(fingerprint) = metadata_fingerprint(&dest_messages[*dest_key]) {
                    destination_by_metadata
                        .entry((dest_key.mailbox.as_ref(), fingerprint))
                        .or_default()
                        .push_back(*dest_key);
                }
            }

            for source_key in source_uids {
                let source_msg = &source_messages[source_key];
                let Some(fingerprint) = metadata_fingerprint(source_msg) else {
                    continue;
                };
                let expected_folder = expected_destination_folder(source_key, folder_mapping);
                if let Some(dest_key) = destination_by_metadata
                    .get_mut(&(expected_folder, fingerprint))
                    .and_then(VecDeque::pop_front)
                {
                    matched_source.insert(*source_key);
                    matched_dest.insert(dest_key);
                }
            }

            // Handle metadata-mismatched but ID-matched pairs
            let mut destination_by_folder = HashMap::<&'a str, VecDeque<&MailboxMessageKey>>::new();
            for dest_key in dest_uids {
                if !matched_dest.contains(dest_key) {
                    destination_by_folder
                        .entry(dest_key.mailbox.as_ref())
                        .or_default()
                        .push_back(*dest_key);
                }
            }
            for source_key in source_uids {
                if matched_source.contains(source_key) {
                    continue;
                }
                let expected_folder = expected_destination_folder(source_key, folder_mapping);
                let Some(dest_key) = destination_by_folder
                    .get_mut(&expected_folder)
                    .and_then(VecDeque::pop_front)
                else {
                    continue;
                };
                let source_msg = &source_messages[source_key];
                let dest_msg = &dest_messages[dest_key];
                matched_source.insert(*source_key);
                matched_dest.insert(dest_key);
                append_mismatch_with_budget(
                    &mut mismatches,
                    make_mismatch(
                        job_id,
                        run_id,
                        MismatchType::MessageIdOnly,
                        Some(source_key),
                        Some(dest_key),
                        Some(source_msg),
                        Some(dest_msg),
                    ),
                    &mut estimated_bytes,
                )?;
            }
        }

        Ok((mismatches, matched_source, matched_dest))
    }

    /// Reconciliation Pass 2: Wrong-folder detection for Message-ID matches.
    /// Finds messages with identical Message-ID and metadata but in unexpected folders.
    #[allow(clippy::too_many_arguments)]
    fn pass_2_wrong_folder_detection<'a>(
        job_id: &Arc<str>,
        run_id: &Arc<str>,
        source_messages: &'a ExtractedMessages,
        dest_messages: &'a ExtractedMessages,
        folder_mapping: &'a HashMap<String, String>,
        unmatched_source: &HashSet<&MailboxMessageKey>,
        unmatched_dest: &HashSet<&MailboxMessageKey>,
        source_by_message_id: &HashMap<&'a str, Vec<&'a MailboxMessageKey>>,
        dest_by_message_id: &HashMap<&'a str, Vec<&'a MailboxMessageKey>>,
    ) -> ReconciliationPassResult<'a> {
        let mut mismatches = Vec::new();
        let mut estimated_bytes = 0usize;
        let mut matched_source = HashSet::new();
        let mut matched_dest = HashSet::new();

        let mut message_ids = source_by_message_id
            .keys()
            .filter(|message_id| dest_by_message_id.contains_key(*message_id))
            .copied()
            .collect::<Vec<_>>();
        message_ids.sort_unstable();
        for message_id in message_ids {
            let source_uids = &source_by_message_id[message_id];
            let Some(dest_uids) = dest_by_message_id.get(message_id) else {
                continue;
            };

            // Build an ordered folder index for each metadata fingerprint. A
            // source message can select the nearest deterministic folder on
            // either side of its expected folder without scanning every
            // folder bucket in this Message-ID group.
            let mut dest_by_metadata: HashMap<
                Option<MetadataFingerprint<'a>>,
                BTreeMap<&'a str, VecDeque<&MailboxMessageKey>>,
            > = HashMap::new();
            for dest_key in dest_uids {
                if !unmatched_dest.contains(dest_key) {
                    continue;
                }
                let metadata = metadata_fingerprint(&dest_messages[dest_key]);
                dest_by_metadata
                    .entry(metadata)
                    .or_default()
                    .entry(dest_key.mailbox.as_ref())
                    .or_default()
                    .push_back(dest_key);
            }

            // For each unmatched source with this Message-ID, try to find it in a wrong folder
            for source_key in source_uids {
                if !unmatched_source.contains(source_key) {
                    continue;
                }
                let expected_folder = expected_destination_folder(source_key, folder_mapping);
                let source_metadata = metadata_fingerprint(&source_messages[source_key]);

                // Try to find in any wrong folder with matching metadata.
                // The range lookup is O(log F), where F is the number of
                // folders in this duplicate-ID group, instead of O(F) for
                // every source candidate.
                let Some(folder_candidates) = dest_by_metadata.get_mut(&source_metadata) else {
                    continue;
                };
                let lower = folder_candidates.range(..expected_folder).next();
                let upper = folder_candidates
                    .range::<&str, _>((Excluded(expected_folder), Unbounded))
                    .next();
                let candidate_folder = match (lower, upper) {
                    (Some((lower, _)), Some((upper, _))) => {
                        Some(if lower <= upper { *lower } else { *upper })
                    }
                    (Some((folder, _)), None) | (None, Some((folder, _))) => Some(*folder),
                    (None, None) => None,
                };
                let Some(candidate_folder) = candidate_folder else {
                    continue;
                };
                let Some(dest_key) = folder_candidates
                    .get_mut(&candidate_folder)
                    .and_then(VecDeque::pop_front)
                else {
                    continue;
                };
                if folder_candidates
                    .get(&candidate_folder)
                    .is_some_and(VecDeque::is_empty)
                {
                    folder_candidates.remove(&candidate_folder);
                }
                matched_source.insert(*source_key);
                matched_dest.insert(dest_key);
                append_mismatch_with_budget(
                    &mut mismatches,
                    make_mismatch(
                        job_id,
                        run_id,
                        MismatchType::PresentWrongFolder,
                        Some(source_key),
                        Some(dest_key),
                        Some(&source_messages[source_key]),
                        Some(&dest_messages[dest_key]),
                    ),
                    &mut estimated_bytes,
                )?;
            }
        }

        Ok((mismatches, matched_source, matched_dest))
    }

    /// Reconciliation Pass 3: Fingerprint fallback for unmatched messages.
    /// Only processes messages not matched in Passes 1-2.
    fn pass_3_fingerprint_fallback<'a>(
        source_messages: &'a ExtractedMessages,
        dest_messages: &'a ExtractedMessages,
        unmatched_source: &HashSet<&'a MailboxMessageKey>,
        unmatched_dest: &HashSet<&'a MailboxMessageKey>,
        folder_mapping: &'a HashMap<String, String>,
    ) -> (
        u64,
        HashSet<&'a MailboxMessageKey>,
        HashSet<&'a MailboxMessageKey>,
    ) {
        let mut probable_matches = 0_u64;
        let mut matched_source = HashSet::new();
        let mut matched_dest = HashSet::new();

        let source_by_fingerprint =
            index_by_fingerprint(source_messages, unmatched_source, folder_mapping, true);
        let dest_by_fingerprint =
            index_by_fingerprint(dest_messages, unmatched_dest, folder_mapping, false);

        let mut fingerprints: Vec<_> = source_by_fingerprint
            .keys()
            .filter(|fp| dest_by_fingerprint.contains_key(fp))
            .cloned()
            .collect();
        fingerprints.sort();

        for fingerprint in fingerprints {
            let source_uids = &source_by_fingerprint[&fingerprint];
            let dest_uids = &dest_by_fingerprint[&fingerprint];
            if source_uids.len() == 1 && dest_uids.len() == 1 {
                matched_source.insert(source_uids[0]);
                matched_dest.insert(dest_uids[0]);
                probable_matches += 1;
            }
        }

        (probable_matches, matched_source, matched_dest)
    }

    #[allow(unused_assignments, clippy::collapsible_if, clippy::needless_borrow)]
    pub fn detect_mismatches_with_folder_mapping(
        job_id: &str,
        run_id: &str,
        source_messages: &ExtractedMessages,
        dest_messages: &ExtractedMessages,
        folder_mapping: &HashMap<String, String>,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        let estimated_state_bytes = estimated_verifier_state_bytes(source_messages, dest_messages);
        enforce_verifier_state_budget(estimated_state_bytes)?;
        let job_id: Arc<str> = Arc::from(job_id);
        let run_id: Arc<str> = Arc::from(run_id);
        let mut all_mismatches = Vec::new();
        let mut estimated_detail_bytes = 0usize;
        let mut total_metadata_matches = 0_u64;
        let mut total_probable_matches = 0_u64;

        let source_by_message_id = index_by_message_id(source_messages);
        let dest_by_message_id = index_by_message_id(dest_messages);

        // Pass 1: Message-ID + exact metadata matching
        let (mut pass1_mismatches, pass1_matched_src, pass1_matched_dst) =
            Self::pass_1_message_id_exact_metadata(
                &job_id,
                &run_id,
                source_messages,
                dest_messages,
                folder_mapping,
                &source_by_message_id,
                &dest_by_message_id,
            )?;
        // Pass 1 returns one mismatch for each matched source that failed
        // exact metadata comparison. Counting those directly avoids scanning
        // the growing mismatch vector once per matched message.
        let pass1_mismatch_count = pass1_mismatches.len();
        total_metadata_matches =
            pass1_matched_src.len().saturating_sub(pass1_mismatch_count) as u64;
        // Move the first-pass records into the final output instead of
        // cloning every mismatch and retaining two full vectors concurrently.
        estimated_detail_bytes = estimated_detail_bytes.saturating_add(
            pass1_mismatches
                .iter()
                .map(estimated_mismatch_bytes)
                .sum::<usize>(),
        );
        if estimated_detail_bytes > MAX_ESTIMATED_MISMATCH_DETAIL_BYTES {
            return Err(format!(
                "verification mismatch detail exceeds the estimated {}-byte evidence budget",
                MAX_ESTIMATED_MISMATCH_DETAIL_BYTES
            ));
        }
        all_mismatches.append(&mut pass1_mismatches);

        // Build unmatched sets for Pass 2
        // Borrow canonical map keys throughout reconciliation. Cloning every
        // key into both unmatched sets materially inflates peak memory on
        // large accounts; owned keys are created only for final membership
        // evidence after classification is complete.
        let mut unmatched_source: HashSet<&MailboxMessageKey> = source_messages
            .keys()
            .filter(|k| !pass1_matched_src.contains(k))
            .collect();
        let mut unmatched_dest: HashSet<&MailboxMessageKey> = dest_messages
            .keys()
            .filter(|k| !pass1_matched_dst.contains(k))
            .collect();

        // Pass 2: Wrong-folder detection for Message-ID matches
        let (mut pass2_mismatches, pass2_matched_src, pass2_matched_dst) =
            Self::pass_2_wrong_folder_detection(
                &job_id,
                &run_id,
                source_messages,
                dest_messages,
                folder_mapping,
                &unmatched_source,
                &unmatched_dest,
                &source_by_message_id,
                &dest_by_message_id,
            )?;
        estimated_detail_bytes = estimated_detail_bytes.saturating_add(
            pass2_mismatches
                .iter()
                .map(estimated_mismatch_bytes)
                .sum::<usize>(),
        );
        if estimated_detail_bytes > MAX_ESTIMATED_MISMATCH_DETAIL_BYTES {
            return Err(format!(
                "verification mismatch detail exceeds the estimated {}-byte evidence budget",
                MAX_ESTIMATED_MISMATCH_DETAIL_BYTES
            ));
        }
        all_mismatches.append(&mut pass2_mismatches);
        for key in &pass2_matched_src {
            unmatched_source.remove(key);
        }
        for key in &pass2_matched_dst {
            unmatched_dest.remove(key);
        }

        // Pass 3: Fingerprint fallback for unmatched messages
        let (pass3_probable, pass3_matched_src, pass3_matched_dst) =
            Self::pass_3_fingerprint_fallback(
                source_messages,
                dest_messages,
                &unmatched_source,
                &unmatched_dest,
                folder_mapping,
            );
        total_probable_matches = pass3_probable;
        for key in &pass3_matched_src {
            unmatched_source.remove(key);
        }
        for key in &pass3_matched_dst {
            unmatched_dest.remove(key);
        }

        // Report remaining unmatched messages
        let mut unmatched_source_vec: Vec<_> = unmatched_source.iter().collect();
        unmatched_source_vec.sort();
        for source_uid in unmatched_source_vec {
            let source_msg = &source_messages[source_uid];
            append_mismatch_with_budget(
                &mut all_mismatches,
                make_mismatch(
                    &job_id,
                    &run_id,
                    MismatchType::Missing,
                    Some(source_uid),
                    None,
                    Some(source_msg),
                    None,
                ),
                &mut estimated_detail_bytes,
            )?;
        }

        // Report duplicates and extra destination messages
        let mut duplicate_dest_uids: Vec<_> = dest_by_message_id
            .iter()
            .filter(|(message_id, dest_uids)| {
                source_by_message_id
                    .get(*message_id)
                    .is_some_and(|source_uids| dest_uids.len() > source_uids.len())
            })
            .flat_map(|(_, uids)| uids.iter())
            .filter(|uid| unmatched_dest.contains(*uid))
            .cloned()
            .collect();
        duplicate_dest_uids.sort();
        for dest_uid in duplicate_dest_uids {
            let dest_msg = &dest_messages[&dest_uid];
            unmatched_dest.remove(&dest_uid);
            append_mismatch_with_budget(
                &mut all_mismatches,
                make_mismatch(
                    &job_id,
                    &run_id,
                    MismatchType::Duplicated,
                    None,
                    Some(&dest_uid),
                    None,
                    Some(dest_msg),
                ),
                &mut estimated_detail_bytes,
            )?;
        }

        let mut unmatched_dest_vec: Vec<_> = unmatched_dest.iter().collect();
        unmatched_dest_vec.sort();
        for dest_uid in unmatched_dest_vec {
            let dest_msg = &dest_messages[dest_uid];
            append_mismatch_with_budget(
                &mut all_mismatches,
                make_mismatch(
                    &job_id,
                    &run_id,
                    MismatchType::Extra,
                    None,
                    Some(dest_uid),
                    None,
                    Some(dest_msg),
                ),
                &mut estimated_detail_bytes,
            )?;
        }

        let source_index = build_uid_folder_index(source_messages);
        let dest_index = build_uid_folder_index(dest_messages);
        let mut missing_source: HashSet<&MailboxMessageKey> = HashSet::new();
        let mut extra_destination: HashSet<&MailboxMessageKey> = HashSet::new();
        let mut duplicated_destination: HashSet<&MailboxMessageKey> = HashSet::new();
        let mut changed_source: HashSet<&MailboxMessageKey> = HashSet::new();
        let mut changed_destination: HashSet<&MailboxMessageKey> = HashSet::new();
        let mut missing_count = 0_u64;
        let mut extra_count = 0_u64;
        let mut duplicated_count = 0_u64;
        let mut changed_count = 0_u64;
        for mismatch in &all_mismatches {
            match mismatch.mismatch_type {
                MismatchType::Missing => {
                    missing_count += 1;
                    if let Some(key) = mismatch_source_key(mismatch, &source_index) {
                        missing_source.insert(key);
                    }
                }
                MismatchType::Extra => {
                    extra_count += 1;
                    if let Some(key) = mismatch_destination_key(mismatch, &dest_index) {
                        extra_destination.insert(key);
                    }
                }
                MismatchType::Duplicated => {
                    duplicated_count += 1;
                    if let Some(key) = mismatch_destination_key(mismatch, &dest_index) {
                        duplicated_destination.insert(key);
                    }
                }
                MismatchType::MessageIdOnly | MismatchType::PresentWrongFolder => {
                    changed_count += 1;
                    if let Some(key) = mismatch_source_key(mismatch, &source_index) {
                        changed_source.insert(key);
                    }
                    if let Some(key) = mismatch_destination_key(mismatch, &dest_index) {
                        changed_destination.insert(key);
                    }
                }
            }
        }

        let summary = VerificationSummary {
            total_source: source_messages.len() as u64,
            total_destination: dest_messages.len() as u64,
            metadata_matches: total_metadata_matches,
            probable_matches: total_probable_matches,
            missing_count,
            extra_count,
            duplicated_count,
            changed_count,
        };

        let membership = VerificationMembership {
            matched_source: pass1_matched_src
                .iter()
                .copied()
                .filter(|key| !changed_source.contains(*key))
                .collect(),
            matched_destination: pass1_matched_dst
                .iter()
                .copied()
                .filter(|key| !changed_destination.contains(*key))
                .collect(),
            probable_source: pass3_matched_src,
            probable_destination: pass3_matched_dst,
            missing_source,
            extra_destination,
            duplicated_destination,
            changed_source,
            changed_destination,
        };
        validate_verification_summary(
            source_messages,
            dest_messages,
            &all_mismatches,
            &summary,
            &membership,
        )?;

        Ok((all_mismatches, summary))
    }

    /// Reconcile metadata staged in SQLite. The live adapter uses this path
    /// for large accounts so only one bounded batch and one candidate row are
    /// resident in Rust at a time.
    pub(crate) fn detect_mismatches_from_stage(
        job_id: &str,
        run_id: &str,
        stage: &MessageMetadataStage,
        folder_mapping: &HashMap<String, String>,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        let connection = stage.connection();
        connection
            .execute_batch(
                "CREATE TEMP TABLE staged_matched(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uid TEXT NOT NULL, PRIMARY KEY(side,mailbox,uidvalidity,uid));
                 CREATE TEMP TABLE staged_folder_mapping(source TEXT PRIMARY KEY, destination TEXT NOT NULL);",
            )
            .map_err(|error| format!("could not initialize staged reconciliation: {error}"))?;
        for (source, destination) in folder_mapping {
            connection
                .execute(
                    "INSERT INTO staged_folder_mapping(source,destination) VALUES(?1,?2)",
                    params![source, destination],
                )
                .map_err(|error| format!("could not stage folder mapping: {error}"))?;
        }

        let job_context: Arc<str> = Arc::from(job_id);
        let run_context: Arc<str> = Arc::from(run_id);
        let mut mismatches = Vec::new();
        let mut estimated_bytes = 0usize;
        let mut metadata_matches = 0_u64;
        let mut probable_matches = 0_u64;
        let mut missing_count = 0_u64;
        let mut extra_count = 0_u64;
        let mut duplicated_count = 0_u64;
        let mut changed_count = 0_u64;

        // Message-ID + exact metadata matching.
        let mut after_rowid = 0_i64;
        loop {
            let batch = stage
                .batch(StagedMessageSide::Source, after_rowid, true, true)
                .map_err(|error| format!("could not read staged source metadata: {error}"))?;
            if batch.is_empty() {
                break;
            }
            after_rowid = batch.last().map_or(after_rowid, |row| row.rowid);
            for source in batch {
                let expected = expected_destination_folder(&source.key, folder_mapping);
                let Some(destination) = stage_candidate(
                    connection,
                    "SELECT rowid,mailbox,uidvalidity,uid,message_id,internal_date,date_key,size_bytes FROM staged_messages d WHERE d.side=1 AND d.message_id=?1 AND d.mailbox=?2 AND d.date_key=?3 AND d.size_bytes=?4 AND NOT EXISTS(SELECT 1 FROM staged_matched m WHERE m.side=d.side AND m.mailbox=d.mailbox AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid) ORDER BY d.mailbox,d.uidvalidity,d.uid LIMIT 1",
                    params![
                        source.message.message_id,
                        expected,
                        source.date_key,
                        sqlite_stage_size(&source)?
                    ],
                )?
                else {
                    continue;
                };
                mark_stage_matched(connection, StagedMessageSide::Source, &source)?;
                mark_stage_matched(connection, StagedMessageSide::Destination, &destination)?;
                metadata_matches = metadata_matches.saturating_add(1);
            }
        }

        // Remaining Message-ID matches in the expected folder are changes.
        after_rowid = 0;
        loop {
            let batch = stage
                .batch(StagedMessageSide::Source, after_rowid, true, false)
                .map_err(|error| format!("could not read staged source IDs: {error}"))?;
            if batch.is_empty() {
                break;
            }
            after_rowid = batch.last().map_or(after_rowid, |row| row.rowid);
            for source in batch {
                let expected = expected_destination_folder(&source.key, folder_mapping);
                let Some(destination) = stage_candidate(
                    connection,
                    "SELECT rowid,mailbox,uidvalidity,uid,message_id,internal_date,date_key,size_bytes FROM staged_messages d WHERE d.side=1 AND d.message_id=?1 AND d.mailbox=?2 AND NOT EXISTS(SELECT 1 FROM staged_matched m WHERE m.side=d.side AND m.mailbox=d.mailbox AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid) ORDER BY d.mailbox,d.uidvalidity,d.uid LIMIT 1",
                    params![source.message.message_id, expected],
                )?
                else {
                    continue;
                };
                mark_stage_matched(connection, StagedMessageSide::Source, &source)?;
                mark_stage_matched(connection, StagedMessageSide::Destination, &destination)?;
                append_stage_mismatch(
                    &mut mismatches,
                    make_mismatch(
                        &job_context,
                        &run_context,
                        MismatchType::MessageIdOnly,
                        Some(&source.key),
                        Some(&destination.key),
                        Some(&source.message),
                        Some(&destination.message),
                    ),
                    &mut estimated_bytes,
                )?;
                changed_count = changed_count.saturating_add(1);
            }
        }

        // Same Message-ID and metadata in a wrong folder.
        after_rowid = 0;
        loop {
            let batch = stage
                .batch(StagedMessageSide::Source, after_rowid, true, true)
                .map_err(|error| format!("could not read staged source metadata: {error}"))?;
            if batch.is_empty() {
                break;
            }
            after_rowid = batch.last().map_or(after_rowid, |row| row.rowid);
            for source in batch {
                let expected = expected_destination_folder(&source.key, folder_mapping);
                let query = "SELECT rowid,mailbox,uidvalidity,uid,message_id,internal_date,date_key,size_bytes FROM staged_messages d WHERE d.side=1 AND d.message_id=?1 AND d.date_key=?2 AND d.size_bytes=?3 AND d.mailbox< ?4 AND NOT EXISTS(SELECT 1 FROM staged_matched m WHERE m.side=d.side AND m.mailbox=d.mailbox AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid) ORDER BY d.mailbox DESC,d.uidvalidity,d.uid LIMIT 1";
                let lower = stage_candidate(
                    connection,
                    query,
                    params![
                        source.message.message_id,
                        source.date_key,
                        sqlite_stage_size(&source)?,
                        expected
                    ],
                )?;
                let query = "SELECT rowid,mailbox,uidvalidity,uid,message_id,internal_date,date_key,size_bytes FROM staged_messages d WHERE d.side=1 AND d.message_id=?1 AND d.date_key=?2 AND d.size_bytes=?3 AND d.mailbox> ?4 AND NOT EXISTS(SELECT 1 FROM staged_matched m WHERE m.side=d.side AND m.mailbox=d.mailbox AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid) ORDER BY d.mailbox,d.uidvalidity,d.uid LIMIT 1";
                let upper = stage_candidate(
                    connection,
                    query,
                    params![
                        source.message.message_id,
                        source.date_key,
                        sqlite_stage_size(&source)?,
                        expected
                    ],
                )?;
                let destination = match (lower, upper) {
                    (Some(lower), Some(upper)) => {
                        if lower.key.mailbox <= upper.key.mailbox {
                            lower
                        } else {
                            upper
                        }
                    }
                    (Some(row), None) | (None, Some(row)) => row,
                    (None, None) => continue,
                };
                mark_stage_matched(connection, StagedMessageSide::Source, &source)?;
                mark_stage_matched(connection, StagedMessageSide::Destination, &destination)?;
                append_stage_mismatch(
                    &mut mismatches,
                    make_mismatch(
                        &job_context,
                        &run_context,
                        MismatchType::PresentWrongFolder,
                        Some(&source.key),
                        Some(&destination.key),
                        Some(&source.message),
                        Some(&destination.message),
                    ),
                    &mut estimated_bytes,
                )?;
                changed_count = changed_count.saturating_add(1);
            }
        }

        // Unique date/size candidates are probable matches. Buckets are held
        // in SQLite, not in a Rust HashSet proportional to account size.
        connection
            .execute_batch("CREATE TEMP TABLE staged_fingerprint_buckets(folder TEXT NOT NULL,date_key TEXT NOT NULL,size_bytes INTEGER NOT NULL,PRIMARY KEY(folder,date_key,size_bytes));")
            .map_err(|error| format!("could not initialize staged fingerprint buckets: {error}"))?;
        after_rowid = 0;
        loop {
            let batch = stage
                .batch(StagedMessageSide::Source, after_rowid, false, true)
                .map_err(|error| format!("could not read staged fallback metadata: {error}"))?;
            if batch.is_empty() {
                break;
            }
            after_rowid = batch.last().map_or(after_rowid, |row| row.rowid);
            for source in batch {
                let expected = expected_destination_folder(&source.key, folder_mapping);
                let inserted = connection
                    .execute(
                        "INSERT OR IGNORE INTO staged_fingerprint_buckets(folder,date_key,size_bytes) VALUES(?1,?2,?3)",
                        params![expected, source.date_key, sqlite_stage_size(&source)?],
                    )
                    .map_err(|error| format!("could not stage fingerprint bucket: {error}"))?;
                if inserted == 0 {
                    continue;
                }
                let source_count: i64 = connection
                    .query_row(
                        "SELECT COUNT(*) FROM staged_messages s LEFT JOIN staged_folder_mapping f ON f.source=s.mailbox WHERE s.side=0 AND COALESCE(f.destination,s.mailbox)=?1 AND s.date_key=?2 AND s.size_bytes=?3 AND NOT EXISTS(SELECT 1 FROM staged_matched m WHERE m.side=s.side AND m.mailbox=s.mailbox AND m.uidvalidity=s.uidvalidity AND m.uid=s.uid)",
                        params![expected, source.date_key, sqlite_stage_size(&source)?],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("could not count staged source bucket: {error}"))?;
                let destination_count: i64 = connection
                    .query_row(
                        "SELECT COUNT(*) FROM staged_messages d WHERE d.side=1 AND d.mailbox=?1 AND d.date_key=?2 AND d.size_bytes=?3 AND NOT EXISTS(SELECT 1 FROM staged_matched m WHERE m.side=d.side AND m.mailbox=d.mailbox AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid)",
                        params![expected, source.date_key, sqlite_stage_size(&source)?],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("could not count staged destination bucket: {error}"))?;
                if source_count != 1 || destination_count != 1 {
                    continue;
                }
                let source_row = stage_candidate(
                    connection,
                    "SELECT s.rowid,s.mailbox,s.uidvalidity,s.uid,s.message_id,s.internal_date,s.date_key,s.size_bytes FROM staged_messages s LEFT JOIN staged_folder_mapping f ON f.source=s.mailbox WHERE s.side=0 AND COALESCE(f.destination,s.mailbox)=?1 AND s.date_key=?2 AND s.size_bytes=?3 AND NOT EXISTS(SELECT 1 FROM staged_matched m WHERE m.side=s.side AND m.mailbox=s.mailbox AND m.uidvalidity=s.uidvalidity AND m.uid=s.uid) LIMIT 1",
                    params![expected, source.date_key, sqlite_stage_size(&source)?],
                )?;
                let destination_row = stage_candidate(
                    connection,
                    "SELECT rowid,mailbox,uidvalidity,uid,message_id,internal_date,date_key,size_bytes FROM staged_messages d WHERE d.side=1 AND d.mailbox=?1 AND d.date_key=?2 AND d.size_bytes=?3 AND NOT EXISTS(SELECT 1 FROM staged_matched m WHERE m.side=d.side AND m.mailbox=d.mailbox AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid) LIMIT 1",
                    params![expected, source.date_key, sqlite_stage_size(&source)?],
                )?;
                if let (Some(source_row), Some(destination_row)) = (source_row, destination_row) {
                    mark_stage_matched(connection, StagedMessageSide::Source, &source_row)?;
                    mark_stage_matched(
                        connection,
                        StagedMessageSide::Destination,
                        &destination_row,
                    )?;
                    probable_matches = probable_matches.saturating_add(1);
                }
            }
        }

        // Remaining rows become bounded mismatch evidence.
        after_rowid = 0;
        loop {
            let batch = stage
                .batch(StagedMessageSide::Source, after_rowid, false, false)
                .map_err(|error| error.to_string())?;
            if batch.is_empty() {
                break;
            }
            after_rowid = batch.last().map_or(after_rowid, |row| row.rowid);
            for source in batch {
                append_stage_mismatch(
                    &mut mismatches,
                    make_mismatch(
                        &job_context,
                        &run_context,
                        MismatchType::Missing,
                        Some(&source.key),
                        None,
                        Some(&source.message),
                        None,
                    ),
                    &mut estimated_bytes,
                )?;
                missing_count = missing_count.saturating_add(1);
            }
        }
        connection.execute_batch("CREATE TEMP TABLE staged_duplicate_ids AS SELECT d.message_id FROM staged_messages d WHERE d.side=1 AND d.message_id IS NOT NULL GROUP BY d.message_id HAVING COUNT(*) > (SELECT COUNT(*) FROM staged_messages s WHERE s.side=0 AND s.message_id=d.message_id) AND (SELECT COUNT(*) FROM staged_messages s WHERE s.side=0 AND s.message_id=d.message_id) > 0;").map_err(|error| error.to_string())?;
        after_rowid = 0;
        loop {
            let batch = stage
                .batch(StagedMessageSide::Destination, after_rowid, false, false)
                .map_err(|error| error.to_string())?;
            if batch.is_empty() {
                break;
            }
            after_rowid = batch.last().map_or(after_rowid, |row| row.rowid);
            for destination in batch {
                let duplicated = match destination.message.message_id.as_deref() {
                    Some(id) => connection
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM staged_duplicate_ids WHERE message_id=?1)",
                            [id],
                            |row| row.get::<_, bool>(0),
                        )
                        .map_err(|error| {
                            format!("could not classify staged duplicate message: {error}")
                        })?,
                    None => false,
                };
                let mismatch_type = if duplicated {
                    MismatchType::Duplicated
                } else {
                    MismatchType::Extra
                };
                append_stage_mismatch(
                    &mut mismatches,
                    make_mismatch(
                        &job_context,
                        &run_context,
                        mismatch_type,
                        None,
                        Some(&destination.key),
                        None,
                        Some(&destination.message),
                    ),
                    &mut estimated_bytes,
                )?;
                if duplicated {
                    duplicated_count = duplicated_count.saturating_add(1);
                } else {
                    extra_count = extra_count.saturating_add(1);
                }
            }
        }

        let total_source = stage
            .count(StagedMessageSide::Source)
            .map_err(|error| error.to_string())?;
        let total_destination = stage
            .count(StagedMessageSide::Destination)
            .map_err(|error| error.to_string())?;
        if total_source
            != metadata_matches
                .saturating_add(probable_matches)
                .saturating_add(missing_count)
                .saturating_add(changed_count)
            || total_destination
                != metadata_matches
                    .saturating_add(probable_matches)
                    .saturating_add(extra_count)
                    .saturating_add(duplicated_count)
                    .saturating_add(changed_count)
        {
            return Err(
                "staged reconciliation accounting did not partition both message populations"
                    .into(),
            );
        }
        Ok((
            mismatches,
            VerificationSummary {
                total_source,
                total_destination,
                metadata_matches,
                probable_matches,
                missing_count,
                extra_count,
                duplicated_count,
                changed_count,
            },
        ))
    }
}

fn sqlite_stage_size(message: &StagedMessage) -> Result<i64, String> {
    message
        .message
        .size_bytes
        .ok_or_else(|| "staged metadata is missing RFC822.SIZE".to_owned())
        .and_then(|value| {
            i64::try_from(value).map_err(|_| "staged RFC822.SIZE exceeds SQLite range".to_owned())
        })
}

fn stage_candidate<P: rusqlite::Params>(
    connection: &rusqlite::Connection,
    sql: &str,
    parameters: P,
) -> Result<Option<StagedMessage>, String> {
    connection
        .query_row(sql, parameters, staged_message_from_row)
        .optional()
        .map_err(|error| format!("could not query staged reconciliation candidate: {error}"))
}

fn mark_stage_matched(
    connection: &rusqlite::Connection,
    side: StagedMessageSide,
    message: &StagedMessage,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO staged_matched(side,mailbox,uidvalidity,uid) VALUES(?1,?2,?3,?4)",
            params![
                side.as_i64(),
                message.key.mailbox.as_ref(),
                message
                    .key
                    .uidvalidity
                    .map(|value| {
                        i64::try_from(value)
                            .map_err(|_| "staged UIDVALIDITY exceeds SQLite range".to_owned())
                    })
                    .transpose()?
                    .unwrap_or(-1),
                message.key.uid,
            ],
        )
        .map(|_| ())
        .map_err(|error| format!("could not mark staged message as matched: {error}"))
}

fn append_stage_mismatch(
    mismatches: &mut Vec<MessageMismatch>,
    mismatch: MessageMismatch,
    estimated_bytes: &mut usize,
) -> Result<(), String> {
    append_mismatch_with_budget(mismatches, mismatch, estimated_bytes)
}

/// Validate that reconciliation accounting is consistent and complete.
/// This catches bugs where messages are counted incorrectly or reconciled multiple times.
#[allow(clippy::collapsible_if)]
pub fn validate_verification_summary<'a>(
    source_messages: &'a ExtractedMessages,
    dest_messages: &'a ExtractedMessages,
    mismatches: &[MessageMismatch],
    summary: &VerificationSummary,
    membership: &VerificationMembership<'a>,
) -> Result<(), String> {
    fn check_partition<'a>(
        side: &str,
        input: &'a ExtractedMessages,
        partitions: &[(&str, &HashSet<&'a MailboxMessageKey>)],
    ) -> Result<(), String> {
        let mut owners = HashMap::<&MailboxMessageKey, Vec<&str>>::new();
        for (name, keys) in partitions {
            for key in *keys {
                owners.entry(key).or_default().push(name);
            }
        }
        for key in input.keys() {
            match owners.get(key).map(Vec::as_slice) {
                Some([_]) => {}
                Some(owners) => {
                    return Err(format!(
                        "{side} message {:?} belongs to {} classifications: {}",
                        key,
                        owners.len(),
                        owners.join(", ")
                    ));
                }
                None => return Err(format!("{side} message {:?} is unclassified", key)),
            }
        }
        if owners.len() != input.len() {
            return Err(format!(
                "{side} classification contains {} keys for {} input messages",
                owners.len(),
                input.len()
            ));
        }
        Ok(())
    }

    check_partition(
        "source",
        source_messages,
        &[
            ("matched", &membership.matched_source),
            ("probable", &membership.probable_source),
            ("missing", &membership.missing_source),
            ("changed", &membership.changed_source),
        ],
    )?;
    check_partition(
        "destination",
        dest_messages,
        &[
            ("matched", &membership.matched_destination),
            ("probable", &membership.probable_destination),
            ("extra", &membership.extra_destination),
            ("duplicated", &membership.duplicated_destination),
            ("changed", &membership.changed_destination),
        ],
    )?;

    let source_index = build_uid_folder_index(source_messages);
    let dest_index = build_uid_folder_index(dest_messages);

    let mut seen_source: HashSet<&MailboxMessageKey> = HashSet::new();
    let mut seen_destination: HashSet<&MailboxMessageKey> = HashSet::new();
    for mismatch in mismatches {
        if let Some(key) = mismatch_source_key(mismatch, &source_index) {
            if !seen_source.insert(key) {
                return Err(format!(
                    "source mismatch identity {:?} appears more than once",
                    key
                ));
            }
        }
        if let Some(key) = mismatch_destination_key(mismatch, &dest_index) {
            if !seen_destination.insert(key) {
                return Err(format!(
                    "destination mismatch identity {:?} appears more than once",
                    key
                ));
            }
        }
    }
    let _ = summary;
    Ok(())
}

fn index_by_message_id(messages: &ExtractedMessages) -> HashMap<&str, Vec<&MailboxMessageKey>> {
    let mut index = HashMap::new();
    for (uid, message) in messages {
        if let Some(message_id) = message
            .message_id
            .as_deref()
            .map(str::trim)
            .filter(|message_id| !message_id.is_empty())
        {
            index.entry(message_id).or_insert_with(Vec::new).push(uid);
        }
    }
    index.values_mut().for_each(|uids| uids.sort());
    index
}

#[cfg(test)]
fn pop_unused_destination<'a>(
    candidates: Option<&mut VecDeque<&'a MailboxMessageKey>>,
    used: &HashSet<&MailboxMessageKey>,
) -> Option<&'a MailboxMessageKey> {
    let candidates = candidates?;
    while let Some(candidate) = candidates.pop_front() {
        if !used.contains(candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
fn unique_fingerprint_index(
    fingerprints: &HashMap<MailboxMessageKey, String>,
) -> HashMap<&String, &MailboxMessageKey> {
    let mut index = HashMap::new();
    let mut duplicated = HashSet::new();
    for key in fingerprints.keys() {
        let fingerprint = &fingerprints[key];
        if duplicated.contains(fingerprint) {
            continue;
        }
        if index.insert(fingerprint, key).is_some() {
            index.remove(fingerprint);
            duplicated.insert(fingerprint);
        }
    }
    index
}

fn index_by_fingerprint<'a>(
    messages: &'a ExtractedMessages,
    eligible: &HashSet<&'a MailboxMessageKey>,
    folder_mapping: &'a HashMap<String, String>,
    source_side: bool,
) -> HashMap<(&'a str, MetadataFingerprint<'a>), Vec<&'a MailboxMessageKey>> {
    let mut index = HashMap::new();
    for uid in eligible {
        let message = &messages[*uid];
        let Some(fingerprint) = metadata_fingerprint(message) else {
            continue;
        };
        let folder: &'a str = if source_side {
            expected_destination_folder(uid, folder_mapping)
        } else {
            uid.mailbox.as_ref()
        };
        index
            .entry((folder, fingerprint))
            .or_insert_with(Vec::new)
            .push(*uid);
    }
    index.values_mut().for_each(|uids| uids.sort());
    index
}

fn expected_destination_folder<'a>(
    source_key: &'a MailboxMessageKey,
    folder_mapping: &'a HashMap<String, String>,
) -> &'a str {
    folder_mapping
        .get(source_key.mailbox.as_ref())
        .map(String::as_str)
        .unwrap_or_else(|| source_key.mailbox.as_ref())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum NormalizedInternalDate<'a> {
    Epoch(i64),
    Raw(&'a str),
}

/// Borrowed metadata key used only while reconciling one in-memory batch.
/// Parsed IMAP dates use their UTC epoch; non-IMAP synthetic dates retain
/// their original representation for compatibility with extracted fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct MetadataFingerprint<'a> {
    internal_date: NormalizedInternalDate<'a>,
    size_bytes: u64,
}

#[cfg(test)]
fn sorted_keys<'a>(keys: &'a HashSet<&'a MailboxMessageKey>) -> Vec<&'a MailboxMessageKey> {
    let mut sorted = keys.iter().copied().collect::<Vec<_>>();
    sorted.sort_by(|left, right| {
        left.mailbox
            .cmp(&right.mailbox)
            .then(left.uidvalidity.cmp(&right.uidvalidity))
            .then(left.uid.cmp(&right.uid))
    });
    sorted
}

fn metadata_fingerprint(message: &ExtractedMessage) -> Option<MetadataFingerprint<'_>> {
    Some(MetadataFingerprint {
        internal_date: normalize_internal_date(message.internal_date.as_deref()?),
        size_bytes: message.size_bytes?,
    })
}

fn normalize_internal_date(value: &str) -> NormalizedInternalDate<'_> {
    DateTime::<FixedOffset>::parse_from_str(value.trim(), "%d-%b-%Y %H:%M:%S %z")
        .map(|date| NormalizedInternalDate::Epoch(date.timestamp()))
        .unwrap_or(NormalizedInternalDate::Raw(value))
}

fn make_mismatch(
    job_id: &Arc<str>,
    run_id: &Arc<str>,
    mismatch_type: MismatchType,
    source_key: Option<&MailboxMessageKey>,
    dest_key: Option<&MailboxMessageKey>,
    source: Option<&ExtractedMessage>,
    destination: Option<&ExtractedMessage>,
) -> MessageMismatch {
    MessageMismatch {
        id: format!(
            "{}-{}-{}",
            run_id,
            mismatch_type.as_str(),
            uuid::Uuid::new_v4()
        ),
        job_id: Arc::clone(job_id),
        run_id: Arc::clone(run_id),
        mismatch_type,
        source_folder: source_key.map(|key| key.mailbox.to_string()),
        destination_folder: dest_key.map(|key| key.mailbox.to_string()),
        source_uidvalidity: source_key.and_then(|key| key.uidvalidity),
        destination_uidvalidity: dest_key.and_then(|key| key.uidvalidity),
        source_uid: source_key.map(|key| key.uid.clone()),
        dest_uid: dest_key.map(|key| key.uid.clone()),
        source_message_id: source.and_then(|message| message.message_id.clone()),
        dest_message_id: destination.and_then(|message| message.message_id.clone()),
        source_size_bytes: source.and_then(|message| message.size_bytes),
        dest_size_bytes: destination.and_then(|message| message.size_bytes),
        source_date: source.and_then(|message| message.internal_date.clone()),
        dest_date: destination.and_then(|message| message.internal_date.clone()),
        source_fingerprint: None,
        destination_fingerprint: None,
    }
}

/// Summary of verification results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationSummary {
    pub total_source: u64,
    pub total_destination: u64,
    /// Messages with a unique Message-ID and matching available metadata.
    /// This is not content verification.
    pub metadata_matches: u64,
    /// Unique internal-date + size pairings. These are reconciliation
    /// candidates, not proof of message identity.
    pub probable_matches: u64,
    pub missing_count: u64,
    pub extra_count: u64,
    pub duplicated_count: u64,
    pub changed_count: u64,
}

/// Evidence classification for message-level reconciliation.
///
/// This is deliberately categorical rather than a synthetic percentage. The
/// most serious observed condition wins, so a large population of successful
/// matches cannot hide missing, changed, or unexpected destination messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub enum EvidenceLevel {
    MetadataMatched,
    StrongMetadataMatch,
    ProbableMatch,
    Ambiguous,
    Missing,
    Changed,
    Unexpected,
}

#[cfg(test)]
impl EvidenceLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MetadataMatched => "metadata_matched",
            Self::StrongMetadataMatch => "strong_metadata_match",
            Self::ProbableMatch => "probable_match",
            Self::Ambiguous => "ambiguous",
            Self::Missing => "missing",
            Self::Changed => "changed",
            Self::Unexpected => "unexpected",
        }
    }
}

impl VerificationSummary {
    /// Convert the in-memory reconciliation result to the same semantic
    /// outcome persisted in `MailboxEvidence` and consumed by reports/UI.
    pub fn verification_outcome(&self) -> VerificationOutcome {
        if self.missing_count > 0 {
            VerificationOutcome::Missing
        } else if self.changed_count > 0 {
            VerificationOutcome::Changed
        } else if self.duplicated_count > 0 || self.extra_count > 0 {
            VerificationOutcome::Unexpected
        } else if self.probable_matches > 0 {
            VerificationOutcome::ProbableMatch
        } else if self.metadata_matches == self.total_source
            && self.metadata_matches == self.total_destination
        {
            VerificationOutcome::ExactMetadataMatch
        } else {
            VerificationOutcome::Ambiguous
        }
    }

    /// Return whether all extracted records reconcile by metadata alone.
    /// This must not be presented as content verification.
    #[cfg(test)]
    pub fn is_perfect_metadata_match(&self) -> bool {
        self.missing_count == 0
            && self.extra_count == 0
            && self.duplicated_count == 0
            && self.changed_count == 0
            && self.probable_matches == 0
            && self.metadata_matches == self.total_source
            && self.metadata_matches == self.total_destination
    }

    /// Return the single authoritative message-evidence classification.
    ///
    /// This method intentionally evaluates every negative category before
    /// successful match counts. A migration with 950 metadata matches and
    /// 10,000 missing messages is therefore not high-confidence or verified.
    #[cfg(test)]
    pub fn evidence_level(&self) -> EvidenceLevel {
        if self.missing_count > 0 {
            EvidenceLevel::Missing
        } else if self.changed_count > 0 {
            EvidenceLevel::Changed
        } else if self.duplicated_count > 0 || self.extra_count > 0 {
            EvidenceLevel::Unexpected
        } else if self.probable_matches > 0 {
            EvidenceLevel::ProbableMatch
        } else if self.metadata_matches == self.total_source
            && self.metadata_matches == self.total_destination
        {
            EvidenceLevel::MetadataMatched
        } else if self.metadata_matches > 0 {
            EvidenceLevel::StrongMetadataMatch
        } else {
            EvidenceLevel::Ambiguous
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_verifier_budget_fails_closed_before_reconciliation() {
        assert!(enforce_verifier_state_budget(MAX_ESTIMATED_VERIFIER_STATE_BYTES).is_ok());
        assert!(enforce_verifier_state_budget(MAX_ESTIMATED_VERIFIER_STATE_BYTES + 1).is_err());
    }

    fn key(uid: &str) -> MailboxMessageKey {
        MailboxMessageKey::new("INBOX", uid)
    }

    #[test]
    fn synthetic_verifier_scale_covers_balanced_mismatch_rates() {
        for &(count, mismatch_percent) in &[
            (10_000_usize, 0_usize),
            (10_000, 10),
            (10_000, 100),
            (100_000, 0),
            (100_000, 10),
            (100_000, 100),
        ] {
            let changed = count * mismatch_percent / 100;
            let mut source = ExtractedMessages::with_capacity(count);
            let mut destination = ExtractedMessages::with_capacity(count);
            for index in 0..count {
                let uid = index.to_string();
                let message_id = format!("<synthetic-{index}@example.test>");
                source.insert(
                    MailboxMessageKey::new("INBOX", &uid),
                    ExtractedMessage {
                        message_id: Some(message_id.clone()),
                        uid: Some(uid.clone()),
                        size_bytes: Some(1_000),
                        internal_date: Some("2024-01-01T00:00:00Z".to_owned()),
                    },
                );
                destination.insert(
                    MailboxMessageKey::new("INBOX", &uid),
                    ExtractedMessage {
                        message_id: Some(message_id),
                        uid: Some(uid),
                        size_bytes: Some(if index < changed { 1_001 } else { 1_000 }),
                        internal_date: Some("2024-01-01T00:00:00Z".to_owned()),
                    },
                );
            }

            let (_, summary) = MessageVerification::detect_mismatches_with_folder_mapping(
                "synthetic-job",
                "synthetic-run",
                &source,
                &destination,
                &HashMap::new(),
            )
            .unwrap();
            assert_eq!(summary.total_source, count as u64);
            assert_eq!(summary.total_destination, count as u64);
            assert_eq!(summary.metadata_matches, (count - changed) as u64);
            assert_eq!(summary.changed_count, changed as u64);
        }
    }

    #[test]
    fn detects_missing_messages() {
        let mut source = HashMap::new();
        source.insert(
            key("1"),
            ExtractedMessage {
                message_id: Some("<a@example.com>".to_string()),
                uid: Some("1".to_string()),
                size_bytes: Some(1000),
                internal_date: Some("2024-01-01".to_string()),
            },
        );
        source.insert(
            key("2"),
            ExtractedMessage {
                message_id: Some("<b@example.com>".to_string()),
                uid: Some("2".to_string()),
                size_bytes: Some(2000),
                internal_date: Some("2024-01-02".to_string()),
            },
        );

        let mut dest = HashMap::new();
        dest.insert(
            key("1"),
            ExtractedMessage {
                message_id: Some("<a@example.com>".to_string()),
                uid: Some("1".to_string()),
                size_bytes: Some(1000),
                internal_date: Some("2024-01-01".to_string()),
            },
        );

        let (mismatches, summary) =
            MessageVerification::detect_mismatches("job1", "run1", &source, &dest).unwrap();

        assert_eq!(summary.missing_count, 1);
        assert_eq!(summary.extra_count, 0);
        assert_eq!(summary.metadata_matches, 1);

        let missing = mismatches
            .iter()
            .find(|m| m.mismatch_type == MismatchType::Missing)
            .unwrap();
        assert_eq!(missing.source_uid, Some("2".to_string()));
    }

    #[test]
    fn equivalent_internal_date_offsets_match_semantically() {
        let message = |date: &str| ExtractedMessage {
            message_id: Some("<same@example.com>".into()),
            uid: Some("1".into()),
            size_bytes: Some(100),
            internal_date: Some(date.into()),
        };
        let source = HashMap::from([(key("1"), message("01-Jan-2024 12:00:00 +0000"))]);
        let destination = HashMap::from([(key("1"), message("01-Jan-2024 07:00:00 -0500"))]);

        let (mismatches, summary) =
            MessageVerification::detect_mismatches("job1", "run1", &source, &destination).unwrap();

        assert!(mismatches.is_empty());
        assert_eq!(summary.metadata_matches, 1);
    }

    #[test]
    fn detects_extra_messages() {
        let mut source = HashMap::new();
        source.insert(
            key("1"),
            ExtractedMessage {
                message_id: Some("<a@example.com>".to_string()),
                uid: Some("1".to_string()),
                size_bytes: Some(1000),
                internal_date: Some("2024-01-01".to_string()),
            },
        );

        let mut dest = HashMap::new();
        dest.insert(
            key("1"),
            ExtractedMessage {
                message_id: Some("<a@example.com>".to_string()),
                uid: Some("1".to_string()),
                size_bytes: Some(1000),
                internal_date: Some("2024-01-01".to_string()),
            },
        );
        dest.insert(
            key("2"),
            ExtractedMessage {
                message_id: Some("<c@example.com>".to_string()),
                uid: Some("2".to_string()),
                size_bytes: Some(3000),
                internal_date: Some("2024-01-03".to_string()),
            },
        );

        let (mismatches, summary) =
            MessageVerification::detect_mismatches("job1", "run1", &source, &dest).unwrap();

        assert_eq!(summary.extra_count, 1);
        assert_eq!(summary.missing_count, 0);

        let extra = mismatches
            .iter()
            .find(|m| m.mismatch_type == MismatchType::Extra)
            .unwrap();
        assert_eq!(extra.dest_uid, Some("2".to_string()));
    }

    #[test]
    fn detects_same_metadata_with_different_content_fingerprints() {
        let source_key = key("1");
        let dest_key = key("99");
        let message = ExtractedMessage {
            message_id: Some("<same@example.com>".into()),
            uid: Some("1".into()),
            size_bytes: Some(100),
            internal_date: Some("2024-01-01".into()),
        };
        let mut source = HashMap::new();
        source.insert(source_key.clone(), message.clone());
        let mut destination = HashMap::new();
        destination.insert(
            dest_key.clone(),
            ExtractedMessage {
                uid: Some("99".into()),
                ..message
            },
        );
        let mut source_fingerprints = HashMap::new();
        source_fingerprints.insert(source_key, "aaaa".into());
        let mut destination_fingerprints = HashMap::new();
        destination_fingerprints.insert(dest_key, "bbbb".into());

        let (mismatches, summary) =
            MessageVerification::detect_mismatches_with_content_fingerprints(
                "job1",
                "run1",
                &source,
                &destination,
                &source_fingerprints,
                &destination_fingerprints,
                &HashMap::new(),
            )
            .unwrap();

        assert_eq!(summary.metadata_matches, 0);
        assert_eq!(summary.changed_count, 1);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].source_fingerprint.as_deref(), Some("aaaa"));
        assert_eq!(
            mismatches[0].destination_fingerprint.as_deref(),
            Some("bbbb")
        );
    }

    #[test]
    fn content_match_does_not_erase_existing_metadata_mismatch() {
        let source_key = key("1");
        let dest_key = key("99");
        let source = HashMap::from([(
            source_key.clone(),
            ExtractedMessage {
                message_id: Some("<same@example.com>".into()),
                uid: Some("1".into()),
                size_bytes: Some(100),
                internal_date: Some("2024-01-01".into()),
            },
        )]);
        let destination = HashMap::from([(
            dest_key.clone(),
            ExtractedMessage {
                message_id: Some("<same@example.com>".into()),
                uid: Some("99".into()),
                size_bytes: Some(200),
                internal_date: Some("2024-01-01".into()),
            },
        )]);
        let source_fingerprints = HashMap::from([(source_key, "same-body".into())]);
        let destination_fingerprints = HashMap::from([(dest_key, "same-body".into())]);

        let (mismatches, summary) =
            MessageVerification::detect_mismatches_with_content_fingerprints(
                "job1",
                "run1",
                &source,
                &destination,
                &source_fingerprints,
                &destination_fingerprints,
                &HashMap::new(),
            )
            .unwrap();

        assert_eq!(summary.metadata_matches, 0);
        assert_eq!(summary.changed_count, 1);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].mismatch_type, MismatchType::MessageIdOnly);
    }

    #[test]
    fn duplicate_message_ids_match_content_as_a_multiset() {
        let source_key_a = key("1");
        let source_key_b = key("2");
        let dest_key_a = key("99");
        let dest_key_b = key("100");
        let message = ExtractedMessage {
            message_id: Some("<duplicate@example.com>".into()),
            uid: None,
            size_bytes: Some(100),
            internal_date: Some("2024-01-01".into()),
        };
        let source = HashMap::from([
            (source_key_a.clone(), message.clone()),
            (source_key_b.clone(), message.clone()),
        ]);
        let destination = HashMap::from([
            (dest_key_a.clone(), message.clone()),
            (dest_key_b.clone(), message),
        ]);
        let source_fingerprints =
            HashMap::from([(source_key_a, "x".into()), (source_key_b, "y".into())]);
        let destination_fingerprints =
            HashMap::from([(dest_key_a, "y".into()), (dest_key_b, "x".into())]);

        let (mismatches, summary) =
            MessageVerification::detect_mismatches_with_content_fingerprints(
                "job1",
                "run1",
                &source,
                &destination,
                &source_fingerprints,
                &destination_fingerprints,
                &HashMap::new(),
            )
            .unwrap();
        assert!(mismatches.is_empty());
        assert_eq!(summary.metadata_matches, 2);
        assert_eq!(summary.changed_count, 0);
    }

    #[test]
    fn detects_changed_messages() {
        let mut source = HashMap::new();
        source.insert(
            key("1"),
            ExtractedMessage {
                message_id: Some("<a@example.com>".to_string()),
                uid: Some("1".to_string()),
                size_bytes: Some(1000),
                internal_date: Some("2024-01-01".to_string()),
            },
        );

        let mut dest = HashMap::new();
        dest.insert(
            key("1"),
            ExtractedMessage {
                message_id: Some("<a@example.com>".to_string()),
                uid: Some("1".to_string()),
                size_bytes: Some(2000), // Different size
                internal_date: Some("2024-01-01".to_string()),
            },
        );

        let (_mismatches, summary) =
            MessageVerification::detect_mismatches("job1", "run1", &source, &dest).unwrap();

        assert_eq!(summary.changed_count, 1);
        assert!(!summary.is_perfect_metadata_match());
    }

    #[test]
    fn mismatches_preserve_complete_mailbox_local_identity() {
        let source_key = MailboxMessageKey::with_uidvalidity("INBOX", 101, "42");
        let destination_key = MailboxMessageKey::with_uidvalidity("Migrated/INBOX", 909, "7742");
        let source = HashMap::from([(
            source_key,
            ExtractedMessage {
                message_id: Some("<identity@example.com>".to_owned()),
                uid: Some("42".to_owned()),
                size_bytes: Some(1_000),
                internal_date: Some("2024-01-01".to_owned()),
            },
        )]);
        let destination = HashMap::from([(
            destination_key,
            ExtractedMessage {
                message_id: Some("<identity@example.com>".to_owned()),
                uid: Some("7742".to_owned()),
                size_bytes: Some(1_001),
                internal_date: Some("2024-01-02".to_owned()),
            },
        )]);

        let (mismatches, _) = MessageVerification::detect_mismatches_with_folder_mapping(
            "job1",
            "run-local-identity",
            &source,
            &destination,
            &HashMap::from([("INBOX".to_owned(), "Migrated/INBOX".to_owned())]),
        )
        .unwrap();

        assert_eq!(mismatches.len(), 1);
        let mismatch = &mismatches[0];
        assert_eq!(mismatch.source_folder.as_deref(), Some("INBOX"));
        assert_eq!(
            mismatch.destination_folder.as_deref(),
            Some("Migrated/INBOX")
        );
        assert_eq!(mismatch.source_uidvalidity, Some(101));
        assert_eq!(mismatch.destination_uidvalidity, Some(909));
        assert_eq!(mismatch.source_uid.as_deref(), Some("42"));
        assert_eq!(mismatch.dest_uid.as_deref(), Some("7742"));
    }

    #[test]
    fn matching_message_in_wrong_folder_is_not_counted_as_metadata_match() {
        let message = ExtractedMessage {
            message_id: Some("<wrong-folder@example.com>".to_owned()),
            uid: Some("1".to_owned()),
            size_bytes: Some(1_000),
            internal_date: Some("2024-01-01".to_owned()),
        };
        let source = HashMap::from([(MailboxMessageKey::new("INBOX", "1"), message.clone())]);
        let destination = HashMap::from([(MailboxMessageKey::new("WrongFolder", "9"), message)]);

        let (mismatches, summary) = MessageVerification::detect_mismatches_with_folder_mapping(
            "job1",
            "run-wrong-folder",
            &source,
            &destination,
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(summary.metadata_matches, 0);
        assert_eq!(summary.changed_count, 1);
        assert_eq!(summary.missing_count, 0);
        assert_eq!(summary.extra_count, 0);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(
            mismatches[0].mismatch_type,
            MismatchType::PresentWrongFolder
        );
        assert_eq!(
            MismatchType::PresentWrongFolder.as_str(),
            "message_present_wrong_folder"
        );
    }

    #[test]
    fn wrong_folder_candidate_selection_is_deterministic() {
        let message = ExtractedMessage {
            message_id: Some("<deterministic@example.com>".to_owned()),
            uid: Some("1".to_owned()),
            size_bytes: Some(1_000),
            internal_date: Some("2024-01-01".to_owned()),
        };
        let source = HashMap::from([(MailboxMessageKey::new("INBOX", "1"), message.clone())]);
        let destination = HashMap::from([
            (MailboxMessageKey::new("Z-Folder", "9"), message.clone()),
            (MailboxMessageKey::new("A-Folder", "8"), message),
        ]);

        let (mismatches, _) = MessageVerification::detect_mismatches_with_folder_mapping(
            "job1",
            "run-deterministic-folder",
            &source,
            &destination,
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(
            mismatches[0].mismatch_type,
            MismatchType::PresentWrongFolder
        );
        assert_eq!(
            mismatches[0].destination_folder.as_deref(),
            Some("A-Folder")
        );
        assert_eq!(mismatches[0].dest_uid.as_deref(), Some("8"));
    }

    #[test]
    fn message_id_without_metadata_is_not_exact_proof() {
        let message = ExtractedMessage {
            message_id: Some("<metadata-absent@example.com>".to_string()),
            uid: Some("1".to_string()),
            size_bytes: None,
            internal_date: None,
        };
        let source = HashMap::from([(key("1"), message.clone())]);
        let destination = HashMap::from([(key("99"), message)]);

        let (mismatches, summary) =
            MessageVerification::detect_mismatches("job1", "run-id-only", &source, &destination)
                .unwrap();

        assert_eq!(summary.metadata_matches, 0);
        assert_eq!(summary.changed_count, 1);
        assert!(!mismatches.is_empty());
        assert!(!summary.is_perfect_metadata_match());
    }

    #[test]
    fn one_missing_metadata_field_is_not_treated_as_matching() {
        let source = HashMap::from([(
            key("1"),
            ExtractedMessage {
                message_id: Some("<one-sided@example.com>".to_string()),
                uid: Some("1".to_string()),
                size_bytes: Some(100),
                internal_date: None,
            },
        )]);
        let destination = HashMap::from([(
            key("2"),
            ExtractedMessage {
                message_id: Some("<one-sided@example.com>".to_string()),
                uid: Some("2".to_string()),
                size_bytes: Some(100),
                internal_date: None,
            },
        )]);

        let (_, summary) =
            MessageVerification::detect_mismatches("job1", "run-one-sided", &source, &destination)
                .unwrap();

        assert_eq!(summary.metadata_matches, 0);
        assert_eq!(summary.changed_count, 1);
        assert!(!summary.is_perfect_metadata_match());
    }

    #[test]
    fn perfect_match_when_identical() {
        let mut source = HashMap::new();
        source.insert(
            key("1"),
            ExtractedMessage {
                message_id: Some("<a@example.com>".to_string()),
                uid: Some("1".to_string()),
                size_bytes: Some(1000),
                internal_date: Some("2024-01-01".to_string()),
            },
        );

        let dest = source.clone();

        let (_mismatches, summary) =
            MessageVerification::detect_mismatches("job1", "run1", &source, &dest).unwrap();

        assert!(summary.is_perfect_metadata_match());
        assert_eq!(summary.metadata_matches, 1);
    }

    #[test]
    fn validation_rejects_duplicate_classification_and_omitted_identity() {
        let empty_message = || ExtractedMessage {
            message_id: None,
            uid: None,
            size_bytes: None,
            internal_date: None,
        };
        let source = HashMap::from([(key("1"), empty_message()), (key("2"), empty_message())]);
        let destination = source.clone();
        let first_source = source.keys().find(|key| key.uid == "1").unwrap();
        let first_destination = destination.keys().find(|key| key.uid == "1").unwrap();
        let membership = VerificationMembership {
            matched_source: HashSet::from([first_source]),
            matched_destination: HashSet::from([first_destination]),
            probable_source: HashSet::from([first_source]),
            probable_destination: HashSet::from([first_destination]),
            ..Default::default()
        };
        let summary = VerificationSummary {
            total_source: 2,
            total_destination: 2,
            metadata_matches: 1,
            probable_matches: 1,
            missing_count: 0,
            extra_count: 0,
            duplicated_count: 0,
            changed_count: 0,
        };

        let error =
            validate_verification_summary(&source, &destination, &[], &summary, &membership)
                .unwrap_err();
        assert!(error.contains("unclassified") || error.contains("classifications"));

        let second_destination = destination.keys().find(|key| key.uid == "2").unwrap();
        let missing_membership = VerificationMembership {
            matched_source: HashSet::from([first_source]),
            matched_destination: HashSet::from([first_destination, second_destination]),
            ..Default::default()
        };
        let error = validate_verification_summary(
            &source,
            &destination,
            &[],
            &summary,
            &missing_membership,
        )
        .unwrap_err();
        assert!(error.contains("source message") && error.contains("unclassified"));
    }

    #[test]
    fn destination_uid_rewrite_is_not_reported_as_missing_and_extra() {
        let source = HashMap::from([(
            key("17"),
            ExtractedMessage {
                message_id: Some("<stable@example.com>".to_string()),
                uid: Some("17".to_string()),
                size_bytes: Some(4096),
                internal_date: Some("2024-01-01T00:00:00Z".to_string()),
            },
        )]);
        let destination = HashMap::from([(
            key("904"),
            ExtractedMessage {
                message_id: Some("<stable@example.com>".to_string()),
                uid: Some("904".to_string()),
                size_bytes: Some(4096),
                internal_date: Some("2024-01-01T00:00:00Z".to_string()),
            },
        )]);

        let (mismatches, summary) = MessageVerification::detect_mismatches(
            "job1",
            "run-uid-rewrite",
            &source,
            &destination,
        )
        .unwrap();

        assert!(mismatches.is_empty());
        assert!(summary.is_perfect_metadata_match());
        assert_eq!(summary.metadata_matches, 1);
    }

    #[test]
    fn date_and_size_fallback_is_probable_not_exact() {
        let source = HashMap::from([(
            key("1"),
            ExtractedMessage {
                message_id: None,
                uid: Some("1".to_string()),
                size_bytes: Some(512),
                internal_date: Some("2024-01-01T00:00:00Z".to_string()),
            },
        )]);
        let destination = HashMap::from([(
            key("88"),
            ExtractedMessage {
                message_id: None,
                uid: Some("88".to_string()),
                size_bytes: Some(512),
                internal_date: Some("2024-01-01T00:00:00Z".to_string()),
            },
        )]);

        let (mismatches, summary) = MessageVerification::detect_mismatches(
            "job1",
            "run-fingerprint",
            &source,
            &destination,
        )
        .unwrap();

        assert!(mismatches.is_empty());
        assert!(!summary.is_perfect_metadata_match());
        assert_eq!(summary.metadata_matches, 0);
        assert_eq!(summary.probable_matches, 1);
    }

    #[test]
    fn ambiguous_fingerprint_fails_closed_instead_of_pairing_by_uid() {
        let message = |uid: &str| ExtractedMessage {
            message_id: None,
            uid: Some(uid.to_string()),
            size_bytes: Some(512),
            internal_date: Some("2024-01-01T00:00:00Z".to_string()),
        };
        let source = HashMap::from([(key("1"), message("1")), (key("2"), message("2"))]);
        let destination = HashMap::from([(key("88"), message("88")), (key("89"), message("89"))]);

        let (mismatches, summary) =
            MessageVerification::detect_mismatches("job1", "run-ambiguous", &source, &destination)
                .unwrap();

        assert_eq!(summary.missing_count, 2);
        assert_eq!(summary.extra_count, 2);
        assert!(!summary.is_perfect_metadata_match());
        assert_eq!(mismatches.len(), 4);
    }

    #[test]
    fn duplicate_message_id_is_reported_without_using_uid_as_identity() {
        let message = |uid: &str| ExtractedMessage {
            message_id: Some("<duplicate@example.com>".to_string()),
            uid: Some(uid.to_string()),
            size_bytes: Some(512),
            internal_date: Some("2024-01-01T00:00:00Z".to_string()),
        };
        let source = HashMap::from([(key("1"), message("1"))]);
        let destination = HashMap::from([(key("88"), message("88")), (key("89"), message("89"))]);

        let (mismatches, summary) =
            MessageVerification::detect_mismatches("job1", "run-duplicate", &source, &destination)
                .unwrap();

        assert_eq!(summary.metadata_matches, 1);
        assert_eq!(summary.extra_count, 0);
        assert!(
            mismatches
                .iter()
                .any(|m| m.mismatch_type == MismatchType::Duplicated)
        );
    }

    #[test]
    fn preserved_source_duplicates_are_not_reported_as_created_duplicates() {
        let message = |uid: &str| ExtractedMessage {
            message_id: Some("<preserved@example.com>".to_string()),
            uid: Some(uid.to_string()),
            size_bytes: Some(512),
            internal_date: Some("2024-01-01T00:00:00Z".to_string()),
        };
        let source = HashMap::from([
            (MailboxMessageKey::new("INBOX", "1"), message("1")),
            (MailboxMessageKey::new("Archive", "1"), message("1")),
        ]);
        let destination = HashMap::from([
            (MailboxMessageKey::new("INBOX", "81"), message("81")),
            (MailboxMessageKey::new("Archive", "92"), message("92")),
        ]);

        let (mismatches, summary) = MessageVerification::detect_mismatches(
            "job1",
            "run-preserved-duplicate",
            &source,
            &destination,
        )
        .unwrap();

        assert!(mismatches.is_empty());
        assert_eq!(summary.metadata_matches, 2);
        assert!(summary.is_perfect_metadata_match());
    }

    #[test]
    fn duplicate_message_id_groups_match_metadata_multisets_not_key_order() {
        let message = |uid: &str, size: u64, date: &str| ExtractedMessage {
            message_id: Some("<same-id@example.com>".to_string()),
            uid: Some(uid.to_string()),
            size_bytes: Some(size),
            internal_date: Some(date.to_string()),
        };
        let source = HashMap::from([
            (key("1"), message("1", 10_000, "2024-01-01")),
            (key("2"), message("2", 30_000, "2024-01-02")),
        ]);
        // Destination key order is deliberately the inverse of the source
        // metadata order: UID 10 contains source B and UID 20 contains A.
        let destination = HashMap::from([
            (key("10"), message("10", 30_000, "2024-01-02")),
            (key("20"), message("20", 10_000, "2024-01-01")),
        ]);

        let (mismatches, summary) = MessageVerification::detect_mismatches(
            "job1",
            "run-reordered-duplicates",
            &source,
            &destination,
        )
        .unwrap();

        assert!(mismatches.is_empty());
        assert_eq!(summary.metadata_matches, 2);
        assert_eq!(summary.changed_count, 0);
        assert!(summary.is_perfect_metadata_match());
    }

    #[test]
    fn duplicate_message_id_groups_report_only_genuine_changed_leftovers() {
        let message = |uid: &str, size: u64, date: &str| ExtractedMessage {
            message_id: Some("<same-id@example.com>".to_string()),
            uid: Some(uid.to_string()),
            size_bytes: Some(size),
            internal_date: Some(date.to_string()),
        };
        let source = HashMap::from([
            (key("1"), message("1", 10_000, "2024-01-01")),
            (key("2"), message("2", 30_000, "2024-01-02")),
        ]);
        let destination = HashMap::from([
            (key("10"), message("10", 30_000, "2024-01-02")),
            (key("20"), message("20", 31_000, "2024-01-03")),
        ]);

        let (mismatches, summary) = MessageVerification::detect_mismatches(
            "job1",
            "run-changed-duplicate",
            &source,
            &destination,
        )
        .unwrap();

        assert_eq!(summary.metadata_matches, 1);
        assert_eq!(summary.changed_count, 1);
        assert_eq!(summary.missing_count, 0);
        assert_eq!(summary.extra_count, 0);
        assert_eq!(summary.duplicated_count, 0);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].mismatch_type, MismatchType::MessageIdOnly);
        assert!(!summary.is_perfect_metadata_match());
    }

    #[test]
    fn repeated_destination_message_id_without_source_is_extra_not_duplicate() {
        let message = |uid: &str| ExtractedMessage {
            message_id: Some("<destination-only@example.com>".to_string()),
            uid: Some(uid.to_string()),
            size_bytes: Some(512),
            internal_date: Some("2024-01-01T00:00:00Z".to_string()),
        };
        let source = HashMap::new();
        let destination = HashMap::from([
            (MailboxMessageKey::new("INBOX", "81"), message("81")),
            (MailboxMessageKey::new("Archive", "92"), message("92")),
        ]);

        let (mismatches, summary) = MessageVerification::detect_mismatches(
            "job1",
            "run-destination-only",
            &source,
            &destination,
        )
        .unwrap();

        assert_eq!(summary.extra_count, 2);
        assert_eq!(
            mismatches
                .iter()
                .filter(|m| m.mismatch_type == MismatchType::Duplicated)
                .count(),
            0
        );
    }

    #[test]
    fn evidence_level_is_fail_closed_and_named() {
        let summary = |metadata_matches,
                       probable_matches,
                       missing_count,
                       extra_count,
                       duplicated_count,
                       changed_count| VerificationSummary {
            total_source: 1_000,
            total_destination: 1_000,
            metadata_matches,
            probable_matches,
            missing_count,
            extra_count,
            duplicated_count,
            changed_count,
        };

        assert_eq!(
            summary(1_000, 0, 0, 0, 0, 0).evidence_level(),
            EvidenceLevel::MetadataMatched
        );
        assert_eq!(
            summary(1_000, 0, 0, 0, 0, 0).evidence_level().as_str(),
            "metadata_matched"
        );
        assert_eq!(
            summary(950, 0, 0, 0, 0, 0).evidence_level(),
            EvidenceLevel::StrongMetadataMatch
        );
        assert_eq!(
            summary(0, 1, 0, 0, 0, 0).evidence_level(),
            EvidenceLevel::ProbableMatch
        );
        assert_eq!(
            summary(950, 0, 10_000, 0, 0, 0).evidence_level(),
            EvidenceLevel::Missing
        );
        assert_eq!(
            summary(950, 0, 0, 0, 0, 1).evidence_level(),
            EvidenceLevel::Changed
        );
        assert_eq!(
            summary(950, 0, 0, 1, 0, 0).evidence_level(),
            EvidenceLevel::Unexpected
        );
    }

    #[test]
    fn staged_reconciliation_matches_in_memory_accounting() {
        let message = |id: Option<&str>, uid: &str, size: u64, date: &str| ExtractedMessage {
            message_id: id.map(str::to_owned),
            uid: Some(uid.to_owned()),
            size_bytes: Some(size),
            internal_date: Some(date.to_owned()),
        };
        let source = ExtractedMessages::from([
            (
                MailboxMessageKey::new("INBOX", "1"),
                message(Some("<a>"), "1", 10, "01-Jan-2024 00:00:00 +0000"),
            ),
            (
                MailboxMessageKey::new("INBOX", "2"),
                message(Some("<b>"), "2", 20, "01-Jan-2024 00:00:00 +0000"),
            ),
            (
                MailboxMessageKey::new("INBOX", "3"),
                message(None, "3", 30, "01-Jan-2024 00:00:00 +0000"),
            ),
            (
                MailboxMessageKey::new("INBOX", "4"),
                message(Some("<missing>"), "4", 40, "01-Jan-2024 00:00:00 +0000"),
            ),
        ]);
        let destination = ExtractedMessages::from([
            (
                MailboxMessageKey::new("INBOX", "11"),
                message(Some("<a>"), "11", 10, "01-Jan-2024 00:00:00 +0000"),
            ),
            (
                MailboxMessageKey::new("INBOX", "12"),
                message(Some("<b>"), "12", 21, "01-Jan-2024 00:00:00 +0000"),
            ),
            (
                MailboxMessageKey::new("INBOX", "13"),
                message(None, "13", 30, "01-Jan-2024 00:00:00 +0000"),
            ),
            (
                MailboxMessageKey::new("INBOX", "14"),
                message(Some("<extra>"), "14", 50, "01-Jan-2024 00:00:00 +0000"),
            ),
        ]);
        let folder_mapping = HashMap::new();
        let (expected_mismatches, expected_summary) = MessageVerification::detect_mismatches(
            "job-staged",
            "run-staged-map",
            &source,
            &destination,
        )
        .unwrap();
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        stage
            .insert_messages(StagedMessageSide::Source, &source)
            .unwrap();
        stage
            .insert_messages(StagedMessageSide::Destination, &destination)
            .unwrap();
        let (staged_mismatches, staged_summary) =
            MessageVerification::detect_mismatches_from_stage(
                "job-staged",
                "run-staged-stage",
                &stage,
                &folder_mapping,
            )
            .unwrap();
        assert_eq!(staged_summary.total_source, expected_summary.total_source);
        assert_eq!(
            staged_summary.total_destination,
            expected_summary.total_destination
        );
        assert_eq!(
            staged_summary.metadata_matches,
            expected_summary.metadata_matches
        );
        assert_eq!(
            staged_summary.probable_matches,
            expected_summary.probable_matches
        );
        assert_eq!(staged_summary.missing_count, expected_summary.missing_count);
        assert_eq!(staged_summary.extra_count, expected_summary.extra_count);
        assert_eq!(
            staged_summary.duplicated_count,
            expected_summary.duplicated_count
        );
        assert_eq!(staged_summary.changed_count, expected_summary.changed_count);
        let mut expected_types = expected_mismatches
            .iter()
            .map(|mismatch| mismatch.mismatch_type.clone())
            .collect::<Vec<_>>();
        let mut staged_types = staged_mismatches
            .iter()
            .map(|mismatch| mismatch.mismatch_type.clone())
            .collect::<Vec<_>>();
        expected_types.sort_by_key(MismatchType::as_str);
        staged_types.sort_by_key(MismatchType::as_str);
        assert_eq!(staged_types, expected_types);
    }

    #[test]
    fn staged_reconciliation_matches_duplicate_and_mapping_cases() {
        let message = |id: Option<&str>, uid: &str, size: u64, date: &str| ExtractedMessage {
            message_id: id.map(str::to_owned),
            uid: Some(uid.to_owned()),
            size_bytes: Some(size),
            internal_date: Some(date.to_owned()),
        };
        let cases = [
            (
                ExtractedMessages::from([
                    (
                        MailboxMessageKey::new("INBOX", "1"),
                        message(Some("<duplicate>"), "1", 10, "01-Jan-2024 00:00:00 +0000"),
                    ),
                    (
                        MailboxMessageKey::new("INBOX", "2"),
                        message(Some("<duplicate>"), "2", 20, "01-Jan-2024 00:00:00 +0000"),
                    ),
                ]),
                ExtractedMessages::from([
                    (
                        MailboxMessageKey::new("Migrated", "9"),
                        message(Some("<duplicate>"), "9", 10, "01-Jan-2024 00:00:00 +0000"),
                    ),
                    (
                        MailboxMessageKey::new("Migrated", "10"),
                        message(Some("<duplicate>"), "10", 20, "01-Jan-2024 00:00:00 +0000"),
                    ),
                    (
                        MailboxMessageKey::new("Migrated", "11"),
                        message(Some("<duplicate>"), "11", 20, "01-Jan-2024 00:00:00 +0000"),
                    ),
                ]),
                HashMap::from([(String::from("INBOX"), String::from("Migrated"))]),
            ),
            (
                ExtractedMessages::from([
                    (
                        MailboxMessageKey::new("INBOX", "1"),
                        message(
                            Some("<wrong-folder>"),
                            "1",
                            30,
                            "01-Jan-2024 00:00:00 +0000",
                        ),
                    ),
                    (
                        MailboxMessageKey::new("Sent", "2"),
                        message(Some("<probable>"), "2", 40, "02-Jan-2024 00:00:00 +0000"),
                    ),
                ]),
                ExtractedMessages::from([
                    (
                        MailboxMessageKey::new("Archive", "7"),
                        message(
                            Some("<wrong-folder>"),
                            "7",
                            30,
                            "01-Jan-2024 00:00:00 +0000",
                        ),
                    ),
                    (
                        MailboxMessageKey::new("Sent", "8"),
                        message(None, "8", 40, "02-Jan-2024 00:00:00 +0000"),
                    ),
                ]),
                HashMap::new(),
            ),
            (
                ExtractedMessages::from([(
                    MailboxMessageKey::new("INBOX", "1"),
                    message(Some("<changed>"), "1", 50, "03-Jan-2024 00:00:00 +0000"),
                )]),
                ExtractedMessages::from([(
                    MailboxMessageKey::new("INBOX", "9"),
                    message(Some("<changed>"), "9", 51, "03-Jan-2024 00:00:00 +0000"),
                )]),
                HashMap::new(),
            ),
        ];

        for (index, (source, destination, folder_mapping)) in cases.into_iter().enumerate() {
            let (expected_mismatches, expected_summary) =
                MessageVerification::detect_mismatches_with_folder_mapping(
                    "job-staged-cases",
                    &format!("run-map-{index}"),
                    &source,
                    &destination,
                    &folder_mapping,
                )
                .unwrap();
            let mut stage = MessageMetadataStage::open_in_memory().unwrap();
            stage
                .insert_messages(StagedMessageSide::Source, &source)
                .unwrap();
            stage
                .insert_messages(StagedMessageSide::Destination, &destination)
                .unwrap();
            let (staged_mismatches, staged_summary) =
                MessageVerification::detect_mismatches_from_stage(
                    "job-staged-cases",
                    &format!("run-stage-{index}"),
                    &stage,
                    &folder_mapping,
                )
                .unwrap();
            assert_eq!(staged_summary, expected_summary, "case {index}");
            let mut expected = expected_mismatches
                .iter()
                .map(|mismatch| {
                    (
                        mismatch.mismatch_type.clone(),
                        mismatch.source_uid.clone(),
                        mismatch.dest_uid.clone(),
                    )
                })
                .collect::<Vec<_>>();
            let mut staged = staged_mismatches
                .iter()
                .map(|mismatch| {
                    (
                        mismatch.mismatch_type.clone(),
                        mismatch.source_uid.clone(),
                        mismatch.dest_uid.clone(),
                    )
                })
                .collect::<Vec<_>>();
            expected.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
            staged.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
            assert_eq!(staged, expected, "case {index}");
        }
    }
}
