//! Durable migration-control-plane primitives.
//!
//! The GUI may be replaced, but project state and verification evidence remain
//! portable SQLite data. No credentials or message content belong in this store.

use crate::storage::bounded_event_detail;
#[cfg(test)]
use crate::storage::{
    EVENT_DETAIL_TRUNCATION_SUFFIX as DURABLE_EVENT_TRUNCATION_SUFFIX,
    MAX_EVENT_DETAIL_BYTES as MAX_DURABLE_EVENT_DETAIL_BYTES,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use crc32fast::Hasher as Crc32Hasher;
use rusqlite::{Connection, OpenFlags, OptionalExtension, backup, params};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
};
use uuid::Uuid;

mod capabilities;
mod database;
mod engine;
mod events;
mod evidence;
mod evidence_ops;
mod message_extraction;
mod message_verification;
mod pre_migration_report;
mod post_migration_report;
mod mailboxes;
mod models;
mod phases;
mod policy;
mod projects;
mod queries;
mod recovery;
mod reports;
mod run_queries;
mod runs;
mod state;
pub use capabilities::ServerCapabilities;
pub use engine::Engine;
pub use evidence::{
    EvidenceScope, MailboxEvidence, ProjectReportSnapshot, ReportMailboxSnapshot,
    ReportRunSnapshot, VerificationAcceptance,
};
pub use message_extraction::{DovecotMessageExtractor, ExtractedMessage, ImapsyncMessageExtractor};
pub use message_verification::{MessageMismatch, MessageVerification, MismatchType, VerificationSummary};
pub use pre_migration_report::{MigrationReadiness, PreMigrationRisk, RiskWarning, WarningSeverity};
pub use post_migration_report::{ExceptionSeverity, MigrationException, PostMigrationReport};
pub use models::{
    ActiveProcess, BatchAdmissionState, BatchChildPlan, MailboxJob, MailboxStateCounts, Project,
    ProjectListItem, RunListItem, RunSummary,
};
pub use state::{AttentionReason, MailboxState, Phase};

pub const CURRENT_SCHEMA_VERSION: i64 = 7;

pub(crate) use policy::{
    attention_reason_for, normalized_destination_identity, valid_dovecot_checkpoint,
    valid_mailbox_transition,
};

pub struct StateStore {
    connection: Connection,
}

impl StateStore {
    #[cfg(test)]
    fn insert_run_for_test(
        &self,
        project_id: &str,
        job_id: Option<&str>,
        run_id: &str,
        engine: &str,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO runs(id,project_id,job_id,engine,phase_at_start,plan_snapshot,status) VALUES(?1,?2,?3,?4,'discovery','','running')",
            params![run_id, project_id, job_id, engine],
        )?;
        Ok(())
    }
    #[cfg(test)]
    fn set_mailbox_state_for_test(&self, job_id: &str, state: &str) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE mailbox_jobs SET state=?1 WHERE id=?2",
            params![state, job_id],
        )?;
        Ok(())
    }
}

fn migration_backup_path(path: &Path, schema_version: i64) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.db");
    path.with_file_name(format!(
        "{file_name}.pre-migrate-v{schema_version}.{}.db",
        Uuid::new_v4()
    ))
}

