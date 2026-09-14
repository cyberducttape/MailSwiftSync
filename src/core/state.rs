//! Durable lifecycle state types and their SQLite wire representations.

use serde::{Deserialize, Serialize};

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
