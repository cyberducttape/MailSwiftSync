#[cfg(test)]
use std::time::Duration;

use super::state::AttentionReason;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptionReason {
    /// User explicitly stopped the migration
    UserInitiated,
    /// Network connectivity lost
    NetworkTimeout,
    /// Provider rate limit or quota exceeded
    ProviderThrottled,
    /// Source or destination became unavailable
    EndpointUnavailable,
    /// Operator intervention (e.g., killed process)
    ProcessTerminated,
    /// Application crash or system shutdown
    CrashOrShutdown,
    /// Unknown cause
    Unknown,
}

impl InterruptionReason {
    /// Map only durable attention categories whose evidence supports a
    /// recovery checklist. Configuration and verification findings need their
    /// own remediation path and must not be presented as resumable outages.
    pub fn from_attention_reason(reason: AttentionReason) -> Option<Self> {
        Some(match reason {
            AttentionReason::Interrupted | AttentionReason::ProcessIdentityUnverified => {
                Self::ProcessTerminated
            }
            AttentionReason::TransportFailed => Self::NetworkTimeout,
            AttentionReason::CapacityLimited => Self::ProviderThrottled,
            _ => return None,
        })
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "user_initiated" => Self::UserInitiated,
            "network_timeout" => Self::NetworkTimeout,
            "provider_throttled" => Self::ProviderThrottled,
            "endpoint_unavailable" => Self::EndpointUnavailable,
            "process_terminated" => Self::ProcessTerminated,
            "crash_or_shutdown" => Self::CrashOrShutdown,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UserInitiated => "user_initiated",
            Self::NetworkTimeout => "network_timeout",
            Self::ProviderThrottled => "provider_throttled",
            Self::EndpointUnavailable => "endpoint_unavailable",
            Self::ProcessTerminated => "process_terminated",
            Self::CrashOrShutdown => "crash_or_shutdown",
            Self::Unknown => "unknown",
        }
    }
}

/// Recovery guidance generator.
pub struct RecoveryPlanner;

