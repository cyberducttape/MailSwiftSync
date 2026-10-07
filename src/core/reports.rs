use super::*;

const MAX_REPORT_PAGE_ROWS: u32 = 1_000;
const MAX_REPORT_SNAPSHOT_MAILBOXES: i64 = 100_000;

impl StateStore {
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

        // Report exports assemble a complete project snapshot in memory so
        // that customer proofs and operator reports share one consistent
        // read model. Keep that explicit export boundary fail-closed rather
        // than allowing a pathological mailbox count to exhaust the
        // application process. Paged status/report views remain available
        // for larger projects.
        let mailbox_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM mailbox_jobs WHERE project_id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if mailbox_count > MAX_REPORT_SNAPSHOT_MAILBOXES {
            return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "project report contains {mailbox_count} mailboxes; report exports are limited to {MAX_REPORT_SNAPSHOT_MAILBOXES}"
                    ),
                ),
            )));
        }

        let mut jobs = Vec::new();
        let mut attention_reasons = HashMap::new();
        let mut statement = tx.prepare(
            "SELECT id,source_mailbox,destination_mailbox,state,attention_reason FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid",
        )?;
        for row in statement.query_map([project_id], |row| {
            let raw_reason: Option<String> = row.get(4)?;
            Ok((
                MailboxJob {
                    id: row.get(0)?,
                    source_mailbox: row.get(1)?,
                    destination_mailbox: row.get(2)?,
                    state: row.get(3)?,
                    config: None,
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
            "SELECT va.job_id,va.run_id,va.operator,va.reason,va.accepted_at FROM verification_acceptances va JOIN mailbox_jobs j ON j.id=va.job_id WHERE j.project_id=?1 AND va.id=(SELECT MAX(latest.id) FROM verification_acceptances latest WHERE latest.job_id=va.job_id)",
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
            "SELECT e.job_id,e.run_id,e.verification_method,e.verification_outcome,e.source_messages,e.destination_messages,e.source_bytes,e.destination_bytes,e.unmatched_messages,e.failed_messages,e.source_folders,e.destination_folders,e.authoritative,e.missing_messages,e.extra_messages,e.modified_messages,e.probable_messages,r.plan_snapshot,f.compared_messages,f.mismatched_messages,f.excepted_messages FROM evidence e JOIN mailbox_jobs j ON j.id=e.job_id JOIN runs r ON r.id=e.run_id LEFT JOIN evidence_flag_verification f ON f.job_id=e.job_id AND f.run_id=e.run_id WHERE j.project_id=?1",
        )?;
        for row in evidence_statement.query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MailboxEvidence {
                    verification_method: VerificationMethod::parse(&row.get::<_, String>(2)?)
                        .ok_or(rusqlite::Error::InvalidQuery)?,
                    verification_outcome: Some(
                        VerificationOutcome::parse(&row.get::<_, String>(3)?)
                            .ok_or(rusqlite::Error::InvalidQuery)?,
                    ),
                    source_messages: sqlite_u64(row.get(4)?)?,
                    destination_messages: sqlite_u64(row.get(5)?)?,
                    source_bytes: sqlite_u64(row.get(6)?)?,
                    destination_bytes: sqlite_u64(row.get(7)?)?,
                    unmatched_messages: sqlite_optional_u64(row.get(8)?)?,
                    failed_messages: sqlite_u64(row.get(9)?)?,
                    source_folders: sqlite_u64(row.get(10)?)?,
                    destination_folders: sqlite_u64(row.get(11)?)?,
                    authoritative: row.get::<_, i64>(12)? != 0,
                    missing_messages: sqlite_u64(row.get(13)?)?,
                    extra_messages: sqlite_u64(row.get(14)?)?,
                    modified_messages: sqlite_u64(row.get(15)?)?,
                    probable_messages: sqlite_u64(row.get(16)?)?,
                    flag_verification: super::evidence_ops::flag_verification_from_row(row, 18)?,
                },
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(17)?,
            ))
        })? {
            let (job_id, value, run_id, plan_snapshot) = row?;
            evidence.insert(job_id, (run_id, value, plan_snapshot));
        }

        let mut runs = Vec::new();
        let mut run_statement = tx.prepare(
            "SELECT r.id,r.job_id,r.parent_run_id,r.engine,r.phase_at_start,r.plan_snapshot,r.status,r.started_at,r.finished_at,r.detail,ev.version,(SELECT SUM(CAST(e.detail AS INTEGER)) FROM events e WHERE e.run_id=r.id AND e.kind='diagnostic_lines_dropped'),(SELECT COUNT(*) FROM events e WHERE e.run_id=r.id AND e.kind='transfer_attempt_started'),(SELECT COUNT(*) FROM events s WHERE s.run_id=r.id AND s.kind='transfer_attempt_started' AND NOT EXISTS(SELECT 1 FROM events f WHERE f.run_id=s.run_id AND f.kind='transfer_attempt_finished' AND f.detail LIKE s.detail || ';outcome=%')) FROM runs r LEFT JOIN engine_versions ev ON ev.run_id=r.id WHERE r.project_id=?1 ORDER BY r.started_at DESC,r.rowid DESC LIMIT 20",
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
                diagnostic_lines_dropped: row.get::<_, Option<i64>>(11)?.map(|count| count as u64),
                transfer_attempt_count: match row.get::<_, i64>(12)? {
                    0 => None,
                    count => Some(count as u64),
                },
                unfinished_transfer_attempt_count: row.get::<_, i64>(13)? as u64,
                transfer_passes: Vec::new(),
            })
        })? {
            runs.push(row?);
        }
        runs.reverse();
        for report_run in &mut runs {
            report_run.transfer_passes = self.transfer_passes(&report_run.run.id)?;
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
        let has_active_runs: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=?1 AND status IN ('queued','running'))",
            [project_id],
            |row| row.get(0),
        )?;
        let snapshot = ProjectReportSnapshot {
            project,
            mailboxes,
            runs,
            has_active_runs,
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
        after_rowid: Option<i64>,
        limit: u32,
    ) -> rusqlite::Result<ReportMailboxPage> {
        self.verification_rows_filtered(project_id, after_rowid, limit, None)
    }

    /// Load a cursor-paginated evidence page narrowed to one durable attention
    /// category. Unknown future wire values are grouped under `Unknown`.
    pub fn verification_rows_for_attention_reason(
        &self,
        project_id: &str,
        after_rowid: Option<i64>,
        limit: u32,
        reason: AttentionReason,
    ) -> rusqlite::Result<ReportMailboxPage> {
        self.verification_rows_filtered(project_id, after_rowid, limit, Some(reason))
    }

    fn verification_rows_filtered(
        &self,
        project_id: &str,
        after_rowid: Option<i64>,
        limit: u32,
        reason: Option<AttentionReason>,
    ) -> rusqlite::Result<ReportMailboxPage> {
        let limit = limit.min(MAX_REPORT_PAGE_ROWS);
        let tx = self.connection.unchecked_transaction()?;
        let mut statement = tx.prepare(
            "SELECT j.id,j.source_mailbox,j.destination_mailbox,j.state,j.attention_reason,va.run_id,va.operator,va.reason,va.accepted_at,e.run_id,e.verification_method,e.verification_outcome,e.source_messages,e.destination_messages,e.source_bytes,e.destination_bytes,e.unmatched_messages,e.failed_messages,e.source_folders,e.destination_folders,e.authoritative,e.missing_messages,e.extra_messages,e.modified_messages,e.probable_messages,j.rowid,f.compared_messages,f.mismatched_messages,f.excepted_messages FROM mailbox_jobs j LEFT JOIN verification_acceptances va ON va.id=(SELECT MAX(latest.id) FROM verification_acceptances latest WHERE latest.job_id=j.id) LEFT JOIN evidence e ON e.job_id=j.id LEFT JOIN evidence_flag_verification f ON f.job_id=e.job_id AND f.run_id=e.run_id WHERE j.project_id=?1 AND j.rowid>?2 AND (?3 IS NULL OR j.attention_reason=?3 OR (?3='unknown' AND j.attention_reason NOT IN ('interrupted','verification_incomplete','verification_limit_exceeded','verification_difference','process_identity_unverified','authentication_failed','transport_failed','policy_blocked','configuration_invalid','capacity_limited','message_rejected','unknown'))) ORDER BY j.rowid LIMIT ?4",
        )?;
        let rows = statement
            .query_map(
                rusqlite::params![
                    project_id,
                    after_rowid.unwrap_or(0),
                    reason.map(AttentionReason::as_str),
                    limit
                ],
                |row| {
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
                                    verification_method: VerificationMethod::parse(
                                        &row.get::<_, String>(10)?,
                                    )
                                    .ok_or(rusqlite::Error::InvalidQuery)?,
                                    verification_outcome: Some(
                                        VerificationOutcome::parse(&row.get::<_, String>(11)?)
                                            .ok_or(rusqlite::Error::InvalidQuery)?,
                                    ),
                                    source_messages: sqlite_u64(row.get(12)?)?,
                                    destination_messages: sqlite_u64(row.get(13)?)?,
                                    source_bytes: sqlite_u64(row.get(14)?)?,
                                    destination_bytes: sqlite_u64(row.get(15)?)?,
                                    unmatched_messages: sqlite_optional_u64(row.get(16)?)?,
                                    failed_messages: sqlite_u64(row.get(17)?)?,
                                    source_folders: sqlite_u64(row.get(18)?)?,
                                    destination_folders: sqlite_u64(row.get(19)?)?,
                                    authoritative: row.get::<_, i64>(20)? != 0,
                                    missing_messages: sqlite_u64(row.get(21)?)?,
                                    extra_messages: sqlite_u64(row.get(22)?)?,
                                    modified_messages: sqlite_u64(row.get(23)?)?,
                                    probable_messages: sqlite_u64(row.get(24)?)?,
                                    flag_verification:
                                        super::evidence_ops::flag_verification_from_row(row, 26)?,
                                },
                                None,
                            ))
                        })
                        .transpose()?;
                    Ok((
                        row.get::<_, i64>(25)?,
                        ReportMailboxSnapshot {
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
                        },
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        tx.commit()?;
        let first_rowid = rows.first().map(|(rowid, _)| *rowid);
        let last_rowid = rows.last().map(|(rowid, _)| *rowid);
        Ok(ReportMailboxPage {
            rows: rows.into_iter().map(|(_, row)| row).collect(),
            first_rowid,
            last_rowid,
        })
    }

    pub fn recent_run_list(
        &self,
        project_id: &str,
        limit: u32,
    ) -> rusqlite::Result<Vec<RunListItem>> {
        let limit = limit.min(MAX_REPORT_PAGE_ROWS);
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
}
