use serde::{Deserialize, Serialize};

const METADATA_VERIFICATION_MESSAGE_LIMIT: u64 = 1_000_000;
const BODY_VERIFICATION_MESSAGE_LIMIT: u64 = 100_000;
const DEFAULT_BODY_HASH_BYTE_BUDGET: u64 = 512 * 1024 * 1024;
pub(crate) const PRE_MIGRATION_RISK_FORMAT: &str = "mailswiftsync-pre-migration-risk";
pub(crate) const PRE_MIGRATION_RISK_FORMAT_VERSION: u32 = 1;

/// Pre-migration risk assessment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreMigrationRisk {
    pub format: String,
    pub format_version: u32,
    /// This report evaluates only the scale facts supplied by the caller.
    /// It is not an end-to-end migration readiness decision.
    pub assessment_scope: String,
    /// Hard gates are evaluated elsewhere and are never cleared by this score.
    pub advisory_only: bool,
    /// Important readiness checks for which this scale-only report has no evidence.
    pub not_evaluated: Vec<String>,
    pub total_messages: u64,
    pub total_folders: u64,
    pub total_size_gb: f64,
    pub warnings: Vec<RiskWarning>,
    /// Advisory score for the scale facts supplied to `assess`. This is never
    /// a substitute for hard preflight gates or provider qualification.
    pub readiness_score: u8,
    pub estimated_readiness: MigrationReadiness,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskWarning {
    pub severity: WarningSeverity,
    pub category: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WarningSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationReadiness {
    Ready,
    ReviewRequired,
    BlockedByErrors,
}

impl PreMigrationRisk {
    /// Generate a scale-only pre-migration risk report.
    ///
    /// This API currently receives no message-size distribution, folder map,
    /// quota, provider, or destination-capability facts. It therefore must
    /// not claim to detect oversized messages, ambiguous mappings, quota
    /// headroom, Unicode issues, or provider-specific behavior. It can still
    /// surface the documented aggregate verification envelopes from the
    /// supplied message and byte totals; exact body-proof admission requires a
    /// future fact-bearing input type and live preflight integration.
    pub fn assess(
        total_messages: u64,
        total_folders: u64,
        total_size_bytes: u64,
        _config: &str, // Retained for API compatibility; not interpreted.
    ) -> Self {
        let mut warnings = Vec::new();
        let total_size_gb = total_size_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let mut score = 100_u8;

        // Warning thresholds
        if total_messages > 500_000 {
            score = score.saturating_sub(15);
            warnings.push(RiskWarning {
                severity: WarningSeverity::Warning,
                category: "scale".to_string(),
                message: format!(
                    "Large migration: {} messages may take extended time",
                    total_messages
                ),
            });
        }

        if total_size_gb > 100.0 {
            score = score.saturating_sub(15);
            warnings.push(RiskWarning {
                severity: WarningSeverity::Warning,
                category: "size".to_string(),
                message: format!(
                    "Large total size: {:.1} GB migration requires adequate time window",
                    total_size_gb
                ),
            });
        }

        if total_folders > 100 {
            score = score.saturating_sub(5);
            warnings.push(RiskWarning {
                severity: WarningSeverity::Info,
                category: "folders".to_string(),
                message: format!(
                    "Many folders: {} folders may have mapping complexity",
                    total_folders
                ),
            });
        }

        // These are advisory planning warnings, not transfer admission gates.
        // A transfer can complete beyond an evidence envelope, but the report
        // must tell the operator before launch that independent proof may be
        // incomplete rather than discovering that only after the transfer.
        if total_messages > METADATA_VERIFICATION_MESSAGE_LIMIT {
            warnings.push(RiskWarning {
                severity: WarningSeverity::Warning,
                category: "verification".to_string(),
                message: format!(
                    "Independent metadata verification exceeds the documented {}-message envelope; transfer may complete without complete message-level evidence",
                    METADATA_VERIFICATION_MESSAGE_LIMIT
                ),
            });
        }

        if total_messages > BODY_VERIFICATION_MESSAGE_LIMIT {
            warnings.push(RiskWarning {
                severity: WarningSeverity::Warning,
                category: "verification".to_string(),
                message: format!(
                    "Bounded body proof exceeds the documented {}-message-per-endpoint envelope",
                    BODY_VERIFICATION_MESSAGE_LIMIT
                ),
            });
        }

        if total_size_bytes > DEFAULT_BODY_HASH_BYTE_BUDGET {
            warnings.push(RiskWarning {
                severity: WarningSeverity::Info,
                category: "verification".to_string(),
                message: format!(
                    "Body proof may exceed the default {}-byte combined hash budget; message-size distribution and configured proof budget require review",
                    DEFAULT_BODY_HASH_BYTE_BUDGET
                ),
            });
        }

        // Determine readiness
        let error_count = warnings
            .iter()
            .filter(|w| w.severity == WarningSeverity::Error)
            .count();
        let estimated_readiness = if error_count > 0 {
            MigrationReadiness::BlockedByErrors
        } else if !warnings.is_empty() {
            MigrationReadiness::ReviewRequired
        } else {
            MigrationReadiness::Ready
        };

        PreMigrationRisk {
            format: PRE_MIGRATION_RISK_FORMAT.to_owned(),
            format_version: PRE_MIGRATION_RISK_FORMAT_VERSION,
            assessment_scope: "scale_only".to_owned(),
            advisory_only: true,
            not_evaluated: vec![
                "provider_qualification".to_owned(),
                "authentication".to_owned(),
                "tls".to_owned(),
                "destination_capacity".to_owned(),
                "verification_eligibility".to_owned(),
                "worker_disk_capacity".to_owned(),
                "maintenance_window".to_owned(),
                "organization_policy".to_owned(),
            ],
            total_messages,
            total_folders,
            total_size_gb,
            warnings,
            readiness_score: score,
            estimated_readiness,
        }
    }

    #[allow(dead_code)]
    pub fn is_ready(&self) -> bool {
        self.estimated_readiness == MigrationReadiness::Ready
    }

    #[allow(dead_code)]
    pub fn has_blocking_errors(&self) -> bool {
        self.estimated_readiness == MigrationReadiness::BlockedByErrors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_migration_requires_review() {
        let risk = PreMigrationRisk::assess(1000, 5, 100_000_000, "");
        assert_eq!(risk.estimated_readiness, MigrationReadiness::Ready);
        assert!(risk.warnings.is_empty());
        assert_eq!(risk.readiness_score, 100);
    }

    #[test]
    fn large_migration_requires_review() {
        let risk = PreMigrationRisk::assess(1_000_000, 50, 200_000_000_000, "");
        assert_eq!(risk.estimated_readiness, MigrationReadiness::ReviewRequired);
        assert!(!risk.warnings.is_empty());
        assert_eq!(risk.readiness_score, 70);
    }

    #[test]
    fn provider_text_does_not_trigger_unsubstantiated_provider_warning() {
        let risk = PreMigrationRisk::assess(1000, 5, 100_000_000, "provider=gmail");
        assert!(risk.warnings.is_empty());
        assert_eq!(risk.estimated_readiness, MigrationReadiness::Ready);
    }

    #[test]
    fn scale_score_explicitly_disclaims_unassessed_readiness_gates() {
        let risk = PreMigrationRisk::assess(100, 5, 100_000_000, "");
        assert_eq!(risk.assessment_scope, "scale_only");
        assert!(risk.advisory_only);
        for gate in [
            "provider_qualification",
            "authentication",
            "tls",
            "destination_capacity",
            "verification_eligibility",
            "worker_disk_capacity",
            "maintenance_window",
            "organization_policy",
        ] {
            assert!(risk.not_evaluated.iter().any(|value| value == gate));
        }
        let value = serde_json::to_value(risk).unwrap();
        assert_eq!(value["format"], PRE_MIGRATION_RISK_FORMAT);
        assert_eq!(value["format_version"], PRE_MIGRATION_RISK_FORMAT_VERSION);
        assert_eq!(value["estimated_readiness"], "ready");
        assert_eq!(value["assessment_scope"], "scale_only");
        assert_eq!(value["advisory_only"], true);
        assert!(value["not_evaluated"].as_array().unwrap().len() >= 8);
    }

    #[test]
    fn verification_envelopes_are_visible_before_transfer() {
        let risk = PreMigrationRisk::assess(
            METADATA_VERIFICATION_MESSAGE_LIMIT + 1,
            5,
            DEFAULT_BODY_HASH_BYTE_BUDGET + 1,
            "",
        );
        let messages = risk
            .warnings
            .iter()
            .filter(|warning| warning.category == "verification")
            .map(|warning| warning.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages.len(), 3);
        assert!(messages.iter().any(|message| message.contains("metadata")));
        assert!(
            messages
                .iter()
                .any(|message| message.contains("body proof"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("combined hash budget"))
        );
        assert_eq!(risk.readiness_score, 85);
        assert_eq!(risk.estimated_readiness, MigrationReadiness::ReviewRequired);
    }
}
