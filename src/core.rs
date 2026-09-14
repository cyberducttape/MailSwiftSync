//! Durable migration-control-plane primitives.
//!
//! The GUI may be replaced, but project state and verification evidence remain
//! portable SQLite data. No credentials or message content belong in this store.

use crate::storage::bounded_event_detail as bound_event_detail;
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
mod engine;
mod evidence;
mod models;
mod state;
pub use capabilities::ServerCapabilities;
pub use engine::Engine;
pub use evidence::{
    EvidenceScope, MailboxEvidence, ProjectReportSnapshot, ReportMailboxSnapshot,
    ReportRunSnapshot, VerificationAcceptance,
};
pub use models::{
    ActiveProcess, BatchAdmissionState, BatchChildPlan, MailboxJob, MailboxStateCounts, Project,
    ProjectListItem, RunListItem, RunSummary,
};
pub use state::{AttentionReason, MailboxState, Phase};

fn normalized_destination_identity(destination_mailbox: &str, config: Option<&str>) -> String {
    if let Some(config) = config
        && let Ok(value) = toml::from_str::<toml::Value>(config)
    {
        let host = value
            .get("destination_host")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|host| !host.is_empty());
        let user = value
            .get("destination_user")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|user| !user.is_empty());
        if let (Some(host), Some(user)) = (host, user) {
            let tls = value
                .get("destination_tls")
                .and_then(toml::Value::as_str)
                .unwrap_or("imaps");
            let port = value
                .get("destination_port")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .unwrap_or_default();
            if let Ok(identity) =
                crate::endpoint::canonical_destination_identity(user, host, tls, port)
            {
                return identity;
            }
        }
    }
    crate::endpoint::mailbox_identity(destination_mailbox)
}

const MAX_DOVECOT_CHECKPOINT_BYTES: usize = 4096;
pub const CURRENT_SCHEMA_VERSION: i64 = 6;

fn attention_reason_for(mailbox_state: &str, detail: &str) -> Option<AttentionReason> {
    if mailbox_state == "verification_difference" {
        return Some(AttentionReason::VerificationDifference);
    }
    if !matches!(mailbox_state, "attention" | "failed" | "cancelled") {
        return None;
    }
    if let Some(reason) = detail
        .strip_prefix("[attention_reason=")
        .and_then(|value| value.split_once(']'))
        .and_then(|(value, _)| AttentionReason::parse(value))
    {
        return Some(reason);
    }
    let detail = detail.to_ascii_lowercase();
    if detail.contains("identity") || detail.contains("ownership") || detail.contains("unverified")
    {
        return Some(AttentionReason::ProcessIdentityUnverified);
    }
    if detail.contains("restart") || detail.contains("interrupt") {
        return Some(AttentionReason::Interrupted);
    }
    if detail.contains("verification") || detail.contains("evidence") {
        return Some(AttentionReason::VerificationIncomplete);
    }
    if detail.contains("auth") || detail.contains("credential") || detail.contains("password") {
        return Some(AttentionReason::AuthenticationFailed);
    }
    if detail.contains("rate") || detail.contains("quota") || detail.contains("capacity") {
        return Some(AttentionReason::CapacityLimited);
    }
    if detail.contains("policy") {
        return Some(AttentionReason::PolicyBlocked);
    }
    if detail.contains("config") || detail.contains("invalid") {
        return Some(AttentionReason::ConfigurationInvalid);
    }
    if detail.contains("network") || detail.contains("timeout") || detail.contains("connection") {
        return Some(AttentionReason::TransportFailed);
    }
    if mailbox_state == "cancelled" {
        return Some(AttentionReason::Interrupted);
    }
    if mailbox_state == "failed" {
        return Some(AttentionReason::Unknown);
    }
    Some(AttentionReason::Unknown)
}

pub(crate) fn valid_dovecot_checkpoint(value: &str) -> bool {
    if value != value.trim()
        || value.len() < 8
        || value.len() > MAX_DOVECOT_CHECKPOINT_BYTES
        || value.bytes().any(|byte| byte.is_ascii_whitespace())
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "success" | "successful" | "completed"
        )
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'+' | b'/' | b'='))
    {
        return false;
    }
    let padding_start = value.find('=');
    if let Some(index) = padding_start {
        if !value.len().is_multiple_of(4)
            || value[index..].len() > 2
            || value[index..].bytes().any(|byte| byte != b'=')
        {
            return false;
        }
    } else if value.len() % 4 == 1 {
        return false;
    }
    let mut padded = value.to_owned();
    while !padded.len().is_multiple_of(4) {
        padded.push('=');
    }
    let Ok(decoded) = BASE64_STANDARD.decode(padded) else {
        return false;
    };

    // This mirrors Dovecot's documented dsync state format rather than
    // treating every syntactically valid Base64 string as a checkpoint:
    // v0's empty state is four zero bytes; v1 is a four-byte header followed
    // by fixed-size mailbox records and a little-endian CRC32.
    if decoded == [0, 0, 0, 0] {
        return true;
    }
    const HEADER_SIZE: usize = 4;
    const CRC_SIZE: usize = 4;
    const MAILBOX_STATE_SIZE: usize = 44;
    if decoded.len() < HEADER_SIZE + CRC_SIZE
        || decoded[..HEADER_SIZE] != [1, 0, 0, 0]
        || !(decoded.len() - HEADER_SIZE - CRC_SIZE).is_multiple_of(MAILBOX_STATE_SIZE)
    {
        return false;
    }
    let checksum_offset = decoded.len() - CRC_SIZE;
    let expected = u32::from_le_bytes([
        decoded[checksum_offset],
        decoded[checksum_offset + 1],
        decoded[checksum_offset + 2],
        decoded[checksum_offset + 3],
    ]);
    let mut hasher = Crc32Hasher::new();
    hasher.update(&decoded[..checksum_offset]);
    hasher.finalize() == expected
}

pub struct StateStore {
    connection: Connection,
}

use bound_event_detail as bounded_event_detail_raw;

fn bounded_event_detail(_kind: &str, detail: &str) -> String {
    bounded_event_detail_raw(detail)
}

