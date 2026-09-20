use serde::{Deserialize, Serialize};

/// Pre-migration risk assessment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreMigrationRisk {
    pub total_messages: u64,
    pub total_folders: u64,
    pub total_size_gb: f64,
    pub warnings: Vec<RiskWarning>,
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
    /// headroom, Unicode issues, or provider-specific behavior. Those checks
    /// require a future fact-bearing input type and live preflight integration.
    pub fn assess(
        total_messages: u64,
        total_folders: u64,
        total_size_bytes: u64,
        _config: &str, // Retained for API compatibility; not interpreted.
    ) -> Self {
        let mut warnings = Vec::new();
        let total_size_gb = total_size_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

        // Warning thresholds
        if total_messages > 500_000 {
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
            warnings.push(RiskWarning {
                severity: WarningSeverity::Info,
                category: "folders".to_string(),
                message: format!(
                    "Many folders: {} folders may have mapping complexity",
                    total_folders
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
            total_messages,
            total_folders,
            total_size_gb,
            warnings,
            estimated_readiness,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.estimated_readiness == MigrationReadiness::Ready
    }

    pub fn has_blocking_errors(&self) -> bool {
        self.estimated_readiness == MigrationReadiness::BlockedByErrors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_migration_requires_review() {
        let risk = PreMigrationRisk::assess(1000, 5, 1_000_000_000, "");
        assert_eq!(risk.estimated_readiness, MigrationReadiness::Ready);
        assert!(risk.warnings.is_empty());
    }

    #[test]
    fn large_migration_requires_review() {
        let risk = PreMigrationRisk::assess(1_000_000, 50, 200_000_000_000, "");
        assert_eq!(risk.estimated_readiness, MigrationReadiness::ReviewRequired);
        assert!(!risk.warnings.is_empty());
    }

    #[test]
    fn provider_text_does_not_trigger_unsubstantiated_provider_warning() {
        let risk = PreMigrationRisk::assess(1000, 5, 1_000_000_000, "provider=gmail");
        assert!(risk.warnings.is_empty());
        assert_eq!(risk.estimated_readiness, MigrationReadiness::Ready);
    }
}
