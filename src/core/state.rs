//! Durable lifecycle state types and their SQLite wire representations.

use serde::{Deserialize, Serialize};

/// Discrete operator-facing migration states. These are deliberately not a
/// percentage or probability: a project is either ready, blocked, verified,
/// or requires a concrete operator decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(dead_code)]
pub enum OperationalState {
    Ready,
    ReadyWithWarnings,
    Blocked,
    Migrated,
    Verified,
    VerifiedWithExceptions,
    CutoverReady,
    NeedsAttention,
}

impl OperationalState {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "READY",
            Self::ReadyWithWarnings => "READY WITH WARNINGS",
            Self::Blocked => "BLOCKED",
            Self::Migrated => "MIGRATED",
            Self::Verified => "VERIFIED",
            Self::VerifiedWithExceptions => "VERIFIED WITH EXCEPTIONS",
            Self::CutoverReady => "CUTOVER READY",
            Self::NeedsAttention => "NEEDS ATTENTION",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OperationalState;

    #[test]
    fn operational_state_labels_are_stable() {
        let states = [
            (OperationalState::Ready, "READY"),
            (OperationalState::ReadyWithWarnings, "READY WITH WARNINGS"),
            (OperationalState::Blocked, "BLOCKED"),
            (OperationalState::Migrated, "MIGRATED"),
            (OperationalState::Verified, "VERIFIED"),
            (
                OperationalState::VerifiedWithExceptions,
                "VERIFIED WITH EXCEPTIONS",
            ),
            (OperationalState::CutoverReady, "CUTOVER READY"),
            (OperationalState::NeedsAttention, "NEEDS ATTENTION"),
        ];
        for (state, label) in states {
            assert_eq!(state.as_str(), label);
        }
    }
}

/// Durable operator-review categories. These stable wire values let reports,
/// automation, and future UI versions classify a row without parsing human
/// facing run output.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AttentionReason {
    Interrupted,
    VerificationIncomplete,
    /// The transfer completed, but verification stopped at a safety limit
    /// (`VerificationLimit`), so the evidence is incomplete for a known,
    /// actionable reason rather than a verifier error.
    VerificationLimitExceeded,
    VerificationDifference,
    ProcessIdentityUnverified,
    AuthenticationFailed,
    TransportFailed,
    PolicyBlocked,
    ConfigurationInvalid,
    CapacityLimited,
    MessageRejected,
    Unknown,
}

impl AttentionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::VerificationIncomplete => "verification_incomplete",
            Self::VerificationLimitExceeded => "verification_limit_exceeded",
            Self::VerificationDifference => "verification_difference",
            Self::ProcessIdentityUnverified => "process_identity_unverified",
            Self::AuthenticationFailed => "authentication_failed",
            Self::TransportFailed => "transport_failed",
            Self::PolicyBlocked => "policy_blocked",
            Self::ConfigurationInvalid => "configuration_invalid",
            Self::CapacityLimited => "capacity_limited",
            Self::MessageRejected => "message_rejected",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "interrupted" => Self::Interrupted,
            "verification_incomplete" => Self::VerificationIncomplete,
            "verification_limit_exceeded" => Self::VerificationLimitExceeded,
            "verification_difference" => Self::VerificationDifference,
            "process_identity_unverified" => Self::ProcessIdentityUnverified,
            "authentication_failed" => Self::AuthenticationFailed,
            "transport_failed" => Self::TransportFailed,
            "policy_blocked" => Self::PolicyBlocked,
            "configuration_invalid" => Self::ConfigurationInvalid,
            "capacity_limited" => Self::CapacityLimited,
            "message_rejected" => Self::MessageRejected,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Interrupted => "Interrupted; recovery review required",
            Self::VerificationIncomplete => "Verification evidence is incomplete",
            Self::VerificationLimitExceeded => {
                "Transfer completed; verification exceeded a safety limit"
            }
            Self::VerificationDifference => "Verification found differences",
            Self::ProcessIdentityUnverified => "Process ownership could not be verified",
            Self::AuthenticationFailed => "Authentication failed",
            Self::TransportFailed => "Network or remote-service failure",
            Self::PolicyBlocked => "Blocked by migration policy",
            Self::ConfigurationInvalid => "Configuration is invalid",
            Self::CapacityLimited => "Capacity or rate limit reached",
            Self::MessageRejected => "A message was rejected by the destination",
            Self::Unknown => "Operator review required",
        }
    }

    pub fn recommended_action(self) -> &'static str {
        match self {
            Self::Interrupted | Self::ProcessIdentityUnverified => {
                "Confirm no migration process remains, then retry"
            }
            Self::VerificationIncomplete | Self::VerificationDifference => {
                "Review the evidence and reconcile before retrying or completing"
            }
            Self::VerificationLimitExceeded => {
                "Follow the limit guidance in the run detail, then rerun to obtain evidence"
            }
            Self::AuthenticationFailed => {
                "Verify credentials and endpoint permissions before retrying"
            }
            Self::TransportFailed => "Check endpoint health and retry with bounded backoff",
            Self::PolicyBlocked | Self::ConfigurationInvalid => {
                "Correct the migration configuration or policy, then rerun preflight"
            }
            Self::CapacityLimited => "Reduce concurrency or rate and retry after capacity recovers",
            Self::MessageRejected => {
                "Review rejected-message detail and destination policy before retrying"
            }
            Self::Unknown => "Inspect the durable run detail before choosing an action",
        }
    }
}

