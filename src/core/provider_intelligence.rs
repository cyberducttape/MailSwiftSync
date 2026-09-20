use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Provider-specific error classification and recommended actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderErrorType {
    /// Rate limit hit; retry with exponential backoff
    RateLimited,
    /// Temporary outage; safe to retry immediately
    TemporarilyUnavailable,
    /// Connection failed; check network and endpoint
    ConnectivityError,
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
            Self::RateLimited | Self::TemporarilyUnavailable | Self::ConnectivityError
        )
    }

    /// Suggested delay before retry.
    pub fn suggested_retry_delay(&self) -> Option<Duration> {
        match self {
            Self::RateLimited => Some(Duration::from_secs(60)),
            Self::TemporarilyUnavailable => Some(Duration::from_secs(5)),
            Self::ConnectivityError => Some(Duration::from_secs(10)),
            _ => None,
        }
    }
}

/// Provider-specific throttling configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderThrottleConfig {
    /// Provider name (Gmail, O365, Fastmail, etc)
    pub provider: String,
    /// Messages per second limit
    pub messages_per_second: f64,
    /// Connection pool size
    pub connection_pool_size: usize,
    /// Recommended batch size
    pub batch_size: usize,
    /// Backoff multiplier for rate limits
    pub backoff_multiplier: f64,
    /// Maximum concurrent operations
    pub max_concurrent_ops: usize,
}

impl ProviderThrottleConfig {
    /// Gmail API rate limits: ~500 requests/user/second.
    /// Conservative estimate: 100 messages/sec for migration.
    pub fn gmail() -> Self {
        Self {
            provider: "Gmail".to_string(),
            messages_per_second: 100.0,
            connection_pool_size: 4,
            batch_size: 100,
            backoff_multiplier: 2.0,
            max_concurrent_ops: 4,
        }
    }

    /// Microsoft 365/Exchange Online: ~2000 requests/sec per tenant.
    /// Conservative: 150 messages/sec for migration.
    pub fn microsoft_365() -> Self {
        Self {
            provider: "Microsoft 365".to_string(),
            messages_per_second: 150.0,
            connection_pool_size: 6,
            batch_size: 200,
            backoff_multiplier: 2.0,
            max_concurrent_ops: 6,
        }
    }

    /// Fastmail: Standard IMAP with no aggressive rate limiting documented.
    /// Conservative: 50 messages/sec for safety.
    pub fn fastmail() -> Self {
        Self {
            provider: "Fastmail".to_string(),
            messages_per_second: 50.0,
            connection_pool_size: 2,
            batch_size: 50,
            backoff_multiplier: 1.5,
            max_concurrent_ops: 2,
        }
    }

    /// Generic IMAP provider: very conservative limits.
    pub fn generic_imap() -> Self {
        Self {
            provider: "Generic IMAP".to_string(),
            messages_per_second: 20.0,
            connection_pool_size: 1,
            batch_size: 25,
            backoff_multiplier: 1.5,
            max_concurrent_ops: 1,
        }
    }

    /// Get delay between messages to respect rate limit.
    pub fn delay_per_message(&self) -> Duration {
        let millis = (1000.0 / self.messages_per_second) as u64;
        Duration::from_millis(millis)
    }
}

/// Classify provider errors from error messages.
pub struct ProviderErrorClassifier;

impl ProviderErrorClassifier {
    /// Classify an error based on message content and provider context.
    pub fn classify(provider: &str, error_msg: &str) -> ProviderErrorType {
        let lower = error_msg.to_lowercase();

        // Rate limit patterns
        if lower.contains("rate limit")
            || lower.contains("quota exceeded")
            || lower.contains("too many requests")
            || lower.contains("throttled")
        {
            return ProviderErrorType::RateLimited;
        }

        // Authentication patterns
        if lower.contains("401") || lower.contains("unauthorized")
            || lower.contains("invalid credentials")
            || lower.contains("authentication failed")
        {
            return ProviderErrorType::AuthenticationFailed;
        }

        // Permission patterns
        if lower.contains("403") || lower.contains("forbidden")
            || lower.contains("permission denied")
            || lower.contains("insufficient privileges")
        {
            return ProviderErrorType::PermissionDenied;
        }

        // Not found patterns
        if lower.contains("404") || lower.contains("not found")
            || lower.contains("no such")
            || lower.contains("does not exist")
        {
            return ProviderErrorType::NotFound;
        }

        // Temporary unavailability
        if lower.contains("502") || lower.contains("503")
            || lower.contains("temporarily unavailable")
            || lower.contains("service unavailable")
            || lower.contains("bad gateway")
        {
            return ProviderErrorType::TemporarilyUnavailable;
        }

        // Connectivity patterns
        if lower.contains("connection refused")
            || lower.contains("connection timeout")
            || lower.contains("connection reset")
            || lower.contains("network unreachable")
        {
            return ProviderErrorType::ConnectivityError;
        }

        // Provider-specific unsupported patterns
        match provider.to_lowercase().as_str() {
            "gmail" => {
                if lower.contains("x-gm-raw") || lower.contains("gmail doesn't support") {
                    return ProviderErrorType::Unsupported;
                }
            }
            "microsoft" | "o365" | "office365" => {
                if lower.contains("not supported")
                    || lower.contains("operation not allowed")
                {
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
    fn gmail_throttle_config() {
        let config = ProviderThrottleConfig::gmail();
        assert_eq!(config.provider, "Gmail");
        assert!(config.messages_per_second > 50.0);
        assert_eq!(config.delay_per_message().as_millis(), 10);
    }

    #[test]
    fn o365_is_less_throttled_than_gmail() {
        let gmail = ProviderThrottleConfig::gmail();
        let o365 = ProviderThrottleConfig::microsoft_365();

        assert!(o365.messages_per_second > gmail.messages_per_second);
        assert!(
            o365.delay_per_message() < gmail.delay_per_message(),
            "O365 should have lower delay than Gmail"
        );
    }
}
