use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Observed IMAP/transport error classification and recommended actions.
///
/// This module does not encode provider API quotas or pretend that an IMAP
/// message-per-second value can be inferred from them. Its classifications
/// are consumed by controller retry policy; provider-specific quota and
/// throttle telemetry remains an explicit qualification concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderErrorType {
    /// A structured transport-layer failure emitted by an adapter.
    TransportError,
    /// TLS negotiation or certificate validation failed.
    TlsError,
    /// DNS or name resolution failed.
    DnsError,
    /// IMAP returned a tagged NO response.
    ImapTaggedNo,
    /// IMAP returned a tagged BAD response.
    ImapBad,
    /// Credentials expired and must be refreshed or replaced.
    AuthenticationExpired,
    /// The migration engine exited unsuccessfully.
    EngineExit,
    /// Verification produced an unusable or contradictory result.
    VerificationError,
    /// Provider rate limit hit; retry with exponential backoff.
    RateLimited,
    /// Provider rejected a connection because of concurrent capacity.
    ConnectionCapacity,
    /// The mailbox or its storage quota is exhausted.
    MailboxQuotaExceeded,
    /// A host or service storage quota is exhausted.
    StorageQuotaExceeded,
    /// Credentials are invalid or expired.
    Authentication,
    /// Provider is temporarily unavailable and may recover.
    TemporaryProviderFailure,
    /// Provider rejected the operation for a non-transient reason.
    PermanentProviderFailure,
    /// Network connectivity failed before a provider response was received.
    Network,
}

impl ProviderErrorType {
    /// Determine if retry is safe for this error. The batch controller's
    /// retry policy defers to this for every Transport-class verdict.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::TransportError
                | Self::RateLimited
                | Self::ConnectionCapacity
                | Self::TemporaryProviderFailure
                | Self::Network
        )
    }

    /// Suggested delay before retry.
    #[allow(dead_code)]
    pub fn suggested_retry_delay(&self) -> Option<Duration> {
        match self {
            Self::TransportError => Some(Duration::from_secs(10)),
            Self::RateLimited => Some(Duration::from_secs(60)),
            Self::ConnectionCapacity => Some(Duration::from_secs(30)),
            Self::TemporaryProviderFailure => Some(Duration::from_secs(5)),
            Self::Network => Some(Duration::from_secs(10)),
            _ => None,
        }
    }
}

/// Classify provider errors from error messages.
pub struct ProviderErrorClassifier;