impl RecoveryPlanner {
    /// Generate recovery guidance based on interruption reason.
    pub fn generate_guidance(reason: InterruptionReason) -> Vec<&'static str> {
        match reason {
            InterruptionReason::UserInitiated => vec![
                "Migration was paused by operator",
                "Review any configuration changes since pause",
                "Click 'Resume' to continue from the last checkpoint",
            ],
            InterruptionReason::NetworkTimeout => vec![
                "Network connection to provider was lost",
                "Retry delay may reduce a transient failure, but does not prove recovery",
                "Revalidate DNS, TCP, TLS, authentication, and IMAP capability before resuming",
                "Check firewall rules and VPN if applicable",
                "Resume uses the engine's checkpoint semantics; reconcile aggregate evidence afterward",
            ],
            InterruptionReason::ProviderThrottled => vec![
                "The provider or IMAP server returned a throttling signal",
                "Wait for the configured retry delay; elapsed time does not prove that the limit has reset",
                "Revalidate DNS, TCP, TLS, authentication, and IMAP capability before resuming",
                "Resume with conservative profile message/byte limits and monitor for another server response",
            ],
            InterruptionReason::EndpointUnavailable => vec![
                "Source or destination mailbox became unavailable",
                "Verify the source mailbox is accessible and credentials are still valid",
                "Check destination mailbox quota and disk space",
                "If source is a shared mailbox, verify access permissions haven't changed",
                "Test connectivity with a manual IMAP connection before resuming",
            ],
            InterruptionReason::ProcessTerminated => vec![
                "Migration process was terminated (killed, system reboot, etc.)",
                "Controller state and completed engine evidence are durably stored; in-flight work requires reconciliation",
                "Review system logs to understand why termination occurred",
                "If termination was due to resource constraints, increase available memory or CPU",
                "Revalidate endpoints and review aggregate evidence before resuming from the engine checkpoint",
            ],
            InterruptionReason::CrashOrShutdown => vec![
                "Application or system crashed unexpectedly",
                "The durable ledger preserves recorded controller state; it does not prove the outcome of in-flight engine work",
                "Review application logs (support bundle) to diagnose the crash",
                "Ensure system has adequate disk space and memory available",
                "Revalidate DNS, TCP, TLS, authentication, and IMAP capability before resuming",
                "Review the engine checkpoint and aggregate evidence before attempting resume",
            ],
            InterruptionReason::Unknown => vec![
                "Interruption cause is unknown",
                "Review the support bundle for detailed diagnostic information",
                "Verify network connectivity, provider status, and endpoint accessibility",
                "Contact support if recovery fails after resume",
            ],
        }
    }

    /// Estimate time to resume based on messages remaining and provider throughput.
    #[cfg(test)]
    pub fn estimate_resume_duration(
        messages_remaining: u64,
        messages_per_second: f64,
    ) -> Option<Duration> {
        if !messages_per_second.is_finite() || messages_per_second <= 0.0 {
            return None;
        }
        let seconds = (messages_remaining as f64 / messages_per_second).ceil();
        if !seconds.is_finite() || seconds > u64::MAX as f64 {
            return None;
        }
        Some(Duration::from_secs(seconds as u64))
    }

    /// Determine whether the retry delay has elapsed.
    ///
    /// This function cannot prove that an endpoint or provider has recovered;
    /// callers must run the endpoint preflight before declaring resume safe.
    #[cfg(test)]
    pub fn is_safe_to_resume(
        reason: InterruptionReason,
        time_since_interruption: Duration,
    ) -> (bool, String) {
        match reason {
            InterruptionReason::ProviderThrottled => {
                if time_since_interruption < Duration::from_secs(300) {
                    (
                        false,
                        "Wait at least 5 minutes after rate limit before resuming".to_string(),
                    )
                } else {
                    (
                        false,
                        "Retry delay elapsed. Endpoint has not yet been revalidated.".to_string(),
                    )
                }
            }
            InterruptionReason::NetworkTimeout => {
                if time_since_interruption < Duration::from_secs(30) {
                    (false, "Wait a moment for network to stabilize".to_string())
                } else {
                    (
                        false,
                        "Retry delay elapsed. Endpoint has not yet been revalidated.".to_string(),
                    )
                }
            }
            _ => (
                false,
                "Resume requires current endpoint revalidation and checkpoint review.".to_string(),
            ),
        }
    }

    /// Return a positive resume decision only when the caller supplies a
    /// successful current endpoint revalidation result. The actual DNS/TCP/
    /// TLS/auth/IMAP probe belongs to the controller's endpoint probe path.
    #[cfg(test)]
    pub fn is_safe_to_resume_after_revalidation(
        reason: InterruptionReason,
        time_since_interruption: Duration,
        endpoint_revalidated: bool,
    ) -> (bool, String) {
        if !endpoint_revalidated {
            return (
                false,
                "Endpoint has not been revalidated; do not declare resume safe.".to_string(),
            );
        }
        let required_delay = match reason {
            InterruptionReason::ProviderThrottled => Duration::from_secs(300),
            InterruptionReason::NetworkTimeout => Duration::from_secs(30),
            _ => Duration::ZERO,
        };
        if time_since_interruption < required_delay {
            return (
                false,
                format!(
                    "Endpoint revalidated, but the retry delay has not elapsed ({:?} required).",
                    required_delay
                ),
            );
        }
        (
            true,
            "Endpoint revalidated and retry delay elapsed; resume may be attempted.".to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_guidance_for_user_initiated_interruption() {
        let guidance = RecoveryPlanner::generate_guidance(InterruptionReason::UserInitiated);
        assert!(!guidance.is_empty());
        assert!(guidance.iter().any(|g| g.contains("Resume")));
    }

    #[test]
    fn only_interruptions_with_recovery_evidence_map_to_guidance() {
        assert_eq!(
            InterruptionReason::from_attention_reason(AttentionReason::Interrupted),
            Some(InterruptionReason::ProcessTerminated)
        );
        assert_eq!(
            InterruptionReason::from_attention_reason(AttentionReason::TransportFailed),
            Some(InterruptionReason::NetworkTimeout)
        );
        assert_eq!(
            InterruptionReason::from_attention_reason(AttentionReason::CapacityLimited),
            Some(InterruptionReason::ProviderThrottled)
        );
        assert_eq!(
            InterruptionReason::from_attention_reason(AttentionReason::ConfigurationInvalid),
            None
        );
        assert_eq!(
            InterruptionReason::from_attention_reason(AttentionReason::VerificationDifference),
            None
        );
    }

    #[test]
    fn network_timeout_suggests_waiting() {
        let guidance = RecoveryPlanner::generate_guidance(InterruptionReason::NetworkTimeout);
        assert!(
            guidance
                .iter()
                .any(|g| g.to_lowercase().contains("network"))
        );
        assert!(
            guidance
                .iter()
                .any(|g| g.to_lowercase().contains("revalidate"))
        );
    }

    #[test]
    fn throttle_guidance_recommends_wait_period() {
        let guidance = RecoveryPlanner::generate_guidance(InterruptionReason::ProviderThrottled);
        assert!(guidance.iter().any(|g| g.to_lowercase().contains("wait")));
        assert!(
            guidance
                .iter()
                .any(|g| g.to_lowercase().contains("revalidate"))
        );
    }

    #[test]
    fn not_safe_to_resume_immediately_after_throttle() {
        let (safe, message) = RecoveryPlanner::is_safe_to_resume(
            InterruptionReason::ProviderThrottled,
            Duration::from_secs(60),
        );
        assert!(!safe);
        assert!(message.contains("5 minutes"));
    }

    #[test]
    fn safe_to_resume_after_throttle_wait() {
        let (safe, _) = RecoveryPlanner::is_safe_to_resume(
            InterruptionReason::ProviderThrottled,
            Duration::from_secs(600),
        );
        assert!(!safe);
        let (safe, message) = RecoveryPlanner::is_safe_to_resume_after_revalidation(
            InterruptionReason::ProviderThrottled,
            Duration::from_secs(600),
            true,
        );
        assert!(safe);
        assert!(message.contains("revalidated"));
    }

    #[test]
    fn estimates_resume_duration() {
        let duration = RecoveryPlanner::estimate_resume_duration(1000, 100.0);
        assert_eq!(duration, Some(Duration::from_secs(10)));

        let duration = RecoveryPlanner::estimate_resume_duration(5000, 50.0);
        assert_eq!(duration, Some(Duration::from_secs(100)));
    }

    #[test]
    fn invalid_throughput_has_no_duration_estimate() {
        for throughput in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                RecoveryPlanner::estimate_resume_duration(1000, throughput),
                None
            );
        }
    }

    #[test]
    fn crash_guidance_mentions_durability() {
        let guidance = RecoveryPlanner::generate_guidance(InterruptionReason::CrashOrShutdown);
        assert!(
            guidance
                .iter()
                .any(|g| g.to_lowercase().contains("durable"))
        );
        assert!(
            guidance
                .iter()
                .any(|g| g.to_lowercase().contains("preserves"))
        );
        assert!(guidance.iter().any(|g| g.contains("in-flight")));
    }

    #[test]
    fn elapsed_delay_does_not_claim_endpoint_recovery() {
        let (safe, message) = RecoveryPlanner::is_safe_to_resume(
            InterruptionReason::NetworkTimeout,
            Duration::from_secs(60),
        );
        assert!(!safe);
        assert!(message.contains("not yet been revalidated"));
    }
}
