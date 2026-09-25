use super::*;

#[cfg(unix)]
type DatabaseIdentity = (u64, u64);
#[cfg(not(unix))]
type DatabaseIdentity = ();

fn database_identity(path: &Path) -> std::io::Result<DatabaseIdentity> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "state database path is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        Ok(())
    }
}

fn verify_database_identity(path: &Path, expected: DatabaseIdentity) -> std::io::Result<()> {
    if database_identity(path)? != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "state database path changed while opening",
        ));
    }
    Ok(())
}

fn verify_database_parent(path: &Path) -> std::io::Result<&Path> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "state database path has no parent directory",
        )
    })?;
    crate::credentials::verify_private_directory(parent)?;
    Ok(parent)
}

fn create_private_database_file(path: &Path) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    options.open(path).map(drop)
}

fn remove_created_database_file(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let sidecar = Path::new(&format!("{}{}", path.display(), suffix)).to_owned();
        let _ = std::fs::remove_file(sidecar);
    }
}

impl StateStore {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let path = path.as_ref();
        verify_database_parent(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        prepare_database_file(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let database_identity = database_identity(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let store = Self {
            connection: Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
            )?,
        };
        verify_database_identity(path, database_identity)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
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
        let path = path.as_ref();
        verify_database_parent(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let database_identity = database_identity(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        verify_database_identity(path, database_identity)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
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
        verify_database_parent(destination)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        if create_private_database_file(destination).is_err() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let result = (|| {
            let mut backup =
                Connection::open_with_flags(destination, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
            {
                let backup_operation = backup::Backup::new(&self.connection, &mut backup)?;
                backup_operation.run_to_completion(
                    128,
                    std::time::Duration::from_millis(1),
                    None,
                )?;
            }
            let integrity: String =
                backup.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            if integrity != "ok" {
                return Err(rusqlite::Error::InvalidQuery);
            }
            drop(backup);
            restrict_database_permissions(destination)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            restrict_database_sidecars(destination)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            Ok(())
        })();
        if result.is_err() {
            remove_created_database_file(destination);
        }
        result
    }

    /// Snapshot an on-disk ledger through SQLite's backup API. Unlike a file
    /// copy, this includes committed pages currently visible through a WAL
    /// and produces a standalone database without `-wal`/`-shm` sidecars.
    pub fn snapshot_to(source: &Path, destination: &Path) -> rusqlite::Result<()> {
        verify_database_parent(source)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let source_identity = database_identity(source)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let source_connection =
            Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        verify_database_identity(source, source_identity)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        source_connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        let integrity: String =
            source_connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(rusqlite::Error::InvalidQuery);
        }

        verify_database_parent(destination)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        if create_private_database_file(destination).is_err() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut destination_connection =
            match Connection::open_with_flags(destination, OpenFlags::SQLITE_OPEN_READ_WRITE) {
                Ok(connection) => connection,
                Err(error) => {
                    remove_created_database_file(destination);
                    return Err(error);
                }
            };
        let result = (|| {
            {
                let backup = backup::Backup::new(&source_connection, &mut destination_connection)?;
                backup.run_to_completion(128, std::time::Duration::from_millis(1), None)?;
            }
            let integrity: String =
                destination_connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            if integrity != "ok" {
                return Err(rusqlite::Error::InvalidQuery);
            }
            Ok(())
        })();
        drop(destination_connection);
        if let Err(error) = result {
            remove_created_database_file(destination);
            return Err(error);
        }
        if let Err(error) = restrict_database_permissions(destination)
            .and_then(|_| restrict_database_sidecars(destination))
        {
            remove_created_database_file(destination);
            return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(error)));
        }
        Ok(())
    }
    pub(crate) fn migrate(&self) -> rusqlite::Result<()> {
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
                    let subject_column: i64 = self.connection.query_row(
                        "SELECT COUNT(*) FROM pragma_table_info('message_mismatches') WHERE name='subject'",
                        [],
                        |row| row.get(0),
                    )?;
                    if subject_column != 0 {
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
                 CREATE TABLE IF NOT EXISTS evidence (job_id TEXT PRIMARY KEY REFERENCES mailbox_jobs(id), verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL, destination_messages INTEGER NOT NULL, source_bytes INTEGER NOT NULL, destination_bytes INTEGER NOT NULL, unmatched_messages INTEGER, failed_messages INTEGER NOT NULL, source_folders INTEGER NOT NULL DEFAULT 0, destination_folders INTEGER NOT NULL DEFAULT 0, authoritative INTEGER NOT NULL DEFAULT 0, missing_messages INTEGER NOT NULL DEFAULT 0, extra_messages INTEGER NOT NULL DEFAULT 0, modified_messages INTEGER NOT NULL DEFAULT 0, probable_messages INTEGER NOT NULL DEFAULT 0, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS evidence_history (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL, verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL, destination_messages INTEGER NOT NULL, source_bytes INTEGER NOT NULL, destination_bytes INTEGER NOT NULL, unmatched_messages INTEGER, failed_messages INTEGER NOT NULL, source_folders INTEGER NOT NULL, destination_folders INTEGER NOT NULL, authoritative INTEGER NOT NULL DEFAULT 0, missing_messages INTEGER NOT NULL DEFAULT 0, extra_messages INTEGER NOT NULL DEFAULT 0, modified_messages INTEGER NOT NULL DEFAULT 0, probable_messages INTEGER NOT NULL DEFAULT 0, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), job_id TEXT REFERENCES mailbox_jobs(id), parent_run_id TEXT REFERENCES runs(id), engine TEXT NOT NULL, phase_at_start TEXT NOT NULL DEFAULT 'legacy_unknown', plan_snapshot TEXT NOT NULL DEFAULT '', status TEXT NOT NULL, started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, finished_at TEXT, detail TEXT NOT NULL DEFAULT '');
                 CREATE TABLE IF NOT EXISTS active_processes (run_id TEXT NOT NULL REFERENCES runs(id), job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), pid INTEGER NOT NULL, start_ticks INTEGER, process_group INTEGER, session_id INTEGER, executable TEXT NOT NULL DEFAULT '', started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(run_id, job_id));
                 CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), run_id TEXT REFERENCES runs(id), kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS verification_acceptances (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), operator TEXT NOT NULL, reason TEXT NOT NULL, accepted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS engine_versions (run_id TEXT PRIMARY KEY REFERENCES runs(id), version TEXT NOT NULL, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS message_mismatches (id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), mismatch_type TEXT NOT NULL, source_uid TEXT, dest_uid TEXT, source_message_id TEXT, dest_message_id TEXT, source_size_bytes INTEGER, dest_size_bytes INTEGER, source_date TEXT, dest_date TEXT, source_folder TEXT, destination_folder TEXT, source_uidvalidity INTEGER, destination_uidvalidity INTEGER, source_fingerprint TEXT, destination_fingerprint TEXT, source_flags TEXT, destination_flags TEXT, recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS message_extraction (id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), side TEXT NOT NULL DEFAULT 'unknown', mailbox TEXT, message_id TEXT, uid TEXT, uidvalidity INTEGER, size_bytes INTEGER, internal_date TEXT, content_fingerprint TEXT, flags TEXT, extracted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS message_mismatch_acceptance (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), mismatch_id TEXT NOT NULL REFERENCES message_mismatches(id), operator TEXT NOT NULL, reason TEXT NOT NULL, accepted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
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
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_message_mismatches_job_run ON message_mismatches(job_id, run_id, recorded_at)",
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
        if !columns.iter().any(|column| column == "verification_method") {
            tx.execute(
                "ALTER TABLE evidence ADD COLUMN verification_method TEXT NOT NULL DEFAULT 'aggregate_engine'",
                [],
            )?;
        }
        if !columns.iter().any(|column| column == "verification_outcome") {
            tx.execute("ALTER TABLE evidence ADD COLUMN verification_outcome TEXT NOT NULL DEFAULT 'incomplete'", [])?;
        }
        if !columns.iter().any(|column| column == "probable_messages") {
            tx.execute("ALTER TABLE evidence ADD COLUMN probable_messages INTEGER NOT NULL DEFAULT 0", [])?;
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
        if !history_columns.iter().any(|column| column == "verification_method") {
            tx.execute(
                "ALTER TABLE evidence_history ADD COLUMN verification_method TEXT NOT NULL DEFAULT 'aggregate_engine'",
                [],
            )?;
        }
        if !history_columns.iter().any(|column| column == "verification_outcome") {
            tx.execute("ALTER TABLE evidence_history ADD COLUMN verification_outcome TEXT NOT NULL DEFAULT 'incomplete'", [])?;
        }
        if !history_columns.iter().any(|column| column == "probable_messages") {
            tx.execute("ALTER TABLE evidence_history ADD COLUMN probable_messages INTEGER NOT NULL DEFAULT 0", [])?;
        }
        if !history_columns
            .iter()
            .any(|column| column == "authoritative")
        {
            tx.execute(
                "ALTER TABLE evidence_history ADD COLUMN authoritative INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        // Message-level verification columns (schema v8)
        if !columns.iter().any(|column| column == "extra_messages") {
            tx.execute(
                "ALTER TABLE evidence ADD COLUMN extra_messages INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !columns.iter().any(|column| column == "modified_messages") {
            tx.execute(
                "ALTER TABLE evidence ADD COLUMN modified_messages INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !columns.iter().any(|column| column == "missing_messages") {
            tx.execute(
                "ALTER TABLE evidence ADD COLUMN missing_messages INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !history_columns
            .iter()
            .any(|column| column == "extra_messages")
        {
            tx.execute(
                "ALTER TABLE evidence_history ADD COLUMN extra_messages INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !history_columns
            .iter()
            .any(|column| column == "modified_messages")
        {
            tx.execute(
                "ALTER TABLE evidence_history ADD COLUMN modified_messages INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !history_columns
            .iter()
            .any(|column| column == "missing_messages")
        {
            tx.execute(
                "ALTER TABLE evidence_history ADD COLUMN missing_messages INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }

        // Schema v10 makes an unknown imapsync unmatched count durable as
        // SQL NULL. Older schemas used NOT NULL and encoded missing proof as
        // the misleading value 1, so rebuild both tables transactionally.
        let evidence_unmatched_not_null: bool = tx.query_row(
            "SELECT \"notnull\" != 0 FROM pragma_table_info('evidence') WHERE name='unmatched_messages'",
            [],
            |row| row.get(0),
        )?;
        if evidence_unmatched_not_null {
            tx.execute_batch(
                "ALTER TABLE evidence RENAME TO evidence_legacy;
                 CREATE TABLE evidence (job_id TEXT PRIMARY KEY REFERENCES mailbox_jobs(id), verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL, destination_messages INTEGER NOT NULL, source_bytes INTEGER NOT NULL, destination_bytes INTEGER NOT NULL, unmatched_messages INTEGER, failed_messages INTEGER NOT NULL, source_folders INTEGER NOT NULL DEFAULT 0, destination_folders INTEGER NOT NULL DEFAULT 0, authoritative INTEGER NOT NULL DEFAULT 0, missing_messages INTEGER NOT NULL DEFAULT 0, extra_messages INTEGER NOT NULL DEFAULT 0, modified_messages INTEGER NOT NULL DEFAULT 0, probable_messages INTEGER NOT NULL DEFAULT 0, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 INSERT INTO evidence SELECT job_id,'aggregate_engine','incomplete',source_messages,destination_messages,source_bytes,destination_bytes,CASE WHEN authoritative=0 AND unmatched_messages=1 THEN NULL ELSE unmatched_messages END,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,0,captured_at FROM evidence_legacy;
                 DROP TABLE evidence_legacy;
                 ALTER TABLE evidence_history RENAME TO evidence_history_legacy;
                 CREATE TABLE evidence_history (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL, verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL, destination_messages INTEGER NOT NULL, source_bytes INTEGER NOT NULL, destination_bytes INTEGER NOT NULL, unmatched_messages INTEGER, failed_messages INTEGER NOT NULL, source_folders INTEGER NOT NULL, destination_folders INTEGER NOT NULL, authoritative INTEGER NOT NULL DEFAULT 0, missing_messages INTEGER NOT NULL DEFAULT 0, extra_messages INTEGER NOT NULL DEFAULT 0, modified_messages INTEGER NOT NULL DEFAULT 0, probable_messages INTEGER NOT NULL DEFAULT 0, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 INSERT INTO evidence_history SELECT id,job_id,run_id,'aggregate_engine','incomplete',source_messages,destination_messages,source_bytes,destination_bytes,CASE WHEN authoritative=0 AND unmatched_messages=1 THEN NULL ELSE unmatched_messages END,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,0,captured_at FROM evidence_history_legacy;
                 DROP TABLE evidence_history_legacy;",
            )?;
        }

        let mismatch_columns = tx
            .prepare("PRAGMA table_info(message_mismatches)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if mismatch_columns.iter().any(|column| column == "subject") {
            tx.execute("ALTER TABLE message_mismatches DROP COLUMN subject", [])?;
        }
        for (column, definition) in [
            ("source_folder", "TEXT"),
            ("destination_folder", "TEXT"),
            ("source_uidvalidity", "INTEGER"),
            ("destination_uidvalidity", "INTEGER"),
            ("source_fingerprint", "TEXT"),
            ("destination_fingerprint", "TEXT"),
            ("source_flags", "TEXT"),
            ("destination_flags", "TEXT"),
        ] {
            if !mismatch_columns.iter().any(|existing| existing == column) {
                tx.execute(
                    &format!("ALTER TABLE message_mismatches ADD COLUMN {column} {definition}"),
                    [],
                )?;
            }
        }
        let extraction_columns = tx
            .prepare("PRAGMA table_info(message_extraction)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (column, definition) in [
            ("side", "TEXT NOT NULL DEFAULT 'unknown'"),
            ("mailbox", "TEXT"),
            ("uidvalidity", "INTEGER"),
            ("content_fingerprint", "TEXT"),
            ("flags", "TEXT"),
        ] {
            if !extraction_columns.iter().any(|existing| existing == column) {
                tx.execute(
                    &format!("ALTER TABLE message_extraction ADD COLUMN {column} {definition}"),
                    [],
                )?;
            }
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
}
