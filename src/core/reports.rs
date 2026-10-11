use super::*;

const MAX_REPORT_PAGE_ROWS: u32 = 1_000;
const REPORT_EXPORT_MAILBOX_PAGE_ROWS: u32 = 100;
const REPORT_EXPORT_MAILBOX_PAGE_PLAN_BUDGET: i64 = 4 * 1024 * 1024;
const REPORT_EXPORT_RUN_PAGE_ROWS: usize = 4;
const MAX_REPORT_SNAPSHOT_MAILBOXES: i64 = 100_000;
const MAX_REPORT_SNAPSHOT_RUNS: i64 = 100_000;
const MAX_REPORT_EXPORT_TRANSFER_PASSES: i64 = 100_000;
const MAX_REPORT_EXPORT_TRANSFER_FOLDERS: i64 = 100_000;
const MAX_REPORT_EXPORT_COMMAND_BYTES: i64 = 16 * 1024 * 1024;

/// A consistent project read snapshot whose large collections are loaded in
/// bounded pages. Export writers may traverse it more than once (for digest
/// generation and output) without mixing database revisions.
pub(crate) struct ProjectReportExport<'a> {
    tx: rusqlite::Transaction<'a>,
    project: Project,
    mailbox_count: u64,
    has_active_runs: bool,
}

impl ProjectReportExport<'_> {
    pub(crate) fn project(&self) -> &Project {
        &self.project
    }

    pub(crate) fn mailbox_count(&self) -> u64 {
        self.mailbox_count
    }

    pub(crate) fn has_active_runs(&self) -> bool {
        self.has_active_runs
    }

    pub(crate) fn for_each_mailbox(
        &self,
        mut emit: impl FnMut(ReportMailboxSnapshot) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut cursor = None;
        loop {
            let page_limit = report_mailbox_export_page_limit(&self.tx, &self.project.id, cursor)
                .map_err(|error| error.to_string())?;
            let page = verification_rows_filtered_in_tx(
                &self.tx,
                &self.project.id,
                cursor,
                page_limit,
                None,
                true,
            )
            .map_err(|error| error.to_string())?;
            if page.rows.is_empty() {
                break;
            }
            cursor = page.last_rowid;
            for mailbox in page.rows {
                emit(mailbox)?;
            }
        }
        Ok(())
    }

    pub(crate) fn for_each_run(
        &self,
        mut emit: impl FnMut(ReportRunSnapshot) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut cursor: Option<(String, i64)> = None;
        loop {
            let (cursor_started_at, cursor_rowid) =
                cursor.as_ref().map_or((None, 0), |(started_at, rowid)| {
                    (Some(started_at.as_str()), *rowid)
                });
            let mut statement = self.tx.prepare(
                "SELECT r.id,r.job_id,r.parent_run_id,r.engine,r.phase_at_start,r.plan_snapshot,r.status,r.started_at,r.finished_at,r.detail,ev.version,(SELECT SUM(CAST(e.detail AS INTEGER)) FROM events e WHERE e.run_id=r.id AND e.kind='diagnostic_lines_dropped'),(SELECT COUNT(*) FROM events e WHERE e.run_id=r.id AND e.kind='transfer_attempt_started'),(SELECT COUNT(*) FROM events s WHERE s.run_id=r.id AND s.kind='transfer_attempt_started' AND NOT EXISTS(SELECT 1 FROM events f WHERE f.run_id=s.run_id AND f.kind='transfer_attempt_finished' AND f.detail LIKE s.detail || ';outcome=%')),r.rowid FROM runs r LEFT JOIN engine_versions ev ON ev.run_id=r.id WHERE r.project_id=?1 AND (?2 IS NULL OR r.started_at>?2 OR (r.started_at=?2 AND r.rowid>?3)) ORDER BY r.started_at,r.rowid LIMIT ?4",
            ).map_err(|error| error.to_string())?;
            let rows = statement
                .query_map(
                    rusqlite::params![
                        self.project.id,
                        cursor_started_at,
                        cursor_rowid,
                        REPORT_EXPORT_RUN_PAGE_ROWS as i64
                    ],
                    |row| {
                        Ok((
                            row.get::<_, i64>(14)?,
                            ReportRunSnapshot {
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
                                diagnostic_lines_dropped: row
                                    .get::<_, Option<i64>>(11)?
                                    .map(|count| count as u64),
                                transfer_attempt_count: match row.get::<_, i64>(12)? {
                                    0 => None,
                                    count => Some(count as u64),
                                },
                                unfinished_transfer_attempt_count: row.get::<_, i64>(13)? as u64,
                                transfer_passes: Vec::new(),
                            },
                        ))
                    },
                )
                .map_err(|error| error.to_string())?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|error| error.to_string())?;
            drop(statement);
            if rows.is_empty() {
                break;
            }
            let mut rows = rows;
            let run_ids = rows
                .iter()
                .map(|(_, run)| run.run.id.clone())
                .collect::<Vec<_>>();
            let mut passes =
                super::transfer_passes::transfer_passes_for_runs_connection(&self.tx, &run_ids)
                    .map_err(|error| error.to_string())?;
            for (_, run) in &mut rows {
                run.transfer_passes = passes.remove(&run.run.id).unwrap_or_default();
            }
            let (rowid, last_run) = rows.last().expect("non-empty page");
            cursor = Some((last_run.run.started_at.clone(), *rowid));
            for (_, run) in rows {
                emit(run)?;
            }
        }
        Ok(())
    }
}

