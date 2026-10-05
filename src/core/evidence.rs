use serde::{Deserialize, Serialize};

use super::{AttentionReason, MailboxJob, Project, RunSummary};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Canonical durable verification record consumed by persistence, lifecycle
/// policy, reports, UI, and integrity signing. `MailboxEvidence` remains a
/// compatibility alias for older callers.
pub struct VerificationEvidence {
    /// Explicitly records which adapter produced this evidence. This is not
    /// inferred from the result counts because a metadata mismatch is not a
    /// body hash.
    pub verification_method: VerificationMethod,
    /// The verifier's authoritative classification. `None` is retained only
    /// for in-memory compatibility fixtures; durable writes always materialize
    /// the computed value.
    pub verification_outcome: Option<VerificationOutcome>,
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
    /// Metadata pairings that remain candidates rather than exact identities.
    pub probable_messages: u64,
}

pub type MailboxEvidence = VerificationEvidence;

/// Independent evidence dimensions.  These are intentionally not collapsed
/// into `VerificationOutcome`: an exact message reconciliation does not prove
/// that flags, ACLs, or provider-specific mailbox state survived.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceDimensionStatus {
    Pass,
    Warning,
    NotVerified,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationDimensions {
    pub message_presence: EvidenceDimensionStatus,
    pub body_integrity: EvidenceDimensionStatus,
    pub internal_dates: EvidenceDimensionStatus,
    pub system_flags: EvidenceDimensionStatus,
    pub custom_keywords: EvidenceDimensionStatus,
    pub folder_subscriptions: EvidenceDimensionStatus,
    pub folder_mapping: EvidenceDimensionStatus,
    pub acls: EvidenceDimensionStatus,
    pub quota_capacity: EvidenceDimensionStatus,
    pub special_use: EvidenceDimensionStatus,
    pub gmail_labels: EvidenceDimensionStatus,
}

#[allow(dead_code)]
impl VerificationEvidence {
    /// Return the dimensions established by this verifier.  Unsupported
    /// mailbox state is explicit so an exact message result cannot be read as
    /// a claim of complete mailbox fidelity.
    pub fn dimensions(&self) -> VerificationDimensions {
        let exact = self.is_exact_match();
        VerificationDimensions {
            message_presence: if exact {
                EvidenceDimensionStatus::Pass
            } else {
                EvidenceDimensionStatus::Warning
            },
            body_integrity: if self.verification_method == VerificationMethod::BodyHash && exact {
                EvidenceDimensionStatus::Pass
            } else {
                EvidenceDimensionStatus::NotVerified
            },
            internal_dates: if exact {
                EvidenceDimensionStatus::Pass
            } else {
                EvidenceDimensionStatus::Warning
            },
            system_flags: EvidenceDimensionStatus::NotVerified,
            custom_keywords: EvidenceDimensionStatus::Warning,
            folder_subscriptions: EvidenceDimensionStatus::NotVerified,
            folder_mapping: if exact {
                EvidenceDimensionStatus::Pass
            } else {
                EvidenceDimensionStatus::Warning
            },
            acls: EvidenceDimensionStatus::NotVerified,
            quota_capacity: EvidenceDimensionStatus::NotVerified,
            special_use: EvidenceDimensionStatus::NotVerified,
            gmail_labels: EvidenceDimensionStatus::NotVerified,
        }
    }
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
    ExactBodyMatch,
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
            Self::ExactBodyMatch => "exact_body_match",
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
            Self::ExactBodyMatch => {
                "Exact body match — bounded RFC822 SHA-256 fingerprints compared"
            }
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

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "exact_body_match" => Self::ExactBodyMatch,
            "exact_metadata_match" => Self::ExactMetadataMatch,
            "probable_match" => Self::ProbableMatch,
            "ambiguous" => Self::Ambiguous,
            "missing" => Self::Missing,
            "changed" => Self::Changed,
            "unexpected" => Self::Unexpected,
            "incomplete" => Self::Incomplete,
            "failed" => Self::Failed,
            _ => return None,
        })
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
pub struct ReportMailboxPage {
    pub rows: Vec<ReportMailboxSnapshot>,
    pub first_rowid: Option<i64>,
    pub last_rowid: Option<i64>,
}

