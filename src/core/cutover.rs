use super::StateStore;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

/// Durable, operator-approved migration lifecycle.  The stage is deliberately
/// separate from `projects.phase`: a project phase describes the evidence
/// currently present, while this record describes the next approved
/// operational pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum CutoverStage {
    Seed,
    CatchUp,
    FinalDelta,
    Verification,
    Completed,
}

impl CutoverStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Seed => "seed",
            Self::CatchUp => "catch_up",
            Self::FinalDelta => "final_delta",
            Self::Verification => "verification",
            Self::Completed => "completed",
        }
    }

    fn next(self) -> Option<Self> {
        match self {
            Self::Seed => Some(Self::CatchUp),
            Self::CatchUp => Some(Self::FinalDelta),
            Self::FinalDelta => Some(Self::Verification),
            Self::Verification => Some(Self::Completed),
            Self::Completed => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CutoverWorkflow {
    pub project_id: String,
    pub stage: CutoverStage,
    pub scheduled_at: String,
    pub maintenance_window: Option<String>,
    pub approved_by: Option<String>,
    pub approved_at: Option<String>,
    pub external_confirmation: Option<String>,
}

fn parse_stage(value: &str) -> rusqlite::Result<CutoverStage> {
    match value {
        "seed" => Ok(CutoverStage::Seed),
        "catch_up" => Ok(CutoverStage::CatchUp),
        "final_delta" => Ok(CutoverStage::FinalDelta),
        "verification" => Ok(CutoverStage::Verification),
        "completed" => Ok(CutoverStage::Completed),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

impl StateStore {
    /// Create an unapproved cutover plan.  Nothing is scheduled until an
    /// operator explicitly approves the plan.
    pub fn create_cutover_workflow(
        &self,
        project_id: &str,
        scheduled_at: &str,
        maintenance_window: Option<&str>,
    ) -> rusqlite::Result<()> {
        if scheduled_at.trim().is_empty() || maintenance_window.is_some_and(|v| v.trim().is_empty())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let phase: String = tx.query_row(
            "SELECT phase FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if !matches!(phase.as_str(), "preflight" | "pilot") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "INSERT INTO cutover_workflows(project_id,stage,scheduled_at,maintenance_window) VALUES(?1,'seed',?2,?3)",
            params![project_id, scheduled_at, maintenance_window],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'cutover_planned',?2)",
            params![project_id, scheduled_at],
        )?;
        tx.commit()
    }

    pub fn cutover_workflow(&self, project_id: &str) -> rusqlite::Result<Option<CutoverWorkflow>> {
        self.connection.query_row(
            "SELECT project_id,stage,scheduled_at,maintenance_window,approved_by,approved_at,external_confirmation FROM cutover_workflows WHERE project_id=?1",
            [project_id],
            |row| Ok(CutoverWorkflow {
                project_id: row.get(0)?, stage: parse_stage(&row.get::<_, String>(1)?)?,
                scheduled_at: row.get(2)?, maintenance_window: row.get(3)?, approved_by: row.get(4)?,
                approved_at: row.get(5)?, external_confirmation: row.get(6)?,
            }),
        ).optional()
    }

    pub fn cutover_stage(&self, project_id: &str) -> rusqlite::Result<Option<CutoverStage>> {
        self.cutover_workflow(project_id)
            .map(|workflow| workflow.map(|w| w.stage))
    }

    pub fn approve_cutover(&self, project_id: &str, operator: &str) -> rusqlite::Result<()> {
        if operator.trim().is_empty() || operator.len() > 256 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let phase: String = tx.query_row(
            "SELECT phase FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if !matches!(phase.as_str(), "preflight" | "pilot") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = tx.execute("UPDATE cutover_workflows SET approved_by=?1,approved_at=CURRENT_TIMESTAMP WHERE project_id=?2 AND approved_by IS NULL", params![operator, project_id])?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute("UPDATE projects SET phase='seed' WHERE id=?1", [project_id])?;
        // Every phase change is audited (and drives lifecycle webhooks).
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'phase_changed','seed')",
            [project_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'cutover_approved',?2)",
            params![project_id, operator],
        )?;
        tx.commit()
    }

    /// Advance only after every mailbox has durable verification evidence.
    /// Intermediate advances requeue verified rows for the next idempotent
    /// delta pass; final completion additionally requires external cutover
    /// confirmation recorded by the operator.
    pub fn advance_cutover(
        &self,
        project_id: &str,
        external_confirmation: Option<&str>,
    ) -> rusqlite::Result<CutoverStage> {
        let tx = self.connection.unchecked_transaction()?;
        let (stage_name, approved): (String, Option<String>) = tx.query_row(
            "SELECT stage,approved_by FROM cutover_workflows WHERE project_id=?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if approved.is_none() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let stage = parse_stage(&stage_name)?;
        // Complete is terminal; only `reopen_project` may leave it.
        let phase: String = tx.query_row(
            "SELECT phase FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if phase == "complete" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let (total, verified): (i64, i64) = tx.query_row("SELECT COUNT(*),COALESCE(SUM(state='verified' OR (state='verified_with_exceptions' AND EXISTS(SELECT 1 FROM verification_acceptances a WHERE a.job_id=mailbox_jobs.id))),0) FROM mailbox_jobs WHERE project_id=?1", [project_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        if total == 0 || total != verified {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let next = stage.next().ok_or(rusqlite::Error::InvalidQuery)?;
        let confirmation = external_confirmation
            .map(str::trim)
            .filter(|v| !v.is_empty());
        if next == CutoverStage::Completed && confirmation.is_none() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if next != CutoverStage::Completed {
            tx.execute("UPDATE mailbox_jobs SET state='ready',attention_reason=NULL WHERE project_id=?1 AND state IN ('verified','verified_with_exceptions')", [project_id])?;
        }
        tx.execute("UPDATE cutover_workflows SET stage=?1,external_confirmation=COALESCE(?2,external_confirmation) WHERE project_id=?3", params![next.as_str(), confirmation, project_id])?;
        let next_phase = match next {
            CutoverStage::CatchUp => "catch_up",
            CutoverStage::FinalDelta => "final_delta",
            CutoverStage::Verification => "verification",
            CutoverStage::Completed => "complete",
            CutoverStage::Seed => "seed",
        };
        tx.execute(
            "UPDATE projects SET phase=?1 WHERE id=?2",
            params![next_phase, project_id],
        )?;
        if phase != next_phase {
            // `final_delta` publishes `migration.cutover_ready` and
            // `complete` publishes `migration.completed` via the outbox.
            tx.execute(
                "INSERT INTO events(project_id,kind,detail) VALUES(?1,'phase_changed',?2)",
                params![project_id, next_phase],
            )?;
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'cutover_advanced',?2)",
            params![project_id, next.as_str()],
        )?;
        tx.commit()?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::{CutoverStage, StateStore};
    use crate::core::Phase;

    #[test]
    fn cutover_requires_approval_evidence_and_external_confirmation() {
        let store = StateStore::in_memory().unwrap();
        let (project, job) = store
            .create_project_with_mailbox("cutover", "source", "destination", "a", "a")
            .unwrap();
        store.transition(&project.id, Phase::Preflight).unwrap();
        store
            .create_cutover_workflow(&project.id, "2026-10-03T22:00:00Z", Some("22:00-01:00"))
            .unwrap();
        assert!(
            store
                .create_cutover_workflow(&project.id, "2026-10-04T22:00:00Z", None)
                .is_err()
        );
        assert!(store.approve_cutover(&project.id, "operator").is_ok());
        assert!(store.advance_cutover(&project.id, None).is_err());
        store
            .connection
            .execute(
                "UPDATE mailbox_jobs SET state='verified' WHERE id=?1",
                [&job],
            )
            .unwrap();
        assert_eq!(
            store.advance_cutover(&project.id, None).unwrap(),
            CutoverStage::CatchUp
        );
        assert_eq!(
            store.cutover_stage(&project.id).unwrap(),
            Some(CutoverStage::CatchUp)
        );
        store
            .connection
            .execute(
                "UPDATE mailbox_jobs SET state='verified' WHERE id=?1",
                [&job],
            )
            .unwrap();
        store.advance_cutover(&project.id, None).unwrap();
        store
            .connection
            .execute(
                "UPDATE mailbox_jobs SET state='verified' WHERE id=?1",
                [&job],
            )
            .unwrap();
        store.advance_cutover(&project.id, None).unwrap();
        store
            .connection
            .execute(
                "UPDATE mailbox_jobs SET state='verified' WHERE id=?1",
                [&job],
            )
            .unwrap();
        assert!(store.advance_cutover(&project.id, None).is_err());
        assert_eq!(
            store
                .advance_cutover(&project.id, Some("MX confirmed by operator"))
                .unwrap(),
            CutoverStage::Completed
        );
        assert_eq!(
            store.project(&project.id).unwrap().unwrap().phase,
            Phase::Complete
        );
        let phase_events = store
            .connection
            .prepare("SELECT detail FROM events WHERE project_id=?1 AND kind='phase_changed' ORDER BY id")
            .unwrap()
            .query_map([&project.id], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            phase_events,
            [
                "preflight",
                "seed",
                "catch_up",
                "final_delta",
                "verification",
                "complete"
            ]
        );
        let cutover_ready: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_ready'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cutover_ready, 1);
    }

    #[test]
    fn cutover_cannot_silently_reopen_a_completed_project() {
        let store = StateStore::in_memory().unwrap();
        let (project, job) = store
            .create_project_with_mailbox("cutover", "source", "destination", "a", "a")
            .unwrap();
        store.transition(&project.id, Phase::Preflight).unwrap();
        store
            .create_cutover_workflow(&project.id, "2026-10-03T22:00:00Z", None)
            .unwrap();
        store.approve_cutover(&project.id, "operator").unwrap();
        store
            .connection
            .execute(
                "UPDATE mailbox_jobs SET state='verified' WHERE id=?1",
                [&job],
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE projects SET phase='complete' WHERE id=?1",
                [&project.id],
            )
            .unwrap();
        assert!(store.advance_cutover(&project.id, None).is_err());
        assert_eq!(
            store.project(&project.id).unwrap().unwrap().phase,
            Phase::Complete
        );
    }
}
