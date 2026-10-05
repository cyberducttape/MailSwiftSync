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
pub(crate) const WEBHOOK_DELIVERY_LEASE_SECONDS: u32 = 120;

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
            let existing: Option<(String, String, String, String)> = self
                .connection
                .query_row(
                    "SELECT payload,endpoint_digest,event_type,project_id FROM webhook_deliveries WHERE event_id=?1",
                    [event_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            if existing
                != Some((
                    payload.to_owned(),
                    endpoint_digest.to_owned(),
                    event_type.to_owned(),
                    project_id.to_owned(),
                ))
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        Ok(())
    }

    #[cfg(test)]
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
        project_id: Option<&str>,
    ) -> rusqlite::Result<()> {
        if endpoint_digest.len() != 64 || project_id.is_some_and(str::is_empty) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.connection.execute(
            "UPDATE webhook_deliveries SET endpoint_digest=?1 WHERE endpoint_digest='' AND status='queued' AND (?2 IS NULL OR project_id=?2)",
            params![endpoint_digest, project_id],
        )?;
        Ok(())
    }

    /// Atomically lease due rows for this endpoint. A single UPDATE ...
    /// RETURNING statement ensures concurrent workers cannot both obtain the
    /// same queued row; expired leases are reclaimed after worker crashes.
    pub(crate) fn claim_webhook_deliveries(
        &self,
        endpoint_digest: &str,
        project_id: Option<&str>,
        lease_owner: &str,
        limit: u32,
    ) -> rusqlite::Result<Vec<WebhookDelivery>> {
        if endpoint_digest.len() != 64
            || lease_owner.is_empty()
            || project_id.is_some_and(str::is_empty)
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let lease_modifier = format!("+{} seconds", WEBHOOK_DELIVERY_LEASE_SECONDS);
        let tx = self.connection.unchecked_transaction()?;
        // An expired lease means a worker crashed or hung without recording
        // an outcome. Count that as a failed attempt so a payload that
        // repeatedly crashes its worker eventually dead-letters instead of
        // being reclaimed forever.
        tx.execute(
            "UPDATE webhook_deliveries
             SET attempts=attempts+1,
                 status=CASE WHEN attempts+1>=?3 THEN 'dead_letter' ELSE 'queued' END,
                 last_error='delivery lease expired before an outcome was recorded',
                 next_attempt_at=CURRENT_TIMESTAMP,
                 lease_owner=NULL,lease_until=NULL
             WHERE endpoint_digest=?1
               AND (?2 IS NULL OR project_id=?2)
               AND status='delivering' AND lease_until<=CURRENT_TIMESTAMP",
            params![endpoint_digest, project_id, i64::from(MAX_WEBHOOK_ATTEMPTS)],
        )?;
        let mut statement = tx.prepare(
            "UPDATE webhook_deliveries
             SET status='delivering',lease_owner=?3,lease_until=datetime('now',?4)
             WHERE event_id IN (
                SELECT event_id FROM webhook_deliveries
                WHERE endpoint_digest=?1
                  AND (?2 IS NULL OR project_id=?2)
                  AND status='queued' AND next_attempt_at<=CURRENT_TIMESTAMP
                ORDER BY created_at,event_id LIMIT ?5
             )
             RETURNING event_id,project_id,event_type,payload,endpoint_digest,attempts,status,last_error",
        )?;
        let claimed = statement
            .query_map(
                params![
                    endpoint_digest,
                    project_id,
                    lease_owner,
                    lease_modifier,
                    i64::from(limit.min(100))
                ],
                |row| {
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
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        tx.commit()?;
        Ok(claimed)
    }

    pub(crate) fn mark_webhook_delivered(
        &self,
        event_id: &str,
        lease_owner: &str,
    ) -> rusqlite::Result<()> {
        let changed = self.connection.execute(
            "UPDATE webhook_deliveries SET status='delivered',delivered_at=CURRENT_TIMESTAMP,last_error=NULL,lease_owner=NULL,lease_until=NULL WHERE event_id=?1 AND status='delivering' AND lease_owner=?2",
            params![event_id, lease_owner],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(())
    }

    pub(crate) fn mark_webhook_failed(
        &self,
        event_id: &str,
        lease_owner: &str,
        error: &str,
    ) -> rusqlite::Result<()> {
        let bounded_error = crate::storage::bounded_event_detail(error);
        let changed = self.connection.execute(
            "UPDATE webhook_deliveries
             SET attempts=attempts+1,
                 status=CASE WHEN attempts+1>=?3 THEN 'dead_letter' ELSE 'queued' END,
                 last_error=?4,
                 next_attempt_at=datetime('now','+' || (2 << (min(attempts+1,10)-1)) || ' seconds'),
                 lease_owner=NULL,lease_until=NULL
             WHERE event_id=?1 AND status='delivering' AND lease_owner=?2",
            params![
                event_id,
                lease_owner,
                i64::from(MAX_WEBHOOK_ATTEMPTS),
                bounded_error
            ],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_WEBHOOK_ATTEMPTS, StateStore};
    use std::{fs, path::PathBuf};

    #[cfg(windows)]
    fn remove_test_directory(path: &std::path::Path) -> std::io::Result<()> {
        for attempt in 0..20 {
            match fs::remove_dir_all(path) {
                Ok(()) => return Ok(()),
                Err(error) if attempt < 19 && matches!(error.raw_os_error(), Some(5 | 32)) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("the bounded Windows cleanup loop always returns")
    }

    #[cfg(not(windows))]
    fn remove_test_directory(path: &std::path::Path) -> std::io::Result<()> {
        fs::remove_dir_all(path)
    }

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
        store.connection.execute(
            "INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES('project-b','test-b','source','destination','discovery')",
            [],
        ).unwrap();
        (store, directory)
    }

    fn close_test_store(store: StateStore) {
        let StateStore { connection } = store;
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
            .unwrap();
        connection.close().unwrap();
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
        assert!(
            store
                .enqueue_webhook_delivery(
                    "event",
                    "project-b",
                    "migration.status_snapshot",
                    "{}",
                    &"a".repeat(64)
                )
                .is_err(),
            "an idempotency-key collision must not silently transfer project ownership"
        );
        // Windows cannot delete the database while SQLite holds it open.
        close_test_store(store);
        remove_test_directory(&directory).unwrap();
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
            let claimed = store
                .claim_webhook_deliveries(&digest, None, "retry-worker", 10)
                .unwrap();
            assert_eq!(claimed.len(), 1);
            store
                .mark_webhook_failed("event", "retry-worker", "endpoint unavailable")
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
        // Windows cannot delete the database while SQLite holds it open.
        close_test_store(store);
        remove_test_directory(&directory).unwrap();
    }

    #[test]
    fn scoped_binding_and_claims_never_cross_project_boundaries() {
        let (store, directory) = test_store();
        let endpoint_a = "a".repeat(64);
        let endpoint_b = "b".repeat(64);
        store
            .enqueue_webhook_delivery("event-a", "project", "mailbox.completed", "{}", "")
            .unwrap();
        store
            .enqueue_webhook_delivery("event-b", "project-b", "mailbox.completed", "{}", "")
            .unwrap();

        store
            .bind_unbound_webhook_deliveries(&endpoint_a, Some("project"))
            .unwrap();
        let claimed_a = store
            .claim_webhook_deliveries(&endpoint_a, Some("project"), "worker-a", 10)
            .unwrap();
        assert_eq!(
            claimed_a
                .iter()
                .map(|row| row.event_id.as_str())
                .collect::<Vec<_>>(),
            ["event-a"]
        );
        assert!(
            store
                .claim_webhook_deliveries(&endpoint_a, Some("project-b"), "worker-a", 10)
                .unwrap()
                .is_empty()
        );
        let digest_b: String = store
            .connection
            .query_row(
                "SELECT endpoint_digest FROM webhook_deliveries WHERE event_id='event-b'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            digest_b.is_empty(),
            "project B must remain unbound after project A notification"
        );

        store
            .bind_unbound_webhook_deliveries(&endpoint_b, Some("project-b"))
            .unwrap();
        let claimed_b = store
            .claim_webhook_deliveries(&endpoint_b, Some("project-b"), "worker-b", 10)
            .unwrap();
        assert_eq!(
            claimed_b
                .iter()
                .map(|row| row.event_id.as_str())
                .collect::<Vec<_>>(),
            ["event-b"]
        );
        assert!(
            store
                .claim_webhook_deliveries(&endpoint_a, None, "worker-a2", 10)
                .unwrap()
                .is_empty()
        );
        close_test_store(store);
        remove_test_directory(&directory).unwrap();
    }

    #[test]
    fn webhook_claim_is_exclusive_and_expired_leases_are_reclaimed() {
        let (store, directory) = test_store();
        let digest = "c".repeat(64);
        store
            .enqueue_webhook_delivery(
                "event",
                "project",
                "migration.status_snapshot",
                "{}",
                &digest,
            )
            .unwrap();
        let first = store
            .claim_webhook_deliveries(&digest, None, "worker-a", 10)
            .unwrap();
        assert_eq!(first.len(), 1);
        assert!(
            store
                .claim_webhook_deliveries(&digest, None, "worker-b", 10)
                .unwrap()
                .is_empty()
        );
        store.connection.execute("UPDATE webhook_deliveries SET lease_until=datetime('now','-1 second') WHERE event_id='event'", []).unwrap();
        let reclaimed = store
            .claim_webhook_deliveries(&digest, None, "worker-b", 10)
            .unwrap();
        assert_eq!(reclaimed.len(), 1);
        assert!(store.mark_webhook_delivered("event", "worker-a").is_err());
        store.mark_webhook_delivered("event", "worker-b").unwrap();
        assert!(
            store
                .claim_webhook_deliveries(&digest, None, "worker-c", 10)
                .unwrap()
                .is_empty()
        );
        close_test_store(store);
        remove_test_directory(&directory).unwrap();
    }

    #[test]
    fn repeatedly_expired_leases_count_as_attempts_and_dead_letter() {
        let (store, directory) = test_store();
        let digest = "d".repeat(64);
        store
            .enqueue_webhook_delivery("event", "project", "mailbox.completed", "{}", &digest)
            .unwrap();
        for attempt in 1..=MAX_WEBHOOK_ATTEMPTS {
            let claimed = store
                .claim_webhook_deliveries(&digest, None, "crashing-worker", 10)
                .unwrap();
            assert_eq!(claimed.len(), 1, "attempt {attempt}");
            assert_eq!(claimed[0].attempts, attempt - 1);
            // The worker crashes without recording an outcome.
            store
                .connection
                .execute(
                    "UPDATE webhook_deliveries SET lease_until=datetime('now','-1 second') WHERE event_id='event'",
                    [],
                )
                .unwrap();
        }
        assert!(
            store
                .claim_webhook_deliveries(&digest, None, "crashing-worker", 10)
                .unwrap()
                .is_empty()
        );
        let (status, attempts): (String, i64) = store
            .connection
            .query_row(
                "SELECT status,attempts FROM webhook_deliveries WHERE event_id='event'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "dead_letter");
        assert_eq!(attempts, i64::from(MAX_WEBHOOK_ATTEMPTS));
        close_test_store(store);
        remove_test_directory(&directory).unwrap();
    }

    #[test]
    fn schema_v23_migration_unbinds_queued_lifecycle_events() {
        let (store, directory) = test_store();
        store
            .enqueue_webhook_delivery(
                "event-a",
                "project",
                "mailbox.completed",
                "{}",
                &"a".repeat(64),
            )
            .unwrap();
        store
            .enqueue_webhook_delivery(
                "snapshot",
                "project",
                "migration.status_snapshot",
                "{}",
                &"b".repeat(64),
            )
            .unwrap();
        let database_path = directory.join("state.db");
        close_test_store(store);

        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            connection
                .execute_batch(
                    "DROP TRIGGER IF EXISTS events_webhook_outbox;
                     ALTER TABLE webhook_deliveries RENAME TO webhook_deliveries_v24;
                     CREATE TABLE webhook_deliveries (
                        event_id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
                        event_type TEXT NOT NULL, payload TEXT NOT NULL,
                        endpoint_digest TEXT NOT NULL,
                        attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
                        status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','delivered','dead_letter')),
                        last_error TEXT,
                        next_attempt_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                        delivered_at TEXT
                     );
                     INSERT INTO webhook_deliveries(event_id,project_id,event_type,payload,endpoint_digest,attempts,status,last_error,next_attempt_at,created_at,delivered_at)
                     SELECT event_id,project_id,event_type,payload,endpoint_digest,attempts,status,last_error,next_attempt_at,created_at,delivered_at FROM webhook_deliveries_v24;
                     DROP TABLE webhook_deliveries_v24;
                     PRAGMA user_version=23;",
                )
                .unwrap();
        }

        let migrated = StateStore::open(&database_path).unwrap();
        let lifecycle_digest: String = migrated
            .connection
            .query_row(
                "SELECT endpoint_digest FROM webhook_deliveries WHERE event_id='event-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let snapshot_digest: String = migrated
            .connection
            .query_row(
                "SELECT endpoint_digest FROM webhook_deliveries WHERE event_id='snapshot'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(lifecycle_digest.is_empty());
        assert_eq!(snapshot_digest, "b".repeat(64));
        close_test_store(migrated);
        remove_test_directory(&directory).unwrap();
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
        // Windows cannot delete the database while SQLite holds it open.
        close_test_store(store);
        remove_test_directory(&directory).unwrap();
    }

    #[test]
    fn failed_mailbox_runs_never_enter_the_completed_outbox() {
        let (store, directory) = test_store();
        store
            .connection
            .execute(
                "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,state) VALUES('job','project','source-user','destination-user','failed')",
                [],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO runs(id,project_id,job_id,engine,status) VALUES('run','project','job','imapsync','failed')",
                [],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO events(project_id,run_id,kind,detail) VALUES('project','run','run_finished','failed')",
                [],
            )
            .unwrap();

        let delivery = store
            .due_webhook_deliveries("", 10)
            .unwrap()
            .into_iter()
            .find(|delivery| delivery.event_id == "ledger-event-1")
            .unwrap();
        assert_eq!(delivery.event_type, "mailbox.failed");
        assert!(delivery.payload.contains("mailbox.failed"));
        // Windows cannot delete the database while SQLite holds it open.
        close_test_store(store);
        remove_test_directory(&directory).unwrap();
    }

    #[test]
    fn failed_preflight_runs_get_a_distinct_webhook_event() {
        let (store, directory) = test_store();
        store
            .connection
            .pragma_update(None, "user_version", 21)
            .unwrap();
        close_test_store(store);
        let store = StateStore::open(directory.join("state.db")).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,state) VALUES('job','project','source-user','destination-user','attention')",
                [],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO runs(id,project_id,job_id,engine,phase_at_start,status) VALUES('run','project','job','imapsync','preflight','failed')",
                [],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO events(project_id,run_id,kind,detail) VALUES('project','run','run_finished','failure')",
                [],
            )
            .unwrap();

        let delivery = store.due_webhook_deliveries("", 10).unwrap();
        assert_eq!(delivery.len(), 1);
        assert_eq!(delivery[0].event_type, "mailbox.preflight_failed");
        assert!(delivery[0].payload.contains("mailbox.preflight_failed"));
        close_test_store(store);
        remove_test_directory(&directory).unwrap();
    }

    #[test]
    fn proof_ready_events_publish_the_signed_artifact_digest() {
        let (store, directory) = test_store();
        let digest = "ab".repeat(32);
        store
            .record_event("project", "proof_ready", &format!("sha256={digest}"))
            .unwrap();

        let delivery = store.due_webhook_deliveries("", 10).unwrap();
        assert_eq!(delivery.len(), 1);
        assert_eq!(delivery[0].event_type, "migration.proof_ready");
        assert!(delivery[0].payload.contains(&format!("sha256={digest}")));
        close_test_store(store);
        remove_test_directory(&directory).unwrap();
    }
}
