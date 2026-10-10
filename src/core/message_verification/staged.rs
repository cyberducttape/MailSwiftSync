//! SQLite-staged reconciliation for accounts too large to hold in memory.

use super::*;

pub(super) fn sqlite_stage_size(message: &StagedMessage) -> Result<i64, String> {
    message
        .message
        .size_bytes
        .ok_or_else(|| "staged metadata is missing RFC822.SIZE".to_owned())
        .and_then(|value| {
            i64::try_from(value).map_err(|_| "staged RFC822.SIZE exceeds SQLite range".to_owned())
        })
}

pub(super) fn stage_candidate<P: rusqlite::Params>(
    connection: &rusqlite::Connection,
    sql: &str,
    parameters: P,
) -> Result<Option<StagedMessage>, String> {
    // Cached: reconciliation runs these few statements once per message.
    crate::core::stage_sql::prepare_cached(connection, sql)
        .and_then(|mut statement| statement.query_row(parameters, staged_message_from_row))
        .optional()
        .map_err(|error| format!("could not query staged reconciliation candidate: {error}"))
}

pub(super) fn mark_stage_matched(
    connection: &rusqlite::Connection,
    side: StagedMessageSide,
    message: &StagedMessage,
) -> Result<(), String> {
    let uidvalidity = message
        .key
        .uidvalidity
        .map(|value| {
            i64::try_from(value).map_err(|_| "staged UIDVALIDITY exceeds SQLite range".to_owned())
        })
        .transpose()?
        .unwrap_or(-1);
    crate::core::stage_sql::prepare_cached(
        connection,
        "INSERT INTO staged_matched(side,mailbox,uidvalidity,uid) VALUES(?1,?2,?3,?4)",
    )
    .and_then(|mut statement| {
        statement.execute(params![
            side.as_i64(),
            message.key.mailbox.as_ref(),
            uidvalidity,
            message.key.uid,
        ])
    })
    .map(|_| ())
    .map_err(|error| format!("could not mark staged message as matched: {error}"))
}

/// Stage one mismatch row; reconciliation keeps no mismatch detail in memory.
pub(super) fn append_stage_mismatch(
    connection: &rusqlite::Connection,
    mismatch: MessageMismatch,
    staged: &mut u64,
) -> Result<(), String> {
    insert_staged_mismatch(connection, &mismatch)?;
    *staged = staged.saturating_add(1);
    Ok(())
}

impl MessageVerification {
    /// Reconcile staged metadata and return its mismatches in memory, under
    /// the estimated mismatch-detail budget. Durable live runs use
    /// `reconcile_stage` instead and stream the staged rows into the ledger.
    pub(crate) fn detect_mismatches_from_stage(
        job_id: &str,
        run_id: &str,
        stage: &MessageMetadataStage,
        folder_mapping: &HashMap<String, String>,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        let (_, summary) = Self::reconcile_stage(job_id, run_id, stage, folder_mapping)?;
        let job_context: Arc<str> = Arc::from(job_id);
        let run_context: Arc<str> = Arc::from(run_id);
        let mut mismatches = Vec::new();
        let mut estimated_bytes = 0usize;
        let mut budget_error = None;
        visit_staged_mismatches(
            stage.connection()?,
            &job_context,
            &run_context,
            &mut |mismatch| {
                append_mismatch_with_budget(&mut mismatches, mismatch.clone(), &mut estimated_bytes)
                    .map_err(|error| {
                        budget_error = Some(error);
                        rusqlite::Error::InvalidQuery
                    })
            },
        )
        .map_err(|error| {
            budget_error
                .take()
                .unwrap_or_else(|| format!("could not read staged mismatches: {error}"))
        })?;
        Ok((mismatches, summary))
    }

