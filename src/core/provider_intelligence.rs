use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Observed IMAP/transport error classification and recommended actions.
///
/// This module does not encode provider API quotas or pretend that an IMAP
/// message-per-second value can be inferred from them. It remains a library
/// prototype until the controller consumes its classifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderErrorType {
    /// Rate limit hit; retry with exponential backoff
    RateLimited,
    /// Temporary outage; safe to retry immediately
    TemporarilyUnavailable,
    /// Connection failed; check network and endpoint
    ConnectivityError,
    /// Provider rejected a connection or operation because of capacity.
    ConnectionLimited,
    /// Invalid credentials or expired token
    AuthenticationFailed,
    /// Account doesn't have required permissions
    PermissionDenied,
    /// Mailbox or folder doesn't exist
    NotFound,
    /// Requested action is not supported by provider
    Unsupported,
    /// Unclassified error
    Unknown,
}

impl ProviderErrorType {
    /// Determine if retry is safe for this error.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited
                | Self::TemporarilyUnavailable
                | Self::ConnectivityError
                | Self::ConnectionLimited
        )
    }

    /// Suggested delay before retry.
    pub fn suggested_retry_delay(&self) -> Option<Duration> {
        match self {
            Self::RateLimited => Some(Duration::from_secs(60)),
            Self::TemporarilyUnavailable => Some(Duration::from_secs(5)),
            Self::ConnectivityError => Some(Duration::from_secs(10)),
            Self::ConnectionLimited => Some(Duration::from_secs(30)),
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

        // Observed server/protocol signals only. These are not provider API
        // quota estimates and must be paired with the engine's configured
        // message/byte limits by an active controller.
        if lower.contains("rate limit")
            || lower.contains("quota exceeded")
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
            return ProviderErrorType::AuthenticationFailed;
        }

        // Permission patterns
        if lower.contains("403")
            || lower.contains("forbidden")
            || lower.contains("permission denied")
            || lower.contains("insufficient privileges")
        {
            return ProviderErrorType::PermissionDenied;
        }

        // Not found patterns
        if lower.contains("404")
            || lower.contains("not found")
            || lower.contains("no such")
            || lower.contains("does not exist")
        {
            return ProviderErrorType::NotFound;
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
            return ProviderErrorType::TemporarilyUnavailable;
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
            return ProviderErrorType::ConnectivityError;
        }

        if lower.contains("too many connections")
            || lower.contains("connection limit")
            || lower.contains("maximum connections")
        {
            return ProviderErrorType::ConnectionLimited;
        }

        // Provider-specific unsupported patterns
        match provider.to_lowercase().as_str() {
            "gmail" => {
                if lower.contains("x-gm-raw") || lower.contains("gmail doesn't support") {
                    return ProviderErrorType::Unsupported;
                }
            }
            "microsoft" | "o365" | "office365" => {
                if lower.contains("not supported") || lower.contains("operation not allowed") {
                    return ProviderErrorType::Unsupported;
                }
            }
            _ => {}
        }

        ProviderErrorType::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_rate_limit_errors() {
        let error = ProviderErrorClassifier::classify("gmail", "rate limit exceeded");
        assert_eq!(error, ProviderErrorType::RateLimited);

        let error = ProviderErrorClassifier::classify("o365", "quota exceeded");
        assert_eq!(error, ProviderErrorType::RateLimited);
    }

    #[test]
    fn classifies_auth_errors() {
        let error = ProviderErrorClassifier::classify("gmail", "401 Unauthorized");
        assert_eq!(error, ProviderErrorType::AuthenticationFailed);

        let error = ProviderErrorClassifier::classify("fastmail", "invalid credentials");
        assert_eq!(error, ProviderErrorType::AuthenticationFailed);
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
            ProviderErrorType::ConnectionLimited
        );
        assert_eq!(
            ProviderErrorClassifier::classify("generic", "* BYE server busy"),
            ProviderErrorType::TemporarilyUnavailable
        );
        assert_eq!(
            ProviderErrorClassifier::classify("generic", "connection closed by server"),
            ProviderErrorType::ConnectivityError
        );
    }
}
