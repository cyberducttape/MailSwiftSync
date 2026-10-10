use super::*;
use std::sync::Arc;

pub(super) fn upsert_evidence_projection(
    tx: &rusqlite::Transaction<'_>,
    job_id: &str,
    run_id: &str,
    value: &MailboxEvidence,
    outcome: VerificationOutcome,
    authoritative: bool,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO evidence(job_id,verification_method,verification_outcome,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,probable_messages,captured_at,run_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,CURRENT_TIMESTAMP,?17) ON CONFLICT(job_id) DO UPDATE SET verification_method=excluded.verification_method,verification_outcome=excluded.verification_outcome,source_messages=excluded.source_messages,destination_messages=excluded.destination_messages,source_bytes=excluded.source_bytes,destination_bytes=excluded.destination_bytes,unmatched_messages=excluded.unmatched_messages,failed_messages=excluded.failed_messages,source_folders=excluded.source_folders,destination_folders=excluded.destination_folders,authoritative=excluded.authoritative,missing_messages=excluded.missing_messages,extra_messages=excluded.extra_messages,modified_messages=excluded.modified_messages,probable_messages=excluded.probable_messages,captured_at=excluded.captured_at,run_id=excluded.run_id",
        params![
            job_id,
            value.verification_method().as_str(),
            outcome.as_str(),
            sqlite_i64(value.source_messages)?,
            sqlite_i64(value.destination_messages)?,
            sqlite_i64(value.source_bytes)?,
            sqlite_i64(value.destination_bytes)?,
            sqlite_optional_i64(value.unmatched_messages)?,
            sqlite_i64(value.failed_messages)?,
            sqlite_i64(value.source_folders)?,
            sqlite_i64(value.destination_folders)?,
            authoritative,
            sqlite_i64(value.missing_messages)?,
            sqlite_i64(value.extra_messages)?,
            sqlite_i64(value.modified_messages)?,
            sqlite_i64(value.probable_messages)?,
            run_id,
        ],
    )?;
    // Flag verification is stored per evidence run, so evidence recorded
    // before it existed keeps reading as "not verified" rather than "clean".
    tx.execute(
        "DELETE FROM evidence_flag_verification WHERE job_id=?1 AND run_id=?2",
        params![job_id, run_id],
    )?;
    if let Some(flags) = value.flag_verification {
        if !flags.is_consistent() {
            return Err(super::ledger_rejection(
                "flag verification counts are inconsistent",
            ));
        }
        tx.execute(
            "INSERT INTO evidence_flag_verification(job_id,run_id,compared_messages,mismatched_messages,excepted_messages) VALUES(?1,?2,?3,?4,?5)",
            params![
                job_id,
                run_id,
                sqlite_i64(flags.compared_messages)?,
                sqlite_i64(flags.mismatched_messages)?,
                sqlite_i64(flags.excepted_messages)?,
            ],
        )?;
    }
    Ok(())
}

const MISMATCH_COLUMNS: &str = "id,mismatch_type,source_folder,destination_folder,source_uidvalidity,destination_uidvalidity,source_uid,dest_uid,source_message_id,dest_message_id,source_size_bytes,dest_size_bytes,source_date,dest_date,source_fingerprint,destination_fingerprint";

/// One keyset page of a run's mismatches. `idx_message_mismatches_job_run_key`
/// lets it seek on `(job_id, run_id, rowid)` instead of scanning all runs.
pub(super) const MISMATCH_PAGE_SQL: &str = "SELECT rowid,mismatch_type,source_folder_digest,destination_folder_digest,source_uidvalidity,destination_uidvalidity,source_uid,dest_uid,source_size_bytes,dest_size_bytes,source_date,dest_date FROM message_mismatches WHERE job_id=?1 AND run_id=?2 AND rowid>?3 AND (?4 IS NULL OR mismatch_type=?4) AND (?5 IS NULL OR COALESCE(source_folder_digest,destination_folder_digest,'')=?5) ORDER BY rowid LIMIT ?6";