impl StateStore {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let path = path.as_ref();
        prepare_database_file(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let store = Self {
            connection: Connection::open(path)?,
        };
        let stored_schema_version: i64 =
            store
                .connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if stored_schema_version < CURRENT_SCHEMA_VERSION
            && std::fs::metadata(path)
                .map(|metadata| metadata.len() > 0)
                .unwrap_or(false)
        {
            // Preserve the exact pre-migration ledger before any schema
            // rewrite. The backup is unique and non-overwriting, so a failed
            // upgrade never destroys the last recovery artifact.
            let backup_path = migration_backup_path(path, stored_schema_version);
            store.backup_to(&backup_path)?;
        }
        restrict_database_permissions(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        store.migrate()?;
        restrict_database_sidecars(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(store)
    }

    /// Open an existing ledger without taking the application lock or
    /// mutating its file. Current-schema ledgers are observed directly through
    /// SQLite's WAL snapshot semantics. Older ledgers are copied into a
    /// private in-memory database and migrated there, so recovery/status tools
    /// can inspect historical state without rewriting the source file.
    pub fn open_readonly(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        let stored_schema_version: i64 =
            connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if stored_schema_version > CURRENT_SCHEMA_VERSION {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if stored_schema_version == CURRENT_SCHEMA_VERSION {
            return Ok(Self { connection });
        }

        let mut migrated = Connection::open_in_memory()?;
        {
            let backup = backup::Backup::new(&connection, &mut migrated)?;
            backup.run_to_completion(128, std::time::Duration::from_millis(1), None)?;
        }
        let store = Self {
            connection: migrated,
        };
        store.migrate()?;
        Ok(store)
    }
    pub fn in_memory() -> rusqlite::Result<Self> {
        let store = Self {
            connection: Connection::open_in_memory()?,
        };
        store.migrate()?;
        Ok(store)
    }

    /// Create a consistent SQLite backup without copying WAL/SHM files by
    /// hand. The destination must not already exist, preventing an operator
    /// typo from silently overwriting a prior recovery artifact.
    pub fn backup_to(&self, destination: &Path) -> rusqlite::Result<()> {
        if destination.exists() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let destination_string = destination.to_string_lossy().into_owned();
        self.connection
            .execute("VACUUM INTO ?1", [&destination_string])?;
        restrict_database_permissions(destination)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let backup = Connection::open(destination)?;
        let integrity: String = backup.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        restrict_database_sidecars(destination)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(())
    }

    /// Snapshot an on-disk ledger through SQLite's backup API. Unlike a file
    /// copy, this includes committed pages currently visible through a WAL
    /// and produces a standalone database without `-wal`/`-shm` sidecars.
    pub fn snapshot_to(source: &Path, destination: &Path) -> rusqlite::Result<()> {
        if destination.exists() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let source_connection =
            Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        source_connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        let integrity: String =
            source_connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(rusqlite::Error::InvalidQuery);
        }

        let mut destination_connection = Connection::open(destination)?;
        {
            let backup = backup::Backup::new(&source_connection, &mut destination_connection)?;
            backup.run_to_completion(128, std::time::Duration::from_millis(1), None)?;
        }
        let integrity: String =
            destination_connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        drop(destination_connection);
        restrict_database_permissions(destination)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        restrict_database_sidecars(destination)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(())
    }
    fn migrate(&self) -> rusqlite::Result<()> {
        // Version 2 adds the durable endpoint-qualified destination identity;
        // version 3 records lifecycle provenance for each admitted run;
        // version 4 adds a stable operator-review reason for attention rows;
        // version 5 adds durable verification-exception acceptance records;
        // version 6 records observed engine-version metadata per run.
        // Keep the compatibility column checks below for pre-versioned alpha
        // databases, then stamp the completed layout explicitly.
        let stored_schema_version: i64 =
            self.connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if stored_schema_version > CURRENT_SCHEMA_VERSION {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // These connection-level settings must be applied outside the schema
        // transaction. Table/index creation itself is deliberately below,
        // inside that transaction, so a failed first open cannot leave a
        // partially initialized ledger behind.
        self.connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )?;
        if stored_schema_version == CURRENT_SCHEMA_VERSION {
            // A version number alone is not sufficient: an older alpha can
            // have stamped the version before a later repair, and tests or a
            // manually recovered ledger may contain invalid rows. Perform a
            // small invariant probe before skipping the migration transaction
            // rather than rewriting every row on every application launch.
            let current_schema_is_clean = (|| -> rusqlite::Result<bool> {
                let indexes: i64 = self.connection.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN ('one_running_run_per_job','one_active_run_per_job','idx_events_project_kind_id')",
                    [],
                    |row| row.get(0),
                )?;
                if indexes != 3 {
                    return Ok(false);
                }
                let attention_reason_column: i64 = self.connection.query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('mailbox_jobs') WHERE name='attention_reason'",
                    [],
                    |row| row.get(0),
                )?;
                if attention_reason_column != 1 {
                    return Ok(false);
                }
                let acceptance_table: i64 = self.connection.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='verification_acceptances'",
                    [],
                    |row| row.get(0),
                )?;
                if acceptance_table != 1 {
                    return Ok(false);
                }
                let engine_versions_table: i64 = self.connection.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='engine_versions'",
                    [],
                    |row| row.get(0),
                )?;
                if engine_versions_table != 1 {
                    return Ok(false);
                }
                let legacy_plan: i64 = self.connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE preflight_plan IS NOT NULL AND (length(preflight_plan) <> 64 OR preflight_plan GLOB '*[^0-9A-Fa-f]*'))",
                    [],
                    |row| row.get(0),
                )?;
                if legacy_plan != 0 {
                    return Ok(false);
                }
                let duplicate_active_run: i64 = self.connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM (SELECT job_id FROM runs WHERE job_id IS NOT NULL AND status IN ('queued','running') GROUP BY job_id HAVING COUNT(*) > 1))",
                    [],
                    |row| row.get(0),
                )?;
                Ok(duplicate_active_run == 0)
            })()
            .unwrap_or(false);
            if current_schema_is_clean {
                let tx = self.connection.unchecked_transaction()?;
                Self::refresh_destination_identities(&tx)?;
                Self::purge_raw_output_events(&tx)?;
                tx.commit()?;
                return Ok(());
            }
        }
        // Keep all compatibility repairs, constraint creation, and the
        // version stamp in one transaction. If an upgrade fails halfway
        // through, SQLite can roll back to the prior durable ledger.
        let tx = self.connection.unchecked_transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, source_endpoint TEXT NOT NULL, destination_endpoint TEXT NOT NULL, phase TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             CREATE TABLE IF NOT EXISTS mailbox_jobs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), source_mailbox TEXT NOT NULL, destination_mailbox TEXT NOT NULL, destination_identity TEXT NOT NULL DEFAULT '', state TEXT NOT NULL, attempt INTEGER NOT NULL DEFAULT 0, checkpoint TEXT, preflight_plan TEXT, config TEXT, attention_reason TEXT);
             CREATE TABLE IF NOT EXISTS evidence (job_id TEXT PRIMARY KEY REFERENCES mailbox_jobs(id), source_messages INTEGER NOT NULL, destination_messages INTEGER NOT NULL, source_bytes INTEGER NOT NULL, destination_bytes INTEGER NOT NULL, unmatched_messages INTEGER NOT NULL, failed_messages INTEGER NOT NULL, source_folders INTEGER NOT NULL DEFAULT 0, destination_folders INTEGER NOT NULL DEFAULT 0, authoritative INTEGER NOT NULL DEFAULT 0, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             CREATE TABLE IF NOT EXISTS evidence_history (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL, source_messages INTEGER NOT NULL, destination_messages INTEGER NOT NULL, source_bytes INTEGER NOT NULL, destination_bytes INTEGER NOT NULL, unmatched_messages INTEGER NOT NULL, failed_messages INTEGER NOT NULL, source_folders INTEGER NOT NULL, destination_folders INTEGER NOT NULL, authoritative INTEGER NOT NULL DEFAULT 0, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), job_id TEXT REFERENCES mailbox_jobs(id), parent_run_id TEXT REFERENCES runs(id), engine TEXT NOT NULL, phase_at_start TEXT NOT NULL DEFAULT 'legacy_unknown', plan_snapshot TEXT NOT NULL DEFAULT '', status TEXT NOT NULL, started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, finished_at TEXT, detail TEXT NOT NULL DEFAULT '');
             CREATE TABLE IF NOT EXISTS active_processes (run_id TEXT NOT NULL REFERENCES runs(id), job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), pid INTEGER NOT NULL, start_ticks INTEGER, process_group INTEGER, session_id INTEGER, executable TEXT NOT NULL DEFAULT '', started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(run_id, job_id));
             CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), run_id TEXT REFERENCES runs(id), kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             CREATE TABLE IF NOT EXISTS verification_acceptances (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), operator TEXT NOT NULL, reason TEXT NOT NULL, accepted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             CREATE TABLE IF NOT EXISTS engine_versions (run_id TEXT PRIMARY KEY REFERENCES runs(id), version TEXT NOT NULL, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             CREATE INDEX IF NOT EXISTS idx_mailbox_jobs_project_state ON mailbox_jobs(project_id, state);
             CREATE INDEX IF NOT EXISTS idx_runs_project_started ON runs(project_id, started_at DESC);
             CREATE INDEX IF NOT EXISTS idx_runs_job_started ON runs(job_id, started_at DESC);
             CREATE INDEX IF NOT EXISTS idx_events_project_created ON events(project_id, created_at DESC);
             CREATE INDEX IF NOT EXISTS idx_events_project_kind_id ON events(project_id, kind, id DESC);
             CREATE INDEX IF NOT EXISTS idx_evidence_history_job_captured ON evidence_history(job_id, captured_at DESC);
             CREATE INDEX IF NOT EXISTS idx_active_processes_pid ON active_processes(pid);",
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_verification_acceptances_job ON verification_acceptances(job_id, id DESC)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_engine_versions_captured ON engine_versions(captured_at DESC)",
            [],
        )?;
        // Existing pre-0.1 databases need the new verification dimensions too.
        let columns = tx
            .prepare("PRAGMA table_info(evidence)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !columns.iter().any(|column| column == "source_folders") {
            tx.execute(
                "ALTER TABLE evidence ADD COLUMN source_folders INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !columns.iter().any(|column| column == "destination_folders") {
            tx.execute(
                "ALTER TABLE evidence ADD COLUMN destination_folders INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !columns.iter().any(|column| column == "authoritative") {
            tx.execute(
                "ALTER TABLE evidence ADD COLUMN authoritative INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        let job_columns = tx
            .prepare("PRAGMA table_info(mailbox_jobs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !job_columns.iter().any(|column| column == "preflight_plan") {
            tx.execute(
                "ALTER TABLE mailbox_jobs ADD COLUMN preflight_plan TEXT",
                [],
            )?;
        }
        if !job_columns.iter().any(|column| column == "config") {
            tx.execute("ALTER TABLE mailbox_jobs ADD COLUMN config TEXT", [])?;
        }
        if !job_columns
            .iter()
            .any(|column| column == "attention_reason")
        {
            tx.execute(
                "ALTER TABLE mailbox_jobs ADD COLUMN attention_reason TEXT",
                [],
            )?;
        }
        if !job_columns
            .iter()
            .any(|column| column == "destination_identity")
        {
            tx.execute(
                "ALTER TABLE mailbox_jobs ADD COLUMN destination_identity TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        // Older releases stored the generated preflight plan itself. Do not
        // carry that potentially sensitive configuration into the hardened
        // schema; an invalidated row must be preflighted again before live
        // admission.
        tx.execute(
            "UPDATE mailbox_jobs SET preflight_plan=NULL WHERE preflight_plan IS NOT NULL AND (length(preflight_plan) <> 64 OR preflight_plan GLOB '*[^0-9A-Fa-f]*')",
            [],
        )?;
        let run_columns = tx
            .prepare("PRAGMA table_info(runs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !run_columns.iter().any(|column| column == "plan_snapshot") {
            tx.execute(
                "ALTER TABLE runs ADD COLUMN plan_snapshot TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        if !run_columns.iter().any(|column| column == "parent_run_id") {
            tx.execute(
                "ALTER TABLE runs ADD COLUMN parent_run_id TEXT REFERENCES runs(id)",
                [],
            )?;
        }
        if !run_columns.iter().any(|column| column == "phase_at_start") {
            tx.execute(
                "ALTER TABLE runs ADD COLUMN phase_at_start TEXT NOT NULL DEFAULT 'legacy_unknown'",
                [],
            )?;
        }
        let event_columns = tx
            .prepare("PRAGMA table_info(events)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !event_columns.iter().any(|column| column == "run_id") {
            tx.execute(
                "ALTER TABLE events ADD COLUMN run_id TEXT REFERENCES runs(id)",
                [],
            )?;
        }
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_run_created ON events(run_id, created_at DESC)",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_project_kind_id ON events(project_id, kind, id DESC)",
            [],
        )?;
        let process_columns = tx
            .prepare("PRAGMA table_info(active_processes)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !process_columns.iter().any(|column| column == "start_ticks") {
            tx.execute(
                "ALTER TABLE active_processes ADD COLUMN start_ticks INTEGER",
                [],
            )?;
        }
        if !process_columns
            .iter()
            .any(|column| column == "process_group")
        {
            tx.execute(
                "ALTER TABLE active_processes ADD COLUMN process_group INTEGER",
                [],
            )?;
        }
        if !process_columns.iter().any(|column| column == "session_id") {
            tx.execute(
                "ALTER TABLE active_processes ADD COLUMN session_id INTEGER",
                [],
            )?;
        }
        if !process_columns.iter().any(|column| column == "executable") {
            tx.execute(
                "ALTER TABLE active_processes ADD COLUMN executable TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        let history_columns = tx
            .prepare("PRAGMA table_info(evidence_history)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !history_columns
            .iter()
            .any(|column| column == "authoritative")
        {
            tx.execute(
                "ALTER TABLE evidence_history ADD COLUMN authoritative INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        // Older alpha versions did not enforce one active run per mailbox.
        // Reconcile those ledgers before creating the partial unique indexes;
        // otherwise an otherwise recoverable database would fail to open.
        Self::reconcile_duplicate_active_runs(&tx)?;
        Self::refresh_destination_identities(&tx)?;
        Self::purge_raw_output_events(&tx)?;
        tx.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS one_running_run_per_job ON runs(job_id) WHERE job_id IS NOT NULL AND status='running';
             CREATE UNIQUE INDEX IF NOT EXISTS one_active_run_per_job ON runs(job_id) WHERE job_id IS NOT NULL AND status IN ('queued','running');",
        )?;
        tx.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;
        tx.commit()?;
        Ok(())
    }

    fn refresh_destination_identities(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
        let rows = tx
            .prepare("SELECT id,destination_mailbox,config FROM mailbox_jobs")?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (job_id, destination, config) in rows {
            let identity = normalized_destination_identity(&destination, config.as_deref());
            tx.execute(
                "UPDATE mailbox_jobs SET destination_identity=?1 WHERE id=?2",
                params![identity, job_id],
            )?;
        }
        Ok(())
    }

    fn purge_raw_output_events(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
        tx.execute("DELETE FROM events WHERE kind='run_output'", [])?;
        Ok(())
    }

    fn reconcile_duplicate_active_runs(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
        let duplicate_jobs = tx
            .prepare(
                "SELECT job_id FROM runs WHERE job_id IS NOT NULL AND status IN ('queued','running') GROUP BY job_id HAVING COUNT(*) > 1",
            )?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if duplicate_jobs.is_empty() {
            return Ok(());
        }
        for job_id in duplicate_jobs {
            let run_ids = tx
                .prepare(
                    "SELECT id FROM runs WHERE job_id=?1 AND status IN ('queued','running') ORDER BY started_at DESC, rowid DESC",
                )?
                .query_map([&job_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for run_id in run_ids.into_iter().skip(1) {
                let project_id: String = tx.query_row(
                    "SELECT project_id FROM runs WHERE id=?1",
                    [&run_id],
                    |row| row.get(0),
                )?;
                tx.execute(
                    "UPDATE runs SET status='abandoned',finished_at=CURRENT_TIMESTAMP,detail='Superseded duplicate active run repaired during schema migration' WHERE id=?1 AND status IN ('queued','running')",
                    [&run_id],
                )?;
                tx.execute(
                    "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'schema_repaired_duplicate_run',?3)",
                    params![project_id, run_id, format!("repaired duplicate active run for mailbox {job_id}")],
                )?;
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub fn create_project(
        &self,
        name: &str,
        source: &str,
        destination: &str,
    ) -> rusqlite::Result<Project> {
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            source_endpoint: source.into(),
            destination_endpoint: destination.into(),
            phase: Phase::Discovery,
        };
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)",
            params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_created','Project created without credentials')",
            [&project.id],
        )?;
        tx.commit()?;
        Ok(project)
    }
    pub fn create_project_with_mailbox(
        &self,
        name: &str,
        source: &str,
        destination: &str,
        source_mailbox: &str,
        destination_mailbox: &str,
    ) -> rusqlite::Result<(Project, String)> {
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            source_endpoint: source.into(),
            destination_endpoint: destination.into(),
            phase: Phase::Discovery,
        };
        let job_id = Uuid::new_v4().to_string();
        let tx = self.connection.unchecked_transaction()?;
        tx.execute("INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)", params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()])?;
        tx.execute("INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,destination_identity,state) VALUES(?1,?2,?3,?4,?5,'queued')", params![job_id, project.id, source_mailbox, destination_mailbox, normalized_destination_identity(destination_mailbox, None)])?;
        tx.execute("INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_created','Project created without credentials')", [&project.id])?;
        tx.commit()?;
        Ok((project, job_id))
    }
    #[cfg(test)]
    pub fn create_project_with_mailboxes(
        &self,
        name: &str,
        source: &str,
        destination: &str,
        mailboxes: &[(String, String)],
    ) -> rusqlite::Result<(Project, Vec<String>)> {
        if mailboxes.is_empty() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut destinations = BTreeSet::new();
        if mailboxes
            .iter()
            .any(|(_, destination)| !destinations.insert(destination.trim().to_ascii_lowercase()))
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            source_endpoint: source.into(),
            destination_endpoint: destination.into(),
            phase: Phase::Discovery,
        };
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)",
            params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()],
        )?;
        let mut ids = Vec::with_capacity(mailboxes.len());
        for (source_mailbox, destination_mailbox) in mailboxes {
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,destination_identity,state) VALUES(?1,?2,?3,?4,?5,'queued')",
                params![id, project.id, source_mailbox, destination_mailbox, normalized_destination_identity(destination_mailbox, None)],
            )?;
            ids.push(id);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_created',?2)",
            params![
                project.id,
                format!("Batch created with {} mailbox jobs", ids.len())
            ],
        )?;
        tx.commit()?;
        Ok((project, ids))
    }
    pub fn create_project_with_mailbox_configs(
        &self,
        name: &str,
        source: &str,
        destination: &str,
        mailboxes: &[(String, String, String)],
    ) -> rusqlite::Result<(Project, Vec<String>)> {
        if mailboxes.is_empty() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut destinations = BTreeSet::new();
        if mailboxes.iter().any(|(_, destination, config)| {
            !destinations.insert(normalized_destination_identity(destination, Some(config)))
        }) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            source_endpoint: source.into(),
            destination_endpoint: destination.into(),
            phase: Phase::Discovery,
        };
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)",
            params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()],
        )?;
        let mut ids = Vec::with_capacity(mailboxes.len());
        for (source_mailbox, destination_mailbox, config) in mailboxes {
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,destination_identity,state,config) VALUES(?1,?2,?3,?4,?5,'queued',?6)",
                params![
                    id,
                    project.id,
                    source_mailbox,
                    destination_mailbox,
                    normalized_destination_identity(destination_mailbox, Some(config)),
                    config
                ],
            )?;
            ids.push(id);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_created',?2)",
            params![
                project.id,
                format!("Batch created with {} mailbox jobs", ids.len())
            ],
        )?;
        tx.commit()?;
        Ok((project, ids))
    }
    pub fn transition(&self, id: &str, phase: Phase) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let current_name: String =
            tx.query_row("SELECT phase FROM projects WHERE id=?1", [id], |row| {
                row.get(0)
            })?;
        let current = Phase::parse(&current_name)?;
        let backwards_transition = current != Phase::Attention
            && phase != Phase::Attention
            && phase_rank(phase) < phase_rank(current);
        if current != phase
            && (current == Phase::Complete
                || (current == Phase::Attention
                    && !matches!(phase, Phase::Preflight | Phase::Verification))
                || backwards_transition)
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if phase == Phase::Verification && current == Phase::Discovery {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if phase == Phase::Complete && current != phase {
            let total: i64 = tx.query_row(
                "SELECT COUNT(*) FROM mailbox_jobs WHERE project_id=?1",
                [id],
                |row| row.get(0),
            )?;
            let verified: i64 = tx.query_row(
                "SELECT COUNT(*) FROM mailbox_jobs WHERE project_id=?1 AND state IN ('verified','verified_with_exceptions')",
                [id],
                |row| row.get(0),
            )?;
            if total == 0 || total != verified || current != Phase::Verification {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        let changed = tx.execute(
            "UPDATE projects SET phase=?1 WHERE id=?2 AND phase=?3",
            params![phase.as_str(), id, current.as_str()],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'phase_changed',?2)",
            params![id, phase.as_str()],
        )?;
        tx.commit()
    }

    /// Reopen a completed project only through an explicit, reason-bearing
    /// operation. This preserves the meaning of Complete while allowing an
    /// operator to document a legitimate post-cutover correction.
    pub fn reopen_project(&self, id: &str, reason: &str) -> rusqlite::Result<()> {
        let reason = reason.trim();
        if reason.is_empty() || reason.len() > 4096 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let current: String =
            self.connection
                .query_row("SELECT phase FROM projects WHERE id=?1", [id], |row| {
                    row.get(0)
                })?;
        if current != Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE projects SET phase='attention' WHERE id=?1 AND phase='complete'",
            [id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_reopened',?2)",
            params![id, reason],
        )?;
        tx.commit()
    }
    #[cfg(test)]
    pub fn add_mailbox(
        &self,
        project_id: &str,
        source: &str,
        destination: &str,
    ) -> rusqlite::Result<String> {
        let phase: String = self.connection.query_row(
            "SELECT phase FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if phase == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let identity = normalized_destination_identity(destination, None);
        let duplicate: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE project_id=?1 AND (destination_identity=?2 OR (destination_identity='' AND lower(trim(destination_mailbox))=lower(trim(?3)))) )",
            params![project_id, identity, destination],
            |row| row.get(0),
        )?;
        if duplicate {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let id = Uuid::new_v4().to_string();
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,destination_identity,state) VALUES(?1,?2,?3,?4,?5,'queued')",
            params![id, project_id, source, destination, identity],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'mailbox_added',?2)",
            params![project_id, format!("Mailbox {destination} added")],
        )?;
        tx.commit()?;
        Ok(id)
    }
    #[cfg(test)]
    pub fn set_mailbox_state(&self, job_id: &str, state: &str) -> rusqlite::Result<()> {
        if !matches!(
            state,
            "queued"
                | "preflight"
                | "ready"
                | "running"
                | "delta_required"
                | "verification_difference"
                | "completed"
                | "failed"
                | "cancelled"
                | "attention"
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // A running mailbox must always have a durable execution owner. Only
        // begin_run() and the batch child-claim path establish that pairing;
        // a generic state update must not create an unowned running row.
        if state == "running" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let (current, phase): (String, String) = self.connection.query_row(
            "SELECT j.state,p.phase FROM mailbox_jobs j JOIN projects p ON p.id=j.project_id WHERE j.id=?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if phase == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // Verification is owned by the evidence terminal methods below. A
        // generic state setter must not reuse evidence from an older run to
        // manufacture a verified result for a newer one.
        if state == "verified" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if current != state && !valid_mailbox_transition(&current, state) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = self.connection.execute(
            "UPDATE mailbox_jobs SET state=?, attention_reason=CASE WHEN ?='attention' THEN COALESCE(attention_reason,'unknown') ELSE NULL END, attempt=CASE WHEN ?='running' AND state<>'running' THEN attempt+1 ELSE attempt END WHERE id=?",
            params![state, state, state, job_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }
    pub fn mailbox_state(&self, job_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT state FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| row.get(0),
            )
            .optional()
    }

    /// Return the durable reason and operator action for an attention row.
    /// Unknown legacy values are deliberately surfaced as `Unknown` rather
    /// than guessed from UI text.
    pub fn mailbox_attention_reason(
        &self,
        job_id: &str,
    ) -> rusqlite::Result<Option<AttentionReason>> {
        self.connection
            .query_row(
                "SELECT attention_reason FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|value| {
                value.flatten().map(|reason| {
                    AttentionReason::parse(&reason).unwrap_or(AttentionReason::Unknown)
                })
            })
    }

    /// Load all durable attention reasons for a project in one read. Missing
    /// reasons are intentionally absent from the map; callers can distinguish
    /// a normal row from an explicitly classified operator-review row without
    /// issuing one query per mailbox.
    pub fn mailbox_attention_reasons(
        &self,
        project_id: &str,
    ) -> rusqlite::Result<HashMap<String, AttentionReason>> {
        let mut statement = self.connection.prepare(
            "SELECT id,attention_reason FROM mailbox_jobs WHERE project_id=?1 AND attention_reason IS NOT NULL",
        )?;
        let mut reasons = HashMap::new();
        for row in statement.query_map([project_id], |row| {
            let reason: String = row.get(1)?;
            Ok((
                row.get::<_, String>(0)?,
                AttentionReason::parse(&reason).unwrap_or(AttentionReason::Unknown),
            ))
        })? {
            let (job_id, reason) = row?;
            reasons.insert(job_id, reason);
        }
        Ok(reasons)
    }
    /// Return the last committed Dovecot stateful-sync checkpoint for a
    /// mailbox. The value is intentionally read separately from credentials;
    /// it contains engine state, not authentication material.
    pub fn mailbox_checkpoint(&self, job_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT checkpoint FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|value| value.flatten())
    }

    /// Read all durable facts needed for batch admission in one consistent
    /// query. The returned vector always follows `job_ids` order and rejects
    /// an ID that is missing from the selected project.
    pub fn batch_admission_states(
        &self,
        project_id: &str,
        job_ids: &[String],
    ) -> rusqlite::Result<Vec<BatchAdmissionState>> {
        if job_ids.is_empty() {
            return Ok(Vec::new());
        }
        // Keep the number of host parameters comfortably below SQLite's
        // commonly configured limit. This matters for large MSP batches,
        // while retaining the single-query-per-chunk behavior that avoids an
        // N+1 admission scan.
        const MAX_IDS_PER_QUERY: usize = 500;
        let mut by_id = HashMap::with_capacity(job_ids.len());
        for chunk in job_ids.chunks(MAX_IDS_PER_QUERY) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT id,state,preflight_plan,checkpoint FROM mailbox_jobs WHERE project_id=?1 AND id IN ({placeholders})"
            );
            let mut values = Vec::with_capacity(chunk.len() + 1);
            values.push(project_id.to_owned());
            values.extend(chunk.iter().cloned());
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    Ok(BatchAdmissionState {
                        job_id: row.get(0)?,
                        state: row.get(1)?,
                        preflight_plan: row.get(2)?,
                        checkpoint: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for row in rows {
                by_id.insert(row.job_id.clone(), row);
            }
        }
        job_ids
            .iter()
            .map(|job_id| {
                by_id
                    .remove(job_id)
                    .ok_or(rusqlite::Error::QueryReturnedNoRows)
            })
            .collect()
    }
    /// Persist the opaque digest of a preflighted plan. Callers should pass a
    /// canonical plan hash rather than generated command arguments; the
    /// stored value is used only for exact equality during live admission.
    pub fn set_preflight_plan(&self, job_id: &str, plan: &str) -> rusqlite::Result<()> {
        if plan.len() != 64 || !plan.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let phase: String = tx.query_row(
            "SELECT p.phase FROM mailbox_jobs j JOIN projects p ON p.id=j.project_id WHERE j.id=?1",
            [job_id],
            |row| row.get(0),
        )?;
        if phase == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = tx.execute(
            // A Dovecot state token is only meaningful for the plan that
            // produced it. Keep it for a repeated preflight of the same
            // plan, but invalidate it when a new plan is preflighted so a
            // later live run cannot resume across endpoint or policy drift.
            "UPDATE mailbox_jobs SET checkpoint=CASE WHEN preflight_plan=?1 THEN checkpoint ELSE NULL END, preflight_plan=?1 WHERE id=?2",
            params![plan, job_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.commit()
    }
    pub fn preflight_plan(&self, job_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT preflight_plan FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|value| value.flatten())
    }
    /// A desktop restart cannot prove that a previous child process still
    /// exists. All runs belonging to an interrupted wave are therefore made
    /// terminal: claimed children become abandoned and unclaimed children
    /// are also abandoned so their queued rows cannot wedge future retries.
    /// Only mailboxes that were actually claimed are moved to `attention`.
    #[cfg(test)]
    pub fn recover_abandoned_jobs(&self) -> rusqlite::Result<usize> {
        self.recover_abandoned_jobs_preserving(&[])
    }

    /// Recover interrupted work while retaining process identities that the
    /// caller could not verify. Those records are intentionally kept until an
    /// operator confirms that no migration engine remains, so a subsequent
    /// restart cannot silently forget the ownership uncertainty.
    pub fn recover_abandoned_jobs_preserving(
        &self,
        preserved_processes: &[ActiveProcess],
    ) -> rusqlite::Result<usize> {
        let projects = self
            .connection
            .prepare(
                "SELECT DISTINCT project_id FROM runs WHERE status='running' OR (status='queued' AND parent_run_id IN (SELECT id FROM runs WHERE status='running' AND job_id IS NULL))",
            )?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let tx = self.connection.unchecked_transaction()?;
        let count = tx.execute(
            "UPDATE mailbox_jobs SET state='attention',attention_reason='process_identity_unverified' WHERE state='running'",
            [],
        )?;
        tx.execute(
            "UPDATE runs SET status='abandoned',finished_at=CURRENT_TIMESTAMP,detail='Application restarted before completion' WHERE status='running'",
            [],
        )?;
        tx.execute(
            "UPDATE runs SET status='abandoned',finished_at=CURRENT_TIMESTAMP,detail='Application restarted before batch child was claimed' WHERE status='queued' AND parent_run_id IN (SELECT id FROM runs WHERE status='abandoned' AND job_id IS NULL AND detail='Application restarted before completion')",
            [],
        )?;
        tx.execute("DELETE FROM active_processes", [])?;
        for process in preserved_processes {
            tx.execute(
                "INSERT INTO active_processes(run_id,job_id,pid,start_ticks,process_group,session_id,executable) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    process.run_id,
                    process.job_id,
                    process.pid,
                    process.start_ticks,
                    process.process_group,
                    process.session_id,
                    process.executable,
                ],
            )?;
        }
        for project in projects {
            tx.execute(
                "INSERT INTO events(project_id,kind,detail) VALUES(?1,'run_recovered','Interrupted batch runs and claimed mailbox jobs were recovered; unclaimed child runs were abandoned')",
                [project],
            )?;
        }
        tx.commit()?;
        Ok(count)
    }

    /// Clear process identities only after an explicit operator review has
    /// established that no untracked migration child remains on the host.
    pub fn clear_active_processes_after_review(&self) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM active_processes", [])?;
        Ok(())
    }

    /// Record the OS process belonging to a durable run. Startup reconciliation
    /// uses this identity to terminate a recorded orphan before allowing an
    /// operator to retry the mailbox.
    pub fn register_process(&self, process: &ActiveProcess) -> rusqlite::Result<()> {
        // Validate ownership and insert the identity in one transaction. A
        // worker can arrive late while the UI is finishing a run; a
        // separate SELECT followed by INSERT would let that stale worker
        // recreate an active-process row after terminal cleanup.
        let tx = self.connection.unchecked_transaction()?;
        let consistent: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs r JOIN mailbox_jobs j ON j.id=?2 AND r.project_id=j.project_id WHERE r.id=?1 AND r.status='running' AND r.job_id=j.id)",
            params![process.run_id, process.job_id],
            |row| row.get(0),
        )?;
        if !consistent {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "INSERT INTO active_processes(run_id,job_id,pid,start_ticks,process_group,session_id,executable) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(run_id,job_id) DO UPDATE SET pid=excluded.pid,start_ticks=excluded.start_ticks,process_group=excluded.process_group,session_id=excluded.session_id,executable=excluded.executable,started_at=CURRENT_TIMESTAMP",
            params![
                process.run_id,
                process.job_id,
                i64::from(process.pid),
                process.start_ticks.map(|value| value as i64),
                process.process_group.map(i64::from),
                process.session_id.map(i64::from),
                process.executable
            ],
        )?;
        tx.commit()
    }

    pub fn active_processes(&self) -> rusqlite::Result<Vec<ActiveProcess>> {
        self.connection
            .prepare("SELECT run_id,job_id,pid,start_ticks,process_group,session_id,executable FROM active_processes")?
            .query_map([], |row| {
                let pid: i64 = row.get(2)?;
                let pid = u32::try_from(pid)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, pid))?;
                let start_ticks: Option<i64> = row.get(3)?;
                let process_group: Option<i64> = row.get(4)?;
                let session_id: Option<i64> = row.get(5)?;
                Ok(ActiveProcess {
                    run_id: row.get(0)?,
                    job_id: row.get(1)?,
                    pid,
                    start_ticks: start_ticks.and_then(|value| u64::try_from(value).ok()),
                    process_group: process_group.and_then(|value| u32::try_from(value).ok()),
                    session_id: session_id.and_then(|value| u32::try_from(value).ok()),
                    executable: row.get(6)?,
                })
            })?
            .collect()
    }

    pub fn clear_processes(&self, run_id: &str) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM active_processes WHERE run_id=?1", [run_id])?;
        Ok(())
    }
    pub fn record_event(&self, project_id: &str, kind: &str, detail: &str) -> rusqlite::Result<()> {
        if kind == "run_output" {
            return Ok(());
        }
        self.event(project_id, kind, detail)
    }
    #[cfg(test)]
    pub fn record_events_batch(&self, events: &[(&str, &str, &str)]) -> rusqlite::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let tx = self.connection.unchecked_transaction()?;
        {
            let mut statement =
                tx.prepare_cached("INSERT INTO events(project_id,kind,detail) VALUES(?1,?2,?3)")?;
            for (project_id, kind, detail) in events {
                if *kind == "run_output" {
                    continue;
                }
                let detail = bounded_event_detail(kind, detail);
                statement.execute(params![project_id, kind, detail])?;
            }
        }
        tx.commit()
    }

    /// Record diagnostic or verification events against the run that emitted
    /// them. The run supplies the project identity inside the same
    /// transaction, so callers cannot accidentally cross-wire a project and
    /// run while persisting asynchronous output.
    #[cfg(test)]
    pub fn record_run_events_batch(
        &self,
        run_id: &str,
        events: &[(&str, &str)],
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let (project_id, status): (String, String) = tx.query_row(
            "SELECT project_id,status FROM runs WHERE id=?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if status != "running" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        {
            let mut statement = tx.prepare_cached(
                "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,?3,?4)",
            )?;
            for (kind, detail) in events {
                if *kind == "run_output" {
                    continue;
                }
                let detail = bounded_event_detail(kind, detail);
                statement.execute(params![project_id, run_id, kind, detail])?;
            }
        }
        tx.commit()
    }

    /// Record diagnostic events for several active or queued child runs in
    /// one transaction. A retry release can move a child back to `queued`
    /// before this buffered batch is flushed; terminal runs remain rejected.
    /// Each tuple is `(run_id, kind, detail)`; project ownership is resolved
    /// from the durable run rather than trusted from an asynchronous caller.
    pub fn record_events_for_runs_batch(
        &self,
        events: &[(&str, &str, &str)],
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let mut statement = tx.prepare_cached(
            "INSERT INTO events(project_id,run_id,kind,detail) SELECT project_id,?1,?2,?3 FROM runs WHERE id=?1 AND (status='running' OR (status='queued' AND parent_run_id IN (SELECT id FROM runs WHERE status='running'))) ",
        )?;
        for (run_id, kind, detail) in events {
            if *kind == "run_output" {
                let valid: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND (status='running' OR (status='queued' AND parent_run_id IN (SELECT id FROM runs WHERE status='running'))))",
                    [run_id],
                    |row| row.get(0),
                )?;
                if !valid {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                continue;
            }
            let changed =
                statement.execute(params![run_id, kind, bounded_event_detail(kind, detail)])?;
            if changed != 1 {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        drop(statement);
        tx.commit()
    }
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
    /// Atomically records a run and moves its mailbox into `running`.
    /// Keeping these writes together prevents restart recovery from seeing a
    /// running mailbox without the run record needed to explain it.
    #[cfg(test)]
    pub fn begin_run(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        engine: &str,
    ) -> rusqlite::Result<()> {
        self.begin_run_with_snapshot(project_id, job_id, run_id, engine, "")
    }
    /// Atomically starts a mailbox run and persists its immutable, secret-free
    /// execution-plan snapshot (with session passwords excluded).
    pub fn begin_run_with_snapshot(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        engine: &str,
        plan_snapshot: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let (current, phase_at_start): (String, String) = tx.query_row(
            "SELECT j.state,p.phase FROM mailbox_jobs j JOIN projects p ON p.id=j.project_id WHERE j.id=?1 AND j.project_id=?2",
            params![job_id, project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if phase_at_start == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // A mailbox may never have two active runs. Recovery must first move
        // the previous run to an operator-review state before retrying it.
        if current == "running" || !valid_mailbox_transition(&current, "running") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let active_run_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=?1 AND job_id=?2 AND status IN ('queued','running'))",
            params![project_id, job_id],
            |row| row.get(0),
        )?;
        if active_run_exists {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "INSERT INTO runs(id,project_id,job_id,engine,phase_at_start,plan_snapshot,status) VALUES(?1,?2,?3,?4,?5,?6,'running')",
            params![run_id, project_id, job_id, engine, phase_at_start, plan_snapshot],
        )?;
        tx.execute(
            "UPDATE mailbox_jobs SET state='running',attention_reason=NULL,attempt=CASE WHEN state<>'running' THEN attempt+1 ELSE attempt END WHERE id=?1",
            [job_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_started',?3)",
            params![project_id, run_id, format!("{engine} ({run_id})")],
        )?;
        tx.commit()
    }
    /// Start a batch as one durable boundary. Child mailboxes remain queued
    /// until an individual worker claims them, so durable state reflects work
    /// that has actually reached the execution layer.
    #[cfg(test)]
    pub fn begin_batch_run(
        &self,
        project_id: &str,
        job_ids: &[String],
        run_id: &str,
        engine: &str,
        expected_plans: &[String],
    ) -> rusqlite::Result<()> {
        self.begin_batch_run_with_snapshot(project_id, job_ids, run_id, engine, expected_plans, "")
    }
    /// Atomically starts a batch and stores its immutable, secret-free plan
    /// snapshot alongside the parent run (with session passwords excluded).
    #[cfg(test)]
    pub fn begin_batch_run_with_snapshot(
        &self,
        project_id: &str,
        job_ids: &[String],
        run_id: &str,
        engine: &str,
        expected_plans: &[String],
        plan_snapshot: &str,
    ) -> rusqlite::Result<()> {
        self.begin_batch_run_with_children(
            project_id,
            job_ids,
            run_id,
            engine,
            expected_plans,
            plan_snapshot,
            &[],
        )
        .map(|_| ())
    }

    /// Atomically starts a batch parent and one durable child run per
    /// mailbox. Child runs and mailbox rows remain queued until workers claim
    /// them, so durable state reflects actual worker ownership.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_batch_run_with_children(
        &self,
        project_id: &str,
        job_ids: &[String],
        run_id: &str,
        engine: &str,
        expected_plans: &[String],
        plan_snapshot: &str,
        child_plans: &[BatchChildPlan],
    ) -> rusqlite::Result<Vec<String>> {
        let mut unique_job_ids = BTreeSet::new();
        let duplicate_job_id = job_ids.iter().any(|job_id| !unique_job_ids.insert(job_id));
        if job_ids.is_empty()
            || duplicate_job_id
            || (!expected_plans.is_empty() && expected_plans.len() != job_ids.len())
            || (!child_plans.is_empty() && child_plans.len() != job_ids.len())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let phase_at_start: String = tx.query_row(
            "SELECT phase FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if phase_at_start == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut destinations = BTreeSet::new();
        for (index, job_id) in job_ids.iter().enumerate() {
            let (
                current,
                destination,
                destination_identity,
                actual_plan,
                config,
                active_run_exists,
            ): (
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                bool,
            ) = tx.query_row(
                "SELECT j.state,j.destination_mailbox,j.destination_identity,j.preflight_plan,j.config,
                        EXISTS(SELECT 1 FROM runs r WHERE r.project_id=j.project_id AND r.job_id=j.id AND r.status IN ('queued','running'))
                 FROM mailbox_jobs j WHERE j.id=?1 AND j.project_id=?2",
                params![job_id, project_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )?;
            let identity = if destination_identity.is_empty() {
                normalized_destination_identity(&destination, config.as_deref())
            } else {
                destination_identity
            };
            if !destinations.insert(identity) {
                return Err(rusqlite::Error::InvalidQuery);
            }
            // Reject an already-running child rather than treating it as a
            // harmless retry. This keeps one durable execution owner per
            // mailbox even when two callers race.
            if current == "running" || !valid_mailbox_transition(&current, "running") {
                return Err(rusqlite::Error::InvalidQuery);
            }
            if active_run_exists {
                return Err(rusqlite::Error::InvalidQuery);
            }
            if let Some(expected_plan) = expected_plans.get(index)
                && actual_plan.as_deref() != Some(expected_plan.as_str())
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        tx.execute(
            "INSERT INTO runs(id,project_id,job_id,engine,phase_at_start,plan_snapshot,status) VALUES(?1,?2,NULL,?3,?4,?5,'running')",
            params![run_id, project_id, engine, phase_at_start, plan_snapshot],
        )?;
        let mut child_run_ids = Vec::with_capacity(job_ids.len());
        for (index, job_id) in job_ids.iter().enumerate() {
            let child_run_id = Uuid::new_v4().to_string();
            let child_plan = child_plans.get(index);
            let child_engine = child_plan
                .map(|plan| plan.engine.as_str())
                .unwrap_or(engine);
            let child_snapshot = child_plan
                .map(|plan| plan.plan_snapshot.as_str())
                .unwrap_or("");
            tx.execute(
                "INSERT INTO runs(id,project_id,job_id,parent_run_id,engine,phase_at_start,plan_snapshot,status) VALUES(?1,?2,?3,?4,?5,?6,?7,'queued')",
                params![
                    child_run_id,
                    project_id,
                    job_id,
                    run_id,
                    child_engine,
                    phase_at_start,
                    child_snapshot
                ],
            )?;
            if let Some(version) = child_plan.and_then(|plan| plan.engine_version.as_deref()) {
                tx.execute(
                    "INSERT INTO engine_versions(run_id,version) VALUES(?1,?2)",
                    params![child_run_id, version],
                )?;
            }
            child_run_ids.push(child_run_id);
        }
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_started',?3)",
            params![
                project_id,
                run_id,
                format!("{engine} ({run_id}); {} child jobs", job_ids.len())
            ],
        )?;
        tx.commit().map(|()| child_run_ids)
    }
    /// Legacy test fixture for the pre-child batch model. Production callers
    /// must use `claim_batch_mailbox_for_child`, which binds the mailbox,
    /// parent wave, and child run in one transaction. Keeping this helper
    /// test-only prevents an external caller from accidentally bypassing the
    /// mailbox-specific child ownership invariant.
    #[cfg(test)]
    pub fn claim_batch_mailbox(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let parent_is_running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND project_id=?2 AND job_id IS NULL AND status='running')",
            params![run_id, project_id],
            |row| row.get(0),
        )?;
        if !parent_is_running {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let (current, phase): (String, String) = tx.query_row(
            "SELECT j.state,p.phase FROM mailbox_jobs j JOIN projects p ON p.id=j.project_id WHERE j.id=?1 AND j.project_id=?2",
            params![job_id, project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if phase == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if current == "running" {
            return tx.commit();
        }
        if !valid_mailbox_transition(&current, "running") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "UPDATE mailbox_jobs SET state='running',attention_reason=NULL,attempt=attempt+1 WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'mailbox_claimed',?3)",
            params![
                project_id,
                run_id,
                format!("{job_id} claimed by batch run {run_id}")
            ],
        )?;
        tx.commit()
    }
    /// Claim a mailbox for its durable child run. The parent run proves that
    /// this belongs to the active batch, while the child relation prevents a
    /// worker from attaching a process to another row in the same project.
    pub fn claim_batch_mailbox_for_child(
        &self,
        project_id: &str,
        job_id: &str,
        parent_run_id: &str,
        child_run_id: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let parent_is_running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND project_id=?2 AND job_id IS NULL AND status='running')",
            params![parent_run_id, project_id],
            |row| row.get(0),
        )?;
        let child_status: String = tx.query_row(
            "SELECT status FROM runs WHERE id=?1 AND project_id=?2 AND job_id=?3 AND parent_run_id=?4",
            params![child_run_id, project_id, job_id, parent_run_id],
            |row| row.get(0),
        )?;
        if !parent_is_running || !matches!(child_status.as_str(), "queued" | "running") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let (current, phase): (String, String) = tx.query_row(
            "SELECT j.state,p.phase FROM mailbox_jobs j JOIN projects p ON p.id=j.project_id WHERE j.id=?1 AND j.project_id=?2",
            params![job_id, project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if phase == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if current == "running" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if !valid_mailbox_transition(&current, "running") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "UPDATE mailbox_jobs SET state='running',attention_reason=NULL,attempt=attempt+1 WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
        )?;
        tx.execute(
            "UPDATE runs SET status='running' WHERE id=?1 AND project_id=?2 AND job_id=?3 AND parent_run_id=?4 AND status='queued'",
            params![child_run_id, project_id, job_id, parent_run_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'mailbox_claimed',?3)",
            params![
                project_id,
                child_run_id,
                format!("{job_id} claimed by child run {child_run_id}")
            ],
        )?;
        tx.commit()
    }

    /// Test-only legacy helper. Production retries keep the original child
    /// claim and run ownership across internal attempts; releasing ownership
    /// between attempts reintroduces UI timing races.
    #[cfg(test)]
    pub fn release_batch_mailbox_for_retry(
        &self,
        project_id: &str,
        job_id: &str,
        parent_run_id: &str,
        child_run_id: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let parent_is_running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND project_id=?2 AND job_id IS NULL AND status='running')",
            params![parent_run_id, project_id],
            |row| row.get(0),
        )?;
        let child_is_running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND project_id=?2 AND job_id=?3 AND parent_run_id=?4 AND status='running')",
            params![child_run_id, project_id, job_id, parent_run_id],
            |row| row.get(0),
        )?;
        let mailbox_is_running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE id=?1 AND project_id=?2 AND state='running')",
            params![job_id, project_id],
            |row| row.get(0),
        )?;
        if !parent_is_running || !child_is_running || !mailbox_is_running {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "UPDATE runs SET status='queued',detail='released for transient retry' WHERE id=?1 AND project_id=?2 AND job_id=?3 AND parent_run_id=?4 AND status='running'",
            params![child_run_id, project_id, job_id, parent_run_id],
        )?;
        tx.execute(
            "UPDATE mailbox_jobs SET state='ready',attention_reason=NULL WHERE id=?1 AND project_id=?2 AND state='running'",
            params![job_id, project_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'mailbox_retry_released',?3)",
            params![project_id, child_run_id, format!("{job_id} released for transient retry")],
        )?;
        tx.commit()
    }
    pub fn finish_run(&self, run_id: &str, status: &str, detail: &str) -> rusqlite::Result<()> {
        if !matches!(
            status,
            "completed" | "failed" | "cancelled" | "abandoned" | "verification_failed"
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM runs WHERE id=?1 AND status='running'",
            [run_id],
            |row| row.get(0),
        )?;
        // A parent batch is not complete while any child is still queued or
        // running.  Keep this invariant in the store so a controller bug or
        // partial event stream cannot produce a deceptively terminal batch.
        let has_unfinished_children: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE parent_run_id=?1 AND status IN ('queued','running'))",
            [run_id],
            |row| row.get(0),
        )?;
        if has_unfinished_children {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let has_unsuccessful_children: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE parent_run_id=?1 AND status<>'completed')",
            [run_id],
            |row| row.get(0),
        )?;
        if status == "completed" && has_unsuccessful_children {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = tx.execute(
            "UPDATE runs SET status=?1,finished_at=CURRENT_TIMESTAMP,detail=?2 WHERE id=?3 AND status='running'",
            params![status, detail, run_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute("DELETE FROM active_processes WHERE run_id=?1", [run_id])?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_finished',?3)",
            params![project_id, run_id, detail],
        )?;
        tx.commit()
    }
    /// Atomically completes a single-mailbox run and records the durable
    /// mailbox state.  Completion is deliberately one transaction: a run
    /// must never be marked finished while its mailbox remains `running` (or
    /// vice versa) after a database failure or process interruption.
    #[cfg(test)]
    pub fn finish_run_for_mailbox(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
    ) -> rusqlite::Result<()> {
        self.finish_run_for_mailbox_with_checkpoint(
            project_id,
            job_id,
            run_id,
            run_status,
            mailbox_state,
            detail,
            None,
        )
    }

    /// Atomically completes a mailbox run and optionally advances its
    /// Dovecot stateful-sync checkpoint. A checkpoint is written only as part
    /// of the same terminal transaction, so a process result and its resume
    /// state can never diverge in the ledger.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_run_for_mailbox_with_checkpoint(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
        checkpoint: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.finish_run_for_mailbox_with_preflight_plan_and_checkpoint(
            project_id,
            job_id,
            run_id,
            run_status,
            mailbox_state,
            detail,
            None,
            checkpoint,
        )
    }

    /// Atomically completes a mailbox run, optionally records its successful
    /// dry-preflight digest, and optionally advances its Dovecot checkpoint.
    /// The two digests are deliberately committed with the terminal state so
    /// a live batch cannot observe a ready child without its preflight proof.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_run_for_mailbox_with_preflight_plan_and_checkpoint(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
        preflight_plan: Option<&str>,
        checkpoint: Option<&str>,
    ) -> rusqlite::Result<()> {
        if preflight_plan.is_some_and(|value| {
            value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if checkpoint.is_some_and(|value| !valid_dovecot_checkpoint(value)) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if checkpoint.is_some() && run_status != "completed" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if !matches!(
            run_status,
            "completed" | "failed" | "cancelled" | "verification_failed"
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if !matches!(
            mailbox_state,
            "ready"
                | "completed"
                | "verified"
                | "delta_required"
                | "verification_difference"
                | "failed"
                | "cancelled"
                | "attention"
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let attention_reason =
            attention_reason_for(mailbox_state, detail).map(AttentionReason::as_str);
        let tx = self.connection.unchecked_transaction()?;
        let current: String = tx.query_row(
            "SELECT state FROM mailbox_jobs WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
            |row| row.get(0),
        )?;
        let terminal_pair_is_valid = match run_status {
            "completed" => matches!(
                mailbox_state,
                "ready" | "completed" | "delta_required" | "verification_difference" | "attention"
            ),
            "failed" => matches!(mailbox_state, "failed" | "attention"),
            "cancelled" => matches!(mailbox_state, "cancelled" | "attention"),
            "verification_failed" => {
                matches!(mailbox_state, "attention" | "verification_difference")
            }
            _ => false,
        };
        if !terminal_pair_is_valid {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // Verification must be committed together with the evidence that
        // proves this run.  Merely finding an older evidence row is not
        // sufficient: otherwise a later run could reuse stale evidence and
        // manufacture `verified` through this generic terminal path.
        if mailbox_state == "verified" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if current != mailbox_state && !valid_mailbox_transition(&current, mailbox_state) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let run_changed = tx.execute(
            "UPDATE runs SET status=?1,finished_at=CURRENT_TIMESTAMP,detail=?2 WHERE id=?3 AND project_id=?4 AND job_id=?5 AND (status='running' OR (status='queued' AND ?1 IN ('cancelled','failed','verification_failed'))) ",
            params![run_status, detail, run_id, project_id, job_id],
        )?;
        if run_changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute(
            "UPDATE mailbox_jobs SET state=?1,attention_reason=?2,preflight_plan=COALESCE(?3,preflight_plan),checkpoint=COALESCE(?4,checkpoint) WHERE id=?5 AND project_id=?6",
            params![mailbox_state, attention_reason, preflight_plan, checkpoint, job_id, project_id],
        )?;
        tx.execute("DELETE FROM active_processes WHERE run_id=?1", [run_id])?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_finished',?3)",
            params![
                project_id,
                run_id,
                if run_status == "completed" {
                    "success"
                } else {
                    "failure"
                }
            ],
        )?;
        tx.commit()
    }
    /// Atomically records terminal verification evidence and completes the
    /// active run. A verified result is allowed to move directly from
    /// `running` here because the evidence and terminal transition share one
    /// transaction; ordinary mailbox-state updates still reject that jump.
    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub fn finish_run_for_mailbox_with_evidence(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
        value: &MailboxEvidence,
    ) -> rusqlite::Result<()> {
        self.finish_run_for_mailbox_with_evidence_and_checkpoint(
            project_id,
            job_id,
            run_id,
            run_status,
            mailbox_state,
            detail,
            value,
            None,
        )
    }

    /// Atomically records verification evidence, completes the run, updates
    /// the mailbox state, and optionally stores the new Dovecot checkpoint.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_run_for_mailbox_with_evidence_and_checkpoint(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
        value: &MailboxEvidence,
        checkpoint: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.finish_run_for_mailbox_with_evidence_and_preflight_plan_and_checkpoint(
            project_id,
            job_id,
            run_id,
            run_status,
            mailbox_state,
            detail,
            value,
            None,
            checkpoint,
        )
    }

    /// Atomically records evidence, completes a mailbox run, and optionally
    /// persists both the dry-preflight digest and Dovecot checkpoint.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_run_for_mailbox_with_evidence_and_preflight_plan_and_checkpoint(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
        value: &MailboxEvidence,
        preflight_plan: Option<&str>,
        checkpoint: Option<&str>,
    ) -> rusqlite::Result<()> {
        if preflight_plan.is_some_and(|value| {
            value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if checkpoint.is_some_and(|value| !valid_dovecot_checkpoint(value)) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if run_status != "completed"
            || !matches!(
                mailbox_state,
                "verified" | "delta_required" | "verification_difference"
            )
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // `verified` is a claim about the evidence, not merely a requested
        // mailbox state. Keep this invariant in the store so a future caller
        // cannot accidentally promote mismatching aggregate evidence by
        // bypassing the current UI decision logic.
        if mailbox_state == "verified" && !value.is_exact_match() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let attention_reason =
            attention_reason_for(mailbox_state, detail).map(AttentionReason::as_str);
        let tx = self.connection.unchecked_transaction()?;
        let current: String = tx.query_row(
            "SELECT state FROM mailbox_jobs WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
            |row| row.get(0),
        )?;
        let evidence_terminal_jump = current == "running"
            && matches!(
                mailbox_state,
                "verified" | "delta_required" | "verification_difference"
            );
        if current != mailbox_state
            && !evidence_terminal_jump
            && !valid_mailbox_transition(&current, mailbox_state)
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let run_changed = tx.execute(
            "UPDATE runs SET status='completed',finished_at=CURRENT_TIMESTAMP,detail=?1 WHERE id=?2 AND project_id=?3 AND job_id=?4 AND status='running'",
            params![detail, run_id, project_id, job_id],
        )?;
        if run_changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute("INSERT INTO evidence_history(job_id,run_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", params![job_id, run_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages, value.source_folders, value.destination_folders, value.authoritative])?;
        tx.execute("INSERT INTO evidence(job_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(job_id) DO UPDATE SET source_messages=excluded.source_messages,destination_messages=excluded.destination_messages,source_bytes=excluded.source_bytes,destination_bytes=excluded.destination_bytes,unmatched_messages=excluded.unmatched_messages,failed_messages=excluded.failed_messages,source_folders=excluded.source_folders,destination_folders=excluded.destination_folders,authoritative=excluded.authoritative,captured_at=CURRENT_TIMESTAMP", params![job_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages, value.source_folders, value.destination_folders, value.authoritative])?;
        tx.execute(
            "UPDATE mailbox_jobs SET state=?1,attention_reason=?2,preflight_plan=COALESCE(?3,preflight_plan),checkpoint=COALESCE(?4,checkpoint) WHERE id=?5 AND project_id=?6",
            params![mailbox_state, attention_reason, preflight_plan, checkpoint, job_id, project_id],
        )?;
        tx.execute("DELETE FROM active_processes WHERE run_id=?1", [run_id])?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_finished',?3)",
            params![project_id, run_id, "success with verification evidence"],
        )?;
        tx.commit()
    }
    #[cfg(test)]
    pub fn run_status(&self, run_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row("SELECT status FROM runs WHERE id=?1", [run_id], |row| {
                row.get(0)
            })
            .optional()
    }
    #[cfg(test)]
    pub fn latest_run(&self, job_id: &str) -> rusqlite::Result<Option<RunSummary>> {
        self.connection
            .query_row(
                "SELECT id,job_id,parent_run_id,engine,phase_at_start,plan_snapshot,status,started_at,finished_at,detail FROM runs WHERE job_id=?1 ORDER BY started_at DESC, rowid DESC LIMIT 1",
                [job_id],
                |row| {
                    Ok(RunSummary {
                        id: row.get(0)?,
                        job_id: row.get(1)?,
                        parent_run_id: row.get(2)?,
                        engine: row.get(3)?,
                        phase_at_start: row.get(4)?,
                        plan_snapshot: row.get(5)?,
                        status: row.get(6)?,
                        started_at: row.get(7)?,
                        finished_at: row.get(8)?,
                        detail: row.get(9)?,
                    })
                },
            )
            .optional()
    }
    #[cfg(test)]
    pub fn run(&self, run_id: &str) -> rusqlite::Result<Option<RunSummary>> {
        self.connection
            .query_row(
                "SELECT id,job_id,parent_run_id,engine,phase_at_start,plan_snapshot,status,started_at,finished_at,detail FROM runs WHERE id=?1",
                [run_id],
                |row| {
                    Ok(RunSummary {
                        id: row.get(0)?,
                        job_id: row.get(1)?,
                        parent_run_id: row.get(2)?,
                        engine: row.get(3)?,
                        phase_at_start: row.get(4)?,
                        plan_snapshot: row.get(5)?,
                        status: row.get(6)?,
                        started_at: row.get(7)?,
                        finished_at: row.get(8)?,
                        detail: row.get(9)?,
                    })
                },
            )
            .optional()
    }
    #[cfg(test)]
    pub fn recent_runs(&self, project_id: &str, limit: u32) -> rusqlite::Result<Vec<RunSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT id,job_id,parent_run_id,engine,phase_at_start,plan_snapshot,status,started_at,finished_at,detail FROM runs WHERE project_id=?1 ORDER BY started_at DESC, rowid DESC LIMIT ?2",
        )?;
        statement
            .query_map(params![project_id, limit], |row| {
                Ok(RunSummary {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    parent_run_id: row.get(2)?,
                    engine: row.get(3)?,
                    phase_at_start: row.get(4)?,
                    plan_snapshot: row.get(5)?,
                    status: row.get(6)?,
                    started_at: row.get(7)?,
                    finished_at: row.get(8)?,
                    detail: row.get(9)?,
                })
            })?
            .collect()
    }

    pub fn project_report_snapshot(
        &self,
        project_id: &str,
    ) -> rusqlite::Result<Option<ProjectReportSnapshot>> {
        // Keep every report query on one SQLite read transaction. In WAL mode
        // this pins one consistent database snapshot, so a concurrent
        // completion cannot produce a report combining rows from different
        // commits (for example, a new mailbox state with old evidence).
        let tx = self.connection.unchecked_transaction()?;
        let Some(project) = tx
            .query_row(
                "SELECT id,name,source_endpoint,destination_endpoint,phase FROM projects WHERE id=?1",
                [project_id],
                |row| {
                    Ok(Project {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        source_endpoint: row.get(2)?,
                        destination_endpoint: row.get(3)?,
                        phase: Phase::parse(&row.get::<_, String>(4)?)?,
                    })
                },
            )
            .optional()? else {
            tx.commit()?;
            return Ok(None);
        };

        let mut jobs = Vec::new();
        let mut attention_reasons = HashMap::new();
        let mut statement = tx.prepare(
            "SELECT id,source_mailbox,destination_mailbox,state,config,attention_reason FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid",
        )?;
        for row in statement.query_map([project_id], |row| {
            let raw_reason: Option<String> = row.get(5)?;
            Ok((
                MailboxJob {
                    id: row.get(0)?,
                    source_mailbox: row.get(1)?,
                    destination_mailbox: row.get(2)?,
                    state: row.get(3)?,
                    config: row.get(4)?,
                },
                raw_reason,
            ))
        })? {
            let (job, raw_reason) = row?;
            attention_reasons.insert(
                job.id.clone(),
                raw_reason.map(|reason| {
                    AttentionReason::parse(&reason).unwrap_or(AttentionReason::Unknown)
                }),
            );
            jobs.push(job);
        }

        let mut acceptances = HashMap::new();
        let mut acceptance_statement = tx.prepare(
            "SELECT va.job_id,va.run_id,va.operator,va.reason,va.accepted_at FROM verification_acceptances va JOIN mailbox_jobs j ON j.id=va.job_id WHERE j.project_id=?1 ORDER BY va.id ASC",
        )?;
        for row in acceptance_statement.query_map([project_id], |row| {
            Ok(VerificationAcceptance {
                job_id: row.get(0)?,
                run_id: row.get(1)?,
                operator: row.get(2)?,
                reason: row.get(3)?,
                accepted_at: row.get(4)?,
            })
        })? {
            let acceptance = row?;
            acceptances.insert(acceptance.job_id.clone(), acceptance);
        }

        let mut evidence = HashMap::new();
        let mut evidence_statement = tx.prepare(
            "SELECT eh.job_id,eh.run_id,eh.source_messages,eh.destination_messages,eh.source_bytes,eh.destination_bytes,eh.unmatched_messages,eh.failed_messages,eh.source_folders,eh.destination_folders,eh.authoritative,r.plan_snapshot FROM evidence_history eh JOIN mailbox_jobs j ON j.id=eh.job_id LEFT JOIN runs r ON r.id=eh.run_id WHERE j.project_id=?1 ORDER BY eh.id ASC",
        )?;
        for row in evidence_statement.query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MailboxEvidence {
                    source_messages: row.get(2)?,
                    destination_messages: row.get(3)?,
                    source_bytes: row.get(4)?,
                    destination_bytes: row.get(5)?,
                    unmatched_messages: row.get(6)?,
                    failed_messages: row.get(7)?,
                    source_folders: row.get(8)?,
                    destination_folders: row.get(9)?,
                    authoritative: row.get::<_, i64>(10)? != 0,
                },
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(11)?,
            ))
        })? {
            let (job_id, value, run_id, plan_snapshot) = row?;
            evidence.insert(job_id, (run_id, value, plan_snapshot));
        }

        let mut runs = Vec::new();
        let mut run_statement = tx.prepare(
            "SELECT r.id,r.job_id,r.parent_run_id,r.engine,r.phase_at_start,r.plan_snapshot,r.status,r.started_at,r.finished_at,r.detail,ev.version FROM runs r LEFT JOIN engine_versions ev ON ev.run_id=r.id WHERE r.project_id=?1 ORDER BY r.started_at ASC,r.rowid ASC",
        )?;
        for row in run_statement.query_map([project_id], |row| {
            Ok(ReportRunSnapshot {
                run: RunSummary {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    parent_run_id: row.get(2)?,
                    engine: row.get(3)?,
                    phase_at_start: row.get(4)?,
                    plan_snapshot: row.get(5)?,
                    status: row.get(6)?,
                    started_at: row.get(7)?,
                    finished_at: row.get(8)?,
                    detail: row.get(9)?,
                },
                engine_version: row.get(10)?,
            })
        })? {
            runs.push(row?);
        }

        let mailboxes = jobs
            .into_iter()
            .map(|job| {
                let attention_reason = attention_reasons.remove(&job.id).flatten();
                let acceptance = acceptances.remove(&job.id);
                let evidence = evidence.remove(&job.id);
                ReportMailboxSnapshot {
                    job,
                    attention_reason,
                    acceptance,
                    evidence,
                }
            })
            .collect();
        let snapshot = ProjectReportSnapshot {
            project,
            mailboxes,
            runs,
        };
        drop(run_statement);
        drop(evidence_statement);
        drop(acceptance_statement);
        drop(statement);
        tx.commit()?;
        Ok(Some(snapshot))
    }

    /// Compact verification projection for interactive views. It omits saved
    /// mailbox configuration and run-plan snapshots; full report assembly
    /// remains an explicit export/detail operation.
    pub fn verification_rows(
        &self,
        project_id: &str,
        offset: u32,
        limit: u32,
    ) -> rusqlite::Result<Vec<ReportMailboxSnapshot>> {
        let tx = self.connection.unchecked_transaction()?;
        let mut statement = tx.prepare(
            "SELECT j.id,j.source_mailbox,j.destination_mailbox,j.state,j.attention_reason,va.run_id,va.operator,va.reason,va.accepted_at,eh.run_id,eh.source_messages,eh.destination_messages,eh.source_bytes,eh.destination_bytes,eh.unmatched_messages,eh.failed_messages,eh.source_folders,eh.destination_folders,eh.authoritative FROM mailbox_jobs j LEFT JOIN verification_acceptances va ON va.id=(SELECT MAX(latest.id) FROM verification_acceptances latest WHERE latest.job_id=j.id) LEFT JOIN evidence_history eh ON eh.id=(SELECT MAX(latest.id) FROM evidence_history latest WHERE latest.job_id=j.id) WHERE j.project_id=?1 ORDER BY j.rowid LIMIT ?2 OFFSET ?3",
        )?;
        let rows = statement
            .query_map(rusqlite::params![project_id, limit, offset], |row| {
                let acceptance_run_id: Option<String> = row.get(5)?;
                let acceptance = acceptance_run_id
                    .map(|run_id| -> rusqlite::Result<VerificationAcceptance> {
                        Ok(VerificationAcceptance {
                            job_id: row.get(0)?,
                            run_id,
                            operator: row.get(6)?,
                            reason: row.get(7)?,
                            accepted_at: row.get(8)?,
                        })
                    })
                    .transpose()?;
                let evidence = row
                    .get::<_, Option<String>>(9)?
                    .map(|run_id| {
                        Ok::<_, rusqlite::Error>((
                            run_id,
                            MailboxEvidence {
                                source_messages: row.get(10)?,
                                destination_messages: row.get(11)?,
                                source_bytes: row.get(12)?,
                                destination_bytes: row.get(13)?,
                                unmatched_messages: row.get(14)?,
                                failed_messages: row.get(15)?,
                                source_folders: row.get(16)?,
                                destination_folders: row.get(17)?,
                                authoritative: row.get::<_, i64>(18)? != 0,
                            },
                            None,
                        ))
                    })
                    .transpose()?;
                Ok(ReportMailboxSnapshot {
                    job: MailboxJob {
                        id: row.get(0)?,
                        source_mailbox: row.get(1)?,
                        destination_mailbox: row.get(2)?,
                        state: row.get(3)?,
                        config: None,
                    },
                    attention_reason: row.get::<_, Option<String>>(4)?.map(|reason| {
                        AttentionReason::parse(&reason).unwrap_or(AttentionReason::Unknown)
                    }),
                    acceptance,
                    evidence,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        tx.commit()?;
        Ok(rows)
    }

    pub fn recent_run_list(
        &self,
        project_id: &str,
        limit: u32,
    ) -> rusqlite::Result<Vec<RunListItem>> {
        let mut statement = self.connection.prepare(
            "SELECT r.id,r.job_id,r.parent_run_id,j.source_mailbox,j.destination_mailbox,r.engine,r.phase_at_start,r.status,r.started_at,r.finished_at,r.detail FROM runs r LEFT JOIN mailbox_jobs j ON j.id=r.job_id AND j.project_id=r.project_id WHERE r.project_id=?1 ORDER BY r.started_at DESC, r.rowid DESC LIMIT ?2",
        )?;
        statement
            .query_map(params![project_id, limit], |row| {
                Ok(RunListItem {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    parent_run_id: row.get(2)?,
                    source_mailbox: row.get(3)?,
                    destination_mailbox: row.get(4)?,
                    engine: row.get(5)?,
                    phase_at_start: row.get(6)?,
                    status: row.get(7)?,
                    started_at: row.get(8)?,
                    finished_at: row.get(9)?,
                    detail: row.get(10)?,
                })
            })?
            .collect()
    }
    // Legacy evidence insertion is retained only as a fixture helper for
    // historical-state tests. Production callers must use the run-owned
    // terminal methods above, which atomically bind evidence to the run and
    // mailbox state transition.
    #[cfg(test)]
    fn record_evidence(&self, job_id: &str, value: &MailboxEvidence) -> rusqlite::Result<()> {
        self.record_evidence_for_run(job_id, "legacy", value)
    }
    #[cfg(test)]
    fn record_evidence_for_run(
        &self,
        job_id: &str,
        run_id: &str,
        value: &MailboxEvidence,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        if run_id != "legacy" {
            let owns_mailbox: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM runs r JOIN mailbox_jobs j ON j.id=r.job_id AND j.project_id=r.project_id WHERE r.id=?1 AND j.id=?2)",
                params![run_id, job_id],
                |row| row.get(0),
            )?;
            if !owns_mailbox {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        tx.execute("INSERT INTO evidence_history(job_id,run_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", params![job_id, run_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages, value.source_folders, value.destination_folders, value.authoritative])?;
        tx.execute("INSERT INTO evidence(job_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(job_id) DO UPDATE SET source_messages=excluded.source_messages,destination_messages=excluded.destination_messages,source_bytes=excluded.source_bytes,destination_bytes=excluded.destination_bytes,unmatched_messages=excluded.unmatched_messages,failed_messages=excluded.failed_messages,source_folders=excluded.source_folders,destination_folders=excluded.destination_folders,authoritative=excluded.authoritative,captured_at=CURRENT_TIMESTAMP", params![job_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages, value.source_folders, value.destination_folders, value.authoritative])?;
        tx.commit()?;
        Ok(())
    }
    #[cfg(test)]
    pub fn evidence(&self, job_id: &str) -> rusqlite::Result<Option<MailboxEvidence>> {
        self.connection.query_row("SELECT source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative FROM evidence WHERE job_id=?1", [job_id], |r| Ok(MailboxEvidence { source_messages:r.get(0)?, destination_messages:r.get(1)?, source_bytes:r.get(2)?, destination_bytes:r.get(3)?, unmatched_messages:r.get(4)?, failed_messages:r.get(5)?, source_folders:r.get(6)?, destination_folders:r.get(7)?, authoritative:r.get::<_, i64>(8)? != 0 })).optional()
    }
    #[cfg(test)]
    pub fn latest_evidence_for_run(
        &self,
        job_id: &str,
    ) -> rusqlite::Result<Option<(String, MailboxEvidence)>> {
        self.connection
            .query_row(
                "SELECT run_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative FROM evidence_history WHERE job_id=?1 ORDER BY captured_at DESC, id DESC LIMIT 1",
                [job_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        MailboxEvidence {
                            source_messages: row.get(1)?,
                            destination_messages: row.get(2)?,
                            source_bytes: row.get(3)?,
                            destination_bytes: row.get(4)?,
                            unmatched_messages: row.get(5)?,
                            failed_messages: row.get(6)?,
                            source_folders: row.get(7)?,
                            destination_folders: row.get(8)?,
                            authoritative: row.get::<_, i64>(9)? != 0,
                        },
                    ))
                },
            )
            .optional()
    }

    /// Permanently accept a recorded verification difference with an
    /// operator-owned explanation. This is intentionally separate from the
    /// evidence terminal path: acceptance is a later human decision, not a
    /// claim that the source and destination were identical.
    pub fn accept_verification_difference(
        &self,
        project_id: &str,
        job_id: &str,
        operator: &str,
        reason: &str,
    ) -> rusqlite::Result<()> {
        let operator = operator.trim();
        let reason = reason.trim();
        if operator.is_empty()
            || operator.len() > 256
            || reason.is_empty()
            || reason.len() > 4096
            || operator.chars().any(|character| character.is_control())
            || reason.chars().any(|character| character.is_control())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let (state, phase): (String, String) = tx.query_row(
            "SELECT j.state,p.phase FROM mailbox_jobs j JOIN projects p ON p.id=j.project_id WHERE j.id=?1 AND j.project_id=?2",
            params![job_id, project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if state != "verification_difference" || phase == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let run_id: String = tx.query_row(
            "SELECT run_id FROM evidence_history WHERE job_id=?1 ORDER BY captured_at DESC, id DESC LIMIT 1",
            [job_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO verification_acceptances(job_id,run_id,operator,reason) VALUES(?1,?2,?3,?4)",
            params![job_id, run_id, operator, reason],
        )?;
        let changed = tx.execute(
            "UPDATE mailbox_jobs SET state='verified_with_exceptions',attention_reason=NULL WHERE id=?1 AND project_id=?2 AND state='verification_difference'",
            params![job_id, project_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'verification_exception_accepted',?2)",
            params![project_id, bounded_event_detail("verification_exception_accepted", &format!("{job_id}: accepted by {operator}: {reason}"))],
        )?;
        tx.commit()
    }

    #[cfg(test)]
    pub fn latest_verification_acceptance(
        &self,
        job_id: &str,
    ) -> rusqlite::Result<Option<VerificationAcceptance>> {
        self.connection
            .query_row(
                "SELECT job_id,run_id,operator,reason,accepted_at FROM verification_acceptances WHERE job_id=?1 ORDER BY id DESC LIMIT 1",
                [job_id],
                |row| {
                    Ok(VerificationAcceptance {
                        job_id: row.get(0)?,
                        run_id: row.get(1)?,
                        operator: row.get(2)?,
                        reason: row.get(3)?,
                        accepted_at: row.get(4)?,
                    })
                },
            )
            .optional()
    }

    /// Record the version string reported by the external engine. This is
    /// metadata only; an unavailable version must remain explicitly absent
    /// rather than being replaced with an invented value.
    pub fn record_engine_version(&self, run_id: &str, version: &str) -> rusqlite::Result<()> {
        let version = version.trim();
        if version.is_empty()
            || version.len() > 512
            || version.chars().any(|character| character.is_control())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = self.connection.execute(
            "INSERT INTO engine_versions(run_id,version) SELECT id,?2 FROM runs WHERE id=?1 ON CONFLICT(run_id) DO UPDATE SET version=excluded.version,captured_at=CURRENT_TIMESTAMP",
            params![run_id, version],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }

    pub fn engine_version(&self, run_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT version FROM engine_versions WHERE run_id=?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()
    }
    pub fn project(&self, id: &str) -> rusqlite::Result<Option<Project>> {
        self.connection.query_row("SELECT id,name,source_endpoint,destination_endpoint,phase FROM projects WHERE id=?1", [id], |r| Ok(Project { id:r.get(0)?, name:r.get(1)?, source_endpoint:r.get(2)?, destination_endpoint:r.get(3)?, phase: Phase::parse(&r.get::<_,String>(4)?)? })).optional()
    }
    pub fn latest_project(&self) -> rusqlite::Result<Option<Project>> {
        self.connection.query_row("SELECT id,name,source_endpoint,destination_endpoint,phase FROM projects ORDER BY created_at DESC, rowid DESC LIMIT 1", [], |r| Ok(Project { id:r.get(0)?, name:r.get(1)?, source_endpoint:r.get(2)?, destination_endpoint:r.get(3)?, phase: Phase::parse(&r.get::<_,String>(4)?)? })).optional()
    }
    pub fn recent_projects(&self, limit: usize) -> rusqlite::Result<Vec<ProjectListItem>> {
        let mut statement = self.connection.prepare(
            "SELECT id,name,source_endpoint,destination_endpoint,phase FROM projects ORDER BY created_at DESC, rowid DESC LIMIT ?1",
        )?;
        statement
            .query_map([limit as i64], |row| {
                Ok(ProjectListItem {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    source_endpoint: row.get(2)?,
                    destination_endpoint: row.get(3)?,
                    phase: Phase::parse(&row.get::<_, String>(4)?)?,
                })
            })?
            .collect()
    }

    /// Return the monotonic durable event position used by presentation
    /// caches. A single cheap query lets another controller's committed work
    /// invalidate the workspace read model without rebuilding every
    /// projection on every refresh interval.
    pub fn read_model_revision(&self) -> rusqlite::Result<i64> {
        self.connection
            .query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |row| {
                row.get(0)
            })
    }

    /// Return the durable event position for one project. This lets the
    /// workspace avoid rebuilding the selected project's projections when a
    /// different project changes.
    pub fn project_read_model_revision(&self, project_id: &str) -> rusqlite::Result<i64> {
        self.connection.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM events WHERE project_id=?1",
            [project_id],
            |row| row.get(0),
        )
    }
    pub fn first_mailbox(&self, project_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT id FROM mailbox_jobs WHERE project_id=?1 LIMIT 1",
                [project_id],
                |r| r.get(0),
            )
            .optional()
    }
    pub fn mailbox_identity(
        &self,
        job_id: &str,
    ) -> rusqlite::Result<Option<(String, String, String)>> {
        self.connection
            .query_row(
                "SELECT source_mailbox,destination_mailbox,state FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
    }
    pub fn mailboxes(&self, project_id: &str) -> rusqlite::Result<Vec<MailboxJob>> {
        let mut statement = self.connection.prepare(
            "SELECT id,source_mailbox,destination_mailbox,state,config FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid",
        )?;
        statement
            .query_map([project_id], |row| {
                Ok(MailboxJob {
                    id: row.get(0)?,
                    source_mailbox: row.get(1)?,
                    destination_mailbox: row.get(2)?,
                    state: row.get(3)?,
                    config: row.get(4)?,
                })
            })?
            .collect()
    }

    /// Load only one presentation page. Durable callers that need every row
    /// should continue using `mailboxes`; the workspace must not materialize a
    /// 100,000-row queue merely to render its historical view.
    pub fn mailbox_page(
        &self,
        project_id: &str,
        offset: u32,
        limit: u32,
    ) -> rusqlite::Result<Vec<MailboxJob>> {
        let mut statement = self.connection.prepare(
            "SELECT id,source_mailbox,destination_mailbox,state,config FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid LIMIT ?2 OFFSET ?3",
        )?;
        statement
            .query_map(params![project_id, limit, offset], |row| {
                Ok(MailboxJob {
                    id: row.get(0)?,
                    source_mailbox: row.get(1)?,
                    destination_mailbox: row.get(2)?,
                    state: row.get(3)?,
                    config: row.get(4)?,
                })
            })?
            .collect()
    }

    pub fn mailbox_state_counts(&self, project_id: &str) -> rusqlite::Result<MailboxStateCounts> {
        self.connection.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN state='ready' THEN 1 ELSE 0 END), SUM(CASE WHEN state IN ('running','claimed') THEN 1 ELSE 0 END), SUM(CASE WHEN state IN ('verified','verified_with_exceptions') THEN 1 ELSE 0 END), SUM(CASE WHEN state IN ('attention','failed','cancelled','verification_difference') THEN 1 ELSE 0 END) FROM mailbox_jobs WHERE project_id=?1",
            [project_id],
            |row| {
                Ok(MailboxStateCounts {
                    total: row.get::<_, i64>(0)? as usize,
                    ready: row.get::<_, Option<i64>>(1)?.unwrap_or(0) as usize,
                    running: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as usize,
                    verified: row.get::<_, Option<i64>>(3)?.unwrap_or(0) as usize,
                    needs_review: row.get::<_, Option<i64>>(4)?.unwrap_or(0) as usize,
                })
            },
        )
    }
    pub fn all_mailboxes_verified(&self, project_id: &str) -> rusqlite::Result<bool> {
        let (total, verified): (i64, i64) = self.connection.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN state IN ('verified','verified_with_exceptions') THEN 1 ELSE 0 END) FROM mailbox_jobs WHERE project_id=?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )?;
        Ok(total > 0 && total == verified)
    }
    fn event(&self, id: &str, kind: &str, detail: &str) -> rusqlite::Result<()> {
        let detail = bounded_event_detail(kind, detail);
        self.connection.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,?2,?3)",
            params![id, kind, detail],
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

