use super::*;

impl StateStore {
    pub fn record_event(&self, project_id: &str, kind: &str, detail: &str) -> rusqlite::Result<()> {
        if kind == "run_output" {
            return Ok(());
        }
        self.event(project_id, kind, detail)
    }
    #[cfg(test)]
    pub fn record_events_batch(&self, events: &[(&str, &str, &str)]) -> rusqlite::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let tx = self.connection.unchecked_transaction()?;
        {
            let mut statement =
                tx.prepare_cached("INSERT INTO events(project_id,kind,detail) VALUES(?1,?2,?3)")?;
            for (project_id, kind, detail) in events {
                if *kind == "run_output" {
                    continue;
                }
                let detail = bounded_event_detail(detail);
                statement.execute(params![project_id, kind, detail])?;
            }
        }
        tx.commit()
    }

    /// Record diagnostic or verification events against the run that emitted
    /// them. The run supplies the project identity inside the same
    /// transaction, so callers cannot accidentally cross-wire a project and
    /// run while persisting asynchronous output.
    #[cfg(test)]
    pub fn record_run_events_batch(
        &self,
        run_id: &str,
        events: &[(&str, &str)],
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let (project_id, status): (String, String) = tx.query_row(
            "SELECT project_id,status FROM runs WHERE id=?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if status != "running" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        {
            let mut statement = tx.prepare_cached(
                "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,?3,?4)",
            )?;
            for (kind, detail) in events {
                if *kind == "run_output" {
                    continue;
                }
                let detail = bounded_event_detail(detail);
                statement.execute(params![project_id, run_id, kind, detail])?;
            }
        }
        tx.commit()
    }

    /// Record diagnostic events for several active or queued child runs in
    /// one transaction. A retry release can move a child back to `queued`
    /// before this buffered batch is flushed; terminal runs remain rejected.
    /// Each tuple is `(run_id, kind, detail)`; project ownership is resolved
    /// from the durable run rather than trusted from an asynchronous caller.
    pub fn record_events_for_runs_batch(
        &self,
        events: &[(&str, &str, &str)],
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let mut statement = tx.prepare_cached(
            "INSERT INTO events(project_id,run_id,kind,detail) SELECT project_id,?1,?2,?3 FROM runs WHERE id=?1 AND (status='running' OR (status='queued' AND parent_run_id IN (SELECT id FROM runs WHERE status='running'))) ",
        )?;
        for (run_id, kind, detail) in events {
            if *kind == "run_output" {
                let valid: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND (status='running' OR (status='queued' AND parent_run_id IN (SELECT id FROM runs WHERE status='running'))))",
                    [run_id],
                    |row| row.get(0),
                )?;
                if !valid {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                continue;
            }
            let changed = statement.execute(params![run_id, kind, bounded_event_detail(detail)])?;
            if changed != 1 {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        drop(statement);
        tx.commit()
    }
}