/// Filter for the verification drill-down.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MismatchFilter {
    pub mismatch_type: Option<MismatchType>,
    /// A `folder_digest`, or the empty string for rows without one.
    pub folder_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MismatchFolderSummary {
    pub folder_digest: String,
    pub mismatch_type: MismatchType,
    pub count: u64,
}

/// A durable mismatch row as the drill-down reads it. Message-IDs and
/// folder names are never stored; folders are project-scoped digests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMismatch {
    /// Keyset pagination key.
    pub key: i64,
    pub mismatch_type: MismatchType,
    pub source_folder_digest: Option<String>,
    pub destination_folder_digest: Option<String>,
    pub source_uidvalidity: Option<u64>,
    pub destination_uidvalidity: Option<u64>,
    pub source_uid: Option<String>,
    pub dest_uid: Option<String>,
    pub source_size_bytes: Option<u64>,
    pub dest_size_bytes: Option<u64>,
    pub source_date: Option<String>,
    pub dest_date: Option<String>,
}

fn mismatch_from_row(
    row: &rusqlite::Row<'_>,
    job_context: &Arc<str>,
    run_context: &Arc<str>,
) -> rusqlite::Result<MessageMismatch> {
    let mismatch_type: String = row.get(1)?;
    let Some(mismatch_type) = MismatchType::parse(&mismatch_type) else {
        return Err(rusqlite::Error::InvalidQuery);
    };
    Ok(MessageMismatch {
        id: row.get(0)?,
        job_id: Arc::clone(job_context),
        run_id: Arc::clone(run_context),
        mismatch_type,
        source_folder: row.get(2)?,
        destination_folder: row.get(3)?,
        source_uidvalidity: row.get::<_, Option<i64>>(4)?.map(sqlite_u64).transpose()?,
        destination_uidvalidity: row.get::<_, Option<i64>>(5)?.map(sqlite_u64).transpose()?,
        source_uid: row.get(6)?,
        dest_uid: row.get(7)?,
        source_message_id: row.get(8)?,
        dest_message_id: row.get(9)?,
        source_size_bytes: row.get::<_, Option<i64>>(10)?.map(sqlite_u64).transpose()?,
        dest_size_bytes: row.get::<_, Option<i64>>(11)?.map(sqlite_u64).transpose()?,
        source_date: row.get(12)?,
        dest_date: row.get(13)?,
        source_fingerprint: row.get(14)?,
        destination_fingerprint: row.get(15)?,
    })
}

/// Folder names and server-supplied values are untrusted: a cell starting
/// with a formula trigger would be evaluated when the export is opened in a
/// spreadsheet, so it is prefixed with an apostrophe (OWASP CSV injection).
fn spreadsheet_safe(value: String) -> String {
    if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value
    }
}

fn finish_csv(
    writer: csv::Writer<Vec<u8>>,
    written: usize,
    truncated: bool,
) -> Result<(String, usize, bool), String> {
    let bytes = writer.into_inner().map_err(|error| error.to_string())?;
    let text = String::from_utf8(bytes).map_err(|error| error.to_string())?;
    Ok((text, written, truncated))
}

