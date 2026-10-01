use serde::{Deserialize, Serialize};

use super::ProjectReportSnapshot;

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
    /// Build a post-migration report from one consistent durable project
    /// snapshot. Counts come only from persisted verification evidence; an
    /// absent or active result is surfaced as an exception instead of being
    /// treated as a clean migration.
    pub(crate) fn from_project_snapshot(snapshot: &ProjectReportSnapshot) -> Self {
        let mut total_processed: u64 = 0;
        let mut total_failed: u64 = 0;
        let mut missing_count: u64 = 0;
        let mut extra_count: u64 = 0;
        let mut changed_count: u64 = 0;
        let mut report = Self::generate(0, 0, 0, 0, 0, 0);

        for mailbox in &snapshot.mailboxes {
            if let Some((_, evidence, _)) = &mailbox.evidence {
                total_processed = total_processed.saturating_add(evidence.source_messages);
                total_failed = total_failed.saturating_add(evidence.failed_messages);
                missing_count = missing_count.saturating_add(evidence.missing_messages);
                extra_count = extra_count.saturating_add(evidence.extra_messages);
                changed_count = changed_count.saturating_add(evidence.modified_messages);
            } else {
                report.exceptions.push(MigrationException {
                    id: format!("incomplete-evidence-{}", uuid::Uuid::new_v4()),
                    severity: ExceptionSeverity::Critical,
                    category: "incomplete_evidence".to_string(),
                    message: format!(
                        "Mailbox {} has no durable verification evidence",
                        mailbox.job.source_mailbox
                    ),
                    affected_folder: None,
                    affected_message_id: None,
                });
                report.remediation_steps.push(format!(
                    "Complete and verify the migration for mailbox {} before accepting the project",
                    mailbox.job.source_mailbox
                ));
            }
        }

        report.total_processed = total_processed;
        report.total_failed = total_failed;
        let generated = Self::generate(
            0,
            0,
            total_failed,
            missing_count,
            extra_count,
            changed_count,
        );
        report.exceptions.extend(generated.exceptions);
        report.remediation_steps.extend(generated.remediation_steps);

        // The ledger does not persist a message-level skipped count. Keep the
        // exported zero explicitly qualified instead of presenting it as an
        // observed fact.
        report.exceptions.push(MigrationException {
            id: format!("skipped-count-unavailable-{}", uuid::Uuid::new_v4()),
            severity: ExceptionSeverity::Warning,
            category: "skipped_count_unavailable".to_string(),
            message: "The durable ledger does not record a message-level skipped count".to_string(),
            affected_folder: None,
            affected_message_id: None,
        });
        report.remediation_steps.push(
            "Use the engine output and mailbox-specific audit report to review exclusions or already-present messages".to_string(),
        );

        if snapshot.has_active_runs {
            report.exceptions.push(MigrationException {
                id: format!("active-run-{}", uuid::Uuid::new_v4()),
                severity: ExceptionSeverity::Critical,
                category: "active_run".to_string(),
                message: "The project still has an active migration run".to_string(),
                affected_folder: None,
                affected_message_id: None,
            });
            report.remediation_steps.push(
                "Wait for active runs to finish before accepting the post-migration report"
                    .to_string(),
            );
        }

        report
    }

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
                message: format!(
                    "{} message(s) present in source but missing from destination",
                    missing_count
                ),
                affected_folder: None,
                affected_message_id: None,
            });

            remediation_steps.push(
                "Review missing messages report: identify if exclusions were applied".to_string(),
            );
            remediation_steps
                .push("Run selective re-sync for missing messages if required".to_string());
        }

        // Extra messages
        if extra_count > 0 {
            exceptions.push(MigrationException {
                id: format!("extra-{}", uuid::Uuid::new_v4()),
                severity: ExceptionSeverity::Info,
                category: "extra_messages".to_string(),
                message: format!(
                    "{} extra message(s) in destination (may be post-migration additions)",
                    extra_count
                ),
                affected_folder: None,
                affected_message_id: None,
            });

            remediation_steps.push(
                "Verify extra messages are post-migration additions (expected behavior)"
                    .to_string(),
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

            remediation_steps
                .push("Review message mismatches: verify no data corruption occurred".to_string());
            remediation_steps.push(
                "Check provider-specific content modifications (e.g., Gmail header rewrites)"
                    .to_string(),
            );
        }

        // Skipped count
        if total_skipped > 0 {
            exceptions.push(MigrationException {
                id: format!("skipped-{}", uuid::Uuid::new_v4()),
                severity: ExceptionSeverity::Info,
                category: "skipped".to_string(),
                message: format!(
                    "{} message(s) skipped (excluded by rules or already present)",
                    total_skipped
                ),
                affected_folder: None,
                affected_message_id: None,
            });
        }

        // Failed messages
        if total_failed > 0 {
            exceptions.push(MigrationException {
                id: format!("failed-{}", uuid::Uuid::new_v4()),
                severity: ExceptionSeverity::Critical,
                category: "failed_messages".to_string(),
                message: format!("{} message(s) failed during migration", total_failed),
                affected_folder: None,
                affected_message_id: None,
            });
            remediation_steps.push(
                "Review failed-message diagnostics and retry only after the underlying cause is understood".to_string(),
            );
        }

        PostMigrationReport {
            total_processed,
            total_skipped,
            total_failed,
            exceptions,
            remediation_steps,
        }
    }

    #[allow(dead_code)]
    pub fn has_critical_issues(&self) -> bool {
        self.exceptions
            .iter()
            .any(|e| e.severity == ExceptionSeverity::Critical)
    }

    #[allow(dead_code)]
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
        let has_missing = report
            .exceptions
            .iter()
            .any(|e| e.category == "missing_messages");
        assert!(has_missing);
        assert!(!report.remediation_steps.is_empty());
    }

    #[test]
    fn failed_messages_create_critical_exception() {
        let report = PostMigrationReport::generate(1000, 0, 2, 0, 0, 0);
        assert!(report.has_critical_issues());
        assert!(
            report
                .exceptions
                .iter()
                .any(|exception| exception.category == "failed_messages")
        );
    }
}