impl StateStore {
    pub(crate) fn begin_project_report_export(
        &self,
        project_id: &str,
    ) -> rusqlite::Result<Option<ProjectReportExport<'_>>> {
        let tx = self.connection.unchecked_transaction()?;
        let project = tx
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
            .optional()?;
        let Some(project) = project else {
            tx.commit()?;
            return Ok(None);
        };
        let mailbox_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM mailbox_jobs WHERE project_id=?1",
            [&project.id],
            |row| row.get(0),
        )?;
        if mailbox_count > MAX_REPORT_SNAPSHOT_MAILBOXES {
            return Err(report_limit_error(format!(
                "project report contains {mailbox_count} mailboxes; report exports are limited to {MAX_REPORT_SNAPSHOT_MAILBOXES}"
            )));
        }
        let run_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM runs WHERE project_id=?1",
            [&project.id],
            |row| row.get(0),
        )?;
        if run_count > MAX_REPORT_SNAPSHOT_RUNS {
            return Err(report_limit_error(format!(
                "project report contains {run_count} runs; complete report exports are limited to {MAX_REPORT_SNAPSHOT_RUNS}"
            )));
        }
        let has_active_runs: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=?1 AND status IN ('queued','running'))",
            [&project.id],
            |row| row.get(0),
        )?;
        let transfer_pass_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM transfer_passes WHERE project_id=?1",
            [&project.id],
            |row| row.get(0),
        )?;
        if transfer_pass_count > MAX_REPORT_EXPORT_TRANSFER_PASSES {
            return Err(report_limit_error(format!(
                "project report contains {transfer_pass_count} transfer attempts; report exports are limited to {MAX_REPORT_EXPORT_TRANSFER_PASSES}"
            )));
        }
        let transfer_folder_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM transfer_pass_folders f JOIN transfer_passes p ON p.run_id=f.run_id AND p.attempt=f.attempt WHERE p.project_id=?1",
            [&project.id],
            |row| row.get(0),
        )?;
        if transfer_folder_count > MAX_REPORT_EXPORT_TRANSFER_FOLDERS {
            return Err(report_limit_error(format!(
                "project report contains {transfer_folder_count} transfer-pass folder records; report exports are limited to {MAX_REPORT_EXPORT_TRANSFER_FOLDERS}"
            )));
        }
        let transfer_command_bytes: i64 = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(command AS BLOB))),0) FROM transfer_passes WHERE project_id=?1",
            [&project.id],
            |row| row.get(0),
        )?;
        if transfer_command_bytes > MAX_REPORT_EXPORT_COMMAND_BYTES {
            return Err(report_limit_error(format!(
                "project report contains {transfer_command_bytes} bytes of transfer command provenance; report exports are limited to {MAX_REPORT_EXPORT_COMMAND_BYTES} bytes"
            )));
        }
        Ok(Some(ProjectReportExport {
            tx,
            project,
            mailbox_count: mailbox_count as u64,
            has_active_runs,
        }))
    }
}

