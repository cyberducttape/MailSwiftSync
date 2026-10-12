//! Streamed read model for complete report exports.
//!
//! A customer proof or project JSON export must cover every mailbox and every
//! run of a project, which at fleet scale is far more than fits comfortably in
//! memory (each evidence row carries its run's plan snapshot). The reader pins
//! one SQLite read transaction, so every pass an exporter makes over the rows
//! (an aggregate pass, the digest pass, and the file pass) sees the same
//! commit, while only one row is resident at a time.

use super::*;

/// Runs are paired with their transfer-pass provenance in batches of this
/// size, which bounds both memory and the `IN (...)` parameter count.
const RUN_PASS_BATCH: usize = 400;

/// One mailbox with its latest acceptance, current evidence, the evidence
/// run's plan snapshot, and flag coverage; decoded by `export_mailbox_from_row`.
const MAILBOX_EXPORT_SELECT: &str = "SELECT j.id,j.source_mailbox,j.destination_mailbox,j.state,j.attention_reason,va.run_id,va.operator,va.reason,va.accepted_at,e.run_id,e.verification_method,e.verification_outcome,e.source_messages,e.destination_messages,e.source_bytes,e.destination_bytes,e.unmatched_messages,e.failed_messages,e.source_folders,e.destination_folders,e.authoritative,e.missing_messages,e.extra_messages,e.modified_messages,e.probable_messages,e.rfc_message_id_matches,e.synthetic_identity_matches,r.id,r.plan_snapshot,f.compared_messages,f.mismatched_messages,f.excepted_messages FROM mailbox_jobs j LEFT JOIN verification_acceptances va ON va.id=(SELECT MAX(latest.id) FROM verification_acceptances latest WHERE latest.job_id=j.id) LEFT JOIN evidence e ON e.job_id=j.id LEFT JOIN runs r ON r.id=e.run_id LEFT JOIN evidence_flag_verification f ON f.job_id=e.job_id AND f.run_id=e.run_id";

pub struct ProjectExportReader<'a> {
    store: &'a StateStore,
    tx: rusqlite::Transaction<'a>,
    project: Project,
    has_active_runs: bool,
}

impl StateStore {
    /// Open a consistent read snapshot of one project for a streamed export.
    /// Returns `None` when the project does not exist.
    pub fn project_export_reader(
        &self,
        project_id: &str,
    ) -> rusqlite::Result<Option<ProjectExportReader<'_>>> {
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
            .optional()?
        else {
            return Ok(None);
        };
        let has_active_runs = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=?1 AND status IN ('queued','running'))",
            [project_id],
            |row| row.get(0),
        )?;
        Ok(Some(ProjectExportReader {
            store: self,
            tx,
            project,
            has_active_runs,
        }))
    }
}

