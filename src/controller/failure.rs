//! Failure taxonomy and retry policy for migration controller outcomes.

use crate::core;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// Typed controller error used after the untrusted diagnostic edge has been
/// classified. Raw provider/engine text is retained only as bounded context;
/// retry, lifecycle, telemetry, and durable attention policy consume this
/// enum instead of reparsing prose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MigrationError {
    Cancellation {
        detail: String,
    },
    Authentication {
        detail: String,
    },
    Quota {
        detail: String,
    },
    Capacity {
        detail: String,
        retry_after: Option<Duration>,
    },
    Transport {
        detail: String,
    },
    Configuration {
        detail: String,
    },
    Message {
        detail: String,
    },
    Verification {
        detail: String,
    },
    Unknown {
        detail: String,
    },
}

impl MigrationError {
    pub(crate) fn class(&self) -> FailureClass {
        match self {
            Self::Cancellation { .. } => FailureClass::Cancellation,
            Self::Authentication { .. } => FailureClass::Authentication,
            Self::Quota { .. } => FailureClass::Quota,
            Self::Capacity { .. } => FailureClass::Capacity,
            Self::Transport { .. } => FailureClass::Transport,
            Self::Configuration { .. } => FailureClass::Configuration,
            Self::Message { .. } => FailureClass::Message,
            Self::Verification { .. } => FailureClass::Verification,
            Self::Unknown { .. } => FailureClass::Unknown,
        }
    }

    pub(crate) fn detail(&self) -> &str {
        match self {
            Self::Cancellation { detail }
            | Self::Authentication { detail }
            | Self::Quota { detail }
            | Self::Capacity { detail, .. }
            | Self::Transport { detail }
            | Self::Configuration { detail }
            | Self::Message { detail }
            | Self::Verification { detail }
            | Self::Unknown { detail } => detail,
        }
    }