fn report_limit_error(message: String) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message,
    )))
}

fn report_mailbox_export_page_limit(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    after_rowid: Option<i64>,
) -> rusqlite::Result<u32> {
    let mut limit = REPORT_EXPORT_MAILBOX_PAGE_ROWS;
    for _ in 0..8 {
        let snapshot_bytes: i64 = tx.query_row(
            "SELECT COALESCE(SUM(length(CAST(r.plan_snapshot AS BLOB))),0) FROM (SELECT id FROM mailbox_jobs WHERE project_id=?1 AND rowid>?2 ORDER BY rowid LIMIT ?3) page JOIN evidence e ON e.job_id=page.id JOIN runs r ON r.id=e.run_id",
            rusqlite::params![project_id, after_rowid.unwrap_or(0), limit],
            |row| row.get(0),
        )?;
        if snapshot_bytes <= REPORT_EXPORT_MAILBOX_PAGE_PLAN_BUDGET || limit == 1 {
            return Ok(limit);
        }
        let proportion = (REPORT_EXPORT_MAILBOX_PAGE_PLAN_BUDGET as u128)
            .saturating_mul(limit as u128)
            / snapshot_bytes as u128;
        limit = u32::try_from(proportion.max(1)).unwrap_or(1).min(limit - 1);
    }
    // If unusually skewed snapshot sizes prevent convergence, prefer a
    // single mailbox row over returning a page above the byte budget.
    Ok(1)
}

impl StateStore {
    pub fn project_report_snapshot(
        &self,
        project_id: &str,
    ) -> rusqlite::Result<Option<ProjectReportSnapshot>> {
        self.project_report_snapshot_with_run_limit(project_id, 20)
    }