fn phase_rank(phase: Phase) -> u8 {
    match phase {
        Phase::Discovery => 0,
        Phase::Preflight => 1,
        Phase::Pilot => 2,
        Phase::Seed => 3,
        Phase::CatchUp => 4,
        Phase::FinalDelta => 5,
        Phase::Verification => 6,
        Phase::Complete => 7,
        Phase::Attention => 255,
    }
}

fn valid_mailbox_transition(current: &str, next: &str) -> bool {
    let (Some(current), Some(next)) = (MailboxState::parse(current), MailboxState::parse(next))
    else {
        return false;
    };
    match current {
        MailboxState::Queued => matches!(
            next,
            MailboxState::Preflight
                | MailboxState::Ready
                | MailboxState::Running
                | MailboxState::Failed
                | MailboxState::Cancelled
                | MailboxState::Attention
        ),
        MailboxState::Preflight => matches!(
            next,
            MailboxState::Ready
                | MailboxState::Running
                | MailboxState::Failed
                | MailboxState::Cancelled
        ),
        MailboxState::Ready => {
            matches!(
                next,
                MailboxState::Running | MailboxState::Failed | MailboxState::Cancelled
            )
        }
        MailboxState::Running => matches!(
            next,
            MailboxState::Ready
                | MailboxState::Completed
                | MailboxState::DeltaRequired
                | MailboxState::VerificationDifference
                | MailboxState::Failed
                | MailboxState::Cancelled
                | MailboxState::Attention
        ),
        MailboxState::DeltaRequired | MailboxState::VerificationDifference => matches!(
            next,
            MailboxState::Running
                | MailboxState::Failed
                | MailboxState::Cancelled
                | MailboxState::Attention
                | MailboxState::VerifiedWithExceptions
        ),
        MailboxState::Completed => matches!(
            next,
            MailboxState::Verified
                | MailboxState::DeltaRequired
                | MailboxState::VerificationDifference
                | MailboxState::Running
                | MailboxState::Attention
        ),
        MailboxState::Failed | MailboxState::Cancelled => {
            matches!(next, MailboxState::Running | MailboxState::Attention)
        }
        MailboxState::Verified | MailboxState::VerifiedWithExceptions => matches!(
            next,
            MailboxState::DeltaRequired | MailboxState::Running | MailboxState::Attention
        ),
        MailboxState::Attention => matches!(next, MailboxState::Running),
    }
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
        assert_eq!(version, 6);
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
        assert_eq!(version, 6);
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
