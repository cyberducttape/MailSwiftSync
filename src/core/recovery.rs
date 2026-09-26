use super::*;

impl StateStore {
    /// A desktop restart cannot prove that a previous child process still
    /// exists. All runs belonging to an interrupted wave are therefore made
    /// terminal: claimed children become abandoned and unclaimed children
    /// are also abandoned so their queued rows cannot wedge future retries.
    /// Only mailboxes that were actually claimed are moved to `attention`.
    #[cfg(test)]
    pub fn recover_abandoned_jobs(&self) -> rusqlite::Result<usize> {
        self.recover_abandoned_jobs_preserving(&[])
    }

    /// Recover interrupted work while retaining process identities that the
    /// caller could not verify. Those records are intentionally kept until an
    /// operator confirms that no migration engine remains, so a subsequent
    /// restart cannot silently forget the ownership uncertainty.
    pub fn recover_abandoned_jobs_preserving(
        &self,
        preserved_processes: &[ActiveProcess],
    ) -> rusqlite::Result<usize> {
        let projects = self
            .connection
            .prepare(
                "SELECT DISTINCT project_id FROM runs WHERE status='running' OR (status='queued' AND parent_run_id IN (SELECT id FROM runs WHERE status='running' AND job_id IS NULL))",
            )?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let tx = self.connection.unchecked_transaction()?;
        let count = tx.execute(
            "UPDATE mailbox_jobs SET state='attention',attention_reason='process_identity_unverified' WHERE state='running'",
            [],
        )?;
        tx.execute(
            "UPDATE runs SET status='abandoned',finished_at=CURRENT_TIMESTAMP,detail='Application restarted before completion' WHERE status='running'",
            [],
        )?;
        tx.execute(
            "UPDATE runs SET status='abandoned',finished_at=CURRENT_TIMESTAMP,detail='Application restarted before batch child was claimed' WHERE status='queued' AND parent_run_id IN (SELECT id FROM runs WHERE status='abandoned' AND job_id IS NULL AND detail='Application restarted before completion')",
            [],
        )?;
        tx.execute("DELETE FROM active_processes", [])?;
        for process in preserved_processes {
            tx.execute(
                "INSERT INTO active_processes(run_id,job_id,pid,start_ticks,process_group,session_id,executable) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    process.run_id,
                    process.job_id,
                    i64::from(process.pid),
                    sqlite_optional_i64(process.start_ticks)?,
                    process.process_group.map(i64::from),
                    process.session_id,
                    process.executable,
                ],
            )?;
        }
        for project in projects {
            tx.execute(
                "INSERT INTO events(project_id,kind,detail) VALUES(?1,'run_recovered','Interrupted batch runs and claimed mailbox jobs were recovered; unclaimed child runs were abandoned')",
                [project],
            )?;
        }
        tx.commit()?;
        Ok(count)
    }

    /// Clear process identities only after an explicit operator review has
    /// established that no untracked migration child remains on the host.
    pub fn clear_active_processes_after_review(&self) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM active_processes", [])?;
        Ok(())
    }

    /// Record the OS process belonging to a durable run. Startup reconciliation
    /// uses this identity to terminate a recorded orphan before allowing an
    /// operator to retry the mailbox.
    pub fn register_process(&self, process: &ActiveProcess) -> rusqlite::Result<()> {
        // Validate ownership and insert the identity in one transaction. A
        // worker can arrive late while the UI is finishing a run; a
        // separate SELECT followed by INSERT would let that stale worker
        // recreate an active-process row after terminal cleanup.
        let tx = self.connection.unchecked_transaction()?;
        let consistent: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs r JOIN mailbox_jobs j ON j.id=?2 AND r.project_id=j.project_id WHERE r.id=?1 AND r.status='running' AND r.job_id=j.id)",
            params![process.run_id, process.job_id],
            |row| row.get(0),
        )?;
        if !consistent {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "INSERT INTO active_processes(run_id,job_id,pid,start_ticks,process_group,session_id,executable) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(run_id,job_id) DO UPDATE SET pid=excluded.pid,start_ticks=excluded.start_ticks,process_group=excluded.process_group,session_id=excluded.session_id,executable=excluded.executable,started_at=CURRENT_TIMESTAMP",
            params![
                process.run_id,
                process.job_id,
                i64::from(process.pid),
                sqlite_optional_i64(process.start_ticks)?,
                process.process_group.map(i64::from),
                process.session_id.map(i64::from),
                process.executable
            ],
        )?;
        tx.commit()
    }

    pub fn active_processes(&self) -> rusqlite::Result<Vec<ActiveProcess>> {
        self.connection
            .prepare("SELECT run_id,job_id,pid,start_ticks,process_group,session_id,executable FROM active_processes")?
            .query_map([], |row| {
                let pid: i64 = row.get(2)?;
                let pid = u32::try_from(pid)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, pid))?;
                let start_ticks: Option<i64> = row.get(3)?;
                let process_group: Option<i64> = row.get(4)?;
                let session_id: Option<i64> = row.get(5)?;
                Ok(ActiveProcess {
                    run_id: row.get(0)?,
                    job_id: row.get(1)?,
                    pid,
                    start_ticks: sqlite_optional_u64(start_ticks)?,
                    process_group: process_group
                        .map(|value| u32::try_from(value).map_err(|_| rusqlite::Error::InvalidQuery))
                        .transpose()?,
                    session_id: session_id
                        .map(|value| u32::try_from(value).map_err(|_| rusqlite::Error::InvalidQuery))
                        .transpose()?,
                    executable: row.get(6)?,
                })
            })?
            .collect()
    }

    pub fn clear_processes(&self, run_id: &str) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM active_processes WHERE run_id=?1", [run_id])?;
        Ok(())
    }
}
