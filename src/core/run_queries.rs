use super::*;

impl StateStore {
    #[cfg(test)]
    pub fn run_status(&self, run_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row("SELECT status FROM runs WHERE id=?1", [run_id], |row| {
                row.get(0)
            })
            .optional()
    }
    #[cfg(test)]
    pub fn latest_run(&self, job_id: &str) -> rusqlite::Result<Option<RunSummary>> {
        self.connection
            .query_row(
                "SELECT id,job_id,parent_run_id,engine,phase_at_start,plan_snapshot,status,started_at,finished_at,detail FROM runs WHERE job_id=?1 ORDER BY started_at DESC, rowid DESC LIMIT 1",
                [job_id],
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
    }
    #[cfg(test)]
    pub fn run(&self, run_id: &str) -> rusqlite::Result<Option<RunSummary>> {
        self.connection
            .query_row(
                "SELECT id,job_id,parent_run_id,engine,phase_at_start,plan_snapshot,status,started_at,finished_at,detail FROM runs WHERE id=?1",
                [run_id],
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
    }
    #[cfg(test)]
    pub fn recent_runs(&self, project_id: &str, limit: u32) -> rusqlite::Result<Vec<RunSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT id,job_id,parent_run_id,engine,phase_at_start,plan_snapshot,status,started_at,finished_at,detail FROM runs WHERE project_id=?1 ORDER BY started_at DESC, rowid DESC LIMIT ?2",
        )?;
        statement
            .query_map(params![project_id, limit], |row| {
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
            })?
            .collect()
    }
}