/// Ensure SQLite's first file creation happens with owner-only permissions.
/// The later chmod calls still repair existing databases and sidecars, but
/// this removes the initial permissive-umask window for a new ledger.
fn prepare_database_file(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let result = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .map(drop)
        }
        #[cfg(not(unix))]
        {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map(drop)
        }
    };
    match result {
        Ok(()) => crate::credentials::restrict_file_permissions(path),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn restrict_database_permissions(path: &Path) -> std::io::Result<()> {
    crate::credentials::restrict_file_permissions(path)
}

fn restrict_database_sidecars(path: &Path) -> std::io::Result<()> {
    for suffix in ["-wal", "-shm"] {
        let sidecar = Path::new(&format!("{}{}", path.display(), suffix)).to_owned();
        if sidecar.exists() {
            restrict_database_permissions(&sidecar)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_state_wire_values_and_policy_are_centralized() {
        let states = [
            MailboxState::Queued,
            MailboxState::Preflight,
            MailboxState::Ready,
            MailboxState::Running,
            MailboxState::Completed,
            MailboxState::Verified,
            MailboxState::VerifiedWithExceptions,
            MailboxState::Failed,
            MailboxState::Cancelled,
            MailboxState::Attention,
            MailboxState::DeltaRequired,
            MailboxState::VerificationDifference,
        ];
        for state in states {
            assert_eq!(MailboxState::parse(state.as_str()), Some(state));
        }
        assert!(MailboxState::Verified.is_verified());
        assert!(MailboxState::VerifiedWithExceptions.is_verified());
        assert!(MailboxState::VerificationDifference.needs_operator_review());
        assert!(!MailboxState::Ready.needs_operator_review());
        assert_eq!(MailboxState::parse("unknown"), None);
    }

    #[test]
    fn capability_parser_selects_modern_strategy() {
        let caps = ServerCapabilities::parse(
            "* CAPABILITY IMAP4rev1 UIDPLUS CONDSTORE QRESYNC SPECIAL-USE\r\na1 OK",
        );
        assert!(caps.supports("qresync"));
        assert!(caps.detected_capabilities().contains(&"QRESYNC advertised"));
        assert!(!caps.inventory_complete);
    }

    #[test]
    fn capability_parser_requires_and_counts_authenticated_inventory() {
        let caps = ServerCapabilities::parse_with_inventory(
            "* CAPABILITY IMAP4rev1 SPECIAL-USE\r\na1 OK",
            "* LIST (\\HasNoChildren \\iNbOx) \"/\" \"INBOX\"\r\n* LIST (\\HasNoChildren \\aRcHiVe) \"/\" \"Archive\"\r\n* LIST (\\HasNoChildren \\fLaGgEd) \"/\" \"Flagged\"\r\n* LIST (\\HasNoChildren) \"/\" \"\\Sent\"\r\na2 OK LIST completed",
        );
        assert!(caps.inventory_complete);
        assert_eq!(caps.mailbox_count, 4);
        assert_eq!(caps.special_use_mailboxes, 2);
    }

    #[test]
    fn capability_parser_does_not_treat_tagged_list_ok_as_inventory() {
        let caps = ServerCapabilities::parse_with_inventory(
            "* CAPABILITY IMAP4rev1\r\na1 OK",
            "a2 OK LIST completed",
        );
        assert!(!caps.inventory_complete);
        assert_eq!(caps.mailbox_count, 0);
    }

    #[test]
    fn quota_parser_distinguishes_unknown_from_exhausted_capacity() {
        let mut caps = ServerCapabilities::parse_with_inventory(
            "* CAPABILITY IMAP4rev1 QUOTA\r\na1 OK",
            "* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\na2 OK",
        );
        caps.record_quota_response("* QUOTAROOT \"\" \"\"\r\na3 OK");
        assert!(!caps.quota_observed);
        assert!(!caps.quota_exceeded);

        caps.record_quota_response(
            "* QUOTA \"\" (STORAGE 100 100 MESSAGE 5 10)\r\na4 OK GETQUOTAROOT completed",
        );
        assert!(caps.quota_observed);
        assert!(caps.quota_exceeded);
    }

    #[test]
    fn quota_parser_does_not_treat_zero_limit_as_exhausted() {
        let mut caps = ServerCapabilities::parse_with_inventory(
            "* CAPABILITY IMAP4rev1 QUOTA\r\na1 OK",
            "* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\na2 OK",
        );
        caps.record_quota_response("* QUOTA \"\" (STORAGE 999 0 MESSAGE 999 0)");
        assert!(caps.quota_observed);
        assert!(!caps.quota_exceeded);
    }

    #[test]
    fn quota_parser_keeps_malformed_usage_unknown() {
        let mut caps = ServerCapabilities::parse_with_inventory(
            "* CAPABILITY IMAP4rev1 QUOTA\r\na1 OK",
            "* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\na2 OK",
        );
        caps.record_quota_response("* QUOTA \"\" (STORAGE unknown 100)");
        assert!(!caps.quota_observed);
        assert!(!caps.quota_exceeded);
    }
    #[test]
    fn evidence_is_durable_and_explainable() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("pilot", "old.example", "new.example")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "a@example", "a@example")
            .unwrap();
        let e = MailboxEvidence {
            source_messages: 8,
            destination_messages: 8,
            source_bytes: 10,
            destination_bytes: 10,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };
        db.record_evidence(&job, &e).unwrap();
        assert_eq!(
            db.evidence(&job).unwrap().unwrap().confidence_percent(),
            100
        );
        assert_eq!(
            db.evidence(&job).unwrap().unwrap().evidence_scope(),
            EvidenceScope::EngineConfirmed
        );
        db.transition(&project.id, Phase::Preflight).unwrap();
        db.transition(&project.id, Phase::Verification).unwrap();
        assert_eq!(
            db.project(&project.id).unwrap().unwrap().phase,
            Phase::Verification
        );
    }

    #[test]
    fn project_and_mailbox_creation_are_recorded_in_the_audit_ledger() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("audit", "old.example", "new.example")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source@example", "destination@example")
            .unwrap();

        let events = db
            .connection
            .prepare("SELECT kind, detail FROM events WHERE project_id=?1 ORDER BY id")
            .unwrap()
            .query_map([&project.id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "project_created");
        assert_eq!(events[1].0, "mailbox_added");
        assert!(events[1].1.contains("destination@example"));
        assert_eq!(db.mailbox_state(&job).unwrap().as_deref(), Some("queued"));
    }

    #[test]
    fn batch_admission_state_is_read_once_and_returned_in_request_order() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("batch", "old.example", "new.example")
            .unwrap();
        let first = db
            .add_mailbox(&project.id, "first-source", "first-destination")
            .unwrap();
        let second = db
            .add_mailbox(&project.id, "second-source", "second-destination")
            .unwrap();
        db.connection
            .execute(
                "UPDATE mailbox_jobs SET state='ready', preflight_plan='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', checkpoint='checkpoint-2' WHERE id=?1",
                [&second],
            )
            .unwrap();
        db.connection
            .execute(
                "UPDATE mailbox_jobs SET state='failed', preflight_plan='bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' WHERE id=?1",
                [&first],
            )
            .unwrap();

        let rows = db
            .batch_admission_states(&project.id, &[second.clone(), first.clone()])
            .unwrap();
        assert_eq!(rows[0].job_id, second);
        assert_eq!(rows[0].state, "ready");
        assert_eq!(rows[0].checkpoint.as_deref(), Some("checkpoint-2"));
        assert_eq!(rows[1].job_id, first);
        assert_eq!(rows[1].state, "failed");
        assert_eq!(rows[1].checkpoint, None);
        assert!(
            db.batch_admission_states(&project.id, &["missing".into()])
                .is_err()
        );
    }

    #[test]
    fn batch_admission_state_handles_more_ids_than_one_sqlite_query_chunk() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("large batch", "old.example", "new.example")
            .unwrap();
        let mut job_ids = Vec::with_capacity(501);
        for index in 0..501 {
            job_ids.push(
                db.add_mailbox(
                    &project.id,
                    &format!("source-{index}@example.com"),
                    &format!("destination-{index}@example.com"),
                )
                .unwrap(),
            );
        }

        let rows = db.batch_admission_states(&project.id, &job_ids).unwrap();
        assert_eq!(rows.len(), job_ids.len());
        assert_eq!(
            rows.first().map(|row| row.job_id.as_str()),
            job_ids.first().map(String::as_str)
        );
        assert_eq!(
            rows.last().map(|row| row.job_id.as_str()),
            job_ids.last().map(String::as_str)
        );
    }

    #[test]
    fn dovecot_checkpoint_commits_with_terminal_mailbox_state() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("checkpoint", "old.example", "new.example")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source@example", "destination@example")
            .unwrap();
        db.begin_run(&project.id, &job, "checkpoint-run", "dovecot")
            .unwrap();

        db.finish_run_for_mailbox_with_checkpoint(
            &project.id,
            &job,
            "checkpoint-run",
            "completed",
            "completed",
            "",
            Some("AQAAAHm4+Jk="),
        )
        .unwrap();

        assert_eq!(
            db.mailbox_checkpoint(&job).unwrap().as_deref(),
            Some("AQAAAHm4+Jk=")
        );
        assert_eq!(
            db.run_status("checkpoint-run").unwrap().as_deref(),
            Some("completed")
        );
    }

    #[test]
    fn malformed_dovecot_checkpoint_is_rejected_before_terminal_commit() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("checkpoint-validation", "old.example", "new.example")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source@example", "destination@example")
            .unwrap();
        db.begin_run(&project.id, &job, "checkpoint-invalid", "dovecot")
            .unwrap();

        assert!(
            db.finish_run_for_mailbox_with_checkpoint(
                &project.id,
                &job,
                "checkpoint-invalid",
                "completed",
                "completed",
                "",
                Some("state with whitespace"),
            )
            .is_err()
        );
        assert_eq!(
            db.run_status("checkpoint-invalid").unwrap().as_deref(),
            Some("running")
        );
        assert_eq!(db.mailbox_checkpoint(&job).unwrap(), None);
    }

    #[test]
    fn failed_run_cannot_replace_previous_dovecot_checkpoint() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("checkpoint-failure", "old.example", "new.example")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source@example", "destination@example")
            .unwrap();
        db.begin_run(&project.id, &job, "checkpoint-failure-run", "dovecot")
            .unwrap();

        assert!(
            db.finish_run_for_mailbox_with_checkpoint(
                &project.id,
                &job,
                "checkpoint-failure-run",
                "failed",
                "failed",
                "verification failed",
                Some("AQAAAHm4+Jk="),
            )
            .is_err()
        );
        assert_eq!(
            db.run_status("checkpoint-failure-run").unwrap().as_deref(),
            Some("running")
        );
        assert_eq!(db.mailbox_checkpoint(&job).unwrap(), None);
    }

    #[test]
    fn folder_mismatch_cannot_claim_full_confidence() {
        let evidence = MailboxEvidence {
            source_messages: 10,
            destination_messages: 10,
            source_bytes: 100,
            destination_bytes: 100,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 4,
            destination_folders: 3,
            authoritative: false,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };
        assert_eq!(evidence.confidence_percent(), 0);
        assert_eq!(evidence.evidence_level(), "Aggregate mismatch");
        assert!(!evidence.is_exact_match());
    }

    #[test]
    fn empty_mailbox_with_failures_is_not_verified() {
        let evidence = MailboxEvidence {
            source_messages: 0,
            destination_messages: 0,
            source_bytes: 0,
            destination_bytes: 0,
            unmatched_messages: 0,
            failed_messages: 1,
            source_folders: 1,
            destination_folders: 1,
            authoritative: false,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };
        assert_eq!(evidence.confidence_percent(), 0);
        assert_eq!(evidence.evidence_level(), "Incomplete evidence");
    }

    #[test]
    fn aggregate_match_has_bounded_but_nonmisleading_score() {
        let evidence = MailboxEvidence {
            source_messages: 12,
            destination_messages: 12,
            source_bytes: 100,
            destination_bytes: 100,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 2,
            destination_folders: 2,
            authoritative: false,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };
        assert_eq!(evidence.confidence_percent(), 85);
        assert!(evidence.is_exact_match());
        assert_eq!(evidence.evidence_level(), "Aggregate match");
        assert_eq!(
            evidence.evidence_scope(),
            EvidenceScope::AggregateReconciled
        );
        assert_eq!(evidence.evidence_scope().label(), "aggregate-reconciled");
    }

    #[test]
    fn mailbox_state_machine_allows_retry_but_rejects_backwards_moves() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        assert!(db.set_mailbox_state(&job, "running").is_err());
        db.set_mailbox_state_for_test(&job, "running").unwrap();
        db.set_mailbox_state(&job, "failed").unwrap();
        db.set_mailbox_state_for_test(&job, "running").unwrap();
        assert!(db.set_mailbox_state(&job, "queued").is_err());
    }

    #[test]
    fn mailbox_state_machine_has_an_exhaustive_transition_matrix() {
        let states = [
            "queued",
            "preflight",
            "ready",
            "running",
            "completed",
            "delta_required",
            "verification_difference",
            "failed",
            "cancelled",
            "verified",
            "attention",
        ];
        let allowed = [
            (
                "queued",
                &[
                    "preflight",
                    "ready",
                    "running",
                    "failed",
                    "cancelled",
                    "attention",
                ] as &[&str],
            ),
            ("preflight", &["ready", "running", "failed", "cancelled"]),
            ("ready", &["running", "failed", "cancelled"]),
            (
                "running",
                &[
                    "ready",
                    "completed",
                    "delta_required",
                    "verification_difference",
                    "failed",
                    "cancelled",
                    "attention",
                ],
            ),
            (
                "completed",
                &[
                    "verified",
                    "delta_required",
                    "verification_difference",
                    "running",
                    "attention",
                ],
            ),
            (
                "delta_required",
                &["running", "failed", "cancelled", "attention"],
            ),
            (
                "verification_difference",
                &["running", "failed", "cancelled", "attention"],
            ),
            ("failed", &["running", "attention"]),
            ("cancelled", &["running", "attention"]),
            ("verified", &["delta_required", "running", "attention"]),
            ("attention", &["running"]),
        ];
        for current in states {
            for next in states {
                let expected = allowed
                    .iter()
                    .find(|(state, _)| *state == current)
                    .is_some_and(|(_, targets)| targets.contains(&next));
                assert_eq!(
                    valid_mailbox_transition(current, next),
                    expected,
                    "unexpected mailbox transition {current} -> {next}"
                );
            }
        }
        assert!(!valid_mailbox_transition("unknown", "running"));
        assert!(!valid_mailbox_transition("ready", "unknown"));
    }

    #[test]
    fn mailbox_cannot_start_two_runs_or_be_verified_without_evidence() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-owner", "test")
            .unwrap();
        assert!(
            db.begin_run(&project.id, &job, "run-second", "test")
                .is_err()
        );
        assert!(db.set_mailbox_state(&job, "verified").is_err());
        assert!(
            db.finish_run_for_mailbox(
                &project.id,
                &job,
                "run-owner",
                "completed",
                "verified",
                "without evidence",
            )
            .is_err()
        );
    }

    #[test]
    fn terminal_run_and_mailbox_states_must_agree() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("terminal-pair", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-terminal-pair", "test")
            .unwrap();

        assert!(
            db.finish_run_for_mailbox(
                &project.id,
                &job,
                "run-terminal-pair",
                "failed",
                "completed",
                "contradictory terminal state",
            )
            .is_err()
        );
        assert_eq!(
            db.run_status("run-terminal-pair").unwrap().as_deref(),
            Some("running")
        );
        assert_eq!(db.mailbox_state(&job).unwrap().as_deref(), Some("running"));
    }

    #[test]
    fn newer_run_cannot_reuse_older_evidence_for_verified_state() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("evidence-age", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 10,
            destination_bytes: 10,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };
        db.begin_run(&project.id, &job, "run-with-evidence", "test")
            .unwrap();
        db.finish_run_for_mailbox_with_evidence(
            &project.id,
            &job,
            "run-with-evidence",
            "completed",
            "verified",
            "",
            &evidence,
        )
        .unwrap();
        db.begin_run(&project.id, &job, "run-without-evidence", "test")
            .unwrap();
        db.finish_run_for_mailbox(
            &project.id,
            &job,
            "run-without-evidence",
            "completed",
            "completed",
            "verification unavailable",
        )
        .unwrap();
        assert!(db.set_mailbox_state(&job, "verified").is_err());
    }

    #[test]
    fn generic_terminal_completion_cannot_reuse_stale_evidence_for_verified() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("stale-terminal-evidence", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 10,
            destination_bytes: 10,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };
        db.begin_run(&project.id, &job, "run-old-evidence", "imapsync")
            .unwrap();
        db.finish_run_for_mailbox_with_evidence(
            &project.id,
            &job,
            "run-old-evidence",
            "completed",
            "verified",
            "",
            &evidence,
        )
        .unwrap();

        db.begin_run(&project.id, &job, "run-no-evidence", "imapsync")
            .unwrap();
        assert!(
            db.finish_run_for_mailbox(
                &project.id,
                &job,
                "run-no-evidence",
                "completed",
                "verified",
                "stale evidence must not be reused",
            )
            .is_err()
        );
        assert_eq!(
            db.run_status("run-no-evidence").unwrap().as_deref(),
            Some("running")
        );
        assert_eq!(db.mailbox_state(&job).unwrap().as_deref(), Some("running"));
    }

    #[test]
    fn database_rejects_two_running_runs_for_one_mailbox() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-one", "test").unwrap();
        let result = db.connection.execute(
            "INSERT INTO runs(id,project_id,job_id,engine,plan_snapshot,status) VALUES(?1,?2,?3,'test','','running')",
            rusqlite::params!["run-two", project.id, job],
        );
        assert!(result.is_err());
    }

    #[test]
    fn project_cannot_complete_without_verified_mailboxes() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.transition(&project.id, Phase::Preflight).unwrap();
        db.transition(&project.id, Phase::Verification).unwrap();
        assert!(!db.all_mailboxes_verified(&project.id).unwrap());
        assert!(db.transition(&project.id, Phase::Complete).is_err());
        db.set_mailbox_state_for_test(&job, "running").unwrap();
        db.insert_run_for_test(&project.id, Some(&job), "run-project-complete", "test")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 10,
            destination_bytes: 10,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };
        db.finish_run_for_mailbox_with_evidence(
            &project.id,
            &job,
            "run-project-complete",
            "completed",
            "verified",
            "verified",
            &evidence,
        )
        .unwrap();
        assert!(db.all_mailboxes_verified(&project.id).unwrap());
        db.transition(&project.id, Phase::Complete).unwrap();

        // Complete is a durable terminal project state. Reopening must be an
        // explicit, separately audited operation rather than an incidental
        // consequence of editing the queue or starting another run.
        assert!(
            db.add_mailbox(&project.id, "new-source", "new-destination")
                .is_err()
        );
        assert!(
            db.begin_run(&project.id, &job, "run-after-complete", "test")
                .is_err()
        );
        assert!(db.set_mailbox_state(&job, "attention").is_err());
        assert!(
            db.set_preflight_plan(
                &job,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .is_err()
        );
        db.reopen_project(&project.id, "customer requested a post-cutover correction")
            .unwrap();
        assert_eq!(
            db.project(&project.id).unwrap().unwrap().phase,
            Phase::Attention
        );
        let reopened_job = db
            .add_mailbox(&project.id, "new-source", "new-destination")
            .unwrap();
        assert_eq!(
            db.mailbox_state(&reopened_job).unwrap().as_deref(),
            Some("queued")
        );
        assert!(db.reopen_project(&project.id, "").is_err());
    }

    #[test]
    fn attention_phase_can_return_to_reviewable_work() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();

        db.transition(&project.id, Phase::Attention).unwrap();
        db.transition(&project.id, Phase::Preflight).unwrap();
        db.transition(&project.id, Phase::Verification).unwrap();

        assert_eq!(
            db.project(&project.id).unwrap().unwrap().phase,
            Phase::Verification
        );
    }

    #[test]
    fn running_jobs_are_recovered_for_operator_review() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.set_mailbox_state_for_test(&job, "running").unwrap();
        db.insert_run_for_test(&project.id, Some(&job), "run-1", "test")
            .unwrap();
        assert_eq!(db.recover_abandoned_jobs().unwrap(), 1);
        assert!(db.set_mailbox_state(&job, "queued").is_err());
        assert_eq!(
            db.run_status("run-1").unwrap().as_deref(),
            Some("abandoned")
        );
        assert_eq!(
            db.mailbox_attention_reason(&job).unwrap(),
            Some(AttentionReason::ProcessIdentityUnverified)
        );
    }

    #[test]
    fn active_process_identity_is_durable_and_cleared_on_recovery() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-process", "test")
            .unwrap();
        db.register_process(&ActiveProcess {
            run_id: "run-process".into(),
            job_id: job.clone(),
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        })
        .unwrap();
        assert_eq!(
            db.active_processes().unwrap(),
            vec![ActiveProcess {
                run_id: "run-process".into(),
                job_id: job.clone(),
                pid: 4242,
                start_ticks: Some(7),
                process_group: Some(4242),
                session_id: Some(4242),
                executable: "test".into(),
            }]
        );
        db.recover_abandoned_jobs().unwrap();
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn active_process_rejects_run_and_mailbox_from_different_projects() {
        let db = StateStore::in_memory().unwrap();
        let first = db
            .create_project("first", "source-a", "destination-a")
            .unwrap();
        let first_job = db
            .add_mailbox(&first.id, "source-a", "destination-a")
            .unwrap();
        let second = db
            .create_project("second", "source-b", "destination-b")
            .unwrap();
        let second_job = db
            .add_mailbox(&second.id, "source-b", "destination-b")
            .unwrap();
        db.begin_run(&first.id, &first_job, "run-first", "test")
            .unwrap();

        let result = db.register_process(&ActiveProcess {
            run_id: "run-first".into(),
            job_id: second_job,
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        });
        assert!(result.is_err());
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn active_process_rejects_wrong_mailbox_for_single_mailbox_run() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("same-project", "source", "destination")
            .unwrap();
        let first_job = db
            .add_mailbox(&project.id, "source-one", "destination-one")
            .unwrap();
        let second_job = db
            .add_mailbox(&project.id, "source-two", "destination-two")
            .unwrap();
        db.begin_run(&project.id, &first_job, "run-single", "test")
            .unwrap();

        let result = db.register_process(&ActiveProcess {
            run_id: "run-single".into(),
            job_id: second_job,
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        });
        assert!(result.is_err());
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn active_process_requires_a_mailbox_specific_child_run() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("batch-process", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source-user", "destination-user")
            .unwrap();
        let child_runs = db
            .begin_batch_run_with_children(
                &project.id,
                std::slice::from_ref(&job),
                "run-parent-process",
                "test",
                &[],
                "batch snapshot",
                &[],
            )
            .unwrap();

        assert!(
            db.register_process(&ActiveProcess {
                run_id: "run-parent-process".into(),
                job_id: job.clone(),
                pid: 4242,
                start_ticks: Some(7),
                process_group: Some(4242),
                session_id: Some(4242),
                executable: "test".into(),
            })
            .is_err()
        );
        db.claim_batch_mailbox_for_child(&project.id, &job, "run-parent-process", &child_runs[0])
            .unwrap();
        db.register_process(&ActiveProcess {
            run_id: child_runs[0].clone(),
            job_id: job.clone(),
            pid: 4243,
            start_ticks: Some(8),
            process_group: Some(4243),
            session_id: Some(4243),
            executable: "test".into(),
        })
        .unwrap();
    }

    #[test]
    fn late_process_registration_after_terminal_run_is_rejected() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("late-process", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source-user", "destination-user")
            .unwrap();
        db.begin_run(&project.id, &job, "run-late-process", "test")
            .unwrap();
        db.finish_run(
            "run-late-process",
            "failed",
            "cancelled before process registration",
        )
        .unwrap();

        let result = db.register_process(&ActiveProcess {
            run_id: "run-late-process".into(),
            job_id: job,
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        });
        assert!(result.is_err());
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn terminal_batch_run_clears_process_identity_atomically() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-process-finish", "test")
            .unwrap();
        db.register_process(&ActiveProcess {
            run_id: "run-process-finish".into(),
            job_id: job.clone(),
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        })
        .unwrap();
        db.finish_run("run-process-finish", "failed", "test failure")
            .unwrap();
        assert!(db.active_processes().unwrap().is_empty());
        assert_eq!(
            db.run_status("run-process-finish").unwrap().as_deref(),
            Some("failed")
        );
        assert!(
            db.finish_run("run-process-finish", "failed", "duplicate")
                .is_err()
        );
    }

    #[test]
    fn terminal_run_and_mailbox_state_are_committed_together() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-atomic", "imapsync")
            .unwrap();
        db.set_mailbox_state(&job, "completed").unwrap();

        db.finish_run_for_mailbox(
            &project.id,
            &job,
            "run-atomic",
            "completed",
            "completed",
            "",
        )
        .unwrap();

        assert_eq!(
            db.mailbox_state(&job).unwrap().as_deref(),
            Some("completed")
        );
        assert_eq!(
            db.run_status("run-atomic").unwrap().as_deref(),
            Some("completed")
        );
        assert!(
            db.finish_run_for_mailbox(
                &project.id,
                &job,
                "run-atomic",
                "completed",
                "completed",
                "duplicate completion",
            )
            .is_err()
        );
    }

    #[test]
    fn evidence_terminal_completion_allows_running_to_verified_atomically() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-evidence", "imapsync")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 3,
            destination_messages: 3,
            source_bytes: 300,
            destination_bytes: 300,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 2,
            destination_folders: 2,
            authoritative: true,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };
        db.register_process(&ActiveProcess {
            run_id: "run-evidence".into(),
            job_id: job.clone(),
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        })
        .unwrap();

        db.finish_run_for_mailbox_with_evidence(
            &project.id,
            &job,
            "run-evidence",
            "completed",
            "verified",
            "",
            &evidence,
        )
        .unwrap();

        assert_eq!(db.mailbox_state(&job).unwrap().as_deref(), Some("verified"));
        assert_eq!(
            db.run_status("run-evidence").unwrap().as_deref(),
            Some("completed")
        );
        assert_eq!(db.evidence(&job).unwrap(), Some(evidence));
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn evidence_difference_is_not_promoted_to_verified_or_complete() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("difference", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-difference", "imapsync")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 10,
            destination_messages: 9,
            source_bytes: 100,
            destination_bytes: 90,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 2,
            destination_folders: 2,
            authoritative: false,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };
        assert!(
            db.finish_run_for_mailbox_with_evidence(
                &project.id,
                &job,
                "run-difference",
                "completed",
                "verified",
                "mismatching evidence",
                &evidence,
            )
            .is_err()
        );
        assert_eq!(db.mailbox_state(&job).unwrap().as_deref(), Some("running"));
        assert_eq!(
            db.run_status("run-difference").unwrap().as_deref(),
            Some("running")
        );
        db.finish_run_for_mailbox_with_evidence(
            &project.id,
            &job,
            "run-difference",
            "completed",
            "verification_difference",
            "aggregate totals differ",
            &evidence,
        )
        .unwrap();
        assert_eq!(
            db.mailbox_state(&job).unwrap().as_deref(),
            Some("verification_difference")
        );
        assert!(db.transition(&project.id, Phase::Complete).is_err());
    }

    #[test]
    fn latest_evidence_resolves_to_its_own_run() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-evidence-one", "imapsync")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 10,
            destination_bytes: 10,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };
        db.finish_run_for_mailbox_with_evidence(
            &project.id,
            &job,
            "run-evidence-one",
            "completed",
            "verified",
            "",
            &evidence,
        )
        .unwrap();
        db.begin_run(&project.id, &job, "run-evidence-two", "imapsync")
            .unwrap();
        db.finish_run_for_mailbox(
            &project.id,
            &job,
            "run-evidence-two",
            "failed",
            "attention",
            "later failure",
        )
        .unwrap();

        let (run_id, latest) = db.latest_evidence_for_run(&job).unwrap().unwrap();
        assert_eq!(run_id, "run-evidence-one");
        assert_eq!(latest, evidence);
        assert_eq!(db.run(&run_id).unwrap().unwrap().status, "completed");
    }

    #[test]
    fn batch_project_creation_is_atomic() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(
            db.first_mailbox(&project.id).unwrap(),
            Some(jobs[0].clone())
        );
        assert_eq!(
            db.mailbox_state(&jobs[1]).unwrap().as_deref(),
            Some("queued")
        );
        assert_eq!(
            db.mailbox_identity(&jobs[0]).unwrap().unwrap(),
            ("one".into(), "one".into(), "queued".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn state_store_does_not_change_parent_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!("mailswiftsync-db-{}", Uuid::new_v4()));
        let path = directory.join("state.db");
        std::fs::create_dir_all(&directory).unwrap();
        let mut permissions = std::fs::metadata(&directory).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&directory, permissions).unwrap();
        let _store = StateStore::open(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o755
        );
        drop(_store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn schema_version_is_recorded_and_future_versions_are_rejected() {
        let db = StateStore::in_memory().unwrap();
        let version: i64 = db
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
        drop(db);

        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-schema-{}", Uuid::new_v4()));
        let path = directory.join("state.db");
        std::fs::create_dir_all(&directory).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "user_version", 99_i64)
            .unwrap();
        drop(connection);
        assert!(StateStore::open(&path).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn readonly_open_migrates_legacy_copy_without_rewriting_source() {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-readonly-legacy-{}", Uuid::new_v4()));
        let path = directory.join("state.db");
        std::fs::create_dir_all(&directory).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE mailbox_jobs (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    source_mailbox TEXT NOT NULL,
                    destination_mailbox TEXT NOT NULL,
                    state TEXT NOT NULL,
                    attempt INTEGER NOT NULL DEFAULT 0,
                    checkpoint TEXT,
                    preflight_plan TEXT,
                    config TEXT
                );
                PRAGMA user_version=1;",
            )
            .unwrap();
        drop(connection);

        let readonly = StateStore::open_readonly(&path).unwrap();
        let copied_version: i64 = readonly
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(copied_version, CURRENT_SCHEMA_VERSION);
        drop(readonly);

        let source = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let source_version: i64 = source
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(source_version, 1);
        let attention_reason_columns: i64 = source
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('mailbox_jobs') WHERE name='attention_reason'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(attention_reason_columns, 0);
        drop(source);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn backup_is_consistent_private_and_non_overwriting() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("backup", "source", "destination")
            .unwrap();
        db.add_mailbox(&project.id, "source", "destination")
            .unwrap();
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-backup-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let destination = directory.join("state-backup.db");
        db.backup_to(&destination).unwrap();
        assert!(destination.is_file());
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(db.backup_to(&destination).is_err());
        let backup = StateStore::open(&destination).unwrap();
        assert_eq!(backup.latest_project().unwrap().unwrap().name, "backup");
        drop(backup);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn legacy_schema_migrates_destination_identity_column() {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-schema-v1-{}", Uuid::new_v4()));
        let path = directory.join("state.db");
        std::fs::create_dir_all(&directory).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE mailbox_jobs (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    source_mailbox TEXT NOT NULL,
                    destination_mailbox TEXT NOT NULL,
                    state TEXT NOT NULL,
                    attempt INTEGER NOT NULL DEFAULT 0,
                    checkpoint TEXT,
                    preflight_plan TEXT,
                    config TEXT
                );
                PRAGMA user_version=1;",
            )
            .unwrap();
        drop(connection);

        let store = StateStore::open(&path).unwrap();
        let version: i64 = store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
        let migration_backups = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("state.db.pre-migrate-v1.")
            })
            .count();
        assert_eq!(migration_backups, 1);
        let has_identity: bool = store
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('mailbox_jobs') WHERE name='destination_identity')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_identity);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn verification_difference_requires_durable_exception_acceptance() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("exceptions", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 10,
            destination_messages: 9,
            source_bytes: 100,
            destination_bytes: 90,
            unmatched_messages: 1,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: false,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };
        db.begin_run(&project.id, &job, "exception-run", "imapsync")
            .unwrap();
        db.finish_run_for_mailbox_with_evidence_and_checkpoint(
            &project.id,
            &job,
            "exception-run",
            "completed",
            "verification_difference",
            "destination-only message accepted later",
            &evidence,
            None,
        )
        .unwrap();
        assert!(!db.all_mailboxes_verified(&project.id).unwrap());
        db.accept_verification_difference(
            &project.id,
            &job,
            "operator@example",
            "Approved in change CHG-123; destination account was active before cutover.",
        )
        .unwrap();
        assert_eq!(
            db.mailbox_state(&job).unwrap().as_deref(),
            Some("verified_with_exceptions")
        );
        let acceptance = db.latest_verification_acceptance(&job).unwrap().unwrap();
        assert_eq!(acceptance.run_id, "exception-run");
        assert_eq!(acceptance.operator, "operator@example");
        assert!(db.all_mailboxes_verified(&project.id).unwrap());
        db.transition(&project.id, Phase::Preflight).unwrap();
        db.transition(&project.id, Phase::Pilot).unwrap();
        db.transition(&project.id, Phase::Seed).unwrap();
        db.transition(&project.id, Phase::CatchUp).unwrap();
        db.transition(&project.id, Phase::FinalDelta).unwrap();
        db.transition(&project.id, Phase::Verification).unwrap();
        db.transition(&project.id, Phase::Complete).unwrap();
        assert_eq!(
            db.project(&project.id).unwrap().unwrap().phase,
            Phase::Complete
        );
        assert!(
            db.accept_verification_difference(
                &project.id,
                &job,
                "operator@example",
                "duplicate acceptance",
            )
            .is_err()
        );
    }

    #[test]
    fn migration_clears_legacy_raw_preflight_plans() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("legacy-plan", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.connection
            .execute(
                "UPDATE mailbox_jobs SET preflight_plan=?1 WHERE id=?2",
                params!["--host1 source.example --password1 secret", job],
            )
            .unwrap();

        db.migrate().unwrap();

        assert_eq!(db.preflight_plan(&job).unwrap(), None);
    }

    #[test]
    fn migration_repairs_duplicate_active_runs_before_recreating_constraints() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("legacy-active", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-newest", "test")
            .unwrap();
        db.connection
            .execute("DROP INDEX one_active_run_per_job", [])
            .unwrap();
        db.connection
            .execute("DROP INDEX one_running_run_per_job", [])
            .unwrap();
        db.connection
            .execute(
                "INSERT INTO runs(id,project_id,job_id,engine,phase_at_start,plan_snapshot,status,started_at) VALUES('run-older',?1,?2,'test','discovery','','running','2000-01-01 00:00:00')",
                params![project.id, job],
            )
            .unwrap();

        db.migrate().unwrap();

        let active: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE job_id=?1 AND status IN ('queued','running')",
                [&job],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active, 1);
        assert_eq!(
            db.run_status("run-older").unwrap().as_deref(),
            Some("abandoned")
        );
        let repaired: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind='schema_repaired_duplicate_run'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repaired, 1);
    }

    #[test]
    fn batch_configuration_is_persisted_without_credentials() {
        let db = StateStore::in_memory().unwrap();
        let (_, jobs) = db
            .create_project_with_mailbox_configs(
                "batch",
                "source",
                "destination",
                &[("one".into(), "one".into(), "engine = \"imap\"".into())],
            )
            .unwrap();
        let restored = db
            .mailboxes(&db.latest_project().unwrap().unwrap().id)
            .unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, jobs[0]);
        assert_eq!(restored[0].config.as_deref(), Some("engine = \"imap\""));
    }

    #[test]
    fn batch_classification_checks_configuration_completeness_without_loading_rows() {
        let db = StateStore::in_memory().unwrap();
        let single = db
            .create_project("single", "source", "destination")
            .unwrap();
        db.add_mailbox(&single.id, "source", "destination").unwrap();
        assert!(!db.project_has_complete_mailbox_configs(&single.id).unwrap());

        let (batch, jobs) = db
            .create_project_with_mailbox_configs(
                "batch",
                "source",
                "destination",
                &[("one".into(), "one".into(), "engine = \"imap\"".into())],
            )
            .unwrap();
        assert!(db.project_has_complete_mailbox_configs(&batch.id).unwrap());

        db.connection
            .execute(
                "UPDATE mailbox_jobs SET config=NULL WHERE id=?1",
                [&jobs[0]],
            )
            .unwrap();
        assert!(!db.project_has_complete_mailbox_configs(&batch.id).unwrap());
    }

    #[test]
    fn latest_run_summary_is_queryable_for_audit_reports() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("audit", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.insert_run_for_test(&project.id, Some(&job), "run-audit", "test")
            .unwrap();
        db.finish_run("run-audit", "completed", "ok").unwrap();
        let run = db.latest_run(&job).unwrap().unwrap();
        assert_eq!(run.id, "run-audit");
        assert_eq!(run.status, "completed");
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn project_report_snapshot_loads_related_rows_as_one_read_model() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("report-snapshot", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.insert_run_for_test(&project.id, Some(&job), "run-report", "imapsync")
            .unwrap();
        db.record_engine_version("run-report", "imapsync 2.300")
            .unwrap();
        db.finish_run("run-report", "completed", "ok").unwrap();

        let snapshot = db.project_report_snapshot(&project.id).unwrap().unwrap();
        assert_eq!(snapshot.mailboxes.len(), 1);
        assert_eq!(snapshot.runs.len(), 1);
        assert_eq!(
            snapshot.runs[0].engine_version.as_deref(),
            Some("imapsync 2.300")
        );
        assert_eq!(snapshot.mailboxes[0].job.id, job);
    }

    #[test]
    fn paged_workspace_reads_keep_large_mailbox_projects_bounded() {
        const MAILBOX_COUNT: usize = 100_000;
        const PAGE_SIZE: u32 = 200;
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("large-workspace", "source", "destination")
            .unwrap();
        {
            let tx = db.connection.unchecked_transaction().unwrap();
            for index in 0..MAILBOX_COUNT {
                let mailbox = format!("user-{index}@source.example");
                let destination = format!("user-{index}@destination.example");
                tx.execute(
                    "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,destination_identity,state) VALUES(?1,?2,?3,?4,?5,'queued')",
                    rusqlite::params![
                        format!("job-{index}"),
                        project.id,
                        mailbox,
                        destination,
                        destination.to_ascii_lowercase(),
                    ],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }

        let first_page = db.mailbox_page(&project.id, 0, PAGE_SIZE).unwrap();
        let last_page = db
            .mailbox_page(
                &project.id,
                (MAILBOX_COUNT as u32).saturating_sub(PAGE_SIZE),
                PAGE_SIZE,
            )
            .unwrap();
        assert_eq!(first_page.len(), PAGE_SIZE as usize);
        assert_eq!(last_page.len(), PAGE_SIZE as usize);
        assert_eq!(first_page[0].source_mailbox, "user-0@source.example");
        assert_eq!(last_page[0].source_mailbox, "user-99800@source.example");

        let counts = db.mailbox_state_counts(&project.id).unwrap();
        assert_eq!(counts.total, MAILBOX_COUNT);
        assert_eq!(counts.ready, 0);
        assert_eq!(counts.running, 0);
        assert_eq!(counts.verified, 0);
        assert_eq!(counts.needs_review, 0);
    }

    #[test]
    fn run_captures_project_phase_at_admission() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("phase-provenance", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.transition(&project.id, Phase::Preflight).unwrap();
        db.begin_run_with_snapshot(
            &project.id,
            &job,
            "run-phase-provenance",
            "imapsync",
            "snapshot",
        )
        .unwrap();

        let run = db.run("run-phase-provenance").unwrap().unwrap();
        assert_eq!(run.phase_at_start, "preflight");
    }

    #[test]
    fn recent_run_list_omits_large_plan_snapshots() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("list", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run_with_snapshot(
            &project.id,
            &job,
            "run-list",
            "test",
            &"large snapshot".repeat(1024),
        )
        .unwrap();

        let list = db.recent_run_list(&project.id, 10).unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "run-list");
        assert_eq!(list[0].source_mailbox.as_deref(), Some("source"));
        assert_eq!(list[0].destination_mailbox.as_deref(), Some("destination"));
        assert_eq!(list[0].status, "running");
    }

    #[test]
    fn begin_run_atomically_moves_mailbox_and_records_run() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("atomic", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.set_mailbox_state(&job, "ready").unwrap();
        db.begin_run(&project.id, &job, "run-atomic", "test")
            .unwrap();
        assert_eq!(db.mailbox_state(&job).unwrap().as_deref(), Some("running"));
        assert_eq!(
            db.run_status("run-atomic").unwrap().as_deref(),
            Some("running")
        );
    }

    #[test]
    fn run_plan_snapshot_is_persisted_at_start() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("snapshot", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run_with_snapshot(
            &project.id,
            &job,
            "run-snapshot",
            "imapsync fallback",
            "dry_run=false\nsource_host = \"source\"",
        )
        .unwrap();
        let run = db.run("run-snapshot").unwrap().unwrap();
        assert_eq!(run.plan_snapshot, "dry_run=false\nsource_host = \"source\"");
    }

    #[test]
    fn begin_batch_run_leaves_children_queued_until_claimed() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-atomic",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        db.begin_batch_run(&project.id, &jobs, "run-batch-atomic", "test", &[])
            .unwrap();
        assert!(
            jobs.iter()
                .all(|job| db.mailbox_state(job).unwrap().as_deref() == Some("queued"))
        );
        assert_eq!(
            db.run_status("run-batch-atomic").unwrap().as_deref(),
            Some("running")
        );
        assert!(
            db.begin_batch_run(&project.id, &jobs, "run-batch-duplicate", "test", &[])
                .is_err()
        );
    }

    #[test]
    fn batch_parent_cannot_finish_with_unresolved_children() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-parent-terminal",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        db.begin_batch_run(&project.id, &jobs, "run-batch-parent", "test", &[])
            .unwrap();

        assert!(
            db.finish_run("run-batch-parent", "completed", "premature completion")
                .is_err()
        );
        assert_eq!(
            db.run_status("run-batch-parent").unwrap().as_deref(),
            Some("running")
        );
        assert_eq!(
            db.mailbox_state(&jobs[0]).unwrap().as_deref(),
            Some("queued")
        );
        assert_eq!(
            db.mailbox_state(&jobs[1]).unwrap().as_deref(),
            Some("queued")
        );
    }

    #[test]
    fn parent_run_rejects_unknown_terminal_status() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("run-status", "source", "destination")
            .unwrap();
        db.insert_run_for_test(&project.id, None, "run-status", "test")
            .unwrap();

        assert!(db.finish_run("run-status", "invented", "invalid").is_err());
        assert_eq!(
            db.run_status("run-status").unwrap().as_deref(),
            Some("running")
        );
    }

    #[test]
    fn core_rejects_duplicate_destination_mailboxes() {
        let db = StateStore::in_memory().unwrap();
        assert!(
            db.create_project_with_mailboxes(
                "duplicate-batch",
                "source",
                "destination",
                &[
                    ("one".into(), "Target@Example.test".into()),
                    ("two".into(), " target@example.test ".into())
                ],
            )
            .is_err()
        );

        let project = db
            .create_project("duplicate-single", "source", "destination")
            .unwrap();
        db.add_mailbox(&project.id, "one", "Target@Example.test")
            .unwrap();
        assert!(
            db.add_mailbox(&project.id, "two", " target@example.test ")
                .is_err()
        );
    }

    #[test]
    fn configured_batch_destination_identity_includes_endpoint() {
        let db = StateStore::in_memory().unwrap();
        let config_a = r#"
destination_host = "mail-a.example.test"
destination_user = "target@example.test"
destination_tls = "imaps"
destination_port = "993"
"#;
        let config_b = r#"
destination_host = "mail-b.example.test"
destination_user = "target@example.test"
destination_tls = "imaps"
destination_port = "993"
"#;
        let (project, jobs) = db
            .create_project_with_mailbox_configs(
                "heterogeneous-destinations",
                "source",
                "batch",
                &[
                    ("one".into(), "target@example.test".into(), config_a.into()),
                    ("two".into(), "target@example.test".into(), config_b.into()),
                ],
            )
            .unwrap();

        // The same mailbox name on two distinct destination endpoints is safe
        // to schedule concurrently; the endpoint-qualified identity prevents
        // the core from applying the UI's more precise check inconsistently.
        db.begin_batch_run_with_children(
            &project.id,
            &jobs,
            "heterogeneous-destination-run",
            "imapsync",
            &[],
            "batch snapshot",
            &[],
        )
        .unwrap();

        let embedded_port_config = r#"
destination_host = "mail-c.example.test:143"
destination_user = "target@example.test"
destination_tls = "starttls"
destination_port = ""
"#;
        let explicit_port_config = r#"
destination_host = "mail-c.example.test"
destination_user = "target@example.test"
destination_tls = "starttls"
destination_port = "143"
"#;
        assert!(
            db.create_project_with_mailbox_configs(
                "equivalent-endpoint-forms",
                "source",
                "batch",
                &[
                    ("three".into(), "one".into(), embedded_port_config.into()),
                    ("four".into(), "two".into(), explicit_port_config.into()),
                ],
            )
            .is_err()
        );
    }

    #[test]
    fn destination_identity_repair_recomputes_existing_rows_from_config() {
        let db = StateStore::in_memory().unwrap();
        let config = r#"
destination_host = "MAIL.example.test."
destination_user = "User@example.test"
destination_tls = "imaps"
destination_port = ""
"#;
        let (project, jobs) = db
            .create_project_with_mailbox_configs(
                "identity-repair",
                "source",
                "batch",
                &[("one".into(), "ignored-row-mailbox".into(), config.into())],
            )
            .unwrap();
        db.connection
            .execute(
                "UPDATE mailbox_jobs SET destination_identity='stale-identity' WHERE id=?1",
                [&jobs[0]],
            )
            .unwrap();
        db.migrate().unwrap();
        let identity: String = db
            .connection
            .query_row(
                "SELECT destination_identity FROM mailbox_jobs WHERE project_id=?1",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(identity, "endpoint:mail.example.test:993:User@example.test");
    }

    #[test]
    fn malformed_configured_endpoint_does_not_become_an_endpoint_identity() {
        let malformed_host = r#"
destination_host = "mail.example:0"
destination_user = "target@example.test"
destination_tls = "imaps"
destination_port = ""
"#;
        assert_eq!(
            normalized_destination_identity("Target@example.test", Some(malformed_host)),
            "mailbox:target@example.test"
        );

        let malformed_port = r#"
destination_host = "mail.example"
destination_user = "target@example.test"
destination_tls = "imaps"
destination_port = "000"
"#;
        assert_eq!(
            normalized_destination_identity("Target@example.test", Some(malformed_port)),
            "mailbox:target@example.test"
        );
    }

    #[test]
    fn successful_parent_run_rejects_unsuccessful_child() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-child-failure",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        db.begin_batch_run(&project.id, &jobs, "run-parent-failure", "test", &[])
            .unwrap();
        let child = db
            .recent_runs(&project.id, 10)
            .unwrap()
            .into_iter()
            .find(|run| run.job_id.as_deref() == Some(jobs[0].as_str()))
            .unwrap();
        db.claim_batch_mailbox_for_child(&project.id, &jobs[0], "run-parent-failure", &child.id)
            .unwrap();
        db.finish_run_for_mailbox(
            &project.id,
            &jobs[0],
            &child.id,
            "failed",
            "failed",
            "synthetic child failure",
        )
        .unwrap();
        let second_child = db
            .recent_runs(&project.id, 10)
            .unwrap()
            .into_iter()
            .find(|run| run.job_id.as_deref() == Some(jobs[1].as_str()))
            .unwrap();
        db.claim_batch_mailbox_for_child(
            &project.id,
            &jobs[1],
            "run-parent-failure",
            &second_child.id,
        )
        .unwrap();
        db.finish_run_for_mailbox(
            &project.id,
            &jobs[1],
            &second_child.id,
            "completed",
            "completed",
            "synthetic child completion",
        )
        .unwrap();

        assert!(
            db.finish_run("run-parent-failure", "completed", "incorrect success")
                .is_err()
        );
        assert_eq!(
            db.run_status("run-parent-failure").unwrap().as_deref(),
            Some("running")
        );
    }

    #[test]
    fn batch_claim_moves_only_the_claimed_mailbox_to_running() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-claim",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        db.begin_batch_run(&project.id, &jobs, "run-batch-claim", "test", &[])
            .unwrap();
        db.claim_batch_mailbox(&project.id, &jobs[0], "run-batch-claim")
            .unwrap();
        assert_eq!(
            db.mailbox_state(&jobs[0]).unwrap().as_deref(),
            Some("running")
        );
        assert_eq!(
            db.mailbox_state(&jobs[1]).unwrap().as_deref(),
            Some("queued")
        );
        db.claim_batch_mailbox(&project.id, &jobs[0], "run-batch-claim")
            .unwrap();
        assert_eq!(
            db.mailbox_state(&jobs[0]).unwrap().as_deref(),
            Some("running")
        );
    }

    #[test]
    fn batch_children_persist_mailbox_specific_runs_and_snapshots() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-children",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        let child_runs = db
            .begin_batch_run_with_children(
                &project.id,
                &jobs,
                "run-parent",
                "batch",
                &[],
                "parent snapshot",
                &[
                    BatchChildPlan {
                        engine: "imapsync".into(),
                        plan_snapshot: "snapshot one".into(),
                        engine_version: Some("imapsync 2.300".into()),
                    },
                    BatchChildPlan {
                        engine: "dovecot".into(),
                        plan_snapshot: "snapshot two".into(),
                        engine_version: None,
                    },
                ],
            )
            .unwrap();
        assert_eq!(child_runs.len(), 2);
        let first = db.run(&child_runs[0]).unwrap().unwrap();
        let second = db.run(&child_runs[1]).unwrap().unwrap();
        assert_eq!(first.job_id.as_deref(), Some(jobs[0].as_str()));
        assert_eq!(first.status, "queued");
        assert_eq!(first.engine, "imapsync");
        assert_eq!(first.plan_snapshot, "snapshot one");
        assert_eq!(
            db.engine_version(&child_runs[0]).unwrap().as_deref(),
            Some("imapsync 2.300")
        );
        assert_eq!(second.job_id.as_deref(), Some(jobs[1].as_str()));
        assert_eq!(second.status, "queued");
        assert_eq!(second.engine, "dovecot");
        assert_eq!(second.plan_snapshot, "snapshot two");
        assert_eq!(db.engine_version(&child_runs[1]).unwrap(), None);
        db.claim_batch_mailbox_for_child(&project.id, &jobs[0], "run-parent", &child_runs[0])
            .unwrap();
        assert!(
            db.claim_batch_mailbox_for_child(&project.id, &jobs[0], "run-parent", &child_runs[0])
                .is_err()
        );
        db.register_process(&ActiveProcess {
            run_id: child_runs[0].clone(),
            job_id: jobs[0].clone(),
            pid: 4242,
            start_ticks: Some(1),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test-engine".into(),
        })
        .unwrap();
        db.finish_run_for_mailbox(
            &project.id,
            &jobs[0],
            &child_runs[0],
            "completed",
            "completed",
            "child complete",
        )
        .unwrap();
        assert_eq!(
            db.run_status(&child_runs[0]).unwrap().as_deref(),
            Some("completed")
        );
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn transient_batch_failure_releases_child_for_a_fresh_claim() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-retry-claim",
                "source",
                "destination",
                &[("one".into(), "one".into())],
            )
            .unwrap();
        let child_runs = db
            .begin_batch_run_with_children(
                &project.id,
                &jobs,
                "run-retry-parent",
                "batch",
                &[],
                "snapshot",
                &[],
            )
            .unwrap();
        db.claim_batch_mailbox_for_child(&project.id, &jobs[0], "run-retry-parent", &child_runs[0])
            .unwrap();
        db.release_batch_mailbox_for_retry(
            &project.id,
            &jobs[0],
            "run-retry-parent",
            &child_runs[0],
        )
        .unwrap();
        assert_eq!(
            db.mailbox_state(&jobs[0]).unwrap().as_deref(),
            Some("ready")
        );
        assert_eq!(
            db.run_status(&child_runs[0]).unwrap().as_deref(),
            Some("queued")
        );

        db.claim_batch_mailbox_for_child(&project.id, &jobs[0], "run-retry-parent", &child_runs[0])
            .unwrap();
        assert_eq!(
            db.mailbox_state(&jobs[0]).unwrap().as_deref(),
            Some("running")
        );
        assert_eq!(
            db.run_status(&child_runs[0]).unwrap().as_deref(),
            Some("running")
        );
    }

    #[test]
    fn batch_dry_completion_commits_preflight_digest_with_child_terminal_state() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-preflight-terminal",
                "source",
                "destination",
                &[("one".into(), "one".into())],
            )
            .unwrap();
        let child_runs = db
            .begin_batch_run_with_children(
                &project.id,
                &jobs,
                "run-preflight-parent",
                "batch validation",
                &[],
                "snapshot",
                &[],
            )
            .unwrap();
        db.claim_batch_mailbox_for_child(
            &project.id,
            &jobs[0],
            "run-preflight-parent",
            &child_runs[0],
        )
        .unwrap();
        let digest = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        db.finish_run_for_mailbox_with_preflight_plan_and_checkpoint(
            &project.id,
            &jobs[0],
            &child_runs[0],
            "completed",
            "ready",
            "dry preflight passed",
            Some(digest),
            None,
        )
        .unwrap();
        assert_eq!(
            db.preflight_plan(&jobs[0]).unwrap().as_deref(),
            Some(digest)
        );
        assert_eq!(
            db.mailbox_state(&jobs[0]).unwrap().as_deref(),
            Some("ready")
        );
        assert_eq!(
            db.run_status(&child_runs[0]).unwrap().as_deref(),
            Some("completed")
        );
    }

    #[test]
    fn batch_start_rejects_duplicate_mailbox_ids_atomically() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "duplicate-selection",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();

        assert!(
            db.begin_batch_run_with_children(
                &project.id,
                &[jobs[0].clone(), jobs[0].clone()],
                "run-duplicate-selection",
                "batch",
                &[],
                "snapshot",
                &[],
            )
            .is_err()
        );
        assert_eq!(db.run_status("run-duplicate-selection").unwrap(), None);
        assert_eq!(
            db.mailbox_state(&jobs[0]).unwrap().as_deref(),
            Some("queued")
        );
    }

    #[test]
    fn batch_start_rejects_duplicate_durable_destinations() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("legacy-duplicate-target", "source", "destination")
            .unwrap();
        let first = db
            .add_mailbox(&project.id, "one", "target@example.test")
            .unwrap();
        let second = Uuid::new_v4().to_string();
        db.connection
            .execute(
                "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,state) VALUES(?1,?2,?3,?4,'queued')",
                params![second, project.id, "two", " Target@Example.Test "],
            )
            .unwrap();

        assert!(
            db.begin_batch_run_with_children(
                &project.id,
                &[first, second],
                "run-legacy-duplicate-target",
                "batch",
                &[],
                "snapshot",
                &[],
            )
            .is_err()
        );
        assert_eq!(db.run_status("run-legacy-duplicate-target").unwrap(), None);
    }

    #[test]
    fn preflight_storage_rejects_non_digest_values() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("digest", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();

        assert!(
            db.set_preflight_plan(&job, "raw command arguments")
                .is_err()
        );
        assert!(db.preflight_plan(&job).unwrap().is_none());
        let digest = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        assert!(db.set_preflight_plan("missing-job", digest).is_err());
        db.set_preflight_plan(&job, digest).unwrap();
        assert_eq!(db.preflight_plan(&job).unwrap().as_deref(), Some(digest));
    }

    #[test]
    fn changing_preflight_plan_invalidates_dovecot_checkpoint() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("checkpoint-plan", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source@example", "destination@example")
            .unwrap();
        let first_plan = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let second_plan = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        db.set_preflight_plan(&job, first_plan).unwrap();
        db.begin_run(&project.id, &job, "checkpoint-plan-run", "dovecot")
            .unwrap();
        db.finish_run_for_mailbox_with_checkpoint(
            &project.id,
            &job,
            "checkpoint-plan-run",
            "completed",
            "completed",
            "",
            Some("AQAAAHm4+Jk="),
        )
        .unwrap();

        db.set_preflight_plan(&job, first_plan).unwrap();
        assert!(db.mailbox_checkpoint(&job).unwrap().is_some());
        db.set_preflight_plan(&job, second_plan).unwrap();
        assert_eq!(db.mailbox_checkpoint(&job).unwrap(), None);
        assert_eq!(
            db.preflight_plan(&job).unwrap().as_deref(),
            Some(second_plan)
        );
    }

    #[test]
    fn recovery_abandons_unclaimed_batch_children_without_marking_them_running() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-before-claim-crash",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        let child_runs = db
            .begin_batch_run_with_children(
                &project.id,
                &jobs,
                "run-parent-before-claim",
                "test",
                &[],
                "batch snapshot",
                &[],
            )
            .unwrap();

        assert_eq!(db.recover_abandoned_jobs().unwrap(), 0);
        assert_eq!(
            db.run_status("run-parent-before-claim").unwrap().as_deref(),
            Some("abandoned")
        );
        for (job, child_run) in jobs.iter().zip(child_runs) {
            assert_eq!(db.mailbox_state(job).unwrap().as_deref(), Some("queued"));
            assert_eq!(
                db.run_status(&child_run).unwrap().as_deref(),
                Some("abandoned")
            );
        }

        db.begin_batch_run(
            &project.id,
            &jobs,
            "run-retry-after-before-claim",
            "test",
            &[],
        )
        .unwrap();
    }

    #[test]
    fn recovery_preserves_unverified_processes_until_explicit_review() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "preserve-unverified-process",
                "source",
                "destination",
                &[("one".into(), "one".into())],
            )
            .unwrap();
        let child_runs = db
            .begin_batch_run_with_children(
                &project.id,
                &jobs,
                "run-preserve-unverified-parent",
                "test",
                &[],
                "batch snapshot",
                &[],
            )
            .unwrap();
        db.claim_batch_mailbox_for_child(
            &project.id,
            &jobs[0],
            "run-preserve-unverified-parent",
            &child_runs[0],
        )
        .unwrap();
        let process = ActiveProcess {
            run_id: child_runs[0].clone(),
            job_id: jobs[0].clone(),
            pid: 4242,
            start_ticks: None,
            process_group: None,
            session_id: None,
            executable: "test".into(),
        };
        db.register_process(&process).unwrap();

        db.recover_abandoned_jobs_preserving(std::slice::from_ref(&process))
            .unwrap();
        assert_eq!(db.active_processes().unwrap(), vec![process]);
        assert_eq!(
            db.mailbox_attention_reason(&jobs[0]).unwrap(),
            Some(AttentionReason::ProcessIdentityUnverified)
        );

        db.clear_active_processes_after_review().unwrap();
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn attention_reason_is_durable_and_has_safe_operator_guidance() {
        assert_eq!(
            AttentionReason::parse("verification_difference"),
            Some(AttentionReason::VerificationDifference)
        );
        assert_eq!(AttentionReason::parse("future_reason"), None);
        assert!(
            AttentionReason::VerificationDifference
                .recommended_action()
                .contains("Review")
        );

        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("attention", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.set_mailbox_state_for_test(&job, "attention").unwrap();
        db.connection
            .execute(
                "UPDATE mailbox_jobs SET attention_reason='future_reason' WHERE id=?1",
                [&job],
            )
            .unwrap();
        assert_eq!(
            db.mailbox_attention_reason(&job).unwrap(),
            Some(AttentionReason::Unknown)
        );
    }

    #[test]
    fn structured_attention_reason_takes_precedence_over_human_detail() {
        assert_eq!(
            attention_reason_for(
                "failed",
                "[attention_reason=message_rejected] [class=message] destination refused APPEND",
            ),
            Some(AttentionReason::MessageRejected)
        );
        assert_eq!(
            attention_reason_for(
                "failed",
                "[attention_reason=not-a-real-reason] [class=transport] connection reset",
            ),
            Some(AttentionReason::TransportFailed)
        );
    }

    #[test]
    fn project_attention_reasons_are_loaded_as_one_read_model() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("attention-map", "source", "destination")
            .unwrap();
        let first = db
            .add_mailbox(&project.id, "source-a", "destination-a")
            .unwrap();
        let second = db
            .add_mailbox(&project.id, "source-b", "destination-b")
            .unwrap();
        db.set_mailbox_state_for_test(&first, "attention").unwrap();
        db.set_mailbox_state_for_test(&second, "failed").unwrap();
        db.connection
            .execute(
                "UPDATE mailbox_jobs SET attention_reason='authentication_failed' WHERE id=?1",
                [&first],
            )
            .unwrap();
        db.connection
            .execute(
                "UPDATE mailbox_jobs SET attention_reason='capacity_limited' WHERE id=?1",
                [&second],
            )
            .unwrap();

        let reasons = db.mailbox_attention_reasons(&project.id).unwrap();
        assert_eq!(
            reasons.get(&first),
            Some(&AttentionReason::AuthenticationFailed)
        );
        assert_eq!(
            reasons.get(&second),
            Some(&AttentionReason::CapacityLimited)
        );
    }

    #[test]
    fn failed_and_cancelled_runs_retain_structured_attention_reasons() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("failure-reasons", "source", "destination")
            .unwrap();
        let auth_job = db
            .add_mailbox(&project.id, "auth-source", "auth-destination")
            .unwrap();
        db.begin_run(&project.id, &auth_job, "auth-run", "imapsync")
            .unwrap();
        db.finish_run_for_mailbox_with_checkpoint(
            &project.id,
            &auth_job,
            "auth-run",
            "failed",
            "failed",
            "IMAP authentication failed",
            None,
        )
        .unwrap();
        assert_eq!(
            db.mailbox_attention_reason(&auth_job).unwrap(),
            Some(AttentionReason::AuthenticationFailed)
        );

        let cancelled_job = db
            .add_mailbox(&project.id, "cancel-source", "cancel-destination")
            .unwrap();
        db.begin_run(&project.id, &cancelled_job, "cancel-run", "imapsync")
            .unwrap();
        db.finish_run_for_mailbox_with_checkpoint(
            &project.id,
            &cancelled_job,
            "cancel-run",
            "cancelled",
            "cancelled",
            "operator cancelled migration",
            None,
        )
        .unwrap();
        assert_eq!(
            db.mailbox_attention_reason(&cancelled_job).unwrap(),
            Some(AttentionReason::Interrupted)
        );
    }

    #[test]
    fn recovery_abandons_queued_and_claimed_batch_children_distinctly() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-partial-claim-crash",
                "source",
                "destination",
                &[
                    ("one".into(), "one".into()),
                    ("two".into(), "two".into()),
                    ("three".into(), "three".into()),
                ],
            )
            .unwrap();
        let child_runs = db
            .begin_batch_run_with_children(
                &project.id,
                &jobs,
                "run-parent-partial-claim",
                "test",
                &[],
                "batch snapshot",
                &[],
            )
            .unwrap();
        db.claim_batch_mailbox_for_child(
            &project.id,
            &jobs[0],
            "run-parent-partial-claim",
            &child_runs[0],
        )
        .unwrap();
        db.register_process(&ActiveProcess {
            run_id: child_runs[0].clone(),
            job_id: jobs[0].clone(),
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        })
        .unwrap();

        assert_eq!(db.recover_abandoned_jobs().unwrap(), 1);
        assert_eq!(
            db.mailbox_state(&jobs[0]).unwrap().as_deref(),
            Some("attention")
        );
        assert_eq!(
            db.mailbox_state(&jobs[1]).unwrap().as_deref(),
            Some("queued")
        );
        assert_eq!(
            db.mailbox_state(&jobs[2]).unwrap().as_deref(),
            Some("queued")
        );
        assert!(jobs.iter().all(|job| {
            db.latest_run(job)
                .unwrap()
                .is_some_and(|run| run.status == "abandoned")
        }));
        assert!(db.active_processes().unwrap().is_empty());

        db.begin_batch_run(
            &project.id,
            &jobs,
            "run-retry-after-partial-claim",
            "test",
            &[],
        )
        .unwrap();
    }

    #[test]
    fn begin_batch_run_checks_all_live_plans_in_one_boundary() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-plans",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        let plan_one = "1111111111111111111111111111111111111111111111111111111111111111";
        let plan_two = "2222222222222222222222222222222222222222222222222222222222222222";
        db.set_preflight_plan(&jobs[0], plan_one).unwrap();
        db.set_preflight_plan(&jobs[1], plan_two).unwrap();
        let result = db.begin_batch_run(
            &project.id,
            &jobs,
            "run-batch-plans",
            "test",
            &[
                plan_one.into(),
                "3333333333333333333333333333333333333333333333333333333333333333".into(),
            ],
        );
        assert!(result.is_err());
        assert!(
            jobs.iter()
                .all(|job| { db.mailbox_state(job).unwrap().as_deref() == Some("queued") })
        );
        assert!(db.run_status("run-batch-plans").unwrap().is_none());
    }

    #[test]
    fn raw_output_events_are_not_committed_to_the_ledger() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("events", "source", "destination")
            .unwrap();
        let details: Vec<String> = vec!["first".into(), "second".into(), "third".into()];
        let batch = details
            .iter()
            .map(|detail| (project.id.as_str(), "run_output", detail.as_str()))
            .collect::<Vec<_>>();
        db.record_events_batch(&batch).unwrap();
        let count: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE project_id=?1 AND kind='run_output'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);

        db.connection
            .execute(
                "INSERT INTO events(project_id,kind,detail) VALUES(?1,'run_output',?2)",
                params![
                    project.id,
                    "Subject: confidential customer migration fixture"
                ],
            )
            .unwrap();
        db.migrate().unwrap();
        let purged: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE project_id=?1 AND kind='run_output'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(purged, 0);
    }

    #[test]
    fn run_events_are_bound_to_their_durable_run() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("run-events", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source-user", "destination-user")
            .unwrap();
        db.begin_run(&project.id, &job, "run-events-1", "test")
            .unwrap();

        db.record_run_events_batch(
            "run-events-1",
            &[
                ("run_output", "transfer output"),
                ("verification_evidence", "aggregate"),
            ],
        )
        .unwrap();

        let rows: Vec<(String, String, String)> = db
            .connection
            .prepare("SELECT project_id, run_id, detail FROM events WHERE run_id=?1 ORDER BY id")
            .unwrap()
            .query_map(["run-events-1"], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(project_id, run_id, _)| {
            project_id == &project.id && run_id == "run-events-1"
        }));
        assert!(rows.iter().any(|(_, _, detail)| detail == "aggregate"));
        db.finish_run_for_mailbox(
            &project.id,
            &job,
            "run-events-1",
            "failed",
            "failed",
            "terminal failure",
        )
        .unwrap();
        assert!(
            db.record_run_events_batch("run-events-1", &[("run_output", "late output")])
                .is_err()
        );
    }

    #[test]
    fn multi_run_events_keep_child_diagnostic_ownership() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("multi-run-events", "source", "destination")
            .unwrap();
        let first = db
            .add_mailbox(&project.id, "source-one", "destination-one")
            .unwrap();
        let second = db
            .add_mailbox(&project.id, "source-two", "destination-two")
            .unwrap();
        db.begin_run(&project.id, &first, "child-run-one", "test")
            .unwrap();
        db.begin_run(&project.id, &second, "child-run-two", "test")
            .unwrap();

        db.record_events_for_runs_batch(&[
            ("child-run-one", "run_output", "one output"),
            ("child-run-two", "run_output", "two output"),
        ])
        .unwrap();

        let first_count: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE detail='one output'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let second_count: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE detail='two output'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(first_count, 0);
        assert_eq!(second_count, 0);
    }

    #[test]
    fn multi_run_event_batch_rolls_back_when_one_run_is_not_active() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("atomic-run-events", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "active-run", "test")
            .unwrap();

        let result = db.record_events_for_runs_batch(&[
            ("active-run", "run_output", "must roll back"),
            ("missing-run", "run_output", "must never persist"),
        ]);
        assert!(result.is_err());
        let count: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE run_id='active-run' AND kind='run_output'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn retry_buffered_output_can_commit_after_child_is_released() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "retry-output",
                "source",
                "destination",
                &[("one".into(), "one".into())],
            )
            .unwrap();
        let child_runs = db
            .begin_batch_run_with_children(
                &project.id,
                &jobs,
                "retry-output-parent",
                "batch",
                &[],
                "snapshot",
                &[],
            )
            .unwrap();
        db.claim_batch_mailbox_for_child(
            &project.id,
            &jobs[0],
            "retry-output-parent",
            &child_runs[0],
        )
        .unwrap();
        db.release_batch_mailbox_for_retry(
            &project.id,
            &jobs[0],
            "retry-output-parent",
            &child_runs[0],
        )
        .unwrap();

        db.record_events_for_runs_batch(&[(&child_runs[0], "run_output", "retry detail")])
            .unwrap();
        let stored: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE detail='retry detail'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, 0);
    }

    #[test]
    fn durable_diagnostic_event_detail_is_bounded() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("detail-limit", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source-user", "destination-user")
            .unwrap();
        let run_id = "run-detail-limit";
        db.begin_run(&project.id, &job, run_id, "imapsync").unwrap();
        let oversized = "é".repeat(MAX_DURABLE_EVENT_DETAIL_BYTES * 2);

        db.record_run_events_batch(run_id, &[("run_output", oversized.as_str())])
            .unwrap();

        let stored_count: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE run_id=?1 AND kind='run_output'",
                [run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_count, 0);

        db.record_run_events_batch(run_id, &[("verification_pending", oversized.as_str())])
            .unwrap();
        let stored_error: String = db
            .connection
            .query_row(
                "SELECT detail FROM events WHERE run_id=?1 AND kind='verification_pending'",
                [run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stored_error.len() <= MAX_DURABLE_EVENT_DETAIL_BYTES);
        assert!(stored_error.ends_with(DURABLE_EVENT_TRUNCATION_SUFFIX));

        db.record_event(&project.id, "operator_error", oversized.as_str())
            .unwrap();
        let stored_direct: String = db
            .connection
            .query_row(
                "SELECT detail FROM events WHERE project_id=?1 AND kind='operator_error'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stored_direct.len() <= MAX_DURABLE_EVENT_DETAIL_BYTES);
        assert!(stored_direct.ends_with(DURABLE_EVENT_TRUNCATION_SUFFIX));
    }

    #[test]
    fn evidence_rejects_orphaned_run_ids() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("audit", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 1,
            destination_bytes: 1,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };
        assert!(
            db.record_evidence_for_run(&job, "missing", &evidence)
                .is_err()
        );
    }

    #[test]
    fn evidence_rejects_a_run_for_a_different_mailbox() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "evidence-ownership",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        db.begin_run(&project.id, &jobs[0], "evidence-run-one", "test")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 1,
            destination_bytes: 1,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
        };

        assert!(
            db.record_evidence_for_run(&jobs[1], "evidence-run-one", &evidence)
                .is_err()
        );
        assert!(db.evidence(&jobs[1]).unwrap().is_none());
    }

    #[test]
    fn recent_projects_returns_compact_rows_for_workspace_selection() {
        let db = StateStore::in_memory().unwrap();
        let first = db
            .create_project("First customer", "source-a", "destination-a")
            .unwrap();
        let second = db
            .create_project("Second customer", "source-b", "destination-b")
            .unwrap();

        let projects = db.recent_projects(1).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, second.id);
        assert_eq!(projects[0].name, "Second customer");
        assert_eq!(projects[0].phase, Phase::Discovery);
        assert_ne!(projects[0].id, first.id);
    }

    #[test]
    fn read_model_revision_advances_after_durable_events() {
        let db = StateStore::in_memory().unwrap();
        let before = db.read_model_revision().unwrap();
        db.create_project("Revision test", "source", "destination")
            .unwrap();
        let after = db.read_model_revision().unwrap();
        assert!(after > before);
    }

    #[test]
    fn project_read_model_revision_ignores_unrelated_projects() {
        let db = StateStore::in_memory().unwrap();
        let first = db
            .create_project("First revision test", "source", "destination")
            .unwrap();
        let first_revision = db.project_read_model_revision(&first.id).unwrap();
        db.create_project("Second revision test", "source", "destination")
            .unwrap();
        assert_eq!(
            db.project_read_model_revision(&first.id).unwrap(),
            first_revision
        );
    }
}
