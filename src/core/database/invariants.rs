//! Whole-ledger internal invariants.
//!
//! `validate_schema_layout`/`validate_schema_constraints` prove the shape of
//! the database (tables, columns, indexes, triggers, CHECKs, foreign keys).
//! This module adds the data-level invariants that shape cannot express:
//! derived tables that triggers or set-based rebuilds maintain, cross-project
//! references that foreign keys do not scope, and state machines spread over
//! several columns. Chaos and property tests call it after injected failures at
//! mutation boundaries; a violation names every broken invariant.

use super::*;

/// One data-level invariant: a name and a query counting violating rows.
const DATA_INVARIANTS: &[(&str, &str)] = &[
    (
        "every queue fact has a matching search-index row",
        "SELECT COUNT(*) FROM mailbox_queue_facts f
         LEFT JOIN mailbox_queue_facts_search s ON s.rowid=f.job_rowid
         WHERE s.rowid IS NULL OR s.search_key IS NOT f.search_key",
    ),
    (
        "the search index has no rows without a queue fact",
        "SELECT (SELECT COUNT(*) FROM mailbox_queue_facts_search)
              - (SELECT COUNT(*) FROM mailbox_queue_facts)",
    ),
    (
        "every queue fact projects its job's rowid, id, project, and state",
        "SELECT COUNT(*) FROM mailbox_queue_facts f
         LEFT JOIN mailbox_jobs j ON j.rowid=f.job_rowid
         WHERE j.id IS NULL OR j.id<>f.job_id OR j.project_id<>f.project_id
            OR j.state<>f.state",
    ),
    (
        "every batch plan is used by a mailbox in its own project",
        "SELECT COUNT(*) FROM batch_plans p
         WHERE NOT EXISTS(SELECT 1 FROM mailbox_jobs j
                          WHERE j.batch_plan_id=p.id AND j.project_id=p.project_id)",
    ),
    (
        "no mailbox references another project's batch plan",
        "SELECT COUNT(*) FROM mailbox_jobs j JOIN batch_plans p ON p.id=j.batch_plan_id
         WHERE p.project_id<>j.project_id",
    ),
    (
        "every run's mailbox belongs to the run's project",
        "SELECT COUNT(*) FROM runs r JOIN mailbox_jobs j ON j.id=r.job_id
         WHERE j.project_id<>r.project_id",
    ),
    (
        "every child run's parent belongs to the same project",
        "SELECT COUNT(*) FROM runs r JOIN runs parent ON parent.id=r.parent_run_id
         WHERE parent.project_id<>r.project_id",
    ),
    (
        "webhook leases exist exactly while a delivery is in flight",
        "SELECT COUNT(*) FROM webhook_deliveries
         WHERE (status='delivering') <> (lease_owner IS NOT NULL AND lease_until IS NOT NULL)",
    ),
];

impl StateStore {
    /// Prove the ledger's structural and data-level invariants. Returns every
    /// violated invariant, not just the first, so a failed chaos case names
    /// the whole blast radius.
    #[cfg(test)]
    pub(crate) fn validate_internal_invariants(&self) -> Result<(), String> {
        Self::validate_internal_invariants_on(&self.connection)
    }

