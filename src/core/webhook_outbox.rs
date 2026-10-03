//! Durable, credential-free webhook delivery outbox.
//!
//! Endpoint URLs and authentication material are deliberately not stored here.
//! The operator supplies the endpoint at delivery time; the outbox binds a
//! queued event to an endpoint digest so a retry cannot silently go to a
//! different destination.

use super::*;
use rusqlite::{OptionalExtension, params};

pub(crate) const MAX_WEBHOOK_PAYLOAD_BYTES: usize = 128 * 1024;
pub(crate) const MAX_WEBHOOK_ATTEMPTS: u32 = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WebhookDelivery {
    pub(crate) event_id: String,
    pub(crate) project_id: String,
    pub(crate) event_type: String,
    pub(crate) payload: String,
    pub(crate) endpoint_digest: String,
    pub(crate) attempts: u32,
    pub(crate) status: String,
    pub(crate) last_error: Option<String>,
}

impl StateStore {
    /// Insert one delivery exactly once. Re-enqueuing the same event is
    /// idempotent, but a changed payload or endpoint is treated as corruption
    /// rather than silently replacing an already-audited delivery.
    pub(crate) fn enqueue_webhook_delivery(
        &self,
        event_id: &str,
        project_id: &str,
        event_type: &str,
        payload: &str,
        endpoint_digest: &str,
    ) -> rusqlite::Result<()> {
        if event_id.is_empty()
            || project_id.is_empty()
            || event_type.is_empty()
            || (!endpoint_digest.is_empty() && endpoint_digest.len() != 64)
            || payload.len() > MAX_WEBHOOK_PAYLOAD_BYTES
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let inserted = self.connection.execute(
            "INSERT OR IGNORE INTO webhook_deliveries(event_id,project_id,event_type,payload,endpoint_digest) VALUES(?1,?2,?3,?4,?5)",
            params![event_id, project_id, event_type, payload, endpoint_digest],
        )?;
        if inserted == 0 {
            let existing: Option<(String, String, String)> = self
                .connection
                .query_row(
                    "SELECT payload,endpoint_digest,event_type FROM webhook_deliveries WHERE event_id=?1",
                    [event_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            if existing
                != Some((
                    payload.to_owned(),
                    endpoint_digest.to_owned(),
                    event_type.to_owned(),
                ))
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        Ok(())
    }

    pub(crate) fn due_webhook_deliveries(
        &self,
        endpoint_digest: &str,
        limit: u32,
    ) -> rusqlite::Result<Vec<WebhookDelivery>> {
        let mut statement = self.connection.prepare(
            "SELECT event_id,project_id,event_type,payload,endpoint_digest,attempts,status,last_error FROM webhook_deliveries WHERE endpoint_digest=?1 AND status='queued' AND next_attempt_at<=CURRENT_TIMESTAMP ORDER BY created_at,event_id LIMIT ?2",
        )?;
        statement
            .query_map(params![endpoint_digest, i64::from(limit.min(100))], |row| {
                Ok(WebhookDelivery {
                    event_id: row.get(0)?,
                    project_id: row.get(1)?,
                    event_type: row.get(2)?,
                    payload: row.get(3)?,
                    endpoint_digest: row.get(4)?,
                    attempts: u32::try_from(row.get::<_, i64>(5)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    status: row.get(6)?,
                    last_error: row.get(7)?,
                })
            })?
            .collect()
    }

    pub(crate) fn bind_unbound_webhook_deliveries(
        &self,
        endpoint_digest: &str,
    ) -> rusqlite::Result<()> {
        if endpoint_digest.len() != 64 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.connection.execute(
            "UPDATE webhook_deliveries SET endpoint_digest=?1 WHERE endpoint_digest='' AND status='queued'",
            [endpoint_digest],
        )?;
        Ok(())
    }

    pub(crate) fn mark_webhook_delivered(&self, event_id: &str) -> rusqlite::Result<()> {
        let changed = self.connection.execute(
            "UPDATE webhook_deliveries SET status='delivered',delivered_at=CURRENT_TIMESTAMP,last_error=NULL WHERE event_id=?1 AND status='queued'",
            [event_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(())
    }

    pub(crate) fn mark_webhook_failed(&self, event_id: &str, error: &str) -> rusqlite::Result<()> {
        let attempts: Option<u32> = self
            .connection
            .query_row(
                "SELECT attempts FROM webhook_deliveries WHERE event_id=?1 AND status='queued'",
                [event_id],
                |row| {
                    row.get::<_, i64>(0)
                        .map(|value| u32::try_from(value).unwrap_or(u32::MAX))
                },
            )
            .optional()?;
        let Some(attempts) = attempts else {
            return Err(rusqlite::Error::InvalidQuery);
        };
        let next_attempts = attempts.saturating_add(1);
        let dead_letter = next_attempts >= MAX_WEBHOOK_ATTEMPTS;
        let delay = 2_u64.pow(next_attempts.min(10));
        let bounded_error = crate::storage::bounded_event_detail(error);
        self.connection.execute(
            "UPDATE webhook_deliveries SET attempts=?2,status=CASE WHEN ?3 THEN 'dead_letter' ELSE 'queued' END,last_error=?4,next_attempt_at=datetime('now',?5) WHERE event_id=?1 AND status='queued'",
            params![event_id, i64::from(next_attempts), dead_letter, bounded_error, format!("+{delay} seconds")],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_WEBHOOK_ATTEMPTS, StateStore};
    use std::{fs, path::PathBuf};

    fn test_store() -> (StateStore, PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-outbox-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = directory.join("state.db");
        let store = StateStore::open(&path).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES('project','test','source','destination','discovery')",
                [],
            )
            .unwrap();
        (store, directory)
    }

    #[test]
    fn outbox_enqueue_is_idempotent_and_endpoint_bound() {
        let (store, directory) = test_store();
        store
            .enqueue_webhook_delivery(
                "event",
                "project",
                "migration.status_snapshot",
                "{}",
                &"a".repeat(64),
            )
            .unwrap();
        store
            .enqueue_webhook_delivery(
                "event",
                "project",
                "migration.status_snapshot",
                "{}",
                &"a".repeat(64),
            )
            .unwrap();
        assert_eq!(
            store
                .due_webhook_deliveries(&"a".repeat(64), 10)
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .enqueue_webhook_delivery(
                    "event",
                    "project",
                    "migration.status_snapshot",
                    "changed",
                    &"a".repeat(64)
                )
                .is_err()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn outbox_failures_back_off_then_dead_letter() {
        let (store, directory) = test_store();
        let digest = "b".repeat(64);
        store
            .enqueue_webhook_delivery(
                "event",
                "project",
                "migration.status_snapshot",
                "{}",
                &digest,
            )
            .unwrap();
        for attempt in 1..=MAX_WEBHOOK_ATTEMPTS {
            store
                .mark_webhook_failed("event", "endpoint unavailable")
                .unwrap();
            if attempt < MAX_WEBHOOK_ATTEMPTS {
                store
                    .connection
                    .execute("UPDATE webhook_deliveries SET next_attempt_at=CURRENT_TIMESTAMP WHERE event_id='event'", [])
                    .unwrap();
            }
        }
        let row = store
            .connection
            .query_row(
                "SELECT status,attempts FROM webhook_deliveries WHERE event_id='event'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            ("dead_letter".to_owned(), i64::from(MAX_WEBHOOK_ATTEMPTS))
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn committed_lifecycle_events_enter_the_unbound_outbox() {
        let (store, directory) = test_store();
        store
            .record_event("project", "phase_changed", "complete")
            .unwrap();
        let deliveries = store.due_webhook_deliveries("", 10).unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].event_type, "migration.completed");
        assert!(deliveries[0].payload.contains("migration.completed"));
        fs::remove_dir_all(directory).unwrap();
    }
}
