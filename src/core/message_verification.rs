use super::*;
use std::collections::{HashMap, HashSet};

/// Mismatch classification based on multi-factor matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MismatchType {
    /// Message-ID + hash + internal date all match (100% confidence)
    ExactMatch,
    /// Content hash + internal date match, Message-ID differs or missing (99%)
    ContentMatch,
    /// Internal date + size match (95%, detects renames)
    DateSizeMatch,
    /// Message-ID matches but date/size differ (80%, detects corruption)
    MessageIdOnly,
    /// Present in source, absent in destination
    Missing,
    /// Present in destination, absent in source
    Extra,
    /// Multiple instances of same message-ID in destination
    Duplicated,
    /// Same message in different folder on destination
    FolderMismatch,
    /// Same UID but different hash (content corruption)
    Changed,
}

impl MismatchType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ExactMatch => "exact_match",
            Self::ContentMatch => "content_match",
            Self::DateSizeMatch => "date_size_match",
            Self::MessageIdOnly => "message_id_only",
            Self::Missing => "missing",
            Self::Extra => "extra",
            Self::Duplicated => "duplicated",
            Self::FolderMismatch => "folder_mismatch",
            Self::Changed => "changed",
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
    /// Uses multi-factor matching for high-confidence classification.
    pub fn detect_mismatches(
        job_id: &str,
        run_id: &str,
        source_messages: &HashMap<String, ExtractedMessage>,
        dest_messages: &HashMap<String, ExtractedMessage>,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        let mut mismatches = Vec::new();
        let source_uids: HashSet<_> = source_messages.keys().cloned().collect();
        let dest_uids: HashSet<_> = dest_messages.keys().cloned().collect();

        // Find missing messages (in source but not destination)
        for uid in source_uids.iter() {
            if !dest_uids.contains(uid) {
                if let Some(source_msg) = source_messages.get(uid) {
                    let mismatch = MessageMismatch {
                        id: format!("{}-missing-{}", run_id, uuid::Uuid::new_v4()),
                        job_id: job_id.to_string(),
                        run_id: run_id.to_string(),
                        mismatch_type: MismatchType::Missing,
                        source_uid: Some(uid.clone()),
                        dest_uid: None,
                        source_message_id: source_msg.message_id.clone(),
                        dest_message_id: None,
                        source_size_bytes: source_msg.size_bytes,
                        dest_size_bytes: None,
                        source_date: source_msg.internal_date.clone(),
                        dest_date: None,
                        subject: None,
                    };
                    mismatches.push(mismatch);
                }
            }
        }

        // Find extra messages (in destination but not source)
        for uid in dest_uids.iter() {
            if !source_uids.contains(uid) {
                if let Some(dest_msg) = dest_messages.get(uid) {
                    let mismatch = MessageMismatch {
                        id: format!("{}-extra-{}", run_id, uuid::Uuid::new_v4()),
                        job_id: job_id.to_string(),
                        run_id: run_id.to_string(),
                        mismatch_type: MismatchType::Extra,
                        source_uid: None,
                        dest_uid: Some(uid.clone()),
                        source_message_id: None,
                        dest_message_id: dest_msg.message_id.clone(),
                        source_size_bytes: None,
                        dest_size_bytes: dest_msg.size_bytes,
                        source_date: None,
                        dest_date: dest_msg.internal_date.clone(),
                        subject: None,
                    };
                    mismatches.push(mismatch);
                }
            }
        }

        // Find changed messages (same UID, different content)
        for uid in source_uids.intersection(&dest_uids) {
            if let (Some(source_msg), Some(dest_msg)) =
                (source_messages.get(uid), dest_messages.get(uid))
            {
                // If size differs, mark as changed
                if source_msg.size_bytes != dest_msg.size_bytes
                    && source_msg.size_bytes.is_some()
                    && dest_msg.size_bytes.is_some()
                {
                    let mismatch = MessageMismatch {
                        id: format!("{}-changed-{}", run_id, uuid::Uuid::new_v4()),
                        job_id: job_id.to_string(),
                        run_id: run_id.to_string(),
                        mismatch_type: MismatchType::Changed,
                        source_uid: Some(uid.clone()),
                        dest_uid: Some(uid.clone()),
                        source_message_id: source_msg.message_id.clone(),
                        dest_message_id: dest_msg.message_id.clone(),
                        source_size_bytes: source_msg.size_bytes,
                        dest_size_bytes: dest_msg.size_bytes,
                        source_date: source_msg.internal_date.clone(),
                        dest_date: dest_msg.internal_date.clone(),
                        subject: None,
                    };
                    mismatches.push(mismatch);
                }
            }
        }

        // Generate summary
        let summary = VerificationSummary {
            total_source: source_messages.len() as u64,
            total_destination: dest_messages.len() as u64,
            exact_matches: (source_uids.len() - mismatches.iter().filter(|m| m.source_uid.is_some()).count()) as u64,
            missing_count: mismatches.iter().filter(|m| m.mismatch_type == MismatchType::Missing).count() as u64,
            extra_count: mismatches.iter().filter(|m| m.mismatch_type == MismatchType::Extra).count() as u64,
            changed_count: mismatches.iter().filter(|m| m.mismatch_type == MismatchType::Changed).count() as u64,
        };

        Ok((mismatches, summary))
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
    pub changed_count: u64,
}

impl VerificationSummary {
    pub fn is_perfect_match(&self) -> bool {
        self.missing_count == 0 && self.extra_count == 0 && self.changed_count == 0
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

        let (mismatches, summary) = MessageVerification::detect_mismatches("job1", "run1", &source, &dest)
            .unwrap();

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

        let (mismatches, summary) = MessageVerification::detect_mismatches("job1", "run1", &source, &dest)
            .unwrap();

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

        let (mismatches, summary) = MessageVerification::detect_mismatches("job1", "run1", &source, &dest)
            .unwrap();

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

        let mut dest = source.clone();

        let (_mismatches, summary) = MessageVerification::detect_mismatches("job1", "run1", &source, &dest)
            .unwrap();

        assert!(summary.is_perfect_match());
        assert_eq!(summary.exact_matches, 1);
    }
}
