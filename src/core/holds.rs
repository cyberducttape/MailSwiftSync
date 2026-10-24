//! Operator project holds: a durable pause that stops new mailbox launches.
//!
//! A hold is checked in the same transaction that claims a mailbox for a
//! batch child run, so every executor (the GUI, `headless`, `supervise`, and
//! a batch already in progress in another process) stops starting mailboxes
//! as soon as the hold commits. Transfers already running finish normally;
//! a hold never interrupts an engine process. Releasing the hold makes the
//! held mailboxes ordinary automation-safe work again.

use super::StateStore;
use rusqlite::{OptionalExtension, params};

const MAX_HOLD_OPERATOR_BYTES: usize = 256;
const MAX_HOLD_REASON_BYTES: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProjectHold {
    pub held_by: String,
    pub reason: String,
    pub held_at: String,
}

/// A claim the ledger refused because the project is on hold. It is a
/// ledger rejection (never a durability fault), and executors settle the
/// mailbox as cancelled so it is picked up again after the hold is released.
#[derive(Debug)]
pub(crate) struct ProjectHeld(pub(crate) String);

impl std::fmt::Display for ProjectHeld {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ProjectHeld {}

pub(crate) fn is_project_held(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::ToSqlConversionFailure(inner) if inner.is::<ProjectHeld>())
}

pub(super) fn held_refusal(hold: &ProjectHold) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(ProjectHeld(format!(
        "the project is on hold ({} by {}); no new mailboxes start until it is resumed",
        hold.reason, hold.held_by
    ))))
}

pub(super) fn project_hold_in(
    connection: &rusqlite::Connection,
    project_id: &str,
) -> rusqlite::Result<Option<ProjectHold>> {
    connection
        .query_row(
            "SELECT held_by,reason,held_at FROM project_holds WHERE project_id=?1",
            [project_id],
            |row| {
                Ok(ProjectHold {
                    held_by: row.get(0)?,
                    reason: row.get(1)?,
                    held_at: row.get(2)?,
                })
            },
        )
        .optional()
}

impl StateStore {
    pub fn project_hold(&self, project_id: &str) -> rusqlite::Result<Option<ProjectHold>> {
        project_hold_in(&self.connection, project_id)
    }

