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
        let current: String =
            self.connection
                .query_row("SELECT phase FROM projects WHERE id=?1", [id], |row| {
                    row.get(0)
                })?;
        if current != Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
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
