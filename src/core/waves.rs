//! Migration waves: named, ordered, approvable subsets of a project's
//! mailboxes (Pilot, Wave 1 - Accounting, Final catch-up, ...).
//!
//! A wave stores only what an operator decides: its members, order, planned
//! start, maintenance window, concurrency, and approval. Status, progress,
//! evidence coverage, and cutover readiness are derived from the members'
//! durable state every time, so they cannot drift from the queue. A mailbox
//! belongs to at most one wave, and a wave is immutable once approved: live
//! admission refuses members of an unapproved wave.

use super::StateStore;
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;
use uuid::Uuid;

/// Members a single wave may hold; matches the durable queue ceiling.
const MAX_WAVE_MEMBERS: usize = super::MAX_DURABLE_MAILBOX_ROWS;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WaveSettings {
    pub name: String,
    /// Operator's planned start, informational (free text, bounded).
    pub scheduled_at: Option<String>,
    /// `HH:MM-HH:MM[@Mon,Tue,...]`; launches are refused outside it.
    pub maintenance_window: Option<String>,
    /// Worker ceiling for this wave; never raises the plan's own ceiling.
    pub concurrency: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wave {
    pub id: String,
    pub position: u32,
    pub settings: WaveSettings,
    pub approved_by: Option<String>,
    pub approved_at: Option<String>,
}

/// A wave with its members counted into the command-center buckets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaveSummary {
    pub wave: Wave,
    pub members: usize,
    pub completed: usize,
    pub migrating: usize,
    pub needs_attention: usize,
    pub catch_up_pending: usize,
    pub waiting: usize,
    /// Members with durable verification evidence.
    pub evidenced: usize,
}

/// Derived lifecycle of a wave, from approval and member state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveStatus {
    /// Not approved; live migration of its members is refused.
    Draft,
    /// Approved; no member has started.
    Approved,
    InProgress,
    NeedsAttention,
    /// Bulk transfer done for some members; the final catch-up remains.
    CatchUpPending,
    /// Every member is verified; ready for cutover.
    Complete,
}

impl WaveStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Approved => "approved",
            Self::InProgress => "in_progress",
            Self::NeedsAttention => "needs_attention",
            Self::CatchUpPending => "catch_up_pending",
            Self::Complete => "complete",
        }
    }
}

impl WaveSummary {
    pub fn status(&self) -> WaveStatus {
        if self.wave.approved_by.is_none() {
            WaveStatus::Draft
        } else if self.members > 0 && self.completed == self.members {
            WaveStatus::Complete
        } else if self.needs_attention > 0 {
            WaveStatus::NeedsAttention
        } else if self.migrating > 0 {
            WaveStatus::InProgress
        } else if self.catch_up_pending > 0 {
            WaveStatus::CatchUpPending
        } else if self.completed > 0 {
            WaveStatus::InProgress
        } else {
            WaveStatus::Approved
        }
    }
}

fn invalid(message: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
        Some(message.to_owned()),
    )
}

fn validate_settings(settings: &WaveSettings) -> rusqlite::Result<WaveSettings> {
    let name = settings.name.trim();
    if name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
        return Err(invalid("wave name must be 1-120 printable characters"));
    }
    let scheduled_at = settings
        .scheduled_at
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    if scheduled_at
        .as_deref()
        .is_some_and(|value| value.len() > 64 || value.chars().any(char::is_control))
    {
        return Err(invalid(
            "wave start must be at most 64 printable characters",
        ));
    }
    let maintenance_window = settings
        .maintenance_window
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    if let Some(window) = &maintenance_window {
        crate::maintenance_window::MaintenanceWindow::parse(window)
            .map_err(|_| invalid("wave maintenance window must be HH:MM-HH:MM[@Mon,Tue,...]"))?;
    }
    if settings.concurrency == Some(0) {
        return Err(invalid("wave concurrency must be at least 1"));
    }
    Ok(WaveSettings {
        name: name.to_owned(),
        scheduled_at,
        maintenance_window,
        concurrency: settings.concurrency,
    })
}

