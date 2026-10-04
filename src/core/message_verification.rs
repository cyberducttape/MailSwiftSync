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

    /// Reconcile metadata first, then enrich the reconciler's own pairs with
    /// independently fetched RFC822 fingerprints:
    ///
    /// identity -> placement -> metadata -> content
    ///
    /// Content never forms a pairing the metadata reconciler rejected. It may
    /// (a) resolve a Missing source and an Extra destination that share a
    /// unique fingerprint in the expected folder, and (b) reclassify a clean
    /// metadata pair whose bodies differ as changed. Every source and
    /// destination message keeps exactly one classification, which is
    /// re-validated before returning.
    pub fn detect_mismatches_with_content_fingerprints(
        job_id: &str,
        run_id: &str,
        source_messages: &ExtractedMessages,
        dest_messages: &ExtractedMessages,
        source_fingerprints: &HashMap<MailboxMessageKey, String>,
        dest_fingerprints: &HashMap<MailboxMessageKey, String>,
        folder_mapping: &HashMap<String, String>,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        for (side, messages, fingerprints) in [
            ("source", source_messages, source_fingerprints),
            ("destination", dest_messages, dest_fingerprints),
        ] {
            // Equal sizes plus one-way containment imply equal key sets.
            if messages.len() != fingerprints.len()
                || messages.keys().any(|key| !fingerprints.contains_key(key))
            {
                return Err(format!(
                    "content verification requires exact {side} message/fingerprint key coverage"
                ));
            }
        }
        let job_context: Arc<str> = Arc::from(job_id);
        let run_context: Arc<str> = Arc::from(run_id);
        let MetadataReconciliation {
            mut mismatches,
            mut summary,
            mut membership,
            clean_pairs,
        } = Self::reconcile_metadata(
            job_id,
            run_id,
            source_messages,
            dest_messages,
            folder_mapping,
        )?;

        // (a) A unique content fingerprint is a fallback identity when the
        // metadata pass could not pair a message. Only a Missing source and
        // an Extra destination in the expected folder are eligible; a
        // stronger classification (changed, duplicated, probable) is never
        // overridden.
        let source_by_fingerprint = unique_fingerprint_index(source_fingerprints);
        let dest_by_fingerprint = unique_fingerprint_index(dest_fingerprints);
        let mut fingerprint_pairs = source_by_fingerprint
            .iter()
            .filter_map(|(fingerprint, source_key)| {
                let source_key = source_messages.get_key_value(*source_key)?.0;
                let dest_key = dest_messages
                    .get_key_value(*dest_by_fingerprint.get(fingerprint)?)?
                    .0;
                (membership.missing_source.contains(source_key)
                    && membership.extra_destination.contains(dest_key)
                    && expected_destination_folder(source_key, folder_mapping)
                        == dest_key.mailbox.as_ref())
                .then_some((source_key, dest_key))
            })
            .collect::<Vec<_>>();
        fingerprint_pairs.sort();
        if !fingerprint_pairs.is_empty() {
            let source_index = build_uid_folder_index(source_messages);
            let dest_index = build_uid_folder_index(dest_messages);
            let resolved_source = fingerprint_pairs
                .iter()
                .map(|(source_key, _)| *source_key)
                .collect::<HashSet<_>>();
            let resolved_destination = fingerprint_pairs
                .iter()
                .map(|(_, dest_key)| *dest_key)
                .collect::<HashSet<_>>();
            mismatches.retain(|mismatch| match mismatch.mismatch_type {
                MismatchType::Missing => mismatch_source_key(mismatch, &source_index)
                    .is_none_or(|key| !resolved_source.contains(key)),
                MismatchType::Extra => mismatch_destination_key(mismatch, &dest_index)
                    .is_none_or(|key| !resolved_destination.contains(key)),
                _ => true,
            });
            for (source_key, dest_key) in fingerprint_pairs {
                membership.missing_source.remove(source_key);
                membership.extra_destination.remove(dest_key);
                membership.matched_source.insert(source_key);
                membership.matched_destination.insert(dest_key);
                summary.missing_count -= 1;
                summary.extra_count -= 1;
                summary.metadata_matches += 1;
            }
        }

        // (b) Content comparison of the reconciler's clean pairs only.
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
        let mut clean_pairs = clean_pairs;
        clean_pairs.sort();
        for (source_key, dest_key) in clean_pairs {
            let (Some(source_fingerprint), Some(destination_fingerprint)) = (
                source_fingerprints.get(source_key),
                dest_fingerprints.get(dest_key),
            ) else {
                return Err(
                    "content verification encountered a missing fingerprint after coverage validation"
                        .into(),
                );
            };
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
            membership.matched_source.remove(source_key);
            membership.matched_destination.remove(dest_key);
            membership.changed_source.insert(source_key);
            membership.changed_destination.insert(dest_key);
            summary.metadata_matches -= 1;
            summary.changed_count += 1;
        }

        validate_verification_summary(
            source_messages,
            dest_messages,
            &mismatches,
            &summary,
            &membership,
        )?;
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
    ) -> Pass1Result<'a> {
        let mut mismatches = Vec::new();
        let mut estimated_bytes = 0usize;
        let mut matched_source = HashSet::new();
        let mut matched_dest = HashSet::new();
        let mut clean_pairs = Vec::new();

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
                    clean_pairs.push((*source_key, dest_key));
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

        Ok((mismatches, matched_source, matched_dest, clean_pairs))
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
                let lower = folder_candidates.range(..expected_folder).next_back();
                let upper = folder_candidates
                    .range::<&str, _>((Excluded(expected_folder), Unbounded))
                    .next();
                // Match the staged SQL rule: choose the greatest folder below
                // the expected folder, falling back to the smallest above it.
                let candidate_folder = lower
                    .map(|(folder, _)| *folder)
                    .or_else(|| upper.map(|(folder, _)| *folder));
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

        let mut probable_matches = 0_u64;
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

    #[cfg(test)]
    pub fn detect_mismatches_with_folder_mapping(
        job_id: &str,
        run_id: &str,
        source_messages: &ExtractedMessages,
        dest_messages: &ExtractedMessages,
        folder_mapping: &HashMap<String, String>,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        let reconciliation = Self::reconcile_metadata(
            job_id,
            run_id,
            source_messages,
            dest_messages,
            folder_mapping,
        )?;
        Ok((reconciliation.mismatches, reconciliation.summary))
    }

    #[allow(unused_assignments, clippy::collapsible_if, clippy::needless_borrow)]
    fn reconcile_metadata<'a>(
        job_id: &str,
        run_id: &str,
        source_messages: &'a ExtractedMessages,
        dest_messages: &'a ExtractedMessages,
        folder_mapping: &'a HashMap<String, String>,
    ) -> Result<MetadataReconciliation<'a>, String> {
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
        let (mut pass1_mismatches, pass1_matched_src, pass1_matched_dst, clean_pairs) =
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

        Ok(MetadataReconciliation {
            mismatches: all_mismatches,
            summary,
            membership,
            clean_pairs,
        })
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
        // One transaction for the whole pass. A durable stage runs with FULL
        // synchronization, so autocommitting each matched-row insert costs a
        // disk sync per message. Reconciliation is reset and recomputed on
        // every run, so rolling back an interrupted pass loses nothing.
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin staged reconciliation: {error}"))?;
        connection
            .execute_batch(
                // Keep reconciliation intermediates in the private stage
                // database. TEMP tables may spill into SQLite's process-wide
                // temporary directory when temp_store=FILE, escaping the
                // per-run permissions and cleanup lifecycle.
                "CREATE TABLE staged_matched(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uid TEXT NOT NULL, PRIMARY KEY(side,mailbox,uidvalidity,uid));
                 CREATE TABLE staged_folder_mapping(source TEXT PRIMARY KEY, destination TEXT NOT NULL);",
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
        // Precompute the effective destination folder and let SQLite seek on
        // it directly during fallback. Evaluating this mapping expression in
        // each count query can repeatedly scan records from every folder that
        // shares the same date/size pair.
        connection
            .execute(
                "UPDATE staged_messages SET match_mailbox=COALESCE((SELECT destination FROM staged_folder_mapping WHERE source=staged_messages.mailbox),mailbox) WHERE side=0",
                [],
            )
            .map_err(|error| format!("could not index staged source folder mapping: {error}"))?;

        let job_context: Arc<str> = Arc::from(job_id);
        let run_context: Arc<str> = Arc::from(run_id);
        let mut mismatches = Vec::new();
        let mut estimated_bytes = 0usize;
        // Pair exact Message-ID/folder/date/size groups in SQLite. Ranked
        // rows are materialized and indexed before joining: joining two
        // unindexed window-function CTEs made SQLite choose a quadratic plan
        // on large stages. Row numbers preserve the old deterministic greedy
        // order (source staging order, then destination mailbox/UID order).
        connection
            .execute_batch(
                "CREATE TABLE staged_exact_source_ranked AS
                    SELECT rowid AS source_rowid,message_id,match_mailbox,date_key,size_bytes,
                           ROW_NUMBER() OVER (
                               PARTITION BY message_id,match_mailbox,date_key,size_bytes
                               ORDER BY rowid
                           ) AS ordinal
                    FROM staged_messages INDEXED BY staged_messages_exact_source
                    WHERE side=0 AND message_id IS NOT NULL
                      AND date_key IS NOT NULL AND size_bytes IS NOT NULL;
                 CREATE INDEX staged_exact_source_ranked_key
                    ON staged_exact_source_ranked(message_id,match_mailbox,date_key,size_bytes,ordinal);
                 CREATE TABLE staged_exact_destination_ranked AS
                    SELECT rowid AS destination_rowid,message_id,mailbox AS match_mailbox,date_key,size_bytes,
                           ROW_NUMBER() OVER (
                               PARTITION BY message_id,mailbox,date_key,size_bytes
                               ORDER BY mailbox,uidvalidity,uid
                           ) AS ordinal
                    FROM staged_messages INDEXED BY staged_messages_exact_destination
                    WHERE side=1 AND message_id IS NOT NULL
                      AND date_key IS NOT NULL AND size_bytes IS NOT NULL;
                 CREATE INDEX staged_exact_destination_ranked_key
                    ON staged_exact_destination_ranked(message_id,match_mailbox,date_key,size_bytes,ordinal);
                 CREATE TABLE staged_exact_pairs AS
                    SELECT s.source_rowid,d.destination_rowid
                    FROM staged_exact_source_ranked s
                    JOIN staged_exact_destination_ranked d
                      ON d.message_id=s.message_id
                     AND d.match_mailbox=s.match_mailbox
                     AND d.date_key=s.date_key
                     AND d.size_bytes=s.size_bytes
                     AND d.ordinal=s.ordinal;
                 INSERT INTO staged_matched(side,mailbox,uidvalidity,uid)
                    SELECT 0,m.mailbox,m.uidvalidity,m.uid
                    FROM staged_exact_pairs p
                    JOIN staged_messages m ON m.rowid=p.source_rowid
                 UNION ALL
                    SELECT 1,m.mailbox,m.uidvalidity,m.uid
                    FROM staged_exact_pairs p
                    JOIN staged_messages m ON m.rowid=p.destination_rowid;
                 DROP TABLE staged_exact_pairs;
                 DROP TABLE staged_exact_source_ranked;
                 DROP TABLE staged_exact_destination_ranked;",
            )
            .map_err(|error| format!("could not reconcile exact staged metadata: {error}"))?;
        let metadata_matches = connection
            .query_row(
                "SELECT COUNT(*) FROM staged_matched WHERE side=0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("could not count exact staged metadata matches: {error}"))
            .and_then(|count: i64| {
                u64::try_from(count)
                    .map_err(|_| format!("exact staged metadata count is invalid: {count}"))
            })?;
        let mut missing_count = 0_u64;
        let mut extra_count = 0_u64;
        let mut duplicated_count = 0_u64;
        let mut changed_count = 0_u64;

        // Remaining Message-ID matches in the expected folder are changes.
        // Rank each unmatched side once, pair ordinally, and materialize the
        // pairs so the only Rust work is constructing bounded mismatch proof.
        connection
            .execute_batch(
                "CREATE TABLE staged_changed_source_ranked AS
                    SELECT rowid AS source_rowid,message_id,match_mailbox,
                           ROW_NUMBER() OVER (
                               PARTITION BY message_id,match_mailbox ORDER BY rowid
                           ) AS ordinal
                    FROM staged_messages s
                    WHERE side=0 AND message_id IS NOT NULL
                      AND NOT EXISTS (
                          SELECT 1 FROM staged_matched m
                          WHERE m.side=s.side AND m.mailbox=s.mailbox
                            AND m.uidvalidity=s.uidvalidity AND m.uid=s.uid
                      );
                 CREATE INDEX staged_changed_source_key
                    ON staged_changed_source_ranked(message_id,match_mailbox,ordinal);
                 CREATE TABLE staged_changed_destination_ranked AS
                    SELECT rowid AS destination_rowid,message_id,mailbox AS match_mailbox,
                           ROW_NUMBER() OVER (
                               PARTITION BY message_id,mailbox
                               ORDER BY mailbox,uidvalidity,uid
                           ) AS ordinal
                    FROM staged_messages d
                    WHERE side=1 AND message_id IS NOT NULL
                      AND NOT EXISTS (
                          SELECT 1 FROM staged_matched m
                          WHERE m.side=d.side AND m.mailbox=d.mailbox
                            AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid
                      );
                 CREATE INDEX staged_changed_destination_key
                    ON staged_changed_destination_ranked(message_id,match_mailbox,ordinal);
                 CREATE TABLE staged_changed_pairs AS
                    SELECT s.source_rowid,d.destination_rowid
                    FROM staged_changed_source_ranked s
                    JOIN staged_changed_destination_ranked d
                      ON d.message_id=s.message_id
                     AND d.match_mailbox=s.match_mailbox
                     AND d.ordinal=s.ordinal;
                 INSERT INTO staged_matched(side,mailbox,uidvalidity,uid)
                    SELECT 0,m.mailbox,m.uidvalidity,m.uid
                    FROM staged_changed_pairs p
                    JOIN staged_messages m ON m.rowid=p.source_rowid
                 UNION ALL
                    SELECT 1,m.mailbox,m.uidvalidity,m.uid
                    FROM staged_changed_pairs p
                    JOIN staged_messages m ON m.rowid=p.destination_rowid;",
            )
            .map_err(|error| format!("could not reconcile changed staged IDs: {error}"))?;
        {
            let mut statement = connection
                .prepare(
                    "SELECT s.rowid,s.mailbox,s.uidvalidity,s.uid,s.message_id,s.internal_date,s.date_key,s.size_bytes,
                            d.rowid,d.mailbox,d.uidvalidity,d.uid,d.message_id,d.internal_date,d.date_key,d.size_bytes
                     FROM staged_changed_pairs p
                     JOIN staged_messages s ON s.rowid=p.source_rowid
                     JOIN staged_messages d ON d.rowid=p.destination_rowid
                     ORDER BY p.source_rowid",
                )
                .map_err(|error| format!("could not read changed staged pairs: {error}"))?;
            let pairs = statement
                .query_map([], staged_message_pair_from_row)
                .map_err(|error| format!("could not query changed staged pairs: {error}"))?;
            for pair in pairs {
                let (source, destination) =
                    pair.map_err(|error| format!("could not decode changed staged pair: {error}"))?;
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
        connection
            .execute_batch(
                "DROP TABLE staged_changed_pairs;
                 DROP TABLE staged_changed_source_ranked;
                 DROP TABLE staged_changed_destination_ranked;",
            )
            .map_err(|error| format!("could not release changed reconciliation rows: {error}"))?;

        // Same Message-ID and metadata in a wrong folder.
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

        // A probable match is permitted only when exactly one unmatched
        // message exists on each side of a mapped-folder/date/size bucket.
        // Aggregate and pair those buckets inside SQLite in one set operation.
        let probable_rows = connection
            .execute(
                "WITH source_buckets AS (
                    SELECT match_mailbox,date_key,size_bytes,COUNT(*) AS n
                    FROM staged_messages s
                    WHERE side=0 AND date_key IS NOT NULL AND size_bytes IS NOT NULL
                      AND NOT EXISTS (
                          SELECT 1 FROM staged_matched m
                          WHERE m.side=s.side AND m.mailbox=s.mailbox
                            AND m.uidvalidity=s.uidvalidity AND m.uid=s.uid
                      )
                    GROUP BY match_mailbox,date_key,size_bytes
                    HAVING COUNT(*)=1
                 ), destination_buckets AS (
                    SELECT match_mailbox,date_key,size_bytes,COUNT(*) AS n
                    FROM staged_messages d
                    WHERE side=1 AND date_key IS NOT NULL AND size_bytes IS NOT NULL
                      AND NOT EXISTS (
                          SELECT 1 FROM staged_matched m
                          WHERE m.side=d.side AND m.mailbox=d.mailbox
                            AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid
                      )
                    GROUP BY match_mailbox,date_key,size_bytes
                    HAVING COUNT(*)=1
                 ), pairs AS (
                    SELECT s.mailbox AS source_mailbox,
                           s.uidvalidity AS source_uidvalidity,
                           s.uid AS source_uid,
                           d.mailbox AS destination_mailbox,
                           d.uidvalidity AS destination_uidvalidity,
                           d.uid AS destination_uid
                    FROM source_buckets sb
                    JOIN destination_buckets db USING(match_mailbox,date_key,size_bytes)
                    JOIN staged_messages s
                      ON s.side=0 AND s.match_mailbox=sb.match_mailbox
                     AND s.date_key=sb.date_key AND s.size_bytes=sb.size_bytes
                     AND NOT EXISTS (
                         SELECT 1 FROM staged_matched m
                         WHERE m.side=s.side AND m.mailbox=s.mailbox
                           AND m.uidvalidity=s.uidvalidity AND m.uid=s.uid
                     )
                    JOIN staged_messages d
                      ON d.side=1 AND d.match_mailbox=db.match_mailbox
                     AND d.date_key=db.date_key AND d.size_bytes=db.size_bytes
                     AND NOT EXISTS (
                         SELECT 1 FROM staged_matched m
                         WHERE m.side=d.side AND m.mailbox=d.mailbox
                           AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid
                     )
                 )
                 INSERT INTO staged_matched(side,mailbox,uidvalidity,uid)
                 SELECT 0,source_mailbox,source_uidvalidity,source_uid FROM pairs
                 UNION ALL
                 SELECT 1,destination_mailbox,destination_uidvalidity,destination_uid FROM pairs",
                [],
            )
            .map_err(|error| format!("could not reconcile unique staged fingerprints: {error}"))?;
        let probable_matches = u64::try_from(probable_rows / 2)
            .map_err(|_| "probable staged match count exceeded SQLite range".to_owned())?;

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
        connection.execute_batch("CREATE TABLE staged_duplicate_ids AS SELECT d.message_id FROM staged_messages d WHERE d.side=1 AND d.message_id IS NOT NULL GROUP BY d.message_id HAVING COUNT(*) > (SELECT COUNT(*) FROM staged_messages s WHERE s.side=0 AND s.message_id=d.message_id) AND (SELECT COUNT(*) FROM staged_messages s WHERE s.side=0 AND s.message_id=d.message_id) > 0; CREATE INDEX staged_duplicate_ids_message_id ON staged_duplicate_ids(message_id);").map_err(|error| error.to_string())?;
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
                        .prepare_cached(
                            "SELECT EXISTS(SELECT 1 FROM staged_duplicate_ids WHERE message_id=?1)",
                        )
                        .and_then(|mut statement| {
                            statement.query_row([id], |row| row.get::<_, bool>(0))
                        })
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
        transaction
            .commit()
            .map_err(|error| format!("could not finish staged reconciliation: {error}"))?;
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
    // Cached: reconciliation runs these few statements once per message.
    connection
        .prepare_cached(sql)
        .and_then(|mut statement| statement.query_row(parameters, staged_message_from_row))
        .optional()
        .map_err(|error| format!("could not query staged reconciliation candidate: {error}"))
}

fn mark_stage_matched(
    connection: &rusqlite::Connection,
    side: StagedMessageSide,
    message: &StagedMessage,
) -> Result<(), String> {
    let uidvalidity = message
        .key
        .uidvalidity
        .map(|value| {
            i64::try_from(value).map_err(|_| "staged UIDVALIDITY exceeds SQLite range".to_owned())
        })
        .transpose()?
        .unwrap_or(-1);
    connection
        .prepare_cached(
            "INSERT INTO staged_matched(side,mailbox,uidvalidity,uid) VALUES(?1,?2,?3,?4)",
        )
        .and_then(|mut statement| {
            statement.execute(params![
                side.as_i64(),
                message.key.mailbox.as_ref(),
                uidvalidity,
                message.key.uid,
            ])
        })
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
    // Counters must be derived from, and never exceed, the partition they
    // summarize; a message can contribute to at most one of them.
    let count = |keys: &HashSet<&MailboxMessageKey>| keys.len() as u64;
    let expected = [
        (
            "total_source",
            summary.total_source,
            source_messages.len() as u64,
        ),
        (
            "total_destination",
            summary.total_destination,
            dest_messages.len() as u64,
        ),
        (
            "metadata_matches",
            summary.metadata_matches,
            count(&membership.matched_source),
        ),
        (
            "matched destination",
            summary.metadata_matches,
            count(&membership.matched_destination),
        ),
        (
            "probable_matches",
            summary.probable_matches,
            count(&membership.probable_source),
        ),
        (
            "probable destination",
            summary.probable_matches,
            count(&membership.probable_destination),
        ),
        (
            "missing_count",
            summary.missing_count,
            count(&membership.missing_source),
        ),
        (
            "extra_count",
            summary.extra_count,
            count(&membership.extra_destination),
        ),
        (
            "duplicated_count",
            summary.duplicated_count,
            count(&membership.duplicated_destination),
        ),
        (
            "changed_count",
            summary.changed_count,
            count(&membership.changed_source),
        ),
        (
            "changed destination",
            summary.changed_count,
            count(&membership.changed_destination),
        ),
    ];
    for (name, reported, derived) in expected {
        if reported != derived {
            return Err(format!(
                "verification summary {name} is {reported} but its classification holds {derived}"
            ));
        }
    }
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
#[path = "message_verification/tests.rs"]
mod tests;
