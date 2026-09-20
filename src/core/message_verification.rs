use std::collections::{HashMap, HashSet};

use super::message_extraction::ExtractedMessage;

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
    pub source_uid: Option<String>,
    pub dest_uid: Option<String>,
    pub source_message_id: Option<String>,
    pub dest_message_id: Option<String>,
    pub source_size_bytes: Option<u64>,
    pub dest_size_bytes: Option<u64>,
    pub source_date: Option<String>,
    pub dest_date: Option<String>,
    pub subject: Option<String>,
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
        source_messages: &HashMap<String, ExtractedMessage>,
        dest_messages: &HashMap<String, ExtractedMessage>,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        let mut mismatches = Vec::new();
        let mut unmatched_source: HashSet<String> = source_messages.keys().cloned().collect();
        let mut unmatched_dest: HashSet<String> = dest_messages.keys().cloned().collect();

        let source_by_message_id = index_by_message_id(source_messages);
        let dest_by_message_id = index_by_message_id(dest_messages);

        // Message-ID remains the strongest portable identity. Match unique
        // IDs even when date/size differ so a legitimate metadata rewrite is
        // reported as changed rather than as missing + extra.
        let mut message_ids = source_by_message_id
            .keys()
            .filter(|id| dest_by_message_id.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        message_ids.sort();
        for message_id in message_ids {
            let source_uids = &source_by_message_id[&message_id];
            let dest_uids = &dest_by_message_id[&message_id];
            if source_uids.len() == 1 && !dest_uids.is_empty() {
                let source_uid = &source_uids[0];
                let dest_uid = &dest_uids[0];
                let source_msg = &source_messages[source_uid];
                let dest_msg = &dest_messages[dest_uid];
                unmatched_source.remove(source_uid);
                unmatched_dest.remove(dest_uid);
                if !same_metadata(source_msg, dest_msg) {
                    mismatches.push(make_mismatch(
                        job_id,
                        run_id,
                        MismatchType::MessageIdOnly,
                        Some(source_uid),
                        Some(dest_uid),
                        Some(source_msg),
                        Some(dest_msg),
                    ));
                }
            }
        }

        // Internal date + size is a useful fallback only when unique on both
        // sides. Ambiguous fingerprints are left unresolved rather than
        // silently pairing unrelated messages.
        let source_by_fingerprint = index_by_fingerprint(source_messages, &unmatched_source);
        let dest_by_fingerprint = index_by_fingerprint(dest_messages, &unmatched_dest);
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
                unmatched_source.remove(&source_uids[0]);
                unmatched_dest.remove(&dest_uids[0]);
            }
        }

        // Duplicate Message-IDs are observable even with rewritten UIDs. Do
        // not report the additional copies as generic extras.
        let mut duplicate_dest_uids = dest_by_message_id
            .values()
            .filter(|uids| uids.len() > 1)
            .flat_map(|uids| uids.iter())
            .filter(|uid| unmatched_dest.contains(*uid))
            .cloned()
            .collect::<Vec<_>>();
        duplicate_dest_uids.sort();
        for dest_uid in duplicate_dest_uids {
            let dest_msg = &dest_messages[&dest_uid];
            unmatched_dest.remove(&dest_uid);
            mismatches.push(make_mismatch(
                job_id,
                run_id,
                MismatchType::Duplicated,
                None,
                Some(&dest_uid),
                None,
                Some(dest_msg),
            ));
        }

        for source_uid in sorted_keys(&unmatched_source) {
            let source_msg = &source_messages[&source_uid];
            mismatches.push(make_mismatch(
                job_id,
                run_id,
                MismatchType::Missing,
                Some(&source_uid),
                None,
                Some(source_msg),
                None,
            ));
        }
        for dest_uid in sorted_keys(&unmatched_dest) {
            let dest_msg = &dest_messages[&dest_uid];
            mismatches.push(make_mismatch(
                job_id,
                run_id,
                MismatchType::Extra,
                None,
                Some(&dest_uid),
                None,
                Some(dest_msg),
            ));
        }

        // Generate summary
        let summary = VerificationSummary {
            total_source: source_messages.len() as u64,
            total_destination: dest_messages.len() as u64,
            exact_matches: source_messages.len() as u64
                - mismatches.iter().filter(|m| m.source_uid.is_some()).count() as u64,
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
                .filter(|m| matches!(m.mismatch_type, MismatchType::MessageIdOnly))
                .count() as u64,
        };

        Ok((mismatches, summary))
    }
}