impl ProviderErrorClassifier {
    /// Classify an error based on message content and provider context.
    pub fn classify(provider: &str, error_msg: &str) -> ProviderErrorType {
        let lower = error_msg.to_lowercase();
        let http_401 = has_http_status_line(error_msg, 401);
        let http_403 = has_http_status_line(error_msg, 403);
        let http_404 = has_http_status_line(error_msg, 404);
        let http_429 = has_http_status_line(error_msg, 429);
        let http_502 = has_http_status_line(error_msg, 502);
        let http_503 = has_http_status_line(error_msg, 503);

        // Adapters should emit these stable tags at the source. Keep the
        // textual classifier as a compatibility fallback for legacy engine
        // and IMAP diagnostics.
        if lower.contains("[error=tls]") {
            return ProviderErrorType::TlsError;
        }
        if lower.contains("[error=dns]") {
            return ProviderErrorType::DnsError;
        }
        if lower.contains("[error=transport]") {
            return ProviderErrorType::TransportError;
        }
        if lower.contains("[imap=tagged-no]") {
            return ProviderErrorType::ImapTaggedNo;
        }
        if lower.contains("[imap=bad]") {
            return ProviderErrorType::ImapBad;
        }
        if lower.contains("[auth=expired]") {
            return ProviderErrorType::AuthenticationExpired;
        }
        if lower.contains("[error=engine-exit]") {
            return ProviderErrorType::EngineExit;
        }
        if lower.contains("[error=verification]") {
            return ProviderErrorType::VerificationError;
        }

        // Specific capacity conditions take precedence over generic
        // disconnect wording, and over other broad classifications that may
        // also appear in a diagnostic tail.
        if lower.contains("too many connections")
            || lower.contains("connection limit")
            || lower.contains("maximum connections")
        {
            return ProviderErrorType::ConnectionCapacity;
        }

        // Observed server/protocol signals only. These are not provider API
        // quota estimates and must be paired with the engine's configured
        // message/byte limits by an active controller.
        if lower.contains("rate limit")
            || lower.contains("too many requests")
            || lower.contains("throttled")
            || lower.contains("slow down")
            || http_429
        {
            return ProviderErrorType::RateLimited;
        }

        if lower.contains("storage quota")
            || lower.contains("disk quota")
            || lower.contains("storage full")
            || lower.contains("disk full")
        {
            return ProviderErrorType::StorageQuotaExceeded;
        }
        if lower.contains("mailbox quota")
            || lower.contains("mailbox full")
            || lower.contains("over quota")
            || lower.contains("quota exceeded")
        {
            return ProviderErrorType::MailboxQuotaExceeded;
        }

        // Authentication patterns
        if http_401
            || lower.contains("unauthorized")
            || lower.contains("invalid credentials")
            || lower.contains("authentication failed")
        {
            return ProviderErrorType::Authentication;
        }

        // Permission patterns
        if http_403
            || lower.contains("forbidden")
            || lower.contains("permission denied")
            || lower.contains("insufficient privileges")
        {
            return ProviderErrorType::PermanentProviderFailure;
        }

        // Not found patterns
        if http_404
            || lower.contains("not found")
            || lower.contains("no such")
            || lower.contains("does not exist")
        {
            return ProviderErrorType::PermanentProviderFailure;
        }

        // Temporary unavailability
        if http_502
            || http_503
            || lower.contains("temporarily unavailable")
            || lower.contains("service unavailable")
            || lower.contains("bad gateway")
            || lower.contains("server busy")
            || lower.contains("try again later")
        {
            return ProviderErrorType::TemporaryProviderFailure;
        }

        // Generic connectivity patterns are deliberately last among the
        // actionable transport/capacity conditions.
        if lower.contains("connection refused")
            || lower.contains("connection timeout")
            || lower.contains("connection reset")
            || lower.contains("network unreachable")
            || lower.contains("connection closed")
            || lower.contains("server bye")
            || lower == "bye"
        {
            return ProviderErrorType::Network;
        }

        // Provider-specific unsupported patterns
        match provider.to_lowercase().as_str() {
            "gmail" => {
                if lower.contains("x-gm-raw") || lower.contains("gmail doesn't support") {
                    return ProviderErrorType::PermanentProviderFailure;
                }
            }
            "microsoft" | "o365" | "office365" => {
                if lower.contains("not supported") || lower.contains("operation not allowed") {
                    return ProviderErrorType::PermanentProviderFailure;
                }
            }
            _ => {}
        }

        ProviderErrorType::PermanentProviderFailure
    }
}

/// Recognize a numeric status only when it is presented as an HTTP status
/// line, never when the digits merely occur in engine output (for example,
/// "429 messages copied"). A bare status line must also carry its standard
/// reason phrase; a versioned `HTTP/...` line is itself sufficient protocol
/// context.
fn has_http_status_line(error: &str, expected: u16) -> bool {
    let reason = match expected {
        401 => "unauthorized",
        403 => "forbidden",
        404 => "not found",
        429 => "too many requests",
        502 => "bad gateway",
        503 => "service unavailable",
        _ => return false,
    };
    error.lines().any(|line| {
        let line = line.trim();
        let versioned = line
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("HTTP/"));
        let status_and_reason = if versioned {
            let Some((_, status)) = take_protocol_token(&line[5..]) else {
                return false;
            };
            status
        } else {
            line
        };
        let Some((status, trailing)) = take_protocol_token(status_and_reason) else {
            return false;
        };
        if status.parse::<u16>().ok() != Some(expected) {
            return false;
        }
        // A versioned status line is unambiguous even when the reason phrase
        // is omitted (permitted by HTTP). Bare numeric lines need the known
        // phrase so message/count diagnostics cannot masquerade as statuses.
        if versioned {
            true
        } else {
            trailing
                .get(..reason.len())
                .is_some_and(|value| value.eq_ignore_ascii_case(reason))
                && trailing
                    .get(reason.len()..)
                    .is_none_or(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
        }
    })
}

