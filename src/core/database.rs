use super::*;

type SchemaColumn = (&'static str, &'static str, bool, i64);
type SchemaTable = (&'static str, &'static [SchemaColumn]);
type ForeignKey = (&'static str, &'static str, &'static str);
type ForeignKeyTable = (&'static str, &'static [ForeignKey]);

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
        let current_schema_needs_repair = stored_schema_version == CURRENT_SCHEMA_VERSION
            && !Self::current_schema_is_clean(&store.connection);
        if (stored_schema_version < CURRENT_SCHEMA_VERSION || current_schema_needs_repair)
            && std::fs::metadata(path)
                .map(|metadata| metadata.len() > 0)
                .unwrap_or(false)
        {
            // Preserve the exact pre-migration ledger before any schema
            // rewrite. The backup is unique and non-overwriting, so a failed
            // upgrade never destroys the last recovery artifact.
            let backup_path = migration_backup_path(path, stored_schema_version);
            // This is intentionally a raw snapshot: a legacy or dirty
            // layout is exactly what the pre-repair artifact must preserve.
            store.backup_to_unchecked(&backup_path)?;
        }
        restrict_database_permissions(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        store.migrate()?;
        Self::validate_schema_layout(&store.connection)?;
        restrict_database_sidecars(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(store)
    }

    /// Validate the complete v12 application schema, rather than treating
    /// SQLite's user_version as a schema proof. Keep this signature close to
    /// the v12 CREATE TABLE statements in migrate(): a stamped database with
    /// missing, extra, or weakened structure must not pass recovery validation.
    fn validate_schema_layout(connection: &Connection) -> rusqlite::Result<()> {
        const TABLES: &[SchemaTable] = &[
            (
                "projects",
                &[
                    ("id", "TEXT", false, 1),
                    ("name", "TEXT", true, 0),
                    ("source_endpoint", "TEXT", true, 0),
                    ("destination_endpoint", "TEXT", true, 0),
                    ("phase", "TEXT", true, 0),
                    ("created_at", "TEXT", true, 0),
                ],
            ),
            (
                "mailbox_jobs",
                &[
                    ("id", "TEXT", false, 1),
                    ("project_id", "TEXT", true, 0),
                    ("source_mailbox", "TEXT", true, 0),
                    ("destination_mailbox", "TEXT", true, 0),
                    ("destination_identity", "TEXT", true, 0),
                    ("state", "TEXT", true, 0),
                    ("attempt", "INTEGER", true, 0),
                    ("checkpoint", "TEXT", false, 0),
                    ("preflight_plan", "TEXT", false, 0),
                    ("config", "TEXT", false, 0),
                    ("attention_reason", "TEXT", false, 0),
                ],
            ),
            (
                "evidence",
                &[
                    ("job_id", "TEXT", false, 1),
                    ("verification_method", "TEXT", true, 0),
                    ("verification_outcome", "TEXT", true, 0),
                    ("source_messages", "INTEGER", true, 0),
                    ("destination_messages", "INTEGER", true, 0),
                    ("source_bytes", "INTEGER", true, 0),
                    ("destination_bytes", "INTEGER", true, 0),
                    ("unmatched_messages", "INTEGER", false, 0),
                    ("failed_messages", "INTEGER", true, 0),
                    ("source_folders", "INTEGER", true, 0),
                    ("destination_folders", "INTEGER", true, 0),
                    ("authoritative", "INTEGER", true, 0),
                    ("missing_messages", "INTEGER", true, 0),
                    ("extra_messages", "INTEGER", true, 0),
                    ("modified_messages", "INTEGER", true, 0),
                    ("probable_messages", "INTEGER", true, 0),
                    ("captured_at", "TEXT", true, 0),
                ],
            ),
            (
                "evidence_history",
                &[
                    ("id", "INTEGER", false, 1),
                    ("job_id", "TEXT", true, 0),
                    ("run_id", "TEXT", true, 0),
                    ("verification_method", "TEXT", true, 0),
                    ("verification_outcome", "TEXT", true, 0),
                    ("source_messages", "INTEGER", true, 0),
                    ("destination_messages", "INTEGER", true, 0),
                    ("source_bytes", "INTEGER", true, 0),
                    ("destination_bytes", "INTEGER", true, 0),
                    ("unmatched_messages", "INTEGER", false, 0),
                    ("failed_messages", "INTEGER", true, 0),
                    ("source_folders", "INTEGER", true, 0),
                    ("destination_folders", "INTEGER", true, 0),
                    ("authoritative", "INTEGER", true, 0),
                    ("missing_messages", "INTEGER", true, 0),
                    ("extra_messages", "INTEGER", true, 0),
                    ("modified_messages", "INTEGER", true, 0),
                    ("probable_messages", "INTEGER", true, 0),
                    ("captured_at", "TEXT", true, 0),
                ],
            ),
            (
                "runs",
                &[
                    ("id", "TEXT", false, 1),
                    ("project_id", "TEXT", true, 0),
                    ("job_id", "TEXT", false, 0),
                    ("parent_run_id", "TEXT", false, 0),
                    ("engine", "TEXT", true, 0),
                    ("phase_at_start", "TEXT", true, 0),
                    ("plan_snapshot", "TEXT", true, 0),
                    ("status", "TEXT", true, 0),
                    ("started_at", "TEXT", true, 0),
                    ("finished_at", "TEXT", false, 0),
                    ("detail", "TEXT", true, 0),
                ],
            ),
            (
                "active_processes",
                &[
                    ("run_id", "TEXT", true, 1),
                    ("job_id", "TEXT", true, 2),
                    ("pid", "INTEGER", true, 0),
                    ("start_ticks", "INTEGER", false, 0),
                    ("process_group", "INTEGER", false, 0),
                    ("session_id", "INTEGER", false, 0),
                    ("executable", "TEXT", true, 0),
                    ("started_at", "TEXT", true, 0),
                ],
            ),
            (
                "events",
                &[
                    ("id", "INTEGER", false, 1),
                    ("project_id", "TEXT", true, 0),
                    ("run_id", "TEXT", false, 0),
                    ("kind", "TEXT", true, 0),
                    ("detail", "TEXT", true, 0),
                    ("created_at", "TEXT", true, 0),
                ],
            ),
            (
                "verification_acceptances",
                &[
                    ("id", "INTEGER", false, 1),
                    ("job_id", "TEXT", true, 0),
                    ("run_id", "TEXT", true, 0),
                    ("operator", "TEXT", true, 0),
                    ("reason", "TEXT", true, 0),
                    ("accepted_at", "TEXT", true, 0),
                ],
            ),
            (
                "engine_versions",
                &[
                    ("run_id", "TEXT", false, 1),
                    ("version", "TEXT", true, 0),
                    ("captured_at", "TEXT", true, 0),
                ],
            ),
            (
                "message_mismatches",
                &[
                    ("id", "TEXT", false, 1),
                    ("job_id", "TEXT", true, 0),
                    ("run_id", "TEXT", true, 0),
                    ("mismatch_type", "TEXT", true, 0),
                    ("source_uid", "TEXT", false, 0),
                    ("dest_uid", "TEXT", false, 0),
                    ("source_message_id", "TEXT", false, 0),
                    ("dest_message_id", "TEXT", false, 0),
                    ("source_size_bytes", "INTEGER", false, 0),
                    ("dest_size_bytes", "INTEGER", false, 0),
                    ("source_date", "TEXT", false, 0),
                    ("dest_date", "TEXT", false, 0),
                    ("source_folder", "TEXT", false, 0),
                    ("destination_folder", "TEXT", false, 0),
                    ("source_uidvalidity", "INTEGER", false, 0),
                    ("destination_uidvalidity", "INTEGER", false, 0),
                    ("source_fingerprint", "TEXT", false, 0),
                    ("destination_fingerprint", "TEXT", false, 0),
                    ("recorded_at", "TEXT", true, 0),
                ],
            ),
        ];
        for (table, expected) in TABLES {
            let mut actual = connection
                .prepare(&format!("PRAGMA table_info({table})"))?
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, i64>(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            // v12 briefly created these unused columns. Accept them as
            // legacy baggage so existing ledgers remain recoverable while
            // new ledgers use the smaller runtime schema.
            if *table == "message_mismatches" {
                actual.retain(|(name, _, _, _)| {
                    name != "source_flags" && name != "destination_flags"
                });
            }
            if actual.len() != expected.len()
                || expected.iter().any(|expected_column| {
                    actual
                        .iter()
                        .find(|actual_column| actual_column.0 == expected_column.0)
                        .is_none_or(|actual_column| {
                            actual_column.1 != expected_column.1
                                || actual_column.2 != expected_column.2
                                || actual_column.3 != expected_column.3
                        })
                })
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        const FOREIGN_KEYS: &[ForeignKeyTable] = &[
            ("mailbox_jobs", &[("projects", "project_id", "id")]),
            ("evidence", &[("mailbox_jobs", "job_id", "id")]),
            (
                "runs",
                &[
                    ("projects", "project_id", "id"),
                    ("mailbox_jobs", "job_id", "id"),
                    ("runs", "parent_run_id", "id"),
                ],
            ),
            (
                "active_processes",
                &[("runs", "run_id", "id"), ("mailbox_jobs", "job_id", "id")],
            ),
            (
                "events",
                &[("projects", "project_id", "id"), ("runs", "run_id", "id")],
            ),
            (
                "verification_acceptances",
                &[("mailbox_jobs", "job_id", "id"), ("runs", "run_id", "id")],
            ),
            ("engine_versions", &[("runs", "run_id", "id")]),
            (
                "message_mismatches",
                &[("mailbox_jobs", "job_id", "id"), ("runs", "run_id", "id")],
            ),
        ];
        for (table, expected) in FOREIGN_KEYS {
            let actual = connection
                .prepare(&format!("PRAGMA foreign_key_list({table})"))?
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if actual.len() != expected.len()
                || !expected.iter().all(|(parent, from, to)| {
                    actual
                        .iter()
                        .any(|(a, b, c)| a == parent && b == from && c == to)
                })
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        const INDEXES: &[&str] = &[
            "idx_mailbox_jobs_project_state",
            "idx_runs_project_started",
            "idx_runs_job_started",
            "idx_events_project_created",
            "idx_events_project_kind_id",
            "idx_evidence_history_job_captured",
            "idx_active_processes_pid",
            "idx_verification_acceptances_job",
            "idx_engine_versions_captured",
            "idx_message_mismatches_job_run",
            "idx_events_run_created",
            "one_running_run_per_job",
            "one_active_run_per_job",
        ];
        for index in INDEXES {
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1)",
                [index],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        for (index, expected_unique) in [
            ("one_running_run_per_job", true),
            ("one_active_run_per_job", true),
        ] {
            let (unique, partial): (i64, i64) = connection.query_row(
                "SELECT \"unique\", partial FROM pragma_index_list('runs') WHERE name=?1",
                [index],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if (unique != expected_unique as i64) || partial != 1 {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        // SQLite INTEGER values are signed, while the application exposes
        // these counters and byte sizes as u64. Reject negative values at the
        // ledger boundary instead of allowing a malformed but structurally
        // valid database to become a huge value through an unchecked cast.
        const NON_NEGATIVE_COLUMNS: &[(&str, &str)] = &[
            ("mailbox_jobs", "attempt"),
            ("evidence", "source_messages"),
            ("evidence", "destination_messages"),
            ("evidence", "source_bytes"),
            ("evidence", "destination_bytes"),
            ("evidence", "unmatched_messages"),
            ("evidence", "failed_messages"),
            ("evidence", "source_folders"),
            ("evidence", "destination_folders"),
            ("evidence", "missing_messages"),
            ("evidence", "extra_messages"),
            ("evidence", "modified_messages"),
            ("evidence", "probable_messages"),
            ("evidence_history", "source_messages"),
            ("evidence_history", "destination_messages"),
            ("evidence_history", "source_bytes"),
            ("evidence_history", "destination_bytes"),
            ("evidence_history", "unmatched_messages"),
            ("evidence_history", "failed_messages"),
            ("evidence_history", "source_folders"),
            ("evidence_history", "destination_folders"),
            ("evidence_history", "missing_messages"),
            ("evidence_history", "extra_messages"),
            ("evidence_history", "modified_messages"),
            ("evidence_history", "probable_messages"),
            ("message_mismatches", "source_uidvalidity"),
            ("message_mismatches", "destination_uidvalidity"),
            ("message_mismatches", "source_size_bytes"),
            ("message_mismatches", "dest_size_bytes"),
            ("active_processes", "pid"),
            ("active_processes", "start_ticks"),
            ("active_processes", "process_group"),
            ("active_processes", "session_id"),
        ];
        for (table, column) in NON_NEGATIVE_COLUMNS {
            let sql = format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {column} < 0)");
            let has_negative: bool = connection.query_row(&sql, [], |row| row.get(0))?;
            if has_negative {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        // Persisted wire values are part of the application schema too. Do
        // not let readers silently map an unknown value to a default enum
        // variant and present corrupted state as an ordinary ledger.
        const ENUM_CHECKS: &[&str] = &[
            "SELECT EXISTS(SELECT 1 FROM projects WHERE phase NOT IN ('discovery','preflight','pilot','seed','catch_up','final_delta','verification','complete','attention'))",
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE state NOT IN ('imported','queued','preflight','ready','running','completed','verified','verified_with_exceptions','failed','cancelled','attention','delta_required','verification_difference'))",
            "SELECT EXISTS(SELECT 1 FROM runs WHERE status NOT IN ('queued','running','completed','failed','cancelled','abandoned','verification_failed'))",
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE verification_method NOT IN ('aggregate_engine','metadata_reconciliation','body_hash','native_dovecot'))",
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE verification_outcome NOT IN ('exact_metadata_match','probable_match','ambiguous','missing','changed','unexpected','incomplete','failed'))",
            "SELECT EXISTS(SELECT 1 FROM evidence_history WHERE verification_method NOT IN ('aggregate_engine','metadata_reconciliation','body_hash','native_dovecot'))",
            "SELECT EXISTS(SELECT 1 FROM evidence_history WHERE verification_outcome NOT IN ('exact_metadata_match','probable_match','ambiguous','missing','changed','unexpected','incomplete','failed'))",
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE authoritative NOT IN (0,1))",
            "SELECT EXISTS(SELECT 1 FROM evidence_history WHERE authoritative NOT IN (0,1))",
            "SELECT EXISTS(SELECT 1 FROM message_mismatches WHERE mismatch_type NOT IN ('message_id_only','message_present_wrong_folder','missing','extra','duplicated'))",
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE attention_reason IS NOT NULL AND attention_reason NOT IN ('interrupted','verification_incomplete','verification_difference','process_identity_unverified','authentication_failed','transport_failed','policy_blocked','configuration_invalid','capacity_limited','message_rejected','unknown'))",
        ];
        for sql in ENUM_CHECKS {
            let has_unknown: bool = connection.query_row(sql, [], |row| row.get(0))?;
            if has_unknown {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        Ok(())
    }

    fn current_schema_is_clean(connection: &Connection) -> bool {
        if Self::validate_schema_layout(connection).is_err() {
            return false;
        }
        (|| -> rusqlite::Result<bool> {
            let legacy_plan: i64 = connection.query_row("SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE preflight_plan IS NOT NULL AND (length(preflight_plan) <> 64 OR preflight_plan GLOB '*[^0-9A-Fa-f]*'))", [], |row| row.get(0))?;
            let duplicate_active_run: i64 = connection.query_row("SELECT EXISTS(SELECT 1 FROM (SELECT job_id FROM runs WHERE job_id IS NOT NULL AND status IN ('queued','running') GROUP BY job_id HAVING COUNT(*) > 1))", [], |row| row.get(0))?;
            let raw_output_event: i64 = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE kind='run_output')",
                [],
                |row| row.get(0),
            )?;
            let mut stale_identity = false;
            let mut identities = connection.prepare(
                "SELECT destination_mailbox, destination_identity, config FROM mailbox_jobs",
            )?;
            for row in identities.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })? {
                let (destination, identity, config) = row?;
                if normalized_destination_identity(&destination, config.as_deref()) != identity {
                    stale_identity = true;
                    break;
                }
            }
            Ok(legacy_plan == 0
                && duplicate_active_run == 0
                && raw_output_event == 0
                && !stale_identity)
        })().unwrap_or(false)
    }

    fn has_legacy_mailbox_table(connection: &Connection) -> rusqlite::Result<bool> {
        connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='mailbox_jobs')",
            [],
            |row| row.get(0),
        )
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
            Self::validate_schema_layout(&connection)?;
            if Self::current_schema_is_clean(&connection) {
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
            Self::validate_schema_layout(&store.connection)?;
            return Ok(store);
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
        Self::validate_schema_layout(&store.connection)?;
        Ok(store)
    }
    pub fn in_memory() -> rusqlite::Result<Self> {
        let store = Self {
            connection: Connection::open_in_memory()?,
        };
        store.migrate()?;
        Self::validate_schema_layout(&store.connection)?;
        Ok(store)
    }

    /// Create a consistent SQLite backup without copying WAL/SHM files by
    /// hand. The destination must not already exist, preventing an operator
    /// typo from silently overwriting a prior recovery artifact.
    pub fn backup_to(&self, destination: &Path) -> rusqlite::Result<()> {
        Self::validate_schema_layout(&self.connection)?;
        self.backup_to_unchecked(destination)
    }

    fn backup_to_unchecked(&self, destination: &Path) -> rusqlite::Result<()> {
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
        let source_schema_version: i64 =
            source_connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if source_schema_version == CURRENT_SCHEMA_VERSION {
            Self::validate_schema_layout(&source_connection)?;
        } else if source_schema_version > CURRENT_SCHEMA_VERSION
            || !Self::has_legacy_mailbox_table(&source_connection)?
        {
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
            if source_schema_version == CURRENT_SCHEMA_VERSION {
                Self::validate_schema_layout(&destination_connection)?;
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
            let current_schema_is_clean = Self::current_schema_is_clean(&self.connection);
            if current_schema_is_clean {
                // A clean v12 ledger needs no launch-time data rewrite. The
                // identity is calculated when a mailbox is created or its
                // configuration changes; raw output cleanup belongs to the
                // compatibility-repair path below.
                return Ok(());
            }
        }
        // Keep all compatibility repairs, constraint creation, and the
        // version stamp in one transaction. If an upgrade fails halfway
        // through, SQLite can roll back to the prior durable ledger.
        let tx = self.connection.unchecked_transaction()?;
        Self::prepare_legacy_mailbox_jobs(&tx)?;
        tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, source_endpoint TEXT NOT NULL, destination_endpoint TEXT NOT NULL, phase TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS mailbox_jobs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), source_mailbox TEXT NOT NULL, destination_mailbox TEXT NOT NULL, destination_identity TEXT NOT NULL DEFAULT '', state TEXT NOT NULL, attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0), checkpoint TEXT, preflight_plan TEXT, config TEXT, attention_reason TEXT);
                 CREATE TABLE IF NOT EXISTS evidence (job_id TEXT PRIMARY KEY REFERENCES mailbox_jobs(id), verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL CHECK(source_messages >= 0), destination_messages INTEGER NOT NULL CHECK(destination_messages >= 0), source_bytes INTEGER NOT NULL CHECK(source_bytes >= 0), destination_bytes INTEGER NOT NULL CHECK(destination_bytes >= 0), unmatched_messages INTEGER CHECK(unmatched_messages IS NULL OR unmatched_messages >= 0), failed_messages INTEGER NOT NULL CHECK(failed_messages >= 0), source_folders INTEGER NOT NULL DEFAULT 0 CHECK(source_folders >= 0), destination_folders INTEGER NOT NULL DEFAULT 0 CHECK(destination_folders >= 0), authoritative INTEGER NOT NULL DEFAULT 0 CHECK(authoritative IN (0,1)), missing_messages INTEGER NOT NULL DEFAULT 0 CHECK(missing_messages >= 0), extra_messages INTEGER NOT NULL DEFAULT 0 CHECK(extra_messages >= 0), modified_messages INTEGER NOT NULL DEFAULT 0 CHECK(modified_messages >= 0), probable_messages INTEGER NOT NULL DEFAULT 0 CHECK(probable_messages >= 0), captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS evidence_history (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL, verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL CHECK(source_messages >= 0), destination_messages INTEGER NOT NULL CHECK(destination_messages >= 0), source_bytes INTEGER NOT NULL CHECK(source_bytes >= 0), destination_bytes INTEGER NOT NULL CHECK(destination_bytes >= 0), unmatched_messages INTEGER CHECK(unmatched_messages IS NULL OR unmatched_messages >= 0), failed_messages INTEGER NOT NULL CHECK(failed_messages >= 0), source_folders INTEGER NOT NULL CHECK(source_folders >= 0), destination_folders INTEGER NOT NULL CHECK(destination_folders >= 0), authoritative INTEGER NOT NULL DEFAULT 0 CHECK(authoritative IN (0,1)), missing_messages INTEGER NOT NULL DEFAULT 0 CHECK(missing_messages >= 0), extra_messages INTEGER NOT NULL DEFAULT 0 CHECK(extra_messages >= 0), modified_messages INTEGER NOT NULL DEFAULT 0 CHECK(modified_messages >= 0), probable_messages INTEGER NOT NULL DEFAULT 0 CHECK(probable_messages >= 0), captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), job_id TEXT REFERENCES mailbox_jobs(id), parent_run_id TEXT REFERENCES runs(id), engine TEXT NOT NULL, phase_at_start TEXT NOT NULL DEFAULT 'legacy_unknown', plan_snapshot TEXT NOT NULL DEFAULT '', status TEXT NOT NULL, started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, finished_at TEXT, detail TEXT NOT NULL DEFAULT '');
                 CREATE TABLE IF NOT EXISTS active_processes (run_id TEXT NOT NULL REFERENCES runs(id), job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), pid INTEGER NOT NULL, start_ticks INTEGER, process_group INTEGER, session_id INTEGER, executable TEXT NOT NULL DEFAULT '', started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(run_id, job_id));
                 CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), run_id TEXT REFERENCES runs(id), kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS verification_acceptances (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), operator TEXT NOT NULL, reason TEXT NOT NULL, accepted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS engine_versions (run_id TEXT PRIMARY KEY REFERENCES runs(id), version TEXT NOT NULL, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS message_mismatches (id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), mismatch_type TEXT NOT NULL, source_uid TEXT, dest_uid TEXT, source_message_id TEXT, dest_message_id TEXT, source_size_bytes INTEGER, dest_size_bytes INTEGER, source_date TEXT, dest_date TEXT, source_folder TEXT, destination_folder TEXT, source_uidvalidity INTEGER, destination_uidvalidity INTEGER, source_fingerprint TEXT, destination_fingerprint TEXT, recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
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
            "CREATE INDEX IF NOT EXISTS idx_message_mismatches_job_run ON message_mismatches(job_id, run_id, recorded_at)",
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
        if !columns.iter().any(|column| column == "verification_method") {
            tx.execute(
                "ALTER TABLE evidence ADD COLUMN verification_method TEXT NOT NULL DEFAULT 'aggregate_engine'",
                [],
            )?;
        }
        if !columns
            .iter()
            .any(|column| column == "verification_outcome")
        {
            tx.execute("ALTER TABLE evidence ADD COLUMN verification_outcome TEXT NOT NULL DEFAULT 'incomplete'", [])?;
        }
        if !columns.iter().any(|column| column == "probable_messages") {
            tx.execute(
                "ALTER TABLE evidence ADD COLUMN probable_messages INTEGER NOT NULL DEFAULT 0",
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
            .any(|column| column == "verification_method")
        {
            tx.execute(
                "ALTER TABLE evidence_history ADD COLUMN verification_method TEXT NOT NULL DEFAULT 'aggregate_engine'",
                [],
            )?;
        }
        if !history_columns
            .iter()
            .any(|column| column == "verification_outcome")
        {
            tx.execute("ALTER TABLE evidence_history ADD COLUMN verification_outcome TEXT NOT NULL DEFAULT 'incomplete'", [])?;
        }
        if !history_columns
            .iter()
            .any(|column| column == "probable_messages")
        {
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
                 CREATE TABLE evidence (job_id TEXT PRIMARY KEY REFERENCES mailbox_jobs(id), verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL CHECK(source_messages >= 0), destination_messages INTEGER NOT NULL CHECK(destination_messages >= 0), source_bytes INTEGER NOT NULL CHECK(source_bytes >= 0), destination_bytes INTEGER NOT NULL CHECK(destination_bytes >= 0), unmatched_messages INTEGER CHECK(unmatched_messages IS NULL OR unmatched_messages >= 0), failed_messages INTEGER NOT NULL CHECK(failed_messages >= 0), source_folders INTEGER NOT NULL DEFAULT 0 CHECK(source_folders >= 0), destination_folders INTEGER NOT NULL DEFAULT 0 CHECK(destination_folders >= 0), authoritative INTEGER NOT NULL DEFAULT 0 CHECK(authoritative IN (0,1)), missing_messages INTEGER NOT NULL DEFAULT 0 CHECK(missing_messages >= 0), extra_messages INTEGER NOT NULL DEFAULT 0 CHECK(extra_messages >= 0), modified_messages INTEGER NOT NULL DEFAULT 0 CHECK(modified_messages >= 0), probable_messages INTEGER NOT NULL DEFAULT 0 CHECK(probable_messages >= 0), captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 INSERT INTO evidence SELECT job_id,'aggregate_engine','incomplete',source_messages,destination_messages,source_bytes,destination_bytes,CASE WHEN authoritative=0 AND unmatched_messages=1 THEN NULL ELSE unmatched_messages END,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,0,captured_at FROM evidence_legacy;
                 DROP TABLE evidence_legacy;
                 ALTER TABLE evidence_history RENAME TO evidence_history_legacy;
                 CREATE TABLE evidence_history (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL, verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL CHECK(source_messages >= 0), destination_messages INTEGER NOT NULL CHECK(destination_messages >= 0), source_bytes INTEGER NOT NULL CHECK(source_bytes >= 0), destination_bytes INTEGER NOT NULL CHECK(destination_bytes >= 0), unmatched_messages INTEGER CHECK(unmatched_messages IS NULL OR unmatched_messages >= 0), failed_messages INTEGER NOT NULL CHECK(failed_messages >= 0), source_folders INTEGER NOT NULL CHECK(source_folders >= 0), destination_folders INTEGER NOT NULL CHECK(destination_folders >= 0), authoritative INTEGER NOT NULL DEFAULT 0 CHECK(authoritative IN (0,1)), missing_messages INTEGER NOT NULL DEFAULT 0 CHECK(missing_messages >= 0), extra_messages INTEGER NOT NULL DEFAULT 0 CHECK(extra_messages >= 0), modified_messages INTEGER NOT NULL DEFAULT 0 CHECK(modified_messages >= 0), probable_messages INTEGER NOT NULL DEFAULT 0 CHECK(probable_messages >= 0), captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
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
        ] {
            if !mismatch_columns.iter().any(|existing| existing == column) {
                tx.execute(
                    &format!("ALTER TABLE message_mismatches ADD COLUMN {column} {definition}"),
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
                "UPDATE mailbox_jobs SET destination_identity=?1 WHERE id=?2 AND destination_identity<>?1",
                params![identity, job_id],
            )?;
        }
        Ok(())
    }

    fn rebuild_mailbox_jobs_with_foreign_key(
        tx: &rusqlite::Transaction<'_>,
    ) -> rusqlite::Result<()> {
        let has_project_foreign_key: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list('mailbox_jobs') WHERE \"from\"='project_id' AND \"table\"='projects' AND \"to\"='id')",
            [],
            |row| row.get(0),
        )?;
        if has_project_foreign_key {
            return Ok(());
        }
        // SQLite cannot add a foreign key to an existing table. Rebuild only
        // legacy layouts that predate the constraint, retaining all data.
        // Placeholder projects keep old orphaned job rows recoverable while
        // making the repaired relationship explicit.
        tx.execute_batch(
            "DROP INDEX IF EXISTS idx_mailbox_jobs_project_state;
             INSERT OR IGNORE INTO projects(id,name,source_endpoint,destination_endpoint,phase)
             SELECT DISTINCT project_id, 'Legacy project', '', '', 'planning'
             FROM mailbox_jobs WHERE project_id IS NOT NULL;
             ALTER TABLE mailbox_jobs RENAME TO mailbox_jobs_legacy;
             CREATE TABLE mailbox_jobs (
                 id TEXT PRIMARY KEY,
                 project_id TEXT NOT NULL REFERENCES projects(id),
                 source_mailbox TEXT NOT NULL,
                 destination_mailbox TEXT NOT NULL,
                 destination_identity TEXT NOT NULL DEFAULT '',
                 state TEXT NOT NULL,
                 attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0),
                 checkpoint TEXT,
                 preflight_plan TEXT,
                 config TEXT,
                 attention_reason TEXT
             );
             INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,destination_identity,state,attempt,checkpoint,preflight_plan,config,attention_reason)
             SELECT id,project_id,source_mailbox,destination_mailbox,destination_identity,state,attempt,checkpoint,preflight_plan,config,attention_reason
             FROM mailbox_jobs_legacy;
             DROP TABLE mailbox_jobs_legacy;",
        )?;
        Ok(())
    }

    fn prepare_legacy_mailbox_jobs(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='mailbox_jobs')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(());
        }
        let has_project_foreign_key: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list('mailbox_jobs') WHERE \"from\"='project_id' AND \"table\"='projects' AND \"to\"='id')",
            [],
            |row| row.get(0),
        )?;
        if has_project_foreign_key {
            return Ok(());
        }
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, source_endpoint TEXT NOT NULL, destination_endpoint TEXT NOT NULL, phase TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);",
        )?;
        let columns = tx
            .prepare("PRAGMA table_info(mailbox_jobs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (column, definition) in [
            ("destination_identity", "TEXT NOT NULL DEFAULT ''"),
            ("preflight_plan", "TEXT"),
            ("config", "TEXT"),
            ("attention_reason", "TEXT"),
        ] {
            if !columns.iter().any(|existing| existing == column) {
                tx.execute(
                    &format!("ALTER TABLE mailbox_jobs ADD COLUMN {column} {definition}"),
                    [],
                )?;
            }
        }
        Self::rebuild_mailbox_jobs_with_foreign_key(tx)
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
