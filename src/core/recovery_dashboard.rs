use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Recovery state after an interrupted migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryState {
    pub job_id: String,
    pub run_id: String,
    pub interrupted_at: String,
    pub last_successful_uid: Option<String>,
    pub messages_processed: u64,
    pub messages_failed: u64,
    pub reason: InterruptionReason,
    pub recovery_guidance: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
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

/// Recovery dashboard for the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryDashboard {
    pub has_interrupted_jobs: bool,
    pub interrupted_jobs: Vec<RecoveryJobSummary>,
    pub total_messages_to_resume: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryJobSummary {
    pub job_id: String,
    pub mailbox: String,
    pub interrupted_at: String,
    pub messages_processed: u64,
    pub messages_remaining_estimate: u64,
    pub reason: String,
    pub recommended_action: String,
}

/// Recovery guidance generator.
pub struct RecoveryPlanner;

impl RecoveryPlanner {
    /// Generate recovery guidance based on interruption reason.
    pub fn generate_guidance(reason: InterruptionReason) -> Vec<String> {
        match reason {
            InterruptionReason::UserInitiated => vec![
                "Migration was paused by operator".to_string(),
                "Review any configuration changes since pause".to_string(),
                "Click 'Resume' to continue from the last checkpoint".to_string(),
            ],
            InterruptionReason::NetworkTimeout => vec![
                "Network connection to provider was lost".to_string(),
                "Verify network connectivity and provider status".to_string(),
                "Check firewall rules and VPN if applicable".to_string(),
                "Provider may be experiencing intermittent issues; wait a few minutes and retry".to_string(),
                "Resume will retry from the last successful message".to_string(),
            ],
            InterruptionReason::ProviderThrottled => vec![
                "Provider rate limit was exceeded".to_string(),
                "Migration has been automatically backed off per provider limits".to_string(),
                "This is normal for large migrations; the system will retry automatically".to_string(),
                "Recommended action: wait 5-10 minutes before resuming to allow provider quota to reset".to_string(),
                "Resume will continue at a reduced pace to respect provider limits".to_string(),
            ],
            InterruptionReason::EndpointUnavailable => vec![
                "Source or destination mailbox became unavailable".to_string(),
                "Verify the source mailbox is accessible and credentials are still valid".to_string(),
                "Check destination mailbox quota and disk space".to_string(),
                "If source is a shared mailbox, verify access permissions haven't changed".to_string(),
                "Test connectivity with a manual IMAP connection before resuming".to_string(),
            ],
            InterruptionReason::ProcessTerminated => vec![
                "Migration process was terminated (killed, system reboot, etc.)".to_string(),
                "No data was lost; all progress is durably stored in the migration log".to_string(),
                "Review system logs to understand why termination occurred".to_string(),
                "If termination was due to resource constraints, increase available memory or CPU".to_string(),
                "Resume will pick up from the last verified message".to_string(),
            ],
            InterruptionReason::CrashOrShutdown => vec![
                "Application or system crashed unexpectedly".to_string(),
                "All progress is durably stored in the migration database".to_string(),
                "Review application logs (support bundle) to diagnose the crash".to_string(),
                "Ensure system has adequate disk space and memory available".to_string(),
                "Verify provider endpoints are still accessible".to_string(),
                "Resume will resume from the last checkpoint automatically".to_string(),
            ],
            InterruptionReason::Unknown => vec![
                "Interruption cause is unknown".to_string(),
                "Review the support bundle for detailed diagnostic information".to_string(),
                "Verify network connectivity, provider status, and endpoint accessibility".to_string(),
                "Contact support if recovery fails after resume".to_string(),
            ],
        }
    }

    /// Estimate time to resume based on messages remaining and provider throughput.
    pub fn estimate_resume_duration(messages_remaining: u64, messages_per_second: f64) -> Duration {
        let seconds = (messages_remaining as f64 / messages_per_second).ceil() as u64;
        Duration::from_secs(seconds)
    }

    /// Determine if resume is safe or if operator should wait.
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
                        true,
                        "Rate limit should have reset; safe to resume".to_string(),
                    )
                }
            }
            InterruptionReason::NetworkTimeout => {
                if time_since_interruption < Duration::from_secs(30) {
                    (false, "Wait a moment for network to stabilize".to_string())
                } else {
                    (
                        true,
                        "Network appears to be recovered; safe to resume".to_string(),
                    )
                }
            }
            _ => (true, "Safe to resume".to_string()),
        }
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
    fn network_timeout_suggests_waiting() {
        let guidance = RecoveryPlanner::generate_guidance(InterruptionReason::NetworkTimeout);
        assert!(guidance.iter().any(|g| g.contains("network")));
        assert!(guidance.iter().any(|g| g.contains("connectivity")));
    }

    #[test]
    fn throttle_guidance_recommends_wait_period() {
        let guidance = RecoveryPlanner::generate_guidance(InterruptionReason::ProviderThrottled);
        assert!(guidance.iter().any(|g| g.contains("wait")));
        assert!(guidance.iter().any(|g| g.contains("quota")));
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
        assert!(safe);
    }

    #[test]
    fn estimates_resume_duration() {
        let duration = RecoveryPlanner::estimate_resume_duration(1000, 100.0);
        assert_eq!(duration, Duration::from_secs(10));

        let duration = RecoveryPlanner::estimate_resume_duration(5000, 50.0);
        assert_eq!(duration, Duration::from_secs(100));
    }

    #[test]
    fn crash_guidance_mentions_durability() {
        let guidance = RecoveryPlanner::generate_guidance(InterruptionReason::CrashOrShutdown);
        assert!(guidance.iter().any(|g| g.contains("durably")));
        assert!(guidance.iter().any(|g| g.contains("stored")));
    }
}
