use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Observed IMAP/transport error classification and recommended actions.
///
/// This module does not encode provider API quotas or pretend that an IMAP
/// message-per-second value can be inferred from them. It remains a library
/// prototype until the controller consumes its classifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderErrorType {
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
    /// Determine if retry is safe for this error.
    #[allow(dead_code)]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited
                | Self::ConnectionCapacity
                | Self::TemporaryProviderFailure
                | Self::Network
        )
    }

    /// Suggested delay before retry.
    #[allow(dead_code)]
    pub fn suggested_retry_delay(&self) -> Option<Duration> {
        match self {
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

        // Quota failures are not rate limits. They require capacity/action,
        // not exponential backoff, so classify them before generic signals.
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

        // Observed server/protocol signals only. These are not provider API
        // quota estimates and must be paired with the engine's configured
        // message/byte limits by an active controller.
        if lower.contains("rate limit")
            || lower.contains("too many requests")
            || lower.contains("throttled")
            || lower.contains("slow down")
            || lower.contains(" 429")
        {
            return ProviderErrorType::RateLimited;
        }

        // Authentication patterns
        if lower.contains("401")
            || lower.contains("unauthorized")
            || lower.contains("invalid credentials")
            || lower.contains("authentication failed")
        {
            return ProviderErrorType::Authentication;
        }

        // Permission patterns
        if lower.contains("403")
            || lower.contains("forbidden")
            || lower.contains("permission denied")
            || lower.contains("insufficient privileges")
        {
            return ProviderErrorType::PermanentProviderFailure;
        }

        // Not found patterns
        if lower.contains("404")
            || lower.contains("not found")
            || lower.contains("no such")
            || lower.contains("does not exist")
        {
            return ProviderErrorType::PermanentProviderFailure;
        }

        // Temporary unavailability
        if lower.contains("502")
            || lower.contains("503")
            || lower.contains("temporarily unavailable")
            || lower.contains("service unavailable")
            || lower.contains("bad gateway")
            || lower.contains("server busy")
            || lower.contains("try again later")
        {
            return ProviderErrorType::TemporaryProviderFailure;
        }

        // Connectivity patterns
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

        if lower.contains("too many connections")
            || lower.contains("connection limit")
            || lower.contains("maximum connections")
        {
            return ProviderErrorType::ConnectionCapacity;
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
    }
}
