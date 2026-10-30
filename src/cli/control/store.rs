//! Central control-plane state: tenants, users, grants, sessions, API keys,
//! projects, and the audit log, in one SQLite file owned by the service.
//!
//! Tenant isolation is enforced here as well as in the API: every query for
//! a tenant-owned row names the tenant, so an identifier from another tenant
//! simply matches nothing. Mailbox credentials are never stored; project
//! plans are refused when they carry credential-like fields.

use super::audit::{AuditEntry, chain_hash};
use super::authn::{self, TokenKind};
use super::authz::{Credential, Grant, Permission, Principal, Role};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;
use zeroize::Zeroizing;

const SCHEMA_VERSION: i64 = 3;
pub(super) const SESSION_SECONDS: i64 = 12 * 60 * 60;
pub(super) const MAX_API_KEY_DAYS: i64 = 365;
pub(super) const LOCKOUT_FAILURES: i64 = 5;
const LOCKOUT_SECONDS: i64 = 15 * 60;
pub(super) const MAX_PLAN_BYTES: usize = 256 * 1024;
const MAX_NAME_CHARS: usize = 200;

const SCHEMA: &str = "
CREATE TABLE tenants(
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
) STRICT;
CREATE TABLE users(
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name TEXT NOT NULL,
    password_hash TEXT,
    failed_logins INTEGER NOT NULL DEFAULT 0 CHECK(failed_logins >= 0),
    locked_until INTEGER,
    disabled_at INTEGER,
    created_at INTEGER NOT NULL
) STRICT;
CREATE TABLE role_grants(
    user_id TEXT NOT NULL REFERENCES users(id),
    role TEXT NOT NULL CHECK(role IN ('administrator','manager','engineer','technician','auditor','customer')),
    tenant_id TEXT REFERENCES tenants(id),
    granted_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    CHECK((role = 'administrator') = (tenant_id IS NULL))
) STRICT;
CREATE UNIQUE INDEX role_grants_unique ON role_grants(user_id, role, COALESCE(tenant_id, ''));
CREATE TABLE sessions(
    token_hash BLOB PRIMARY KEY CHECK(length(token_hash) = 32),
    user_id TEXT NOT NULL REFERENCES users(id),
    method TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER
) STRICT;
CREATE INDEX sessions_user ON sessions(user_id);
CREATE TABLE api_keys(
    id TEXT PRIMARY KEY,
    key_hash BLOB NOT NULL UNIQUE CHECK(length(key_hash) = 32),
    owner_user_id TEXT NOT NULL REFERENCES users(id),
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    name TEXT NOT NULL,
    scopes TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER,
    last_used_at INTEGER
) STRICT;
CREATE INDEX api_keys_owner ON api_keys(owner_user_id);
CREATE TABLE projects(
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    name TEXT NOT NULL,
    plan TEXT NOT NULL,
    plan_revision INTEGER NOT NULL CHECK(plan_revision >= 1),
    plan_changed_by TEXT NOT NULL,
    approved_revision INTEGER,
    approved_by TEXT,
    created_at INTEGER NOT NULL,
    UNIQUE(tenant_id, name)
) STRICT;
CREATE TABLE audit_log(
    seq INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,
    request_id TEXT NOT NULL,
    actor TEXT NOT NULL,
    tenant_id TEXT,
    action TEXT NOT NULL,
    resource TEXT NOT NULL,
    decision TEXT NOT NULL CHECK(decision IN ('allow','deny')),
    detail TEXT NOT NULL,
    prev_hash BLOB NOT NULL CHECK(length(prev_hash) = 32),
    hash BLOB NOT NULL CHECK(length(hash) = 32)
) STRICT;
CREATE INDEX audit_log_tenant ON audit_log(tenant_id, seq);
CREATE TRIGGER audit_log_no_update BEFORE UPDATE ON audit_log
    BEGIN SELECT RAISE(ABORT, 'the audit log is append-only'); END;
CREATE TRIGGER audit_log_no_delete BEFORE DELETE ON audit_log
    BEGIN SELECT RAISE(ABORT, 'the audit log is append-only'); END;
";

pub(super) fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Who is acting, for audit entries written alongside a mutation.
#[derive(Clone, Debug)]
pub(super) struct Actor<'a> {
    pub(super) request_id: &'a str,
    pub(super) label: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum StoreError {
    /// The request conflicts with current state (duplicate name, stale
    /// approval, separation of duties).
    Conflict(String),
    /// The request is malformed.
    Invalid(String),
    /// The named resource does not exist in the caller's scope.
    NotFound,
    Internal(String),
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        match &error {
            rusqlite::Error::SqliteFailure(failure, _)
                if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                StoreError::Conflict("the change conflicts with an existing record".into())
            }
            _ => StoreError::Internal(error.to_string()),
        }
    }
}