    pub(crate) fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Capacity { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

fn controller_failure_class(error: &str) -> Option<FailureClass> {
    error
        .strip_prefix("[attention_reason=")
        .and_then(|value| value.split_once("] [class="))
        .and_then(|(_, value)| value.split_once(']'))
        .and_then(|(label, _)| FailureClass::parse_label(label))
}

fn classify_failure_text(provider: &str, error: &str) -> FailureClass {
    // Controller-generated failures carry a stable machine-readable class.
    // Prefer it over the diagnostic prose: retry and terminal-state policy
    // must not change because an engine happened to mention another keyword.
    if let Some(class) = controller_failure_class(error) {
        return class;
    }

    // Runner diagnostics append a presentation-only tail after this marker.
    // It can contain arbitrary mailbox/server prose and must never influence
    // retry, cooldown, or terminal-state policy.
    let error = control_error_text(error);

    // Keep the controller taxonomy as the durable contract, but delegate
    // provider/IMAP signal recognition to the provider-intelligence module.
    // The caller supplies the execution-boundary provider identity selected
    // from the endpoint that emitted the failure. RFC IMAP signals remain
    // provider-neutral, while documented provider signatures are rejected
    // when they belong to a different provider. This keeps retry, cooldown,
    // and durable failure policy aligned with the endpoint that failed.
    let normalized_error = error.to_ascii_lowercase();
    // IMAP diagnostics often prefix a server response with a local operation
    // label such as "authentication failed". Preserve the signal from the
    // response itself when it clearly says the provider is busy.
    if normalized_error.contains("server busy") {
        return FailureClass::Capacity;
    }
    match crate::core::provider_intelligence::ProviderErrorClassifier::classify(provider, error) {
        crate::core::provider_intelligence::ProviderErrorType::AuthenticationExpired => {
            return FailureClass::Authentication;
        }
        crate::core::provider_intelligence::ProviderErrorType::TlsError
        | crate::core::provider_intelligence::ProviderErrorType::DnsError
        | crate::core::provider_intelligence::ProviderErrorType::TransportError => {
            return FailureClass::Transport;
        }
        crate::core::provider_intelligence::ProviderErrorType::ImapTaggedNo
        | crate::core::provider_intelligence::ProviderErrorType::ImapBad
        | crate::core::provider_intelligence::ProviderErrorType::EngineExit => {
            return FailureClass::Transport;
        }
        crate::core::provider_intelligence::ProviderErrorType::VerificationError => {
            return FailureClass::Verification;
        }
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

pub(crate) fn classify_error_for_provider(provider: &str, error: &str) -> MigrationError {
    let detail = control_error_text(error).to_owned();
    let class = classify_failure_text(provider, error);
    let retry_after =
        crate::core::provider_intelligence::provider_signal_for_provider(provider, &detail)
            .and_then(|signal| signal.retry_after);
    match class {
        FailureClass::Cancellation => MigrationError::Cancellation { detail },
        FailureClass::Authentication => MigrationError::Authentication { detail },
        FailureClass::Quota => MigrationError::Quota { detail },
        FailureClass::Capacity => MigrationError::Capacity {
            detail,
            retry_after,
        },
        FailureClass::Transport => MigrationError::Transport { detail },
        FailureClass::Configuration => MigrationError::Configuration { detail },
        FailureClass::Message => MigrationError::Message { detail },
        FailureClass::Verification => MigrationError::Verification { detail },
        FailureClass::Unknown => MigrationError::Unknown { detail },
    }
}

pub(crate) fn classify_error(error: &str) -> MigrationError {
    classify_error_for_provider("generic", error)
}

pub(crate) fn classify_failure(error: &str) -> FailureClass {
    classify_error(error).class()
}

pub(crate) fn classify_failure_for_provider(provider: &str, error: &str) -> FailureClass {
    classify_error_for_provider(provider, error).class()
}

/// Choose a provider classifier only when the engine diagnostic identifies
/// one endpoint. Ambiguous diagnostics must not borrow either provider's
/// provider-specific signatures.
pub(crate) fn provider_for_sided_error<'a>(
    source_provider: &'a str,
    destination_provider: &'a str,
    error: &str,
) -> &'a str {
    match crate::controller::rate_domains::failure_sides(error).as_slice() {
        [crate::controller::rate_domains::Side::Source] => source_provider,
        [crate::controller::rate_domains::Side::Destination] => destination_provider,
        _ => crate::core::provider_intelligence::UNATTRIBUTED_PROVIDER,
    }
}

#[cfg(test)]
pub(crate) fn is_transient_batch_error(error: &str) -> bool {
    is_transient_batch_error_for_provider("generic", error)
}

pub(crate) fn is_transient_batch_error_for_provider(provider: &str, error: &str) -> bool {
    use crate::core::provider_intelligence::{ProviderErrorClassifier, ProviderErrorType};
    let typed = classify_error_for_provider(provider, error);
    let class = typed.class();
    if class != FailureClass::Transport || controller_failure_class(error).is_some() {
        return class == FailureClass::Capacity || class == FailureClass::Transport;
    }
    // The durable taxonomy folds several provider verdicts (tagged NO, BAD,
    // engine exit, TLS, DNS) into Transport for attention reporting, but those
    // verdicts carry their own retry contract: a tagged NO or BAD can be a
    // permanent policy or configuration rejection. Retry only what the
    // provider classifier itself declares retryable. Its fallback verdict
    // (PermanentProviderFailure) means it recognized nothing, so the
    // controller's own transport heuristics decided the class and stand.
    match ProviderErrorClassifier::classify(provider, control_error_text(error)) {
        ProviderErrorType::PermanentProviderFailure => true,
        provider => provider.is_retryable(),
    }
}

#[cfg(test)]
pub(crate) fn should_retry_batch_error(error: &str, attempt: usize, retry_count: usize) -> bool {
    should_retry_batch_error_for_provider("generic", error, attempt, retry_count)
}

pub(crate) fn should_retry_batch_error_for_provider(
    provider: &str,
    error: &str,
    attempt: usize,
    retry_count: usize,
) -> bool {
    classify_failure_for_provider(provider, error) != FailureClass::Cancellation
        && attempt < retry_count
        && is_transient_batch_error_for_provider(provider, error)
}

#[cfg(test)]
pub(crate) fn transient_retry_delay(error: &str, attempt: usize) -> Duration {
    transient_retry_delay_for_provider("generic", error, attempt)
}

pub(crate) fn transient_retry_delay_for_provider(
    provider: &str,
    error: &str,
    attempt: usize,
) -> Duration {
    let typed = classify_error_for_provider(provider, error);
    let error = typed.detail();
    let provider_error =
        crate::core::provider_intelligence::ProviderErrorClassifier::classify(provider, error);
    // A delay the server itself requested (for example Exchange Online's
    // suggested backoff) is honored as the base, capped by the provider
    // module; otherwise the class default applies.
    let server_requested = typed.retry_after();
    let base_millis = server_requested
        .or(provider_error.suggested_retry_delay())
        .map_or_else(
            || {
                if typed.class() == FailureClass::Capacity {
                    5_000_u64
                } else {
                    1_000_u64
                }
            },
            |delay| delay.as_millis().min(u128::from(u64::MAX)) as u64,
        );
    let multiplier = 1_u64 << attempt.min(5);
    let exponential = base_millis.saturating_mul(multiplier).min(120_000);
    // Add per-attempt entropy so concurrent workers do not wake on the same
    // deterministic boundary. This is deliberately bounded and local; it is
    // a collision-avoidance jitter, not a claim that provider capacity reset.
    let entropy = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.subsec_nanos() as u64)
        ^ std::process::id() as u64
        ^ attempt as u64;
    let jitter = if exponential >= 120_000 {
        0
    } else {
        entropy % (exponential / 2 + 1)
    };
    // Never retry sooner than the server asked, even past the usual cap.
    Duration::from_millis(exponential.saturating_add(jitter).min(120_000))
        .max(server_requested.unwrap_or_default())
}

/// Return only the primary failure, excluding untrusted diagnostic text added
/// for operator presentation by the process runner.
pub(crate) fn control_error_text(error: &str) -> &str {
    error
        .split_once("; recent output:")
        .map_or(error, |(primary, _)| primary)
}

fn classified_failure_detail_internal(
    provider: &str,
    error: &str,
    include_provider_context: bool,
) -> String {
    let class = classify_failure_text(provider, error);
    // A recognized documented response names its source and, when the
    // failure needs a person, what to check. The tag follows the class so
    // the durable prefix parse is unchanged.
    let signal = crate::core::provider_intelligence::provider_signal_for_provider(
        provider,
        control_error_text(error),
    )
    .map(|signal| {
        let remediation = if signal.remediation.is_empty() {
            String::new()
        } else {
            format!(" Next step: {}.", signal.remediation)
        };
        (
            format!("[signal={}:{}] ", signal.source, signal.name),
            remediation,
        )
    })
    .unwrap_or_default();
    let provider_context = if include_provider_context
        && provider != crate::core::provider_intelligence::UNATTRIBUTED_PROVIDER
    {
        format!("[provider={provider}] ")
    } else {
        String::new()
    };
    format!(
        "[attention_reason={}] [class={}] {provider_context}{}{error}{}",
        class.attention_reason().as_str(),
        class.label(),
        signal.0,
        signal.1
    )
}

/// Classify a failure with the provider selected at the execution boundary.
/// The provider marker is durable operator context; policy still consumes the
/// typed class and never parses the marker or raw diagnostic prose.
pub(crate) fn classified_failure_detail_for_provider(provider: &str, error: &str) -> String {
    classified_failure_detail_internal(provider, error, true)
}

#[cfg(test)]
pub(crate) fn classified_failure_detail(error: &str) -> String {
    classified_failure_detail_internal("generic", error, false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn terminal_provider_selection_requires_unambiguous_endpoint_attribution() {
        assert_eq!(
            super::provider_for_sided_error("gmail", "microsoft365", "Host1: server busy"),
            "gmail"
        );
        assert_eq!(
            super::provider_for_sided_error("gmail", "microsoft365", "Host2: server busy"),
            "microsoft365"
        );
        assert_eq!(
            super::provider_for_sided_error("gmail", "microsoft365", "server busy"),
            crate::core::provider_intelligence::UNATTRIBUTED_PROVIDER
        );
        assert_eq!(
            super::provider_for_sided_error("gmail", "microsoft365", "Host1 and Host2 refused"),
            crate::core::provider_intelligence::UNATTRIBUTED_PROVIDER
        );
    }

    #[test]
    fn failure_detail_names_a_recognized_signal_and_its_next_step() {
        let detail =
            super::classified_failure_detail("BAD User is authenticated but not connected.");
        assert!(
            detail.starts_with("[attention_reason=authentication_failed] [class=authentication] [signal=microsoft365:authenticated_not_connected] "),
            "{detail}"
        );
        assert!(detail.contains("Next step: Enable IMAP"), "{detail}");
        assert_eq!(
            super::classify_failure(&detail),
            super::FailureClass::Authentication
        );
        // Unrecognized failures keep the existing format.
        assert_eq!(
            super::classified_failure_detail("too many requests"),
            "[attention_reason=capacity_limited] [class=capacity] too many requests"
        );
    }

    #[test]
    fn typed_migration_error_keeps_policy_out_of_diagnostic_prose() {
        let error = super::classify_error(
            "BAD Request is throttled. Suggested Backoff Time: 30000 milliseconds; recent output: quota",
        );
        assert_eq!(error.class(), super::FailureClass::Capacity);
        assert_eq!(
            error.retry_after(),
            Some(std::time::Duration::from_secs(30))
        );
        assert_eq!(
            error.detail(),
            "BAD Request is throttled. Suggested Backoff Time: 30000 milliseconds"
        );

        let error = super::classify_error("message too large; recent output: rate limit");
        assert_eq!(error.class(), super::FailureClass::Message);
        assert_eq!(error.retry_after(), None);
    }

    #[test]
    fn provider_context_is_preserved_in_durable_batch_failure_detail() {
        let detail = super::classified_failure_detail_for_provider(
            "gmail",
            "Too many simultaneous connections",
        );
        assert!(
            detail.starts_with(
                "[attention_reason=capacity_limited] [class=capacity] [provider=gmail]"
            ),
            "{detail}"
        );
        assert_eq!(
            super::classify_failure(&detail),
            super::FailureClass::Capacity
        );
    }

    #[test]
    fn unattributed_batch_failure_does_not_claim_a_provider_signal() {
        let detail = super::classified_failure_detail_for_provider(
            crate::core::provider_intelligence::UNATTRIBUTED_PROVIDER,
            "Too many simultaneous connections",
        );
        assert!(!detail.contains("[provider="), "{detail}");
        assert!(!detail.contains("[signal=gmail:"), "{detail}");
        assert_eq!(
            super::classify_failure(&detail),
            super::FailureClass::Unknown
        );
    }

    #[test]
    fn server_requested_backoff_is_a_floor_for_the_retry_delay() {
        let delay = super::transient_retry_delay(
            "BAD Request is throttled. Suggested Backoff Time: 299961 milliseconds",
            0,
        );
        assert!(
            delay >= std::time::Duration::from_millis(299_961),
            "{delay:?}"
        );
        assert!(super::is_transient_batch_error(
            "* BYE [ALERT] Too many simultaneous connections. (Failure)"
        ));
        assert!(!super::is_transient_batch_error(
            "BAD User is authenticated but not connected."
        ));
    }

    use super::{FailureClass, classify_failure, should_retry_batch_error, transient_retry_delay};

    #[test]
    fn message_counts_cannot_trigger_capacity_or_http_status_retries() {
        for diagnostic in [
            "Migration failed unexpectedly\n429 messages copied before termination",
            "folder contains 401 messages",
            "processed 403 items",
        ] {
            assert_ne!(
                classify_failure(diagnostic),
                FailureClass::Capacity,
                "diagnostic: {diagnostic}"
            );
            assert!(!should_retry_batch_error(diagnostic, 0, 2), "{diagnostic}");
        }
    }

    #[test]
    fn protocol_status_lines_still_drive_the_intended_retry_classes() {
        assert_eq!(
            classify_failure("HTTP/1.1 429 Too Many Requests"),
            FailureClass::Capacity
        );
        assert!(should_retry_batch_error(
            "HTTP/1.1 429 Too Many Requests",
            0,
            2
        ));
        assert_eq!(
            classify_failure("HTTP/1.1 401 Unauthorized"),
            FailureClass::Authentication
        );
    }

    #[test]
    fn connection_capacity_precedes_generic_disconnect_for_retry_policy() {
        let error = "connection closed: too many connections";
        assert_eq!(classify_failure(error), FailureClass::Capacity);
        assert!(should_retry_batch_error(error, 0, 2));
        assert!(super::transient_retry_delay(error, 0) >= std::time::Duration::from_secs(30));
    }

    #[test]
    fn recent_engine_output_cannot_change_retry_class_or_backoff() {
        let error =
            "connection reset by peer; recent output: too many connections; rate limit exceeded";
        assert_eq!(classify_failure(error), FailureClass::Transport);
        assert!(should_retry_batch_error(error, 0, 2));
        assert!(transient_retry_delay(error, 0) < std::time::Duration::from_secs(20));

        let diagnostics_only =
            "Migration failed unexpectedly; recent output: HTTP/1.1 429 Too Many Requests";
        assert_eq!(classify_failure(diagnostics_only), FailureClass::Unknown);
        assert!(!should_retry_batch_error(diagnostics_only, 0, 2));
    }
}
