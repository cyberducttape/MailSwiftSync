use super::*;
mod files;
use files::{
    create_private_database_file, database_identity, remove_created_database_file,
    verify_database_identity, verify_database_parent,
};

type SchemaColumn = (&'static str, &'static str, bool, i64);
type SchemaTable = (&'static str, &'static [SchemaColumn]);
type ForeignKey = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);
type ForeignKeyTable = (&'static str, &'static [ForeignKey]);

/// Extract CHECK expressions from SQLite's stored table DDL. Looking for a
/// CHECK-looking substring is unsafe here: the same text can appear in a
/// default string or a comment without enforcing any constraint.
fn sqlite_check_expressions(sql: &str) -> Vec<String> {
    fn skip_quoted(bytes: &[u8], mut index: usize, quote: u8) -> usize {
        index += 1;
        while index < bytes.len() {
            if bytes[index] == quote {
                if bytes.get(index + 1) == Some(&quote) {
                    index += 2;
                    continue;
                }
                return index + 1;
            }
            index += 1;
        }
        bytes.len()
    }

    fn skip_comment(bytes: &[u8], index: usize) -> usize {
        if bytes.get(index..index + 2) == Some(b"--") {
            bytes[index + 2..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |offset| index + 2 + offset + 1)
        } else if bytes.get(index..index + 2) == Some(b"/*") {
            bytes[index + 2..]
                .windows(2)
                .position(|pair| pair == b"*/")
                .map_or(bytes.len(), |offset| index + 2 + offset + 2)
        } else {
            index
        }
    }

    let bytes = sql.as_bytes();
    let mut checks = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => index = skip_quoted(bytes, index, bytes[index]),
            b'[' => {
                index = bytes[index + 1..]
                    .iter()
                    .position(|byte| *byte == b']')
                    .map_or(bytes.len(), |offset| index + offset + 2);
            }
            b'-' | b'/' => {
                let next = skip_comment(bytes, index);
                index = if next == index { index + 1 } else { next };
            }
            byte if byte.eq_ignore_ascii_case(&b'C')
                && bytes
                    .get(index..index + 5)
                    .is_some_and(|token| token.eq_ignore_ascii_case(b"check"))
                && (index == 0 || !is_sql_identifier(bytes[index - 1]))
                && bytes
                    .get(index + 5)
                    .is_none_or(|byte| !is_sql_identifier(*byte)) =>
            {
                let mut open = index + 5;
                while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
                    open += 1;
                }
                if bytes.get(open) != Some(&b'(') {
                    index += 5;
                    continue;
                }
                let mut depth = 1usize;
                let mut cursor = open + 1;
                let mut expression = Vec::new();
                while cursor < bytes.len() && depth > 0 {
                    match bytes[cursor] {
                        b'\'' | b'"' | b'`' => {
                            let end = skip_quoted(bytes, cursor, bytes[cursor]);
                            expression.extend_from_slice(&bytes[cursor..end]);
                            cursor = end;
                        }
                        b'[' => {
                            let end = bytes[cursor + 1..]
                                .iter()
                                .position(|byte| *byte == b']')
                                .map_or(bytes.len(), |offset| cursor + offset + 2);
                            expression.extend_from_slice(&bytes[cursor..end]);
                            cursor = end;
                        }
                        b'-' | b'/' => {
                            let end = skip_comment(bytes, cursor);
                            if end == cursor {
                                expression.push(bytes[cursor]);
                                cursor += 1;
                            } else {
                                cursor = end;
                            }
                        }
                        b'(' => {
                            depth += 1;
                            expression.push(b'(');
                            cursor += 1;
                        }
                        b')' => {
                            depth -= 1;
                            if depth > 0 {
                                expression.push(b')');
                            }
                            cursor += 1;
                        }
                        byte => {
                            if !byte.is_ascii_whitespace() {
                                expression.push(byte.to_ascii_lowercase());
                            }
                            cursor += 1;
                        }
                    }
                }
                if depth == 0 {
                    checks.push(String::from_utf8_lossy(&expression).into_owned());
                    index = cursor;
                } else {
                    index = bytes.len();
                }
            }
            _ => index += 1,
        }
    }
    checks
}