/// Operator-facing assurance facts derived only from durable mailbox state and
/// evidence. This is deliberately a read model: it never upgrades an unknown
/// fact into a success claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxAssurance {
    pub transfer_completed: bool,
    pub destination_reachable: Option<bool>,
    pub inventory_reconciled: bool,
    pub message_level_evidence: bool,
    pub differences_found: u64,
    pub differences_accepted: bool,
    pub unresolved: bool,
    pub verification_authority: Option<String>,
    pub evidence_run_id: Option<String>,
    pub plan_snapshot: Option<String>,
}

impl ReportMailboxSnapshot {
    pub fn assurance(&self) -> MailboxAssurance {
        let transfer_completed = matches!(
            self.job.state.as_str(),
            "completed"
                | "verified"
                | "verified_with_exceptions"
                | "delta_required"
                | "verification_difference"
        );
        let (evidence_run_id, evidence, plan_snapshot) = self
            .evidence
            .as_ref()
            .map(|(run_id, evidence, plan)| (Some(run_id.clone()), Some(evidence), plan.clone()))
            .unwrap_or((None, None, None));
        let differences_found = evidence.map_or(0, |value| {
            value
                .missing_messages
                .saturating_add(value.extra_messages)
                .saturating_add(value.modified_messages)
        });
        MailboxAssurance {
            transfer_completed,
            destination_reachable: evidence.map(|_| true),
            // Equal aggregate folder counts do not prove that the same
            // folders were present. Only an exact per-message reconciliation
            // establishes that the independently inventoried mailboxes
            // reconcile.
            inventory_reconciled: evidence.is_some_and(|value| {
                matches!(
                    value.verification_method(),
                    VerificationMethod::MetadataReconciliation | VerificationMethod::BodyHash
                ) && value.is_exact_match()
            }),
            message_level_evidence: evidence.is_some_and(|value| {
                matches!(
                    value.verification_method(),
                    VerificationMethod::BodyHash | VerificationMethod::MetadataReconciliation
                )
            }),
            differences_found,
            differences_accepted: self.acceptance.is_some(),
            // A successful transfer attempt is not the same as a mailbox
            // that is safe to close. Until verification is accepted (with or
            // without exceptions), the project still has unresolved work.
            unresolved: !matches!(
                self.job.state.as_str(),
                "verified" | "verified_with_exceptions"
            ),
            verification_authority: evidence
                .map(|value| value.verification_method().as_str().to_owned()),
            evidence_run_id,
            plan_snapshot,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRunSnapshot {
    pub run: RunSummary,
    pub engine_version: Option<String>,
    /// `Some(n)` means n presentation lines were dropped. `None` means no
    /// durable drop accounting exists (including runs predating this marker).
    pub diagnostic_lines_dropped: Option<u64>,
    /// Number of engine transfer attempts durably started for this run.
    /// `None` means the run predates attempt accounting or no attempt started.
    pub transfer_attempt_count: Option<u64>,
    /// Starts without a matching durable finish record, including an
    /// interruption before the engine result was durably recorded.
    pub unfinished_transfer_attempt_count: u64,
    /// Durable provenance of each transfer attempt of this run (schema v15
    /// and later; empty for earlier runs).
    pub transfer_passes: Vec<super::TransferPassRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectReportSnapshot {
    pub project: Project,
    pub mailboxes: Vec<ReportMailboxSnapshot>,
    /// The most recent runs, for presentation. Bounded; do not use it to
    /// decide whether work is still active.
    pub runs: Vec<ReportRunSnapshot>,
    /// Whether any run of the project, not only a recent one, is queued or
    /// running. A batch parent can be older than its newest children.
    pub has_active_runs: bool,
}

/// The strongest claim supported by the current verifier adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceScope {
    EngineConfirmed,
    AggregateReconciled,
    BodyHashed,
}

impl EvidenceScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::EngineConfirmed => "engine-confirmed",
            Self::AggregateReconciled => "aggregate-reconciled",
            Self::BodyHashed => "body-hash",
        }
    }
}

