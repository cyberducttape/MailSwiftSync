use serde::{Deserialize, Serialize};

const METADATA_VERIFICATION_MESSAGE_LIMIT: u64 = 1_000_000;
const BODY_VERIFICATION_MESSAGE_LIMIT: u64 = 1_000_000;
const DEFAULT_BODY_HASH_BYTE_BUDGET: u64 = 512 * 1024 * 1024;
pub(crate) const PRE_MIGRATION_RISK_FORMAT: &str = "mailswiftsync-pre-migration-risk";
pub(crate) const PRE_MIGRATION_RISK_FORMAT_VERSION: u32 = 4;
const PROJECTED_ENDPOINTS: u64 = 2;
/// Planning-only allowance based on observed per-row stage-plus-journal use
/// in the synthetic 1M-message profiles. This is not a proven maximum:
/// unusually large metadata and destination extras can increase actual use.
const PROJECTED_STAGE_BYTES_PER_MESSAGE: u64 = 1024;

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
    /// Input size is for one source-endpoint inventory; destination size is
    /// not measured by this scale-only API.
    pub size_input_scope: String,
    pub total_size_gb: f64,
    /// Advisory equal-size source + destination projection. `None` means the
    /// sum cannot be represented as a u64.
    pub estimated_combined_body_hash_bytes: Option<u64>,
    pub body_hash_byte_projection: String,
    /// Advisory SQLite metadata-stage plus journal projection. It assumes an
    /// equal message count at both endpoints and uses a planning allowance,
    /// not a qualified upper bound. `None` means arithmetic overflow.
    pub estimated_verification_stage_and_journal_bytes: Option<u64>,
    pub verification_stage_projection_basis: String,
    /// Always `unknown_not_measured` for this scale-only API: it has neither a
    /// destination inventory nor a live filesystem-space observation.
    pub verification_stage_capacity_status: String,
    pub warnings: Vec<RiskWarning>,
    /// Advisory score for the scale facts supplied to `assess`. This is never
    /// a substitute for hard preflight gates or provider qualification.
    pub readiness_score: u8,
    /// Result of the scale-only analysis. `ScaleClear` does not imply that
    /// the migration is ready to run; it means only that supplied scale facts
    /// triggered no warning.
    pub scale_assessment: ScaleAssessment,
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
pub enum ScaleAssessment {
    ScaleClear,
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
                    "Bounded body proof exceeds the durable SQLite-backed {}-message-per-endpoint envelope",
                    BODY_VERIFICATION_MESSAGE_LIMIT
                ),
            });
        }

        let estimated_combined_body_hash_bytes = total_size_bytes.checked_mul(2);
        let estimated_verification_stage_and_journal_bytes = total_messages
            .checked_mul(PROJECTED_ENDPOINTS)
            .and_then(|messages| messages.checked_mul(PROJECTED_STAGE_BYTES_PER_MESSAGE));
        if estimated_combined_body_hash_bytes
            .is_none_or(|combined| combined > DEFAULT_BODY_HASH_BYTE_BUDGET)
        {
            warnings.push(RiskWarning {
                severity: WarningSeverity::Info,
                category: "verification".to_string(),
                message: match estimated_combined_body_hash_bytes {
                    Some(combined) => format!(
                        "Equal-size source/destination projection is {combined} bytes, above the default {DEFAULT_BODY_HASH_BYTE_BUDGET}-byte combined body-hash budget; actual destination size and fetched RFC822 bytes are unknown, so review the measured proof budget"
                    ),
                    None => format!(
                        "Equal-size source/destination projection cannot be represented; review actual endpoint sizes against the default {DEFAULT_BODY_HASH_BYTE_BUDGET}-byte combined body-hash budget"
                    ),
                },
            });
        }

        // Determine readiness
        let error_count = warnings
            .iter()
            .filter(|w| w.severity == WarningSeverity::Error)
            .count();
        let scale_assessment = if error_count > 0 {
            ScaleAssessment::BlockedByErrors
        } else if !warnings.is_empty() {
            ScaleAssessment::ReviewRequired
        } else {
            ScaleAssessment::ScaleClear
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
            size_input_scope: "source_endpoint_inventory".to_owned(),
            total_size_gb,
            estimated_combined_body_hash_bytes,
            body_hash_byte_projection: "source_bytes_plus_equal_size_destination_estimate"
                .to_owned(),
            estimated_verification_stage_and_journal_bytes,
            verification_stage_projection_basis: format!(
                "source_messages × {PROJECTED_ENDPOINTS} endpoints (equal-count assumption) × {PROJECTED_STAGE_BYTES_PER_MESSAGE} bytes/message; unqualified planning projection, not a safe upper bound"
            ),
            verification_stage_capacity_status: "unknown_not_measured".to_owned(),
            warnings,
            readiness_score: score,
            scale_assessment,
        }
    }

    #[allow(dead_code)]
    pub fn is_scale_clear(&self) -> bool {
        self.scale_assessment == ScaleAssessment::ScaleClear
    }

    #[allow(dead_code)]
    pub fn has_blocking_errors(&self) -> bool {
        self.scale_assessment == ScaleAssessment::BlockedByErrors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_migration_requires_review() {
        let risk = PreMigrationRisk::assess(1000, 5, 100_000_000, "");
        assert_eq!(risk.scale_assessment, ScaleAssessment::ScaleClear);
        assert!(risk.warnings.is_empty());
        assert_eq!(risk.readiness_score, 100);
    }

    #[test]
    fn large_migration_requires_review() {
        let risk = PreMigrationRisk::assess(1_000_000, 50, 200_000_000_000, "");
        assert_eq!(risk.scale_assessment, ScaleAssessment::ReviewRequired);
        assert!(!risk.warnings.is_empty());
        assert_eq!(risk.readiness_score, 70);
    }

    #[test]
    fn provider_text_does_not_trigger_unsubstantiated_provider_warning() {
        let risk = PreMigrationRisk::assess(1000, 5, 100_000_000, "provider=gmail");
        assert!(risk.warnings.is_empty());
        assert_eq!(risk.scale_assessment, ScaleAssessment::ScaleClear);
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
        assert_eq!(value["scale_assessment"], "scale_clear");
        assert!(value.get("estimated_readiness").is_none());
        assert_eq!(value["assessment_scope"], "scale_only");
        assert_eq!(value["advisory_only"], true);
        assert!(value["not_evaluated"].as_array().unwrap().len() >= 8);
    }

    #[test]
    fn verification_envelopes_are_visible_before_transfer() {
        let risk = PreMigrationRisk::assess(
            METADATA_VERIFICATION_MESSAGE_LIMIT + 1,
            5,
            DEFAULT_BODY_HASH_BYTE_BUDGET / 2 + 1,
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
                .any(|message| { message.contains("1000000-message-per-endpoint") })
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("Equal-size source/destination projection"))
        );
        assert_eq!(
            risk.estimated_combined_body_hash_bytes,
            Some(DEFAULT_BODY_HASH_BYTE_BUDGET + 2)
        );
        assert_eq!(risk.readiness_score, 85);
        assert_eq!(risk.scale_assessment, ScaleAssessment::ReviewRequired);
    }

    #[test]
    fn body_hash_scale_warning_uses_durable_stage_limit_not_ephemeral_limit() {
        let below_durable_limit =
            PreMigrationRisk::assess(BODY_VERIFICATION_MESSAGE_LIMIT, 5, 1, "");
        assert!(
            !below_durable_limit
                .warnings
                .iter()
                .any(|warning| { warning.message.contains("Bounded body proof exceeds") })
        );

        let above_durable_limit =
            PreMigrationRisk::assess(BODY_VERIFICATION_MESSAGE_LIMIT + 1, 5, 1, "");
        assert!(above_durable_limit.warnings.iter().any(|warning| {
            warning.message.contains(
                "Bounded body proof exceeds the durable SQLite-backed 1000000-message-per-endpoint envelope",
            )
        }));
    }

    #[test]
    fn body_hash_projection_uses_both_endpoints_and_handles_overflow() {
        let at_budget = PreMigrationRisk::assess(100, 5, DEFAULT_BODY_HASH_BYTE_BUDGET / 2, "");
        assert_eq!(
            at_budget.estimated_combined_body_hash_bytes,
            Some(DEFAULT_BODY_HASH_BYTE_BUDGET)
        );
        assert!(!at_budget.warnings.iter().any(|warning| {
            warning
                .message
                .contains("equal-size source/destination projection")
        }));

        let overflow = PreMigrationRisk::assess(100, 5, u64::MAX, "");
        assert_eq!(overflow.estimated_combined_body_hash_bytes, None);
        assert!(
            overflow
                .warnings
                .iter()
                .any(|warning| { warning.message.contains("projection cannot be represented") })
        );
    }

    #[test]
    fn stage_projection_is_explicitly_advisory_and_capacity_remains_unknown() {
        let one_million = PreMigrationRisk::assess(1_000_000, 50, 200_000_000_000, "");
        assert_eq!(
            one_million.estimated_verification_stage_and_journal_bytes,
            Some(2_048_000_000)
        );
        assert!(
            one_million
                .verification_stage_projection_basis
                .contains("equal-count assumption")
        );
        assert!(
            one_million
                .verification_stage_projection_basis
                .contains("not a safe upper bound")
        );
        assert_eq!(
            one_million.verification_stage_capacity_status,
            "unknown_not_measured"
        );
        assert!(
            one_million
                .not_evaluated
                .iter()
                .any(|gate| gate == "worker_disk_capacity")
        );

        let overflow = PreMigrationRisk::assess(u64::MAX, 1, 1, "");
        assert_eq!(
            overflow.estimated_verification_stage_and_journal_bytes,
            None
        );
        assert_eq!(
            overflow.verification_stage_capacity_status,
            "unknown_not_measured"
        );
    }
}
