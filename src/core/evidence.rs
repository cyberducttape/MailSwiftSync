use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxEvidence {
    pub source_messages: u64,
    pub destination_messages: u64,
    pub source_bytes: u64,
    pub destination_bytes: u64,
    /// A literal unresolved-message count when the verifier provides one.
    pub unmatched_messages: u64,
    pub failed_messages: u64,
    pub source_folders: u64,
    pub destination_folders: u64,
    /// True only when the engine supplied a stronger engine-confirmed summary.
    pub authoritative: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationAcceptance {
    pub job_id: String,
    pub run_id: String,
    pub operator: String,
    pub reason: String,
    pub accepted_at: String,
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
    pub fn evidence_scope(&self) -> EvidenceScope {
        if self.authoritative {
            EvidenceScope::EngineConfirmed
        } else {
            EvidenceScope::AggregateReconciled
        }
    }

    pub fn evidence_level(&self) -> &'static str {
        if self.failed_messages > 0 || self.unmatched_messages > 0 {
            return "Incomplete evidence";
        }
        let exact = self.source_messages == self.destination_messages
            && self.source_bytes == self.destination_bytes
            && self.source_folders == self.destination_folders;
        if self.evidence_scope() == EvidenceScope::EngineConfirmed && exact {
            "Engine-confirmed exact match"
        } else if exact {
            "Aggregate match"
        } else {
            "Aggregate mismatch"
        }
    }

    #[allow(dead_code)]
    pub fn confidence_percent(&self) -> u8 {
        let exact = self.source_messages == self.destination_messages
            && self.source_bytes == self.destination_bytes
            && self.source_folders == self.destination_folders;
        if !self.authoritative {
            return if exact && self.unmatched_messages == 0 && self.failed_messages == 0 {
                85
            } else {
                0
            };
        }
        if self.source_messages == 0
            && self.destination_messages == 0
            && self.source_folders == self.destination_folders
            && self.unmatched_messages == 0
            && self.failed_messages == 0
        {
            return 100;
        }
        let count_ok = self.source_messages == self.destination_messages;
        let bytes_ok = self.source_bytes == self.destination_bytes;
        let folders_ok = self.source_folders == self.destination_folders;
        if count_ok
            && bytes_ok
            && folders_ok
            && self.unmatched_messages == 0
            && self.failed_messages == 0
        {
            100
        } else if self.unmatched_messages == 0 && self.failed_messages == 0 {
            85
        } else {
            0
        }
    }

    pub fn is_exact_match(&self) -> bool {
        self.source_messages == self.destination_messages
            && self.source_bytes == self.destination_bytes
            && self.source_folders == self.destination_folders
            && self.unmatched_messages == 0
            && self.failed_messages == 0
    }
}
