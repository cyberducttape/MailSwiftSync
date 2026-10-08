use super::*;

impl StateStore {
    pub(super) fn validate_schema_layout(connection: &Connection) -> rusqlite::Result<()> {
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
                "waves",
                &[
                    ("id", "TEXT", false, 1),
                    ("project_id", "TEXT", true, 0),
                    ("name", "TEXT", true, 0),
                    ("position", "INTEGER", true, 0),
                    ("scheduled_at", "TEXT", false, 0),
                    ("maintenance_window", "TEXT", false, 0),
                    ("concurrency", "INTEGER", false, 0),
                    ("approved_by", "TEXT", false, 0),
                    ("approved_at", "TEXT", false, 0),
                    ("created_at", "TEXT", true, 0),
                ],
            ),
            (
                "wave_members",
                &[("job_id", "TEXT", false, 1), ("wave_id", "TEXT", true, 0)],
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
                "evidence_flag_verification",
                &[
                    ("job_id", "TEXT", true, 1),
                    ("run_id", "TEXT", true, 2),
                    ("compared_messages", "INTEGER", true, 0),
                    ("mismatched_messages", "INTEGER", true, 0),
                    ("excepted_messages", "INTEGER", true, 0),
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
                    ("lease_owner", "TEXT", false, 0),
                    ("lease_until", "TEXT", false, 0),
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
                    ("source_folder_digest", "TEXT", false, 0),
                    ("destination_folder_digest", "TEXT", false, 0),
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
        let queue_search_sql: Option<String> = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='mailbox_queue_facts_search'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let queue_search_columns = connection
            .prepare("PRAGMA table_info(mailbox_queue_facts_search)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !queue_search_sql.is_some_and(|sql| {
            let sql = sql
                .to_ascii_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            sql.contains("using fts5") && sql.contains("tokenize='trigram'")
        }) || queue_search_columns != ["search_key"]
        {
            return Err(rusqlite::Error::InvalidQuery);
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
                "waves",
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
                "wave_members",
                &[
                    (
                        "mailbox_jobs",
                        "job_id",
                        "id",
                        "NO ACTION",
                        "NO ACTION",
                        "NONE",
                    ),
                    ("waves", "wave_id", "id", "NO ACTION", "NO ACTION", "NONE"),
                ],
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
                "evidence_flag_verification",
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
        for (trigger_name, expected) in [
            (
                "mailbox_queue_facts_search_insert",
                "after insert on mailbox_queue_facts",
            ),
            (
                "mailbox_queue_facts_search_update",
                "after update of job_rowid,project_id,search_key on mailbox_queue_facts",
            ),
            (
                "mailbox_queue_facts_search_delete",
                "after delete on mailbox_queue_facts",
            ),
        ] {
            let trigger: Option<String> = connection
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?1",
                    [trigger_name],
                    |row| row.get(0),
                )
                .optional()?;
            if !trigger.is_some_and(|sql| {
                let sql = sql
                    .to_ascii_lowercase()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                sql.contains(expected) && sql.contains("mailbox_queue_facts_search")
            }) {
                return Err(rusqlite::Error::InvalidQuery);
            }
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
                "idx_message_mismatches_job_run_key",
                &["job_id", "run_id"],
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
            ("idx_wave_members_wave", &["wave_id"], false, false),
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
            ("idx_message_mismatches_job_run_key", "message_mismatches"),
            ("idx_events_run_created", "events"),
            ("idx_transfer_passes_mailbox", "transfer_passes"),
            ("idx_mailbox_queue_facts_project", "mailbox_queue_facts"),
            ("idx_mailbox_jobs_project_queue", "mailbox_jobs"),
            ("one_running_run_per_job", "runs"),
            ("one_active_run_per_job", "runs"),
            ("idx_wave_members_wave", "wave_members"),
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
            ("evidence_flag_verification", "compared_messages"),
            ("evidence_flag_verification", "mismatched_messages"),
            ("evidence_flag_verification", "excepted_messages"),
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
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE verification_outcome NOT IN ('exact_body_match','exact_metadata_match','probable_match','ambiguous','flags_changed','missing','changed','unexpected','incomplete','failed'))",
            "SELECT EXISTS(SELECT 1 FROM evidence_history WHERE verification_method NOT IN ('aggregate_engine','metadata_reconciliation','body_hash','native_dovecot'))",
            "SELECT EXISTS(SELECT 1 FROM evidence_history WHERE verification_outcome NOT IN ('exact_body_match','exact_metadata_match','probable_match','ambiguous','flags_changed','missing','changed','unexpected','incomplete','failed'))",
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE authoritative NOT IN (0,1))",
            "SELECT EXISTS(SELECT 1 FROM evidence_history WHERE authoritative NOT IN (0,1))",
            "SELECT EXISTS(SELECT 1 FROM message_mismatches WHERE mismatch_type NOT IN ('message_id_only','message_present_wrong_folder','missing','extra','duplicated'))",
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE attention_reason IS NOT NULL AND attention_reason NOT IN ('interrupted','verification_incomplete','verification_limit_exceeded','verification_difference','process_identity_unverified','authentication_failed','transport_failed','policy_blocked','configuration_invalid','capacity_limited','message_rejected','unknown'))",
            "SELECT EXISTS(SELECT 1 FROM webhook_deliveries WHERE status NOT IN ('queued','delivering','delivered','dead_letter'))",
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
            "SELECT EXISTS(SELECT 1 FROM evidence e JOIN evidence_flag_verification f ON f.job_id=e.job_id AND f.run_id=e.run_id WHERE e.verification_outcome IN ('exact_body_match','exact_metadata_match') AND f.mismatched_messages<>0)",
            "SELECT EXISTS(SELECT 1 FROM evidence_history h JOIN evidence_flag_verification f ON f.job_id=h.job_id AND f.run_id=h.run_id WHERE h.verification_outcome IN ('exact_body_match','exact_metadata_match') AND f.mismatched_messages<>0)",
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
            "SELECT EXISTS(SELECT 1 FROM evidence_flag_verification f JOIN runs r ON r.id=f.run_id WHERE r.job_id IS NULL OR r.job_id<>f.job_id)",
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

    pub(super) fn validate_schema_constraints(connection: &Connection) -> rusqlite::Result<()> {
        const REQUIRED_CHECKS: &[(&str, &[&str])] = &[
            (
                "waves",
                &[
                    "length(name)between1and120",
                    "position>=0",
                    "concurrencyisnullorconcurrency>=1",
                ],
            ),
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
                "evidence_flag_verification",
                &[
                    "compared_messages>=0",
                    "mismatched_messages>=0",
                    "excepted_messages>=0",
                    "mismatched_messages+excepted_messages<=compared_messages",
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
                    "statusin('queued','delivering','delivered','dead_letter')",
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

    pub(super) fn current_schema_is_clean(connection: &Connection) -> bool {
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

    pub(super) fn has_legacy_mailbox_table(connection: &Connection) -> rusqlite::Result<bool> {
        connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='mailbox_jobs')",
            [],
            |row| row.get(0),
        )
    }
}
