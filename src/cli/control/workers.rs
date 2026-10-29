//! Worker enrollment and fenced assignment leases.
//!
//! A worker is bound to exactly one tenant. It enrolls once with a
//! short-lived, single-use enrollment token minted by a tenant manager, and
//! receives a worker credential that can only call the worker endpoints.
//!
//! Work is handed out as leases. Every lease increments the assignment's
//! fencing token; heartbeats and completions must present the current token
//! on an unexpired lease held by the same worker. A worker whose lease
//! expired, was revoked, or was re-leased to another worker is refused, so
//! a stale worker cannot overwrite the result of the worker that replaced
//! it. Assignments are created only for plans approved at their current
//! revision, and a plan change stops further lease renewals.

use super::audit::AuditEntry;
use super::authn::{self, TokenKind};
use super::store::{self, Actor, Store, StoreError, StoreResult, append_audit, now};
use rusqlite::{OptionalExtension, params};
use zeroize::Zeroizing;

pub(super) const SCHEMA_V2: &str = "
CREATE TABLE worker_enrollments(
    token_hash BLOB PRIMARY KEY CHECK(length(token_hash) = 32),
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    name TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at INTEGER
) STRICT;
CREATE TABLE workers(
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    name TEXT NOT NULL,
    credential_hash BLOB NOT NULL UNIQUE CHECK(length(credential_hash) = 32),
    capabilities TEXT NOT NULL,
    enrolled_at INTEGER NOT NULL,
    last_seen_at INTEGER,
    revoked_at INTEGER,
    UNIQUE(tenant_id, name)
) STRICT;
CREATE TABLE assignments(
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    project_id TEXT NOT NULL REFERENCES projects(id),
    plan_revision INTEGER NOT NULL,
    mailbox TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('queued','leased','completed','failed','cancelled')),
    fencing_token INTEGER NOT NULL DEFAULT 0 CHECK(fencing_token >= 0),
    lease_worker_id TEXT REFERENCES workers(id),
    lease_expires_at INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    result TEXT,
    created_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK((state = 'leased') = (lease_worker_id IS NOT NULL AND lease_expires_at IS NOT NULL))
) STRICT;
CREATE INDEX assignments_lease ON assignments(tenant_id, state, created_at);
CREATE INDEX assignments_project ON assignments(tenant_id, project_id);
";

pub(super) const MAX_ENROLLMENT_MINUTES: i64 = 60;
pub(super) const DEFAULT_LEASE_SECONDS: i64 = 300;
pub(super) const MAX_LEASE_SECONDS: i64 = 900;
const MAX_LEASE_ATTEMPTS: i64 = 10;
const MAX_ASSIGNMENTS_PER_REQUEST: usize = 1_000;
const MAX_RESULT_BYTES: usize = 64 * 1024;
const MAX_CAPABILITIES: usize = 32;

/// The worker a credential authenticates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WorkerIdentity {
    pub(super) id: String,
    pub(super) tenant: String,
}

