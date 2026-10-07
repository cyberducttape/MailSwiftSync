//! Structured verification safety limits.
//!
//! The verifier fails closed when an account exceeds one of its bounds. Such a
//! failure happens after the transfer completed, so it must stay
//! distinguishable from a failed transfer and from a verifier error: the
//! migration may be fine while the evidence is out of reach. Limit sites tag
//! their error with `[verification_limit=<code>]`; the controller reads the
//! tag back into a `VerificationLimit` to choose the durable attention reason
//! and the operator guidance. The codes are a stable automation contract.

/// One verifier safety limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationLimit {
    /// IMAP LIST inventory bytes or folder count.
    FolderInventory,
    /// One tagged IMAP response exceeded its byte bound.
    ResponseSize,
    /// Transient fetched-page state exceeded its admission budget.
    FetchedState,
    /// More messages on one endpoint than metadata verification admits.
    MessageCount,
    /// More messages on one endpoint than body-hash verification admits.
    BodyHashMessageCount,
    /// One message body exceeded the configured body-hash bound.
    BodyHashMessageSize,
    /// All hashed bodies together exceeded the configured total bound.
    BodyHashTotalSize,
    /// Reconciliation state or mismatch detail exceeded its budget.
    ReconciliationState,
    /// Verification ran past its execution deadline.
    Deadline,
}

const ALL: [VerificationLimit; 9] = [
    VerificationLimit::FolderInventory,
    VerificationLimit::ResponseSize,
    VerificationLimit::FetchedState,
    VerificationLimit::MessageCount,
    VerificationLimit::BodyHashMessageCount,
    VerificationLimit::BodyHashMessageSize,
    VerificationLimit::BodyHashTotalSize,
    VerificationLimit::ReconciliationState,
    VerificationLimit::Deadline,
];

const MARKER_PREFIX: &str = "[verification_limit=";

impl VerificationLimit {
    pub fn code(self) -> &'static str {
        match self {
            Self::FolderInventory => "folder_inventory",
            Self::ResponseSize => "response_size",
            Self::FetchedState => "fetched_state",
            Self::MessageCount => "message_count",
            Self::BodyHashMessageCount => "body_hash_message_count",
            Self::BodyHashMessageSize => "body_hash_message_size",
            Self::BodyHashTotalSize => "body_hash_total_size",
            Self::ReconciliationState => "reconciliation_state",
            Self::Deadline => "deadline",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        ALL.into_iter().find(|limit| limit.code() == code)
    }

    /// The tag a limit site puts in front of its error text.
    pub fn marker(self) -> String {
        format!("{MARKER_PREFIX}{}]", self.code())
    }

    /// Prefix `error` with this limit's tag.
    pub fn tag(self, error: impl std::fmt::Display) -> String {
        format!("{} {error}", self.marker())
    }

    /// The first limit tagged anywhere in `detail`. Errors are wrapped as
    /// they propagate, so the tag is not required to lead the text.
    pub fn from_detail(detail: &str) -> Option<Self> {
        let (_, rest) = detail.split_once(MARKER_PREFIX)?;
        let (code, _) = rest.split_once(']')?;
        Self::parse(code)
    }

    /// Whether rerunning the same plan can make progress: staged pages are
    /// reused, so a run that ran out of time or hit a transient bound picks
    /// up where it stopped. The other limits recur until the plan or scope
    /// changes.
    pub fn resumable(self) -> bool {
        matches!(self, Self::Deadline | Self::FetchedState)
    }

    /// What the operator should do. Every answer keeps verification fail
    /// closed: none of them accepts the missing evidence implicitly.
    pub fn guidance(self) -> &'static str {
        match self {
            Self::FolderInventory => {
                "The account's folder list exceeds the verifier's inventory bound (32 MiB or 100,000 folders). Exclude folders from the plan or split the account, then rerun to obtain evidence."
            }
            Self::ResponseSize => {
                "A server response exceeded the verifier's 64 MiB bound, usually a very large message during body-hash verification. Lower the body-hash per-message bound or use metadata verification, then rerun."
            }
            Self::FetchedState => {
                "Verification exceeded its transient memory budget. Rerun to resume from the staged pages; if it recurs, use metadata verification."
            }
            Self::MessageCount => {
                "An endpoint holds more than the 1,000,000 messages metadata verification admits. Split the account into smaller folder scopes, or obtain evidence another way and accept the difference explicitly."
            }
            Self::BodyHashMessageCount => {
                "Body-hash verification admits at most 100,000 messages per endpoint. Disable body-hash verification to obtain metadata evidence for this mailbox, then rerun."
            }
            Self::BodyHashMessageSize => {
                "A message exceeded the body-hash per-message bound. Raise the bound (up to 64 MiB) or disable body-hash verification, then rerun."
            }
            Self::BodyHashTotalSize => {
                "Hashed bodies exceeded the body-hash total bound. Raise the total bound (up to 8 GiB) or disable body-hash verification, then rerun."
            }
            Self::ReconciliationState => {
                "Reconciliation found more differences than it can record, which usually means the destination differs substantially. Inspect the transfer, run another delta pass, then rerun."
            }
            Self::Deadline => {
                "Verification ran past the migration timeout. Rerun to resume from the staged pages, or raise the migration timeout."
            }
        }
    }

    /// Run detail for a completed transfer whose verification hit this
    /// limit: the durable attention tag, the limit tag, the verifier's error,
    /// and the guidance.
    pub fn attention_detail(self, error: &str) -> String {
        let error = error.trim();
        let error = if Self::from_detail(error) == Some(self) {
            error.to_owned()
        } else {
            self.tag(error)
        };
        format!(
            "[attention_reason={}] transfer completed; verification exceeded a safety limit: {error} Next step: {}",
            super::AttentionReason::VerificationLimitExceeded.as_str(),
            self.guidance()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_and_are_unique() {
        for limit in ALL {
            assert_eq!(VerificationLimit::parse(limit.code()), Some(limit));
            assert_eq!(
                ALL.iter()
                    .filter(|other| other.code() == limit.code())
                    .count(),
                1
            );
        }
        assert_eq!(VerificationLimit::parse("unknown"), None);
    }

    #[test]
    fn tags_survive_wrapping_and_are_read_back() {
        let wrapped = format!(
            "imap.example.test: 1 of 3 folders incomplete: INBOX: {}",
            VerificationLimit::ResponseSize.tag("response exceeded")
        );
        assert_eq!(
            VerificationLimit::from_detail(&wrapped),
            Some(VerificationLimit::ResponseSize)
        );
        assert_eq!(VerificationLimit::from_detail("connection reset"), None);
        assert_eq!(
            VerificationLimit::from_detail("[verification_limit=bogus] x"),
            None
        );
    }

    #[test]
    fn attention_detail_carries_reason_limit_and_guidance_once() {
        let error = VerificationLimit::BodyHashMessageCount.tag("too many messages");
        let detail = VerificationLimit::BodyHashMessageCount.attention_detail(&error);
        assert!(detail.starts_with("[attention_reason=verification_limit_exceeded] "));
        assert_eq!(detail.matches("[verification_limit=").count(), 1);
        assert!(detail.ends_with(VerificationLimit::BodyHashMessageCount.guidance()));
        assert_eq!(
            super::super::policy::attention_reason_for("attention", &detail),
            Some(super::super::AttentionReason::VerificationLimitExceeded)
        );
    }
}
