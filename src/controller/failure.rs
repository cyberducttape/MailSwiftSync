//! Failure taxonomy and retry policy for migration controller outcomes.

use crate::core;
use std::time::Duration;

/// External success may advance the durable lifecycle only after the
/// terminal result has been committed and no durability fault remains.
pub(crate) fn terminal_phase_advance_allowed(
    external_succeeded: bool,
    terminal_write_ok: bool,
    durability_error: bool,
) -> bool {
    external_succeeded && terminal_write_ok && !durability_error
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureClass {
    Cancellation,
    Authentication,
    Quota,
    Capacity,
    Transport,
    Configuration,
    Message,
    Verification,
    Unknown,
}

impl FailureClass {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Cancellation => "cancelled",
            Self::Authentication => "authentication",
            Self::Quota => "quota",
            Self::Capacity => "capacity",
            Self::Transport => "transport",
            Self::Configuration => "configuration",
            Self::Message => "message",
            Self::Verification => "verification",
            Self::Unknown => "unknown",
        }
    }

    pub(crate) fn attention_reason(self) -> core::AttentionReason {
        match self {
            Self::Cancellation => core::AttentionReason::Interrupted,
            Self::Authentication => core::AttentionReason::AuthenticationFailed,
            Self::Quota => core::AttentionReason::CapacityLimited,
            Self::Capacity => core::AttentionReason::CapacityLimited,
            Self::Transport => core::AttentionReason::TransportFailed,
            Self::Configuration => core::AttentionReason::ConfigurationInvalid,
            Self::Message => core::AttentionReason::MessageRejected,
            Self::Verification => core::AttentionReason::VerificationIncomplete,
            Self::Unknown => core::AttentionReason::Unknown,
        }
    }

    pub(crate) fn parse_label(label: &str) -> Option<Self> {
        Some(match label {
            "cancelled" => Self::Cancellation,
            "authentication" => Self::Authentication,
            "quota" => Self::Quota,
            "capacity" => Self::Capacity,
            "transport" => Self::Transport,
            "configuration" => Self::Configuration,
            "message" => Self::Message,
            "verification" => Self::Verification,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

pub(crate) fn classify_failure(error: &str) -> FailureClass {
    // Controller-generated failures carry a stable machine-readable class.
    // Prefer it over the diagnostic prose: retry and terminal-state policy
    // must not change because an engine happened to mention another keyword.
    if let Some(class) = error
        .strip_prefix("[attention_reason=")
        .and_then(|value| value.split_once("] [class="))
        .and_then(|(_, value)| value.split_once(']'))
        .and_then(|(label, _)| FailureClass::parse_label(label))
    {
        return class;
    }

    // Keep the controller taxonomy as the durable contract, but delegate
    // provider/IMAP signal recognition to the provider-intelligence module.
    // This is deliberately provider-neutral here because a failure can be
    // emitted by either endpoint; the richer provider context remains a
    // future plan-level input. Previously this classifier duplicated only a
    // subset of those patterns, leaving quota and connection-capacity signals
    // on the generic/unknown path.
    let normalized_error = error.to_ascii_lowercase();
    // IMAP diagnostics often prefix a server response with a local operation
    // label such as "authentication failed". Preserve the signal from the
    // response itself when it clearly says the provider is busy.
    if normalized_error.contains("server busy") {
        return FailureClass::Capacity;
    }
    match crate::core::provider_intelligence::ProviderErrorClassifier::classify("generic", error) {
        crate::core::provider_intelligence::ProviderErrorType::RateLimited
        | crate::core::provider_intelligence::ProviderErrorType::ConnectionCapacity => {
            return FailureClass::Capacity;
        }
        crate::core::provider_intelligence::ProviderErrorType::MailboxQuotaExceeded
        | crate::core::provider_intelligence::ProviderErrorType::StorageQuotaExceeded => {
            return FailureClass::Quota;
        }
        crate::core::provider_intelligence::ProviderErrorType::Authentication => {
            return FailureClass::Authentication;
        }
        crate::core::provider_intelligence::ProviderErrorType::TemporaryProviderFailure => {
            // Preserve the controller's established retry contract: an IMAP
            // "server busy" response is capacity pressure and receives the
            // longer backoff, while other temporary provider failures remain
            // ordinary transport retries.
            if normalized_error.contains("server busy") {
                return FailureClass::Capacity;
            }
            return FailureClass::Transport;
        }
        crate::core::provider_intelligence::ProviderErrorType::Network => {
            return FailureClass::Transport;
        }
        crate::core::provider_intelligence::ProviderErrorType::PermanentProviderFailure => {}
    }

    let error = error.to_ascii_lowercase();
    if ["cancelled", "canceled", "operator cancellation"]
        .iter()
        .any(|marker| error.contains(marker))
    {
        FailureClass::Cancellation
    } else if [
        "rate limit",
        "rate-limit",
        "throttl",
        "too many connections",
        "too many requests",
        "server busy",
        "temporarily overloaded",
        "429",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Capacity
    } else if [
        "overquota",
        "over quota",
        "quota exceeded",
        "quota",
        "mailbox is full",
        "insufficient storage",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Quota
    } else if [
        "message too large",
        "too large",
        "append failed",
        "invalid message",
        "message rejected",
        "msg rejected",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Message
    } else if [
        "verification",
        "evidence incomplete",
        "evidence unavailable",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Verification
    } else if [
        "invalid peer certificate",
        "certificate verify failed",
        "certificate validation",
        "certificate expired",
        "certificate sha-256 pin mismatch",
        "unknown issuer",
        "not valid for name",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Configuration
    } else if [
        "timed out",
        "timeout",
        "operation would block",
        "connection reset",
        "connection refused",
        "connection closed",
        "peer closed connection",
        "connection aborted",
        "network is unreachable",
        "host is unreachable",
        "broken pipe",
        "unexpected eof",
        "failed to lookup address",
        "name or service not known",
        "nodename nor servname provided",
        "temporary failure in name resolution",
        "no address found",
        "tls handshake eof",
        "handshake timed out",
        "list response exceeded the 60-second processing limit",
        "temporarily unavailable",
        "try again",
        "throttl",
        "rate limit",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Transport
    } else if [
        "authentication",
        "auth failed",
        "authentification",
        "invalid credentials",
        "login denied",
        "login failed",
        "authenticationfailed",
        "permission denied",
        "not authorized",
        "authorization failed",
        "access denied",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Authentication
    } else if [
        "invalid option",
        "unknown option",
        "could not start",
        "not found",
        "no such file",
        "configuration",
        "invalid endpoint",
        "missing",
    ]
    .iter()
    .any(|marker| error.contains(marker))
    {
        FailureClass::Configuration
    } else {
        FailureClass::Unknown
    }
}

pub(crate) fn is_transient_batch_error(error: &str) -> bool {
    matches!(
        classify_failure(error),
        FailureClass::Transport | FailureClass::Capacity
    )
}

pub(crate) fn should_retry_batch_error(error: &str, attempt: usize, retry_count: usize) -> bool {
    classify_failure(error) != FailureClass::Cancellation
        && attempt < retry_count
        && is_transient_batch_error(error)
}

pub(crate) fn transient_retry_delay(error: &str, attempt: usize) -> Duration {
    let base_seconds = if classify_failure(error) == FailureClass::Capacity {
        5
    } else {
        1
    };
    let multiplier = 1_u64 << attempt.min(5);
    Duration::from_secs((base_seconds * multiplier).min(120))
}

pub(crate) fn classified_failure_detail(error: &str) -> String {
    let class = classify_failure(error);
    format!(
        "[attention_reason={}] [class={}] {error}",
        class.attention_reason().as_str(),
        class.label()
    )
}