impl ProjectExportReader<'_> {
    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Whether any run of the project, not only a recent one, is queued or
    /// running in this snapshot.
    pub fn has_active_runs(&self) -> bool {
        self.has_active_runs
    }

    pub fn has_mailboxes(&self) -> Result<bool, String> {
        self.tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE project_id=?1)",
                [&self.project.id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())
    }

    /// One mailbox of this project with its acceptance and evidence.
    pub fn mailbox(&self, job_id: &str) -> Result<Option<ReportMailboxSnapshot>, String> {
        self.tx
            .query_row(
                &format!("{MAILBOX_EXPORT_SELECT} WHERE j.project_id=?1 AND j.id=?2"),
                [&self.project.id, job_id],
                export_mailbox_from_row,
            )
            .optional()
            .map_err(|error| error.to_string())
    }

    /// One run of this project, wherever it falls in the run history.
    pub fn run(&self, run_id: &str) -> Result<Option<RunSummary>, String> {
        self.tx
            .query_row(
                "SELECT id,job_id,parent_run_id,engine,phase_at_start,plan_snapshot,status,started_at,finished_at,detail FROM runs WHERE project_id=?1 AND id=?2",
                [&self.project.id, run_id],
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
            .map_err(|error| error.to_string())
    }

    /// Visit every mailbox in insertion order with its latest acceptance and
    /// current evidence. Evidence whose run row no longer exists is reported
    /// as absent, matching the in-memory report snapshot.
    pub fn for_each_mailbox(
        &self,
        mut visit: impl FnMut(ReportMailboxSnapshot) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut statement = self
            .tx
            .prepare_cached(&format!(
                "{MAILBOX_EXPORT_SELECT} WHERE j.project_id=?1 ORDER BY j.rowid"
            ))
            .map_err(|error| error.to_string())?;
        let mut rows = statement
            .query([&self.project.id])
            .map_err(|error| error.to_string())?;
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            let mailbox = export_mailbox_from_row(row).map_err(|error| error.to_string())?;
            visit(mailbox)?;
        }
        Ok(())
    }

    /// Visit every run in start order with its engine version, attempt
    /// accounting, and transfer-pass provenance.
    pub fn for_each_run(
        &self,
        mut visit: impl FnMut(ReportRunSnapshot) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut statement = self
            .tx
            .prepare_cached(
                "SELECT r.id,r.job_id,r.parent_run_id,r.engine,r.phase_at_start,r.plan_snapshot,r.status,r.started_at,r.finished_at,r.detail,ev.version,(SELECT SUM(CAST(e.detail AS INTEGER)) FROM events e WHERE e.run_id=r.id AND e.kind='diagnostic_lines_dropped'),(SELECT COUNT(*) FROM events e WHERE e.run_id=r.id AND e.kind='transfer_attempt_started'),(SELECT COUNT(*) FROM events s WHERE s.run_id=r.id AND s.kind='transfer_attempt_started' AND NOT EXISTS(SELECT 1 FROM events f WHERE f.run_id=s.run_id AND f.kind='transfer_attempt_finished' AND f.detail LIKE s.detail || ';outcome=%')) FROM runs r LEFT JOIN engine_versions ev ON ev.run_id=r.id WHERE r.project_id=?1 ORDER BY r.started_at,r.rowid",
            )
            .map_err(|error| error.to_string())?;
        let mut rows = statement
            .query([&self.project.id])
            .map_err(|error| error.to_string())?;
        let mut batch = Vec::with_capacity(RUN_PASS_BATCH);
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            batch.push(report_run_from_row(row).map_err(|error| error.to_string())?);
            if batch.len() == RUN_PASS_BATCH {
                self.emit_run_batch(&mut batch, &mut visit)?;
            }
        }
        self.emit_run_batch(&mut batch, &mut visit)
    }

    fn emit_run_batch(
        &self,
        batch: &mut Vec<ReportRunSnapshot>,
        visit: &mut impl FnMut(ReportRunSnapshot) -> Result<(), String>,
    ) -> Result<(), String> {
        if batch.is_empty() {
            return Ok(());
        }
        let run_ids = batch
            .iter()
            .map(|report_run| report_run.run.id.clone())
            .collect::<Vec<_>>();
        // Same connection, so this read stays inside the pinned transaction.
        let mut passes = self
            .store
            .transfer_passes_for_runs(&run_ids)
            .map_err(|error| error.to_string())?;
        for mut report_run in batch.drain(..) {
            report_run.transfer_passes = passes.remove(&report_run.run.id).unwrap_or_default();
            visit(report_run)?;
        }
        Ok(())
    }
}

pub(super) fn report_run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReportRunSnapshot> {
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
}

fn export_mailbox_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReportMailboxSnapshot> {
    let acceptance = row
        .get::<_, Option<String>>(5)?
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
    // Evidence is only reported with its run row present (r.id).
    let evidence = match (
        row.get::<_, Option<String>>(9)?,
        row.get::<_, Option<String>>(27)?,
    ) {
        (Some(run_id), Some(_)) => Some((
            run_id,
            MailboxEvidence {
                verification_method: VerificationMethod::parse(&row.get::<_, String>(10)?)
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
                flag_verification: super::evidence_ops::flag_verification_from_row(row, 29)?,
            },
            row.get::<_, Option<String>>(28)?,
        )),
        _ => None,
    };
    Ok(ReportMailboxSnapshot {
        job: MailboxJob {
            id: row.get(0)?,
            source_mailbox: row.get(1)?,
            destination_mailbox: row.get(2)?,
            state: row.get(3)?,
            config: None,
        },
        attention_reason: row
            .get::<_, Option<String>>(4)?
            .map(|reason| AttentionReason::parse(&reason).unwrap_or(AttentionReason::Unknown)),
        acceptance,
        evidence,
    })
}
