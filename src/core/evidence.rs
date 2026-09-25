use serde::{Deserialize, Serialize};

use super::{AttentionReason, MailboxJob, Project, RunSummary};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxEvidence {
    /// Explicitly records which adapter produced this evidence. This is not
    /// inferred from the result counts because a metadata mismatch is not a
    /// body hash.
    pub verification_method: VerificationMethod,
    pub source_messages: u64,
    pub destination_messages: u64,
    pub source_bytes: u64,
    pub destination_bytes: u64,
    /// A literal unresolved-message count when the verifier provides one.
    /// `None` means the verifier could not establish a count; it is never a
    /// sentinel for an unknown or incomplete result.
    pub unmatched_messages: Option<u64>,
    pub failed_messages: u64,
    pub source_folders: u64,
    pub destination_folders: u64,
    /// True only when the engine supplied a stronger engine-confirmed summary.
    pub authoritative: bool,
    /// Message-level verification: count of messages present in source but absent in destination.
    pub missing_messages: u64,
    /// Message-level verification: count of messages present in destination but absent in source.
    pub extra_messages: u64,
    /// Message-level verification: count of messages with the same portable
    /// identity but different available content/metadata.
    pub modified_messages: u64,
}

/// Verification adapter that produced the persisted evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationMethod {
    AggregateEngine,
    MetadataReconciliation,
    BodyHash,
    NativeDovecot,
}

impl VerificationMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AggregateEngine => "aggregate_engine",
            Self::MetadataReconciliation => "metadata_reconciliation",
            Self::BodyHash => "body_hash",
            Self::NativeDovecot => "native_dovecot",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "aggregate_engine" => Self::AggregateEngine,
            "metadata_reconciliation" => Self::MetadataReconciliation,
            "body_hash" => Self::BodyHash,
            "native_dovecot" => Self::NativeDovecot,
            _ => return None,
        })
    }
}

/// Canonical persisted/reporting outcome. The ordering in
/// `MailboxEvidence::verification_outcome` is the single severity policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationOutcome {
    ExactMetadataMatch,
    ProbableMatch,
    Ambiguous,
    Missing,
    Changed,
    Unexpected,
    Incomplete,
    Failed,
}

impl VerificationOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactMetadataMatch => "exact_metadata_match",
            Self::ProbableMatch => "probable_match",
            Self::Ambiguous => "ambiguous",
            Self::Missing => "missing",
            Self::Changed => "changed",
            Self::Unexpected => "unexpected",
            Self::Incomplete => "incomplete",
            Self::Failed => "failed",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::ExactMetadataMatch => "Exact metadata match — message bodies not compared",
            Self::ProbableMatch => "Probable metadata match — message bodies not compared",
            Self::Ambiguous => "Ambiguous metadata result — message bodies not compared",
            Self::Missing => "Missing messages detected",
            Self::Changed => "Changed messages detected",
            Self::Unexpected => "Unexpected messages detected",
            Self::Incomplete => "Verification evidence incomplete",
            Self::Failed => "Verification failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationAcceptance {
    pub job_id: String,
    pub run_id: String,
    pub operator: String,
    pub reason: String,
    pub accepted_at: String,
}

/// Read model used by forensic and customer proof exports. It deliberately
/// gathers related rows in a small fixed number of queries so large projects
/// do not turn report generation into a per-mailbox/per-run N+1 workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportMailboxSnapshot {
    pub job: MailboxJob,
    pub attention_reason: Option<AttentionReason>,
    pub acceptance: Option<VerificationAcceptance>,
    pub evidence: Option<(String, MailboxEvidence, Option<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRunSnapshot {
    pub run: RunSummary,
    pub engine_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectReportSnapshot {
    pub project: Project,
    pub mailboxes: Vec<ReportMailboxSnapshot>,
    pub runs: Vec<ReportRunSnapshot>,
}

/// The strongest claim supported by the current verifier adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceScope {
    EngineConfirmed,
    AggregateReconciled,
}

impl EvidenceScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::EngineConfirmed => "engine-confirmed",
            Self::AggregateReconciled => "aggregate-reconciled",
        }
    }
}

impl MailboxEvidence {
    fn aggregate_totals_match(&self) -> bool {
        self.source_messages == self.destination_messages
            && self.source_bytes == self.destination_bytes
            && self.source_folders == self.destination_folders
    }

    fn has_message_level_mismatch(&self) -> bool {
        self.missing_messages > 0 || self.extra_messages > 0 || self.modified_messages > 0
    }

    fn has_verification_exception(&self) -> bool {
        self.failed_messages > 0
            || self.unmatched_messages.is_some_and(|count| count > 0)
            || self.has_message_level_mismatch()
    }

    pub fn evidence_scope(&self) -> EvidenceScope {
        if self.authoritative {
            EvidenceScope::EngineConfirmed
        } else {
            EvidenceScope::AggregateReconciled
        }
    }

    pub fn verification_method(&self) -> VerificationMethod {
        self.verification_method
    }

