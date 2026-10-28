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

/// Validate the workflow/phase binding and the operator's current
/// source-quiescence attestation for both Final Delta and final Verification.
/// This is called inside durable run-admission transactions as well as by the
/// headless preflight, so UI and batch callers cannot bypass managed cutover
/// rules.
pub(crate) fn validate_cutover_run_stage(
    stage: &str,
    phase: &str,
    source_quiescence_at: Option<&str>,
) -> Result<(), String> {
    if stage != phase {
        return Err("managed cutover stage does not match the project phase".into());
    }
    if stage != CutoverStage::FinalDelta.as_str() && stage != CutoverStage::Verification.as_str() {
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
        "SELECT COUNT(*),COALESCE(SUM(state='verified'),0),COALESCE(SUM(state='verified_with_exceptions' AND EXISTS(SELECT 1 FROM verification_acceptances a JOIN evidence e ON e.job_id=a.job_id AND e.run_id=a.run_id WHERE a.job_id=mailbox_jobs.id)),0) FROM mailbox_jobs WHERE project_id=?1",
        [project_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if total == 0 || total != clean_verified + accepted_exceptions {
        return Ok(());
    }
    let Some((independently_revalidated, engine_or_aggregate_only)) =
        final_verification_evidence_summary(tx, project_id)?
    else {
        return Ok(());
    };
    let latest_ready_detail: Option<String> = tx
        .query_row(
            "SELECT detail FROM events WHERE project_id=?1 AND kind='cutover_ready' ORDER BY id DESC LIMIT 1",
            [project_id],
            |row| row.get(0),
        )
        .optional()?;
    if latest_ready_detail
        .as_deref()
        .is_some_and(cutover_ready_event_is_current)
    {
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
        "final verification complete; mailboxes={total}; clean_verified={clean_verified}; accepted_exceptions={accepted_exceptions}; independent_cursor_revalidated={independently_revalidated}; engine_or_aggregate_only={engine_or_aggregate_only}; source-quiescence attestation valid until {valid_until}; external cutover approval pending"
    );
    tx.execute(
        "INSERT INTO events(project_id,kind,detail) VALUES(?1,'cutover_ready',?2)",
        params![project_id, detail],
    )?;
    Ok(())
}

/// Cutover readiness must be tied to the current Verification-stage evidence,
/// not merely to a mailbox state string. Independent IMAP evidence additionally
/// requires its persisted post-scan source and destination cursor checks. The
/// Dovecot/engine evidence paths do not expose those IMAP observations, so keep
/// them explicitly counted as engine/aggregate-only rather than implying that
/// they received cursor revalidation.
fn final_verification_evidence_summary(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
) -> rusqlite::Result<Option<(i64, i64)>> {
    let mut statement = tx.prepare(
        "SELECT j.id,e.run_id,e.verification_method,e.verification_outcome,r.phase_at_start,r.status,e.source_folders,e.destination_folders FROM mailbox_jobs j LEFT JOIN evidence e ON e.job_id=j.id LEFT JOIN runs r ON r.id=e.run_id WHERE j.project_id=?1 AND j.state IN ('verified','verified_with_exceptions') ORDER BY j.id",
    )?;
    let mut rows = statement.query([project_id])?;

    let mut independently_revalidated = 0_i64;
    let mut engine_or_aggregate_only = 0_i64;
    while let Some(row) = rows.next()? {
        let (run_id, method, outcome, phase, run_status, source_folders, destination_folders) = (
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<i64>>(7)?,
        );
        let (
            Some(run_id),
            Some(method),
            Some(outcome),
            Some(phase),
            Some(run_status),
            Some(source_folders),
            Some(destination_folders),
        ) = (
            run_id,
            method,
            outcome,
            phase,
            run_status,
            source_folders,
            destination_folders,
        )
        else {
            return Ok(None);
        };
        if phase != CutoverStage::Verification.as_str() || run_status != "completed" {
            return Ok(None);
        }
        match method.as_str() {
            "metadata_reconciliation" | "body_hash" => {
                let pass_attempt: Option<i64> = tx
                    .query_row(
                        "SELECT attempt FROM transfer_passes WHERE run_id=?1 AND verification_method=?2 AND verification_outcome=?3 AND verified_at IS NOT NULL ORDER BY pass_sequence DESC LIMIT 1",
                        params![run_id, method, outcome],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(pass_attempt) = pass_attempt else {
                    return Ok(None);
                };
                let valid_side = |side: i64, expected: i64| {
                    tx.query_row(
                        "SELECT COUNT(*),COALESCE(SUM(complete=1 AND scan_completed_at IS NOT NULL AND ((?3=0 AND source_revalidated_at >= (SELECT MAX(scan_completed_at) FROM transfer_pass_folders WHERE run_id=?1 AND attempt=?2 AND side=1)) OR (?3=1 AND destination_revalidated_at >= (SELECT MAX(scan_completed_at) FROM transfer_pass_folders WHERE run_id=?1 AND attempt=?2 AND side=0)))),0) FROM transfer_pass_folders WHERE run_id=?1 AND attempt=?2 AND side=?3",
                        params![run_id, pass_attempt, side],
                        |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                        },
                    )
                    .map(|(observed, validated)| observed == expected && validated == expected)
                };
                if source_folders < 0
                    || destination_folders < 0
                    || !valid_side(0, source_folders)?
                    || !valid_side(1, destination_folders)?
                {
                    return Ok(None);
                }
                independently_revalidated += 1;
            }
            "aggregate_engine" | "native_dovecot" => engine_or_aggregate_only += 1,
            _ => return Ok(None),
        }
    }
    Ok(Some((independently_revalidated, engine_or_aggregate_only)))
}

fn cutover_ready_event_is_current(detail: &str) -> bool {
    let Some(expiry) = detail
        .split_once("source-quiescence attestation valid until ")
        .and_then(|(_, suffix)| suffix.split_whitespace().next())
        .and_then(|value| {
            chrono::DateTime::parse_from_rfc3339(value.trim_end_matches([';', ','])).ok()
        })
    else {
        return false;
    };
    expiry > chrono::Utc::now()
}

/// Publish the final-verification lifecycle event only after the operator has
/// reconfirmed source quiescence at the Verification stage. Entering the stage
/// alone is not authorization to scan a source that may have resumed delivery.
fn maybe_emit_cutover_verification_started(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
) -> rusqlite::Result<()> {
    let stage: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT c.stage,p.phase,c.source_quiescence_at FROM cutover_workflows c JOIN projects p ON p.id=c.project_id WHERE c.project_id=?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((stage, phase, source_quiescence_at)) = stage else {
        return Ok(());
    };
    if stage != CutoverStage::Verification.as_str()
        || phase != CutoverStage::Verification.as_str()
        || source_quiescence_error(source_quiescence_at.as_deref()).is_some()
    {
        return Ok(());
    }
    let already_published: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE project_id=?1 AND kind='cutover_verification_started')",
        [project_id],
        |row| row.get(0),
    )?;
    if already_published {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO events(project_id,kind,detail) VALUES(?1,'cutover_verification_started','final delta complete; source quiescence reconfirmed for final verification')",
        [project_id],
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

    /// Record the operator's source-quiescence attestation before a Final Delta
    /// or final Verification pass. The evidence is intentionally explicit that
    /// this is a manual operational claim, not machine-verified source
    /// stability.
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
            "UPDATE cutover_workflows SET source_quiescence_operator=?1,source_quiescence_evidence=?2,source_quiescence_at=CURRENT_TIMESTAMP WHERE project_id=?3 AND stage IN ('catch_up','final_delta','verification') AND approved_by IS NOT NULL",
            params![operator, evidence, project_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'cutover_source_quiescence_attested',?2)",
            params![project_id, format!("operator={operator}; evidence={evidence}")],
        )?;
        maybe_emit_cutover_verification_started(&tx, project_id)?;
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
            "SELECT COUNT(*),COALESCE(SUM(state='verified'),0),COALESCE(SUM(state='verified_with_exceptions' AND EXISTS(SELECT 1 FROM verification_acceptances a JOIN evidence e ON e.job_id=a.job_id AND e.run_id=a.run_id WHERE a.job_id=mailbox_jobs.id)),0) FROM mailbox_jobs WHERE project_id=?1",
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
        if next == CutoverStage::Completed
            && final_verification_evidence_summary(&tx, project_id)?.is_none()
        {
            return Err(CutoverAdvanceError::Refused(
                "final cutover requires completed Verification-stage evidence with current folder observations".into(),
            ));
        }
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
        tx.execute(
            "UPDATE cutover_workflows SET stage=?1,external_confirmation=COALESCE(?2,external_confirmation),source_quiescence_operator=CASE WHEN ?1='verification' THEN NULL ELSE source_quiescence_operator END,source_quiescence_evidence=CASE WHEN ?1='verification' THEN NULL ELSE source_quiescence_evidence END,source_quiescence_at=CASE WHEN ?1='verification' THEN NULL ELSE source_quiescence_at END WHERE project_id=?3",
            params![next.as_str(), confirmation, project_id],
        )?;
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
            // The Verification-stage signal is emitted only after a new
            // source-quiescence attestation is recorded in this stage.
            CutoverStage::Verification => {}
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
        let refused = store
            .claim_batch_mailbox_for_child(
                &project.id,
                &job,
                "delayed-final-delta-batch",
                &children[0],
            )
            .unwrap_err();
        // A policy refusal, named for the operator, not a storage failure.
        assert!(super::super::is_ledger_rejection(&refused));
        assert!(
            refused.to_string().contains("older than 15 minutes"),
            "{refused}"
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
    fn stale_catch_up_attestation_can_be_refreshed_before_final_delta() {
        let store = StateStore::in_memory().unwrap();
        let (project, job) = store
            .create_project_with_mailbox(
                "catch-up-attestation-refresh",
                "source",
                "destination",
                "a",
                "a",
            )
            .unwrap();
        store.transition(&project.id, Phase::Preflight).unwrap();
        store
            .create_cutover_workflow(&project.id, "2026-10-15T23:00:00Z", None)
            .unwrap();
        store.approve_cutover(&project.id, "operator").unwrap();
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
            .attest_cutover_source_quiescence(&project.id, "operator", "Initial pause")
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE cutover_workflows SET source_quiescence_at=datetime('now','-16 minutes') WHERE project_id=?1",
                [&project.id],
            )
            .unwrap();

        store
            .attest_cutover_source_quiescence(
                &project.id,
                "operator",
                "Delivery pause reconfirmed before final delta",
            )
            .expect("an expired Catch-up attestation must be refreshable in place");
        assert_eq!(
            store.advance_cutover(&project.id, None).unwrap(),
            CutoverStage::FinalDelta
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
                "INSERT INTO runs(id,project_id,job_id,engine,phase_at_start,status,finished_at) VALUES('final-verification-evidence',?1,?2,'imapsync','verification','completed',CURRENT_TIMESTAMP)",
                rusqlite::params![project.id, job],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO evidence(job_id,run_id,verification_method,verification_outcome,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative) VALUES(?1,'final-verification-evidence','metadata_reconciliation','exact_metadata_match',1,1,128,128,0,0,1,1,0)",
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
        assert!(
            store
                .begin_run(
                    &project.id,
                    &job,
                    "verification-without-reconfirmation",
                    "imapsync"
                )
                .is_err(),
            "the pre-final-delta attestation must not authorize final verification"
        );
        assert!(
            store
                .begin_batch_run_with_children(
                    &project.id,
                    std::slice::from_ref(&job),
                    "verification-batch-without-reconfirmation",
                    "imapsync",
                    &[],
                    "",
                    &[],
                )
                .is_err(),
            "batch admission must enforce the same post-delta barrier"
        );
        assert!(
            store
                .begin_verification_run(
                    &project.id,
                    &job,
                    "reverify-without-reconfirmation",
                    "historical-plan-snapshot",
                )
                .is_err(),
            "standalone verification must enforce the managed cutover barrier too"
        );
        let verification_started_before_reconfirmation: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_verification_started'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(verification_started_before_reconfirmation, 0);
        store
            .attest_cutover_source_quiescence(
                &project.id,
                "operator",
                "Reconfirmed source delivery remains paused after final delta",
            )
            .unwrap();
        let verification_started_after_reconfirmation: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_verification_started'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(verification_started_after_reconfirmation, 1);
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
        let without_cursor_revalidation: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_ready'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            without_cursor_revalidation, 0,
            "metadata evidence without persisted source/destination cursor revalidation is not cutover-ready"
        );
        store
            .connection
            .execute(
                "INSERT INTO transfer_passes(run_id,attempt,project_id,mailbox_digest,pass_sequence,pass_kind,engine,executable_identity,command_sha256,command,folder_scope,source_range,finished_at,outcome,verification_method,verification_outcome,verified_at) VALUES('final-verification-evidence',1,?1,?2,1,'imapsync_sync','imapsync','sha256:test',?3,'[]','all_selectable_folders','full_mailbox',CURRENT_TIMESTAMP,'completed','metadata_reconciliation','exact_metadata_match',CURRENT_TIMESTAMP)",
                rusqlite::params![project.id, "a".repeat(64), "b".repeat(64)],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO transfer_pass_folders(run_id,attempt,side,folder_digest,uidvalidity,uidnext,exists_count,verified_through_uid,staged_messages,complete,scan_started_at,scan_completed_at,source_revalidated_at,destination_revalidated_at) VALUES('final-verification-evidence',1,0,?1,1,2,1,1,1,1,'2026-10-15T10:00:00.000Z','2026-10-15T10:00:00.000Z','2026-10-15T10:02:00.000Z',NULL),('final-verification-evidence',1,1,?1,1,2,1,1,1,1,'2026-10-15T10:01:00.000Z','2026-10-15T10:01:00.000Z',NULL,'2026-10-15T10:03:00.000Z')",
                [crate::core::folder_digest(&project.id, "INBOX")],
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE transfer_passes SET verification_outcome='exact_body_match' WHERE run_id='final-verification-evidence' AND attempt=1",
                [],
            )
            .unwrap();
        store
            .attest_cutover_source_quiescence(
                &project.id,
                "operator",
                "Cursor evidence exists but belongs to a different verification outcome",
            )
            .unwrap();
        let mismatched_outcome_readiness: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_ready'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mismatched_outcome_readiness, 0);
        store
            .connection
            .execute(
                "UPDATE transfer_passes SET verification_outcome='exact_metadata_match' WHERE run_id='final-verification-evidence' AND attempt=1",
                [],
            )
            .unwrap();
        store
            .attest_cutover_source_quiescence(
                &project.id,
                "operator",
                "Source cursor revalidation completed after both endpoint scans",
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
        let readiness_detail: String = store
            .connection
            .query_row(
                "SELECT detail FROM events WHERE project_id=?1 AND kind='cutover_ready' ORDER BY id DESC LIMIT 1",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(readiness_detail.contains("independent_cursor_revalidated=1"));
        assert!(readiness_detail.contains("engine_or_aggregate_only=0"));
        let repeated_ready_tx = store.connection.unchecked_transaction().unwrap();
        super::maybe_emit_cutover_ready(&repeated_ready_tx, &project.id).unwrap();
        repeated_ready_tx.commit().unwrap();
        let deduplicated_ready_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE project_id=?1 AND event_type='migration.cutover_ready'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            deduplicated_ready_count, 1,
            "a still-valid event is deduplicated"
        );
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
            .connection
            .execute(
                "UPDATE events SET detail='final verification complete; source-quiescence attestation valid until 2000-01-01T00:00:00+00:00; external cutover approval pending' WHERE project_id=?1 AND kind='cutover_ready'",
                [&project.id],
            )
            .unwrap();
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
        assert_eq!(
            cutover_ready, 2,
            "a new readiness window must have a new durable event after the prior one expires"
        );
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
    fn cutover_completion_requires_current_final_verification_evidence() {
        let store = StateStore::in_memory().unwrap();
        let (project, job) = store
            .create_project_with_mailbox(
                "cutover-final-evidence-required",
                "source",
                "destination",
                "a",
                "a",
            )
            .unwrap();
        store.transition(&project.id, Phase::Preflight).unwrap();
        store
            .create_cutover_workflow(&project.id, "2026-10-15T22:00:00Z", None)
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
                "UPDATE projects SET phase='verification' WHERE id=?1",
                [&project.id],
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE cutover_workflows SET stage='verification',source_quiescence_at=datetime('now') WHERE project_id=?1",
                [&project.id],
            )
            .unwrap();

        let error = store
            .advance_cutover(&project.id, Some("Approved MX cutover CHG-123"))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("final cutover requires completed Verification-stage evidence")
        );
        assert_eq!(
            store.project(&project.id).unwrap().unwrap().phase,
            Phase::Verification
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
        store
            .attest_cutover_source_quiescence(
                &project.id,
                "operator",
                "Source delivery remains paused after final delta",
            )
            .unwrap();
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
            .begin_verification_run(
                &project.id,
                &job,
                "cutover-audit-only-recheck",
                "historical-plan-snapshot",
            )
            .unwrap();
        store
            .finish_verification_only_run(
                &project.id,
                &job,
                "cutover-audit-only-recheck",
                super::super::VerificationOnlyRun {
                    evidence: &evidence,
                    mismatches: &[],
                    folders: &[],
                    detail: "audit-only recheck still found a difference",
                },
            )
            .unwrap();
        assert!(
            store
                .accept_verification_difference(
                    &project.id,
                    &job,
                    "operator",
                    "must be tied to the final cutover evidence",
                )
                .is_err(),
            "an audit-only recheck cannot be substituted for the cutover evidence projection"
        );
        assert_eq!(
            store.mailbox_state(&job).unwrap().as_deref(),
            Some("verification_difference")
        );

        // A fresh final-pass run replaces the projection, so its own
        // difference can be accepted and used in the cutover decision.
        store
            .begin_run(
                &project.id,
                &job,
                "cutover-exception-run-refreshed",
                "imapsync",
            )
            .unwrap();
        store
            .finish_run_for_mailbox_with_evidence(
                &project.id,
                &job,
                "cutover-exception-run-refreshed",
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