fn index_by_message_id(
    messages: &HashMap<String, ExtractedMessage>,
) -> HashMap<String, Vec<String>> {
    let mut index = HashMap::new();
    for (uid, message) in messages {
        if let Some(message_id) = message
            .message_id
            .as_deref()
            .map(str::trim)
            .filter(|message_id| !message_id.is_empty())
        {
            index
                .entry(message_id.to_owned())
                .or_insert_with(Vec::new)
                .push(uid.clone());
        }
    }
    index.values_mut().for_each(|uids| uids.sort());
    index
}

fn index_by_fingerprint(
    messages: &HashMap<String, ExtractedMessage>,
    eligible: &HashSet<String>,
) -> HashMap<String, Vec<String>> {
    let mut index = HashMap::new();
    for uid in eligible {
        let message = &messages[uid];
        let (Some(date), Some(size)) = (&message.internal_date, message.size_bytes) else {
            continue;
        };
        index
            .entry(format!("{date}\0{size}"))
            .or_insert_with(Vec::new)
            .push(uid.clone());
    }
    index.values_mut().for_each(|uids| uids.sort());
    index
}

fn sorted_keys(keys: &HashSet<String>) -> Vec<String> {
    let mut sorted = keys.iter().cloned().collect::<Vec<_>>();
    sorted.sort();
    sorted
}

fn same_metadata(source: &ExtractedMessage, destination: &ExtractedMessage) -> bool {
    source.size_bytes == destination.size_bytes && source.internal_date == destination.internal_date
}

fn make_mismatch(
    job_id: &str,
    run_id: &str,
    mismatch_type: MismatchType,
    source_uid: Option<&String>,
    dest_uid: Option<&String>,
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
        source_uid: source_uid.cloned(),
        dest_uid: dest_uid.cloned(),
        source_message_id: source.and_then(|message| message.message_id.clone()),
        dest_message_id: destination.and_then(|message| message.message_id.clone()),
        source_size_bytes: source.and_then(|message| message.size_bytes),
        dest_size_bytes: destination.and_then(|message| message.size_bytes),
        source_date: source.and_then(|message| message.internal_date.clone()),
        dest_date: destination.and_then(|message| message.internal_date.clone()),
        subject: None,
    }
}

/// Summary of verification results.
#[derive(Debug, Clone)]
pub struct VerificationSummary {
    pub total_source: u64,
    pub total_destination: u64,
    pub exact_matches: u64,
    pub missing_count: u64,
    pub extra_count: u64,
    pub duplicated_count: u64,
    pub changed_count: u64,
}

