//! Durable reads for the recovery workspace: mailboxes that need an
//! operator decision, grouped by state and durable attention reason, with
//! enough of their recent history to decide what to do next.

use super::{AttentionReason, StateStore};
use rusqlite::params;

/// One group of mailboxes sharing a state and attention reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryGroup {
    pub state: String,
    pub reason: Option<AttentionReason>,
    pub count: usize,
}

/// One mailbox of a group, with its most recent run and transfer attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryRow {
    pub rowid: i64,
    pub id: String,
    pub label: String,
    pub source_user: String,
    pub destination_user: String,
    /// Classified, redacted detail of the mailbox's latest run.
    pub last_run_detail: Option<String>,
    pub last_run_finished_at: Option<String>,
    /// Outcome of the latest recorded transfer attempt, if any.
    pub last_attempt_outcome: Option<String>,
    /// Latest bounded engine-progress checkpoint, if one was durably seen.
    pub last_progress_checkpoint: Option<String>,
}

fn reason_value(reason: Option<AttentionReason>) -> Option<&'static str> {
    reason.map(AttentionReason::as_str)
}

impl StateStore {
    /// Groups of mailboxes whose state leaves work unfinished (attention,
    /// failed, cancelled, verification difference, delta required), largest
    /// first.
    pub fn recovery_groups(&self, project_id: &str) -> rusqlite::Result<Vec<RecoveryGroup>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT state,attention_reason,COUNT(*) FROM mailbox_jobs WHERE project_id=?1 AND state IN ('attention','failed','cancelled','verification_difference','delta_required') GROUP BY state,attention_reason ORDER BY COUNT(*) DESC,state,attention_reason",
        )?;
        statement
            .query_map([project_id], |row| {
                Ok(RecoveryGroup {
                    state: row.get(0)?,
                    reason: row.get::<_, Option<String>>(1)?.map(|value| {
                        AttentionReason::parse(&value).unwrap_or(AttentionReason::Unknown)
                    }),
                    count: row.get::<_, i64>(2)? as usize,
                })
            })?
            .collect()
    }

    /// A page of one group's mailboxes in queue order, after `after_rowid`.
    pub fn recovery_rows(
        &self,
        project_id: &str,
        state: &str,
        reason: Option<AttentionReason>,
        after_rowid: Option<i64>,
        limit: u32,
    ) -> rusqlite::Result<Vec<RecoveryRow>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT m.rowid,m.id,COALESCE(f.label,m.source_mailbox || ' → ' || m.destination_mailbox),m.source_mailbox,m.destination_mailbox,
                    (SELECT r.detail FROM runs r WHERE r.job_id=m.id ORDER BY r.started_at DESC,r.rowid DESC LIMIT 1),
                    (SELECT r.finished_at FROM runs r WHERE r.job_id=m.id ORDER BY r.started_at DESC,r.rowid DESC LIMIT 1),
                    (SELECT tp.outcome FROM transfer_passes tp JOIN runs r ON r.id=tp.run_id WHERE r.job_id=m.id ORDER BY tp.started_at DESC,tp.rowid DESC LIMIT 1)
                    ,(SELECT e.detail FROM events e WHERE e.kind='transfer_progress_checkpoint' AND e.run_id IN (SELECT r.id FROM runs r WHERE r.job_id=m.id) ORDER BY e.created_at DESC,e.id DESC LIMIT 1)
             FROM mailbox_jobs m LEFT JOIN mailbox_queue_facts f ON f.job_rowid=m.rowid
             WHERE m.project_id=?1 AND m.state=?2 AND m.attention_reason IS ?3 AND m.rowid>?4
             ORDER BY m.rowid LIMIT ?5",
        )?;
        statement
            .query_map(
                params![
                    project_id,
                    state,
                    reason_value(reason),
                    after_rowid.unwrap_or(0),
                    limit.min(500)
                ],
                |row| {
                    Ok(RecoveryRow {
                        rowid: row.get(0)?,
                        id: row.get(1)?,
                        label: row.get(2)?,
                        source_user: row.get(3)?,
                        destination_user: row.get(4)?,
                        last_run_detail: row
                            .get::<_, Option<String>>(5)?
                            .filter(|detail| !detail.is_empty()),
                        last_run_finished_at: row.get(6)?,
                        last_attempt_outcome: row.get(7)?,
                        last_progress_checkpoint: row.get(8)?,
                    })
                },
            )?
            .collect()
    }

    /// Every job ID of one group, for selecting the group.
    pub fn recovery_group_ids(
        &self,
        project_id: &str,
        state: &str,
        reason: Option<AttentionReason>,
    ) -> rusqlite::Result<Vec<String>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id FROM mailbox_jobs WHERE project_id=?1 AND state=?2 AND attention_reason IS ?3 ORDER BY rowid",
        )?;
        statement
            .query_map(params![project_id, state, reason_value(reason)], |row| {
                row.get(0)
            })?
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_rows_and_ids_follow_state_and_durable_reason() {
        let store = StateStore::in_memory().unwrap();
        let project = store.create_project("recovery", "a", "b").unwrap();
        let ids = (0..6)
            .map(|index| {
                store
                    .add_mailbox(&project.id, &format!("s{index}"), &format!("d{index}"))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for (index, (state, reason)) in [
            ("attention", Some("transport_failed")),
            ("attention", Some("transport_failed")),
            ("attention", Some("authentication_failed")),
            ("delta_required", None),
            ("ready", None),
            ("failed", None),
        ]
        .into_iter()
        .enumerate()
        {
            store
                .connection
                .execute(
                    "UPDATE mailbox_jobs SET state=?2,attention_reason=?3 WHERE id=?1",
                    params![ids[index], state, reason],
                )
                .unwrap();
        }
        store
            .insert_run_for_test(&project.id, Some(&ids[0]), "run-0", "imapsync")
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE runs SET detail='[class=transport] timed out' WHERE id='run-0'",
                [],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,'run-0','transfer_progress_checkpoint','{\"attempt\":1,\"progress\":{\"messages_copied\":42}}')",
                [&project.id],
            )
            .unwrap();

        let groups = store.recovery_groups(&project.id).unwrap();
        assert_eq!(groups.len(), 4);
        assert_eq!(
            (groups[0].state.as_str(), groups[0].reason, groups[0].count),
            ("attention", Some(AttentionReason::TransportFailed), 2)
        );
        assert!(groups.iter().all(|group| group.state != "ready"));
        assert!(
            groups
                .iter()
                .any(|group| group.state == "delta_required" && group.reason.is_none())
        );

        let transport = Some(AttentionReason::TransportFailed);
        let page = store
            .recovery_rows(&project.id, "attention", transport, None, 1)
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, ids[0]);
        assert_eq!(
            page[0].last_run_detail.as_deref(),
            Some("[class=transport] timed out")
        );
        assert_eq!(
            page[0].last_progress_checkpoint.as_deref(),
            Some("{\"attempt\":1,\"progress\":{\"messages_copied\":42}}")
        );
        assert_eq!(page[0].label, "s0 → d0");
        let next = store
            .recovery_rows(&project.id, "attention", transport, Some(page[0].rowid), 10)
            .unwrap();
        assert_eq!(
            next.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
            [ids[1].clone()]
        );
        assert_eq!(
            store
                .recovery_group_ids(&project.id, "delta_required", None)
                .unwrap(),
            [ids[3].clone()]
        );
    }
}