impl WorkerIdentity {
    pub(super) fn actor(&self) -> String {
        format!("worker:{}", self.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct WorkerView {
    pub(super) id: String,
    pub(super) tenant_id: String,
    pub(super) name: String,
    pub(super) capabilities: Vec<String>,
    pub(super) enrolled_at: i64,
    pub(super) last_seen_at: Option<i64>,
    pub(super) revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct AssignmentView {
    pub(super) id: String,
    pub(super) project_id: String,
    pub(super) plan_revision: i64,
    pub(super) mailbox: String,
    pub(super) state: String,
    pub(super) fencing_token: i64,
    pub(super) lease_worker_id: Option<String>,
    pub(super) lease_expires_at: Option<i64>,
    pub(super) attempts: i64,
    pub(super) result: Option<serde_json::Value>,
}

/// What a worker receives with a lease: the non-secret plan and the
/// mailbox label. Credentials for that mailbox are already on the worker.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct Lease {
    pub(super) assignment_id: String,
    pub(super) fencing_token: i64,
    pub(super) lease_expires_at: i64,
    pub(super) project_id: String,
    pub(super) plan_revision: i64,
    pub(super) plan: serde_json::Value,
    pub(super) mailbox: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Completed,
    Failed,
}

impl Outcome {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

const ASSIGNMENT_COLUMNS: &str = "id,project_id,plan_revision,mailbox,state,fencing_token,lease_worker_id,lease_expires_at,attempts,result";

fn assignment_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssignmentView> {
    let result_json: Option<String> = row.get(9)?;
    let result = result_json
        .map(|text| {
            serde_json::from_str(&text).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()?;
    Ok(AssignmentView {
        id: row.get(0)?,
        project_id: row.get(1)?,
        plan_revision: row.get(2)?,
        mailbox: row.get(3)?,
        state: row.get(4)?,
        fencing_token: row.get(5)?,
        lease_worker_id: row.get(6)?,
        lease_expires_at: row.get(7)?,
        attempts: row.get(8)?,
        result,
    })
}

fn lease_lost() -> StoreError {
    StoreError::Conflict(
        "this worker no longer holds the lease (it expired, was revoked, was reassigned, or the plan approval changed); stop work on this assignment".into(),
    )
}

/// An approved, unchanged plan for the assignment's project.
const PLAN_STILL_APPROVED: &str = "EXISTS(SELECT 1 FROM projects p WHERE p.id=assignments.project_id AND p.tenant_id=assignments.tenant_id AND p.approved_revision=p.plan_revision AND p.plan_revision=assignments.plan_revision)";

impl Store {
    /// Mint a single-use enrollment token for a new worker in `tenant`.
    pub(super) fn create_enrollment(
        &self,
        tenant: &str,
        name: &str,
        minutes: i64,
        actor: &Actor<'_>,
    ) -> StoreResult<(Zeroizing<String>, i64)> {
        let name = store::validate_name(name, "worker name")?;
        if !(1..=MAX_ENROLLMENT_MINUTES).contains(&minutes) {
            return Err(StoreError::Invalid(format!(
                "enrollment tokens expire after 1 to {MAX_ENROLLMENT_MINUTES} minutes"
            )));
        }
        let token = authn::new_token(TokenKind::Enrollment).map_err(StoreError::Internal)?;
        let created_at = now();
        let expires_at = created_at + minutes * 60;
        self.mutate(actor, Some(tenant), "worker.enrollment_create", &format!("worker_name:{name}"), "", |tx| {
            let taken: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM workers WHERE tenant_id=?1 AND name=?2)",
                params![tenant, name],
                |row| row.get(0),
            )?;
            if taken {
                return Err(StoreError::Conflict("a worker with that name is already enrolled in this tenant".into()));
            }
            tx.execute(
                "INSERT INTO worker_enrollments(token_hash,tenant_id,name,created_by,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6)",
                params![authn::token_hash(&token).as_slice(), tenant, name, actor.label, created_at, expires_at],
            )?;
            Ok(())
        })?;
        Ok((token, expires_at))
    }

    /// Exchange an enrollment token for a worker credential. The token is
    /// consumed even if the worker never uses the credential.
    pub(super) fn enroll_worker(
        &self,
        enrollment_token: &str,
        capabilities: &[String],
        request_id: &str,
    ) -> StoreResult<(WorkerView, Zeroizing<String>)> {
        if TokenKind::of(enrollment_token) != Some(TokenKind::Enrollment) {
            return Err(StoreError::NotFound);
        }
        if capabilities.len() > MAX_CAPABILITIES
            || capabilities.iter().any(|capability| {
                capability.is_empty()
                    || capability.len() > 64
                    || !capability
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
            })
        {
            return Err(StoreError::Invalid(format!(
                "declare at most {MAX_CAPABILITIES} capabilities of letters, digits, and - _ . :"
            )));
        }
        let credential = authn::new_token(TokenKind::Worker).map_err(StoreError::Internal)?;
        let id = store::new_id("wrk")?;
        let at = now();
        let mut connection = self.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let enrollment: Option<(String, String)> = tx
            .query_row(
                "SELECT tenant_id,name FROM worker_enrollments WHERE token_hash=?1 AND used_at IS NULL AND expires_at>?2",
                params![authn::token_hash(enrollment_token).as_slice(), at],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((tenant, name)) = enrollment else {
            return Err(StoreError::NotFound);
        };
        tx.execute(
            "UPDATE worker_enrollments SET used_at=?2 WHERE token_hash=?1",
            params![authn::token_hash(enrollment_token).as_slice(), at],
        )?;
        tx.execute(
            "INSERT INTO workers(id,tenant_id,name,credential_hash,capabilities,enrolled_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![id, tenant, name, authn::token_hash(&credential).as_slice(), capabilities.join(","), at],
        )?;
        append_audit(
            &tx,
            &AuditEntry {
                at,
                request_id,
                actor: &format!("worker:{id}"),
                tenant: Some(&tenant),
                action: "worker.enroll",
                resource: &format!("worker:{id}"),
                allowed: true,
                detail: &name,
            },
        )?;
        tx.commit()?;
        Ok((
            WorkerView {
                id,
                tenant_id: tenant,
                name,
                capabilities: capabilities.to_vec(),
                enrolled_at: at,
                last_seen_at: None,
                revoked: false,
            },
            credential,
        ))
    }

    /// The enrolled, unrevoked worker a credential authenticates.
    pub(super) fn worker_identity(&self, credential: &str) -> StoreResult<Option<WorkerIdentity>> {
        if TokenKind::of(credential) != Some(TokenKind::Worker) {
            return Ok(None);
        }
        let connection = self.lock();
        let at = now();
        let identity: Option<(String, String, Option<i64>)> = connection
            .query_row(
                "SELECT id,tenant_id,last_seen_at FROM workers WHERE credential_hash=?1 AND revoked_at IS NULL",
                [authn::token_hash(credential).as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((id, tenant, last_seen)) = identity else {
            return Ok(None);
        };
        if last_seen.is_none_or(|last| at - last >= 30) {
            connection.execute(
                "UPDATE workers SET last_seen_at=?2 WHERE id=?1",
                params![id, at],
            )?;
        }
        Ok(Some(WorkerIdentity { id, tenant }))
    }

    pub(super) fn workers(&self, tenant: &str) -> StoreResult<Vec<WorkerView>> {
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT id,tenant_id,name,capabilities,enrolled_at,last_seen_at,revoked_at IS NOT NULL FROM workers WHERE tenant_id=?1 ORDER BY name",
        )?;
        let workers = statement
            .query_map([tenant], |row| {
                Ok(WorkerView {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    name: row.get(2)?,
                    capabilities: row
                        .get::<_, String>(3)?
                        .split(',')
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    enrolled_at: row.get(4)?,
                    last_seen_at: row.get(5)?,
                    revoked: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(workers)
    }

    /// Revoke a worker. Its credential stops working at once and every
    /// lease it holds returns to the queue; the next lease of each carries a
    /// higher fencing token, so the revoked worker cannot report on it.
    pub(super) fn revoke_worker(
        &self,
        tenant: &str,
        worker: &str,
        actor: &Actor<'_>,
    ) -> StoreResult<usize> {
        self.mutate(actor, Some(tenant), "worker.revoke", &format!("worker:{worker}"), "", |tx| {
            let at = now();
            let changed = tx.execute(
                "UPDATE workers SET revoked_at=?3 WHERE tenant_id=?1 AND id=?2 AND revoked_at IS NULL",
                params![tenant, worker, at],
            )?;
            if changed == 0 {
                return Err(StoreError::NotFound);
            }
            Ok(tx.execute(
                "UPDATE assignments SET state='queued',lease_worker_id=NULL,lease_expires_at=NULL,updated_at=?3 WHERE tenant_id=?1 AND lease_worker_id=?2 AND state='leased'",
                params![tenant, worker, at],
            )?)
        })
    }

    /// Queue one assignment per mailbox label. The project's plan must be
    /// approved at its current revision.
    pub(super) fn create_assignments(
        &self,
        tenant: &str,
        project: &str,
        mailboxes: &[String],
        actor: &Actor<'_>,
    ) -> StoreResult<Vec<AssignmentView>> {
        if mailboxes.is_empty() || mailboxes.len() > MAX_ASSIGNMENTS_PER_REQUEST {
            return Err(StoreError::Invalid(format!(
                "send 1 to {MAX_ASSIGNMENTS_PER_REQUEST} mailboxes"
            )));
        }
        let labels = mailboxes
            .iter()
            .map(|mailbox| store::validate_name(mailbox, "mailbox label"))
            .collect::<StoreResult<Vec<_>>>()?;
        let ids = labels
            .iter()
            .map(|_| store::new_id("asg"))
            .collect::<StoreResult<Vec<_>>>()?;
        self.mutate(
            actor,
            Some(tenant),
            "assignment.create",
            &format!("project:{project}"),
            &format!("{} mailbox(es)", labels.len()),
            |tx| {
                let approved: Option<(i64, Option<i64>)> = tx
                    .query_row(
                        "SELECT plan_revision,approved_revision FROM projects WHERE tenant_id=?1 AND id=?2",
                        params![tenant, project],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let Some((revision, approved_revision)) = approved else {
                    return Err(StoreError::NotFound);
                };
                if approved_revision != Some(revision) {
                    return Err(StoreError::Conflict(
                        "the project's current plan is not approved; approve it before queuing work".into(),
                    ));
                }
                let at = now();
                for (id, mailbox) in ids.iter().zip(&labels) {
                    tx.execute(
                        "INSERT INTO assignments(id,tenant_id,project_id,plan_revision,mailbox,state,created_by,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,'queued',?6,?7,?7)",
                        params![id, tenant, project, revision, mailbox, actor.label, at],
                    )?;
                }
                Ok(())
            },
        )?;
        let connection = self.lock();
        ids.iter()
            .map(|id| {
                connection
                    .query_row(
                        &format!("SELECT {ASSIGNMENT_COLUMNS} FROM assignments WHERE tenant_id=?1 AND id=?2"),
                        params![tenant, id],
                        assignment_from_row,
                    )
                    .map_err(StoreError::from)
            })
            .collect()
    }

    pub(super) fn assignments(
        &self,
        tenant: &str,
        project: &str,
    ) -> StoreResult<Vec<AssignmentView>> {
        let connection = self.lock();
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE tenant_id=?1 AND id=?2)",
            params![tenant, project],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StoreError::NotFound);
        }
        let mut statement = connection.prepare(&format!(
            "SELECT {ASSIGNMENT_COLUMNS} FROM assignments WHERE tenant_id=?1 AND project_id=?2 ORDER BY created_at,id"
        ))?;
        let assignments = statement
            .query_map(params![tenant, project], assignment_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(assignments)
    }

    /// Lease the oldest available assignment in the worker's tenant: queued
    /// work, or a lease that expired without completing. Each lease raises
    /// the fencing token. Expired leases that have used every attempt fail.
    pub(super) fn lease(
        &self,
        worker: &WorkerIdentity,
        seconds: i64,
        request_id: &str,
    ) -> StoreResult<Option<Lease>> {
        if !(30..=MAX_LEASE_SECONDS).contains(&seconds) {
            return Err(StoreError::Invalid(format!(
                "leases last 30 to {MAX_LEASE_SECONDS} seconds"
            )));
        }
        let at = now();
        let mut connection = self.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let worker_is_active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM workers WHERE id=?1 AND tenant_id=?2 AND revoked_at IS NULL)",
            params![worker.id, worker.tenant],
            |row| row.get(0),
        )?;
        if !worker_is_active {
            append_audit(
                &tx,
                &AuditEntry {
                    at,
                    request_id,
                    actor: &worker.actor(),
                    tenant: Some(&worker.tenant),
                    action: "assignment.lease",
                    resource: "assignment:queue",
                    allowed: false,
                    detail: "worker is revoked or no longer exists",
                },
            )?;
            tx.commit()?;
            return Err(StoreError::NotFound);
        }
        tx.execute(
            "UPDATE assignments SET state='failed',lease_worker_id=NULL,lease_expires_at=NULL,updated_at=?2,
                result='{\"error\":\"the lease expired after the maximum number of attempts\"}'
             WHERE tenant_id=?1 AND state='leased' AND lease_expires_at<=?2 AND attempts>=?3",
            params![worker.tenant, at, MAX_LEASE_ATTEMPTS],
        )?;
        let candidate: Option<String> = tx
            .query_row(
                &format!(
                    "SELECT id FROM assignments WHERE tenant_id=?1 AND (state='queued' OR (state='leased' AND lease_expires_at<=?2)) AND {PLAN_STILL_APPROVED} ORDER BY created_at,id LIMIT 1"
                ),
                params![worker.tenant, at],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = candidate else {
            tx.commit()?;
            return Ok(None);
        };
        let expires_at = at + seconds;
        tx.execute(
            "UPDATE assignments SET state='leased',fencing_token=fencing_token+1,lease_worker_id=?3,lease_expires_at=?4,attempts=attempts+1,updated_at=?5
             WHERE tenant_id=?1 AND id=?2",
            params![worker.tenant, id, worker.id, expires_at, at],
        )?;
        let lease = tx.query_row(
            "SELECT a.fencing_token,a.project_id,a.plan_revision,p.plan,a.mailbox FROM assignments a JOIN projects p ON p.id=a.project_id AND p.tenant_id=a.tenant_id WHERE a.tenant_id=?1 AND a.id=?2",
            params![worker.tenant, id],
            |row| {
                let plan_json: String = row.get(3)?;
                let plan = serde_json::from_str(&plan_json).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                Ok(Lease {
                    assignment_id: id.clone(),
                    fencing_token: row.get(0)?,
                    lease_expires_at: expires_at,
                    project_id: row.get(1)?,
                    plan_revision: row.get(2)?,
                    plan,
                    mailbox: row.get(4)?,
                })
            },
        )?;
        append_audit(
            &tx,
            &AuditEntry {
                at,
                request_id,
                actor: &worker.actor(),
                tenant: Some(&worker.tenant),
                action: "assignment.lease",
                resource: &format!("assignment:{id}"),
                allowed: true,
                detail: &format!("fencing token {}", lease.fencing_token),
            },
        )?;
        tx.commit()?;
        Ok(Some(lease))
    }

    /// Extend a lease the worker still holds. Refused once the lease has
    /// expired, moved, or the plan approval behind it changed.
    pub(super) fn renew_lease(
        &self,
        worker: &WorkerIdentity,
        assignment: &str,
        fencing_token: i64,
        seconds: i64,
        request_id: &str,
    ) -> StoreResult<i64> {
        if !(30..=MAX_LEASE_SECONDS).contains(&seconds) {
            return Err(StoreError::Invalid(format!(
                "leases last 30 to {MAX_LEASE_SECONDS} seconds"
            )));
        }
        let at = now();
        let expires_at = at + seconds;
        self.fenced(worker, assignment, fencing_token, "assignment.renew", request_id, |tx| {
            let changed = tx.execute(
                &format!(
                    "UPDATE assignments SET lease_expires_at=?6,updated_at=?5 WHERE tenant_id=?1 AND id=?2 AND state='leased' AND lease_worker_id=?3 AND fencing_token=?4 AND lease_expires_at>?5 AND {PLAN_STILL_APPROVED}"
                ),
                params![worker.tenant, assignment, worker.id, fencing_token, at, expires_at],
            )?;
            if changed == 0 {
                return Err(lease_lost());
            }
            Ok(expires_at)
        })
    }

    /// Record the outcome of a leased assignment. Only the current lease
    /// holder, with the current fencing token, before the lease expires, may
    /// report; the result describes work done under the approved plan.
    pub(super) fn complete(
        &self,
        worker: &WorkerIdentity,
        assignment: &str,
        fencing_token: i64,
        outcome: Outcome,
        result: &serde_json::Value,
        request_id: &str,
    ) -> StoreResult<()> {
        let text = store::validate_plan(result).map_err(|error| match error {
            StoreError::Invalid(message) => StoreError::Invalid(message.replace("plan", "result")),
            other => other,
        })?;
        if text.len() > MAX_RESULT_BYTES {
            return Err(StoreError::Invalid(format!(
                "the result exceeds {MAX_RESULT_BYTES} bytes"
            )));
        }
        let at = now();
        self.fenced(worker, assignment, fencing_token, "assignment.complete", request_id, |tx| {
            let changed = tx.execute(
                "UPDATE assignments SET state=?6,result=?7,lease_worker_id=NULL,lease_expires_at=NULL,updated_at=?5
                 WHERE tenant_id=?1 AND id=?2 AND state='leased' AND lease_worker_id=?3 AND fencing_token=?4 AND lease_expires_at>?5",
                params![worker.tenant, assignment, worker.id, fencing_token, at, outcome.as_str(), text],
            )?;
            if changed == 0 {
                return Err(lease_lost());
            }
            Ok(())
        })
    }

    /// Run a fenced worker change with its audit entry; refusals are
    /// audited as denials so stale-worker activity is visible.
    fn fenced<T>(
        &self,
        worker: &WorkerIdentity,
        assignment: &str,
        fencing_token: i64,
        action: &str,
        request_id: &str,
        change: impl FnOnce(&rusqlite::Transaction<'_>) -> StoreResult<T>,
    ) -> StoreResult<T> {
        let actor = Actor {
            request_id,
            label: worker.actor(),
        };
        let resource = format!("assignment:{assignment}");
        let detail = format!("fencing token {fencing_token}");
        let mut connection = self.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let outcome = change(&tx);
        append_audit(
            &tx,
            &AuditEntry {
                at: now(),
                request_id: actor.request_id,
                actor: &actor.label,
                tenant: Some(&worker.tenant),
                action,
                resource: &resource,
                allowed: outcome.is_ok(),
                detail: &detail,
            },
        )?;
        tx.commit()?;
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        store: Store,
        tenant: String,
        other_tenant: String,
        project: String,
        admin: Actor<'static>,
    }

    fn fixture() -> Fixture {
        let store = Store::in_memory();
        let password = authn::test_secret();
        let admin_id = store
            .bootstrap_administrator("admin@msp.example", "Admin", &password)
            .unwrap();
        let admin = Actor {
            request_id: "test",
            label: format!("user:{admin_id}"),
        };
        let tenant = store.create_tenant("Acme", &admin).unwrap().id;
        let other_tenant = store.create_tenant("Globex", &admin).unwrap().id;
        let editor = Actor {
            request_id: "test",
            label: "user:usr_editor".into(),
        };
        let project = store
            .create_project(
                &tenant,
                "Move",
                &serde_json::json!({"source_host": "imap.acme.example"}),
                &editor,
            )
            .unwrap()
            .id;
        Fixture {
            store,
            tenant,
            other_tenant,
            project,
            admin,
        }
    }

    impl Fixture {
        fn approve(&self) {
            let revision = self
                .store
                .project(&self.tenant, &self.project)
                .unwrap()
                .plan_revision;
            let admin_id = self.admin.label.trim_start_matches("user:").to_owned();
            self.store
                .approve_project(
                    &self.tenant,
                    &self.project,
                    revision,
                    &admin_id,
                    &self.admin,
                )
                .unwrap();
        }

        fn worker(&self, tenant: &str, name: &str) -> WorkerIdentity {
            let (token, _) = self
                .store
                .create_enrollment(tenant, name, 10, &self.admin)
                .unwrap();
            let (view, credential) = self
                .store
                .enroll_worker(&token, &["imapsync".into()], "r")
                .unwrap();
            assert!(
                self.store.enroll_worker(&token, &[], "r").is_err(),
                "enrollment is single-use"
            );
            let identity = self.store.worker_identity(&credential).unwrap().unwrap();
            assert_eq!(identity.id, view.id);
            identity
        }

        fn expire_lease(&self, assignment: &str) {
            self.store
                .lock()
                .execute(
                    "UPDATE assignments SET lease_expires_at=?2 WHERE id=?1",
                    params![assignment, now() - 1],
                )
                .unwrap();
        }
    }

    #[test]
    fn assignments_need_an_approved_plan_and_stay_in_their_tenant() {
        let fixture = fixture();
        assert!(matches!(
            fixture.store.create_assignments(
                &fixture.tenant,
                &fixture.project,
                &["a@acme.example".into()],
                &fixture.admin
            ),
            Err(StoreError::Conflict(_))
        ));
        fixture.approve();
        fixture
            .store
            .create_assignments(
                &fixture.tenant,
                &fixture.project,
                &["a@acme.example".into()],
                &fixture.admin,
            )
            .unwrap();
        assert_eq!(
            fixture
                .store
                .assignments(&fixture.other_tenant, &fixture.project),
            Err(StoreError::NotFound)
        );
        let outsider = fixture.worker(&fixture.other_tenant, "globex-1");
        assert_eq!(fixture.store.lease(&outsider, 300, "r").unwrap(), None);
    }

    #[test]
    fn a_stale_worker_is_fenced_after_its_lease_moves() {
        let fixture = fixture();
        fixture.approve();
        let [assignment] = &fixture
            .store
            .create_assignments(
                &fixture.tenant,
                &fixture.project,
                &["a@acme.example".into()],
                &fixture.admin,
            )
            .unwrap()[..]
        else {
            panic!("one assignment")
        };
        let first = fixture.worker(&fixture.tenant, "w1");
        let second = fixture.worker(&fixture.tenant, "w2");

        let lease = fixture.store.lease(&first, 300, "r").unwrap().unwrap();
        assert_eq!(
            (lease.assignment_id.as_str(), lease.fencing_token),
            (assignment.id.as_str(), 1)
        );
        assert_eq!(lease.plan["source_host"], "imap.acme.example");
        assert_eq!(
            fixture.store.lease(&second, 300, "r").unwrap(),
            None,
            "a live lease is not handed out twice"
        );

        fixture.expire_lease(&assignment.id);
        let taken_over = fixture.store.lease(&second, 300, "r").unwrap().unwrap();
        assert_eq!(taken_over.fencing_token, 2);

        // The first worker comes back: every write with its old token fails.
        let result = serde_json::json!({"messages": 10});
        assert!(matches!(
            fixture
                .store
                .renew_lease(&first, &assignment.id, 1, 300, "r"),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            fixture
                .store
                .complete(&first, &assignment.id, 1, Outcome::Completed, &result, "r"),
            Err(StoreError::Conflict(_))
        ));
        // Even with the new token it is not the holder.
        assert!(
            fixture
                .store
                .complete(&first, &assignment.id, 2, Outcome::Completed, &result, "r")
                .is_err()
        );
        fixture
            .store
            .complete(&second, &assignment.id, 2, Outcome::Completed, &result, "r")
            .unwrap();
        let stored = &fixture
            .store
            .assignments(&fixture.tenant, &fixture.project)
            .unwrap()[0];
        assert_eq!(stored.state, "completed");
        assert_eq!(stored.result, Some(result));

        let denials = fixture
            .store
            .audit_entries(Some(&fixture.tenant), None, 50)
            .unwrap();
        assert!(
            denials.iter().any(
                |entry| entry["decision"] == "deny" && entry["action"] == "assignment.complete"
            )
        );
    }

    #[test]
    fn revoking_a_worker_requeues_its_leases_and_stops_its_credential() {
        let fixture = fixture();
        fixture.approve();
        fixture
            .store
            .create_assignments(
                &fixture.tenant,
                &fixture.project,
                &["a@acme.example".into()],
                &fixture.admin,
            )
            .unwrap();
        let (token, _) = fixture
            .store
            .create_enrollment(&fixture.tenant, "w1", 10, &fixture.admin)
            .unwrap();
        let (_, credential) = fixture.store.enroll_worker(&token, &[], "r").unwrap();
        let worker = fixture.store.worker_identity(&credential).unwrap().unwrap();
        let lease = fixture.store.lease(&worker, 300, "r").unwrap().unwrap();

        assert_eq!(
            fixture
                .store
                .revoke_worker(&fixture.tenant, &worker.id, &fixture.admin)
                .unwrap(),
            1
        );
        assert_eq!(fixture.store.worker_identity(&credential).unwrap(), None);
        assert_eq!(
            fixture.store.lease(&worker, 300, "r"),
            Err(StoreError::NotFound),
            "a stale identity snapshot cannot lease work after revocation"
        );
        assert!(
            fixture
                .store
                .complete(
                    &worker,
                    &lease.assignment_id,
                    lease.fencing_token,
                    Outcome::Completed,
                    &serde_json::json!({}),
                    "r"
                )
                .is_err()
        );
        let replacement = fixture.worker(&fixture.tenant, "w2");
        let next = fixture
            .store
            .lease(&replacement, 300, "r")
            .unwrap()
            .unwrap();
        assert_eq!(next.fencing_token, lease.fencing_token + 1);
        assert_eq!(
            fixture
                .store
                .revoke_worker(&fixture.other_tenant, &replacement.id, &fixture.admin),
            Err(StoreError::NotFound),
            "a worker cannot be revoked through another tenant"
        );
    }

    #[test]
    fn malformed_persisted_json_fails_closed_without_claiming_work() {
        let first = fixture();
        first.approve();
        let [assignment] = &first
            .store
            .create_assignments(
                &first.tenant,
                &first.project,
                &["a@acme.example".into()],
                &first.admin,
            )
            .unwrap()[..]
        else {
            panic!("one assignment")
        };
        let worker = first.worker(&first.tenant, "w1");
        first
            .store
            .lock()
            .execute(
                "UPDATE projects SET plan='not-json' WHERE tenant_id=?1 AND id=?2",
                params![first.tenant, first.project],
            )
            .unwrap();

        assert!(first.store.lease(&worker, 300, "r").is_err());
        let connection = first.store.lock();
        let (state, fencing_token): (String, i64) = connection
            .query_row(
                "SELECT state,fencing_token FROM assignments WHERE tenant_id=?1 AND id=?2",
                params![first.tenant, assignment.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((state.as_str(), fencing_token), ("queued", 0));
        drop(connection);
        assert!(first.store.project(&first.tenant, &first.project).is_err());

        let second = fixture();
        second.approve();
        let [assignment] = &second
            .store
            .create_assignments(
                &second.tenant,
                &second.project,
                &["b@acme.example".into()],
                &second.admin,
            )
            .unwrap()[..]
        else {
            panic!("one assignment")
        };
        second
            .store
            .lock()
            .execute(
                "UPDATE assignments SET result='not-json' WHERE tenant_id=?1 AND id=?2",
                params![second.tenant, assignment.id],
            )
            .unwrap();
        assert!(
            second
                .store
                .assignments(&second.tenant, &second.project)
                .is_err()
        );
    }

    #[test]
    fn a_plan_change_stops_renewals_and_new_leases() {
        let fixture = fixture();
        fixture.approve();
        fixture
            .store
            .create_assignments(
                &fixture.tenant,
                &fixture.project,
                &["a@acme.example".into(), "b@acme.example".into()],
                &fixture.admin,
            )
            .unwrap();
        let worker = fixture.worker(&fixture.tenant, "w1");
        let lease = fixture.store.lease(&worker, 300, "r").unwrap().unwrap();
        fixture
            .store
            .update_plan(
                &fixture.tenant,
                &fixture.project,
                &serde_json::json!({"source_host": "imap.new"}),
                1,
                &fixture.admin,
            )
            .unwrap();
        assert!(
            fixture
                .store
                .renew_lease(&worker, &lease.assignment_id, lease.fencing_token, 300, "r")
                .is_err()
        );
        assert_eq!(fixture.store.lease(&worker, 300, "r").unwrap(), None);
    }

    #[test]
    fn expired_leases_fail_after_the_attempt_limit_and_results_stay_secret_free() {
        let fixture = fixture();
        fixture.approve();
        fixture
            .store
            .create_assignments(
                &fixture.tenant,
                &fixture.project,
                &["a@acme.example".into()],
                &fixture.admin,
            )
            .unwrap();
        let worker = fixture.worker(&fixture.tenant, "w1");
        let mut last = None;
        for _ in 0..MAX_LEASE_ATTEMPTS {
            let lease = fixture.store.lease(&worker, 300, "r").unwrap().unwrap();
            fixture.expire_lease(&lease.assignment_id);
            last = Some(lease);
        }
        assert_eq!(fixture.store.lease(&worker, 300, "r").unwrap(), None);
        let stored = &fixture
            .store
            .assignments(&fixture.tenant, &fixture.project)
            .unwrap()[0];
        assert_eq!(stored.state, "failed");

        let fixture = self::fixture();
        fixture.approve();
        fixture
            .store
            .create_assignments(
                &fixture.tenant,
                &fixture.project,
                &["a@acme.example".into()],
                &fixture.admin,
            )
            .unwrap();
        let worker = fixture.worker(&fixture.tenant, "w1");
        let lease = fixture.store.lease(&worker, 300, "r").unwrap().unwrap();
        assert!(matches!(
            fixture.store.complete(
                &worker,
                &lease.assignment_id,
                lease.fencing_token,
                Outcome::Failed,
                &serde_json::json!({"oauth_token": "x"}),
                "r"
            ),
            Err(StoreError::Invalid(_))
        ));
        drop(last);
    }

    #[test]
    fn enrollment_tokens_are_short_lived_and_typed() {
        let fixture = fixture();
        assert!(
            fixture
                .store
                .create_enrollment(
                    &fixture.tenant,
                    "w",
                    MAX_ENROLLMENT_MINUTES + 1,
                    &fixture.admin
                )
                .is_err()
        );
        assert_eq!(
            fixture
                .store
                .enroll_worker("mss_s_not-an-enrollment", &[], "r")
                .map(|_| ()),
            Err(StoreError::NotFound)
        );
        let (token, _) = fixture
            .store
            .create_enrollment(&fixture.tenant, "w", 1, &fixture.admin)
            .unwrap();
        fixture
            .store
            .lock()
            .execute("UPDATE worker_enrollments SET expires_at=?1", [now() - 1])
            .unwrap();
        assert_eq!(
            fixture.store.enroll_worker(&token, &[], "r").map(|_| ()),
            Err(StoreError::NotFound)
        );
        assert!(
            fixture
                .store
                .enroll_worker(&"x".repeat(10), &["bad cap".into()], "r")
                .is_err()
        );
    }
}