fn take_protocol_token(value: &str) -> Option<(&str, &str)> {
    let value = value.trim_start();
    match value.find(char::is_whitespace) {
        Some(split) => Some((&value[..split], value[split..].trim_start())),
        None if !value.is_empty() => Some((value, "")),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_rate_limit_errors() {
        let error = ProviderErrorClassifier::classify("gmail", "rate limit exceeded");
        assert_eq!(error, ProviderErrorType::RateLimited);

        let error = ProviderErrorClassifier::classify("o365", "too many requests");
        assert_eq!(error, ProviderErrorType::RateLimited);
    }

    #[test]
    fn quota_failures_are_not_rate_limits() {
        assert_eq!(
            ProviderErrorClassifier::classify("o365", "destination mailbox quota exceeded"),
            ProviderErrorType::MailboxQuotaExceeded
        );
        assert_eq!(
            ProviderErrorClassifier::classify("generic", "storage quota exceeded"),
            ProviderErrorType::StorageQuotaExceeded
        );
        assert!(!ProviderErrorType::MailboxQuotaExceeded.is_retryable());
        assert!(!ProviderErrorType::StorageQuotaExceeded.is_retryable());
    }

    #[test]
    fn classifies_auth_errors() {
        let error = ProviderErrorClassifier::classify("gmail", "401 Unauthorized");
        assert_eq!(error, ProviderErrorType::Authentication);

        let error = ProviderErrorClassifier::classify("fastmail", "invalid credentials");
        assert_eq!(error, ProviderErrorType::Authentication);
    }

    #[test]
    fn rate_limited_is_retryable() {
        assert!(ProviderErrorType::RateLimited.is_retryable());
        assert_eq!(
            ProviderErrorType::RateLimited.suggested_retry_delay(),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn classifies_observed_imap_capacity_signals() {
        assert_eq!(
            ProviderErrorClassifier::classify("generic", "too many connections"),
            ProviderErrorType::ConnectionCapacity
        );
        assert_eq!(
            ProviderErrorClassifier::classify("generic", "* BYE server busy"),
            ProviderErrorType::TemporaryProviderFailure
        );
        assert_eq!(
            ProviderErrorClassifier::classify("generic", "connection closed by server"),
            ProviderErrorType::Network
        );
        for error in [
            "connection closed: too many connections",
            "connection refused; connection limit reached",
            "connection reset after maximum connections exceeded",
            "rate limit observed while too many connections are open",
            "mailbox quota reported; too many connections",
        ] {
            assert_eq!(
                ProviderErrorClassifier::classify("generic", error),
                ProviderErrorType::ConnectionCapacity,
                "diagnostic: {error}"
            );
        }
    }

    #[test]
    fn structured_adapter_signals_precede_text_heuristics() {
        assert_eq!(
            ProviderErrorClassifier::classify("gmail", "[error=tls] server busy"),
            ProviderErrorType::TlsError
        );
        assert_eq!(
            ProviderErrorClassifier::classify("o365", "[auth=expired] authentication failed"),
            ProviderErrorType::AuthenticationExpired
        );
        assert_eq!(
            ProviderErrorClassifier::classify("generic", "[imap=bad] invalid command"),
            ProviderErrorType::ImapBad
        );
    }

    #[test]
    fn numeric_message_counts_do_not_masquerade_as_http_statuses() {
        for diagnostic in [
            "Migration failed unexpectedly\n429 messages copied before termination",
            "folder contains 401 messages",
            "processed 403 items",
        ] {
            assert_eq!(
                ProviderErrorClassifier::classify("generic", diagnostic),
                ProviderErrorType::PermanentProviderFailure,
                "diagnostic: {diagnostic}"
            );
        }
    }

    #[test]
    fn numeric_statuses_require_http_line_context_or_a_status_reason_phrase() {
        assert_eq!(
            ProviderErrorClassifier::classify("generic", "HTTP/1.1 429 Too Many Requests"),
            ProviderErrorType::RateLimited
        );
        assert_eq!(
            ProviderErrorClassifier::classify("generic", "401 Unauthorized"),
            ProviderErrorType::Authentication
        );
        assert_eq!(
            ProviderErrorClassifier::classify("generic", "HTTP/2 503"),
            ProviderErrorType::TemporaryProviderFailure
        );
    }
}