pub(super) type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LoginError {
    /// Unknown user, wrong password, disabled account, or no password: one
    /// answer for all, so accounts cannot be enumerated.
    Rejected,
    Locked,
    Internal(String),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct Tenant {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct User {
    pub(super) id: String,
    pub(super) email: String,
    pub(super) display_name: String,
    pub(super) has_password: bool,
    pub(super) disabled: bool,
    pub(super) grants: Vec<GrantView>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct GrantView {
    pub(super) role: &'static str,
    pub(super) tenant_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct ApiKeyView {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) tenant_id: String,
    pub(super) scopes: Vec<String>,
    pub(super) created_at: i64,
    pub(super) expires_at: i64,
    pub(super) revoked: bool,
    pub(super) last_used_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct Project {
    pub(super) id: String,
    pub(super) tenant_id: String,
    pub(super) name: String,
    pub(super) plan: serde_json::Value,
    pub(super) plan_revision: i64,
    pub(super) plan_changed_by: String,
    pub(super) approved: bool,
    pub(super) approved_revision: Option<i64>,
    pub(super) approved_by: Option<String>,
    pub(super) created_at: i64,
}

pub(super) struct Store {
    connection: Mutex<Connection>,
}

impl Store {
    /// Open (creating if needed) the control-plane database at `path`. A new
    /// file is created owner-only; an existing file must already be.
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        let existed = path.exists();
        if !existed {
            crate::credentials::open_secret_file(path)
                .map_err(|error| format!("could not create {}: {error}", path.display()))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = std::fs::symlink_metadata(path)
                .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(format!(
                    "{} must be a regular file readable only by its owner (0600)",
                    path.display()
                ));
            }
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(format!(
                    "{} must be owned by the user running the control plane",
                    path.display()
                ));
            }
        }
        let connection = Connection::open(path)
            .map_err(|error| format!("could not open {}: {error}", path.display()))?;
        Self::from_connection(connection)
    }

    #[cfg(test)]
    pub(super) fn in_memory() -> Self {
        Self::from_connection(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn from_connection(connection: Connection) -> Result<Self, String> {
        authn::initialize_password_timing_equalizer()?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA secure_delete=ON;",
            )
            .map_err(|error| format!("could not configure the database: {error}"))?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        match version {
            0..=2 => {
                let base = if version == 0 { SCHEMA } else { "" };
                let workers = if version <= 1 {
                    super::workers::SCHEMA_V2
                } else {
                    ""
                };
                connection
                    .execute_batch(&format!(
                        "BEGIN IMMEDIATE; {base} {workers} {} PRAGMA user_version={SCHEMA_VERSION}; COMMIT;",
                        super::oidc::SCHEMA_V3
                    ))
                    .map_err(|error| format!("could not create or upgrade the schema: {error}"))?;
            }
            SCHEMA_VERSION => {}
            other => {
                return Err(format!(
                    "the control-plane database has schema version {other}; this build supports {SCHEMA_VERSION}"
                ));
            }
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    // ----- audit ---------------------------------------------------------

    /// Append one entry. Used for reads and refusals; mutations write their
    /// entry inside their own transaction with [`append_audit`].
    pub(super) fn audit(&self, entry: &AuditEntry<'_>) -> StoreResult<()> {
        let mut connection = self.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        append_audit(&tx, entry)?;
        tx.commit()?;
        Ok(())
    }

    /// Audit entries, newest first, optionally limited to one tenant.
    pub(super) fn audit_entries(
        &self,
        tenant: Option<&str>,
        before: Option<i64>,
        limit: i64,
    ) -> StoreResult<Vec<serde_json::Value>> {
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT seq,at,request_id,actor,tenant_id,action,resource,decision,detail,hash FROM audit_log
             WHERE (?1 IS NULL OR tenant_id=?1) AND (?2 IS NULL OR seq<?2)
             ORDER BY seq DESC LIMIT ?3",
        )?;
        let rows = statement
            .query_map(params![tenant, before, limit.clamp(1, 1000)], |row| {
                Ok(serde_json::json!({
                    "seq": row.get::<_, i64>(0)?,
                    "at": row.get::<_, i64>(1)?,
                    "request_id": row.get::<_, String>(2)?,
                    "actor": row.get::<_, String>(3)?,
                    "tenant_id": row.get::<_, Option<String>>(4)?,
                    "action": row.get::<_, String>(5)?,
                    "resource": row.get::<_, String>(6)?,
                    "decision": row.get::<_, String>(7)?,
                    "detail": row.get::<_, String>(8)?,
                    "hash": hex(&row.get::<_, Vec<u8>>(9)?),
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Recompute the hash chain. Returns the number of entries verified, or
    /// the sequence number of the first entry that does not match.
    pub(super) fn verify_audit_chain(&self) -> Result<u64, String> {
        let connection = self.lock();
        let mut statement = connection
            .prepare(
                "SELECT seq,at,request_id,actor,tenant_id,action,resource,decision,detail,prev_hash,hash FROM audit_log ORDER BY seq",
            )
            .map_err(|error| error.to_string())?;
        let mut rows = statement.query([]).map_err(|error| error.to_string())?;
        let mut previous = [0_u8; 32];
        let mut count = 0_u64;
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            let read = |index| {
                row.get::<_, String>(index)
                    .map_err(|error| error.to_string())
            };
            let seq: i64 = row.get(0).map_err(|error| error.to_string())?;
            let tenant = row
                .get::<_, Option<String>>(4)
                .map_err(|error| error.to_string())?;
            let entry = AuditEntry {
                at: row.get(1).map_err(|error| error.to_string())?,
                request_id: &read(2)?,
                actor: &read(3)?,
                tenant: tenant.as_deref(),
                action: &read(5)?,
                resource: &read(6)?,
                allowed: read(7)? == "allow",
                detail: &read(8)?,
            };
            let prev_hash: Vec<u8> = row.get(9).map_err(|error| error.to_string())?;
            let hash: Vec<u8> = row.get(10).map_err(|error| error.to_string())?;
            if prev_hash != previous || hash != chain_hash(&previous, &entry) {
                return Err(format!(
                    "audit log entry {seq} does not match the hash chain"
                ));
            }
            previous.copy_from_slice(&hash);
            count += 1;
        }
        Ok(count)
    }

    // ----- users, sessions, and login ------------------------------------

    /// Create the first administrator. Refused once any user exists, so the
    /// bootstrap command cannot be used to add a second, unaudited admin.
    pub(super) fn bootstrap_administrator(
        &self,
        email: &str,
        display_name: &str,
        password: &str,
    ) -> StoreResult<String> {
        let email = validate_email(email)?;
        let display_name = validate_name(display_name, "display name")?;
        let hash = authn::hash_password(password).map_err(StoreError::Invalid)?;
        let id = new_id("usr")?;
        let mut connection = self.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let users: i64 = tx.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        if users > 0 {
            return Err(StoreError::Conflict(
                "the control plane already has users; add administrators through the API".into(),
            ));
        }
        let at = now();
        tx.execute(
            "INSERT INTO users(id,email,display_name,password_hash,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![id, email, display_name, hash, at],
        )?;
        tx.execute(
            "INSERT INTO role_grants(user_id,role,tenant_id,granted_by,created_at) VALUES(?1,'administrator',NULL,'bootstrap',?2)",
            params![id, at],
        )?;
        append_audit(
            &tx,
            &AuditEntry {
                at,
                request_id: "bootstrap",
                actor: "bootstrap",
                tenant: None,
                action: "user.bootstrap_administrator",
                resource: &format!("user:{id}"),
                allowed: true,
                detail: &email,
            },
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Verify a password and open a session. Repeated failures lock the
    /// account for a while; every failure costs the same hashing work.
    pub(super) fn login(
        &self,
        email: &str,
        password: &str,
        request_id: &str,
    ) -> Result<(Zeroizing<String>, i64), LoginError> {
        let internal = |error: rusqlite::Error| LoginError::Internal(error.to_string());
        let at = now();
        let account: Option<(String, Option<String>, Option<i64>, bool)> = self
            .lock()
            .query_row(
                "SELECT id,password_hash,locked_until,disabled_at IS NOT NULL FROM users WHERE email=?1",
                [email.trim()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(internal)?;
        let Some((user_id, Some(stored), locked_until, disabled)) = account else {
            authn::equalize_password_timing(password).map_err(LoginError::Internal)?;
            return Err(LoginError::Rejected);
        };
        if locked_until.is_some_and(|until| until > at) {
            authn::equalize_password_timing(password).map_err(LoginError::Internal)?;
            return Err(LoginError::Locked);
        }
        let verified = authn::verify_password(password, &stored);
        let mut connection = self.lock();
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(internal)?;
        if !verified || disabled {
            if !disabled {
                tx.execute(
                    "UPDATE users SET locked_until=CASE WHEN failed_logins+1>=?2 THEN ?3 ELSE locked_until END,
                        failed_logins=CASE WHEN failed_logins+1>=?2 THEN 0 ELSE failed_logins+1 END
                     WHERE id=?1",
                    params![user_id, LOCKOUT_FAILURES, at + LOCKOUT_SECONDS],
                )
                .map_err(internal)?;
            }
            append_audit(
                &tx,
                &AuditEntry {
                    at,
                    request_id,
                    actor: &format!("user:{user_id}"),
                    tenant: None,
                    action: "session.login",
                    resource: &format!("user:{user_id}"),
                    allowed: false,
                    detail: if disabled {
                        "account disabled"
                    } else {
                        "wrong password"
                    },
                },
            )
            .map_err(|error| LoginError::Internal(format!("{error:?}")))?;
            tx.commit().map_err(internal)?;
            return Err(LoginError::Rejected);
        }
        tx.execute(
            "UPDATE users SET failed_logins=0,locked_until=NULL WHERE id=?1",
            [&user_id],
        )
        .map_err(internal)?;
        let session = insert_session(&tx, &user_id, "password", at)
            .map_err(|error| LoginError::Internal(format!("{error:?}")))?;
        append_audit(
            &tx,
            &AuditEntry {
                at,
                request_id,
                actor: &format!("user:{user_id}"),
                tenant: None,
                action: "session.login",
                resource: &format!("user:{user_id}"),
                allowed: true,
                detail: "password",
            },
        )
        .map_err(|error| LoginError::Internal(format!("{error:?}")))?;
        tx.commit().map_err(internal)?;
        Ok(session)
    }

    /// Open a session for a user already authenticated another way (OIDC).
    pub(super) fn open_session(
        &self,
        user_id: &str,
        method: &str,
        actor: &Actor<'_>,
    ) -> StoreResult<(Zeroizing<String>, i64)> {
        let mut connection = self.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let at = now();
        let session = insert_session(&tx, user_id, method, at)?;
        append_audit(
            &tx,
            &AuditEntry {
                at,
                request_id: actor.request_id,
                actor: &actor.label,
                tenant: None,
                action: "session.login",
                resource: &format!("user:{user_id}"),
                allowed: true,
                detail: method,
            },
        )?;
        tx.commit()?;
        Ok(session)
    }

    pub(super) fn logout(&self, token: &str, actor: &Actor<'_>) -> StoreResult<()> {
        let mut connection = self.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let at = now();
        tx.execute(
            "UPDATE sessions SET revoked_at=?2 WHERE token_hash=?1 AND revoked_at IS NULL",
            params![authn::token_hash(token).as_slice(), at],
        )?;
        append_audit(
            &tx,
            &AuditEntry {
                at,
                request_id: actor.request_id,
                actor: &actor.label,
                tenant: None,
                action: "session.logout",
                resource: "session",
                allowed: true,
                detail: "",
            },
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Resolve a bearer token to the caller it authenticates, or `None` for
    /// an unknown, expired, revoked, or disabled-owner credential.
    pub(super) fn principal(&self, token: &str) -> StoreResult<Option<Principal>> {
        let at = now();
        let hash = authn::token_hash(token);
        let connection = self.lock();
        let (user_id, via) = match TokenKind::of(token) {
            Some(TokenKind::Session) => {
                let Some(user_id) = connection
                    .query_row(
                        "SELECT s.user_id FROM sessions s JOIN users u ON u.id=s.user_id
                         WHERE s.token_hash=?1 AND s.revoked_at IS NULL AND s.expires_at>?2 AND u.disabled_at IS NULL",
                        params![hash.as_slice(), at],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                else {
                    return Ok(None);
                };
                (user_id, Credential::Session)
            }
            Some(TokenKind::ApiKey) => {
                let Some((key_id, user_id, tenant, scopes, last_used)) = connection
                    .query_row(
                        "SELECT k.id,k.owner_user_id,k.tenant_id,k.scopes,k.last_used_at FROM api_keys k JOIN users u ON u.id=k.owner_user_id
                         WHERE k.key_hash=?1 AND k.revoked_at IS NULL AND k.expires_at>?2 AND u.disabled_at IS NULL",
                        params![hash.as_slice(), at],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, Option<i64>>(4)?,
                            ))
                        },
                    )
                    .optional()?
                else {
                    return Ok(None);
                };
                // Record use at most once a minute; it is operational
                // metadata, not an audit record.
                if last_used.is_none_or(|last| at - last >= 60) {
                    connection.execute(
                        "UPDATE api_keys SET last_used_at=?2 WHERE id=?1",
                        params![key_id, at],
                    )?;
                }
                let scopes = scopes
                    .split(',')
                    .filter_map(Permission::parse)
                    .collect::<BTreeSet<_>>();
                (
                    user_id,
                    Credential::ApiKey {
                        key_id,
                        tenant,
                        scopes,
                    },
                )
            }
            _ => return Ok(None),
        };
        let grants = grants_of(&connection, &user_id)?;
        Ok(Some(Principal {
            user_id,
            grants,
            via,
        }))
    }

    pub(super) fn user(&self, user_id: &str) -> StoreResult<User> {
        let connection = self.lock();
        let (email, display_name, has_password, disabled) = connection
            .query_row(
                "SELECT email,display_name,password_hash IS NOT NULL,disabled_at IS NOT NULL FROM users WHERE id=?1",
                [user_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        let grants = grants_of(&connection, user_id)?
            .into_iter()
            .map(|grant| GrantView {
                role: grant.role.as_str(),
                tenant_id: grant.tenant,
            })
            .collect();
        Ok(User {
            id: user_id.to_owned(),
            email,
            display_name,
            has_password,
            disabled,
            grants,
        })
    }

    pub(super) fn users(&self) -> StoreResult<Vec<User>> {
        let ids = {
            let connection = self.lock();
            let mut statement = connection.prepare("SELECT id FROM users ORDER BY email")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        ids.iter().map(|id| self.user(id)).collect()
    }

    /// Create a user. Without a password the account can sign in only
    /// through a linked OIDC identity.
    pub(super) fn create_user(
        &self,
        email: &str,
        display_name: &str,
        password: Option<&str>,
        actor: &Actor<'_>,
    ) -> StoreResult<String> {
        let email = validate_email(email)?;
        let display_name = validate_name(display_name, "display name")?;
        let hash = password
            .map(authn::hash_password)
            .transpose()
            .map_err(StoreError::Invalid)?;
        let id = new_id("usr")?;
        self.mutate(actor, None, "user.create", &format!("user:{id}"), &email, |tx| {
            tx.execute(
                "INSERT INTO users(id,email,display_name,password_hash,created_at) VALUES(?1,?2,?3,?4,?5)",
                params![id, email, display_name, hash, now()],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    /// Disable a user and revoke every session and API key they hold.
    pub(super) fn disable_user(&self, user_id: &str, actor: &Actor<'_>) -> StoreResult<()> {
        self.mutate(
            actor,
            None,
            "user.disable",
            &format!("user:{user_id}"),
            "",
            |tx| {
                let at = now();
                let changed = tx.execute(
                    "UPDATE users SET disabled_at=?2 WHERE id=?1 AND disabled_at IS NULL",
                    params![user_id, at],
                )?;
                if changed == 0 {
                    return Err(StoreError::NotFound);
                }
                tx.execute(
                    "UPDATE sessions SET revoked_at=?2 WHERE user_id=?1 AND revoked_at IS NULL",
                    params![user_id, at],
                )?;
                tx.execute(
                "UPDATE api_keys SET revoked_at=?2 WHERE owner_user_id=?1 AND revoked_at IS NULL",
                params![user_id, at],
            )?;
                Ok(())
            },
        )
    }

    pub(super) fn add_grant(
        &self,
        user_id: &str,
        role: Role,
        tenant: Option<&str>,
        actor: &Actor<'_>,
    ) -> StoreResult<()> {
        if role.is_global() != tenant.is_none() {
            return Err(StoreError::Invalid(if role.is_global() {
                "the administrator role is service-wide and takes no tenant".into()
            } else {
                format!("the {} role needs a tenant", role.as_str())
            }));
        }
        self.mutate(
            actor,
            tenant,
            "grant.add",
            &format!("user:{user_id}"),
            role.as_str(),
            |tx| {
                let user_exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1)",
                    [user_id],
                    |row| row.get(0),
                )?;
                let tenant_exists = match tenant {
                    None => true,
                    Some(tenant) => tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM tenants WHERE id=?1)",
                        [tenant],
                        |row| row.get(0),
                    )?,
                };
                if !user_exists || !tenant_exists {
                    return Err(StoreError::NotFound);
                }
                tx.execute(
                    "INSERT INTO role_grants(user_id,role,tenant_id,granted_by,created_at) VALUES(?1,?2,?3,?4,?5)",
                    params![user_id, role.as_str(), tenant, actor.label, now()],
                )?;
                Ok(())
            },
        )
    }

    pub(super) fn remove_grant(
        &self,
        user_id: &str,
        role: Role,
        tenant: Option<&str>,
        actor: &Actor<'_>,
    ) -> StoreResult<()> {
        self.mutate(
            actor,
            tenant,
            "grant.remove",
            &format!("user:{user_id}"),
            role.as_str(),
            |tx| {
                let removed = tx.execute(
                    "DELETE FROM role_grants WHERE user_id=?1 AND role=?2 AND COALESCE(tenant_id,'')=COALESCE(?3,'')",
                    params![user_id, role.as_str(), tenant],
                )?;
                if removed == 0 {
                    return Err(StoreError::NotFound);
                }
                if role == Role::Administrator {
                    let remaining: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM role_grants g JOIN users u ON u.id=g.user_id WHERE g.role='administrator' AND u.disabled_at IS NULL",
                        [],
                        |row| row.get(0),
                    )?;
                    if remaining == 0 {
                        return Err(StoreError::Conflict(
                            "the last active administrator cannot be removed".into(),
                        ));
                    }
                }
                Ok(())
            },
        )
    }

    // ----- tenants --------------------------------------------------------

    pub(super) fn create_tenant(&self, name: &str, actor: &Actor<'_>) -> StoreResult<Tenant> {
        let name = validate_name(name, "tenant name")?;
        let id = new_id("ten")?;
        let created_at = now();
        self.mutate(
            actor,
            Some(&id),
            "tenant.create",
            &format!("tenant:{id}"),
            &name,
            |tx| {
                tx.execute(
                    "INSERT INTO tenants(id,name,created_at) VALUES(?1,?2,?3)",
                    params![id, name, created_at],
                )?;
                Ok(())
            },
        )?;
        Ok(Tenant {
            id,
            name,
            created_at,
        })
    }

    /// Tenants, limited to `visible` when given.
    pub(super) fn tenants(&self, visible: Option<&BTreeSet<String>>) -> StoreResult<Vec<Tenant>> {
        let connection = self.lock();
        let mut statement =
            connection.prepare("SELECT id,name,created_at FROM tenants ORDER BY name")?;
        let tenants = statement
            .query_map([], |row| {
                Ok(Tenant {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tenants
            .into_iter()
            .filter(|tenant| visible.is_none_or(|visible| visible.contains(&tenant.id)))
            .collect())
    }

    pub(super) fn tenant_exists(&self, tenant: &str) -> StoreResult<bool> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM tenants WHERE id=?1)",
            [tenant],
            |row| row.get(0),
        )?)
    }

    // ----- API keys -------------------------------------------------------

    /// Issue a key for `principal`'s user in `tenant`. Every requested scope
    /// must be one the user currently holds there; the secret is returned
    /// once and only its digest is stored.
    pub(super) fn create_api_key(
        &self,
        principal: &Principal,
        tenant: &str,
        name: &str,
        scopes: &BTreeSet<Permission>,
        lifetime_days: i64,
        actor: &Actor<'_>,
    ) -> StoreResult<(ApiKeyView, Zeroizing<String>)> {
        let name = validate_name(name, "key name")?;
        if scopes.is_empty() {
            return Err(StoreError::Invalid(
                "an API key needs at least one scope".into(),
            ));
        }
        if !(1..=MAX_API_KEY_DAYS).contains(&lifetime_days) {
            return Err(StoreError::Invalid(format!(
                "API keys expire after 1 to {MAX_API_KEY_DAYS} days"
            )));
        }
        for scope in scopes {
            if !scope.grantable_to_api_key() || !principal.allows(*scope, Some(tenant)) {
                return Err(StoreError::Invalid(format!(
                    "you cannot issue a key with the {} scope in this tenant",
                    scope.as_str()
                )));
            }
        }
        let id = new_id("key")?;
        let token = authn::new_token(TokenKind::ApiKey).map_err(StoreError::Internal)?;
        let created_at = now();
        let expires_at = created_at + lifetime_days * 86_400;
        let scope_list = scopes
            .iter()
            .map(|scope| scope.as_str())
            .collect::<Vec<_>>();
        self.mutate(
            actor,
            Some(tenant),
            "api_key.create",
            &format!("api_key:{id}"),
            &scope_list.join(","),
            |tx| {
                tx.execute(
                    "INSERT INTO api_keys(id,key_hash,owner_user_id,tenant_id,name,scopes,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        id,
                        authn::token_hash(&token).as_slice(),
                        principal.user_id,
                        tenant,
                        name,
                        scope_list.join(","),
                        created_at,
                        expires_at
                    ],
                )?;
                Ok(())
            },
        )?;
        Ok((
            ApiKeyView {
                id,
                name,
                tenant_id: tenant.to_owned(),
                scopes: scope_list.into_iter().map(str::to_owned).collect(),
                created_at,
                expires_at,
                revoked: false,
                last_used_at: None,
            },
            token,
        ))
    }

    /// Keys owned by `owner`, or every key when `owner` is `None`.
    pub(super) fn api_keys(&self, owner: Option<&str>) -> StoreResult<Vec<ApiKeyView>> {
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT id,name,tenant_id,scopes,created_at,expires_at,revoked_at IS NOT NULL,last_used_at FROM api_keys WHERE ?1 IS NULL OR owner_user_id=?1 ORDER BY created_at DESC",
        )?;
        let keys = statement
            .query_map([owner], |row| {
                Ok(ApiKeyView {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    tenant_id: row.get(2)?,
                    scopes: row
                        .get::<_, String>(3)?
                        .split(',')
                        .map(str::to_owned)
                        .collect(),
                    created_at: row.get(4)?,
                    expires_at: row.get(5)?,
                    revoked: row.get(6)?,
                    last_used_at: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(keys)
    }

    /// Revoke a key. `owner` limits it to the caller's own keys; an
    /// administrator passes `None`.
    pub(super) fn revoke_api_key(
        &self,
        key_id: &str,
        owner: Option<&str>,
        actor: &Actor<'_>,
    ) -> StoreResult<()> {
        self.mutate(actor, None, "api_key.revoke", &format!("api_key:{key_id}"), "", |tx| {
            let changed = tx.execute(
                "UPDATE api_keys SET revoked_at=?3 WHERE id=?1 AND (?2 IS NULL OR owner_user_id=?2) AND revoked_at IS NULL",
                params![key_id, owner, now()],
            )?;
            if changed == 0 {
                return Err(StoreError::NotFound);
            }
            Ok(())
        })
    }

    // ----- projects -------------------------------------------------------

    pub(super) fn create_project(
        &self,
        tenant: &str,
        name: &str,
        plan: &serde_json::Value,
        actor: &Actor<'_>,
    ) -> StoreResult<Project> {
        let name = validate_name(name, "project name")?;
        let plan_text = validate_plan(plan)?;
        let id = new_id("prj")?;
        self.mutate(actor, Some(tenant), "project.create", &format!("project:{id}"), &name, |tx| {
            tx.execute(
                "INSERT INTO projects(id,tenant_id,name,plan,plan_revision,plan_changed_by,created_at) VALUES(?1,?2,?3,?4,1,?5,?6)",
                params![id, tenant, name, plan_text, actor.label, now()],
            )?;
            Ok(())
        })?;
        self.project(tenant, &id)
    }

    pub(super) fn projects(&self, tenant: &str) -> StoreResult<Vec<Project>> {
        let connection = self.lock();
        let mut statement = connection.prepare(&format!(
            "SELECT {PROJECT_COLUMNS} FROM projects WHERE tenant_id=?1 ORDER BY name"
        ))?;
        let projects = statement
            .query_map([tenant], project_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(projects)
    }

    pub(super) fn project(&self, tenant: &str, id: &str) -> StoreResult<Project> {
        self.lock()
            .query_row(
                &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE tenant_id=?1 AND id=?2"),
                params![tenant, id],
                project_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    /// Replace a project's plan. Any change withdraws a prior approval.
    pub(super) fn update_plan(
        &self,
        tenant: &str,
        id: &str,
        plan: &serde_json::Value,
        expected_revision: i64,
        actor: &Actor<'_>,
    ) -> StoreResult<Project> {
        let plan_text = validate_plan(plan)?;
        self.mutate(actor, Some(tenant), "project.update_plan", &format!("project:{id}"), "", |tx| {
            let changed = tx.execute(
                "UPDATE projects SET plan=?3,plan_revision=plan_revision+1,plan_changed_by=?4,approved_revision=NULL,approved_by=NULL
                 WHERE tenant_id=?1 AND id=?2 AND plan_revision=?5",
                params![tenant, id, plan_text, actor.label, expected_revision],
            )?;
            if changed == 0 {
                return Err(revision_conflict_or_missing(tx, tenant, id)?);
            }
            Ok(())
        })?;
        self.project(tenant, id)
    }

    /// Approve the current plan revision. The person who last changed the
    /// plan cannot approve it (separation of duties), and approving a
    /// revision other than the current one is refused.
    pub(super) fn approve_project(
        &self,
        tenant: &str,
        id: &str,
        revision: i64,
        approver_user_id: &str,
        actor: &Actor<'_>,
    ) -> StoreResult<Project> {
        self.mutate(
            actor,
            Some(tenant),
            "project.approve",
            &format!("project:{id}"),
            &format!("revision {revision}"),
            |tx| {
                let current: Option<(i64, String)> = tx
                    .query_row(
                        "SELECT plan_revision,plan_changed_by FROM projects WHERE tenant_id=?1 AND id=?2",
                        params![tenant, id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let Some((current_revision, changed_by)) = current else {
                    return Err(StoreError::NotFound);
                };
                if current_revision != revision {
                    return Err(StoreError::Conflict(format!(
                        "the plan is at revision {current_revision}; review it and approve that revision"
                    )));
                }
                if changed_by == format!("user:{approver_user_id}")
                    || changed_by.starts_with(&format!("user:{approver_user_id} "))
                {
                    return Err(StoreError::Conflict(
                        "the person who last changed the plan cannot approve it".into(),
                    ));
                }
                tx.execute(
                    "UPDATE projects SET approved_revision=?3,approved_by=?4 WHERE tenant_id=?1 AND id=?2",
                    params![tenant, id, revision, actor.label],
                )?;
                Ok(())
            },
        )?;
        self.project(tenant, id)
    }

    /// Run `change` and its audit entry in one transaction; a failed change
    /// is audited as a refusal instead.
    pub(super) fn mutate<T>(
        &self,
        actor: &Actor<'_>,
        tenant: Option<&str>,
        action: &str,
        resource: &str,
        detail: &str,
        change: impl FnOnce(&Transaction<'_>) -> StoreResult<T>,
    ) -> StoreResult<T> {
        let result = {
            let mut connection = self.lock();
            let tx =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            match change(&tx) {
                Ok(value) => {
                    append_audit(
                        &tx,
                        &AuditEntry {
                            at: now(),
                            request_id: actor.request_id,
                            actor: &actor.label,
                            tenant,
                            action,
                            resource,
                            allowed: true,
                            detail,
                        },
                    )?;
                    tx.commit()?;
                    Ok(value)
                }
                Err(error) => Err(error),
            }
        };
        if let Err(error) = &result {
            let reason = match error {
                StoreError::Conflict(reason) | StoreError::Invalid(reason) => reason.as_str(),
                StoreError::NotFound => "not found",
                StoreError::Internal(_) => "internal error",
            };
            let _ = self.audit(&AuditEntry {
                at: now(),
                request_id: actor.request_id,
                actor: &actor.label,
                tenant,
                action,
                resource,
                allowed: false,
                detail: reason,
            });
        }
        result
    }
}

const PROJECT_COLUMNS: &str =
    "id,tenant_id,name,plan,plan_revision,plan_changed_by,approved_revision,approved_by,created_at";

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    let plan_revision: i64 = row.get(4)?;
    let approved_revision: Option<i64> = row.get(6)?;
    let plan_json: String = row.get(3)?;
    let plan = serde_json::from_str(&plan_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Project {
        id: row.get(0)?,
        tenant_id: row.get(1)?,
        name: row.get(2)?,
        plan,
        plan_revision,
        plan_changed_by: row.get(5)?,
        approved: approved_revision == Some(plan_revision),
        approved_revision,
        approved_by: row.get(7)?,
        created_at: row.get(8)?,
    })
}

fn revision_conflict_or_missing(
    tx: &Transaction<'_>,
    tenant: &str,
    id: &str,
) -> StoreResult<StoreError> {
    let current: Option<i64> = tx
        .query_row(
            "SELECT plan_revision FROM projects WHERE tenant_id=?1 AND id=?2",
            params![tenant, id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(match current {
        None => StoreError::NotFound,
        Some(revision) => StoreError::Conflict(format!(
            "the plan changed concurrently and is now at revision {revision}"
        )),
    })
}

fn grants_of(connection: &Connection, user_id: &str) -> rusqlite::Result<Vec<Grant>> {
    let mut statement = connection.prepare(
        "SELECT role,tenant_id FROM role_grants WHERE user_id=?1 ORDER BY role,tenant_id",
    )?;
    let grants = statement
        .query_map([user_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .filter_map(|row| {
            row.map(|(role, tenant)| Role::parse(&role).map(|role| Grant { role, tenant }))
                .transpose()
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(grants)
}

fn insert_session(
    tx: &Transaction<'_>,
    user_id: &str,
    method: &str,
    at: i64,
) -> StoreResult<(Zeroizing<String>, i64)> {
    let token = authn::new_token(TokenKind::Session).map_err(StoreError::Internal)?;
    let expires_at = at + SESSION_SECONDS;
    tx.execute(
        "INSERT INTO sessions(token_hash,user_id,method,created_at,expires_at) VALUES(?1,?2,?3,?4,?5)",
        params![authn::token_hash(&token).as_slice(), user_id, method, at, expires_at],
    )?;
    Ok((token, expires_at))
}

pub(super) fn append_audit(tx: &Transaction<'_>, entry: &AuditEntry<'_>) -> StoreResult<()> {
    let previous: Option<Vec<u8>> = tx
        .query_row(
            "SELECT hash FROM audit_log ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let mut prev_hash = [0_u8; 32];
    if let Some(previous) = previous {
        if previous.len() != 32 {
            return Err(StoreError::Internal(
                "the audit log hash chain is corrupt".into(),
            ));
        }
        prev_hash.copy_from_slice(&previous);
    }
    let hash = chain_hash(&prev_hash, entry);
    tx.execute(
        "INSERT INTO audit_log(at,request_id,actor,tenant_id,action,resource,decision,detail,prev_hash,hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            entry.at,
            entry.request_id,
            entry.actor,
            entry.tenant,
            entry.action,
            entry.resource,
            if entry.allowed { "allow" } else { "deny" },
            entry.detail,
            prev_hash.as_slice(),
            hash.as_slice()
        ],
    )?;
    Ok(())
}

pub(super) fn new_id(prefix: &str) -> StoreResult<String> {
    authn::new_id(prefix).map_err(StoreError::Internal)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn validate_name(value: &str, what: &str) -> StoreResult<String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > MAX_NAME_CHARS
        || value.chars().any(char::is_control)
    {
        return Err(StoreError::Invalid(format!(
            "the {what} must be 1 to {MAX_NAME_CHARS} characters without control characters"
        )));
    }
    Ok(value.to_owned())
}

fn validate_email(value: &str) -> StoreResult<String> {
    let value = validate_name(value, "email address")?;
    let valid = value.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty() && domain.contains('.') && !domain.contains('@')
    }) && !value.chars().any(char::is_whitespace);
    if !valid {
        return Err(StoreError::Invalid("the email address is not valid".into()));
    }
    Ok(value)
}

/// Plans are non-secret migration settings. Refuse any field whose name
/// suggests a credential, so the central service never becomes a store of
/// mailbox secrets by accident; workers keep those locally.
pub(super) fn validate_plan(plan: &serde_json::Value) -> StoreResult<String> {
    fn credential_field(value: &serde_json::Value) -> Option<String> {
        const MARKERS: [&str; 6] = [
            "password",
            "passwd",
            "secret",
            "token",
            "credential",
            "private_key",
        ];
        match value {
            serde_json::Value::Object(map) => map.iter().find_map(|(key, value)| {
                let lower = key.to_ascii_lowercase();
                if MARKERS.iter().any(|marker| lower.contains(marker)) {
                    Some(key.clone())
                } else {
                    credential_field(value)
                }
            }),
            serde_json::Value::Array(items) => items.iter().find_map(credential_field),
            _ => None,
        }
    }
    if !plan.is_object() {
        return Err(StoreError::Invalid("the plan must be a JSON object".into()));
    }
    if let Some(field) = credential_field(plan) {
        return Err(StoreError::Invalid(format!(
            "the plan field {field:?} looks like a credential; mailbox credentials stay on the worker and are never sent to the control plane"
        )));
    }
    let text =
        serde_json::to_string(plan).map_err(|error| StoreError::Invalid(error.to_string()))?;
    if text.len() > MAX_PLAN_BYTES {
        return Err(StoreError::Invalid(format!(
            "the plan exceeds {MAX_PLAN_BYTES} bytes"
        )));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(label: &str) -> Actor<'static> {
        Actor {
            request_id: "test",
            label: label.to_owned(),
        }
    }

    fn bootstrapped() -> (Store, String, String) {
        let store = Store::in_memory();
        let password = authn::test_secret();
        let admin = store
            .bootstrap_administrator("admin@msp.example", "Admin", &password)
            .unwrap();
        (store, admin, password)
    }

    #[test]
    fn bootstrap_is_one_time_and_login_issues_a_session() {
        let (store, admin, password) = bootstrapped();
        let second_password = authn::test_secret();
        assert!(matches!(
            store.bootstrap_administrator("second@msp.example", "Second", &second_password),
            Err(StoreError::Conflict(_))
        ));
        let (token, expires) = store.login("ADMIN@msp.example", &password, "r1").unwrap();
        assert!(expires > now());
        let principal = store.principal(&token).unwrap().unwrap();
        assert_eq!(principal.user_id, admin);
        assert!(principal.allows(Permission::ServiceAdmin, None));

        store.logout(&token, &actor("user:x")).unwrap();
        assert!(store.principal(&token).unwrap().is_none());
    }

    #[test]
    fn failed_logins_lock_the_account_without_revealing_which_accounts_exist() {
        let (store, _, password) = bootstrapped();
        let wrong_password = authn::test_secret();
        assert_eq!(
            store.login("nobody@msp.example", &wrong_password, "r"),
            Err(LoginError::Rejected)
        );
        for _ in 0..LOCKOUT_FAILURES {
            assert_eq!(
                store.login("admin@msp.example", &wrong_password, "r"),
                Err(LoginError::Rejected)
            );
        }
        assert_eq!(
            store.login("admin@msp.example", &password, "r"),
            Err(LoginError::Locked)
        );
    }

    #[test]
    fn disabling_a_user_revokes_sessions_and_keys() {
        let (store, admin, _) = bootstrapped();
        let engineer_password = authn::test_secret();
        let admin_actor = actor(&format!("user:{admin}"));
        let tenant = store.create_tenant("Acme", &admin_actor).unwrap();
        let user = store
            .create_user(
                "eng@msp.example",
                "Eng",
                Some(&engineer_password),
                &admin_actor,
            )
            .unwrap();
        store
            .add_grant(&user, Role::Engineer, Some(&tenant.id), &admin_actor)
            .unwrap();
        let (session, _) = store
            .login("eng@msp.example", &engineer_password, "r")
            .unwrap();
        let principal = store.principal(&session).unwrap().unwrap();
        let (_, key) = store
            .create_api_key(
                &principal,
                &tenant.id,
                "ci",
                &BTreeSet::from([Permission::ProjectRead]),
                30,
                &admin_actor,
            )
            .unwrap();
        assert!(store.principal(&key).unwrap().is_some());

        store.disable_user(&user, &admin_actor).unwrap();
        assert!(store.principal(&session).unwrap().is_none());
        assert!(store.principal(&key).unwrap().is_none());
        assert_eq!(
            store.login("eng@msp.example", &engineer_password, "r"),
            Err(LoginError::Rejected)
        );
    }

    #[test]
    fn api_keys_cannot_exceed_the_owner_grants() {
        let (store, admin, _) = bootstrapped();
        let admin_actor = actor(&format!("user:{admin}"));
        let tenant = store.create_tenant("Acme", &admin_actor).unwrap();
        let technician_password = authn::test_secret();
        let user = store
            .create_user(
                "tech@msp.example",
                "Tech",
                Some(&technician_password),
                &admin_actor,
            )
            .unwrap();
        store
            .add_grant(&user, Role::Technician, Some(&tenant.id), &admin_actor)
            .unwrap();
        let (session, _) = store
            .login("tech@msp.example", &technician_password, "r")
            .unwrap();
        let principal = store.principal(&session).unwrap().unwrap();
        assert!(matches!(
            store.create_api_key(
                &principal,
                &tenant.id,
                "too broad",
                &BTreeSet::from([Permission::ProjectWrite]),
                30,
                &admin_actor
            ),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.create_api_key(
                &principal,
                &tenant.id,
                "too long",
                &BTreeSet::from([Permission::ProjectRead]),
                MAX_API_KEY_DAYS + 1,
                &admin_actor
            ),
            Err(StoreError::Invalid(_))
        ));
    }

    #[test]
    fn projects_are_tenant_scoped_and_approval_separates_duties() {
        let (store, admin, _) = bootstrapped();
        let admin_actor = actor(&format!("user:{admin}"));
        let acme = store.create_tenant("Acme", &admin_actor).unwrap();
        let globex = store.create_tenant("Globex", &admin_actor).unwrap();
        let editor_actor = actor("user:usr_editor");
        let project = store
            .create_project(
                &acme.id,
                "Mail move",
                &serde_json::json!({"source_host": "imap.a"}),
                &editor_actor,
            )
            .unwrap();
        assert_eq!(
            store.project(&globex.id, &project.id),
            Err(StoreError::NotFound)
        );
        assert!(store.projects(&globex.id).unwrap().is_empty());

        assert!(matches!(
            store.approve_project(&acme.id, &project.id, 1, "usr_editor", &editor_actor),
            Err(StoreError::Conflict(_))
        ));
        let approved = store
            .approve_project(&acme.id, &project.id, 1, &admin, &admin_actor)
            .unwrap();
        assert!(approved.approved);

        let changed = store
            .update_plan(
                &acme.id,
                &project.id,
                &serde_json::json!({"source_host": "imap.b"}),
                1,
                &editor_actor,
            )
            .unwrap();
        assert!(!changed.approved, "a plan change withdraws approval");
        assert!(matches!(
            store.update_plan(
                &acme.id,
                &project.id,
                &serde_json::json!({}),
                1,
                &editor_actor
            ),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.approve_project(&acme.id, &project.id, 1, &admin, &admin_actor),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn plans_with_credential_fields_are_refused() {
        let (store, admin, _) = bootstrapped();
        let admin_actor = actor(&format!("user:{admin}"));
        let acme = store.create_tenant("Acme", &admin_actor).unwrap();
        for plan in [
            serde_json::json!({"source_password": "x"}),
            serde_json::json!({"mailboxes": [{"oauth": {"refresh_token": "x"}}]}),
            serde_json::json!(["not", "an", "object"]),
        ] {
            assert!(matches!(
                store.create_project(&acme.id, "p", &plan, &admin_actor),
                Err(StoreError::Invalid(_))
            ));
        }
    }

    #[test]
    fn the_audit_log_is_append_only_and_hash_chained() {
        let (store, admin, _) = bootstrapped();
        let admin_actor = actor(&format!("user:{admin}"));
        store.create_tenant("Acme", &admin_actor).unwrap();
        assert!(store.create_tenant("Acme", &admin_actor).is_err());
        let entries = store.audit_entries(None, None, 10).unwrap();
        assert_eq!(
            entries[0]["decision"], "deny",
            "a refused change is audited"
        );
        assert_eq!(store.verify_audit_chain().unwrap(), 3);

        let connection = store.lock();
        assert!(connection.execute("DELETE FROM audit_log", []).is_err());
        assert!(
            connection
                .execute("UPDATE audit_log SET detail='edited' WHERE seq=1", [])
                .is_err()
        );
        // Bypass the triggers to simulate tampering with the file itself.
        connection
            .execute_batch(
                "DROP TRIGGER audit_log_no_update; UPDATE audit_log SET detail='edited' WHERE seq=2;",
            )
            .unwrap();
        drop(connection);
        assert!(store.verify_audit_chain().unwrap_err().contains("entry 2"));
    }

    #[test]
    fn a_version_one_database_upgrades_in_place() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(&format!("{SCHEMA} PRAGMA user_version=1;"))
            .unwrap();
        connection
            .execute(
                "INSERT INTO tenants(id,name,created_at) VALUES('ten_old','Old',1)",
                [],
            )
            .unwrap();
        let store = Store::from_connection(connection).unwrap();
        let version: i64 = store
            .lock()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert!(store.tenant_exists("ten_old").unwrap());
        assert!(store.workers("ten_old").unwrap().is_empty());

        let newer = Connection::open_in_memory().unwrap();
        newer.execute_batch("PRAGMA user_version=99;").unwrap();
        assert!(Store::from_connection(newer).is_err());
    }

    #[test]
    fn the_last_administrator_cannot_be_removed() {
        let (store, admin, _) = bootstrapped();
        let admin_actor = actor(&format!("user:{admin}"));
        assert!(matches!(
            store.remove_grant(&admin, Role::Administrator, None, &admin_actor),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            store.add_grant(&admin, Role::Engineer, None, &admin_actor),
            Err(StoreError::Invalid(_))
        ));
    }
}
