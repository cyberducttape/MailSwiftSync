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
mod cutover;
mod database;
mod engine;
mod events;
mod evidence;
mod evidence_ops;
mod mailboxes;
mod message_extraction;
mod message_staging;
mod message_verification;
mod models;
mod phases;
mod policy;
pub(crate) mod post_migration_report;
pub(crate) mod pre_migration_report;
mod projects;
pub(crate) mod provider_intelligence;
pub(crate) mod provider_runbooks;
mod queries;
mod queue;
mod recovery;
pub(crate) mod recovery_dashboard;
mod recovery_queue;
mod reports;
mod run_queries;
mod runs;
mod state;
mod transfer_passes;
mod webhook_outbox;
pub use capabilities::{NamespaceEntry, NamespaceInfo, ServerCapabilities};
pub(crate) use cutover::CutoverStage;
pub use engine::Engine;
#[allow(unused_imports)]
pub use evidence::{
    EvidenceScope, MailboxAssurance, MailboxEvidence, ProjectReportSnapshot, ReportMailboxPage,
    ReportMailboxSnapshot, ReportRunSnapshot, VerificationAcceptance, VerificationEvidence,
    VerificationMethod, VerificationOutcome,
};
pub(crate) use message_extraction::{ExtractedMessage, ExtractedMessages, MailboxMessageKey};
pub(crate) use message_staging::{
    FolderCursor, FolderSnapshot, MessageMetadataStage, StagedMessageSide, durable_stage_path,
};
pub(crate) use message_verification::{MessageMismatch, MessageVerification, MismatchType};
pub use models::{
    ActiveProcess, BatchAdmissionState, BatchChildPlan, MailboxJob, MailboxPage,
    MailboxStateCounts, MailboxStatusPage, Project, ProjectListItem, RunListItem, RunSummary,
};
pub use policy::destination_identity_from_parts;
pub use queue::{QueueInsert, QueuePlanRow, QueueRow, QueueRowFacts};
pub use recovery_queue::{RecoveryGroup, RecoveryRow};
pub use state::{AttentionReason, MailboxState, Phase};
pub use transfer_passes::{
    EngineCompletionCounters, PassSide, TransferPassCompletion, TransferPassFolder,
    TransferPassIntent, TransferPassRecord, folder_digest, sha256_hex,
};
pub const CURRENT_SCHEMA_VERSION: i64 = 21;
pub(crate) const DESTINATION_IDENTITY_SCHEMA_VERSION: i64 = 13;
pub(crate) const MAX_DURABLE_MAILBOX_ROWS: usize = 100_000;

pub(crate) use policy::{
    MAX_PERSISTED_PROFILE_BYTES, MAX_TOTAL_PERSISTED_PROFILE_BYTES, attention_reason_for,
    dovecot_checkpoint_context, dovecot_checkpoint_state, encode_dovecot_checkpoint,
    normalized_destination_identity, normalized_destination_lock_identity,
    valid_dovecot_checkpoint, valid_mailbox_transition,
};

pub(crate) fn sqlite_i64(value: u64) -> rusqlite::Result<i64> {
    i64::try_from(value).map_err(|_| rusqlite::Error::InvalidQuery)
}

pub(crate) fn sqlite_u64(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| rusqlite::Error::InvalidQuery)
}

pub(crate) fn sqlite_usize(value: i64) -> rusqlite::Result<usize> {
    usize::try_from(value).map_err(|_| rusqlite::Error::InvalidQuery)
}

pub(crate) fn sqlite_optional_i64(value: Option<u64>) -> rusqlite::Result<Option<i64>> {
    value.map(sqlite_i64).transpose()
}

pub(crate) fn sqlite_optional_u64(value: Option<i64>) -> rusqlite::Result<Option<u64>> {
    value.map(sqlite_u64).transpose()
}

pub struct StateStore {
    connection: Connection,
}

impl StateStore {
    #[cfg(test)]
    pub(crate) fn insert_run_for_test(
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
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "state database path is a symbolic link",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if path.exists() {
        if !path.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "state database path is not a regular file",
            ));
        }
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
#[path = "core/tests.rs"]
mod tests;
