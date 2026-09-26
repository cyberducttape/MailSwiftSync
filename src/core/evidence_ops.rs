use super::*;

impl StateStore {
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
        let mut statement = self.connection.prepare(
            "SELECT id,mismatch_type,source_folder,destination_folder,source_uidvalidity,destination_uidvalidity,source_uid,dest_uid,source_message_id,dest_message_id,source_size_bytes,dest_size_bytes,source_date,dest_date,source_fingerprint,destination_fingerprint FROM message_mismatches WHERE job_id=?1 AND run_id=?2 ORDER BY recorded_at,id LIMIT ?3",
        )?;
        let mut rows = statement.query(params![job_id, run_id, (limit + 1) as i64])?;
        let mut mismatches = Vec::with_capacity(limit.min(256));
        while let Some(row) = rows.next()? {
            let mismatch_type: String = row.get(1)?;
            let Some(mismatch_type) = MismatchType::parse(&mismatch_type) else {
                return Err(rusqlite::Error::InvalidQuery);
            };
            mismatches.push(MessageMismatch {
                id: row.get(0)?,
                job_id: job_id.to_owned(),
                run_id: run_id.to_owned(),
                mismatch_type,
                source_folder: row.get(2)?,
                destination_folder: row.get(3)?,
                source_uidvalidity: row.get::<_, Option<i64>>(4)?.map(sqlite_u64).transpose()?,
                destination_uidvalidity: row
                    .get::<_, Option<i64>>(5)?
                    .map(sqlite_u64)
                    .transpose()?,
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
            });
        }
        let truncated = mismatches.len() > limit;
        if truncated {
            mismatches.truncate(limit);
        }
        Ok((mismatches, truncated))
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
        tx.execute("INSERT INTO evidence_history(job_id,run_id,verification_method,verification_outcome,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,probable_messages) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)", params![job_id, run_id, value.verification_method().as_str(), value.verification_outcome().as_str(), source_messages, destination_messages, source_bytes, destination_bytes, unmatched_messages, failed_messages, source_folders, destination_folders, value.authoritative, missing_messages, extra_messages, modified_messages, probable_messages])?;
        tx.execute("INSERT INTO evidence(job_id,verification_method,verification_outcome,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,probable_messages) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16) ON CONFLICT(job_id) DO UPDATE SET verification_method=excluded.verification_method,verification_outcome=excluded.verification_outcome,source_messages=excluded.source_messages,destination_messages=excluded.destination_messages,source_bytes=excluded.source_bytes,destination_bytes=excluded.destination_bytes,unmatched_messages=excluded.unmatched_messages,failed_messages=excluded.failed_messages,source_folders=excluded.source_folders,destination_folders=excluded.destination_folders,authoritative=excluded.authoritative,missing_messages=excluded.missing_messages,extra_messages=excluded.extra_messages,modified_messages=excluded.modified_messages,probable_messages=excluded.probable_messages,captured_at=CURRENT_TIMESTAMP", params![job_id, value.verification_method().as_str(), value.verification_outcome().as_str(), source_messages, destination_messages, source_bytes, destination_bytes, unmatched_messages, failed_messages, source_folders, destination_folders, value.authoritative, missing_messages, extra_messages, modified_messages, probable_messages])?;
        tx.commit()?;
        Ok(())
    }
    #[cfg(test)]
    pub fn evidence(&self, job_id: &str) -> rusqlite::Result<Option<MailboxEvidence>> {
        self.connection.query_row("SELECT verification_method,verification_outcome,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,probable_messages FROM evidence WHERE job_id=?1", [job_id], |r| Ok(MailboxEvidence { verification_method: VerificationMethod::parse(&r.get::<_, String>(0)?).ok_or(rusqlite::Error::InvalidQuery)?, verification_outcome: Some(VerificationOutcome::parse(&r.get::<_, String>(1)?).ok_or(rusqlite::Error::InvalidQuery)?), source_messages: sqlite_u64(r.get(2)?)?, destination_messages: sqlite_u64(r.get(3)?)?, source_bytes: sqlite_u64(r.get(4)?)?, destination_bytes: sqlite_u64(r.get(5)?)?, unmatched_messages: sqlite_optional_u64(r.get(6)?)?, failed_messages: sqlite_u64(r.get(7)?)?, source_folders: sqlite_u64(r.get(8)?)?, destination_folders: sqlite_u64(r.get(9)?)?, authoritative:r.get::<_, i64>(10)? != 0, missing_messages: sqlite_u64(r.get(11)?)?, extra_messages: sqlite_u64(r.get(12)?)?, modified_messages: sqlite_u64(r.get(13)?)?, probable_messages: sqlite_u64(r.get(14)?)? })).optional()
    }
    #[cfg(test)]
    pub fn latest_evidence_for_run(
        &self,
        job_id: &str,
    ) -> rusqlite::Result<Option<(String, MailboxEvidence)>> {
        self.connection
            .query_row(
                "SELECT run_id,verification_method,verification_outcome,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages,probable_messages FROM evidence_history WHERE job_id=?1 ORDER BY captured_at DESC, id DESC LIMIT 1",
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
        let (total, verified): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN state IN ('verified','verified_with_exceptions') THEN 1 ELSE 0 END) FROM mailbox_jobs WHERE project_id=?1",
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