    pub(super) fn validate_internal_invariants_on(connection: &Connection) -> Result<(), String> {
        let mut violations = Vec::new();
        if let Err(error) = Self::validate_schema_layout(connection) {
            violations.push(format!(
                "schema layout (tables, indexes, triggers, foreign keys): {error}"
            ));
        }
        if let Err(error) = Self::validate_schema_constraints(connection) {
            violations.push(format!("schema constraints: {error}"));
        }
        match connection.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)) {
            Ok(result) if result == "ok" => {}
            Ok(result) => violations.push(format!("integrity_check: {result}")),
            Err(error) => violations.push(format!("integrity_check failed to run: {error}")),
        }
        for (name, query) in DATA_INVARIANTS {
            match connection.query_row(query, [], |row| row.get::<_, i64>(0)) {
                Ok(0) => {}
                Ok(count) => violations.push(format!("{name}: {count} violation(s)")),
                Err(error) => violations.push(format!("{name}: could not evaluate: {error}")),
            }
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StateStore;
    use crate::core::{Phase, QueueInsert, QueueRowFacts};
    use std::sync::Arc;

    fn rows(count: usize) -> Vec<QueueInsert> {
        let plan = Arc::<str>::from("destination_host = \"new.example\"\n");
        (0..count)
            .map(|index| {
                let user = format!("user{index}@example.test");
                QueueInsert {
                    source_mailbox: user.clone(),
                    destination_mailbox: user.clone(),
                    destination_identity: crate::core::policy::normalized_destination_identity(
                        &user,
                        Some(&format!(
                            "destination_host = \"new.example\"\ndestination_user = \"{user}\"\n"
                        )),
                    ),
                    config: String::new(),
                    batch_plan_config: Some(Arc::clone(&plan)),
                    row_overrides: Some(format!("destination_user = \"{user}\"\n")),
                    facts: QueueRowFacts {
                        label: format!("Row {index}"),
                        source_host: "old.example".into(),
                        destination_host: "new.example".into(),
                        search_key: format!("row {index} {user}"),
                        destructive: false,
                        policy: "additive".into(),
                    },
                }
            })
            .collect()
    }

    /// Make the next write to `table` that matches `when` abort its statement,
    /// simulating a failure at that mutation boundary.
    fn inject_failure(store: &StateStore, table: &str, when: &str) {
        store
            .connection
            .execute_batch(&format!(
                "CREATE TRIGGER injected_failure BEFORE INSERT ON {table} WHEN {when}
                 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;"
            ))
            .unwrap();
    }

    fn remove_injected_failure(store: &StateStore) {
        store
            .connection
            .execute_batch("DROP TRIGGER injected_failure")
            .unwrap();
    }

    fn counts(store: &StateStore) -> [i64; 5] {
        [
            "projects",
            "mailbox_jobs",
            "batch_plans",
            "mailbox_queue_facts",
            "events",
        ]
        .map(|table| {
            store
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        })
    }

    #[test]
    fn a_populated_ledger_satisfies_every_invariant() {
        let store = StateStore::in_memory().unwrap();
        store
            .create_batch_queue("queue", "old.example", "new.example", &rows(25))
            .unwrap();
        store.validate_internal_invariants().unwrap();
    }

    #[test]
    fn each_data_invariant_detects_its_violation() {
        for (corruption, expected) in [
            (
                "DELETE FROM mailbox_queue_facts_search WHERE rowid=(SELECT MIN(job_rowid) FROM mailbox_queue_facts)",
                "matching search-index row",
            ),
            (
                "INSERT INTO mailbox_queue_facts_search(rowid,search_key) VALUES(999999,'ghost')",
                "without a queue fact",
            ),
            (
                "UPDATE mailbox_queue_facts SET state='verified' WHERE job_rowid=(SELECT MIN(job_rowid) FROM mailbox_queue_facts)",
                "projects its job",
            ),
            (
                "INSERT INTO batch_plans(id,project_id,config) SELECT 'orphan',id,'x' FROM projects LIMIT 1",
                "used by a mailbox",
            ),
        ] {
            let store = StateStore::in_memory().unwrap();
            store
                .create_batch_queue("queue", "old.example", "new.example", &rows(3))
                .unwrap();
            // The facts state trigger would repair a job-side change, so
            // corrupt the derived tables directly.
            store.connection.execute_batch(corruption).unwrap();
            let error = store.validate_internal_invariants().unwrap_err();
            assert!(error.contains(expected), "{corruption}: {error}");
        }
    }

    #[test]
    fn failed_queue_imports_roll_back_without_breaking_invariants() {
        // Every write boundary of a batch import. The facts and events
        // failures land while, and after, the search-index insert trigger is
        // dropped for its set-based rebuild, so they prove the drop rolls back.
        // (SQLite cannot attach a failure trigger to the FTS table itself.)
        for (table, when) in [
            ("projects", "1"),
            ("batch_plans", "1"),
            ("mailbox_jobs", "NEW.source_mailbox='user7@example.test'"),
            ("mailbox_queue_facts", "NEW.label='Row 7'"),
            ("events", "NEW.kind='project_created'"),
        ] {
            let store = StateStore::in_memory().unwrap();
            store
                .create_batch_queue("existing", "old.example", "new.example", &rows(4))
                .unwrap();
            let before = counts(&store);
            inject_failure(&store, table, when);
            assert!(
                store
                    .create_batch_queue("queue", "old.example", "new.example", &rows(12))
                    .is_err(),
                "failure injected into {table} must fail the import"
            );
            remove_injected_failure(&store);
            assert_eq!(counts(&store), before, "{table}: partial import persisted");
            store
                .validate_internal_invariants()
                .unwrap_or_else(|error| panic!("{table}: {error}"));
            // The ledger stays usable: the same import now succeeds.
            store
                .create_batch_queue("queue", "old.example", "new.example", &rows(12))
                .unwrap();
            store.validate_internal_invariants().unwrap();
        }
    }

    #[test]
    fn failed_lifecycle_writes_roll_back_without_breaking_invariants() {
        let store = StateStore::in_memory().unwrap();
        let (project, _) = store
            .create_batch_queue("queue", "old.example", "new.example", &rows(3))
            .unwrap();
        // A phase change, a lifecycle webhook produced by an event, and
        // cutover planning each fail at their own boundary.
        inject_failure(&store, "events", "NEW.kind='phase_changed'");
        assert!(store.transition(&project.id, Phase::Preflight).is_err());
        remove_injected_failure(&store);
        assert_eq!(
            store.project(&project.id).unwrap().unwrap().phase,
            Phase::Discovery
        );
        store.validate_internal_invariants().unwrap();

        inject_failure(&store, "webhook_deliveries", "1");
        assert!(
            store
                .record_event(&project.id, "proof_ready", "sha256=00")
                .is_err(),
            "a failed outbox write must abort the event that produced it"
        );
        remove_injected_failure(&store);
        let proof_events: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind='proof_ready'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(proof_events, 0);
        store.validate_internal_invariants().unwrap();

        store.transition(&project.id, Phase::Preflight).unwrap();
        inject_failure(&store, "events", "NEW.kind='cutover_planned'");
        assert!(
            store
                .create_cutover_workflow(&project.id, "2026-10-05T22:00:00Z", None)
                .is_err()
        );
        remove_injected_failure(&store);
        assert!(store.cutover_workflow(&project.id).unwrap().is_none());
        store.validate_internal_invariants().unwrap();
    }

    #[test]
    fn failed_webhook_outcome_keeps_the_lease_consistent() {
        let store = StateStore::in_memory().unwrap();
        let (project, _) = store
            .create_batch_queue("queue", "old.example", "new.example", &rows(1))
            .unwrap();
        let digest = "e".repeat(64);
        store
            .enqueue_webhook_delivery("event", &project.id, "mailbox.completed", "{}", &digest)
            .unwrap();
        assert_eq!(
            store
                .claim_webhook_deliveries(&digest, None, "worker", 1)
                .unwrap()
                .len(),
            1
        );
        store.validate_internal_invariants().unwrap();
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER injected_failure BEFORE UPDATE ON webhook_deliveries
                 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
            )
            .unwrap();
        assert!(
            store
                .mark_webhook_failed("event", "worker", "boom")
                .is_err()
        );
        remove_injected_failure(&store);
        store.validate_internal_invariants().unwrap();
        store
            .mark_webhook_failed("event", "worker", "boom")
            .unwrap();
        store.validate_internal_invariants().unwrap();
    }
}
