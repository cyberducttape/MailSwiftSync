use super::*;

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

        let mut jobs = Vec::new();
        let mut attention_reasons = HashMap::new();
        let mut statement = tx.prepare(
            "SELECT id,source_mailbox,destination_mailbox,state,config,attention_reason FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid",
        )?;
        for row in statement.query_map([project_id], |row| {
            let raw_reason: Option<String> = row.get(5)?;
            Ok((
                MailboxJob {
                    id: row.get(0)?,
                    source_mailbox: row.get(1)?,
                    destination_mailbox: row.get(2)?,
                    state: row.get(3)?,
                    config: row.get(4)?,
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
            "SELECT va.job_id,va.run_id,va.operator,va.reason,va.accepted_at FROM verification_acceptances va JOIN mailbox_jobs j ON j.id=va.job_id WHERE j.project_id=?1 ORDER BY va.id ASC",
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
            "SELECT eh.job_id,eh.run_id,eh.verification_method,eh.verification_outcome,eh.source_messages,eh.destination_messages,eh.source_bytes,eh.destination_bytes,eh.unmatched_messages,eh.failed_messages,eh.source_folders,eh.destination_folders,eh.authoritative,eh.missing_messages,eh.extra_messages,eh.modified_messages,eh.probable_messages,r.plan_snapshot FROM evidence_history eh JOIN mailbox_jobs j ON j.id=eh.job_id LEFT JOIN runs r ON r.id=eh.run_id WHERE j.project_id=?1 ORDER BY eh.id ASC",
        )?;
        for row in evidence_statement.query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MailboxEvidence {
                    verification_method: VerificationMethod::parse(&row.get::<_, String>(2)?)
                        .unwrap_or(VerificationMethod::AggregateEngine),
                    verification_outcome: VerificationOutcome::parse(&row.get::<_, String>(3)?),
                    source_messages: row.get::<_, i64>(4)? as u64,
                    destination_messages: row.get::<_, i64>(5)? as u64,
                    source_bytes: row.get::<_, i64>(6)? as u64,
                    destination_bytes: row.get::<_, i64>(7)? as u64,
                    unmatched_messages: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                    failed_messages: row.get::<_, i64>(9)? as u64,
                    source_folders: row.get::<_, i64>(10)? as u64,
                    destination_folders: row.get::<_, i64>(11)? as u64,
                    authoritative: row.get::<_, i64>(12)? != 0,
                    missing_messages: row.get::<_, i64>(13)? as u64,
                    extra_messages: row.get::<_, i64>(14)? as u64,
                    modified_messages: row.get::<_, i64>(15)? as u64,
                    probable_messages: row.get::<_, i64>(16)? as u64,
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
            "SELECT r.id,r.job_id,r.parent_run_id,r.engine,r.phase_at_start,r.plan_snapshot,r.status,r.started_at,r.finished_at,r.detail,ev.version FROM runs r LEFT JOIN engine_versions ev ON ev.run_id=r.id WHERE r.project_id=?1 ORDER BY r.started_at ASC,r.rowid ASC",
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
            })
        })? {
            runs.push(row?);
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
        let snapshot = ProjectReportSnapshot {
            project,
            mailboxes,
            runs,
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
        offset: u32,
        limit: u32,
    ) -> rusqlite::Result<Vec<ReportMailboxSnapshot>> {
        let tx = self.connection.unchecked_transaction()?;
        let mut statement = tx.prepare(
            "SELECT j.id,j.source_mailbox,j.destination_mailbox,j.state,j.attention_reason,va.run_id,va.operator,va.reason,va.accepted_at,eh.run_id,eh.verification_method,eh.verification_outcome,eh.source_messages,eh.destination_messages,eh.source_bytes,eh.destination_bytes,eh.unmatched_messages,eh.failed_messages,eh.source_folders,eh.destination_folders,eh.authoritative,eh.missing_messages,eh.extra_messages,eh.modified_messages,eh.probable_messages FROM mailbox_jobs j LEFT JOIN verification_acceptances va ON va.id=(SELECT MAX(latest.id) FROM verification_acceptances latest WHERE latest.job_id=j.id) LEFT JOIN evidence_history eh ON eh.id=(SELECT MAX(latest.id) FROM evidence_history latest WHERE latest.job_id=j.id) WHERE j.project_id=?1 ORDER BY j.rowid LIMIT ?2 OFFSET ?3",
        )?;
        let rows = statement
            .query_map(rusqlite::params![project_id, limit, offset], |row| {
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
                                .unwrap_or(VerificationMethod::AggregateEngine),
                                verification_outcome: VerificationOutcome::parse(
                                    &row.get::<_, String>(11)?,
                                ),
                                source_messages: row.get::<_, i64>(12)? as u64,
                                destination_messages: row.get::<_, i64>(13)? as u64,
                                source_bytes: row.get::<_, i64>(14)? as u64,
                                destination_bytes: row.get::<_, i64>(15)? as u64,
                                unmatched_messages: row.get::<_, Option<i64>>(16)?.map(|v| v as u64),
                                failed_messages: row.get::<_, i64>(17)? as u64,
                                source_folders: row.get::<_, i64>(18)? as u64,
                                destination_folders: row.get::<_, i64>(19)? as u64,
                                authoritative: row.get::<_, i64>(20)? != 0,
                                missing_messages: row.get::<_, i64>(21)? as u64,
                                extra_messages: row.get::<_, i64>(22)? as u64,
                                modified_messages: row.get::<_, i64>(23)? as u64,
                                probable_messages: row.get::<_, i64>(24)? as u64,
                            },
                            None,
                        ))
                    })
                    .transpose()?;
                Ok(ReportMailboxSnapshot {
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
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        tx.commit()?;
        Ok(rows)
    }

    pub fn recent_run_list(
        &self,
        project_id: &str,
        limit: u32,
    ) -> rusqlite::Result<Vec<RunListItem>> {
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