/// Stable durable mailbox states. SQLite stores their wire representation,
/// while controller and UI policy use this enum so a new terminal state
/// cannot be accidentally omitted from one execution path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MailboxState {
    Queued,
    Preflight,
    Ready,
    Running,
    Completed,
    Verified,
    VerifiedWithExceptions,
    Failed,
    Cancelled,
    Attention,
    DeltaRequired,
    VerificationDifference,
}

impl MailboxState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Preflight => "preflight",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Verified => "verified",
            Self::VerifiedWithExceptions => "verified_with_exceptions",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Attention => "attention",
            Self::DeltaRequired => "delta_required",
            Self::VerificationDifference => "verification_difference",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "preflight" => Self::Preflight,
            "ready" => Self::Ready,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "verified" => Self::Verified,
            "verified_with_exceptions" => Self::VerifiedWithExceptions,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "attention" => Self::Attention,
            "delta_required" => Self::DeltaRequired,
            "verification_difference" => Self::VerificationDifference,
            _ => return None,
        })
    }

    /// Parse a wire state without allocating a normalized copy. Imported
    /// queues may carry display-case values, while durable state remains
    /// lowercase; callers that only need classification should use this
    /// helper instead of allocating through `to_ascii_lowercase()`.
    pub fn parse_ascii_case_insensitive(value: &str) -> Option<Self> {
        [
            ("queued", Self::Queued),
            ("preflight", Self::Preflight),
            ("ready", Self::Ready),
            ("running", Self::Running),
            ("completed", Self::Completed),
            ("verified", Self::Verified),
            ("verified_with_exceptions", Self::VerifiedWithExceptions),
            ("failed", Self::Failed),
            ("cancelled", Self::Cancelled),
            ("attention", Self::Attention),
            ("delta_required", Self::DeltaRequired),
            ("verification_difference", Self::VerificationDifference),
        ]
        .into_iter()
        .find_map(|(wire, state)| value.eq_ignore_ascii_case(wire).then_some(state))
    }

    pub fn is_verified(self) -> bool {
        matches!(self, Self::Verified | Self::VerifiedWithExceptions)
    }

    pub fn needs_operator_review(self) -> bool {
        matches!(
            self,
            Self::Attention | Self::Failed | Self::Cancelled | Self::VerificationDifference
        )
    }
}

/// The only legal durable mailbox-state edges. Keeping this policy beside the
/// wire enum makes the transition invariant independently fuzzable and avoids
/// callers inventing terminal-state shortcuts.
pub(crate) fn valid_mailbox_transition(current: &str, next: &str) -> bool {
    let (Some(current), Some(next)) = (MailboxState::parse(current), MailboxState::parse(next))
    else {
        return false;
    };
    match current {
        MailboxState::Queued => matches!(
            next,
            MailboxState::Preflight
                | MailboxState::Ready
                | MailboxState::Running
                | MailboxState::Failed
                | MailboxState::Cancelled
                | MailboxState::Attention
        ),
        MailboxState::Preflight => matches!(
            next,
            MailboxState::Ready
                | MailboxState::Running
                | MailboxState::Failed
                | MailboxState::Cancelled
        ),
        MailboxState::Ready => matches!(
            next,
            MailboxState::Running | MailboxState::Failed | MailboxState::Cancelled
        ),
        MailboxState::Running => matches!(
            next,
            MailboxState::Ready
                | MailboxState::Completed
                | MailboxState::DeltaRequired
                | MailboxState::VerificationDifference
                | MailboxState::Failed
                | MailboxState::Cancelled
                | MailboxState::Attention
        ),
        MailboxState::DeltaRequired | MailboxState::VerificationDifference => matches!(
            next,
            MailboxState::Running
                | MailboxState::Failed
                | MailboxState::Cancelled
                | MailboxState::Attention
                | MailboxState::VerifiedWithExceptions
        ),
        MailboxState::Completed => matches!(
            next,
            MailboxState::Verified
                | MailboxState::DeltaRequired
                | MailboxState::VerificationDifference
                | MailboxState::Running
                | MailboxState::Attention
        ),
        MailboxState::Failed | MailboxState::Cancelled => {
            matches!(next, MailboxState::Running | MailboxState::Attention)
        }
        MailboxState::Verified | MailboxState::VerifiedWithExceptions => matches!(
            next,
            MailboxState::Ready
                | MailboxState::DeltaRequired
                | MailboxState::Running
                | MailboxState::Attention
        ),
        MailboxState::Attention => matches!(next, MailboxState::Running),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Phase {
    Discovery,
    Preflight,
    Pilot,
    Seed,
    CatchUp,
    FinalDelta,
    Verification,
    Complete,
    Attention,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Preflight => "preflight",
            Self::Pilot => "pilot",
            Self::Seed => "seed",
            Self::CatchUp => "catch_up",
            Self::FinalDelta => "final_delta",
            Self::Verification => "verification",
            Self::Complete => "complete",
            Self::Attention => "attention",
        }
    }

    pub(crate) fn parse(value: &str) -> rusqlite::Result<Self> {
        match value {
            "discovery" => Ok(Self::Discovery),
            "preflight" => Ok(Self::Preflight),
            "pilot" => Ok(Self::Pilot),
            "seed" => Ok(Self::Seed),
            "catch_up" => Ok(Self::CatchUp),
            "final_delta" => Ok(Self::FinalDelta),
            "verification" => Ok(Self::Verification),
            "complete" => Ok(Self::Complete),
            "attention" => Ok(Self::Attention),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}