    /// Canonical classification consumed by all report and UI layers.
    pub fn verification_outcome(&self) -> VerificationOutcome {
        if self.failed_messages > 0 {
            VerificationOutcome::Failed
        } else if self.unmatched_messages.is_none() {
            VerificationOutcome::Incomplete
        } else if self.missing_messages > 0 {
            VerificationOutcome::Missing
        } else if self.modified_messages > 0 {
            VerificationOutcome::Changed
        } else if self.extra_messages > 0 {
            VerificationOutcome::Unexpected
        } else if self.unmatched_messages.is_some_and(|count| count > 0) {
            VerificationOutcome::Ambiguous
        } else if self.aggregate_totals_match() {
            VerificationOutcome::ExactMetadataMatch
        } else {
            VerificationOutcome::Incomplete
        }
    }

    pub fn unresolved_count(&self) -> Option<u64> {
        self.unmatched_messages
    }

    pub fn missing_count(&self) -> u64 {
        self.missing_messages
    }

    pub fn extra_count(&self) -> u64 {
        self.extra_messages
    }

    pub fn modified_count(&self) -> u64 {
        self.modified_messages
    }

    pub fn probable_count(&self) -> u64 {
        0
    }

    pub fn metadata_matched_count(&self) -> u64 {
        self.source_messages
            .saturating_sub(self.unmatched_messages.unwrap_or(0))
    }

    pub fn evidence_level(&self) -> &'static str {
        if self.failed_messages > 0 || self.unmatched_messages.is_none_or(|count| count > 0) {
            return "Incomplete evidence";
        }
        if self.has_message_level_mismatch() {
            return "Message-level mismatch";
        }
        let exact = self.aggregate_totals_match();
        if self.evidence_scope() == EvidenceScope::EngineConfirmed && exact {
            "Engine-confirmed exact match — not message-body proof"
        } else if exact {
            "Aggregate match — not message-body proof"
        } else {
            "Aggregate mismatch"
        }
    }

    pub fn verification_reason(&self) -> Option<&'static str> {
        match self.verification_outcome() {
            VerificationOutcome::Failed => Some("engine reported migration errors"),
            VerificationOutcome::Incomplete => Some("verification evidence is incomplete"),
            VerificationOutcome::Missing => Some("message-level reconciliation found missing messages"),
            VerificationOutcome::Changed => Some("message-level reconciliation found changed messages"),
            VerificationOutcome::Unexpected => Some("message-level reconciliation found unexpected messages"),
            VerificationOutcome::ProbableMatch => Some("message identity remains probable rather than exact"),
            VerificationOutcome::Ambiguous => Some("message identity could not be resolved unambiguously"),
            VerificationOutcome::ExactMetadataMatch => None,
        }
    }

    /// Named assurance level exposed to operators and proof consumers. Live
    /// adapters currently provide aggregate reconciliation; message identity
    /// sampling and full reconciliation are deliberately not inferred.
    pub fn verification_level(&self) -> &'static str {
        // Compatibility wording for older callers. New reports and UI use
        // `verification_outcome` directly so this legacy aggregate label
        // cannot affect customer semantics.
        if self.unmatched_messages.is_none() || self.failed_messages > 0 {
            "Level 0 — Process completed, verification incomplete"
        } else {
            "Level 2 — Aggregate reconciliation — not message-body proof"
        }
    }

    #[allow(dead_code)]
    pub fn confidence_percent(&self) -> u8 {
        if self.has_verification_exception() {
            return 0;
        }
        let exact = self.aggregate_totals_match();
        if !self.authoritative {
            return if exact { 85 } else { 0 };
        }
        if exact { 100 } else { 85 }
    }

    pub fn is_exact_match(&self) -> bool {
        self.aggregate_totals_match() && !self.has_verification_exception()
    }
}

#[cfg(test)]
mod tests {
    use super::{MailboxEvidence, VerificationMethod, VerificationOutcome};

    fn missing_evidence() -> MailboxEvidence {
        MailboxEvidence {
            verification_method: VerificationMethod::MetadataReconciliation,
            source_messages: 10,
            destination_messages: 9,
            source_bytes: 100,
            destination_bytes: 90,
            unmatched_messages: Some(1),
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: false,
            missing_messages: 1,
            extra_messages: 0,
            modified_messages: 0,
        }
    }

    #[test]
    fn missing_message_classification_is_canonical() {
        let evidence = missing_evidence();
        assert_eq!(
            evidence.verification_outcome(),
            VerificationOutcome::Missing
        );
        assert_eq!(evidence.verification_outcome().as_str(), "missing");
        assert_eq!(evidence.unresolved_count(), Some(1));
        assert_eq!(evidence.missing_count(), 1);
        assert_eq!(evidence.extra_count(), 0);
        assert_eq!(evidence.modified_count(), 0);
        assert_eq!(evidence.verification_reason(), Some("message-level reconciliation found missing messages"));
    }

    #[test]
    fn metadata_mismatch_does_not_claim_body_hash_verification() {
        let mut evidence = missing_evidence();
        evidence.modified_messages = 1;

        assert_eq!(
            evidence.verification_method(),
            VerificationMethod::MetadataReconciliation
        );
    }
}