impl VerificationSummary {
    pub fn is_perfect_match(&self) -> bool {
        self.missing_count == 0
            && self.extra_count == 0
            && self.duplicated_count == 0
            && self.changed_count == 0
            && self.total_source == self.total_destination
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_missing_messages() {
        let mut source = HashMap::new();
        source.insert(
            "1".to_string(),
            ExtractedMessage {
                message_id: Some("<a@example.com>".to_string()),
                uid: Some("1".to_string()),
                size_bytes: Some(1000),
                internal_date: Some("2024-01-01".to_string()),
            },
        );
        source.insert(
            "2".to_string(),
            ExtractedMessage {
                message_id: Some("<b@example.com>".to_string()),
                uid: Some("2".to_string()),
                size_bytes: Some(2000),
                internal_date: Some("2024-01-02".to_string()),
            },
        );

        let mut dest = HashMap::new();
        dest.insert(
            "1".to_string(),
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
        assert_eq!(summary.exact_matches, 1);

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
            "1".to_string(),
            ExtractedMessage {
                message_id: Some("<a@example.com>".to_string()),
                uid: Some("1".to_string()),
                size_bytes: Some(1000),
                internal_date: Some("2024-01-01".to_string()),
            },
        );

        let mut dest = HashMap::new();
        dest.insert(
            "1".to_string(),
            ExtractedMessage {
                message_id: Some("<a@example.com>".to_string()),
                uid: Some("1".to_string()),
                size_bytes: Some(1000),
                internal_date: Some("2024-01-01".to_string()),
            },
        );
        dest.insert(
            "2".to_string(),
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
            "1".to_string(),
            ExtractedMessage {
                message_id: Some("<a@example.com>".to_string()),
                uid: Some("1".to_string()),
                size_bytes: Some(1000),
                internal_date: Some("2024-01-01".to_string()),
            },
        );

        let mut dest = HashMap::new();
        dest.insert(
            "1".to_string(),
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
        assert!(!summary.is_perfect_match());
    }

    #[test]
    fn perfect_match_when_identical() {
        let mut source = HashMap::new();
        source.insert(
            "1".to_string(),
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

        assert!(summary.is_perfect_match());
        assert_eq!(summary.exact_matches, 1);
    }

    #[test]
    fn destination_uid_rewrite_is_not_reported_as_missing_and_extra() {
        let source = HashMap::from([(
            "17".to_string(),
            ExtractedMessage {
                message_id: Some("<stable@example.com>".to_string()),
                uid: Some("17".to_string()),
                size_bytes: Some(4096),
                internal_date: Some("2024-01-01T00:00:00Z".to_string()),
            },
        )]);
        let destination = HashMap::from([(
            "904".to_string(),
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
        assert!(summary.is_perfect_match());
        assert_eq!(summary.exact_matches, 1);
    }

    #[test]
    fn date_and_size_fallback_is_also_uid_independent() {
        let source = HashMap::from([(
            "1".to_string(),
            ExtractedMessage {
                message_id: None,
                uid: Some("1".to_string()),
                size_bytes: Some(512),
                internal_date: Some("2024-01-01T00:00:00Z".to_string()),
            },
        )]);
        let destination = HashMap::from([(
            "88".to_string(),
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
        assert!(summary.is_perfect_match());
    }

    #[test]
    fn ambiguous_fingerprint_fails_closed_instead_of_pairing_by_uid() {
        let message = |uid: &str| ExtractedMessage {
            message_id: None,
            uid: Some(uid.to_string()),
            size_bytes: Some(512),
            internal_date: Some("2024-01-01T00:00:00Z".to_string()),
        };
        let source = HashMap::from([
            ("1".to_string(), message("1")),
            ("2".to_string(), message("2")),
        ]);
        let destination = HashMap::from([
            ("88".to_string(), message("88")),
            ("89".to_string(), message("89")),
        ]);

        let (mismatches, summary) =
            MessageVerification::detect_mismatches("job1", "run-ambiguous", &source, &destination)
                .unwrap();

        assert_eq!(summary.missing_count, 2);
        assert_eq!(summary.extra_count, 2);
        assert!(!summary.is_perfect_match());
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
        let source = HashMap::from([("1".to_string(), message("1"))]);
        let destination = HashMap::from([
            ("88".to_string(), message("88")),
            ("89".to_string(), message("89")),
        ]);

        let (mismatches, summary) =
            MessageVerification::detect_mismatches("job1", "run-duplicate", &source, &destination)
                .unwrap();

        assert_eq!(summary.exact_matches, 1);
        assert_eq!(summary.extra_count, 0);
        assert!(
            mismatches
                .iter()
                .any(|m| m.mismatch_type == MismatchType::Duplicated)
        );
    }
}