    /// Reconcile metadata staged in SQLite, writing every mismatch to the
    /// stage's `staged_mismatches` table. Only one bounded batch and one
    /// candidate row are resident in Rust at a time, and mismatch volume is
    /// bounded by disk, not memory. Returns the staged mismatch count.
    pub(crate) fn reconcile_stage(
        job_id: &str,
        run_id: &str,
        stage: &MessageMetadataStage,
        folder_mapping: &HashMap<String, String>,
    ) -> Result<(u64, VerificationSummary), String> {
        let connection = stage.connection()?;
        // One transaction for the whole pass. A durable stage runs with FULL
        // synchronization, so autocommitting each matched-row insert costs a
        // disk sync per message. Reconciliation is reset and recomputed on
        // every run, so rolling back an interrupted pass loses nothing.
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin staged reconciliation: {error}"))?;
        crate::core::stage_sql::execute_batch(connection,
                // Keep reconciliation intermediates in the private stage
                // database. TEMP tables may spill into SQLite's process-wide
                // temporary directory when temp_store=FILE, escaping the
                // per-run permissions and cleanup lifecycle.
                &format!("CREATE TABLE staged_matched(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uid TEXT NOT NULL, PRIMARY KEY(side,mailbox,uidvalidity,uid));
                 CREATE TABLE staged_folder_mapping(source TEXT PRIMARY KEY, destination TEXT NOT NULL);
                 DROP TABLE IF EXISTS staged_mismatches;
                 {STAGED_MISMATCHES_TABLE};"),
            )
            .map_err(|error| format!("could not initialize staged reconciliation: {error}"))?;
        for (source, destination) in folder_mapping {
            crate::core::stage_sql::execute(
                connection,
                "INSERT INTO staged_folder_mapping(source,destination) VALUES(?1,?2)",
                params![source, destination],
            )
            .map_err(|error| format!("could not stage folder mapping: {error}"))?;
        }
        // Precompute the effective destination folder and let SQLite seek on
        // it directly during fallback. Evaluating this mapping expression in
        // each count query can repeatedly scan records from every folder that
        // shares the same date/size pair.
        crate::core::stage_sql::execute(connection,
                "UPDATE staged_messages SET match_mailbox=COALESCE((SELECT destination FROM staged_folder_mapping WHERE source=staged_messages.mailbox),mailbox) WHERE side=0",
                [],
            )
            .map_err(|error| format!("could not index staged source folder mapping: {error}"))?;

        let job_context: Arc<str> = Arc::from(job_id);
        let run_context: Arc<str> = Arc::from(run_id);
        let mut staged_mismatches = 0_u64;
        // Pair exact Message-ID/folder/date/size groups in SQLite. Ranked
        // rows are materialized and indexed before joining: joining two
        // unindexed window-function CTEs made SQLite choose a quadratic plan
        // on large stages. Row numbers preserve the old deterministic greedy
        // order (source staging order, then destination mailbox/UID order).
        crate::core::stage_sql::execute_batch(connection,
                "CREATE TABLE staged_exact_source_ranked AS
                    SELECT rowid AS source_rowid,message_id,match_mailbox,date_key,size_bytes,
                           ROW_NUMBER() OVER (
                               PARTITION BY message_id,match_mailbox,date_key,size_bytes
                               ORDER BY rowid
                           ) AS ordinal
                    FROM staged_messages INDEXED BY staged_messages_exact_source
                    WHERE side=0 AND message_id IS NOT NULL
                      AND date_key IS NOT NULL AND size_bytes IS NOT NULL;
                 CREATE INDEX staged_exact_source_ranked_key
                    ON staged_exact_source_ranked(message_id,match_mailbox,date_key,size_bytes,ordinal);
                 CREATE TABLE staged_exact_destination_ranked AS
                    SELECT rowid AS destination_rowid,message_id,mailbox AS match_mailbox,date_key,size_bytes,
                           ROW_NUMBER() OVER (
                               PARTITION BY message_id,mailbox,date_key,size_bytes
                               ORDER BY mailbox,uidvalidity,uid
                           ) AS ordinal
                    FROM staged_messages INDEXED BY staged_messages_exact_destination
                    WHERE side=1 AND message_id IS NOT NULL
                      AND date_key IS NOT NULL AND size_bytes IS NOT NULL;
                 CREATE INDEX staged_exact_destination_ranked_key
                    ON staged_exact_destination_ranked(message_id,match_mailbox,date_key,size_bytes,ordinal);
                 CREATE TABLE staged_exact_pairs AS
                    SELECT s.source_rowid,d.destination_rowid
                    FROM staged_exact_source_ranked s
                    JOIN staged_exact_destination_ranked d
                      ON d.message_id=s.message_id
                     AND d.match_mailbox=s.match_mailbox
                     AND d.date_key=s.date_key
                     AND d.size_bytes=s.size_bytes
                     AND d.ordinal=s.ordinal;
                 INSERT INTO staged_matched(side,mailbox,uidvalidity,uid)
                    SELECT 0,m.mailbox,m.uidvalidity,m.uid
                    FROM staged_exact_pairs p
                    JOIN staged_messages m ON m.rowid=p.source_rowid
                 UNION ALL
                    SELECT 1,m.mailbox,m.uidvalidity,m.uid
                    FROM staged_exact_pairs p
                    JOIN staged_messages m ON m.rowid=p.destination_rowid;
                 DROP TABLE staged_exact_pairs;
                 DROP TABLE staged_exact_source_ranked;
                 DROP TABLE staged_exact_destination_ranked;",
            )
            .map_err(|error| format!("could not reconcile exact staged metadata: {error}"))?;
        let metadata_matches = connection
            .query_row(
                "SELECT COUNT(*) FROM staged_matched WHERE side=0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("could not count exact staged metadata matches: {error}"))
            .and_then(|count: i64| {
                u64::try_from(count)
                    .map_err(|_| format!("exact staged metadata count is invalid: {count}"))
            })?;
        let mut missing_count = 0_u64;
        let mut extra_count = 0_u64;
        let mut duplicated_count = 0_u64;
        let mut changed_count = 0_u64;

        // Remaining Message-ID matches in the expected folder are changes.
        // Rank each unmatched side once, pair ordinally, and materialize the
        // pairs so the only Rust work is constructing bounded mismatch proof.
        crate::core::stage_sql::execute_batch(
            connection,
            "CREATE TABLE staged_changed_source_ranked AS
                    SELECT rowid AS source_rowid,message_id,match_mailbox,
                           ROW_NUMBER() OVER (
                               PARTITION BY message_id,match_mailbox ORDER BY rowid
                           ) AS ordinal
                    FROM staged_messages s
                    WHERE side=0 AND message_id IS NOT NULL
                      AND NOT EXISTS (
                          SELECT 1 FROM staged_matched m
                          WHERE m.side=s.side AND m.mailbox=s.mailbox
                            AND m.uidvalidity=s.uidvalidity AND m.uid=s.uid
                      );
                 CREATE INDEX staged_changed_source_key
                    ON staged_changed_source_ranked(message_id,match_mailbox,ordinal);
                 CREATE TABLE staged_changed_destination_ranked AS
                    SELECT rowid AS destination_rowid,message_id,mailbox AS match_mailbox,
                           ROW_NUMBER() OVER (
                               PARTITION BY message_id,mailbox
                               ORDER BY mailbox,uidvalidity,uid
                           ) AS ordinal
                    FROM staged_messages d
                    WHERE side=1 AND message_id IS NOT NULL
                      AND NOT EXISTS (
                          SELECT 1 FROM staged_matched m
                          WHERE m.side=d.side AND m.mailbox=d.mailbox
                            AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid
                      );
                 CREATE INDEX staged_changed_destination_key
                    ON staged_changed_destination_ranked(message_id,match_mailbox,ordinal);
                 CREATE TABLE staged_changed_pairs AS
                    SELECT s.source_rowid,d.destination_rowid
                    FROM staged_changed_source_ranked s
                    JOIN staged_changed_destination_ranked d
                      ON d.message_id=s.message_id
                     AND d.match_mailbox=s.match_mailbox
                     AND d.ordinal=s.ordinal;
                 INSERT INTO staged_matched(side,mailbox,uidvalidity,uid)
                    SELECT 0,m.mailbox,m.uidvalidity,m.uid
                    FROM staged_changed_pairs p
                    JOIN staged_messages m ON m.rowid=p.source_rowid
                 UNION ALL
                    SELECT 1,m.mailbox,m.uidvalidity,m.uid
                    FROM staged_changed_pairs p
                    JOIN staged_messages m ON m.rowid=p.destination_rowid;",
        )
        .map_err(|error| format!("could not reconcile changed staged IDs: {error}"))?;
        {
            let mut statement = crate::core::stage_sql::prepare(connection,
                    "SELECT s.rowid,s.mailbox,s.uidvalidity,s.uid,s.message_id,s.internal_date,s.date_key,s.size_bytes,
                            d.rowid,d.mailbox,d.uidvalidity,d.uid,d.message_id,d.internal_date,d.date_key,d.size_bytes
                     FROM staged_changed_pairs p
                     JOIN staged_messages s ON s.rowid=p.source_rowid
                     JOIN staged_messages d ON d.rowid=p.destination_rowid
                     ORDER BY p.source_rowid",
                )
                .map_err(|error| format!("could not read changed staged pairs: {error}"))?;
            let pairs = statement
                .query_map([], staged_message_pair_from_row)
                .map_err(|error| format!("could not query changed staged pairs: {error}"))?;
            for pair in pairs {
                let (source, destination) =
                    pair.map_err(|error| format!("could not decode changed staged pair: {error}"))?;
                append_stage_mismatch(
                    connection,
                    make_mismatch(
                        &job_context,
                        &run_context,
                        MismatchType::MessageIdOnly,
                        Some(&source.key),
                        Some(&destination.key),
                        Some(&source.message),
                        Some(&destination.message),
                    ),
                    &mut staged_mismatches,
                )?;
                changed_count = changed_count.saturating_add(1);
            }
        }
        crate::core::stage_sql::execute_batch(
            connection,
            "DROP TABLE staged_changed_pairs;
                 DROP TABLE staged_changed_source_ranked;
                 DROP TABLE staged_changed_destination_ranked;",
        )
        .map_err(|error| format!("could not release changed reconciliation rows: {error}"))?;

        // Same Message-ID and metadata in a wrong folder.
        let mut after_rowid = 0_i64;
        loop {
            let batch = stage
                .batch(StagedMessageSide::Source, after_rowid, true, true)
                .map_err(|error| format!("could not read staged source metadata: {error}"))?;
            if batch.is_empty() {
                break;
            }
            after_rowid = batch.last().map_or(after_rowid, |row| row.rowid);
            for source in batch {
                let expected = expected_destination_folder(&source.key, folder_mapping);
                let query = "SELECT rowid,mailbox,uidvalidity,uid,message_id,internal_date,date_key,size_bytes FROM staged_messages d INDEXED BY staged_messages_exact_destination WHERE d.side=1 AND d.message_id=?1 AND d.date_key=?2 AND d.size_bytes=?3 AND d.mailbox< ?4 AND NOT EXISTS(SELECT 1 FROM staged_matched m WHERE m.side=d.side AND m.mailbox=d.mailbox AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid) ORDER BY d.mailbox DESC,d.uidvalidity,d.uid LIMIT 1";
                let lower = stage_candidate(
                    connection,
                    query,
                    params![
                        source.message.message_id,
                        source.date_key,
                        sqlite_stage_size(&source)?,
                        expected
                    ],
                )?;
                let query = "SELECT rowid,mailbox,uidvalidity,uid,message_id,internal_date,date_key,size_bytes FROM staged_messages d INDEXED BY staged_messages_exact_destination WHERE d.side=1 AND d.message_id=?1 AND d.date_key=?2 AND d.size_bytes=?3 AND d.mailbox> ?4 AND NOT EXISTS(SELECT 1 FROM staged_matched m WHERE m.side=d.side AND m.mailbox=d.mailbox AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid) ORDER BY d.mailbox,d.uidvalidity,d.uid LIMIT 1";
                let upper = stage_candidate(
                    connection,
                    query,
                    params![
                        source.message.message_id,
                        source.date_key,
                        sqlite_stage_size(&source)?,
                        expected
                    ],
                )?;
                let destination = match (lower, upper) {
                    (Some(lower), Some(upper)) => {
                        if lower.key.mailbox <= upper.key.mailbox {
                            lower
                        } else {
                            upper
                        }
                    }
                    (Some(row), None) | (None, Some(row)) => row,
                    (None, None) => continue,
                };
                mark_stage_matched(connection, StagedMessageSide::Source, &source)?;
                mark_stage_matched(connection, StagedMessageSide::Destination, &destination)?;
                append_stage_mismatch(
                    connection,
                    make_mismatch(
                        &job_context,
                        &run_context,
                        MismatchType::PresentWrongFolder,
                        Some(&source.key),
                        Some(&destination.key),
                        Some(&source.message),
                        Some(&destination.message),
                    ),
                    &mut staged_mismatches,
                )?;
                changed_count = changed_count.saturating_add(1);
            }
        }

        // A probable match is permitted only when exactly one unmatched
        // message exists on each side of a mapped-folder/date/size bucket.
        // Reduce each side to its singleton buckets first, then pair them
        // one to one. Joining rows before discarding shared buckets cost the
        // square of a bucket's size: 100,000 messages per side with one
        // shared date/size bucket of 5,000 took about a minute.
        crate::core::stage_sql::execute_batch(
            connection,
            "CREATE TABLE staged_probable_source AS
                SELECT mailbox,uidvalidity,uid,match_mailbox,date_key,size_bytes FROM (
                    SELECT s.mailbox,s.uidvalidity,s.uid,s.match_mailbox,s.date_key,s.size_bytes,
                           COUNT(*) OVER (PARTITION BY s.match_mailbox,s.date_key,s.size_bytes) AS n
                    FROM staged_messages s
                    WHERE s.side=0 AND s.date_key IS NOT NULL AND s.size_bytes IS NOT NULL
                      AND NOT EXISTS (
                          SELECT 1 FROM staged_matched m
                          WHERE m.side=s.side AND m.mailbox=s.mailbox
                            AND m.uidvalidity=s.uidvalidity AND m.uid=s.uid
                      )
                ) WHERE n=1;
             CREATE INDEX staged_probable_source_key
                ON staged_probable_source(match_mailbox,date_key,size_bytes);
             CREATE TABLE staged_probable_destination AS
                SELECT mailbox,uidvalidity,uid,match_mailbox,date_key,size_bytes FROM (
                    SELECT d.mailbox,d.uidvalidity,d.uid,d.match_mailbox,d.date_key,d.size_bytes,
                           COUNT(*) OVER (PARTITION BY d.match_mailbox,d.date_key,d.size_bytes) AS n
                    FROM staged_messages d
                    WHERE d.side=1 AND d.date_key IS NOT NULL AND d.size_bytes IS NOT NULL
                      AND NOT EXISTS (
                          SELECT 1 FROM staged_matched m
                          WHERE m.side=d.side AND m.mailbox=d.mailbox
                            AND m.uidvalidity=d.uidvalidity AND m.uid=d.uid
                      )
                ) WHERE n=1;
             CREATE TABLE staged_probable_pairs AS
                SELECT s.mailbox AS source_mailbox,
                       s.uidvalidity AS source_uidvalidity,
                       s.uid AS source_uid,
                       d.mailbox AS destination_mailbox,
                       d.uidvalidity AS destination_uidvalidity,
                       d.uid AS destination_uid
                FROM staged_probable_destination d
                JOIN staged_probable_source s
                  ON s.match_mailbox=d.match_mailbox
                 AND s.date_key=d.date_key AND s.size_bytes=d.size_bytes;
             INSERT INTO staged_matched(side,mailbox,uidvalidity,uid)
                SELECT 0,source_mailbox,source_uidvalidity,source_uid FROM staged_probable_pairs
             UNION ALL
                SELECT 1,destination_mailbox,destination_uidvalidity,destination_uid FROM staged_probable_pairs;",
        )
        .map_err(|error| format!("could not reconcile unique staged fingerprints: {error}"))?;
        let probable_matches = connection
            .query_row("SELECT COUNT(*) FROM staged_probable_pairs", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|error| format!("could not count probable staged matches: {error}"))
            .and_then(|count| {
                u64::try_from(count)
                    .map_err(|_| format!("probable staged match count is invalid: {count}"))
            })?;
        crate::core::stage_sql::execute_batch(
            connection,
            "DROP TABLE staged_probable_pairs;
             DROP TABLE staged_probable_source;
             DROP TABLE staged_probable_destination;",
        )
        .map_err(|error| format!("could not release probable reconciliation rows: {error}"))?;

        // Remaining rows become bounded mismatch evidence.
        after_rowid = 0;
        loop {
            let batch = stage
                .batch(StagedMessageSide::Source, after_rowid, false, false)
                .map_err(|error| error.to_string())?;
            if batch.is_empty() {
                break;
            }
            after_rowid = batch.last().map_or(after_rowid, |row| row.rowid);
            for source in batch {
                append_stage_mismatch(
                    connection,
                    make_mismatch(
                        &job_context,
                        &run_context,
                        MismatchType::Missing,
                        Some(&source.key),
                        None,
                        Some(&source.message),
                        None,
                    ),
                    &mut staged_mismatches,
                )?;
                missing_count = missing_count.saturating_add(1);
            }
        }
        crate::core::stage_sql::execute_batch(connection, "CREATE TABLE staged_duplicate_ids AS SELECT d.message_id FROM staged_messages d WHERE d.side=1 AND d.message_id IS NOT NULL GROUP BY d.message_id HAVING COUNT(*) > (SELECT COUNT(*) FROM staged_messages s WHERE s.side=0 AND s.message_id=d.message_id) AND (SELECT COUNT(*) FROM staged_messages s WHERE s.side=0 AND s.message_id=d.message_id) > 0; CREATE INDEX staged_duplicate_ids_message_id ON staged_duplicate_ids(message_id);").map_err(|error| error.to_string())?;
        after_rowid = 0;
        loop {
            let batch = stage
                .batch(StagedMessageSide::Destination, after_rowid, false, false)
                .map_err(|error| error.to_string())?;
            if batch.is_empty() {
                break;
            }
            after_rowid = batch.last().map_or(after_rowid, |row| row.rowid);
            for destination in batch {
                let duplicated = match destination.message.message_id.as_deref() {
                    Some(id) => crate::core::stage_sql::prepare_cached(
                        connection,
                        "SELECT EXISTS(SELECT 1 FROM staged_duplicate_ids WHERE message_id=?1)",
                    )
                    .and_then(|mut statement| {
                        statement.query_row([id], |row| row.get::<_, bool>(0))
                    })
                    .map_err(|error| {
                        format!("could not classify staged duplicate message: {error}")
                    })?,
                    None => false,
                };
                let mismatch_type = if duplicated {
                    MismatchType::Duplicated
                } else {
                    MismatchType::Extra
                };
                append_stage_mismatch(
                    connection,
                    make_mismatch(
                        &job_context,
                        &run_context,
                        mismatch_type,
                        None,
                        Some(&destination.key),
                        None,
                        Some(&destination.message),
                    ),
                    &mut staged_mismatches,
                )?;
                if duplicated {
                    duplicated_count = duplicated_count.saturating_add(1);
                } else {
                    extra_count = extra_count.saturating_add(1);
                }
            }
        }

        let total_source = stage
            .count(StagedMessageSide::Source)
            .map_err(|error| error.to_string())?;
        let total_destination = stage
            .count(StagedMessageSide::Destination)
            .map_err(|error| error.to_string())?;
        if total_source
            != metadata_matches
                .saturating_add(probable_matches)
                .saturating_add(missing_count)
                .saturating_add(changed_count)
            || total_destination
                != metadata_matches
                    .saturating_add(probable_matches)
                    .saturating_add(extra_count)
                    .saturating_add(duplicated_count)
                    .saturating_add(changed_count)
        {
            return Err(
                "staged reconciliation accounting did not partition both message populations"
                    .into(),
            );
        }
        transaction
            .commit()
            .map_err(|error| format!("could not finish staged reconciliation: {error}"))?;
        Ok((
            staged_mismatches,
            VerificationSummary {
                total_source,
                total_destination,
                metadata_matches,
                probable_matches,
                missing_count,
                extra_count,
                duplicated_count,
                changed_count,
            },
        ))
    }
}
