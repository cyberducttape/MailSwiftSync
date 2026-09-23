use serde::{Deserialize, Serialize};

use super::{AttentionReason, MailboxJob, Project, RunSummary};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxEvidence {
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

    pub fn evidence_level(&self) -> &'static str {
        if self.failed_messages > 0 || self.unmatched_messages.map_or(true, |count| count > 0) {
            return "Incomplete evidence";
        }
        if self.has_message_level_mismatch() {
            return "Message-level mismatch";
        }
        let exact = self.aggregate_totals_match();
        if self.evidence_scope() == EvidenceScope::EngineConfirmed && exact {
            "Engine-confirmed exact match"
        } else if exact {
            "Aggregate match"
        } else {
            "Aggregate mismatch"
        }
    }

    pub fn verification_reason(&self) -> Option<&'static str> {
        if self.unmatched_messages.is_none() {
            Some("imapsync completion proof absent")
        } else if self.failed_messages > 0 {
            Some("engine reported migration errors")
        } else if self.has_message_level_mismatch() {
            Some("message-level reconciliation found differences")
        } else {
            None
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