#[allow(dead_code)]
impl VerificationEvidence {
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
            || self.probable_messages > 0
            || self.has_message_level_mismatch()
    }

    pub fn evidence_scope(&self) -> EvidenceScope {
        if self.verification_method == VerificationMethod::BodyHash {
            EvidenceScope::BodyHashed
        } else if self.authoritative {
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
        if let Some(outcome) = self.verification_outcome {
            return outcome;
        }
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
        } else if self.probable_messages > 0 {
            VerificationOutcome::ProbableMatch
        } else if self.unmatched_messages.is_some_and(|count| count > 0) {
            VerificationOutcome::Ambiguous
        } else if self.aggregate_totals_match() {
            if self.verification_method == VerificationMethod::BodyHash {
                VerificationOutcome::ExactBodyMatch
            } else {
                VerificationOutcome::ExactMetadataMatch
            }
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
        self.probable_messages
    }

    pub fn metadata_matched_count(&self) -> u64 {
        self.source_messages
            .saturating_sub(self.unmatched_messages.unwrap_or(0))
            .saturating_sub(self.probable_messages)
    }

    pub fn evidence_level(&self) -> &'static str {
        if self.failed_messages > 0 || self.unmatched_messages.is_none_or(|count| count > 0) {
            return "Incomplete evidence";
        }
        if self.has_message_level_mismatch() || self.probable_messages > 0 {
            return "Message-level mismatch";
        }
        let exact = self.aggregate_totals_match();
        if self.evidence_scope() == EvidenceScope::BodyHashed && exact {
            "Level 3 — Bounded content fingerprints"
        } else if self.evidence_scope() == EvidenceScope::EngineConfirmed && exact {
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
            VerificationOutcome::Missing => {
                Some("message-level reconciliation found missing messages")
            }
            VerificationOutcome::Changed => {
                Some("message-level reconciliation found changed messages")
            }
            VerificationOutcome::Unexpected => {
                Some("message-level reconciliation found unexpected messages")
            }
            VerificationOutcome::ProbableMatch => {
                Some("message identity remains probable rather than exact")
            }
            VerificationOutcome::Ambiguous => {
                Some("message identity could not be resolved unambiguously")
            }
            VerificationOutcome::ExactBodyMatch | VerificationOutcome::ExactMetadataMatch => None,
        }
    }

    /// Named evidence tier exposed to operators and proof consumers. A tier
    /// describes the kind of comparison performed, not its outcome or
    /// completeness; those remain separate fields.
    pub fn verification_level(&self) -> &'static str {
        if self.unmatched_messages.is_none() || self.failed_messages > 0 {
            "Incomplete evidence — no verification level"
        } else {
            match self.verification_method {
                VerificationMethod::AggregateEngine | VerificationMethod::NativeDovecot => {
                    "Level 1 — Aggregate evidence — individual messages not compared"
                }
                VerificationMethod::MetadataReconciliation => {
                    "Level 2 — Per-message metadata reconciliation — bodies not compared"
                }
                VerificationMethod::BodyHash => {
                    "Level 3 — Bounded content fingerprints — not full byte-for-byte proof"
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn confidence_percent(&self) -> u8 {
        if self.has_verification_exception() {
            return 0;
        }
        if self.verification_method == VerificationMethod::BodyHash
            && self.verification_outcome() == VerificationOutcome::ExactBodyMatch
        {
            return 100;
        }
        let exact = self.aggregate_totals_match();
        if !self.authoritative {
            return if exact { 85 } else { 0 };
        }
        if exact { 100 } else { 85 }
    }

    pub fn is_exact_match(&self) -> bool {
        matches!(
            self.verification_outcome(),
            VerificationOutcome::ExactBodyMatch | VerificationOutcome::ExactMetadataMatch
        ) && self.aggregate_totals_match()
            && !self.has_verification_exception()
    }
}

#[cfg(test)]
mod tests {
    use super::{MailboxEvidence, ReportMailboxSnapshot, VerificationMethod, VerificationOutcome};
    use crate::core::MailboxJob;

    fn missing_evidence() -> MailboxEvidence {
        MailboxEvidence {
            verification_method: VerificationMethod::MetadataReconciliation,
            verification_outcome: None,
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
            probable_messages: 0,
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
        assert_eq!(
            evidence.verification_reason(),
            Some("message-level reconciliation found missing messages")
        );
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

    #[test]
    fn body_hash_exact_evidence_is_not_labeled_as_metadata_only() {
        let evidence = MailboxEvidence {
            verification_method: VerificationMethod::BodyHash,
            verification_outcome: None,
            source_messages: 2,
            destination_messages: 2,
            source_bytes: 20,
            destination_bytes: 20,
            unmatched_messages: Some(0),
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: false,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
            probable_messages: 0,
        };
        assert_eq!(
            evidence.verification_outcome(),
            VerificationOutcome::ExactBodyMatch
        );
        assert_eq!(evidence.evidence_scope(), super::EvidenceScope::BodyHashed);
        assert_eq!(evidence.confidence_percent(), 100);
        assert!(evidence.is_exact_match());
        assert!(
            evidence
                .verification_outcome()
                .display_label()
                .contains("SHA-256")
        );
    }

    #[test]
    fn probable_matches_cannot_be_promoted_to_exact() {
        let mut evidence = missing_evidence();
        evidence.verification_outcome = None;
        evidence.source_messages = 100;
        evidence.destination_messages = 100;
        evidence.source_bytes = 1_000;
        evidence.destination_bytes = 1_000;
        evidence.unmatched_messages = Some(0);
        evidence.missing_messages = 0;
        evidence.extra_messages = 0;
        evidence.modified_messages = 0;
        evidence.probable_messages = 1;

        assert_eq!(
            evidence.verification_outcome(),
            VerificationOutcome::ProbableMatch
        );
        assert!(!evidence.is_exact_match());
    }

    #[test]
    fn assurance_never_claims_completion_without_durable_evidence() {
        let mailbox = ReportMailboxSnapshot {
            job: MailboxJob {
                id: "job".into(),
                source_mailbox: "source".into(),
                destination_mailbox: "destination".into(),
                state: "completed".into(),
                config: None,
            },
            attention_reason: None,
            acceptance: None,
            evidence: None,
        };
        let assurance = mailbox.assurance();
        assert!(assurance.transfer_completed);
        assert_eq!(assurance.destination_reachable, None);
        assert!(!assurance.inventory_reconciled);
        assert!(!assurance.message_level_evidence);
        assert!(
            assurance.unresolved,
            "a completed transfer without verification is not safe to close"
        );
    }

    #[test]
    fn assurance_does_not_infer_inventory_identity_from_equal_aggregate_counts() {
        let mailbox = ReportMailboxSnapshot {
            job: MailboxJob {
                id: "job".into(),
                source_mailbox: "source".into(),
                destination_mailbox: "destination".into(),
                state: "delta_required".into(),
                config: None,
            },
            attention_reason: None,
            acceptance: None,
            evidence: Some((
                "run".into(),
                MailboxEvidence {
                    verification_method: VerificationMethod::AggregateEngine,
                    verification_outcome: Some(VerificationOutcome::ExactMetadataMatch),
                    source_messages: 10,
                    destination_messages: 10,
                    source_bytes: 100,
                    destination_bytes: 100,
                    unmatched_messages: Some(0),
                    failed_messages: 0,
                    source_folders: 3,
                    destination_folders: 3,
                    authoritative: true,
                    missing_messages: 0,
                    extra_messages: 0,
                    modified_messages: 0,
                    probable_messages: 0,
                },
                None,
            )),
        };

        let assurance = mailbox.assurance();
        assert!(assurance.transfer_completed);
        assert_eq!(assurance.destination_reachable, Some(true));
        assert!(
            !assurance.inventory_reconciled,
            "matching folder counts do not establish matching folder identities"
        );
        assert!(
            assurance.unresolved,
            "a mailbox awaiting its delta is not safe to close"
        );
    }

    #[test]
    fn assurance_saturates_difference_totals_instead_of_wrapping() {
        let mailbox = ReportMailboxSnapshot {
            job: MailboxJob {
                id: "job".into(),
                source_mailbox: "source".into(),
                destination_mailbox: "destination".into(),
                state: "verification_difference".into(),
                config: None,
            },
            attention_reason: None,
            acceptance: None,
            evidence: Some((
                "run".into(),
                MailboxEvidence {
                    verification_method: VerificationMethod::MetadataReconciliation,
                    verification_outcome: Some(VerificationOutcome::Missing),
                    source_messages: 0,
                    destination_messages: 0,
                    source_bytes: 0,
                    destination_bytes: 0,
                    unmatched_messages: Some(u64::MAX),
                    failed_messages: 0,
                    source_folders: 0,
                    destination_folders: 0,
                    authoritative: false,
                    missing_messages: u64::MAX,
                    extra_messages: u64::MAX,
                    modified_messages: u64::MAX,
                    probable_messages: 0,
                },
                None,
            )),
        };

        assert_eq!(mailbox.assurance().differences_found, u64::MAX);
    }
}