fn is_sql_identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
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
            && (!Self::current_schema_is_clean(&store.connection)
                || Self::validate_schema_layout(&store.connection).is_err());
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
        Self::validate_schema_constraints(&store.connection)?;
        restrict_database_sidecars(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(store)
    }

    /// Validate the complete current application schema, rather than treating
    /// SQLite's user_version as a schema proof. Keep this signature close to
    /// the current CREATE TABLE statements in migrate(): a stamped database with
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
                    ("batch_plan_id", "TEXT", false, 0),
                    ("row_overrides", "TEXT", false, 0),
                ],
            ),
            (
                "batch_plans",
                &[
                    ("id", "TEXT", false, 1),
                    ("project_id", "TEXT", true, 0),
                    ("config", "TEXT", true, 0),
                ],
            ),
            (
                "cutover_workflows",
                &[
                    ("project_id", "TEXT", false, 1),
                    ("stage", "TEXT", true, 0),
                    ("scheduled_at", "TEXT", true, 0),
                    ("maintenance_window", "TEXT", false, 0),
                    ("approved_by", "TEXT", false, 0),
                    ("approved_at", "TEXT", false, 0),
                    ("external_confirmation", "TEXT", false, 0),
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
                    ("run_id", "TEXT", false, 0),
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
                "webhook_deliveries",
                &[
                    ("event_id", "TEXT", false, 1),
                    ("project_id", "TEXT", true, 0),
                    ("event_type", "TEXT", true, 0),
                    ("payload", "TEXT", true, 0),
                    ("endpoint_digest", "TEXT", true, 0),
                    ("attempts", "INTEGER", true, 0),
                    ("status", "TEXT", true, 0),
                    ("last_error", "TEXT", false, 0),
                    ("next_attempt_at", "TEXT", true, 0),
                    ("created_at", "TEXT", true, 0),
                    ("delivered_at", "TEXT", false, 0),
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
            (
                "transfer_passes",
                &[
                    ("run_id", "TEXT", true, 1),
                    ("attempt", "INTEGER", true, 2),
                    ("project_id", "TEXT", true, 0),
                    ("mailbox_digest", "TEXT", true, 0),
                    ("pass_sequence", "INTEGER", true, 0),
                    ("pass_kind", "TEXT", true, 0),
                    ("engine", "TEXT", true, 0),
                    ("executable_identity", "TEXT", true, 0),
                    ("command_sha256", "TEXT", true, 0),
                    ("command", "TEXT", true, 0),
                    ("folder_scope", "TEXT", true, 0),
                    ("source_range", "TEXT", true, 0),
                    ("started_at", "TEXT", true, 0),
                    ("finished_at", "TEXT", false, 0),
                    ("outcome", "TEXT", false, 0),
                    ("delta_required", "INTEGER", false, 0),
                    ("completion_evidence", "TEXT", false, 0),
                    ("emitted_state_sha256", "TEXT", false, 0),
                    ("verification_method", "TEXT", false, 0),
                    ("verification_outcome", "TEXT", false, 0),
                    ("verified_at", "TEXT", false, 0),
                ],
            ),
            (
                "mailbox_queue_facts",
                &[
                    ("job_rowid", "INTEGER", false, 1),
                    ("job_id", "TEXT", true, 0),
                    ("project_id", "TEXT", true, 0),
                    ("label", "TEXT", true, 0),
                    ("source_host", "TEXT", true, 0),
                    ("destination_host", "TEXT", true, 0),
                    ("search_key", "TEXT", true, 0),
                    ("destructive", "INTEGER", true, 0),
                    ("policy", "TEXT", true, 0),
                    ("state", "TEXT", true, 0),
                ],
            ),
            (
                "transfer_pass_folders",
                &[
                    ("run_id", "TEXT", true, 1),
                    ("attempt", "INTEGER", true, 2),
                    ("side", "INTEGER", true, 3),
                    ("folder_digest", "TEXT", true, 4),
                    ("uidvalidity", "INTEGER", true, 0),
                    ("uidnext", "INTEGER", true, 0),
                    ("exists_count", "INTEGER", true, 0),
                    ("verified_through_uid", "INTEGER", true, 0),
                    ("staged_messages", "INTEGER", true, 0),
                    ("complete", "INTEGER", true, 0),
                ],
            ),
        ];
        for (table, expected) in TABLES {
            let object_type: String = connection.query_row(
                "SELECT type FROM sqlite_master WHERE name=?1",
                [*table],
                |row| row.get(0),
            )?;
            if object_type != "table" {
                return Err(rusqlite::Error::InvalidQuery);
            }
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
            (
                "mailbox_jobs",
                &[
                    (
                        "projects",
                        "project_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                    (
                        "batch_plans",
                        "batch_plan_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                ],
            ),
            (
                "batch_plans",
                &[(
                    "projects",
                    "project_id",
                    "id",
                    "NO ACTION",
                    "NO ACTION",
                    "NONE",
                )],
            ),
            (
                "cutover_workflows",
                &[(
                    "projects",
                    "project_id",
                    "id",
                    "NO ACTION",
                    "NO ACTION",
                    "NONE",
                )],
            ),
            (
                "evidence",
                &[
                    (
                        "mailbox_jobs",
                        "job_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                    ("runs", "run_id", "id", "NO ACTION", "NO ACTION", "NONE"),
                ],
            ),
            (
                "evidence_history",
                &[
                    (
                        "mailbox_jobs",
                        "job_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                    ("runs", "run_id", "id", "NO ACTION", "NO ACTION", "NONE"),
                ],
            ),
            (
                "runs",
                &[
                    (
                        "projects",
                        "project_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                    (
                        "mailbox_jobs",
                        "job_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                    (
                        "runs",
                        "parent_run_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                ],
            ),
            (
                "active_processes",
                &[
                    ("runs", "run_id", "id", "NO ACTION", "NO ACTION", "NONE"),
                    (
                        "mailbox_jobs",
                        "job_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                ],
            ),
            (
                "events",
                &[
                    (
                        "projects",
                        "project_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                    ("runs", "run_id", "id", "NO ACTION", "NO ACTION", "NONE"),
                ],
            ),
            (
                "verification_acceptances",
                &[
                    (
                        "mailbox_jobs",
                        "job_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                    ("runs", "run_id", "id", "NO ACTION", "NO ACTION", "NONE"),
                ],
            ),
            (
                "engine_versions",
                &[("runs", "run_id", "id", "NO ACTION", "NO ACTION", "NONE")],
            ),
            (
                "message_mismatches",
                &[
                    (
                        "mailbox_jobs",
                        "job_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                    ("runs", "run_id", "id", "NO ACTION", "NO ACTION", "NONE"),
                ],
            ),
            (
                "transfer_passes",
                &[
                    ("runs", "run_id", "id", "NO ACTION", "NO ACTION", "NONE"),
                    (
                        "projects",
                        "project_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                ],
            ),
            (
                "mailbox_queue_facts",
                &[
                    (
                        "mailbox_jobs",
                        "job_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                    (
                        "projects",
                        "project_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                ],
            ),
            (
                "transfer_pass_folders",
                &[
                    (
                        "transfer_passes",
                        "run_id",
                        "run_id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                    (
                        "transfer_passes",
                        "attempt",
                        "attempt",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                ],
            ),
        ];
        // The facts table's state copy is kept current only by this trigger;
        // a ledger without it would present stale queue states.
        let trigger: Option<String> = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='mailbox_queue_facts_state' AND tbl_name='mailbox_jobs'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if !trigger.is_some_and(|sql| {
            let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
            sql.contains("AFTER UPDATE OF state ON mailbox_jobs")
                && sql.contains(
                    "UPDATE mailbox_queue_facts SET state=NEW.state WHERE job_rowid=NEW.rowid",
                )
        }) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let webhook_trigger: Option<String> = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='events_webhook_outbox' AND tbl_name='events'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if !webhook_trigger.is_some_and(|sql| {
            let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
            sql.contains("AFTER INSERT ON events")
                && sql.contains("INSERT OR IGNORE INTO webhook_deliveries")
        }) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        for (table, expected) in FOREIGN_KEYS {
            let actual = connection
                .prepare(&format!("PRAGMA foreign_key_list({table})"))?
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if actual.len() != expected.len()
                || !expected
                    .iter()
                    .all(|(parent, from, to, on_update, on_delete, match_type)| {
                        actual.iter().any(|(a, b, c, d, e, f)| {
                            a == parent
                                && b == from
                                && c == to
                                && d == on_update
                                && e == on_delete
                                && f == match_type
                        })
                    })
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        const INDEX_SIGNATURES: &[(&str, &[&str], bool, bool)] = &[
            (
                "idx_mailbox_jobs_project_state",
                &["project_id", "state"],
                false,
                false,
            ),
            (
                "idx_mailbox_jobs_project_rowid",
                &["project_id"],
                false,
                false,
            ),
            (
                "idx_runs_project_started",
                &["project_id", "started_at"],
                false,
                false,
            ),
            (
                "idx_runs_job_started",
                &["job_id", "started_at"],
                false,
                false,
            ),
            (
                "idx_events_project_created",
                &["project_id", "created_at"],
                false,
                false,
            ),
            (
                "idx_events_project_kind_id",
                &["project_id", "kind", "id"],
                false,
                false,
            ),
            (
                "idx_evidence_history_job_captured",
                &["job_id", "captured_at"],
                false,
                false,
            ),
            ("idx_active_processes_pid", &["pid"], false, false),
            (
                "idx_verification_acceptances_job",
                &["job_id", "id"],
                false,
                false,
            ),
            (
                "idx_engine_versions_captured",
                &["captured_at"],
                false,
                false,
            ),
            (
                "idx_message_mismatches_job_run",
                &["job_id", "run_id", "recorded_at"],
                false,
                false,
            ),
            (
                "idx_events_run_created",
                &["run_id", "created_at"],
                false,
                false,
            ),
            (
                "idx_transfer_passes_mailbox",
                &["project_id", "mailbox_digest", "pass_sequence"],
                false,
                false,
            ),
            (
                "idx_mailbox_queue_facts_project",
                &["project_id", "job_rowid"],
                false,
                false,
            ),
            (
                "idx_mailbox_jobs_project_queue",
                &["project_id", "id", "state"],
                false,
                false,
            ),
            ("one_running_run_per_job", &["job_id"], true, true),
            ("one_active_run_per_job", &["job_id"], true, true),
        ];
        const INDEX_TABLES: &[(&str, &str)] = &[
            ("idx_mailbox_jobs_project_state", "mailbox_jobs"),
            ("idx_mailbox_jobs_project_rowid", "mailbox_jobs"),
            ("idx_runs_project_started", "runs"),
            ("idx_runs_job_started", "runs"),
            ("idx_events_project_created", "events"),
            ("idx_events_project_kind_id", "events"),
            ("idx_evidence_history_job_captured", "evidence_history"),
            ("idx_active_processes_pid", "active_processes"),
            (
                "idx_verification_acceptances_job",
                "verification_acceptances",
            ),
            ("idx_engine_versions_captured", "engine_versions"),
            ("idx_message_mismatches_job_run", "message_mismatches"),
            ("idx_events_run_created", "events"),
            ("idx_transfer_passes_mailbox", "transfer_passes"),
            ("idx_mailbox_queue_facts_project", "mailbox_queue_facts"),
            ("idx_mailbox_jobs_project_queue", "mailbox_jobs"),
            ("one_running_run_per_job", "runs"),
            ("one_active_run_per_job", "runs"),
        ];
        for &(index, expected_columns, expected_unique, expected_partial) in INDEX_SIGNATURES {
            let expected_table = INDEX_TABLES
                .iter()
                .find(|(name, _)| *name == index)
                .map(|(_, table)| *table)
                .ok_or(rusqlite::Error::InvalidQuery)?;
            let table: String = connection.query_row(
                "SELECT tbl_name FROM sqlite_master WHERE type='index' AND name=?1",
                [index],
                |row| row.get(0),
            )?;
            let (unique, partial): (i64, i64) = connection.query_row(
                "SELECT \"unique\", partial FROM pragma_index_list(?1) WHERE name=?2",
                rusqlite::params![table, index],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let actual_columns = connection
                .prepare(&format!("PRAGMA index_info({index})"))?
                .query_map([], |row| row.get::<_, Option<String>>(2))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if table != expected_table
                || unique != expected_unique as i64
                || partial != expected_partial as i64
                || actual_columns.len() != expected_columns.len()
                || actual_columns
                    .iter()
                    .zip(expected_columns.iter())
                    .any(|(actual, expected)| actual.as_deref() != Some(*expected))
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        // `partial=1` alone is not a schema guarantee: a same-named unique
        // index with a weaker WHERE clause can admit multiple active runs.
        // These predicates enforce durable run ownership and are part of the
        // v12 schema signature just like the indexed columns above.
        const PARTIAL_INDEX_PREDICATES: &[(&str, &str)] = &[
            (
                "one_running_run_per_job",
                "wherejob_idisnotnullandstatus='running'",
            ),
            (
                "one_active_run_per_job",
                "wherejob_idisnotnullandstatusin('queued','running')",
            ),
        ];
        for (index, expected_predicate) in PARTIAL_INDEX_PREDICATES {
            let sql: String = connection.query_row(
                "SELECT sql FROM sqlite_master WHERE type='index' AND name=?1",
                [*index],
                |row| row.get(0),
            )?;
            let normalized = sql
                .to_ascii_lowercase()
                .chars()
                .filter(|character| {
                    !character.is_ascii_whitespace() && !matches!(character, '"' | '`' | '[' | ']')
                })
                .collect::<String>();
            if !normalized.ends_with(expected_predicate) {
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
        // Batch profiles are decoded into operator-facing jobs during restore.
        // Enforce both the per-row parser limit and an aggregate bound at the
        // ledger boundary so a crafted large queue cannot turn startup into an
        // unbounded allocation before admission has a chance to run.
        let profile_budget_exceeded: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE config IS NOT NULL AND length(CAST(config AS BLOB)) > ?1) OR EXISTS(SELECT 1 FROM batch_plans WHERE length(CAST(config AS BLOB)) > ?1) OR COALESCE((SELECT SUM(length(CAST(config AS BLOB))) FROM mailbox_jobs WHERE config IS NOT NULL), 0) + COALESCE((SELECT SUM(length(CAST(config AS BLOB))) FROM batch_plans), 0) > ?2",
            rusqlite::params![
                MAX_PERSISTED_PROFILE_BYTES as i64,
                MAX_TOTAL_PERSISTED_PROFILE_BYTES as i64
            ],
            |row| row.get(0),
        )?;
        if profile_budget_exceeded {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // Persisted wire values are part of the application schema too. Do
        // not let readers silently map an unknown value to a default enum
        // variant and present corrupted state as an ordinary ledger.
        const ENUM_CHECKS: &[&str] = &[
            "SELECT EXISTS(SELECT 1 FROM projects WHERE phase NOT IN ('discovery','preflight','pilot','seed','catch_up','final_delta','verification','complete','attention'))",
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE state NOT IN ('imported','queued','preflight','ready','running','completed','verified','verified_with_exceptions','failed','cancelled','attention','delta_required','verification_difference'))",
            "SELECT EXISTS(SELECT 1 FROM runs WHERE status NOT IN ('queued','running','completed','failed','cancelled','abandoned','verification_failed'))",
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE verification_method NOT IN ('aggregate_engine','metadata_reconciliation','body_hash','native_dovecot'))",
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE verification_outcome NOT IN ('exact_body_match','exact_metadata_match','probable_match','ambiguous','missing','changed','unexpected','incomplete','failed'))",
            "SELECT EXISTS(SELECT 1 FROM evidence_history WHERE verification_method NOT IN ('aggregate_engine','metadata_reconciliation','body_hash','native_dovecot'))",
            "SELECT EXISTS(SELECT 1 FROM evidence_history WHERE verification_outcome NOT IN ('exact_body_match','exact_metadata_match','probable_match','ambiguous','missing','changed','unexpected','incomplete','failed'))",
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE authoritative NOT IN (0,1))",
            "SELECT EXISTS(SELECT 1 FROM evidence_history WHERE authoritative NOT IN (0,1))",
            "SELECT EXISTS(SELECT 1 FROM message_mismatches WHERE mismatch_type NOT IN ('message_id_only','message_present_wrong_folder','missing','extra','duplicated'))",
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE attention_reason IS NOT NULL AND attention_reason NOT IN ('interrupted','verification_incomplete','verification_difference','process_identity_unverified','authentication_failed','transport_failed','policy_blocked','configuration_invalid','capacity_limited','message_rejected','unknown'))",
            "SELECT EXISTS(SELECT 1 FROM webhook_deliveries WHERE status NOT IN ('queued','delivered','dead_letter'))",
        ];
        for sql in ENUM_CHECKS {
            let has_unknown: bool = connection.query_row(sql, [], |row| row.get(0))?;
            if has_unknown {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        // The outcome is a claim about the counters, not an independent label
        // that may be edited without changing them. In particular, never let
        // a forged exact outcome survive read-only recovery validation.
        const SEMANTIC_CHECKS: &[&str] = &[
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE verification_outcome IN ('exact_body_match','exact_metadata_match') AND (source_messages<>destination_messages OR source_bytes<>destination_bytes OR source_folders<>destination_folders OR unmatched_messages IS NULL OR unmatched_messages<>0 OR failed_messages<>0 OR missing_messages<>0 OR extra_messages<>0 OR modified_messages<>0 OR probable_messages<>0 OR (verification_method='aggregate_engine' AND authoritative<>1) OR (verification_outcome='exact_body_match' AND verification_method<>'body_hash') OR (verification_method='body_hash' AND verification_outcome='exact_metadata_match')))",
            "SELECT EXISTS(SELECT 1 FROM evidence_history WHERE verification_outcome IN ('exact_body_match','exact_metadata_match') AND (source_messages<>destination_messages OR source_bytes<>destination_bytes OR source_folders<>destination_folders OR unmatched_messages IS NULL OR unmatched_messages<>0 OR failed_messages<>0 OR missing_messages<>0 OR extra_messages<>0 OR probable_messages<>0 OR (verification_method='aggregate_engine' AND authoritative<>1) OR (verification_outcome='exact_body_match' AND verification_method<>'body_hash') OR (verification_method='body_hash' AND verification_outcome='exact_metadata_match')))",
        ];
        for sql in SEMANTIC_CHECKS {
            let contradictory: bool = connection.query_row(sql, [], |row| row.get(0))?;
            if contradictory {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        const OWNERSHIP_CHECKS: &[&str] = &[
            "SELECT EXISTS(SELECT 1 FROM runs r JOIN mailbox_jobs j ON j.id=r.job_id WHERE r.job_id IS NOT NULL AND r.project_id<>j.project_id)",
            "SELECT EXISTS(SELECT 1 FROM evidence e JOIN runs r ON r.id=e.run_id WHERE r.job_id IS NULL OR r.job_id<>e.job_id)",
            "SELECT EXISTS(SELECT 1 FROM evidence_history h JOIN runs r ON r.id=h.run_id WHERE r.job_id IS NULL OR r.job_id<>h.job_id)",
            "SELECT EXISTS(SELECT 1 FROM message_mismatches m JOIN runs r ON r.id=m.run_id WHERE r.job_id IS NULL OR r.job_id<>m.job_id)",
            "SELECT EXISTS(SELECT 1 FROM active_processes p JOIN runs r ON r.id=p.run_id WHERE r.job_id IS NULL OR r.job_id<>p.job_id)",
            "SELECT EXISTS(SELECT 1 FROM verification_acceptances a JOIN runs r ON r.id=a.run_id WHERE r.job_id<>a.job_id)",
            "SELECT EXISTS(SELECT 1 FROM events e JOIN runs r ON r.id=e.run_id WHERE r.project_id<>e.project_id)",
        ];
        for sql in OWNERSHIP_CHECKS {
            let mismatched_owner: bool = connection.query_row(sql, [], |row| row.get(0))?;
            if mismatched_owner {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        // A project queue larger than the admission/readback limit cannot be
        // restored as an executable batch. Reject that inconsistent durable
        // state at open time instead of accepting it and failing later in UI
        // restore or batch retry.
        let oversized_mailbox_queue: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs GROUP BY project_id HAVING COUNT(*) > ?1)",
            [MAX_DURABLE_MAILBOX_ROWS as i64],
            |row| row.get(0),
        )?;
        if oversized_mailbox_queue {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // Foreign-key enforcement protects new writes, but SQLite does not
        // retroactively validate rows that were imported or edited while the
        // pragma was disabled. Recovery and read-only validation must reject
        // those orphaned records rather than presenting a partially connected
        // ledger as trustworthy evidence.
        let mut foreign_key_check = connection.prepare("PRAGMA foreign_key_check")?;
        if foreign_key_check.query([])?.next()?.is_some() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(())
    }

    fn validate_schema_constraints(connection: &Connection) -> rusqlite::Result<()> {
        const REQUIRED_CHECKS: &[(&str, &[&str])] = &[
            (
                "evidence",
                &[
                    "source_messages>=0",
                    "destination_messages>=0",
                    "source_bytes>=0",
                    "destination_bytes>=0",
                    "unmatched_messagesisnullorunmatched_messages>=0",
                    "failed_messages>=0",
                    "source_folders>=0",
                    "destination_folders>=0",
                    "authoritativein(0,1)",
                    "missing_messages>=0",
                    "extra_messages>=0",
                    "modified_messages>=0",
                    "probable_messages>=0",
                ],
            ),
            (
                "evidence_history",
                &[
                    "source_messages>=0",
                    "destination_messages>=0",
                    "source_bytes>=0",
                    "destination_bytes>=0",
                    "unmatched_messagesisnullorunmatched_messages>=0",
                    "failed_messages>=0",
                    "source_folders>=0",
                    "destination_folders>=0",
                    "authoritativein(0,1)",
                    "missing_messages>=0",
                    "extra_messages>=0",
                    "modified_messages>=0",
                    "probable_messages>=0",
                ],
            ),
            (
                "active_processes",
                &[
                    "pid>=0",
                    "start_ticksisnullorstart_ticks>=0",
                    "process_groupisnullorprocess_group>=0",
                    "session_idisnullorsession_id>=0",
                ],
            ),
            (
                "message_mismatches",
                &[
                    "source_size_bytesisnullorsource_size_bytes>=0",
                    "dest_size_bytesisnullordest_size_bytes>=0",
                    "source_uidvalidityisnullorsource_uidvalidity>=0",
                    "destination_uidvalidityisnullordestination_uidvalidity>=0",
                ],
            ),
            (
                "transfer_passes",
                &[
                    "attempt>0",
                    "pass_sequence>0",
                    "delta_requiredisnullordelta_requiredin(0,1)",
                ],
            ),
            ("mailbox_queue_facts", &["destructivein(0,1)"]),
            (
                "webhook_deliveries",
                &[
                    "attempts>=0",
                    "statusin('queued','delivered','dead_letter')",
                ],
            ),
            (
                "cutover_workflows",
                &["stagein('seed','catch_up','final_delta','verification','completed')"],
            ),
            (
                "transfer_pass_folders",
                &[
                    "sidein(0,1)",
                    "uidvalidity>=0",
                    "uidnext>=0",
                    "exists_count>=0",
                    "verified_through_uid>=0",
                    "staged_messages>=0",
                    "completein(0,1)",
                ],
            ),
        ];
        for (table, required_checks) in REQUIRED_CHECKS {
            let sql: String = connection.query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                [*table],
                |row| row.get(0),
            )?;
            let checks = sqlite_check_expressions(&sql);
            if required_checks
                .iter()
                .any(|required| !checks.iter().any(|check| check == required))
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        Ok(())
    }

    fn current_schema_is_clean(connection: &Connection) -> bool {
        if Self::validate_schema_layout(connection).is_err() {
            return false;
        }
        if Self::validate_schema_constraints(connection).is_err() {
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
            // Schema v13 establishes destination-identity policy v2 for every
            // existing row transactionally. Current-version startup trusts
            // that invariant rather than parsing every mailbox config again.
            let stored_schema_version: i64 =
                connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            let mut stale_identity = false;
            if stored_schema_version < DESTINATION_IDENTITY_SCHEMA_VERSION {
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
            Self::validate_schema_constraints(&store.connection)?;
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
        Self::validate_schema_constraints(&store.connection)?;
        Ok(store)
    }
    pub fn in_memory() -> rusqlite::Result<Self> {
        let store = Self {
            connection: Connection::open_in_memory()?,
        };
        store.migrate()?;
        Self::validate_schema_layout(&store.connection)?;
        Self::validate_schema_constraints(&store.connection)?;
        Ok(store)
    }

    /// Create a consistent SQLite backup without copying WAL/SHM files by
    /// hand. The destination must not already exist, preventing an operator
    /// typo from silently overwriting a prior recovery artifact.
    pub fn backup_to(&self, destination: &Path) -> rusqlite::Result<()> {
        Self::validate_schema_layout(&self.connection)?;
        Self::validate_schema_constraints(&self.connection)?;
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

        // A read-only open may need to repair a current-version ledger in an
        // in-memory copy (for example, duplicate active runs or missing
        // counter constraints). Snapshot the validated connection rather than
        // blindly copying the original file, otherwise restore would install
        // the unrepaired source after using the copy only as a validator.
        let repaired_source = if source_schema_version == CURRENT_SCHEMA_VERSION
            && Self::current_schema_is_clean(&source_connection)
        {
            None
        } else {
            Some(Self::open_readonly(source)?)
        };
        let backup_source = repaired_source
            .as_ref()
            .map_or(&source_connection, |store| &store.connection);

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
                let backup = backup::Backup::new(backup_source, &mut destination_connection)?;
                backup.run_to_completion(128, std::time::Duration::from_millis(1), None)?;
            }
            let integrity: String =
                destination_connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            if integrity != "ok" {
                return Err(rusqlite::Error::InvalidQuery);
            }
            Self::validate_schema_layout(&destination_connection)?;
            Self::validate_schema_constraints(&destination_connection)?;
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
        // version 6 records observed engine-version metadata per run; version
        // 13 establishes destination identity policy v2; version 14 binds the
        // current evidence projection directly to its run for report reads;
        // version 15 adds durable per-attempt transfer-pass provenance;
        // version 16 adds narrow per-mailbox queue presentation facts; version
        // 17 stores shared batch policies separately from mailbox deltas; 18
        // adds the credential-free webhook delivery outbox; 19 adds the
        // transaction-bound lifecycle-event outbox trigger; 20 distinguishes
        // mailbox failure events from successful completion events; version
        // 22 distinguishes failed mailbox preflights in the webhook outbox.
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
            let current_schema_is_clean = Self::current_schema_is_clean(&self.connection)
                && Self::validate_schema_layout(&self.connection).is_ok();
            if current_schema_is_clean {
                // A clean current ledger needs no launch-time data rewrite. The
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
                 CREATE TABLE IF NOT EXISTS cutover_workflows (project_id TEXT PRIMARY KEY REFERENCES projects(id), stage TEXT NOT NULL CHECK(stage IN ('seed','catch_up','final_delta','verification','completed')), scheduled_at TEXT NOT NULL, maintenance_window TEXT, approved_by TEXT, approved_at TEXT, external_confirmation TEXT);
                 CREATE TABLE IF NOT EXISTS batch_plans (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), config TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS mailbox_jobs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), source_mailbox TEXT NOT NULL, destination_mailbox TEXT NOT NULL, destination_identity TEXT NOT NULL DEFAULT '', state TEXT NOT NULL, attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0), checkpoint TEXT, preflight_plan TEXT, config TEXT, attention_reason TEXT, batch_plan_id TEXT REFERENCES batch_plans(id), row_overrides TEXT);
                 CREATE TABLE IF NOT EXISTS evidence (job_id TEXT PRIMARY KEY REFERENCES mailbox_jobs(id), verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL CHECK(source_messages >= 0), destination_messages INTEGER NOT NULL CHECK(destination_messages >= 0), source_bytes INTEGER NOT NULL CHECK(source_bytes >= 0), destination_bytes INTEGER NOT NULL CHECK(destination_bytes >= 0), unmatched_messages INTEGER CHECK(unmatched_messages IS NULL OR unmatched_messages >= 0), failed_messages INTEGER NOT NULL CHECK(failed_messages >= 0), source_folders INTEGER NOT NULL DEFAULT 0 CHECK(source_folders >= 0), destination_folders INTEGER NOT NULL DEFAULT 0 CHECK(destination_folders >= 0), authoritative INTEGER NOT NULL DEFAULT 0 CHECK(authoritative IN (0,1)), missing_messages INTEGER NOT NULL DEFAULT 0 CHECK(missing_messages >= 0), extra_messages INTEGER NOT NULL DEFAULT 0 CHECK(extra_messages >= 0), modified_messages INTEGER NOT NULL DEFAULT 0 CHECK(modified_messages >= 0), probable_messages INTEGER NOT NULL DEFAULT 0 CHECK(probable_messages >= 0), captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS evidence_history (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL CHECK(source_messages >= 0), destination_messages INTEGER NOT NULL CHECK(destination_messages >= 0), source_bytes INTEGER NOT NULL CHECK(source_bytes >= 0), destination_bytes INTEGER NOT NULL CHECK(destination_bytes >= 0), unmatched_messages INTEGER CHECK(unmatched_messages IS NULL OR unmatched_messages >= 0), failed_messages INTEGER NOT NULL CHECK(failed_messages >= 0), source_folders INTEGER NOT NULL CHECK(source_folders >= 0), destination_folders INTEGER NOT NULL CHECK(destination_folders >= 0), authoritative INTEGER NOT NULL DEFAULT 0 CHECK(authoritative IN (0,1)), missing_messages INTEGER NOT NULL DEFAULT 0 CHECK(missing_messages >= 0), extra_messages INTEGER NOT NULL DEFAULT 0 CHECK(extra_messages >= 0), modified_messages INTEGER NOT NULL DEFAULT 0 CHECK(modified_messages >= 0), probable_messages INTEGER NOT NULL DEFAULT 0 CHECK(probable_messages >= 0), captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), job_id TEXT REFERENCES mailbox_jobs(id), parent_run_id TEXT REFERENCES runs(id), engine TEXT NOT NULL, phase_at_start TEXT NOT NULL DEFAULT 'legacy_unknown', plan_snapshot TEXT NOT NULL DEFAULT '', status TEXT NOT NULL, started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, finished_at TEXT, detail TEXT NOT NULL DEFAULT '');
                 CREATE TABLE IF NOT EXISTS active_processes (run_id TEXT NOT NULL REFERENCES runs(id), job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), pid INTEGER NOT NULL CHECK(pid >= 0), start_ticks INTEGER CHECK(start_ticks IS NULL OR start_ticks >= 0), process_group INTEGER CHECK(process_group IS NULL OR process_group >= 0), session_id INTEGER CHECK(session_id IS NULL OR session_id >= 0), executable TEXT NOT NULL DEFAULT '', started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(run_id, job_id));
                 CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), run_id TEXT REFERENCES runs(id), kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS webhook_deliveries (event_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, event_type TEXT NOT NULL, payload TEXT NOT NULL, endpoint_digest TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0), status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','delivered','dead_letter')), last_error TEXT, next_attempt_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, delivered_at TEXT);
                 CREATE TRIGGER IF NOT EXISTS events_webhook_outbox AFTER INSERT ON events WHEN NEW.kind IN ('run_started','run_finished','verification_exception_accepted') OR (NEW.kind='phase_changed' AND NEW.detail IN ('complete','final_delta')) BEGIN INSERT OR IGNORE INTO webhook_deliveries(event_id,project_id,event_type,payload,endpoint_digest) SELECT printf('ledger-event-%lld',NEW.id),NEW.project_id,CASE WHEN NEW.kind='run_started' THEN 'migration.started' WHEN NEW.kind='run_finished' AND EXISTS(SELECT 1 FROM mailbox_jobs j JOIN runs r ON r.id=NEW.run_id WHERE j.id=r.job_id AND j.state='verification_difference') THEN 'mailbox.verification_difference' WHEN NEW.kind='run_finished' THEN 'mailbox.completed' WHEN NEW.kind='phase_changed' AND NEW.detail='complete' THEN 'migration.completed' WHEN NEW.kind='phase_changed' AND NEW.detail='final_delta' THEN 'migration.cutover_ready' ELSE 'mailbox.verification_accepted' END,json_object('format','mailswiftsync-webhook-event','event_id',printf('ledger-event-%lld',NEW.id),'event_type',CASE WHEN NEW.kind='run_started' THEN 'migration.started' WHEN NEW.kind='run_finished' AND EXISTS(SELECT 1 FROM mailbox_jobs j JOIN runs r ON r.id=NEW.run_id WHERE j.id=r.job_id AND j.state='verification_difference') THEN 'mailbox.verification_difference' WHEN NEW.kind='run_finished' THEN 'mailbox.completed' WHEN NEW.kind='phase_changed' AND NEW.detail='complete' THEN 'migration.completed' WHEN NEW.kind='phase_changed' AND NEW.detail='final_delta' THEN 'migration.cutover_ready' ELSE 'mailbox.verification_accepted' END,'project_id',NEW.project_id,'run_id',NEW.run_id,'detail',NEW.detail),''; END;
                 CREATE TABLE IF NOT EXISTS verification_acceptances (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), operator TEXT NOT NULL, reason TEXT NOT NULL, accepted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS engine_versions (run_id TEXT PRIMARY KEY REFERENCES runs(id), version TEXT NOT NULL, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS message_mismatches (id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), mismatch_type TEXT NOT NULL, source_uid TEXT, dest_uid TEXT, source_message_id TEXT, dest_message_id TEXT, source_size_bytes INTEGER CHECK(source_size_bytes IS NULL OR source_size_bytes >= 0), dest_size_bytes INTEGER CHECK(dest_size_bytes IS NULL OR dest_size_bytes >= 0), source_date TEXT, dest_date TEXT, source_folder TEXT, destination_folder TEXT, source_uidvalidity INTEGER CHECK(source_uidvalidity IS NULL OR source_uidvalidity >= 0), destination_uidvalidity INTEGER CHECK(destination_uidvalidity IS NULL OR destination_uidvalidity >= 0), source_fingerprint TEXT, destination_fingerprint TEXT, recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 CREATE INDEX IF NOT EXISTS idx_mailbox_jobs_project_state ON mailbox_jobs(project_id, state);
                 CREATE INDEX IF NOT EXISTS idx_mailbox_jobs_project_rowid ON mailbox_jobs(project_id);
                 CREATE INDEX IF NOT EXISTS idx_runs_project_started ON runs(project_id, started_at DESC);
                 CREATE INDEX IF NOT EXISTS idx_runs_job_started ON runs(job_id, started_at DESC);
                 CREATE TABLE IF NOT EXISTS transfer_passes (run_id TEXT NOT NULL REFERENCES runs(id), attempt INTEGER NOT NULL CHECK(attempt > 0), project_id TEXT NOT NULL REFERENCES projects(id), mailbox_digest TEXT NOT NULL, pass_sequence INTEGER NOT NULL CHECK(pass_sequence > 0), pass_kind TEXT NOT NULL, engine TEXT NOT NULL, executable_identity TEXT NOT NULL, command_sha256 TEXT NOT NULL, command TEXT NOT NULL, folder_scope TEXT NOT NULL, source_range TEXT NOT NULL, started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, finished_at TEXT, outcome TEXT, delta_required INTEGER CHECK(delta_required IS NULL OR delta_required IN (0,1)), completion_evidence TEXT, emitted_state_sha256 TEXT, verification_method TEXT, verification_outcome TEXT, verified_at TEXT, PRIMARY KEY(run_id, attempt));
                 CREATE INDEX IF NOT EXISTS idx_transfer_passes_mailbox ON transfer_passes(project_id, mailbox_digest, pass_sequence);
                 CREATE TABLE IF NOT EXISTS mailbox_queue_facts (job_rowid INTEGER PRIMARY KEY, job_id TEXT NOT NULL UNIQUE REFERENCES mailbox_jobs(id), project_id TEXT NOT NULL REFERENCES projects(id), label TEXT NOT NULL, source_host TEXT NOT NULL, destination_host TEXT NOT NULL, search_key TEXT NOT NULL, destructive INTEGER NOT NULL CHECK(destructive IN (0,1)), policy TEXT NOT NULL, state TEXT NOT NULL);
                 CREATE TRIGGER IF NOT EXISTS mailbox_queue_facts_state AFTER UPDATE OF state ON mailbox_jobs BEGIN UPDATE mailbox_queue_facts SET state=NEW.state WHERE job_rowid=NEW.rowid; END;
                 CREATE INDEX IF NOT EXISTS idx_mailbox_queue_facts_project ON mailbox_queue_facts(project_id, job_rowid);
                 CREATE INDEX IF NOT EXISTS idx_mailbox_jobs_project_queue ON mailbox_jobs(project_id, id, state);
                 CREATE INDEX IF NOT EXISTS idx_batch_plans_project ON batch_plans(project_id);
                 CREATE TABLE IF NOT EXISTS transfer_pass_folders (run_id TEXT NOT NULL, attempt INTEGER NOT NULL, side INTEGER NOT NULL CHECK(side IN (0,1)), folder_digest TEXT NOT NULL, uidvalidity INTEGER NOT NULL CHECK(uidvalidity >= 0), uidnext INTEGER NOT NULL CHECK(uidnext >= 0), exists_count INTEGER NOT NULL CHECK(exists_count >= 0), verified_through_uid INTEGER NOT NULL CHECK(verified_through_uid >= 0), staged_messages INTEGER NOT NULL CHECK(staged_messages >= 0), complete INTEGER NOT NULL CHECK(complete IN (0,1)), PRIMARY KEY(run_id, attempt, side, folder_digest), FOREIGN KEY(run_id, attempt) REFERENCES transfer_passes(run_id, attempt));
                 CREATE INDEX IF NOT EXISTS idx_events_project_created ON events(project_id, created_at DESC);
                 CREATE INDEX IF NOT EXISTS idx_events_project_kind_id ON events(project_id, kind, id DESC);
                 CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_due ON webhook_deliveries(endpoint_digest, status, next_attempt_at, created_at);
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
        // Version 20 replaces the version-19 trigger.  A failed or cancelled
        // mailbox must never be published as completed, and a project-level
        // run must not be mislabeled as a mailbox event.
        tx.execute_batch(
            "DROP TRIGGER IF EXISTS events_webhook_outbox;
             CREATE TRIGGER events_webhook_outbox AFTER INSERT ON events
             WHEN NEW.kind IN ('run_started','run_finished','verification_exception_accepted')
                 OR (NEW.kind='phase_changed' AND NEW.detail IN ('complete','final_delta'))
             BEGIN
                 INSERT OR IGNORE INTO webhook_deliveries(event_id,project_id,event_type,payload,endpoint_digest)
                 SELECT printf('ledger-event-%lld',NEW.id),NEW.project_id,
                     CASE
                         WHEN NEW.kind='run_started' THEN 'migration.started'
                         WHEN NEW.kind='run_finished' AND EXISTS(SELECT 1 FROM runs r WHERE r.id=NEW.run_id AND r.job_id IS NOT NULL AND r.phase_at_start='preflight' AND r.status='failed') THEN 'mailbox.preflight_failed'
                         WHEN NEW.kind='run_finished' AND EXISTS(SELECT 1 FROM mailbox_jobs j JOIN runs r ON r.id=NEW.run_id WHERE j.id=r.job_id AND j.state='verification_difference') THEN 'mailbox.verification_difference'
                         WHEN NEW.kind='run_finished' AND EXISTS(SELECT 1 FROM mailbox_jobs j JOIN runs r ON r.id=NEW.run_id WHERE j.id=r.job_id AND j.state IN ('failed','cancelled','attention')) THEN 'mailbox.failed'
                         WHEN NEW.kind='run_finished' AND EXISTS(SELECT 1 FROM runs r WHERE r.id=NEW.run_id AND r.job_id IS NOT NULL) THEN 'mailbox.completed'
                         WHEN NEW.kind='run_finished' THEN 'migration.completed'
                         WHEN NEW.kind='phase_changed' AND NEW.detail='complete' THEN 'migration.completed'
                         WHEN NEW.kind='phase_changed' AND NEW.detail='final_delta' THEN 'migration.cutover_ready'
                         ELSE 'mailbox.verification_accepted'
                     END,
                     json_object('format','mailswiftsync-webhook-event','event_id',printf('ledger-event-%lld',NEW.id),'event_type',
                         CASE
                             WHEN NEW.kind='run_started' THEN 'migration.started'
                             WHEN NEW.kind='run_finished' AND EXISTS(SELECT 1 FROM runs r WHERE r.id=NEW.run_id AND r.job_id IS NOT NULL AND r.phase_at_start='preflight' AND r.status='failed') THEN 'mailbox.preflight_failed'
                             WHEN NEW.kind='run_finished' AND EXISTS(SELECT 1 FROM mailbox_jobs j JOIN runs r ON r.id=NEW.run_id WHERE j.id=r.job_id AND j.state='verification_difference') THEN 'mailbox.verification_difference'
                             WHEN NEW.kind='run_finished' AND EXISTS(SELECT 1 FROM mailbox_jobs j JOIN runs r ON r.id=NEW.run_id WHERE j.id=r.job_id AND j.state IN ('failed','cancelled','attention')) THEN 'mailbox.failed'
                             WHEN NEW.kind='run_finished' AND EXISTS(SELECT 1 FROM runs r WHERE r.id=NEW.run_id AND r.job_id IS NOT NULL) THEN 'mailbox.completed'
                             WHEN NEW.kind='run_finished' THEN 'migration.completed'
                             WHEN NEW.kind='phase_changed' AND NEW.detail='complete' THEN 'migration.completed'
                             WHEN NEW.kind='phase_changed' AND NEW.detail='final_delta' THEN 'migration.cutover_ready'
                             ELSE 'mailbox.verification_accepted'
                         END,'project_id',NEW.project_id,'run_id',NEW.run_id,'detail',NEW.detail),'';
             END;",
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
        if !job_columns.iter().any(|column| column == "batch_plan_id") {
            tx.execute(
                "ALTER TABLE mailbox_jobs ADD COLUMN batch_plan_id TEXT REFERENCES batch_plans(id)",
                [],
            )?;
        }
        if !job_columns.iter().any(|column| column == "row_overrides") {
            tx.execute("ALTER TABLE mailbox_jobs ADD COLUMN row_overrides TEXT", [])?;
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
                 CREATE TABLE evidence_history (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL CHECK(source_messages >= 0), destination_messages INTEGER NOT NULL CHECK(destination_messages >= 0), source_bytes INTEGER NOT NULL CHECK(source_bytes >= 0), destination_bytes INTEGER NOT NULL CHECK(destination_bytes >= 0), unmatched_messages INTEGER CHECK(unmatched_messages IS NULL OR unmatched_messages >= 0), failed_messages INTEGER NOT NULL CHECK(failed_messages >= 0), source_folders INTEGER NOT NULL CHECK(source_folders >= 0), destination_folders INTEGER NOT NULL CHECK(destination_folders >= 0), authoritative INTEGER NOT NULL DEFAULT 0 CHECK(authoritative IN (0,1)), missing_messages INTEGER NOT NULL DEFAULT 0 CHECK(missing_messages >= 0), extra_messages INTEGER NOT NULL DEFAULT 0 CHECK(extra_messages >= 0), modified_messages INTEGER NOT NULL DEFAULT 0 CHECK(modified_messages >= 0), probable_messages INTEGER NOT NULL DEFAULT 0 CHECK(probable_messages >= 0), captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 INSERT INTO evidence_history SELECT id,job_id,run_id,'aggregate_engine','incomplete',source_messages,destination_messages,source_bytes,destination_bytes,CASE WHEN authoritative=0 AND unmatched_messages=1 THEN NULL ELSE unmatched_messages END,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,0,captured_at FROM evidence_history_legacy;
                 DROP TABLE evidence_history_legacy;",
            )?;
        }
        Self::ensure_evidence_counter_constraints(&tx)?;
        Self::ensure_evidence_history_run_foreign_key(&tx)?;

        // Schema v14 turns the one-row-per-mailbox evidence projection into
        // the fast-path source for current reports. Preserve the immutable
        // evidence_history ledger and bind this projection to its owning run.
        let evidence_columns = tx
            .prepare("PRAGMA table_info(evidence)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !evidence_columns.iter().any(|column| column == "run_id") {
            tx.execute(
                "ALTER TABLE evidence ADD COLUMN run_id TEXT REFERENCES runs(id)",
                [],
            )?;
        }
        tx.execute(
            "UPDATE evidence SET run_id=(SELECT latest.run_id FROM evidence_history latest WHERE latest.job_id=evidence.job_id ORDER BY latest.captured_at DESC,latest.id DESC LIMIT 1) WHERE run_id IS NULL",
            [],
        )?;

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
        Self::ensure_runtime_numeric_constraints(&tx)?;
        // Older alpha versions did not enforce one active run per mailbox.
        // Reconcile those ledgers before creating the partial unique indexes;
        // otherwise an otherwise recoverable database would fail to open.
        Self::reconcile_duplicate_active_runs(&tx)?;
        Self::refresh_destination_identities(&tx)?;
        Self::purge_raw_output_events(&tx)?;
        tx.execute_batch(
                "DROP INDEX IF EXISTS one_running_run_per_job;
                 DROP INDEX IF EXISTS one_active_run_per_job;
                 CREATE UNIQUE INDEX one_running_run_per_job ON runs(job_id) WHERE job_id IS NOT NULL AND status='running';
                 CREATE UNIQUE INDEX one_active_run_per_job ON runs(job_id) WHERE job_id IS NOT NULL AND status IN ('queued','running');",
            )?;
        tx.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;
        tx.commit()?;
        Ok(())
    }

    fn ensure_evidence_counter_constraints(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
        let has_constraints = |table: &str| -> rusqlite::Result<bool> {
            let sql: String = tx.query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )?;
            let checks = sqlite_check_expressions(&sql);
            let required = [
                "source_messages>=0",
                "destination_messages>=0",
                "source_bytes>=0",
                "destination_bytes>=0",
                "unmatched_messagesisnullorunmatched_messages>=0",
                "failed_messages>=0",
                "source_folders>=0",
                "destination_folders>=0",
                "authoritativein(0,1)",
                "missing_messages>=0",
                "extra_messages>=0",
                "modified_messages>=0",
                "probable_messages>=0",
            ];
            Ok(required
                .iter()
                .all(|required| checks.iter().any(|check| check == required)))
        };
        if has_constraints("evidence")? && has_constraints("evidence_history")? {
            return Ok(());
        }
        tx.execute_batch(
            "DROP INDEX IF EXISTS idx_evidence_history_job_captured;
             ALTER TABLE evidence RENAME TO evidence_unconstrained;
             CREATE TABLE evidence (job_id TEXT PRIMARY KEY REFERENCES mailbox_jobs(id), verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL CHECK(source_messages >= 0), destination_messages INTEGER NOT NULL CHECK(destination_messages >= 0), source_bytes INTEGER NOT NULL CHECK(source_bytes >= 0), destination_bytes INTEGER NOT NULL CHECK(destination_bytes >= 0), unmatched_messages INTEGER CHECK(unmatched_messages IS NULL OR unmatched_messages >= 0), failed_messages INTEGER NOT NULL CHECK(failed_messages >= 0), source_folders INTEGER NOT NULL DEFAULT 0 CHECK(source_folders >= 0), destination_folders INTEGER NOT NULL DEFAULT 0 CHECK(destination_folders >= 0), authoritative INTEGER NOT NULL DEFAULT 0 CHECK(authoritative IN (0,1)), missing_messages INTEGER NOT NULL DEFAULT 0 CHECK(missing_messages >= 0), extra_messages INTEGER NOT NULL DEFAULT 0 CHECK(extra_messages >= 0), modified_messages INTEGER NOT NULL DEFAULT 0 CHECK(modified_messages >= 0), probable_messages INTEGER NOT NULL DEFAULT 0 CHECK(probable_messages >= 0), captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             INSERT INTO evidence SELECT job_id,verification_method,verification_outcome,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,probable_messages,captured_at FROM evidence_unconstrained;
             DROP TABLE evidence_unconstrained;
             ALTER TABLE evidence_history RENAME TO evidence_history_unconstrained;
             CREATE TABLE evidence_history (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL CHECK(source_messages >= 0), destination_messages INTEGER NOT NULL CHECK(destination_messages >= 0), source_bytes INTEGER NOT NULL CHECK(source_bytes >= 0), destination_bytes INTEGER NOT NULL CHECK(destination_bytes >= 0), unmatched_messages INTEGER CHECK(unmatched_messages IS NULL OR unmatched_messages >= 0), failed_messages INTEGER NOT NULL CHECK(failed_messages >= 0), source_folders INTEGER NOT NULL CHECK(source_folders >= 0), destination_folders INTEGER NOT NULL CHECK(destination_folders >= 0), authoritative INTEGER NOT NULL DEFAULT 0 CHECK(authoritative IN (0,1)), missing_messages INTEGER NOT NULL DEFAULT 0 CHECK(missing_messages >= 0), extra_messages INTEGER NOT NULL DEFAULT 0 CHECK(extra_messages >= 0), modified_messages INTEGER NOT NULL DEFAULT 0 CHECK(modified_messages >= 0), probable_messages INTEGER NOT NULL DEFAULT 0 CHECK(probable_messages >= 0), captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             INSERT INTO evidence_history SELECT id,job_id,run_id,verification_method,verification_outcome,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,probable_messages,captured_at FROM evidence_history_unconstrained;
             DROP TABLE evidence_history_unconstrained;
             CREATE INDEX IF NOT EXISTS idx_evidence_history_job_captured ON evidence_history(job_id, captured_at DESC);",
        )?;
        Ok(())
    }

    fn ensure_evidence_history_run_foreign_key(
        tx: &rusqlite::Transaction<'_>,
    ) -> rusqlite::Result<()> {
        let has_run_foreign_key: bool = tx
            .prepare("PRAGMA foreign_key_list(evidence_history)")?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .any(|(table, from, to, on_update, on_delete, match_type)| {
                table == "runs"
                    && from == "run_id"
                    && to == "id"
                    && on_update == "NO ACTION"
                    && on_delete == "NO ACTION"
                    && match_type == "NONE"
            });
        if has_run_foreign_key {
            return Ok(());
        }
        tx.execute_batch(
            "DROP INDEX IF EXISTS idx_evidence_history_job_captured;
             ALTER TABLE evidence_history RENAME TO evidence_history_legacy;
             CREATE TABLE evidence_history (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), verification_method TEXT NOT NULL DEFAULT 'aggregate_engine', verification_outcome TEXT NOT NULL DEFAULT 'incomplete', source_messages INTEGER NOT NULL CHECK(source_messages >= 0), destination_messages INTEGER NOT NULL CHECK(destination_messages >= 0), source_bytes INTEGER NOT NULL CHECK(source_bytes >= 0), destination_bytes INTEGER NOT NULL CHECK(destination_bytes >= 0), unmatched_messages INTEGER CHECK(unmatched_messages IS NULL OR unmatched_messages >= 0), failed_messages INTEGER NOT NULL CHECK(failed_messages >= 0), source_folders INTEGER NOT NULL CHECK(source_folders >= 0), destination_folders INTEGER NOT NULL CHECK(destination_folders >= 0), authoritative INTEGER NOT NULL DEFAULT 0 CHECK(authoritative IN (0,1)), missing_messages INTEGER NOT NULL DEFAULT 0 CHECK(missing_messages >= 0), extra_messages INTEGER NOT NULL DEFAULT 0 CHECK(extra_messages >= 0), modified_messages INTEGER NOT NULL DEFAULT 0 CHECK(modified_messages >= 0), probable_messages INTEGER NOT NULL DEFAULT 0 CHECK(probable_messages >= 0), captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             INSERT INTO evidence_history SELECT id,job_id,run_id,verification_method,verification_outcome,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,probable_messages,captured_at FROM evidence_history_legacy;
             DROP TABLE evidence_history_legacy;
             CREATE INDEX idx_evidence_history_job_captured ON evidence_history(job_id, captured_at DESC);",
        )
    }

    fn ensure_runtime_numeric_constraints(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
        let has_checks = |table: &str, required: &[&str]| -> rusqlite::Result<bool> {
            let sql: String = tx.query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )?;
            let checks = sqlite_check_expressions(&sql);
            Ok(required
                .iter()
                .all(|required| checks.iter().any(|check| check == required)))
        };
        let process_checks = [
            "pid>=0",
            "start_ticksisnullorstart_ticks>=0",
            "process_groupisnullorprocess_group>=0",
            "session_idisnullorsession_id>=0",
        ];
        if !has_checks("active_processes", &process_checks)? {
            tx.execute_batch(
                "ALTER TABLE active_processes RENAME TO active_processes_legacy;
                 CREATE TABLE active_processes (run_id TEXT NOT NULL REFERENCES runs(id), job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), pid INTEGER NOT NULL CHECK(pid >= 0), start_ticks INTEGER CHECK(start_ticks IS NULL OR start_ticks >= 0), process_group INTEGER CHECK(process_group IS NULL OR process_group >= 0), session_id INTEGER CHECK(session_id IS NULL OR session_id >= 0), executable TEXT NOT NULL DEFAULT '', started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(run_id, job_id));
                 INSERT INTO active_processes(run_id,job_id,pid,start_ticks,process_group,session_id,executable,started_at) SELECT run_id,job_id,pid,start_ticks,process_group,session_id,executable,started_at FROM active_processes_legacy;
                 DROP TABLE active_processes_legacy;
                 CREATE INDEX IF NOT EXISTS idx_active_processes_pid ON active_processes(pid);",
            )?;
        }
        let mismatch_checks = [
            "source_size_bytesisnullorsource_size_bytes>=0",
            "dest_size_bytesisnullordest_size_bytes>=0",
            "source_uidvalidityisnullorsource_uidvalidity>=0",
            "destination_uidvalidityisnullordestination_uidvalidity>=0",
        ];
        if !has_checks("message_mismatches", &mismatch_checks)? {
            tx.execute_batch(
                "DROP INDEX IF EXISTS idx_message_mismatches_job_run;
                 ALTER TABLE message_mismatches RENAME TO message_mismatches_legacy;
                 CREATE TABLE message_mismatches (id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL REFERENCES runs(id), mismatch_type TEXT NOT NULL, source_uid TEXT, dest_uid TEXT, source_message_id TEXT, dest_message_id TEXT, source_size_bytes INTEGER CHECK(source_size_bytes IS NULL OR source_size_bytes >= 0), dest_size_bytes INTEGER CHECK(dest_size_bytes IS NULL OR dest_size_bytes >= 0), source_date TEXT, dest_date TEXT, source_folder TEXT, destination_folder TEXT, source_uidvalidity INTEGER CHECK(source_uidvalidity IS NULL OR source_uidvalidity >= 0), destination_uidvalidity INTEGER CHECK(destination_uidvalidity IS NULL OR destination_uidvalidity >= 0), source_fingerprint TEXT, destination_fingerprint TEXT, recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                 INSERT INTO message_mismatches(id,job_id,run_id,mismatch_type,source_uid,dest_uid,source_message_id,dest_message_id,source_size_bytes,dest_size_bytes,source_date,dest_date,source_folder,destination_folder,source_uidvalidity,destination_uidvalidity,source_fingerprint,destination_fingerprint,recorded_at) SELECT id,job_id,run_id,mismatch_type,source_uid,dest_uid,source_message_id,dest_message_id,source_size_bytes,dest_size_bytes,source_date,dest_date,source_folder,destination_folder,source_uidvalidity,destination_uidvalidity,source_fingerprint,destination_fingerprint,recorded_at FROM message_mismatches_legacy;
                 DROP TABLE message_mismatches_legacy;
                 CREATE INDEX IF NOT EXISTS idx_message_mismatches_job_run ON message_mismatches(job_id, run_id, recorded_at);",
            )?;
        }
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
            ("batch_plan_id", "TEXT"),
            ("row_overrides", "TEXT"),
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

#[cfg(test)]
mod schema_check_expression_tests {
    use super::sqlite_check_expressions;

    #[test]
    fn extracts_real_nested_checks_but_ignores_literals_and_comments() {
        let ddl = r#"CREATE TABLE evidence (
            note TEXT DEFAULT 'CHECK(source_messages >= 0)',
            other TEXT DEFAULT "CHECK(destination_messages >= 0)",
            /* CHECK(source_bytes >= 0) */
            source_messages INTEGER CHECK (source_messages >= 0),
            CHECK(unmatched_messages IS NULL OR unmatched_messages >= 0),
            CHECK(length(')') > 0)
        )"#;
        let checks = sqlite_check_expressions(ddl);
        assert!(checks.contains(&"source_messages>=0".to_owned()));
        assert!(checks.contains(&"unmatched_messagesisnullorunmatched_messages>=0".to_owned()));
        assert!(checks.contains(&"length(')')>0".to_owned()));
        assert!(!checks.contains(&"destination_messages>=0".to_owned()));
        assert!(!checks.contains(&"source_bytes>=0".to_owned()));
    }

    #[test]
    fn ignores_check_text_in_identifier_names_and_line_comments() {
        let ddl =
            "CREATE TABLE t ([CHECK(source_messages >= 0)] TEXT) -- CHECK(source_messages >= 0)\n";
        assert!(sqlite_check_expressions(ddl).is_empty());
    }
}

#[cfg(test)]
mod database_identity_tests {
    use super::{database_identity, verify_database_identity};

    #[test]
    fn replaced_database_path_is_detected() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("state.db");
        let replacement = directory.join("replacement.db");
        std::fs::write(&path, b"original").unwrap();
        std::fs::write(&replacement, b"replacement").unwrap();
        let identity = database_identity(&path).unwrap();
        verify_database_identity(&path, identity).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let error = verify_database_identity(&path, identity).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
