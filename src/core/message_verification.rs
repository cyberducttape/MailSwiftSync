use std::collections::{HashMap, HashSet, VecDeque};

use super::message_extraction::{ExtractedMessage, ExtractedMessages, MailboxMessageKey};

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
}

/// Mismatch record to persist in the database.
#[derive(Debug, Clone)]
pub struct MessageMismatch {
    pub id: String,
    pub job_id: String,
    pub run_id: String,
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
}

/// Core message verification engine.
pub struct MessageVerification;

impl MessageVerification {
    /// Detect mismatches between source and destination message sets.
    ///
    /// IMAP UIDs are mailbox-local and are deliberately never used as the
    /// identity across the two inputs. Messages are matched first by a unique
    /// RFC Message-ID, then by a unique internal-date/size fingerprint. A
    /// UID is retained only as diagnostic context in the result.
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

    /// Detect mismatches while enforcing the expected source-to-destination
    /// folder mapping. An absent mapping entry means the source folder is
    /// expected to retain its name. Provider-specific label semantics belong
    /// in the mapping supplied by the caller, not in this generic verifier.
    pub fn detect_mismatches_with_folder_mapping(
        job_id: &str,
        run_id: &str,
        source_messages: &ExtractedMessages,
        dest_messages: &ExtractedMessages,
        folder_mapping: &HashMap<String, String>,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        let mut mismatches = Vec::new();
        let mut metadata_matches = 0_u64;
        let mut probable_matches = 0_u64;
        // UIDs are mailbox-local diagnostic metadata. Borrow them from the
        // input maps instead of cloning every UID into a second set.
        let mut unmatched_source: HashSet<&MailboxMessageKey> = source_messages.keys().collect();
        let mut unmatched_dest: HashSet<&MailboxMessageKey> = dest_messages.keys().collect();

        let source_by_message_id = index_by_message_id(source_messages);
        let dest_by_message_id = index_by_message_id(dest_messages);

        // Message-ID remains the strongest portable identity. Match unique
        // IDs even when date/size differ so a legitimate metadata rewrite is
        // reported as changed rather than as missing + extra.
        let mut message_ids = source_by_message_id
            .keys()
            .filter(|id| dest_by_message_id.contains_key(*id))
            .copied()
            .collect::<Vec<_>>();
        message_ids.sort_unstable();
        for message_id in message_ids {
            let source_uids = &source_by_message_id[&message_id];
            let dest_uids = &dest_by_message_id[&message_id];
            // A repeated Message-ID identifies a group, not an ordering.
            // Reconcile the group's metadata multiset first so UID/mailbox
            // ordering cannot turn preserved duplicates into false changes.
            // The expected destination folder is part of this key; a matching
            // message in another folder must never count as a match.
            let mut destination_by_metadata =
                HashMap::<(String, MetadataFingerprint<'_>), VecDeque<&MailboxMessageKey>>::new();
            for dest_key in dest_uids {
                if let Some(fingerprint) = metadata_fingerprint(&dest_messages[*dest_key]) {
                    destination_by_metadata
                        .entry((dest_key.mailbox.clone(), fingerprint))
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
                    unmatched_source.remove(source_key);
                    unmatched_dest.remove(dest_key);
                    metadata_matches += 1;
                }
            }

            // The remaining common multiplicity shares only Message-ID.
            // Pair it deterministically for diagnostics and classify it as
            // changed; source or destination excess remains for the explicit
            // missing/duplicate passes below.
            let remaining_source = source_uids
                .iter()
                .copied()
                .filter(|key| unmatched_source.contains(key))
                .collect::<Vec<_>>();
            let remaining_dest = dest_uids
                .iter()
                .copied()
                .filter(|key| {
                    source_uids.iter().any(|source_key| {
                        unmatched_source.contains(source_key)
                            && expected_destination_folder(source_key, folder_mapping)
                                == key.mailbox
                    })
                })
                .filter(|key| unmatched_dest.contains(key))
                .collect::<Vec<_>>();
            for (source_key, dest_key) in remaining_source.iter().zip(remaining_dest.iter()) {
                let source_msg = &source_messages[source_key];
                let dest_msg = &dest_messages[dest_key];
                unmatched_source.remove(source_key);
                unmatched_dest.remove(dest_key);
                mismatches.push(make_mismatch(
                    job_id,
                    run_id,
                    MismatchType::MessageIdOnly,
                    Some(source_key),
                    Some(dest_key),
                    Some(source_msg),
                    Some(dest_msg),
                ));
            }
        }

        // A matching identity in an unexpected folder is a placement error,
        // not a successful cross-folder match. Prefer metadata equality when
        // pairing duplicate Message-IDs, then retain the full folder context
        // in the mismatch record for operator remediation.
        let mut wrong_folder_pairs = Vec::new();
        for (message_id, source_uids) in &source_by_message_id {
            let Some(dest_uids) = dest_by_message_id.get(message_id) else {
                continue;
            };
            for source_key in source_uids {
                if !unmatched_source.contains(source_key) {
                    continue;
                }
                let expected_folder = expected_destination_folder(source_key, folder_mapping);
                let destination = dest_uids
                    .iter()
                    .filter(|dest_key| {
                        unmatched_dest.contains(*dest_key) && dest_key.mailbox != expected_folder
                    })
                    .find(|dest_key| {
                        metadata_fingerprint(&source_messages[source_key])
                            == metadata_fingerprint(&dest_messages[dest_key])
                    })
                    .or_else(|| {
                        dest_uids.iter().find(|dest_key| {
                            unmatched_dest.contains(*dest_key)
                                && dest_key.mailbox != expected_folder
                        })
                    });
                if let Some(dest_key) = destination {
                    wrong_folder_pairs.push((*source_key, *dest_key));
                }
            }
        }
        for (source_key, dest_key) in wrong_folder_pairs {
            if !unmatched_source.remove(source_key) || !unmatched_dest.remove(dest_key) {
                continue;
            }
            mismatches.push(make_mismatch(
                job_id,
                run_id,
                MismatchType::PresentWrongFolder,
                Some(source_key),
                Some(dest_key),
                Some(&source_messages[source_key]),
                Some(&dest_messages[dest_key]),
            ));
        }

        // Internal date + size is a useful fallback only when unique on both
        // sides. Ambiguous fingerprints are left unresolved rather than
        // silently pairing unrelated messages.
        let source_by_fingerprint =
            index_by_fingerprint(source_messages, &unmatched_source, folder_mapping, true);
        let dest_by_fingerprint =
            index_by_fingerprint(dest_messages, &unmatched_dest, folder_mapping, false);
        let mut fingerprints = source_by_fingerprint
            .keys()
            .filter(|fingerprint| dest_by_fingerprint.contains_key(*fingerprint))
            .cloned()
            .collect::<Vec<_>>();
        fingerprints.sort();
        for fingerprint in fingerprints {
            let source_uids = &source_by_fingerprint[&fingerprint];
            let dest_uids = &dest_by_fingerprint[&fingerprint];
            if source_uids.len() == 1 && dest_uids.len() == 1 {
                unmatched_source.remove(source_uids[0]);
                unmatched_dest.remove(dest_uids[0]);
                // Date + size is only a candidate identity. It is useful for
                // reconciliation, but it is not proof that the messages are
                // the same and must never contribute to metadata_matches.
                probable_matches += 1;
            }
        }

        // Report only destination occurrences beyond the source multiplicity.
        let mut duplicate_dest_uids = dest_by_message_id
            .iter()
            .filter(|(message_id, dest_uids)| {
                source_by_message_id
                    .get(*message_id)
                    .is_some_and(|source_uids| dest_uids.len() > source_uids.len())
            })
            .flat_map(|(_, uids)| uids.iter())
            .filter(|uid| unmatched_dest.contains(*uid))
            .copied()
            .collect::<Vec<_>>();
        duplicate_dest_uids.sort();
        for dest_uid in duplicate_dest_uids {
            let dest_msg = &dest_messages[dest_uid];
            unmatched_dest.remove(dest_uid);
            mismatches.push(make_mismatch(
                job_id,
                run_id,
                MismatchType::Duplicated,
                None,
                Some(dest_uid),
                None,
                Some(dest_msg),
            ));
        }

        for source_uid in sorted_keys(&unmatched_source) {
            let source_msg = &source_messages[source_uid];
            mismatches.push(make_mismatch(
                job_id,
                run_id,
                MismatchType::Missing,
                Some(source_uid),
                None,
                Some(source_msg),
                None,
            ));
        }
        for dest_uid in sorted_keys(&unmatched_dest) {
            let dest_msg = &dest_messages[dest_uid];
            mismatches.push(make_mismatch(
                job_id,
                run_id,
                MismatchType::Extra,
                None,
                Some(dest_uid),
                None,
                Some(dest_msg),
            ));
        }

        // Generate summary
        let summary = VerificationSummary {
            total_source: source_messages.len() as u64,
            total_destination: dest_messages.len() as u64,
            metadata_matches,
            probable_matches,
            missing_count: mismatches
                .iter()
                .filter(|m| m.mismatch_type == MismatchType::Missing)
                .count() as u64,
            extra_count: mismatches
                .iter()
                .filter(|m| m.mismatch_type == MismatchType::Extra)
                .count() as u64,
            duplicated_count: mismatches
                .iter()
                .filter(|m| m.mismatch_type == MismatchType::Duplicated)
                .count() as u64,
            changed_count: mismatches
                .iter()
                .filter(|m| {
                    matches!(
                        m.mismatch_type,
                        MismatchType::MessageIdOnly | MismatchType::PresentWrongFolder
                    )
                })
                .count() as u64,
        };

        Ok((mismatches, summary))
    }
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

fn index_by_fingerprint<'a>(
    messages: &'a ExtractedMessages,
    eligible: &HashSet<&'a MailboxMessageKey>,
    folder_mapping: &HashMap<String, String>,
    source_side: bool,
) -> HashMap<(String, MetadataFingerprint<'a>), Vec<&'a MailboxMessageKey>> {
    let mut index = HashMap::new();
    for uid in eligible {
        let message = &messages[*uid];
        let Some(fingerprint) = metadata_fingerprint(message) else {
            continue;
        };
        let folder = if source_side {
            expected_destination_folder(uid, folder_mapping)
        } else {
            uid.mailbox.clone()
        };
        index
            .entry((folder, fingerprint))
            .or_insert_with(Vec::new)
            .push(*uid);
    }
    index.values_mut().for_each(|uids| uids.sort());
    index
}

fn expected_destination_folder(
    source_key: &MailboxMessageKey,
    folder_mapping: &HashMap<String, String>,
) -> String {
    folder_mapping
        .get(&source_key.mailbox)
        .cloned()
        .unwrap_or_else(|| source_key.mailbox.clone())
}

/// Borrowed metadata key used only while reconciling one in-memory batch.
/// Keeping the date as a borrowed normalized string avoids constructing a
/// delimiter-based fingerprint for every message. A future SQLite-backed
/// verifier can store the normalized date and size as separate indexed
/// columns instead of materializing this map at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct MetadataFingerprint<'a> {
    internal_date: &'a str,
    size_bytes: u64,
}

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
        internal_date: message.internal_date.as_deref()?,
        size_bytes: message.size_bytes?,
    })
}

fn make_mismatch(
    job_id: &str,
    run_id: &str,
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
        job_id: job_id.to_owned(),
        run_id: run_id.to_owned(),
        mismatch_type,
        source_folder: source_key.map(|key| key.mailbox.clone()),
        destination_folder: dest_key.map(|key| key.mailbox.clone()),
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
    }
}

/// Summary of verification results.
#[derive(Debug, Clone)]
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
pub enum EvidenceLevel {
    MetadataMatched,
    StrongMetadataMatch,
    ProbableMatch,
    Ambiguous,
    Missing,
    Changed,
    Unexpected,
}

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
    /// Return whether all extracted records reconcile by metadata alone.
    /// This must not be presented as content verification.
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

    fn key(uid: &str) -> MailboxMessageKey {
        MailboxMessageKey::new("INBOX", uid)
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
}