    fn project_report_snapshot_with_run_limit(
        &self,
        project_id: &str,
        run_limit: i64,
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
            "SELECT e.job_id,e.run_id,e.verification_method,e.verification_outcome,e.source_messages,e.destination_messages,e.source_bytes,e.destination_bytes,e.unmatched_messages,e.failed_messages,e.source_folders,e.destination_folders,e.authoritative,e.missing_messages,e.extra_messages,e.modified_messages,e.probable_messages,e.rfc_message_id_matches,e.synthetic_identity_matches,r.plan_snapshot,f.compared_messages,f.mismatched_messages,f.excepted_messages FROM evidence e JOIN mailbox_jobs j ON j.id=e.job_id JOIN runs r ON r.id=e.run_id LEFT JOIN evidence_flag_verification f ON f.job_id=e.job_id AND f.run_id=e.run_id WHERE j.project_id=?1",
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
                    rfc_message_id_matches: sqlite_u64(row.get(17)?)?,
                    synthetic_identity_matches: sqlite_u64(row.get(18)?)?,
                    flag_verification: super::evidence_ops::flag_verification_from_row(row, 20)?,
                },
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(19)?,
            ))
        })? {
            let (job_id, value, run_id, plan_snapshot) = row?;
            evidence.insert(job_id, (run_id, value, plan_snapshot));
        }

        let mut runs = Vec::new();
        let mut run_statement = tx.prepare(
            "SELECT r.id,r.job_id,r.parent_run_id,r.engine,r.phase_at_start,r.plan_snapshot,r.status,r.started_at,r.finished_at,r.detail,ev.version,(SELECT SUM(CAST(e.detail AS INTEGER)) FROM events e WHERE e.run_id=r.id AND e.kind='diagnostic_lines_dropped'),(SELECT COUNT(*) FROM events e WHERE e.run_id=r.id AND e.kind='transfer_attempt_started'),(SELECT COUNT(*) FROM events s WHERE s.run_id=r.id AND s.kind='transfer_attempt_started' AND NOT EXISTS(SELECT 1 FROM events f WHERE f.run_id=s.run_id AND f.kind='transfer_attempt_finished' AND f.detail LIKE s.detail || ';outcome=%')) FROM runs r LEFT JOIN engine_versions ev ON ev.run_id=r.id WHERE r.project_id=?1 ORDER BY r.started_at DESC,r.rowid DESC LIMIT ?2",
        )?;
        for row in run_statement.query_map(rusqlite::params![project_id, run_limit], |row| {
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
        let run_ids = runs
            .iter()
            .map(|report_run| report_run.run.id.clone())
            .collect::<Vec<_>>();
        let mut transfer_passes = self.transfer_passes_for_runs(&run_ids)?;
        for report_run in &mut runs {
            report_run.transfer_passes = transfer_passes
                .remove(&report_run.run.id)
                .unwrap_or_default();
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
        let tx = self.connection.unchecked_transaction()?;
        let page =
            verification_rows_filtered_in_tx(&tx, project_id, after_rowid, limit, reason, false)?;
        tx.commit()?;
        Ok(page)
    }
}

fn verification_rows_filtered_in_tx(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    after_rowid: Option<i64>,
    limit: u32,
    reason: Option<AttentionReason>,
    include_plan_snapshot: bool,
) -> rusqlite::Result<ReportMailboxPage> {
    let limit = limit.min(MAX_REPORT_PAGE_ROWS);
    let mut statement = tx.prepare(
        "SELECT j.id,j.source_mailbox,j.destination_mailbox,j.state,j.attention_reason,va.run_id,va.operator,va.reason,va.accepted_at,e.run_id,e.verification_method,e.verification_outcome,e.source_messages,e.destination_messages,e.source_bytes,e.destination_bytes,e.unmatched_messages,e.failed_messages,e.source_folders,e.destination_folders,e.authoritative,e.missing_messages,e.extra_messages,e.modified_messages,e.probable_messages,e.rfc_message_id_matches,e.synthetic_identity_matches,j.rowid,f.compared_messages,f.mismatched_messages,f.excepted_messages,CASE WHEN ?5 THEN er.plan_snapshot END FROM mailbox_jobs j LEFT JOIN verification_acceptances va ON va.id=(SELECT MAX(latest.id) FROM verification_acceptances latest WHERE latest.job_id=j.id) LEFT JOIN evidence e ON e.job_id=j.id LEFT JOIN runs er ON er.id=e.run_id LEFT JOIN evidence_flag_verification f ON f.job_id=e.job_id AND f.run_id=e.run_id WHERE j.project_id=?1 AND j.rowid>?2 AND (?3 IS NULL OR j.attention_reason=?3 OR (?3='unknown' AND j.attention_reason NOT IN ('interrupted','verification_incomplete','verification_limit_exceeded','verification_difference','process_identity_unverified','authentication_failed','transport_failed','policy_blocked','configuration_invalid','capacity_limited','message_rejected','unknown'))) ORDER BY j.rowid LIMIT ?4",
        )?;
    let rows = statement
        .query_map(
            rusqlite::params![
                project_id,
                after_rowid.unwrap_or(0),
                reason.map(AttentionReason::as_str),
                limit,
                include_plan_snapshot,
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
                let evidence =
                    row.get::<_, Option<String>>(9)?
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
                                    rfc_message_id_matches: sqlite_u64(row.get(25)?)?,
                                    synthetic_identity_matches: sqlite_u64(row.get(26)?)?,
                                    flag_verification:
                                        super::evidence_ops::flag_verification_from_row(row, 28)?,
                                },
                                row.get::<_, Option<String>>(31)?,
                            ))
                        })
                        .transpose()?;
                Ok((
                    row.get::<_, i64>(27)?,
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
    let first_rowid = rows.first().map(|(rowid, _)| *rowid);
    let last_rowid = rows.last().map(|(rowid, _)| *rowid);
    Ok(ReportMailboxPage {
        rows: rows.into_iter().map(|(_, row)| row).collect(),
        first_rowid,
        last_rowid,
    })
}

impl StateStore {
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

#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn report_export_pages_remain_on_one_snapshot_during_concurrent_commits() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let state = directory.join("state.db");
        let writer = StateStore::open(&state).unwrap();
        let project = writer
            .create_project("streamed-export", "source", "destination")
            .unwrap();
        let plan_snapshot = crate::Form::default().plan_snapshot();
        let first_job = writer
            .add_mailbox(&project.id, "first-source", "first-destination")
            .unwrap();
        writer
            .begin_run_with_snapshot(&project.id, &first_job, "first-run", "test", &plan_snapshot)
            .unwrap();
        writer
            .finish_run_for_mailbox_with_evidence(
                &project.id,
                &first_job,
                "first-run",
                "completed",
                "verification_difference",
                "export snapshot fixture",
                &MailboxEvidence {
                    verification_method: VerificationMethod::MetadataReconciliation,
                    verification_outcome: Some(VerificationOutcome::Missing),
                    source_messages: 1,
                    destination_messages: 0,
                    source_bytes: 1,
                    destination_bytes: 0,
                    unmatched_messages: Some(1),
                    failed_messages: 0,
                    source_folders: 0,
                    destination_folders: 0,
                    authoritative: false,
                    missing_messages: 1,
                    extra_messages: 0,
                    modified_messages: 0,
                    probable_messages: 0,
                    rfc_message_id_matches: 0,
                    synthetic_identity_matches: 0,
                    flag_verification: None,
                },
            )
            .unwrap();
        let reader = StateStore::open(&state).unwrap();
        let compact_page = reader.verification_rows(&project.id, None, 10).unwrap();
        assert!(compact_page.rows[0].evidence.as_ref().unwrap().2.is_none());
        let export = reader
            .begin_project_report_export(&project.id)
            .unwrap()
            .unwrap();
        assert_eq!(export.mailbox_count(), 1);
        let mut exported_plan = None;
        export
            .for_each_mailbox(|mailbox| {
                exported_plan = mailbox.evidence.and_then(|(_, _, plan)| plan);
                Ok(())
            })
            .unwrap();
        assert_eq!(exported_plan.as_deref(), Some(plan_snapshot.as_str()));

        let second_job = writer
            .add_mailbox(&project.id, "second-source", "second-destination")
            .unwrap();
        writer
            .begin_run_with_snapshot(&project.id, &second_job, "second-run", "test", "")
            .unwrap();

        let mut mailbox_ids = Vec::new();
        export
            .for_each_mailbox(|mailbox| {
                mailbox_ids.push(mailbox.job.id);
                Ok(())
            })
            .unwrap();
        let mut run_ids = Vec::new();
        export
            .for_each_run(|run| {
                run_ids.push(run.run.id);
                Ok(())
            })
            .unwrap();
        assert_eq!(mailbox_ids, vec![first_job]);
        assert_eq!(run_ids, vec!["first-run"]);

        drop(export);
        let latest = reader
            .begin_project_report_export(&project.id)
            .unwrap()
            .unwrap();
        assert_eq!(latest.mailbox_count(), 2);
        assert!(latest.has_active_runs());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