    /// Put a project on hold. Holding an already-held project is refused so
    /// the original operator and reason stay on record.
    pub fn hold_project(
        &self,
        project_id: &str,
        operator: &str,
        reason: &str,
    ) -> rusqlite::Result<ProjectHold> {
        let operator = crate::storage::single_line_audit_text(operator, MAX_HOLD_OPERATOR_BYTES)
            .ok_or_else(|| {
                super::ledger_rejection("the operator must be 1-256 printable characters")
            })?;
        let reason = crate::storage::single_line_audit_text(reason, MAX_HOLD_REASON_BYTES)
            .ok_or_else(|| {
                super::ledger_rejection("the hold reason must be 1-500 printable characters")
            })?;
        let tx = self.connection.unchecked_transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [project_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        if let Some(existing) = project_hold_in(&tx, project_id)? {
            return Err(super::ledger_rejection_owned(format!(
                "the project is already on hold by {}",
                existing.held_by
            )));
        }
        tx.execute(
            "INSERT INTO project_holds(project_id,held_by,reason) VALUES(?1,?2,?3)",
            params![project_id, operator, reason],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_held',?2)",
            params![project_id, format!("held by {operator}: {reason}")],
        )?;
        let hold = project_hold_in(&tx, project_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        tx.commit()?;
        Ok(hold)
    }

    /// Close a batch child run the hold kept from starting. The child was
    /// never claimed, so the mailbox keeps its state (for example
    /// `delta_required` stays a pending final catch-up) and becomes ordinary
    /// automation-safe work again once the hold is released.
    pub fn cancel_held_child_run(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        detail: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE runs SET status='cancelled',finished_at=CURRENT_TIMESTAMP,detail=?1 WHERE id=?2 AND project_id=?3 AND job_id=?4 AND status='queued'",
            params![crate::storage::bounded_event_detail(detail), run_id, project_id, job_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_held',?3)",
            params![
                project_id,
                run_id,
                format!("{job_id} not started: project on hold")
            ],
        )?;
        tx.commit()
    }

    /// Release a hold and return what it was. Releasing a project that is
    /// not held is refused, so a stale dashboard cannot report a resume that
    /// did not happen.
    pub fn release_project_hold(
        &self,
        project_id: &str,
        operator: &str,
    ) -> rusqlite::Result<ProjectHold> {
        let operator = crate::storage::single_line_audit_text(operator, MAX_HOLD_OPERATOR_BYTES)
            .ok_or_else(|| {
                super::ledger_rejection("the operator must be 1-256 printable characters")
            })?;
        let tx = self.connection.unchecked_transaction()?;
        let Some(hold) = project_hold_in(&tx, project_id)? else {
            return Err(super::ledger_rejection("the project is not on hold"));
        };
        tx.execute(
            "DELETE FROM project_holds WHERE project_id=?1",
            [project_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_resumed',?2)",
            params![
                project_id,
                format!("resumed by {operator}; hold by {} lifted", hold.held_by)
            ],
        )?;
        tx.commit()?;
        Ok(hold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hold_is_recorded_once_and_released_with_an_audit_trail() {
        let store = StateStore::in_memory().unwrap();
        let (project, _) = store
            .create_project_with_mailbox("hold", "source", "destination", "a", "a")
            .unwrap();
        assert_eq!(store.project_hold(&project.id).unwrap(), None);

        let hold = store
            .hold_project(&project.id, "Alice", "customer change freeze")
            .unwrap();
        assert_eq!(hold.held_by, "Alice");
        assert_eq!(hold.reason, "customer change freeze");
        assert_eq!(store.project_hold(&project.id).unwrap(), Some(hold));

        let again = store.hold_project(&project.id, "Bob", "other").unwrap_err();
        assert!(super::super::is_ledger_rejection(&again));
        assert!(again.to_string().contains("already on hold by Alice"));

        let released = store.release_project_hold(&project.id, "Bob").unwrap();
        assert_eq!(released.held_by, "Alice");
        assert_eq!(store.project_hold(&project.id).unwrap(), None);
        assert!(store.release_project_hold(&project.id, "Bob").is_err());
    }

    #[test]
    fn a_hold_stops_new_runs_and_claims_without_touching_mailbox_state() {
        let store = StateStore::in_memory().unwrap();
        let project = store
            .create_project("hold", "source", "destination")
            .unwrap();
        let first = store.add_mailbox(&project.id, "a", "a").unwrap();
        let second = store.add_mailbox(&project.id, "b", "b").unwrap();
        let children = store
            .begin_batch_run_with_children(
                &project.id,
                &[first.clone(), second.clone()],
                "parent",
                "test",
                &[],
                "batch snapshot",
                &[],
            )
            .unwrap();
        store
            .claim_batch_mailbox_for_child(&project.id, &first, "parent", &children[0])
            .unwrap();
        let before = store.mailbox_state(&second).unwrap();

        store.hold_project(&project.id, "Alice", "freeze").unwrap();
        // A batch already in progress cannot start another mailbox ...
        let refused = store
            .claim_batch_mailbox_for_child(&project.id, &second, "parent", &children[1])
            .unwrap_err();
        assert!(is_project_held(&refused));
        assert!(super::super::is_ledger_rejection(&refused));
        assert!(refused.to_string().contains("freeze by Alice"));
        // ... and the held child closes without changing its mailbox.
        store
            .cancel_held_child_run(&project.id, &second, &children[1], "held")
            .unwrap();
        assert_eq!(
            store.run_status(&children[1]).unwrap().as_deref(),
            Some("cancelled")
        );
        assert_eq!(store.mailbox_state(&second).unwrap(), before);
        // The mailbox already running is not interrupted by the hold.
        assert_eq!(
            store.mailbox_state(&first).unwrap().as_deref(),
            Some("running")
        );
        // New runs are refused until the hold is released.
        assert!(is_project_held(
            &store
                .begin_batch_run(
                    &project.id,
                    std::slice::from_ref(&second),
                    "next",
                    "test",
                    &[]
                )
                .unwrap_err()
        ));
        store.release_project_hold(&project.id, "Bob").unwrap();
        assert!(
            store
                .begin_batch_run(
                    &project.id,
                    std::slice::from_ref(&second),
                    "next",
                    "test",
                    &[]
                )
                .is_ok()
        );
    }

    #[test]
    fn supervise_skips_held_projects() {
        let store = StateStore::in_memory().unwrap();
        let (project, _) = store
            .create_project_with_mailbox("hold", "source", "destination", "a", "a")
            .unwrap();
        let before = store.projects_with_automation_safe_batch_work().unwrap();
        store.hold_project(&project.id, "Alice", "freeze").unwrap();
        let held = store.projects_with_automation_safe_batch_work().unwrap();
        assert!(!held.contains(&project.id));
        store.release_project_hold(&project.id, "Alice").unwrap();
        assert_eq!(
            store.projects_with_automation_safe_batch_work().unwrap(),
            before
        );
    }

    #[test]
    fn holds_refuse_unknown_projects_and_unprintable_audit_text() {
        let store = StateStore::in_memory().unwrap();
        assert!(matches!(
            store.hold_project("missing", "Alice", "freeze"),
            Err(rusqlite::Error::QueryReturnedNoRows)
        ));
        let (project, _) = store
            .create_project_with_mailbox("hold", "source", "destination", "a", "a")
            .unwrap();
        for (operator, reason) in [("", "freeze"), ("Alice", ""), ("Alice", "line\nbreak")] {
            assert!(store.hold_project(&project.id, operator, reason).is_err());
        }
        assert_eq!(store.project_hold(&project.id).unwrap(), None);
    }
}
