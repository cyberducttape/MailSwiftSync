use serde::{Deserialize, Serialize};

/// Post-migration exception report for operator review and remediation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostMigrationReport {
    pub total_processed: u64,
    pub total_skipped: u64,
    pub total_failed: u64,
    pub exceptions: Vec<MigrationException>,
    pub remediation_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationException {
    pub id: String,
    pub severity: ExceptionSeverity,
    pub category: String,
    pub message: String,
    pub affected_folder: Option<String>,
    pub affected_message_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExceptionSeverity {
    Info,
    Warning,
    Critical,
}

impl PostMigrationReport {
    /// Generate post-migration report from verification results.
    /// Detects: missing messages, extra messages, content mismatches,
    /// folder discrepancies, and other operational concerns.
    pub fn generate(
        total_processed: u64,
        total_skipped: u64,
        total_failed: u64,
        missing_count: u64,
        extra_count: u64,
        changed_count: u64,
    ) -> Self {
        let mut exceptions = Vec::new();
        let mut remediation_steps = Vec::new();

        // Missing messages
        if missing_count > 0 {
            exceptions.push(MigrationException {
                id: format!("missing-{}", uuid::Uuid::new_v4()),
                severity: ExceptionSeverity::Warning,
                category: "missing_messages".to_string(),
                message: format!("{} message(s) present in source but missing from destination", missing_count),
                affected_folder: None,
                affected_message_id: None,
            });

            remediation_steps.push(
                "Review missing messages report: identify if exclusions were applied".to_string()
            );
            remediation_steps.push(
                "Run selective re-sync for missing messages if required".to_string()
            );
        }

        // Extra messages
        if extra_count > 0 {
            exceptions.push(MigrationException {
                id: format!("extra-{}", uuid::Uuid::new_v4()),
                severity: ExceptionSeverity::Info,
                category: "extra_messages".to_string(),
                message: format!("{} extra message(s) in destination (may be post-migration additions)", extra_count),
                affected_folder: None,
                affected_message_id: None,
            });

            remediation_steps.push(
                "Verify extra messages are post-migration additions (expected behavior)".to_string()
            );
        }

        // Changed messages
        if changed_count > 0 {
            exceptions.push(MigrationException {
                id: format!("changed-{}", uuid::Uuid::new_v4()),
                severity: ExceptionSeverity::Warning,
                category: "content_mismatch".to_string(),
                message: format!("{} message(s) with size/content mismatch", changed_count),
                affected_folder: None,
                affected_message_id: None,
            });

            remediation_steps.push(
                "Review message mismatches: verify no data corruption occurred".to_string()
            );
            remediation_steps.push(
                "Check provider-specific content modifications (e.g., Gmail header rewrites)".to_string()
            );
        }

        // Skipped count
        if total_skipped > 0 {
            exceptions.push(MigrationException {
                id: format!("skipped-{}", uuid::Uuid::new_v4()),
                severity: ExceptionSeverity::Info,
                category: "skipped".to_string(),
                message: format!("{} message(s) skipped (excluded by rules or already present)", total_skipped),
                affected_folder: None,
                affected_message_id: None,
            });
        }

        PostMigrationReport {
            total_processed,
            total_skipped,
            total_failed,
            exceptions,
            remediation_steps,
        }
    }

    pub fn has_critical_issues(&self) -> bool {
        self.exceptions
            .iter()
            .any(|e| e.severity == ExceptionSeverity::Critical)
    }

    pub fn is_successful(&self) -> bool {
        self.total_failed == 0 && !self.has_critical_issues() && self.exceptions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_migration_succeeds() {
        let report = PostMigrationReport::generate(1000, 0, 0, 0, 0, 0);
        assert!(report.is_successful());
        assert_eq!(report.exceptions.len(), 0);
    }

    #[test]
    fn missing_messages_create_warning() {
        let report = PostMigrationReport::generate(1000, 0, 0, 5, 0, 0);
        assert!(!report.is_successful());
        let has_missing = report.exceptions.iter().any(|e| e.category == "missing_messages");
        assert!(has_missing);
        assert!(report.remediation_steps.len() > 0);
    }
}