/// Decode the three nullable `evidence_flag_verification` columns selected by
/// a LEFT JOIN starting at `offset`. A missing row means not verified.
pub(super) fn flag_verification_from_row(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Option<FlagVerification>> {
    let Some(compared) = row.get::<_, Option<i64>>(offset)? else {
        return Ok(None);
    };
    Ok(Some(FlagVerification {
        compared_messages: sqlite_u64(compared)?,
        mismatched_messages: sqlite_u64(row.get(offset + 1)?)?,
        excepted_messages: sqlite_u64(row.get(offset + 2)?)?,
    }))
}

impl StateStore {
    /// Finish an audit-only verification run. Unlike transfer completion this
    /// writes history and mismatch evidence without replacing the mailbox's
    /// current migration projection or changing its operational state.
    pub fn finish_verification_only_run(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        value: &MailboxEvidence,
        mismatches: &(impl super::MismatchSource + ?Sized),
        detail: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE runs SET status='completed',finished_at=CURRENT_TIMESTAMP,detail=?1 WHERE id=?2 AND project_id=?3 AND job_id=?4 AND status='running'",
            params![bounded_event_detail(detail), run_id, project_id, job_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute(
            "INSERT INTO evidence_history(job_id,run_id,verification_method,verification_outcome,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,probable_messages) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,1,?13,?14,?15,?16)",
            params![
                job_id,
                run_id,
                value.verification_method().as_str(),
                value.verification_outcome().as_str(),
                sqlite_i64(value.source_messages)?,
                sqlite_i64(value.destination_messages)?,
                sqlite_i64(value.source_bytes)?,
                sqlite_i64(value.destination_bytes)?,
                sqlite_optional_i64(value.unmatched_messages)?,
                sqlite_i64(value.failed_messages)?,
                sqlite_i64(value.source_folders)?,
                sqlite_i64(value.destination_folders)?,
                sqlite_i64(value.missing_messages)?,
                sqlite_i64(value.extra_messages)?,
                sqlite_i64(value.modified_messages)?,
                sqlite_i64(value.probable_messages)?,
            ],
        )?;
        if let Some(flags) = value.flag_verification {
            if !flags.is_consistent() {
                return Err(rusqlite::Error::InvalidQuery);
            }
            tx.execute(
                "INSERT INTO evidence_flag_verification(job_id,run_id,compared_messages,mismatched_messages,excepted_messages) VALUES(?1,?2,?3,?4,?5)",
                params![
                    job_id,
                    run_id,
                    sqlite_i64(flags.compared_messages)?,
                    sqlite_i64(flags.mismatched_messages)?,
                    sqlite_i64(flags.excepted_messages)?,
                ],
            )?;
        }
        tx.execute(
            "DELETE FROM message_mismatches WHERE job_id=?1 AND run_id=?2",
            params![job_id, run_id],
        )?;
        let mut insert = tx.prepare_cached(
            "INSERT INTO message_mismatches(id,job_id,run_id,mismatch_type,source_uid,dest_uid,source_message_id,dest_message_id,source_size_bytes,dest_size_bytes,source_date,dest_date,source_folder,destination_folder,source_uidvalidity,destination_uidvalidity,source_fingerprint,destination_fingerprint,source_folder_digest,destination_folder_digest) VALUES(?1,?2,?3,?4,?5,?6,NULL,NULL,?7,?8,?9,?10,NULL,NULL,?11,?12,?13,?14,?15,?16)",
        )?;
        // Stream and validate each row; a rejection rolls back the commit.
        mismatches.try_for_each_mismatch(&mut |mismatch| {
            if mismatch.job_id.as_ref() != job_id
                || mismatch.run_id.as_ref() != run_id
                || mismatch.id.is_empty()
            {
                return Err(super::ledger_rejection(
                    "a mismatch row belongs to a different run or has no identity",
                ));
            }
            insert.execute(params![
                mismatch.id,
                job_id,
                run_id,
                mismatch.mismatch_type.as_str(),
                mismatch.source_uid,
                mismatch.dest_uid,
                mismatch
                    .source_size_bytes
                    .map(i64::try_from)
                    .transpose()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                mismatch
                    .dest_size_bytes
                    .map(i64::try_from)
                    .transpose()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                mismatch.source_date,
                mismatch.dest_date,
                mismatch
                    .source_uidvalidity
                    .map(i64::try_from)
                    .transpose()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                mismatch
                    .destination_uidvalidity
                    .map(i64::try_from)
                    .transpose()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                mismatch.source_fingerprint,
                mismatch.destination_fingerprint,
                mismatch
                    .source_folder
                    .as_deref()
                    .map(|folder| super::folder_digest(project_id, folder)),
                mismatch
                    .destination_folder
                    .as_deref()
                    .map(|folder| super::folder_digest(project_id, folder)),
            ])?;
            Ok(())
        })?;
        drop(insert);
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'verification_finished',?3)",
            params![project_id, run_id, bounded_event_detail(detail)],
        )?;
        tx.commit()
    }

    /// Load a bounded operator-facing mismatch page for one evidence run.
    /// The boolean indicates that additional durable rows exist beyond the
    /// returned page, so reports cannot imply that a truncated view is
    /// complete.
    pub(crate) fn message_mismatches_for_run(
        &self,
        job_id: &str,
        run_id: &str,
        limit: usize,
    ) -> rusqlite::Result<(Vec<MessageMismatch>, bool)> {
        let limit = limit.clamp(1, 10_000);
        let job_context: Arc<str> = Arc::from(job_id);
        let run_context: Arc<str> = Arc::from(run_id);
        let mut statement = self.connection.prepare(&format!(
            "SELECT {MISMATCH_COLUMNS} FROM message_mismatches WHERE job_id=?1 AND run_id=?2 ORDER BY recorded_at,id LIMIT ?3"
        ))?;
        let mut rows = statement.query(params![job_id, run_id, (limit + 1) as i64])?;
        let mut mismatches = Vec::with_capacity(limit.min(256));
        while let Some(row) = rows.next()? {
            mismatches.push(mismatch_from_row(row, &job_context, &run_context)?);
        }
        let truncated = mismatches.len() > limit;
        if truncated {
            mismatches.truncate(limit);
        }
        Ok((mismatches, truncated))
    }

    /// Mismatch counts per folder and type for one evidence run, for the
    /// verification drill-down. Folders are project-scoped digests (names
    /// are never stored); a message's folder is its source folder, or its
    /// destination folder when it exists only there. Rows recorded before
    /// digests existed group under an empty digest.
    pub(crate) fn mismatch_folder_summary(
        &self,
        job_id: &str,
        run_id: &str,
    ) -> rusqlite::Result<Vec<MismatchFolderSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT COALESCE(source_folder_digest,destination_folder_digest,''),mismatch_type,COUNT(*) FROM message_mismatches WHERE job_id=?1 AND run_id=?2 GROUP BY 1,2 ORDER BY 1,2 LIMIT 10000",
        )?;
        let rows = statement.query_map(params![job_id, run_id], |row| {
            Ok(MismatchFolderSummary {
                folder_digest: row.get(0)?,
                mismatch_type: MismatchType::parse(&row.get::<_, String>(1)?)
                    .ok_or(rusqlite::Error::InvalidQuery)?,
                count: sqlite_u64(row.get(2)?)?,
            })
        })?;
        rows.collect()
    }

    /// One keyset page of an evidence run's mismatches matching `filter`,
    /// after the row key `after`, and whether more rows follow.
    pub(crate) fn message_mismatch_page(
        &self,
        job_id: &str,
        run_id: &str,
        filter: &MismatchFilter,
        after: i64,
        limit: usize,
    ) -> rusqlite::Result<(Vec<StoredMismatch>, bool)> {
        let limit = limit.clamp(1, 1_000);
        let mut statement = self.connection.prepare(MISMATCH_PAGE_SQL)?;
        let mut rows = statement.query(params![
            job_id,
            run_id,
            after,
            filter.mismatch_type.map(|value| value.as_str()),
            filter.folder_digest.as_deref(),
            (limit + 1) as i64
        ])?;
        let optional_u64 = |value: Option<i64>| -> rusqlite::Result<Option<u64>> {
            value.map(sqlite_u64).transpose()
        };
        let mut page = Vec::with_capacity(limit.min(256));
        while let Some(row) = rows.next()? {
            page.push(StoredMismatch {
                key: row.get(0)?,
                mismatch_type: MismatchType::parse(&row.get::<_, String>(1)?)
                    .ok_or(rusqlite::Error::InvalidQuery)?,
                source_folder_digest: row.get(2)?,
                destination_folder_digest: row.get(3)?,
                source_uidvalidity: optional_u64(row.get(4)?)?,
                destination_uidvalidity: optional_u64(row.get(5)?)?,
                source_uid: row.get(6)?,
                dest_uid: row.get(7)?,
                source_size_bytes: optional_u64(row.get(8)?)?,
                dest_size_bytes: optional_u64(row.get(9)?)?,
                source_date: row.get(10)?,
                dest_date: row.get(11)?,
            });
        }
        let more = page.len() > limit;
        page.truncate(limit);
        Ok((page, more))
    }

    /// An evidence run's mismatches matching `filter` as CSV, at most
    /// `max_rows` rows. `folder_name` resolves a digest to a name this
    /// process observed; otherwise the digest is written. Returns the CSV,
    /// the rows written, and whether more matched.
    pub(crate) fn export_message_mismatches_csv(
        &self,
        job_id: &str,
        run_id: &str,
        filter: &MismatchFilter,
        max_rows: usize,
        folder_name: &dyn Fn(&str) -> Option<String>,
    ) -> Result<(String, usize, bool), String> {
        let mut writer = csv::Writer::from_writer(Vec::new());
        writer
            .write_record([
                "type",
                "source_folder",
                "destination_folder",
                "source_uidvalidity",
                "source_uid",
                "destination_uidvalidity",
                "destination_uid",
                "source_size_bytes",
                "destination_size_bytes",
                "source_internal_date",
                "destination_internal_date",
            ])
            .map_err(|error| error.to_string())?;
        let folder = |digest: &Option<String>| {
            digest.as_deref().map_or_else(String::new, |digest| {
                folder_name(digest)
                    .map(spreadsheet_safe)
                    .unwrap_or_else(|| format!("sha256:{digest}"))
            })
        };
        let text = |value: &Option<String>| value.clone().map(spreadsheet_safe).unwrap_or_default();
        let number = |value: Option<u64>| value.map(|value| value.to_string()).unwrap_or_default();
        let (mut after, mut written) = (0_i64, 0_usize);
        loop {
            let (page, more) = self
                .message_mismatch_page(job_id, run_id, filter, after, 1_000)
                .map_err(|error| error.to_string())?;
            for mismatch in &page {
                if written == max_rows {
                    return finish_csv(writer, written, true);
                }
                writer
                    .write_record([
                        mismatch.mismatch_type.as_str().to_owned(),
                        folder(&mismatch.source_folder_digest),
                        folder(&mismatch.destination_folder_digest),
                        number(mismatch.source_uidvalidity),
                        text(&mismatch.source_uid),
                        number(mismatch.destination_uidvalidity),
                        text(&mismatch.dest_uid),
                        number(mismatch.source_size_bytes),
                        number(mismatch.dest_size_bytes),
                        text(&mismatch.source_date),
                        text(&mismatch.dest_date),
                    ])
                    .map_err(|error| error.to_string())?;
                written += 1;
                after = mismatch.key;
            }
            if !more {
                return finish_csv(writer, written, false);
            }
        }
    }

    // Legacy evidence insertion is retained only as a fixture helper for
    // historical-state tests. Production callers must use the run-owned
    // terminal methods above, which atomically bind evidence to the run and
    // mailbox state transition.
    #[cfg(test)]
    pub(crate) fn record_evidence(
        &self,
        job_id: &str,
        value: &MailboxEvidence,
    ) -> rusqlite::Result<()> {
        self.record_evidence_for_run(job_id, "legacy", value)
    }
    #[cfg(test)]
    pub(crate) fn record_evidence_for_run(
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
        let source_messages = sqlite_i64(value.source_messages)?;
        let destination_messages = sqlite_i64(value.destination_messages)?;
        let source_bytes = sqlite_i64(value.source_bytes)?;
        let destination_bytes = sqlite_i64(value.destination_bytes)?;
        let unmatched_messages = sqlite_optional_i64(value.unmatched_messages)?;
        let failed_messages = sqlite_i64(value.failed_messages)?;
        let source_folders = sqlite_i64(value.source_folders)?;
        let destination_folders = sqlite_i64(value.destination_folders)?;
        let missing_messages = sqlite_i64(value.missing_messages)?;
        let extra_messages = sqlite_i64(value.extra_messages)?;
        let modified_messages = sqlite_i64(value.modified_messages)?;
        let probable_messages = sqlite_i64(value.probable_messages)?;
        let persisted_run_id = if run_id == "legacy" {
            if let Some(existing_run_id) = tx
                .query_row(
                    "SELECT id FROM runs WHERE job_id=?1 ORDER BY started_at DESC, rowid DESC LIMIT 1",
                    [job_id],
                    |row| row.get(0),
                )
                .optional()?
            {
                existing_run_id
            } else {
                let legacy_run_id = format!("legacy-{job_id}");
                tx.execute(
                    "INSERT INTO runs(id,project_id,job_id,engine,phase_at_start,plan_snapshot,status,finished_at,detail) SELECT ?1,project_id,id,'test','discovery','','completed',CURRENT_TIMESTAMP,'legacy test evidence' FROM mailbox_jobs WHERE id=?2",
                    params![legacy_run_id, job_id],
                )?;
                legacy_run_id
            }
        } else {
            run_id.to_owned()
        };
        tx.execute("INSERT INTO evidence_history(job_id,run_id,verification_method,verification_outcome,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,probable_messages) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)", params![job_id, persisted_run_id, value.verification_method().as_str(), value.verification_outcome().as_str(), source_messages, destination_messages, source_bytes, destination_bytes, unmatched_messages, failed_messages, source_folders, destination_folders, value.authoritative, missing_messages, extra_messages, modified_messages, probable_messages])?;
        upsert_evidence_projection(
            &tx,
            job_id,
            &persisted_run_id,
            value,
            value.verification_outcome(),
            value.authoritative,
        )?;
        tx.commit()?;
        Ok(())
    }
    #[cfg(test)]
    pub fn evidence(&self, job_id: &str) -> rusqlite::Result<Option<MailboxEvidence>> {
        self.connection.query_row("SELECT e.verification_method,e.verification_outcome,e.source_messages,e.destination_messages,e.source_bytes,e.destination_bytes,e.unmatched_messages,e.failed_messages,e.source_folders,e.destination_folders,e.authoritative,e.missing_messages,e.extra_messages,e.modified_messages,e.probable_messages,f.compared_messages,f.mismatched_messages,f.excepted_messages FROM evidence e LEFT JOIN evidence_flag_verification f ON f.job_id=e.job_id AND f.run_id=e.run_id WHERE e.job_id=?1", [job_id], |r| Ok(MailboxEvidence { verification_method: VerificationMethod::parse(&r.get::<_, String>(0)?).ok_or(rusqlite::Error::InvalidQuery)?, verification_outcome: Some(VerificationOutcome::parse(&r.get::<_, String>(1)?).ok_or(rusqlite::Error::InvalidQuery)?), source_messages: sqlite_u64(r.get(2)?)?, destination_messages: sqlite_u64(r.get(3)?)?, source_bytes: sqlite_u64(r.get(4)?)?, destination_bytes: sqlite_u64(r.get(5)?)?, unmatched_messages: sqlite_optional_u64(r.get(6)?)?, failed_messages: sqlite_u64(r.get(7)?)?, source_folders: sqlite_u64(r.get(8)?)?, destination_folders: sqlite_u64(r.get(9)?)?, authoritative:r.get::<_, i64>(10)? != 0, missing_messages: sqlite_u64(r.get(11)?)?, extra_messages: sqlite_u64(r.get(12)?)?, modified_messages: sqlite_u64(r.get(13)?)?, probable_messages: sqlite_u64(r.get(14)?)?, flag_verification: flag_verification_from_row(r, 15)? })).optional()
    }
    #[cfg(test)]
    pub fn latest_evidence_for_run(
        &self,
        job_id: &str,
    ) -> rusqlite::Result<Option<(String, MailboxEvidence)>> {
        self.connection
            .query_row(
                "SELECT h.run_id,h.verification_method,h.verification_outcome,h.source_messages,h.destination_messages,h.source_bytes,h.destination_bytes,h.unmatched_messages,h.failed_messages,h.source_folders,h.destination_folders,h.authoritative,h.missing_messages,h.extra_messages,h.modified_messages,h.probable_messages,f.compared_messages,f.mismatched_messages,f.excepted_messages FROM evidence_history h LEFT JOIN evidence_flag_verification f ON f.job_id=h.job_id AND f.run_id=h.run_id WHERE h.job_id=?1 ORDER BY h.captured_at DESC, h.id DESC LIMIT 1",
                [job_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        MailboxEvidence {
                            verification_method: VerificationMethod::parse(&row.get::<_, String>(1)?).ok_or(rusqlite::Error::InvalidQuery)?,
                            verification_outcome: Some(VerificationOutcome::parse(&row.get::<_, String>(2)?).ok_or(rusqlite::Error::InvalidQuery)?),
                            source_messages: sqlite_u64(row.get(3)?)?,
                            destination_messages: sqlite_u64(row.get(4)?)?,
                            source_bytes: sqlite_u64(row.get(5)?)?,
                            destination_bytes: sqlite_u64(row.get(6)?)?,
                            unmatched_messages: sqlite_optional_u64(row.get(7)?)?,
                            failed_messages: sqlite_u64(row.get(8)?)?,
                            source_folders: sqlite_u64(row.get(9)?)?,
                            destination_folders: sqlite_u64(row.get(10)?)?,
                            authoritative: row.get::<_, i64>(11)? != 0,
                            missing_messages: sqlite_u64(row.get(12)?)?,
                            extra_messages: sqlite_u64(row.get(13)?)?,
                            modified_messages: sqlite_u64(row.get(14)?)?,
                            probable_messages: sqlite_u64(row.get(15)?)?,
                            flag_verification: flag_verification_from_row(row, 16)?,
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
        // Fully exact means exact messages and complete, clean flag coverage.
        // Exact messages with partial or excepted flags remain a genuine,
        // acceptable difference.
        let (run_id, outcome, flags_complete): (String, String, bool) = tx.query_row(
            "SELECT h.run_id,h.verification_outcome,
                    f.job_id IS NULL OR (f.mismatched_messages=0 AND f.excepted_messages=0 AND f.compared_messages=h.source_messages)
             FROM evidence_history h
             LEFT JOIN evidence_flag_verification f ON f.job_id=h.job_id AND f.run_id=h.run_id
             WHERE h.job_id=?1 ORDER BY h.captured_at DESC, h.id DESC LIMIT 1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if flags_complete
            && matches!(
                outcome.as_str(),
                "exact_body_match" | "exact_metadata_match"
            )
        {
            // A later exact re-verification does not silently erase the
            // migration state's outstanding difference. Require the operator
            // to resolve the current verification state with a fresh, actual
            // exception record rather than accepting stale provenance.
            return Err(rusqlite::Error::InvalidQuery);
        }
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
        let (total, verified): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN state='verified' OR (state='verified_with_exceptions' AND EXISTS(SELECT 1 FROM verification_acceptances a WHERE a.job_id=mailbox_jobs.id)) THEN 1 ELSE 0 END) FROM mailbox_jobs WHERE project_id=?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )?;
        if phase == Phase::Verification.as_str() && total > 0 && total == verified {
            let completed = tx.execute(
                "UPDATE projects SET phase='complete' WHERE id=?1 AND phase='verification'",
                [project_id],
            )?;
            if completed != 1 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            tx.execute(
                "INSERT INTO events(project_id,kind,detail) VALUES(?1,'phase_changed','complete')",
                [project_id],
            )?;
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'verification_exception_accepted',?2)",
            params![project_id, bounded_event_detail(&format!("{job_id}: accepted by {operator}: {reason}"))],
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
}
