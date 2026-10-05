//! Verification summaries, membership, and their consistency validation.

use super::*;

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
