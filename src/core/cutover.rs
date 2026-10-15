use super::StateStore;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::fmt;

#[derive(Debug)]
pub(crate) enum CutoverAdvanceError {
    Ledger(rusqlite::Error),
    Refused(String),
}

impl From<rusqlite::Error> for CutoverAdvanceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Ledger(error)
    }
}

impl fmt::Display for CutoverAdvanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ledger(error) => write!(formatter, "ledger operation failed: {error}"),
            Self::Refused(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for CutoverAdvanceError {}

/// Validate the workflow/phase binding and the final-delta operator
/// attestation. This is called inside durable run-admission transactions as
/// well as by the headless preflight, so UI and batch callers cannot bypass
/// managed cutover rules.
pub(crate) fn validate_cutover_run_stage(
    stage: &str,
    phase: &str,
    source_quiescence_at: Option<&str>,
) -> Result<(), String> {
    if stage != phase {
        return Err("managed cutover stage does not match the project phase".into());
    }
    if stage != CutoverStage::FinalDelta.as_str() {
        return Ok(());
    }
    if let Some(error) = source_quiescence_error(source_quiescence_at) {
        return Err(error);
    }
    Ok(())
}

fn source_quiescence_error(source_quiescence_at: Option<&str>) -> Option<String> {
    let Some(attested_at) = source_quiescence_at else {
        return Some("source-quiescence attestation is missing".into());
    };
    // SQLite CURRENT_TIMESTAMP is UTC in `YYYY-MM-DD HH:MM:SS` form.
    // A stale operator assertion must not authorize a later final-delta
    // launch after mail delivery or mailbox edits may have resumed.
    let attested_at = match chrono::NaiveDateTime::parse_from_str(attested_at, "%Y-%m-%d %H:%M:%S")
    {
        Ok(value) => value.and_utc(),
        Err(_) => return Some("source-quiescence attestation timestamp is invalid".into()),
    };
    let age = chrono::Utc::now().signed_duration_since(attested_at);
    if age < chrono::Duration::zero() {
        return Some("source-quiescence attestation timestamp is in the future".into());
    }
    if age > chrono::Duration::minutes(15) {
        return Some("source-quiescence attestation is older than 15 minutes".into());
    }
    None
}

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

/// Publish cutover readiness only after the last mailbox's final verification
/// result is durable and the source-quiescence assertion is fresh. Called from
/// the same SQLite transaction that commits verified state or an explicit
/// exception acceptance, and when an operator refreshes the assertion during
/// Verification.
pub(super) fn maybe_emit_cutover_ready(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
) -> rusqlite::Result<()> {
    let workflow: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT c.stage,p.phase,c.source_quiescence_at FROM cutover_workflows c JOIN projects p ON p.id=c.project_id WHERE c.project_id=?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((stage, phase, source_quiescence_at)) = workflow else {
        return Ok(());
    };
    if stage != "verification"
        || phase != "verification"
        || source_quiescence_error(source_quiescence_at.as_deref()).is_some()
    {
        return Ok(());
    }
    let (total, clean_verified, accepted_exceptions): (i64, i64, i64) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(state='verified'),0),COALESCE(SUM(state='verified_with_exceptions' AND EXISTS(SELECT 1 FROM verification_acceptances a WHERE a.job_id=mailbox_jobs.id)),0) FROM mailbox_jobs WHERE project_id=?1",
        [project_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if total == 0 || total != clean_verified + accepted_exceptions {
        return Ok(());
    }
    let already_published: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE project_id=?1 AND kind='cutover_ready')",
        [project_id],
        |row| row.get(0),
    )?;
    if already_published {
        return Ok(());
    }
    let Some(attested_at) = source_quiescence_at else {
        return Ok(());
    };
    let attested_at = chrono::NaiveDateTime::parse_from_str(&attested_at, "%Y-%m-%d %H:%M:%S")
        .map_err(|_| rusqlite::Error::InvalidQuery)?
        .and_utc();
    let valid_until = (attested_at + chrono::Duration::minutes(15)).to_rfc3339();
    let detail = format!(
        "final verification complete; mailboxes={total}; clean_verified={clean_verified}; accepted_exceptions={accepted_exceptions}; source-quiescence attestation valid until {valid_until}; external cutover approval pending"
    );
    tx.execute(
        "INSERT INTO events(project_id,kind,detail) VALUES(?1,'cutover_ready',?2)",
        params![project_id, detail],
    )?;
    Ok(())
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
    /// Operator attestation that source delivery/mutation was quiesced before
    /// the final delta. This is not an automatically observed IMAP watermark.
    pub source_quiescence_operator: Option<String>,
    pub source_quiescence_evidence: Option<String>,
    pub source_quiescence_at: Option<String>,
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
        let scheduled_at = crate::storage::single_line_audit_text(scheduled_at, 256)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        let maintenance_window = maintenance_window
            .map(|value| {
                crate::storage::single_line_audit_text(value, 1024)
                    .ok_or(rusqlite::Error::InvalidQuery)
            })
            .transpose()?;
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
            "SELECT project_id,stage,scheduled_at,maintenance_window,approved_by,approved_at,external_confirmation,source_quiescence_operator,source_quiescence_evidence,source_quiescence_at FROM cutover_workflows WHERE project_id=?1",
            [project_id],
            |row| Ok(CutoverWorkflow {
                project_id: row.get(0)?, stage: parse_stage(&row.get::<_, String>(1)?)?,
                scheduled_at: row.get(2)?, maintenance_window: row.get(3)?, approved_by: row.get(4)?,
                approved_at: row.get(5)?, external_confirmation: row.get(6)?,
                source_quiescence_operator: row.get(7)?, source_quiescence_evidence: row.get(8)?,
                source_quiescence_at: row.get(9)?,
            }),
        ).optional()
    }

    pub fn cutover_stage(&self, project_id: &str) -> rusqlite::Result<Option<CutoverStage>> {
        self.cutover_workflow(project_id)
            .map(|workflow| workflow.map(|w| w.stage))
    }

    #[cfg(test)]
    pub(crate) fn drop_cutover_table_for_test(&self) -> rusqlite::Result<()> {
        self.connection
            .execute_batch("DROP TABLE cutover_workflows")
    }

    pub fn approve_cutover(&self, project_id: &str, operator: &str) -> rusqlite::Result<()> {
        let operator = crate::storage::single_line_audit_text(operator, 256)
            .ok_or(rusqlite::Error::InvalidQuery)?;
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

    /// Record the operator's source-quiescence attestation before entering the
    /// final-delta stage. The evidence is intentionally explicit that this is a
    /// manual operational claim, not machine-verified source stability.
    pub fn attest_cutover_source_quiescence(
        &self,
        project_id: &str,
        operator: &str,
        evidence: &str,
    ) -> rusqlite::Result<()> {
        let operator = crate::storage::single_line_audit_text(operator, 256)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        let evidence = crate::storage::single_line_audit_text(evidence, 4096)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        let tx = self.connection.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE cutover_workflows SET source_quiescence_operator=?1,source_quiescence_evidence=?2,source_quiescence_at=CURRENT_TIMESTAMP WHERE project_id=?3 AND stage IN ('catch_up','final_delta','verification') AND approved_by IS NOT NULL AND (source_quiescence_at IS NULL OR stage IN ('final_delta','verification'))",
            params![operator, evidence, project_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'cutover_source_quiescence_attested',?2)",
            params![project_id, format!("operator={operator}; evidence={evidence}")],
        )?;
        maybe_emit_cutover_ready(&tx, project_id)?;
        tx.commit()
    }

    /// Advance only after every mailbox has durable verification evidence.
    /// Intermediate advances requeue verified rows for the next idempotent
    /// delta pass. Final-delta entry requires a recorded source-quiescence
    /// attestation; final completion additionally requires external cutover
    /// confirmation. The attestation is not automatic server revalidation.
    pub(crate) fn advance_cutover(
        &self,
        project_id: &str,
        external_confirmation: Option<&str>,
    ) -> Result<CutoverStage, CutoverAdvanceError> {
        let tx = self.connection.unchecked_transaction()?;
        let (stage_name, approved): (String, Option<String>) = tx.query_row(
            "SELECT stage,approved_by FROM cutover_workflows WHERE project_id=?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if approved.is_none() {
            return Err(CutoverAdvanceError::Refused(
                "cutover advancement requires explicit approval".into(),
            ));
        }
        let stage = parse_stage(&stage_name)?;
        // Complete is terminal; only `reopen_project` may leave it.
        let phase: String = tx.query_row(
            "SELECT phase FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if phase == "complete" {
            return Err(CutoverAdvanceError::Refused(
                "completed cutover workflows cannot be advanced again".into(),
            ));
        }
        let (total, clean_verified, accepted_exceptions): (i64, i64, i64) = tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(state='verified'),0),COALESCE(SUM(state='verified_with_exceptions' AND EXISTS(SELECT 1 FROM verification_acceptances a WHERE a.job_id=mailbox_jobs.id)),0) FROM mailbox_jobs WHERE project_id=?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if total == 0 || total != clean_verified + accepted_exceptions {
            return Err(CutoverAdvanceError::Refused(
                "every mailbox must have durable verification evidence or an accepted exception before advancing".into(),
            ));
        }
        let next = stage.next().ok_or_else(|| {
            CutoverAdvanceError::Refused("cutover workflow is already at its terminal stage".into())
        })?;
        if next == CutoverStage::FinalDelta
            || stage == CutoverStage::FinalDelta
            || next == CutoverStage::Completed
        {
            let attested_at: Option<String> = tx.query_row(
                "SELECT source_quiescence_at FROM cutover_workflows WHERE project_id=?1",
                [project_id],
                |row| row.get(0),
            )?;
            if let Some(error) = source_quiescence_error(attested_at.as_deref()) {
                return Err(CutoverAdvanceError::Refused(format!(
                    "cannot advance cutover: {error}; reconfirm source delivery is paused and refresh the attestation"
                )));
            }
        }
        let confirmation = external_confirmation
            .map(|value| {
                crate::storage::single_line_audit_text(value, 4096).ok_or_else(|| {
                    CutoverAdvanceError::Refused(
                        "external confirmation must be nonempty, single-line audit text".into(),
                    )
                })
            })
            .transpose()?;
        if next == CutoverStage::Completed && confirmation.is_none() {
            return Err(CutoverAdvanceError::Refused(
                "external cutover confirmation is required before completing the workflow".into(),
            ));
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
            tx.execute(
                "INSERT INTO events(project_id,kind,detail) VALUES(?1,'phase_changed',?2)",
                params![project_id, next_phase],
            )?;
        }
        // Emit phase-start signals transactionally. Readiness itself is
        // emitted only when the final Verification-phase mailbox reaches an
        // acceptable terminal state after this method requeues prior evidence.
        match next {
            CutoverStage::FinalDelta => {
                tx.execute(
                    "INSERT INTO events(project_id,kind,detail) VALUES(?1,'cutover_final_delta_started','source quiescence attested; final delta started')",
                    [project_id],
                )?;
            }
            CutoverStage::Verification => {
                tx.execute(
                    "INSERT INTO events(project_id,kind,detail) VALUES(?1,'cutover_verification_started','final delta complete; final verification stage started')",
                    [project_id],
                )?;
            }
            _ => {}
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
    use crate::core::{MailboxEvidence, Phase, VerificationMethod};

    #[test]
    fn durable_single_and_batch_admission_reject_stale_managed_final_delta() {
        let store = StateStore::in_memory().unwrap();
        let (project, job) = store
            .create_project_with_mailbox("cutover-admission", "source", "destination", "a", "a")
            .unwrap();
        store.transition(&project.id, Phase::Preflight).unwrap();
        store
            .create_cutover_workflow(&project.id, "2026-10-15T23:00:00Z", None)
            .unwrap();
        store.approve_cutover(&project.id, "operator").unwrap();
        store
            .connection
            .execute(
                "UPDATE projects SET phase='final_delta' WHERE id=?1",
                [&project.id],
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE cutover_workflows SET stage='final_delta',source_quiescence_at='2026-10-14 00:00:00' WHERE project_id=?1",
                [&project.id],
            )
            .unwrap();

        assert!(
            store
                .begin_run(&project.id, &job, "stale-final-delta-single", "imapsync")
                .is_err()
        );
        assert!(
            store
                .begin_batch_run_with_children(
                    &project.id,
                    std::slice::from_ref(&job),
                    "stale-final-delta-batch",
                    "imapsync",
                    &[],
                    "",
                    &[],
                )
                .is_err()
        );
        let run_count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(run_count, 0, "rejected admission must be atomic");
    }

    #[test]
    fn unapproved_cutover_cannot_be_advanced_or_used_to_admit_runs() {
        let store = StateStore::in_memory().unwrap();
        let (project, job) = store
            .create_project_with_mailbox("unapproved-cutover", "source", "destination", "a", "a")
            .unwrap();
        store.transition(&project.id, Phase::Preflight).unwrap();
        store
            .create_cutover_workflow(&project.id, "2026-10-15T23:00:00Z", None)
            .unwrap();
        assert!(store.transition(&project.id, Phase::Seed).is_err());

        // Simulate legacy/corrupt phase drift: the durable run boundary must
        // still require the workflow's explicit approval.
        store
            .connection
            .execute(
                "UPDATE projects SET phase='seed' WHERE id=?1",
                [&project.id],
            )
            .unwrap();
        assert!(
            store
                .begin_run(&project.id, &job, "unapproved-cutover-run", "imapsync")
                .is_err()
        );
        assert!(
            store
                .begin_batch_run_with_children(
                    &project.id,
                    std::slice::from_ref(&job),
                    "unapproved-cutover-batch",
                    "imapsync",
                    &[],
                    "",
                    &[],
                )
                .is_err()
        );
        let run_count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(run_count, 0);
    }

    #[test]
    fn delayed_batch_child_claim_rechecks_and_can_refresh_final_delta_attestation() {
        let store = StateStore::in_memory().unwrap();
        let (project, job) = store
            .create_project_with_mailbox("cutover-child-claim", "source", "destination", "a", "a")
            .unwrap();
        store.transition(&project.id, Phase::Preflight).unwrap();
        store
            .create_cutover_workflow(&project.id, "2026-10-15T23:00:00Z", None)
            .unwrap();
        store.approve_cutover(&project.id, "operator").unwrap();
        store
            .connection
            .execute(
                "UPDATE projects SET phase='final_delta' WHERE id=?1",
                [&project.id],
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE cutover_workflows SET stage='final_delta',source_quiescence_at=CURRENT_TIMESTAMP WHERE project_id=?1",
                [&project.id],
            )
            .unwrap();
        let children = store
            .begin_batch_run_with_children(
                &project.id,
                std::slice::from_ref(&job),
                "delayed-final-delta-batch",
                "imapsync",
                &[],
                "",
                &[],
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE cutover_workflows SET source_quiescence_at=datetime('now','-16 minutes') WHERE project_id=?1",
                [&project.id],
            )
            .unwrap();
        assert!(
            store
                .claim_batch_mailbox_for_child(
                    &project.id,
                    &job,
                    "delayed-final-delta-batch",
                    &children[0],
                )
                .is_err()
        );

        store
            .attest_cutover_source_quiescence(
                &project.id,
                "operator",
                "Delivery pause reconfirmed; CHG-123",
            )
            .unwrap();
        assert!(
            store
                .claim_batch_mailbox_for_child(
                    &project.id,
                    &job,
                    "delayed-final-delta-batch",
                    &children[0],
                )
                .is_ok()
        );
    }

    #[test]
    fn cutover_requires_approval_evidence_and_external_confirmation() {
        let store = StateStore::in_memory().unwrap();
        let (project, job) = store
            .create_project_with_mailbox("cutover", "source", "destination", "a", "a")
            .unwrap();
        store.transition(&project.id, Phase::Preflight).unwrap();
        assert!(
            store
                .create_cutover_workflow(
                    &project.id,
                    "2026-10-03T22:00:00Z\ncutover_approved",
                    None
                )
                .is_err()
        );
        assert!(
            store
                .create_cutover_workflow(
                    &project.id,
                    "2026-10-03T22:00:00Z\u{2028}cutover_approved",
                    None
                )
                .is_err()
        );
        assert!(
            store
                .create_cutover_workflow(
                    &project.id,
                    "2026-10-03T22:00:00Z",
                    Some("22:00\ncutover_completed")
                )
                .is_err()
        );
        store
            .create_cutover_workflow(&project.id, "2026-10-03T22:00:00Z", Some("22:00-01:00"))
            .unwrap();
        assert!(
            store
                .create_cutover_workflow(&project.id, "2026-10-04T22:00:00Z", None)
                .is_err()
        );
        assert!(
            store
                .approve_cutover(&project.id, "operator\ncutover_approved")
                .is_err()
        );
        assert!(store.approve_cutover(&project.id, "operator").is_ok());
        assert!(store.advance_cutover(&project.id, None).is_err());
        assert!(
            store
                .attest_cutover_source_quiescence(
                    &project.id,
                    "operator",
                    "delivery paused\ncutover_advanced; completed"
                )
                .is_err()
        );
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
        let missing_final_delta_attestation = store.advance_cutover(&project.id, None).unwrap_err();
        assert!(
            missing_final_delta_attestation
                .to_string()
                .contains("attestation is missing")
        );
        store
            .attest_cutover_source_quiescence(
                &project.id,
                "operator",
                "Source delivery paused at 22:00 UTC; MX change ticket CHG-123",
            )
            .unwrap();
        assert_eq!(
            store.advance_cutover(&project.id, None).unwrap(),
            CutoverStage::FinalDelta
        );
        let cutover_ready_before_final_delta: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_ready'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cutover_ready_before_final_delta, 0);
        let final_delta_started: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.final_delta_started'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(final_delta_started, 1);
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
                "UPDATE cutover_workflows SET source_quiescence_at=datetime('now','-16 minutes') WHERE project_id=?1",
                [&project.id],
            )
            .unwrap();
        let stale_verification_attestation = store.advance_cutover(&project.id, None).unwrap_err();
        assert!(
            stale_verification_attestation
                .to_string()
                .contains("older than 15 minutes")
        );
        store
            .attest_cutover_source_quiescence(
                &project.id,
                "operator",
                "Reconfirmed source delivery remains paused before final verification",
            )
            .unwrap();
        assert_eq!(
            store.advance_cutover(&project.id, None).unwrap(),
            CutoverStage::Verification
        );
        let cutover_ready_before_final_verification: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_ready'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cutover_ready_before_final_verification, 0);
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
                "UPDATE cutover_workflows SET source_quiescence_at=datetime('now','-16 minutes') WHERE project_id=?1",
                [&project.id],
            )
            .unwrap();
        let verification_tx = store.connection.unchecked_transaction().unwrap();
        super::maybe_emit_cutover_ready(&verification_tx, &project.id).unwrap();
        verification_tx.commit().unwrap();
        let stale_cutover_ready_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_ready'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stale_cutover_ready_count, 0);
        store
            .attest_cutover_source_quiescence(
                &project.id,
                "operator",
                "Reconfirmed source delivery remains paused after final verification",
            )
            .unwrap();
        let cutover_ready_after_final_verification: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_ready'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cutover_ready_after_final_verification, 1);
        let event_payload: String = store
            .connection
            .query_row(
                "SELECT payload FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_ready'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&event_payload).unwrap();
        assert_eq!(payload["event_type"], "migration.cutover_ready");
        assert!(
            payload["detail"]
                .as_str()
                .unwrap()
                .contains("clean_verified=1")
        );
        assert!(
            payload["detail"]
                .as_str()
                .unwrap()
                .contains("accepted_exceptions=0")
        );
        assert!(
            payload["detail"]
                .as_str()
                .unwrap()
                .contains("source-quiescence attestation valid until")
        );
        assert!(store.advance_cutover(&project.id, None).is_err());
        assert!(
            store
                .advance_cutover(&project.id, Some("MX confirmed\ncutover completed"))
                .is_err()
        );
        store
            .connection
            .execute(
                "UPDATE cutover_workflows SET source_quiescence_at='2026-10-14 00:00:00' WHERE project_id=?1",
                [&project.id],
            )
            .unwrap();
        let stale_completion = store
            .advance_cutover(&project.id, Some("MX confirmed by operator"))
            .unwrap_err();
        assert!(
            stale_completion
                .to_string()
                .contains("older than 15 minutes")
        );
        store
            .attest_cutover_source_quiescence(
                &project.id,
                "operator",
                "Reconfirmed source delivery remains paused at external cutover",
            )
            .unwrap();
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
        assert!(
            store
                .reopen_project(&project.id, "post-cutover correction requested")
                .is_err()
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
    fn generic_final_delta_phase_does_not_publish_cutover_events() {
        let store = StateStore::in_memory().unwrap();
        let (project, _) = store
            .create_project_with_mailbox("ordinary", "source", "destination", "a", "a")
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO events(project_id,kind,detail) VALUES(?1,'phase_changed','final_delta')",
                [&project.id],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO events(project_id,kind,detail) VALUES(?1,'phase_changed','verification')",
                [&project.id],
            )
            .unwrap();
        let lifecycle_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type IN ('migration.final_delta_started','migration.cutover_verification_started','migration.cutover_ready')",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lifecycle_count, 0);
    }

    #[test]
    fn schema_37_repairs_queued_legacy_cutover_ready_events() {
        let store = StateStore::in_memory().unwrap();
        let (managed, _) = store
            .create_project_with_mailbox("managed", "source", "destination", "a", "a")
            .unwrap();
        store.transition(&managed.id, Phase::Preflight).unwrap();
        store
            .create_cutover_workflow(&managed.id, "2026-10-15T23:00:00Z", None)
            .unwrap();
        let (generic, _) = store
            .create_project_with_mailbox("generic", "source", "destination", "b", "b")
            .unwrap();

        let mut legacy_event_ids = Vec::new();
        for (project_id, phase) in [
            (&managed.id, "final_delta"),
            (&managed.id, "verification"),
            (&generic.id, "final_delta"),
        ] {
            store
                .connection
                .execute(
                    "INSERT INTO events(project_id,kind,detail) VALUES(?1,'phase_changed',?2)",
                    rusqlite::params![project_id, phase],
                )
                .unwrap();
            let event_id = format!("ledger-event-{}", store.connection.last_insert_rowid());
            store
                .connection
                .execute(
                    "INSERT INTO webhook_deliveries(event_id,project_id,event_type,payload,endpoint_digest) VALUES(?1,?2,'migration.cutover_ready','{\"event_type\":\"migration.cutover_ready\"}','')",
                    rusqlite::params![event_id, project_id],
                )
                .unwrap();
            legacy_event_ids.push(event_id);
        }
        store
            .connection
            .pragma_update(None, "user_version", 35)
            .unwrap();
        store.migrate().unwrap();

        let managed_event: (String, String, String) = store
            .connection
            .query_row(
                "SELECT event_type,status,json_extract(payload,'$.event_type') FROM webhook_deliveries WHERE event_id=?1",
                [&legacy_event_ids[0]],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            managed_event,
            (
                "migration.final_delta_started".to_owned(),
                "queued".to_owned(),
                "migration.final_delta_started".to_owned()
            )
        );
        let verification_started_event: (String, String, String) = store
            .connection
            .query_row(
                "SELECT event_type,status,json_extract(payload,'$.event_type') FROM webhook_deliveries WHERE event_id=?1",
                [&legacy_event_ids[1]],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            verification_started_event,
            (
                "migration.cutover_verification_started".to_owned(),
                "queued".to_owned(),
                "migration.cutover_verification_started".to_owned()
            )
        );
        let generic_event: (String, String) = store
            .connection
            .query_row(
                "SELECT status,last_error FROM webhook_deliveries WHERE event_id=?1",
                [&legacy_event_ids[2]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(generic_event.0, "dead_letter");
        assert!(generic_event.1.contains("no managed cutover workflow"));
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

    #[test]
    fn accepted_difference_cannot_bypass_cutover_confirmation() {
        let store = StateStore::in_memory().unwrap();
        let (project, job) = store
            .create_project_with_mailbox("cutover", "source", "destination", "a", "a")
            .unwrap();
        store.transition(&project.id, Phase::Preflight).unwrap();
        store
            .create_cutover_workflow(&project.id, "2026-10-15T22:00:00Z", None)
            .unwrap();
        store.approve_cutover(&project.id, "operator").unwrap();

        // Simulate verified seed, catch-up, and final-delta passes. The cutover
        // workflow owns phase advancement and keeps external confirmation as
        // the final transition.
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
        store
            .connection
            .execute(
                "UPDATE mailbox_jobs SET state='verified' WHERE id=?1",
                [&job],
            )
            .unwrap();
        store
            .attest_cutover_source_quiescence(
                &project.id,
                "operator",
                "Source delivery paused for the final delta",
            )
            .unwrap();
        assert_eq!(
            store.advance_cutover(&project.id, None).unwrap(),
            CutoverStage::FinalDelta
        );
        store
            .connection
            .execute(
                "UPDATE mailbox_jobs SET state='verified' WHERE id=?1",
                [&job],
            )
            .unwrap();
        assert_eq!(
            store.advance_cutover(&project.id, None).unwrap(),
            CutoverStage::Verification
        );
        let ready_before_final_verification: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_ready'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ready_before_final_verification, 0);

        let evidence = MailboxEvidence {
            verification_method: VerificationMethod::AggregateEngine,
            verification_outcome: None,
            source_messages: 2,
            destination_messages: 1,
            source_bytes: 20,
            destination_bytes: 10,
            unmatched_messages: Some(1),
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: false,
            missing_messages: 1,
            extra_messages: 0,
            modified_messages: 0,
            probable_messages: 0,
            rfc_message_id_matches: 0,
            synthetic_identity_matches: 0,
            flag_verification: None,
        };
        store
            .begin_run(&project.id, &job, "cutover-exception-run", "imapsync")
            .unwrap();
        store
            .finish_run_for_mailbox_with_evidence(
                &project.id,
                &job,
                "cutover-exception-run",
                "completed",
                "verification_difference",
                "difference requires review",
                &evidence,
            )
            .unwrap();
        store
            .accept_verification_difference(
                &project.id,
                &job,
                "operator",
                "Accepted with customer approval",
            )
            .unwrap();

        let ready_payload: String = store
            .connection
            .query_row(
                "SELECT payload FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_ready'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        let ready_payload: serde_json::Value = serde_json::from_str(&ready_payload).unwrap();
        assert!(
            ready_payload["detail"]
                .as_str()
                .unwrap()
                .contains("accepted_exceptions=1")
        );

        assert_eq!(
            store.project(&project.id).unwrap().unwrap().phase,
            Phase::Verification
        );
        assert_eq!(
            store.cutover_stage(&project.id).unwrap(),
            Some(CutoverStage::Verification)
        );
        assert!(store.transition(&project.id, Phase::Complete).is_err());
        assert_eq!(
            store
                .advance_cutover(&project.id, Some("Approved MX cutover CHG-123"))
                .unwrap(),
            CutoverStage::Completed
        );
        assert_eq!(
            store.project(&project.id).unwrap().unwrap().phase,
            Phase::Complete
        );
    }

    #[test]
    fn schema_33_cutover_rows_gain_empty_source_quiescence_fields() {
        let store = StateStore::in_memory().unwrap();
        store
            .connection
            .execute_batch(
                "ALTER TABLE cutover_workflows DROP COLUMN source_quiescence_operator;
                 ALTER TABLE cutover_workflows DROP COLUMN source_quiescence_evidence;
                 ALTER TABLE cutover_workflows DROP COLUMN source_quiescence_at;
                 PRAGMA user_version=33;",
            )
            .unwrap();
        store.migrate().unwrap();
        let columns: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('cutover_workflows') WHERE name IN ('source_quiescence_operator','source_quiescence_evidence','source_quiescence_at')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(columns, 3);
    }

    #[test]
    fn schema_34_folder_observations_gain_source_revalidation_timestamp() {
        let store = StateStore::in_memory().unwrap();
        store
            .connection
            .execute_batch(
                "ALTER TABLE transfer_pass_folders DROP COLUMN source_revalidated_at;
                 ALTER TABLE verification_run_folders DROP COLUMN source_revalidated_at;
                 PRAGMA user_version=34;",
            )
            .unwrap();
        store.migrate().unwrap();
        for table in ["transfer_pass_folders", "verification_run_folders"] {
            let columns: i64 = store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name='source_revalidated_at'",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(columns, 1, "{table} should be upgraded");
        }
    }

    #[test]
    fn schema_37_folder_observations_gain_destination_revalidation_timestamp() {
        let store = StateStore::in_memory().unwrap();
        store
            .connection
            .execute_batch(
                "ALTER TABLE transfer_pass_folders DROP COLUMN destination_revalidated_at;
                 ALTER TABLE verification_run_folders DROP COLUMN destination_revalidated_at;
                 PRAGMA user_version=37;",
            )
            .unwrap();
        store.migrate().unwrap();
        for table in ["transfer_pass_folders", "verification_run_folders"] {
            let columns: i64 = store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name='destination_revalidated_at'",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(columns, 1, "{table} should be upgraded");
        }
    }
}