const WAVE_COLUMNS: &str = "w.id,w.position,w.name,w.scheduled_at,w.maintenance_window,w.concurrency,w.approved_by,w.approved_at";

fn wave_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Wave> {
    Ok(Wave {
        id: row.get(0)?,
        position: u32::try_from(row.get::<_, i64>(1)?).unwrap_or(u32::MAX),
        settings: WaveSettings {
            name: row.get(2)?,
            scheduled_at: row.get(3)?,
            maintenance_window: row.get(4)?,
            concurrency: row
                .get::<_, Option<i64>>(5)?
                .and_then(|value| u32::try_from(value).ok()),
        },
        approved_by: row.get(6)?,
        approved_at: row.get(7)?,
    })
}

impl StateStore {
    /// Create a wave from mailboxes of one project, appended after the
    /// existing waves. Refuses members of another project or another wave.
    pub fn create_wave(
        &self,
        project_id: &str,
        settings: &WaveSettings,
        job_ids: &[String],
    ) -> rusqlite::Result<Wave> {
        let settings = validate_settings(settings)?;
        if job_ids.is_empty() || job_ids.len() > MAX_WAVE_MEMBERS {
            return Err(invalid("a wave needs between 1 and 100,000 mailboxes"));
        }
        let tx = self.connection.unchecked_transaction()?;
        let position: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position)+1,0) FROM waves WHERE project_id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        let id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO waves(id,project_id,name,position,scheduled_at,maintenance_window,concurrency) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                id,
                project_id,
                settings.name,
                position,
                settings.scheduled_at,
                settings.maintenance_window,
                settings.concurrency.map(i64::from)
            ],
        )
        .map_err(|error| match error {
            rusqlite::Error::SqliteFailure(failure, _)
                if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                invalid("a wave with this name already exists in the project")
            }
            other => other,
        })?;
        {
            let mut project_of =
                tx.prepare_cached("SELECT project_id FROM mailbox_jobs WHERE id=?1")?;
            let mut existing = tx.prepare_cached(
                "SELECT w.name FROM wave_members m JOIN waves w ON w.id=m.wave_id WHERE m.job_id=?1",
            )?;
            let mut insert =
                tx.prepare_cached("INSERT INTO wave_members(job_id,wave_id) VALUES(?1,?2)")?;
            for job_id in job_ids {
                let owner: Option<String> = project_of
                    .query_row([job_id], |row| row.get(0))
                    .optional()?;
                if owner.as_deref() != Some(project_id) {
                    return Err(invalid(
                        "every wave member must be a mailbox of this project",
                    ));
                }
                if let Some(wave) = existing
                    .query_row([job_id], |row| row.get::<_, String>(0))
                    .optional()?
                {
                    return Err(invalid(&format!(
                        "a selected mailbox already belongs to wave \"{wave}\"; remove that wave first"
                    )));
                }
                insert.execute(params![job_id, id])?;
            }
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'wave_created',?2)",
            params![
                project_id,
                format!("{} ({} mailboxes)", settings.name, job_ids.len())
            ],
        )?;
        tx.commit()?;
        Ok(Wave {
            id,
            position: u32::try_from(position).unwrap_or(u32::MAX),
            settings,
            approved_by: None,
            approved_at: None,
        })
    }

    /// Record an operator's approval. Approval is final: an approved wave's
    /// members and settings no longer change.
    pub fn approve_wave(&self, wave_id: &str, operator: &str) -> rusqlite::Result<()> {
        let operator = operator.trim();
        if operator.is_empty() || operator.len() > 256 || operator.chars().any(char::is_control) {
            return Err(invalid("approver must be 1-256 printable characters"));
        }
        let tx = self.connection.unchecked_transaction()?;
        let (project_id, name): (String, String) = tx.query_row(
            "SELECT project_id,name FROM waves WHERE id=?1",
            [wave_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let changed = tx.execute(
            "UPDATE waves SET approved_by=?1,approved_at=CURRENT_TIMESTAMP WHERE id=?2 AND approved_by IS NULL",
            params![operator, wave_id],
        )?;
        if changed != 1 {
            return Err(invalid("this wave is already approved"));
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'wave_approved',?2)",
            params![project_id, format!("{name} approved by {operator}")],
        )?;
        tx.commit()
    }

    /// Remove an unapproved wave; its mailboxes return to the unassigned pool.
    pub fn delete_wave(&self, wave_id: &str) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let (project_id, name, approved): (String, String, Option<String>) = tx.query_row(
            "SELECT project_id,name,approved_by FROM waves WHERE id=?1",
            [wave_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if approved.is_some() {
            return Err(invalid(
                "an approved wave is an audit record and cannot be deleted",
            ));
        }
        tx.execute("DELETE FROM wave_members WHERE wave_id=?1", [wave_id])?;
        tx.execute("DELETE FROM waves WHERE id=?1", [wave_id])?;
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'wave_deleted',?2)",
            params![project_id, name],
        )?;
        tx.commit()
    }

    /// The project's waves in order, each with member counts by bucket.
    pub fn wave_summaries(&self, project_id: &str) -> rusqlite::Result<Vec<WaveSummary>> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {WAVE_COLUMNS},
                    COUNT(m.job_id),
                    COALESCE(SUM(j.state='verified' OR (j.state='verified_with_exceptions' AND EXISTS(SELECT 1 FROM verification_acceptances a WHERE a.job_id=j.id))),0),
                    COALESCE(SUM(j.state IN ('preflight','running','completed')),0),
                    COALESCE(SUM(j.state IN ('attention','failed','verification_difference')),0),
                    COALESCE(SUM(j.state='delta_required'),0),
                    COALESCE(SUM(e.job_id IS NOT NULL),0)
             FROM waves w
             LEFT JOIN wave_members m ON m.wave_id=w.id
             LEFT JOIN mailbox_jobs j ON j.id=m.job_id
             LEFT JOIN evidence e ON e.job_id=m.job_id
             WHERE w.project_id=?1
             GROUP BY w.id ORDER BY w.position"
        ))?;
        statement
            .query_map([project_id], |row| {
                let count = |index: usize| -> rusqlite::Result<usize> {
                    Ok(usize::try_from(row.get::<_, i64>(index)?).unwrap_or(0))
                };
                let members = count(8)?;
                let completed = count(9)?;
                let migrating = count(10)?;
                let needs_attention = count(11)?;
                let catch_up_pending = count(12)?;
                Ok(WaveSummary {
                    wave: wave_from_row(row)?,
                    members,
                    completed,
                    migrating,
                    needs_attention,
                    catch_up_pending,
                    waiting: members
                        .saturating_sub(completed + migrating + needs_attention + catch_up_pending),
                    evidenced: count(13)?,
                })
            })?
            .collect()
    }

    pub fn wave_member_ids(&self, wave_id: &str) -> rusqlite::Result<Vec<String>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT m.job_id FROM wave_members m JOIN mailbox_jobs j ON j.id=m.job_id WHERE m.wave_id=?1 ORDER BY j.rowid",
        )?;
        statement.query_map([wave_id], |row| row.get(0))?.collect()
    }

    pub fn wave(&self, wave_id: &str) -> rusqlite::Result<Option<Wave>> {
        self.connection
            .query_row(
                &format!("SELECT {WAVE_COLUMNS} FROM waves w WHERE w.id=?1"),
                [wave_id],
                wave_from_row,
            )
            .optional()
    }

    /// Mailboxes of the project that belong to an unapproved wave, mapped to
    /// that wave's name. Live admission refuses them.
    pub fn unapproved_wave_members(
        &self,
        project_id: &str,
    ) -> rusqlite::Result<HashMap<String, String>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT m.job_id,w.name FROM wave_members m JOIN waves w ON w.id=m.wave_id
             WHERE w.project_id=?1 AND w.approved_by IS NULL",
        )?;
        statement
            .query_map([project_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{WaveSettings, WaveStatus};
    use crate::core::StateStore;

    fn settings(name: &str) -> WaveSettings {
        WaveSettings {
            name: name.into(),
            ..WaveSettings::default()
        }
    }

    fn project_with_mailboxes(store: &StateStore, count: usize) -> (String, Vec<String>) {
        let (project, first) = store
            .create_project_with_mailbox("waves", "source", "destination", "u0", "u0")
            .unwrap();
        let mut ids = vec![first];
        for index in 1..count {
            ids.push(
                store
                    .add_mailbox(&project.id, &format!("u{index}"), &format!("u{index}"))
                    .unwrap(),
            );
        }
        (project.id, ids)
    }

    #[test]
    fn waves_are_ordered_exclusive_and_immutable_once_approved() {
        let store = StateStore::in_memory().unwrap();
        let (project, ids) = project_with_mailboxes(&store, 5);
        let pilot = store
            .create_wave(&project, &settings("Pilot"), &ids[..1])
            .unwrap();
        let accounting = store
            .create_wave(
                &project,
                &WaveSettings {
                    name: "Wave 1 - Accounting".into(),
                    scheduled_at: Some("Sat 22:00".into()),
                    maintenance_window: Some("22:00-04:00@Sat,Sun".into()),
                    concurrency: Some(4),
                },
                &ids[1..3],
            )
            .unwrap();
        assert_eq!((pilot.position, accounting.position), (0, 1));
        // A mailbox belongs to at most one wave.
        let error = store
            .create_wave(&project, &settings("Overlap"), &ids[2..4])
            .unwrap_err();
        assert!(error.to_string().contains("already belongs"), "{error}");
        // Names are unique per project.
        assert!(
            store
                .create_wave(&project, &settings("Pilot"), &ids[4..])
                .is_err()
        );
        // Bad settings are refused before anything is written.
        for bad in [
            WaveSettings {
                name: " ".into(),
                ..WaveSettings::default()
            },
            WaveSettings {
                maintenance_window: Some("whenever".into()),
                ..settings("W")
            },
            WaveSettings {
                concurrency: Some(0),
                ..settings("W")
            },
        ] {
            assert!(
                store.create_wave(&project, &bad, &ids[4..]).is_err(),
                "{bad:?}"
            );
        }

        assert_eq!(
            store.unapproved_wave_members(&project).unwrap().len(),
            3,
            "every member of an unapproved wave is gated"
        );
        store.approve_wave(&pilot.id, "operator").unwrap();
        assert!(store.approve_wave(&pilot.id, "operator").is_err());
        assert!(
            store.delete_wave(&pilot.id).is_err(),
            "approved waves are records"
        );
        let gated = store.unapproved_wave_members(&project).unwrap();
        assert_eq!(gated.len(), 2);
        assert!(gated.values().all(|name| name == "Wave 1 - Accounting"));

        store.delete_wave(&accounting.id).unwrap();
        assert!(store.unapproved_wave_members(&project).unwrap().is_empty());
        assert_eq!(store.wave_member_ids(&pilot.id).unwrap(), ids[..1]);
        store.validate_internal_invariants().unwrap();
    }

    #[test]
    fn wave_status_is_derived_from_member_state() {
        let store = StateStore::in_memory().unwrap();
        let (project, ids) = project_with_mailboxes(&store, 3);
        let wave = store
            .create_wave(&project, &settings("Wave"), &ids)
            .unwrap();
        let status = |store: &StateStore| store.wave_summaries(&project).unwrap()[0].status();
        assert_eq!(status(&store), WaveStatus::Draft);
        store.approve_wave(&wave.id, "operator").unwrap();
        assert_eq!(status(&store), WaveStatus::Approved);
        store.force_mailbox_state(&ids[0], "running").unwrap();
        assert_eq!(status(&store), WaveStatus::InProgress);
        store
            .force_mailbox_state(&ids[0], "delta_required")
            .unwrap();
        assert_eq!(status(&store), WaveStatus::CatchUpPending);
        store.force_mailbox_state(&ids[1], "failed").unwrap();
        assert_eq!(status(&store), WaveStatus::NeedsAttention);
        for id in &ids {
            store.force_mailbox_state(id, "verified").unwrap();
        }
        let summary = &store.wave_summaries(&project).unwrap()[0];
        assert_eq!(summary.status(), WaveStatus::Complete);
        assert_eq!(
            (summary.members, summary.completed, summary.waiting),
            (3, 3, 0)
        );
    }
}
