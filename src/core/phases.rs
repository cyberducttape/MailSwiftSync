use super::policy::phase_rank;
use super::*;

impl StateStore {
    pub fn transition(&self, id: &str, phase: Phase) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let current_name: String =
            tx.query_row("SELECT phase FROM projects WHERE id=?1", [id], |row| {
                row.get(0)
            })?;
        let current = Phase::parse(&current_name)?;
        let backwards_transition = current != Phase::Attention
            && phase != Phase::Attention
            && phase_rank(phase) < phase_rank(current);
        if current != phase
            && (current == Phase::Complete
                || (current == Phase::Attention
                    && !matches!(phase, Phase::Preflight | Phase::Verification))
                || backwards_transition)
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if phase == Phase::Verification && current == Phase::Discovery {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if phase == Phase::Complete && current != phase {
            let total: i64 = tx.query_row(
                "SELECT COUNT(*) FROM mailbox_jobs WHERE project_id=?1",
                [id],
                |row| row.get(0),
            )?;
            let verified: i64 = tx.query_row(
                "SELECT COUNT(*) FROM mailbox_jobs WHERE project_id=?1 AND state IN ('verified','verified_with_exceptions')",
                [id],
                |row| row.get(0),
            )?;
            if total == 0 || total != verified || current != Phase::Verification {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        let changed = tx.execute(
            "UPDATE projects SET phase=?1 WHERE id=?2 AND phase=?3",
            params![phase.as_str(), id, current.as_str()],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'phase_changed',?2)",
            params![id, phase.as_str()],
        )?;
        tx.commit()
    }

    /// Reopen a completed project only through an explicit, reason-bearing
    /// operation. This preserves the meaning of Complete while allowing an
    /// operator to document a legitimate post-cutover correction.
    pub fn reopen_project(&self, id: &str, reason: &str) -> rusqlite::Result<()> {
        let reason = reason.trim();
        if reason.is_empty() || reason.len() > 4096 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let current: String =
            tx.query_row("SELECT phase FROM projects WHERE id=?1", [id], |row| {
                row.get(0)
            })?;
        if current != Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let (total, verified, active_runs, active_processes): (i64, i64, i64, i64) = tx.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN j.state IN ('verified','verified_with_exceptions') THEN 1 ELSE 0 END),0),
                    (SELECT COUNT(*) FROM runs r WHERE r.project_id=?1 AND r.status IN ('queued','running')),
                    (SELECT COUNT(*) FROM active_processes ap JOIN runs r ON r.id=ap.run_id WHERE r.project_id=?1)
             FROM mailbox_jobs j WHERE j.project_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        if total == 0 || verified != total || active_runs != 0 || active_processes != 0 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = tx.execute(
            "UPDATE projects SET phase='attention' WHERE id=?1 AND phase='complete'",
            [id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_reopened',?2)",
            params![id, reason],
        )?;
        tx.commit()
    }
}
