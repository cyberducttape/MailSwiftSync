//! Durable migration-control-plane primitives.
//!
//! The GUI may be replaced, but project state and verification evidence remain
//! portable SQLite data. No credentials or message content belong in this store.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};
use uuid::Uuid;

/// The transfer engine is a policy decision, not an implementation detail.
/// Dovecot destinations should use the destination server's own dsync engine;
/// imapsync remains available for arbitrary IMAP destinations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Engine {
    #[default]
    Auto,
    Dovecot,
    ImapSync,
}

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Conservative default",
            Self::Dovecot => "Dovecot native",
            Self::ImapSync => "imapsync fallback",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Auto => {
                "Use imapsync as the conservative default; select Dovecot native explicitly when appropriate."
            }
            Self::Dovecot => {
                "Use destination-side doveadm/dsync when the destination is Dovecot and admin access is available."
            }
            Self::ImapSync => {
                "Use imapsync when both ends are arbitrary IMAP servers or no destination admin stack is available."
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Phase {
    Discovery,
    Preflight,
    Pilot,
    Seed,
    CatchUp,
    FinalDelta,
    Verification,
    Complete,
    Attention,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Preflight => "preflight",
            Self::Pilot => "pilot",
            Self::Seed => "seed",
            Self::CatchUp => "catch_up",
            Self::FinalDelta => "final_delta",
            Self::Verification => "verification",
            Self::Complete => "complete",
            Self::Attention => "attention",
        }
    }
    fn parse(value: &str) -> rusqlite::Result<Self> {
        match value {
            "discovery" => Ok(Self::Discovery),
            "preflight" => Ok(Self::Preflight),
            "pilot" => Ok(Self::Pilot),
            "seed" => Ok(Self::Seed),
            "catch_up" => Ok(Self::CatchUp),
            "final_delta" => Ok(Self::FinalDelta),
            "verification" => Ok(Self::Verification),
            "complete" => Ok(Self::Complete),
            "attention" => Ok(Self::Attention),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub source_endpoint: String,
    pub destination_endpoint: String,
    pub phase: Phase,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxJob {
    pub id: String,
    pub source_mailbox: String,
    pub destination_mailbox: String,
    pub state: String,
    /// Secret-free serialized configuration, if the importer supplied one.
    pub config: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub id: String,
    pub job_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub engine: String,
    /// Serialized execution plan captured when the run started. Session
    /// passwords and raw operator-supplied extra-option values are excluded;
    /// the application may retain a digest of those options for identity.
    pub plan_snapshot: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub detail: String,
}

/// Lightweight run row for activity views and health summaries. The full
/// immutable plan snapshot is intentionally excluded; callers that need the
/// historical execution plan can request the individual run by ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunListItem {
    pub id: String,
    pub job_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub engine: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub detail: String,
}

/// Immutable per-mailbox metadata supplied when a batch wave is admitted.
/// Secrets are intentionally not part of this structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchChildPlan {
    pub engine: String,
    pub plan_snapshot: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveProcess {
    pub run_id: String,
    pub job_id: String,
    pub pid: u32,
    pub start_ticks: Option<u64>,
    pub process_group: Option<u32>,
    pub session_id: Option<u32>,
    pub executable: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxEvidence {
    pub source_messages: u64,
    pub destination_messages: u64,
    pub source_bytes: u64,
    pub destination_bytes: u64,
    /// A literal unresolved-message count when the verifier provides one.
    /// The current imapsync summary adapter uses `1` as an unresolved-proof
    /// sentinel when its success line is absent; reports must therefore not
    /// describe that value as a literal message count for that engine.
    pub unmatched_messages: u64,
    pub failed_messages: u64,
    pub source_folders: u64,
    pub destination_folders: u64,
    /// True only when the engine supplied a stronger engine-confirmed summary.
    /// Aggregate mailbox totals must never be presented as message-level proof.
    pub authoritative: bool,
}

/// The strongest claim supported by the current verifier adapter. This is a
/// typed interpretation of the legacy durable `authoritative` bit; keeping
/// the interpretation here prevents reports and UI code from inventing
/// stronger meanings for aggregate totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceScope {
    EngineConfirmed,
    AggregateReconciled,
}

impl EvidenceScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::EngineConfirmed => "engine-confirmed",
            Self::AggregateReconciled => "aggregate-reconciled",
        }
    }
}

impl MailboxEvidence {
    pub fn evidence_scope(&self) -> EvidenceScope {
        if self.authoritative {
            EvidenceScope::EngineConfirmed
        } else {
            EvidenceScope::AggregateReconciled
        }
    }

    /// Human-readable evidence category for operators and exported reports.
    /// The percentage remains available for compatibility, but it is not a
    /// probability of correctness.
    pub fn evidence_level(&self) -> &'static str {
        if self.failed_messages > 0 || self.unmatched_messages > 0 {
            return "Incomplete evidence";
        }
        let exact = self.source_messages == self.destination_messages
            && self.source_bytes == self.destination_bytes
            && self.source_folders == self.destination_folders;
        if self.evidence_scope() == EvidenceScope::EngineConfirmed && exact {
            "Engine-confirmed exact match"
        } else if exact {
            "Aggregate match"
        } else {
            "Aggregate mismatch"
        }
    }

    pub fn confidence_percent(&self) -> u8 {
        let exact = self.source_messages == self.destination_messages
            && self.source_bytes == self.destination_bytes
            && self.source_folders == self.destination_folders;
        if !self.authoritative {
            return if exact && self.unmatched_messages == 0 && self.failed_messages == 0 {
                85
            } else {
                0
            };
        }
        if self.source_messages == 0
            && self.destination_messages == 0
            && self.source_folders == self.destination_folders
            && self.unmatched_messages == 0
            && self.failed_messages == 0
        {
            return 100;
        }
        let count_ok = self.source_messages == self.destination_messages;
        let bytes_ok = self.source_bytes == self.destination_bytes;
        let folders_ok = self.source_folders == self.destination_folders;
        if count_ok
            && bytes_ok
            && folders_ok
            && self.unmatched_messages == 0
            && self.failed_messages == 0
        {
            100
        } else if self.unmatched_messages == 0 && self.failed_messages == 0 {
            85
        } else {
            0
        }
    }

    /// Whether all available aggregate dimensions reconcile without reported
    /// failures. This is separate from the compatibility score: a score is
    /// not a probability of correctness.
    pub fn is_exact_match(&self) -> bool {
        self.source_messages == self.destination_messages
            && self.source_bytes == self.destination_bytes
            && self.source_folders == self.destination_folders
            && self.unmatched_messages == 0
            && self.failed_messages == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCapabilities {
    pub values: BTreeSet<String>,
}

impl ServerCapabilities {
    pub fn parse(response: &str) -> Self {
        let mut values = BTreeSet::new();
        for line in response
            .lines()
            .filter(|line| line.to_ascii_uppercase().contains("CAPABILITY"))
        {
            let mut after_marker = false;
            for token in line.split_whitespace() {
                if after_marker {
                    values.insert(
                        token
                            .trim_matches(|c: char| c == '\r' || c == '\n')
                            .to_ascii_uppercase(),
                    );
                }
                if token.eq_ignore_ascii_case("CAPABILITY") {
                    after_marker = true;
                }
            }
        }
        Self { values }
    }
    pub fn supports(&self, capability: &str) -> bool {
        self.values.contains(&capability.to_ascii_uppercase())
    }
    pub fn strategy(&self) -> Vec<&'static str> {
        let mut plan = vec!["UID-based initial scan"];
        if self.supports("QRESYNC") {
            plan.push("QRESYNC delta synchronization");
        } else if self.supports("CONDSTORE") {
            plan.push("CONDSTORE flag-change tracking");
        }
        if self.supports("SPECIAL-USE") {
            plan.push("SPECIAL-USE folder mapping");
        }
        if self.supports("UIDPLUS") {
            plan.push("UIDPLUS destination acknowledgement");
        }
        plan
    }
}

pub struct StateStore {
    connection: Connection,
}

/// Verbose subprocess output is diagnostic context, not the immutable audit
/// record. Keep a bounded per-project tail so multi-wave migrations cannot
/// grow the ledger without limit; lifecycle, run, evidence, and phase events
/// remain retained.
const MAX_DURABLE_RUN_OUTPUT_EVENTS: i64 = 10_000;

impl StateStore {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let path = path.as_ref();
        let store = Self {
            connection: Connection::open(path)?,
        };
        restrict_database_permissions(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        store.migrate()?;
        restrict_database_sidecars(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(store)
    }
    pub fn in_memory() -> rusqlite::Result<Self> {
        let store = Self {
            connection: Connection::open_in_memory()?,
        };
        store.migrate()?;
        Ok(store)
    }
    fn migrate(&self) -> rusqlite::Result<()> {
        const CURRENT_SCHEMA_VERSION: i64 = 1;
        let stored_schema_version: i64 =
            self.connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if stored_schema_version > CURRENT_SCHEMA_VERSION {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;
          CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, source_endpoint TEXT NOT NULL, destination_endpoint TEXT NOT NULL, phase TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
          CREATE TABLE IF NOT EXISTS mailbox_jobs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), source_mailbox TEXT NOT NULL, destination_mailbox TEXT NOT NULL, state TEXT NOT NULL, attempt INTEGER NOT NULL DEFAULT 0, checkpoint TEXT, preflight_plan TEXT, config TEXT);
          CREATE TABLE IF NOT EXISTS evidence (job_id TEXT PRIMARY KEY REFERENCES mailbox_jobs(id), source_messages INTEGER NOT NULL, destination_messages INTEGER NOT NULL, source_bytes INTEGER NOT NULL, destination_bytes INTEGER NOT NULL, unmatched_messages INTEGER NOT NULL, failed_messages INTEGER NOT NULL, source_folders INTEGER NOT NULL DEFAULT 0, destination_folders INTEGER NOT NULL DEFAULT 0, authoritative INTEGER NOT NULL DEFAULT 0, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
          CREATE TABLE IF NOT EXISTS evidence_history (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL, source_messages INTEGER NOT NULL, destination_messages INTEGER NOT NULL, source_bytes INTEGER NOT NULL, destination_bytes INTEGER NOT NULL, unmatched_messages INTEGER NOT NULL, failed_messages INTEGER NOT NULL, source_folders INTEGER NOT NULL, destination_folders INTEGER NOT NULL, authoritative INTEGER NOT NULL DEFAULT 0, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
          CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), job_id TEXT REFERENCES mailbox_jobs(id), parent_run_id TEXT REFERENCES runs(id), engine TEXT NOT NULL, plan_snapshot TEXT NOT NULL DEFAULT '', status TEXT NOT NULL, started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, finished_at TEXT, detail TEXT NOT NULL DEFAULT '');
          CREATE TABLE IF NOT EXISTS active_processes (run_id TEXT NOT NULL REFERENCES runs(id), job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), pid INTEGER NOT NULL, start_ticks INTEGER, process_group INTEGER, session_id INTEGER, executable TEXT NOT NULL DEFAULT '', started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(run_id, job_id));
          CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), run_id TEXT REFERENCES runs(id), kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
          CREATE INDEX IF NOT EXISTS idx_mailbox_jobs_project_state ON mailbox_jobs(project_id, state);
          CREATE INDEX IF NOT EXISTS idx_runs_project_started ON runs(project_id, started_at DESC);
          CREATE INDEX IF NOT EXISTS idx_runs_job_started ON runs(job_id, started_at DESC);
          CREATE INDEX IF NOT EXISTS idx_events_project_created ON events(project_id, created_at DESC);
          CREATE INDEX IF NOT EXISTS idx_evidence_history_job_captured ON evidence_history(job_id, captured_at DESC);
          CREATE INDEX IF NOT EXISTS idx_active_processes_pid ON active_processes(pid);
          CREATE UNIQUE INDEX IF NOT EXISTS one_running_run_per_job ON runs(job_id) WHERE job_id IS NOT NULL AND status='running';
          CREATE UNIQUE INDEX IF NOT EXISTS one_active_run_per_job ON runs(job_id) WHERE job_id IS NOT NULL AND status IN ('queued','running');")?;
        // Existing pre-0.1 databases need the new verification dimensions too.
        let columns = self
            .connection
            .prepare("PRAGMA table_info(evidence)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !columns.iter().any(|column| column == "source_folders") {
            self.connection.execute(
                "ALTER TABLE evidence ADD COLUMN source_folders INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !columns.iter().any(|column| column == "destination_folders") {
            self.connection.execute(
                "ALTER TABLE evidence ADD COLUMN destination_folders INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !columns.iter().any(|column| column == "authoritative") {
            self.connection.execute(
                "ALTER TABLE evidence ADD COLUMN authoritative INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        let job_columns = self
            .connection
            .prepare("PRAGMA table_info(mailbox_jobs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !job_columns.iter().any(|column| column == "preflight_plan") {
            self.connection.execute(
                "ALTER TABLE mailbox_jobs ADD COLUMN preflight_plan TEXT",
                [],
            )?;
        }
        if !job_columns.iter().any(|column| column == "config") {
            self.connection
                .execute("ALTER TABLE mailbox_jobs ADD COLUMN config TEXT", [])?;
        }
        let run_columns = self
            .connection
            .prepare("PRAGMA table_info(runs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !run_columns.iter().any(|column| column == "plan_snapshot") {
            self.connection.execute(
                "ALTER TABLE runs ADD COLUMN plan_snapshot TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        if !run_columns.iter().any(|column| column == "parent_run_id") {
            self.connection.execute(
                "ALTER TABLE runs ADD COLUMN parent_run_id TEXT REFERENCES runs(id)",
                [],
            )?;
        }
        let event_columns = self
            .connection
            .prepare("PRAGMA table_info(events)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !event_columns.iter().any(|column| column == "run_id") {
            self.connection.execute(
                "ALTER TABLE events ADD COLUMN run_id TEXT REFERENCES runs(id)",
                [],
            )?;
        }
        self.connection.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_run_created ON events(run_id, created_at DESC)",
            [],
        )?;
        self.connection
            .pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;
        let process_columns = self
            .connection
            .prepare("PRAGMA table_info(active_processes)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !process_columns.iter().any(|column| column == "start_ticks") {
            self.connection.execute(
                "ALTER TABLE active_processes ADD COLUMN start_ticks INTEGER",
                [],
            )?;
        }
        if !process_columns
            .iter()
            .any(|column| column == "process_group")
        {
            self.connection.execute(
                "ALTER TABLE active_processes ADD COLUMN process_group INTEGER",
                [],
            )?;
        }
        if !process_columns.iter().any(|column| column == "session_id") {
            self.connection.execute(
                "ALTER TABLE active_processes ADD COLUMN session_id INTEGER",
                [],
            )?;
        }
        if !process_columns.iter().any(|column| column == "executable") {
            self.connection.execute(
                "ALTER TABLE active_processes ADD COLUMN executable TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        let history_columns = self
            .connection
            .prepare("PRAGMA table_info(evidence_history)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !history_columns
            .iter()
            .any(|column| column == "authoritative")
        {
            self.connection.execute(
                "ALTER TABLE evidence_history ADD COLUMN authoritative INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        Ok(())
    }
    pub fn create_project(
        &self,
        name: &str,
        source: &str,
        destination: &str,
    ) -> rusqlite::Result<Project> {
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            source_endpoint: source.into(),
            destination_endpoint: destination.into(),
            phase: Phase::Discovery,
        };
        self.connection.execute("INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)", params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()])?;
        self.event(
            &project.id,
            "project_created",
            "Project created without credentials",
        )?;
        Ok(project)
    }
    pub fn create_project_with_mailbox(
        &self,
        name: &str,
        source: &str,
        destination: &str,
        source_mailbox: &str,
        destination_mailbox: &str,
    ) -> rusqlite::Result<(Project, String)> {
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            source_endpoint: source.into(),
            destination_endpoint: destination.into(),
            phase: Phase::Discovery,
        };
        let job_id = Uuid::new_v4().to_string();
        let tx = self.connection.unchecked_transaction()?;
        tx.execute("INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)", params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()])?;
        tx.execute("INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,state) VALUES(?1,?2,?3,?4,'queued')", params![job_id, project.id, source_mailbox, destination_mailbox])?;
        tx.execute("INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_created','Project created without credentials')", [&project.id])?;
        tx.commit()?;
        Ok((project, job_id))
    }
    pub fn create_project_with_mailboxes(
        &self,
        name: &str,
        source: &str,
        destination: &str,
        mailboxes: &[(String, String)],
    ) -> rusqlite::Result<(Project, Vec<String>)> {
        if mailboxes.is_empty() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            source_endpoint: source.into(),
            destination_endpoint: destination.into(),
            phase: Phase::Discovery,
        };
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)",
            params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()],
        )?;
        let mut ids = Vec::with_capacity(mailboxes.len());
        for (source_mailbox, destination_mailbox) in mailboxes {
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,state) VALUES(?1,?2,?3,?4,'queued')",
                params![id, project.id, source_mailbox, destination_mailbox],
            )?;
            ids.push(id);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_created',?2)",
            params![
                project.id,
                format!("Batch created with {} mailbox jobs", ids.len())
            ],
        )?;
        tx.commit()?;
        Ok((project, ids))
    }
    pub fn create_project_with_mailbox_configs(
        &self,
        name: &str,
        source: &str,
        destination: &str,
        mailboxes: &[(String, String, String)],
    ) -> rusqlite::Result<(Project, Vec<String>)> {
        if mailboxes.is_empty() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            source_endpoint: source.into(),
            destination_endpoint: destination.into(),
            phase: Phase::Discovery,
        };
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)",
            params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()],
        )?;
        let mut ids = Vec::with_capacity(mailboxes.len());
        for (source_mailbox, destination_mailbox, config) in mailboxes {
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,state,config) VALUES(?1,?2,?3,?4,'queued',?5)",
                params![id, project.id, source_mailbox, destination_mailbox, config],
            )?;
            ids.push(id);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_created',?2)",
            params![
                project.id,
                format!("Batch created with {} mailbox jobs", ids.len())
            ],
        )?;
        tx.commit()?;
        Ok((project, ids))
    }
    pub fn transition(&self, id: &str, phase: Phase) -> rusqlite::Result<()> {
        let current = self
            .project(id)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?
            .phase;
        let backwards_transition = current != Phase::Attention
            && phase != Phase::Attention
            && phase_rank(phase) < phase_rank(current);
        if current != phase
            && (current == Phase::Complete
                || (current == Phase::Attention
                    && !matches!(phase, Phase::Preflight | Phase::Verification))
                || backwards_transition)
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if phase == Phase::Verification && current == Phase::Discovery {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if phase == Phase::Complete && current != phase {
            let total: i64 = self.connection.query_row(
                "SELECT COUNT(*) FROM mailbox_jobs WHERE project_id=?1",
                [id],
                |row| row.get(0),
            )?;
            let verified: i64 = self.connection.query_row(
                "SELECT COUNT(*) FROM mailbox_jobs WHERE project_id=?1 AND state='verified'",
                [id],
                |row| row.get(0),
            )?;
            if total == 0 || total != verified || current != Phase::Verification {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "UPDATE projects SET phase=?1 WHERE id=?2",
            params![phase.as_str(), id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'phase_changed',?2)",
            params![id, phase.as_str()],
        )?;
        tx.commit()
    }
    pub fn add_mailbox(
        &self,
        project_id: &str,
        source: &str,
        destination: &str,
    ) -> rusqlite::Result<String> {
        let id = Uuid::new_v4().to_string();
        self.connection.execute("INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,state) VALUES(?1,?2,?3,?4,'queued')", params![id, project_id, source, destination])?;
        Ok(id)
    }
    pub fn set_mailbox_state(&self, job_id: &str, state: &str) -> rusqlite::Result<()> {
        if !matches!(
            state,
            "queued"
                | "preflight"
                | "ready"
                | "running"
                | "delta_required"
                | "verification_difference"
                | "completed"
                | "failed"
                | "cancelled"
                | "attention"
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let current: String = self.connection.query_row(
            "SELECT state FROM mailbox_jobs WHERE id=?1",
            [job_id],
            |row| row.get(0),
        )?;
        // Verification is owned by the evidence terminal methods below. A
        // generic state setter must not reuse evidence from an older run to
        // manufacture a verified result for a newer one.
        if state == "verified" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if current != state && !valid_mailbox_transition(&current, state) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = self.connection.execute(
            "UPDATE mailbox_jobs SET state=?, attempt=CASE WHEN ?='running' AND state<>'running' THEN attempt+1 ELSE attempt END WHERE id=?",
            params![state, state, job_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }
    pub fn mailbox_state(&self, job_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT state FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| row.get(0),
            )
            .optional()
    }
    /// Persist the opaque digest of a preflighted plan. Callers should pass a
    /// canonical plan hash rather than generated command arguments; the
    /// stored value is used only for exact equality during live admission.
    pub fn set_preflight_plan(&self, job_id: &str, plan: &str) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE mailbox_jobs SET preflight_plan=?1 WHERE id=?2",
            params![plan, job_id],
        )?;
        Ok(())
    }
    pub fn preflight_plan(&self, job_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT preflight_plan FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| row.get(0),
            )
            .optional()
    }
    /// A desktop restart cannot prove that a previous child process still
    /// exists. All runs belonging to an interrupted wave are therefore made
    /// terminal: claimed children become abandoned and unclaimed children
    /// are also abandoned so their queued rows cannot wedge future retries.
    /// Only mailboxes that were actually claimed are moved to `attention`.
    pub fn recover_abandoned_jobs(&self) -> rusqlite::Result<usize> {
        let projects = self
            .connection
            .prepare(
                "SELECT DISTINCT project_id FROM runs WHERE status='running' OR (status='queued' AND parent_run_id IN (SELECT id FROM runs WHERE status='running' AND job_id IS NULL))",
            )?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let tx = self.connection.unchecked_transaction()?;
        let count = tx.execute(
            "UPDATE mailbox_jobs SET state='attention' WHERE state='running'",
            [],
        )?;
        tx.execute(
            "UPDATE runs SET status='abandoned',finished_at=CURRENT_TIMESTAMP,detail='Application restarted before completion' WHERE status='running'",
            [],
        )?;
        tx.execute(
            "UPDATE runs SET status='abandoned',finished_at=CURRENT_TIMESTAMP,detail='Application restarted before batch child was claimed' WHERE status='queued' AND parent_run_id IN (SELECT id FROM runs WHERE status='abandoned' AND job_id IS NULL AND detail='Application restarted before completion')",
            [],
        )?;
        tx.execute("DELETE FROM active_processes", [])?;
        for project in projects {
            tx.execute(
                "INSERT INTO events(project_id,kind,detail) VALUES(?1,'run_recovered','Interrupted batch runs and claimed mailbox jobs were recovered; unclaimed child runs were abandoned')",
                [project],
            )?;
        }
        tx.commit()?;
        Ok(count)
    }

    /// Record the OS process belonging to a durable run. Startup reconciliation
    /// uses this identity to terminate a recorded orphan before allowing an
    /// operator to retry the mailbox.
    pub fn register_process(&self, process: &ActiveProcess) -> rusqlite::Result<()> {
        // Validate ownership and insert the identity in one transaction. A
        // worker can arrive late while the UI is finishing a run; a
        // separate SELECT followed by INSERT would let that stale worker
        // recreate an active-process row after terminal cleanup.
        let tx = self.connection.unchecked_transaction()?;
        let consistent: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs r JOIN mailbox_jobs j ON j.id=?2 AND r.project_id=j.project_id WHERE r.id=?1 AND r.status='running' AND r.job_id=j.id)",
            params![process.run_id, process.job_id],
            |row| row.get(0),
        )?;
        if !consistent {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "INSERT INTO active_processes(run_id,job_id,pid,start_ticks,process_group,session_id,executable) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(run_id,job_id) DO UPDATE SET pid=excluded.pid,start_ticks=excluded.start_ticks,process_group=excluded.process_group,session_id=excluded.session_id,executable=excluded.executable,started_at=CURRENT_TIMESTAMP",
            params![
                process.run_id,
                process.job_id,
                i64::from(process.pid),
                process.start_ticks.map(|value| value as i64),
                process.process_group.map(i64::from),
                process.session_id.map(i64::from),
                process.executable
            ],
        )?;
        tx.commit()
    }

    pub fn active_processes(&self) -> rusqlite::Result<Vec<ActiveProcess>> {
        self.connection
            .prepare("SELECT run_id,job_id,pid,start_ticks,process_group,session_id,executable FROM active_processes")?
            .query_map([], |row| {
                let pid: i64 = row.get(2)?;
                let pid = u32::try_from(pid)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, pid))?;
                let start_ticks: Option<i64> = row.get(3)?;
                let process_group: Option<i64> = row.get(4)?;
                let session_id: Option<i64> = row.get(5)?;
                Ok(ActiveProcess {
                    run_id: row.get(0)?,
                    job_id: row.get(1)?,
                    pid,
                    start_ticks: start_ticks.and_then(|value| u64::try_from(value).ok()),
                    process_group: process_group.and_then(|value| u32::try_from(value).ok()),
                    session_id: session_id.and_then(|value| u32::try_from(value).ok()),
                    executable: row.get(6)?,
                })
            })?
            .collect()
    }

    pub fn clear_processes(&self, run_id: &str) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM active_processes WHERE run_id=?1", [run_id])?;
        Ok(())
    }
    pub fn record_event(&self, project_id: &str, kind: &str, detail: &str) -> rusqlite::Result<()> {
        self.event(project_id, kind, detail)
    }
    pub fn record_events(
        &self,
        project_id: &str,
        kind: &str,
        details: &[String],
    ) -> rusqlite::Result<()> {
        let batch = details
            .iter()
            .map(|detail| (project_id, kind, detail.as_str()))
            .collect::<Vec<_>>();
        self.record_events_batch(&batch)
    }
    pub fn record_events_batch(&self, events: &[(&str, &str, &str)]) -> rusqlite::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let tx = self.connection.unchecked_transaction()?;
        {
            let mut statement =
                tx.prepare_cached("INSERT INTO events(project_id,kind,detail) VALUES(?1,?2,?3)")?;
            for (project_id, kind, detail) in events {
                statement.execute(params![project_id, kind, detail])?;
            }
        }
        let projects = events
            .iter()
            .map(|(project_id, _, _)| *project_id)
            .collect::<BTreeSet<_>>();
        for project_id in projects {
            tx.execute(
                "DELETE FROM events WHERE project_id=?1 AND kind='run_output' AND id NOT IN (SELECT id FROM events WHERE project_id=?1 AND kind='run_output' ORDER BY id DESC LIMIT ?2)",
                params![project_id, MAX_DURABLE_RUN_OUTPUT_EVENTS],
            )?;
        }
        tx.commit()
    }

    /// Record diagnostic or verification events against the run that emitted
    /// them. The run supplies the project identity inside the same
    /// transaction, so callers cannot accidentally cross-wire a project and
    /// run while persisting asynchronous output.
    pub fn record_run_events_batch(
        &self,
        run_id: &str,
        events: &[(&str, &str)],
    ) -> rusqlite::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let tx = self.connection.unchecked_transaction()?;
        let project_id: String =
            tx.query_row("SELECT project_id FROM runs WHERE id=?1", [run_id], |row| {
                row.get(0)
            })?;
        {
            let mut statement = tx.prepare_cached(
                "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,?3,?4)",
            )?;
            for (kind, detail) in events {
                statement.execute(params![project_id, run_id, kind, detail])?;
            }
        }
        tx.execute(
            "DELETE FROM events WHERE project_id=?1 AND kind='run_output' AND id NOT IN (SELECT id FROM events WHERE project_id=?1 AND kind='run_output' ORDER BY id DESC LIMIT ?2)",
            params![project_id, MAX_DURABLE_RUN_OUTPUT_EVENTS],
        )?;
        tx.commit()
    }
    #[cfg(test)]
    fn insert_run_for_test(
        &self,
        project_id: &str,
        job_id: Option<&str>,
        run_id: &str,
        engine: &str,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO runs(id,project_id,job_id,engine,plan_snapshot,status) VALUES(?1,?2,?3,?4,'','running')",
            params![run_id, project_id, job_id, engine],
        )?;
        Ok(())
    }
    /// Atomically records a run and moves its mailbox into `running`.
    /// Keeping these writes together prevents restart recovery from seeing a
    /// running mailbox without the run record needed to explain it.
    pub fn begin_run(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        engine: &str,
    ) -> rusqlite::Result<()> {
        self.begin_run_with_snapshot(project_id, job_id, run_id, engine, "")
    }
    /// Atomically starts a mailbox run and persists its immutable, secret-free
    /// execution-plan snapshot (with session passwords excluded).
    pub fn begin_run_with_snapshot(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        engine: &str,
        plan_snapshot: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let current: String = tx.query_row(
            "SELECT state FROM mailbox_jobs WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
            |row| row.get(0),
        )?;
        // A mailbox may never have two active runs. Recovery must first move
        // the previous run to an operator-review state before retrying it.
        if current == "running" || !valid_mailbox_transition(&current, "running") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let active_run_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=?1 AND job_id=?2 AND status IN ('queued','running'))",
            params![project_id, job_id],
            |row| row.get(0),
        )?;
        if active_run_exists {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "INSERT INTO runs(id,project_id,job_id,engine,plan_snapshot,status) VALUES(?1,?2,?3,?4,?5,'running')",
            params![run_id, project_id, job_id, engine, plan_snapshot],
        )?;
        tx.execute(
            "UPDATE mailbox_jobs SET state='running',attempt=CASE WHEN state<>'running' THEN attempt+1 ELSE attempt END WHERE id=?1",
            [job_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_started',?3)",
            params![project_id, run_id, format!("{engine} ({run_id})")],
        )?;
        tx.commit()
    }
    /// Start a batch as one durable boundary. Child mailboxes remain queued
    /// until an individual worker claims them, so durable state reflects work
    /// that has actually reached the execution layer.
    pub fn begin_batch_run(
        &self,
        project_id: &str,
        job_ids: &[String],
        run_id: &str,
        engine: &str,
        expected_plans: &[String],
    ) -> rusqlite::Result<()> {
        self.begin_batch_run_with_snapshot(project_id, job_ids, run_id, engine, expected_plans, "")
    }
    /// Atomically starts a batch and stores its immutable, secret-free plan
    /// snapshot alongside the parent run (with session passwords excluded).
    pub fn begin_batch_run_with_snapshot(
        &self,
        project_id: &str,
        job_ids: &[String],
        run_id: &str,
        engine: &str,
        expected_plans: &[String],
        plan_snapshot: &str,
    ) -> rusqlite::Result<()> {
        self.begin_batch_run_with_children(
            project_id,
            job_ids,
            run_id,
            engine,
            expected_plans,
            plan_snapshot,
            &[],
        )
        .map(|_| ())
    }

    /// Atomically starts a batch parent and one durable child run per
    /// mailbox. Child runs and mailbox rows remain queued until workers claim
    /// them, so durable state reflects actual worker ownership.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_batch_run_with_children(
        &self,
        project_id: &str,
        job_ids: &[String],
        run_id: &str,
        engine: &str,
        expected_plans: &[String],
        plan_snapshot: &str,
        child_plans: &[BatchChildPlan],
    ) -> rusqlite::Result<Vec<String>> {
        if job_ids.is_empty()
            || (!expected_plans.is_empty() && expected_plans.len() != job_ids.len())
            || (!child_plans.is_empty() && child_plans.len() != job_ids.len())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        for (index, job_id) in job_ids.iter().enumerate() {
            let current: String = tx.query_row(
                "SELECT state FROM mailbox_jobs WHERE id=?1 AND project_id=?2",
                params![job_id, project_id],
                |row| row.get(0),
            )?;
            // Reject an already-running child rather than treating it as a
            // harmless retry. This keeps one durable execution owner per
            // mailbox even when two callers race.
            if current == "running" || !valid_mailbox_transition(&current, "running") {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let active_run_exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=?1 AND job_id=?2 AND status IN ('queued','running'))",
                params![project_id, job_id],
                |row| row.get(0),
            )?;
            if active_run_exists {
                return Err(rusqlite::Error::InvalidQuery);
            }
            if let Some(expected_plan) = expected_plans.get(index) {
                let actual: Option<String> = tx.query_row(
                    "SELECT preflight_plan FROM mailbox_jobs WHERE id=?1 AND project_id=?2",
                    params![job_id, project_id],
                    |row| row.get(0),
                )?;
                if actual.as_deref() != Some(expected_plan.as_str()) {
                    return Err(rusqlite::Error::InvalidQuery);
                }
            }
        }
        tx.execute(
            "INSERT INTO runs(id,project_id,job_id,engine,plan_snapshot,status) VALUES(?1,?2,NULL,?3,?4,'running')",
            params![run_id, project_id, engine, plan_snapshot],
        )?;
        let mut child_run_ids = Vec::with_capacity(job_ids.len());
        for (index, job_id) in job_ids.iter().enumerate() {
            let child_run_id = Uuid::new_v4().to_string();
            let child_plan = child_plans.get(index);
            let child_engine = child_plan
                .map(|plan| plan.engine.as_str())
                .unwrap_or(engine);
            let child_snapshot = child_plan
                .map(|plan| plan.plan_snapshot.as_str())
                .unwrap_or("");
            tx.execute(
                "INSERT INTO runs(id,project_id,job_id,parent_run_id,engine,plan_snapshot,status) VALUES(?1,?2,?3,?4,?5,?6,'queued')",
                params![
                    child_run_id,
                    project_id,
                    job_id,
                    run_id,
                    child_engine,
                    child_snapshot
                ],
            )?;
            child_run_ids.push(child_run_id);
        }
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_started',?3)",
            params![
                project_id,
                run_id,
                format!("{engine} ({run_id}); {} child jobs", job_ids.len())
            ],
        )?;
        tx.commit().map(|()| child_run_ids)
    }
    /// Atomically claims one child of a running parent batch. The operation
    /// is idempotent for a child already claimed by that same batch because
    /// retry/status events may be observed more than once by the UI.
    pub fn claim_batch_mailbox(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let parent_is_running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND project_id=?2 AND job_id IS NULL AND status='running')",
            params![run_id, project_id],
            |row| row.get(0),
        )?;
        if !parent_is_running {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let current: String = tx.query_row(
            "SELECT state FROM mailbox_jobs WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
            |row| row.get(0),
        )?;
        if current == "running" {
            return tx.commit();
        }
        if !valid_mailbox_transition(&current, "running") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "UPDATE mailbox_jobs SET state='running',attempt=attempt+1 WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'mailbox_claimed',?3)",
            params![
                project_id,
                run_id,
                format!("{job_id} claimed by batch run {run_id}")
            ],
        )?;
        tx.commit()
    }
    /// Claim a mailbox for its durable child run. The parent run proves that
    /// this belongs to the active batch, while the child relation prevents a
    /// worker from attaching a process to another row in the same project.
    pub fn claim_batch_mailbox_for_child(
        &self,
        project_id: &str,
        job_id: &str,
        parent_run_id: &str,
        child_run_id: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let parent_is_running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND project_id=?2 AND job_id IS NULL AND status='running')",
            params![parent_run_id, project_id],
            |row| row.get(0),
        )?;
        let child_status: String = tx.query_row(
            "SELECT status FROM runs WHERE id=?1 AND project_id=?2 AND job_id=?3 AND parent_run_id=?4",
            params![child_run_id, project_id, job_id, parent_run_id],
            |row| row.get(0),
        )?;
        if !parent_is_running || !matches!(child_status.as_str(), "queued" | "running") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let current: String = tx.query_row(
            "SELECT state FROM mailbox_jobs WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
            |row| row.get(0),
        )?;
        if current == "running" {
            if child_status != "running" {
                return Err(rusqlite::Error::InvalidQuery);
            }
            return tx.commit();
        }
        if !valid_mailbox_transition(&current, "running") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "UPDATE mailbox_jobs SET state='running',attempt=attempt+1 WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
        )?;
        tx.execute(
            "UPDATE runs SET status='running' WHERE id=?1 AND project_id=?2 AND job_id=?3 AND parent_run_id=?4 AND status='queued'",
            params![child_run_id, project_id, job_id, parent_run_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'mailbox_claimed',?3)",
            params![
                project_id,
                child_run_id,
                format!("{job_id} claimed by child run {child_run_id}")
            ],
        )?;
        tx.commit()
    }
    pub fn finish_run(&self, run_id: &str, status: &str, detail: &str) -> rusqlite::Result<()> {
        if !matches!(
            status,
            "completed" | "failed" | "cancelled" | "abandoned" | "verification_failed"
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM runs WHERE id=?1 AND status='running'",
            [run_id],
            |row| row.get(0),
        )?;
        // A parent batch is not complete while any child is still queued or
        // running.  Keep this invariant in the store so a controller bug or
        // partial event stream cannot produce a deceptively terminal batch.
        let has_unfinished_children: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE parent_run_id=?1 AND status IN ('queued','running'))",
            [run_id],
            |row| row.get(0),
        )?;
        if has_unfinished_children {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let has_unsuccessful_children: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE parent_run_id=?1 AND status<>'completed')",
            [run_id],
            |row| row.get(0),
        )?;
        if status == "completed" && has_unsuccessful_children {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = tx.execute(
            "UPDATE runs SET status=?1,finished_at=CURRENT_TIMESTAMP,detail=?2 WHERE id=?3 AND status='running'",
            params![status, detail, run_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute("DELETE FROM active_processes WHERE run_id=?1", [run_id])?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_finished',?3)",
            params![project_id, run_id, detail],
        )?;
        tx.commit()
    }
    /// Atomically completes a single-mailbox run and records the durable
    /// mailbox state.  Completion is deliberately one transaction: a run
    /// must never be marked finished while its mailbox remains `running` (or
    /// vice versa) after a database failure or process interruption.
    pub fn finish_run_for_mailbox(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
    ) -> rusqlite::Result<()> {
        if !matches!(
            run_status,
            "completed" | "failed" | "cancelled" | "verification_failed"
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if !matches!(
            mailbox_state,
            "ready"
                | "completed"
                | "verified"
                | "delta_required"
                | "verification_difference"
                | "failed"
                | "cancelled"
                | "attention"
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let current: String = tx.query_row(
            "SELECT state FROM mailbox_jobs WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
            |row| row.get(0),
        )?;
        // Verification must be committed together with the evidence that
        // proves this run.  Merely finding an older evidence row is not
        // sufficient: otherwise a later run could reuse stale evidence and
        // manufacture `verified` through this generic terminal path.
        if mailbox_state == "verified" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if current != mailbox_state && !valid_mailbox_transition(&current, mailbox_state) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let run_changed = tx.execute(
            "UPDATE runs SET status=?1,finished_at=CURRENT_TIMESTAMP,detail=?2 WHERE id=?3 AND project_id=?4 AND job_id=?5 AND (status='running' OR (status='queued' AND ?1 IN ('cancelled','failed','verification_failed'))) ",
            params![run_status, detail, run_id, project_id, job_id],
        )?;
        if run_changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute(
            "UPDATE mailbox_jobs SET state=?1 WHERE id=?2 AND project_id=?3",
            params![mailbox_state, job_id, project_id],
        )?;
        tx.execute("DELETE FROM active_processes WHERE run_id=?1", [run_id])?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_finished',?3)",
            params![
                project_id,
                run_id,
                if run_status == "completed" {
                    "success"
                } else {
                    "failure"
                }
            ],
        )?;
        tx.commit()
    }
    /// Atomically records terminal verification evidence and completes the
    /// active run. A verified result is allowed to move directly from
    /// `running` here because the evidence and terminal transition share one
    /// transaction; ordinary mailbox-state updates still reject that jump.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_run_for_mailbox_with_evidence(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
        value: &MailboxEvidence,
    ) -> rusqlite::Result<()> {
        if run_status != "completed"
            || !matches!(
                mailbox_state,
                "verified" | "delta_required" | "verification_difference"
            )
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // `verified` is a claim about the evidence, not merely a requested
        // mailbox state. Keep this invariant in the store so a future caller
        // cannot accidentally promote mismatching aggregate evidence by
        // bypassing the current UI decision logic.
        if mailbox_state == "verified" && !value.is_exact_match() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let current: String = tx.query_row(
            "SELECT state FROM mailbox_jobs WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
            |row| row.get(0),
        )?;
        let evidence_terminal_jump = current == "running"
            && matches!(
                mailbox_state,
                "verified" | "delta_required" | "verification_difference"
            );
        if current != mailbox_state
            && !evidence_terminal_jump
            && !valid_mailbox_transition(&current, mailbox_state)
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let run_changed = tx.execute(
            "UPDATE runs SET status='completed',finished_at=CURRENT_TIMESTAMP,detail=?1 WHERE id=?2 AND project_id=?3 AND job_id=?4 AND status='running'",
            params![detail, run_id, project_id, job_id],
        )?;
        if run_changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute("INSERT INTO evidence_history(job_id,run_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", params![job_id, run_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages, value.source_folders, value.destination_folders, value.authoritative])?;
        tx.execute("INSERT INTO evidence(job_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(job_id) DO UPDATE SET source_messages=excluded.source_messages,destination_messages=excluded.destination_messages,source_bytes=excluded.source_bytes,destination_bytes=excluded.destination_bytes,unmatched_messages=excluded.unmatched_messages,failed_messages=excluded.failed_messages,source_folders=excluded.source_folders,destination_folders=excluded.destination_folders,authoritative=excluded.authoritative,captured_at=CURRENT_TIMESTAMP", params![job_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages, value.source_folders, value.destination_folders, value.authoritative])?;
        tx.execute(
            "UPDATE mailbox_jobs SET state=?1 WHERE id=?2 AND project_id=?3",
            params![mailbox_state, job_id, project_id],
        )?;
        tx.execute("DELETE FROM active_processes WHERE run_id=?1", [run_id])?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_finished',?3)",
            params![project_id, run_id, "success with verification evidence"],
        )?;
        tx.commit()
    }
    pub fn run_status(&self, run_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row("SELECT status FROM runs WHERE id=?1", [run_id], |row| {
                row.get(0)
            })
            .optional()
    }
    pub fn latest_run(&self, job_id: &str) -> rusqlite::Result<Option<RunSummary>> {
        self.connection
            .query_row(
                "SELECT id,job_id,parent_run_id,engine,plan_snapshot,status,started_at,finished_at,detail FROM runs WHERE job_id=?1 ORDER BY started_at DESC, rowid DESC LIMIT 1",
                [job_id],
                |row| {
                    Ok(RunSummary {
                        id: row.get(0)?,
                        job_id: row.get(1)?,
                        parent_run_id: row.get(2)?,
                        engine: row.get(3)?,
                        plan_snapshot: row.get(4)?,
                        status: row.get(5)?,
                        started_at: row.get(6)?,
                        finished_at: row.get(7)?,
                        detail: row.get(8)?,
                    })
                },
            )
            .optional()
    }
    pub fn run(&self, run_id: &str) -> rusqlite::Result<Option<RunSummary>> {
        self.connection
            .query_row(
                "SELECT id,job_id,parent_run_id,engine,plan_snapshot,status,started_at,finished_at,detail FROM runs WHERE id=?1",
                [run_id],
                |row| {
                    Ok(RunSummary {
                        id: row.get(0)?,
                        job_id: row.get(1)?,
                        parent_run_id: row.get(2)?,
                        engine: row.get(3)?,
                        plan_snapshot: row.get(4)?,
                        status: row.get(5)?,
                        started_at: row.get(6)?,
                        finished_at: row.get(7)?,
                        detail: row.get(8)?,
                    })
                },
            )
            .optional()
    }
    pub fn recent_runs(&self, project_id: &str, limit: u32) -> rusqlite::Result<Vec<RunSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT id,job_id,parent_run_id,engine,plan_snapshot,status,started_at,finished_at,detail FROM runs WHERE project_id=?1 ORDER BY started_at DESC, rowid DESC LIMIT ?2",
        )?;
        statement
            .query_map(params![project_id, limit], |row| {
                Ok(RunSummary {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    parent_run_id: row.get(2)?,
                    engine: row.get(3)?,
                    plan_snapshot: row.get(4)?,
                    status: row.get(5)?,
                    started_at: row.get(6)?,
                    finished_at: row.get(7)?,
                    detail: row.get(8)?,
                })
            })?
            .collect()
    }

    pub fn recent_run_list(
        &self,
        project_id: &str,
        limit: u32,
    ) -> rusqlite::Result<Vec<RunListItem>> {
        let mut statement = self.connection.prepare(
            "SELECT id,job_id,parent_run_id,engine,status,started_at,finished_at,detail FROM runs WHERE project_id=?1 ORDER BY started_at DESC, rowid DESC LIMIT ?2",
        )?;
        statement
            .query_map(params![project_id, limit], |row| {
                Ok(RunListItem {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    parent_run_id: row.get(2)?,
                    engine: row.get(3)?,
                    status: row.get(4)?,
                    started_at: row.get(5)?,
                    finished_at: row.get(6)?,
                    detail: row.get(7)?,
                })
            })?
            .collect()
    }
    // Legacy evidence insertion is retained only as a fixture helper for
    // historical-state tests. Production callers must use the run-owned
    // terminal methods above, which atomically bind evidence to the run and
    // mailbox state transition.
    #[cfg(test)]
    fn record_evidence(&self, job_id: &str, value: &MailboxEvidence) -> rusqlite::Result<()> {
        self.record_evidence_for_run(job_id, "legacy", value)
    }
    pub fn record_evidence_for_run(
        &self,
        job_id: &str,
        run_id: &str,
        value: &MailboxEvidence,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        if run_id != "legacy" {
            let owns_mailbox: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM runs r JOIN mailbox_jobs j ON j.id=r.job_id AND j.project_id=r.project_id WHERE r.id=?1 AND j.id=?2)",
                params![run_id, job_id],
                |row| row.get(0),
            )?;
            if !owns_mailbox {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        tx.execute("INSERT INTO evidence_history(job_id,run_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", params![job_id, run_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages, value.source_folders, value.destination_folders, value.authoritative])?;
        tx.execute("INSERT INTO evidence(job_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(job_id) DO UPDATE SET source_messages=excluded.source_messages,destination_messages=excluded.destination_messages,source_bytes=excluded.source_bytes,destination_bytes=excluded.destination_bytes,unmatched_messages=excluded.unmatched_messages,failed_messages=excluded.failed_messages,source_folders=excluded.source_folders,destination_folders=excluded.destination_folders,authoritative=excluded.authoritative,captured_at=CURRENT_TIMESTAMP", params![job_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages, value.source_folders, value.destination_folders, value.authoritative])?;
        tx.commit()?;
        Ok(())
    }
    pub fn evidence(&self, job_id: &str) -> rusqlite::Result<Option<MailboxEvidence>> {
        self.connection.query_row("SELECT source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative FROM evidence WHERE job_id=?1", [job_id], |r| Ok(MailboxEvidence { source_messages:r.get(0)?, destination_messages:r.get(1)?, source_bytes:r.get(2)?, destination_bytes:r.get(3)?, unmatched_messages:r.get(4)?, failed_messages:r.get(5)?, source_folders:r.get(6)?, destination_folders:r.get(7)?, authoritative:r.get::<_, i64>(8)? != 0 })).optional()
    }
    pub fn latest_evidence_for_run(
        &self,
        job_id: &str,
    ) -> rusqlite::Result<Option<(String, MailboxEvidence)>> {
        self.connection
            .query_row(
                "SELECT run_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative FROM evidence_history WHERE job_id=?1 ORDER BY captured_at DESC, id DESC LIMIT 1",
                [job_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        MailboxEvidence {
                            source_messages: row.get(1)?,
                            destination_messages: row.get(2)?,
                            source_bytes: row.get(3)?,
                            destination_bytes: row.get(4)?,
                            unmatched_messages: row.get(5)?,
                            failed_messages: row.get(6)?,
                            source_folders: row.get(7)?,
                            destination_folders: row.get(8)?,
                            authoritative: row.get::<_, i64>(9)? != 0,
                        },
                    ))
                },
            )
            .optional()
    }
    pub fn project(&self, id: &str) -> rusqlite::Result<Option<Project>> {
        self.connection.query_row("SELECT id,name,source_endpoint,destination_endpoint,phase FROM projects WHERE id=?1", [id], |r| Ok(Project { id:r.get(0)?, name:r.get(1)?, source_endpoint:r.get(2)?, destination_endpoint:r.get(3)?, phase: Phase::parse(&r.get::<_,String>(4)?)? })).optional()
    }
    pub fn latest_project(&self) -> rusqlite::Result<Option<Project>> {
        self.connection.query_row("SELECT id,name,source_endpoint,destination_endpoint,phase FROM projects ORDER BY created_at DESC, rowid DESC LIMIT 1", [], |r| Ok(Project { id:r.get(0)?, name:r.get(1)?, source_endpoint:r.get(2)?, destination_endpoint:r.get(3)?, phase: Phase::parse(&r.get::<_,String>(4)?)? })).optional()
    }
    pub fn first_mailbox(&self, project_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT id FROM mailbox_jobs WHERE project_id=?1 LIMIT 1",
                [project_id],
                |r| r.get(0),
            )
            .optional()
    }
    pub fn mailbox_identity(
        &self,
        job_id: &str,
    ) -> rusqlite::Result<Option<(String, String, String)>> {
        self.connection
            .query_row(
                "SELECT source_mailbox,destination_mailbox,state FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
    }
    pub fn project_id_for_mailbox(&self, job_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT project_id FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| row.get(0),
            )
            .optional()
    }
    pub fn mailboxes(&self, project_id: &str) -> rusqlite::Result<Vec<MailboxJob>> {
        let mut statement = self.connection.prepare(
            "SELECT id,source_mailbox,destination_mailbox,state,config FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid",
        )?;
        statement
            .query_map([project_id], |row| {
                Ok(MailboxJob {
                    id: row.get(0)?,
                    source_mailbox: row.get(1)?,
                    destination_mailbox: row.get(2)?,
                    state: row.get(3)?,
                    config: row.get(4)?,
                })
            })?
            .collect()
    }
    pub fn all_mailboxes_verified(&self, project_id: &str) -> rusqlite::Result<bool> {
        let (total, verified): (i64, i64) = self.connection.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN state='verified' THEN 1 ELSE 0 END) FROM mailbox_jobs WHERE project_id=?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )?;
        Ok(total > 0 && total == verified)
    }
    fn event(&self, id: &str, kind: &str, detail: &str) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,?2,?3)",
            params![id, kind, detail],
        )?;
        Ok(())
    }
}

#[cfg(unix)]
fn restrict_database_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions)
}

fn restrict_database_sidecars(path: &Path) -> std::io::Result<()> {
    for suffix in ["-wal", "-shm"] {
        let sidecar = Path::new(&format!("{}{}", path.display(), suffix)).to_owned();
        if sidecar.exists() {
            restrict_database_permissions(&sidecar)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_database_permissions(_: &Path) -> std::io::Result<()> {
    Ok(())
}

fn phase_rank(phase: Phase) -> u8 {
    match phase {
        Phase::Discovery => 0,
        Phase::Preflight => 1,
        Phase::Pilot => 2,
        Phase::Seed => 3,
        Phase::CatchUp => 4,
        Phase::FinalDelta => 5,
        Phase::Verification => 6,
        Phase::Complete => 7,
        Phase::Attention => 255,
    }
}

fn valid_mailbox_transition(current: &str, next: &str) -> bool {
    match current {
        "queued" => matches!(
            next,
            "preflight" | "ready" | "running" | "failed" | "cancelled" | "attention"
        ),
        "preflight" => matches!(next, "ready" | "running" | "failed" | "cancelled"),
        "ready" => matches!(next, "running" | "failed" | "cancelled"),
        "running" => matches!(
            next,
            "ready"
                | "completed"
                | "delta_required"
                | "verification_difference"
                | "failed"
                | "cancelled"
                | "attention"
        ),
        "delta_required" | "verification_difference" => {
            matches!(next, "running" | "failed" | "cancelled" | "attention")
        }
        "completed" => matches!(
            next,
            "verified" | "delta_required" | "verification_difference" | "running" | "attention"
        ),
        "failed" | "cancelled" => matches!(next, "running" | "attention"),
        "verified" => matches!(next, "delta_required" | "running" | "attention"),
        "attention" => matches!(next, "running"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capability_parser_selects_modern_strategy() {
        let caps = ServerCapabilities::parse(
            "* CAPABILITY IMAP4rev1 UIDPLUS CONDSTORE QRESYNC SPECIAL-USE\r\na1 OK",
        );
        assert!(caps.supports("qresync"));
        assert!(caps.strategy().contains(&"QRESYNC delta synchronization"));
    }
    #[test]
    fn evidence_is_durable_and_explainable() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("pilot", "old.example", "new.example")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "a@example", "a@example")
            .unwrap();
        let e = MailboxEvidence {
            source_messages: 8,
            destination_messages: 8,
            source_bytes: 10,
            destination_bytes: 10,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
        };
        db.record_evidence(&job, &e).unwrap();
        assert_eq!(
            db.evidence(&job).unwrap().unwrap().confidence_percent(),
            100
        );
        assert_eq!(
            db.evidence(&job).unwrap().unwrap().evidence_scope(),
            EvidenceScope::EngineConfirmed
        );
        db.transition(&project.id, Phase::Preflight).unwrap();
        db.transition(&project.id, Phase::Verification).unwrap();
        assert_eq!(
            db.project(&project.id).unwrap().unwrap().phase,
            Phase::Verification
        );
    }

    #[test]
    fn folder_mismatch_cannot_claim_full_confidence() {
        let evidence = MailboxEvidence {
            source_messages: 10,
            destination_messages: 10,
            source_bytes: 100,
            destination_bytes: 100,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 4,
            destination_folders: 3,
            authoritative: false,
        };
        assert_eq!(evidence.confidence_percent(), 0);
        assert_eq!(evidence.evidence_level(), "Aggregate mismatch");
        assert!(!evidence.is_exact_match());
    }

    #[test]
    fn empty_mailbox_with_failures_is_not_verified() {
        let evidence = MailboxEvidence {
            source_messages: 0,
            destination_messages: 0,
            source_bytes: 0,
            destination_bytes: 0,
            unmatched_messages: 0,
            failed_messages: 1,
            source_folders: 1,
            destination_folders: 1,
            authoritative: false,
        };
        assert_eq!(evidence.confidence_percent(), 0);
        assert_eq!(evidence.evidence_level(), "Incomplete evidence");
    }

    #[test]
    fn aggregate_match_has_bounded_but_nonmisleading_score() {
        let evidence = MailboxEvidence {
            source_messages: 12,
            destination_messages: 12,
            source_bytes: 100,
            destination_bytes: 100,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 2,
            destination_folders: 2,
            authoritative: false,
        };
        assert_eq!(evidence.confidence_percent(), 85);
        assert!(evidence.is_exact_match());
        assert_eq!(evidence.evidence_level(), "Aggregate match");
        assert_eq!(
            evidence.evidence_scope(),
            EvidenceScope::AggregateReconciled
        );
        assert_eq!(evidence.evidence_scope().label(), "aggregate-reconciled");
    }

    #[test]
    fn mailbox_state_machine_allows_retry_but_rejects_backwards_moves() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.set_mailbox_state(&job, "running").unwrap();
        db.set_mailbox_state(&job, "failed").unwrap();
        db.set_mailbox_state(&job, "running").unwrap();
        assert!(db.set_mailbox_state(&job, "queued").is_err());
    }

    #[test]
    fn mailbox_state_machine_has_an_exhaustive_transition_matrix() {
        let states = [
            "queued",
            "preflight",
            "ready",
            "running",
            "completed",
            "delta_required",
            "verification_difference",
            "failed",
            "cancelled",
            "verified",
            "attention",
        ];
        let allowed = [
            (
                "queued",
                &[
                    "preflight",
                    "ready",
                    "running",
                    "failed",
                    "cancelled",
                    "attention",
                ] as &[&str],
            ),
            ("preflight", &["ready", "running", "failed", "cancelled"]),
            ("ready", &["running", "failed", "cancelled"]),
            (
                "running",
                &[
                    "ready",
                    "completed",
                    "delta_required",
                    "verification_difference",
                    "failed",
                    "cancelled",
                    "attention",
                ],
            ),
            (
                "completed",
                &[
                    "verified",
                    "delta_required",
                    "verification_difference",
                    "running",
                    "attention",
                ],
            ),
            (
                "delta_required",
                &["running", "failed", "cancelled", "attention"],
            ),
            (
                "verification_difference",
                &["running", "failed", "cancelled", "attention"],
            ),
            ("failed", &["running", "attention"]),
            ("cancelled", &["running", "attention"]),
            ("verified", &["delta_required", "running", "attention"]),
            ("attention", &["running"]),
        ];
        for current in states {
            for next in states {
                let expected = allowed
                    .iter()
                    .find(|(state, _)| *state == current)
                    .is_some_and(|(_, targets)| targets.contains(&next));
                assert_eq!(
                    valid_mailbox_transition(current, next),
                    expected,
                    "unexpected mailbox transition {current} -> {next}"
                );
            }
        }
        assert!(!valid_mailbox_transition("unknown", "running"));
        assert!(!valid_mailbox_transition("ready", "unknown"));
    }

    #[test]
    fn mailbox_cannot_start_two_runs_or_be_verified_without_evidence() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-owner", "test")
            .unwrap();
        assert!(
            db.begin_run(&project.id, &job, "run-second", "test")
                .is_err()
        );
        assert!(db.set_mailbox_state(&job, "verified").is_err());
        assert!(
            db.finish_run_for_mailbox(
                &project.id,
                &job,
                "run-owner",
                "completed",
                "verified",
                "without evidence",
            )
            .is_err()
        );
    }

    #[test]
    fn newer_run_cannot_reuse_older_evidence_for_verified_state() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("evidence-age", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 10,
            destination_bytes: 10,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
        };
        db.begin_run(&project.id, &job, "run-with-evidence", "test")
            .unwrap();
        db.finish_run_for_mailbox_with_evidence(
            &project.id,
            &job,
            "run-with-evidence",
            "completed",
            "verified",
            "",
            &evidence,
        )
        .unwrap();
        db.begin_run(&project.id, &job, "run-without-evidence", "test")
            .unwrap();
        db.finish_run_for_mailbox(
            &project.id,
            &job,
            "run-without-evidence",
            "completed",
            "completed",
            "verification unavailable",
        )
        .unwrap();
        assert!(db.set_mailbox_state(&job, "verified").is_err());
    }

    #[test]
    fn generic_terminal_completion_cannot_reuse_stale_evidence_for_verified() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("stale-terminal-evidence", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 10,
            destination_bytes: 10,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
        };
        db.begin_run(&project.id, &job, "run-old-evidence", "imapsync")
            .unwrap();
        db.finish_run_for_mailbox_with_evidence(
            &project.id,
            &job,
            "run-old-evidence",
            "completed",
            "verified",
            "",
            &evidence,
        )
        .unwrap();

        db.begin_run(&project.id, &job, "run-no-evidence", "imapsync")
            .unwrap();
        assert!(
            db.finish_run_for_mailbox(
                &project.id,
                &job,
                "run-no-evidence",
                "completed",
                "verified",
                "stale evidence must not be reused",
            )
            .is_err()
        );
        assert_eq!(
            db.run_status("run-no-evidence").unwrap().as_deref(),
            Some("running")
        );
        assert_eq!(db.mailbox_state(&job).unwrap().as_deref(), Some("running"));
    }

    #[test]
    fn database_rejects_two_running_runs_for_one_mailbox() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-one", "test").unwrap();
        let result = db.connection.execute(
            "INSERT INTO runs(id,project_id,job_id,engine,plan_snapshot,status) VALUES(?1,?2,?3,'test','','running')",
            rusqlite::params!["run-two", project.id, job],
        );
        assert!(result.is_err());
    }

    #[test]
    fn project_cannot_complete_without_verified_mailboxes() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.transition(&project.id, Phase::Preflight).unwrap();
        db.transition(&project.id, Phase::Verification).unwrap();
        assert!(!db.all_mailboxes_verified(&project.id).unwrap());
        assert!(db.transition(&project.id, Phase::Complete).is_err());
        db.set_mailbox_state(&job, "running").unwrap();
        db.insert_run_for_test(&project.id, Some(&job), "run-project-complete", "test")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 10,
            destination_bytes: 10,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
        };
        db.finish_run_for_mailbox_with_evidence(
            &project.id,
            &job,
            "run-project-complete",
            "completed",
            "verified",
            "verified",
            &evidence,
        )
        .unwrap();
        assert!(db.all_mailboxes_verified(&project.id).unwrap());
        db.transition(&project.id, Phase::Complete).unwrap();
    }

    #[test]
    fn attention_phase_can_return_to_reviewable_work() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();

        db.transition(&project.id, Phase::Attention).unwrap();
        db.transition(&project.id, Phase::Preflight).unwrap();
        db.transition(&project.id, Phase::Verification).unwrap();

        assert_eq!(
            db.project(&project.id).unwrap().unwrap().phase,
            Phase::Verification
        );
    }

    #[test]
    fn running_jobs_are_recovered_for_operator_review() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.set_mailbox_state(&job, "running").unwrap();
        db.insert_run_for_test(&project.id, Some(&job), "run-1", "test")
            .unwrap();
        assert_eq!(db.recover_abandoned_jobs().unwrap(), 1);
        assert!(db.set_mailbox_state(&job, "queued").is_err());
        assert_eq!(
            db.run_status("run-1").unwrap().as_deref(),
            Some("abandoned")
        );
    }

    #[test]
    fn active_process_identity_is_durable_and_cleared_on_recovery() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-process", "test")
            .unwrap();
        db.register_process(&ActiveProcess {
            run_id: "run-process".into(),
            job_id: job.clone(),
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        })
        .unwrap();
        assert_eq!(
            db.active_processes().unwrap(),
            vec![ActiveProcess {
                run_id: "run-process".into(),
                job_id: job.clone(),
                pid: 4242,
                start_ticks: Some(7),
                process_group: Some(4242),
                session_id: Some(4242),
                executable: "test".into(),
            }]
        );
        db.recover_abandoned_jobs().unwrap();
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn active_process_rejects_run_and_mailbox_from_different_projects() {
        let db = StateStore::in_memory().unwrap();
        let first = db
            .create_project("first", "source-a", "destination-a")
            .unwrap();
        let first_job = db
            .add_mailbox(&first.id, "source-a", "destination-a")
            .unwrap();
        let second = db
            .create_project("second", "source-b", "destination-b")
            .unwrap();
        let second_job = db
            .add_mailbox(&second.id, "source-b", "destination-b")
            .unwrap();
        db.begin_run(&first.id, &first_job, "run-first", "test")
            .unwrap();

        let result = db.register_process(&ActiveProcess {
            run_id: "run-first".into(),
            job_id: second_job,
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        });
        assert!(result.is_err());
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn active_process_rejects_wrong_mailbox_for_single_mailbox_run() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("same-project", "source", "destination")
            .unwrap();
        let first_job = db
            .add_mailbox(&project.id, "source-one", "destination-one")
            .unwrap();
        let second_job = db
            .add_mailbox(&project.id, "source-two", "destination-two")
            .unwrap();
        db.begin_run(&project.id, &first_job, "run-single", "test")
            .unwrap();

        let result = db.register_process(&ActiveProcess {
            run_id: "run-single".into(),
            job_id: second_job,
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        });
        assert!(result.is_err());
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn active_process_requires_a_mailbox_specific_child_run() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("batch-process", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source-user", "destination-user")
            .unwrap();
        let child_runs = db
            .begin_batch_run_with_children(
                &project.id,
                std::slice::from_ref(&job),
                "run-parent-process",
                "test",
                &[],
                "batch snapshot",
                &[],
            )
            .unwrap();

        assert!(
            db.register_process(&ActiveProcess {
                run_id: "run-parent-process".into(),
                job_id: job.clone(),
                pid: 4242,
                start_ticks: Some(7),
                process_group: Some(4242),
                session_id: Some(4242),
                executable: "test".into(),
            })
            .is_err()
        );
        db.claim_batch_mailbox_for_child(&project.id, &job, "run-parent-process", &child_runs[0])
            .unwrap();
        db.register_process(&ActiveProcess {
            run_id: child_runs[0].clone(),
            job_id: job.clone(),
            pid: 4243,
            start_ticks: Some(8),
            process_group: Some(4243),
            session_id: Some(4243),
            executable: "test".into(),
        })
        .unwrap();
    }

    #[test]
    fn late_process_registration_after_terminal_run_is_rejected() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("late-process", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source-user", "destination-user")
            .unwrap();
        db.begin_run(&project.id, &job, "run-late-process", "test")
            .unwrap();
        db.finish_run(
            "run-late-process",
            "failed",
            "cancelled before process registration",
        )
        .unwrap();

        let result = db.register_process(&ActiveProcess {
            run_id: "run-late-process".into(),
            job_id: job,
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        });
        assert!(result.is_err());
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn terminal_batch_run_clears_process_identity_atomically() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-process-finish", "test")
            .unwrap();
        db.register_process(&ActiveProcess {
            run_id: "run-process-finish".into(),
            job_id: job.clone(),
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        })
        .unwrap();
        db.finish_run("run-process-finish", "failed", "test failure")
            .unwrap();
        assert!(db.active_processes().unwrap().is_empty());
        assert_eq!(
            db.run_status("run-process-finish").unwrap().as_deref(),
            Some("failed")
        );
        assert!(
            db.finish_run("run-process-finish", "failed", "duplicate")
                .is_err()
        );
    }

    #[test]
    fn terminal_run_and_mailbox_state_are_committed_together() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-atomic", "imapsync")
            .unwrap();
        db.set_mailbox_state(&job, "completed").unwrap();

        db.finish_run_for_mailbox(
            &project.id,
            &job,
            "run-atomic",
            "completed",
            "completed",
            "",
        )
        .unwrap();

        assert_eq!(
            db.mailbox_state(&job).unwrap().as_deref(),
            Some("completed")
        );
        assert_eq!(
            db.run_status("run-atomic").unwrap().as_deref(),
            Some("completed")
        );
        assert!(
            db.finish_run_for_mailbox(
                &project.id,
                &job,
                "run-atomic",
                "completed",
                "completed",
                "duplicate completion",
            )
            .is_err()
        );
    }

    #[test]
    fn evidence_terminal_completion_allows_running_to_verified_atomically() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-evidence", "imapsync")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 3,
            destination_messages: 3,
            source_bytes: 300,
            destination_bytes: 300,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 2,
            destination_folders: 2,
            authoritative: true,
        };
        db.register_process(&ActiveProcess {
            run_id: "run-evidence".into(),
            job_id: job.clone(),
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        })
        .unwrap();

        db.finish_run_for_mailbox_with_evidence(
            &project.id,
            &job,
            "run-evidence",
            "completed",
            "verified",
            "",
            &evidence,
        )
        .unwrap();

        assert_eq!(db.mailbox_state(&job).unwrap().as_deref(), Some("verified"));
        assert_eq!(
            db.run_status("run-evidence").unwrap().as_deref(),
            Some("completed")
        );
        assert_eq!(db.evidence(&job).unwrap(), Some(evidence));
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn evidence_difference_is_not_promoted_to_verified_or_complete() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("difference", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-difference", "imapsync")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 10,
            destination_messages: 9,
            source_bytes: 100,
            destination_bytes: 90,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 2,
            destination_folders: 2,
            authoritative: false,
        };
        assert!(
            db.finish_run_for_mailbox_with_evidence(
                &project.id,
                &job,
                "run-difference",
                "completed",
                "verified",
                "mismatching evidence",
                &evidence,
            )
            .is_err()
        );
        assert_eq!(db.mailbox_state(&job).unwrap().as_deref(), Some("running"));
        assert_eq!(
            db.run_status("run-difference").unwrap().as_deref(),
            Some("running")
        );
        db.finish_run_for_mailbox_with_evidence(
            &project.id,
            &job,
            "run-difference",
            "completed",
            "verification_difference",
            "aggregate totals differ",
            &evidence,
        )
        .unwrap();
        assert_eq!(
            db.mailbox_state(&job).unwrap().as_deref(),
            Some("verification_difference")
        );
        assert!(db.transition(&project.id, Phase::Complete).is_err());
    }

    #[test]
    fn latest_evidence_resolves_to_its_own_run() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run(&project.id, &job, "run-evidence-one", "imapsync")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 10,
            destination_bytes: 10,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
        };
        db.finish_run_for_mailbox_with_evidence(
            &project.id,
            &job,
            "run-evidence-one",
            "completed",
            "verified",
            "",
            &evidence,
        )
        .unwrap();
        db.begin_run(&project.id, &job, "run-evidence-two", "imapsync")
            .unwrap();
        db.finish_run_for_mailbox(
            &project.id,
            &job,
            "run-evidence-two",
            "failed",
            "attention",
            "later failure",
        )
        .unwrap();

        let (run_id, latest) = db.latest_evidence_for_run(&job).unwrap().unwrap();
        assert_eq!(run_id, "run-evidence-one");
        assert_eq!(latest, evidence);
        assert_eq!(db.run(&run_id).unwrap().unwrap().status, "completed");
    }

    #[test]
    fn batch_project_creation_is_atomic() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(
            db.first_mailbox(&project.id).unwrap(),
            Some(jobs[0].clone())
        );
        assert_eq!(
            db.mailbox_state(&jobs[1]).unwrap().as_deref(),
            Some("queued")
        );
        assert_eq!(
            db.mailbox_identity(&jobs[0]).unwrap().unwrap(),
            ("one".into(), "one".into(), "queued".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn state_store_does_not_change_parent_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!("mailswiftsync-db-{}", Uuid::new_v4()));
        let path = directory.join("state.db");
        std::fs::create_dir_all(&directory).unwrap();
        let mut permissions = std::fs::metadata(&directory).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&directory, permissions).unwrap();
        let _store = StateStore::open(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o755
        );
        drop(_store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn schema_version_is_recorded_and_future_versions_are_rejected() {
        let db = StateStore::in_memory().unwrap();
        let version: i64 = db
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
        drop(db);

        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-schema-{}", Uuid::new_v4()));
        let path = directory.join("state.db");
        std::fs::create_dir_all(&directory).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "user_version", 99_i64)
            .unwrap();
        drop(connection);
        assert!(StateStore::open(&path).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn batch_configuration_is_persisted_without_credentials() {
        let db = StateStore::in_memory().unwrap();
        let (_, jobs) = db
            .create_project_with_mailbox_configs(
                "batch",
                "source",
                "destination",
                &[("one".into(), "one".into(), "engine = \"imap\"".into())],
            )
            .unwrap();
        let restored = db
            .mailboxes(&db.latest_project().unwrap().unwrap().id)
            .unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, jobs[0]);
        assert_eq!(restored[0].config.as_deref(), Some("engine = \"imap\""));
    }

    #[test]
    fn latest_run_summary_is_queryable_for_audit_reports() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("audit", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.insert_run_for_test(&project.id, Some(&job), "run-audit", "test")
            .unwrap();
        db.finish_run("run-audit", "completed", "ok").unwrap();
        let run = db.latest_run(&job).unwrap().unwrap();
        assert_eq!(run.id, "run-audit");
        assert_eq!(run.status, "completed");
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn recent_run_list_omits_large_plan_snapshots() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("list", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run_with_snapshot(
            &project.id,
            &job,
            "run-list",
            "test",
            &"large snapshot".repeat(1024),
        )
        .unwrap();

        let list = db.recent_run_list(&project.id, 10).unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "run-list");
        assert_eq!(list[0].status, "running");
    }

    #[test]
    fn begin_run_atomically_moves_mailbox_and_records_run() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("atomic", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.set_mailbox_state(&job, "ready").unwrap();
        db.begin_run(&project.id, &job, "run-atomic", "test")
            .unwrap();
        assert_eq!(db.mailbox_state(&job).unwrap().as_deref(), Some("running"));
        assert_eq!(
            db.run_status("run-atomic").unwrap().as_deref(),
            Some("running")
        );
    }

    #[test]
    fn run_plan_snapshot_is_persisted_at_start() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("snapshot", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.begin_run_with_snapshot(
            &project.id,
            &job,
            "run-snapshot",
            "imapsync fallback",
            "dry_run=false\nsource_host = \"source\"",
        )
        .unwrap();
        let run = db.run("run-snapshot").unwrap().unwrap();
        assert_eq!(run.plan_snapshot, "dry_run=false\nsource_host = \"source\"");
    }

    #[test]
    fn begin_batch_run_leaves_children_queued_until_claimed() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-atomic",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        db.begin_batch_run(&project.id, &jobs, "run-batch-atomic", "test", &[])
            .unwrap();
        assert!(
            jobs.iter()
                .all(|job| db.mailbox_state(job).unwrap().as_deref() == Some("queued"))
        );
        assert_eq!(
            db.run_status("run-batch-atomic").unwrap().as_deref(),
            Some("running")
        );
        assert!(
            db.begin_batch_run(&project.id, &jobs, "run-batch-duplicate", "test", &[])
                .is_err()
        );
    }

    #[test]
    fn batch_parent_cannot_finish_with_unresolved_children() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-parent-terminal",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        db.begin_batch_run(&project.id, &jobs, "run-batch-parent", "test", &[])
            .unwrap();

        assert!(
            db.finish_run("run-batch-parent", "completed", "premature completion")
                .is_err()
        );
        assert_eq!(
            db.run_status("run-batch-parent").unwrap().as_deref(),
            Some("running")
        );
        assert_eq!(
            db.mailbox_state(&jobs[0]).unwrap().as_deref(),
            Some("queued")
        );
        assert_eq!(
            db.mailbox_state(&jobs[1]).unwrap().as_deref(),
            Some("queued")
        );
    }

    #[test]
    fn parent_run_rejects_unknown_terminal_status() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("run-status", "source", "destination")
            .unwrap();
        db.insert_run_for_test(&project.id, None, "run-status", "test")
            .unwrap();

        assert!(db.finish_run("run-status", "invented", "invalid").is_err());
        assert_eq!(
            db.run_status("run-status").unwrap().as_deref(),
            Some("running")
        );
    }

    #[test]
    fn successful_parent_run_rejects_unsuccessful_child() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-child-failure",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        db.begin_batch_run(&project.id, &jobs, "run-parent-failure", "test", &[])
            .unwrap();
        let child = db
            .recent_runs(&project.id, 10)
            .unwrap()
            .into_iter()
            .find(|run| run.job_id.as_deref() == Some(jobs[0].as_str()))
            .unwrap();
        db.claim_batch_mailbox_for_child(&project.id, &jobs[0], "run-parent-failure", &child.id)
            .unwrap();
        db.finish_run_for_mailbox(
            &project.id,
            &jobs[0],
            &child.id,
            "failed",
            "failed",
            "synthetic child failure",
        )
        .unwrap();
        let second_child = db
            .recent_runs(&project.id, 10)
            .unwrap()
            .into_iter()
            .find(|run| run.job_id.as_deref() == Some(jobs[1].as_str()))
            .unwrap();
        db.claim_batch_mailbox_for_child(
            &project.id,
            &jobs[1],
            "run-parent-failure",
            &second_child.id,
        )
        .unwrap();
        db.finish_run_for_mailbox(
            &project.id,
            &jobs[1],
            &second_child.id,
            "completed",
            "completed",
            "synthetic child completion",
        )
        .unwrap();

        assert!(
            db.finish_run("run-parent-failure", "completed", "incorrect success")
                .is_err()
        );
        assert_eq!(
            db.run_status("run-parent-failure").unwrap().as_deref(),
            Some("running")
        );
    }

    #[test]
    fn batch_claim_moves_only_the_claimed_mailbox_to_running() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-claim",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        db.begin_batch_run(&project.id, &jobs, "run-batch-claim", "test", &[])
            .unwrap();
        db.claim_batch_mailbox(&project.id, &jobs[0], "run-batch-claim")
            .unwrap();
        assert_eq!(
            db.mailbox_state(&jobs[0]).unwrap().as_deref(),
            Some("running")
        );
        assert_eq!(
            db.mailbox_state(&jobs[1]).unwrap().as_deref(),
            Some("queued")
        );
        db.claim_batch_mailbox(&project.id, &jobs[0], "run-batch-claim")
            .unwrap();
        assert_eq!(
            db.mailbox_state(&jobs[0]).unwrap().as_deref(),
            Some("running")
        );
    }

    #[test]
    fn batch_children_persist_mailbox_specific_runs_and_snapshots() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-children",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        let child_runs = db
            .begin_batch_run_with_children(
                &project.id,
                &jobs,
                "run-parent",
                "batch",
                &[],
                "parent snapshot",
                &[
                    BatchChildPlan {
                        engine: "imapsync".into(),
                        plan_snapshot: "snapshot one".into(),
                    },
                    BatchChildPlan {
                        engine: "dovecot".into(),
                        plan_snapshot: "snapshot two".into(),
                    },
                ],
            )
            .unwrap();
        assert_eq!(child_runs.len(), 2);
        let first = db.run(&child_runs[0]).unwrap().unwrap();
        let second = db.run(&child_runs[1]).unwrap().unwrap();
        assert_eq!(first.job_id.as_deref(), Some(jobs[0].as_str()));
        assert_eq!(first.status, "queued");
        assert_eq!(first.engine, "imapsync");
        assert_eq!(first.plan_snapshot, "snapshot one");
        assert_eq!(second.job_id.as_deref(), Some(jobs[1].as_str()));
        assert_eq!(second.status, "queued");
        assert_eq!(second.engine, "dovecot");
        assert_eq!(second.plan_snapshot, "snapshot two");
        db.claim_batch_mailbox_for_child(&project.id, &jobs[0], "run-parent", &child_runs[0])
            .unwrap();
        db.register_process(&ActiveProcess {
            run_id: child_runs[0].clone(),
            job_id: jobs[0].clone(),
            pid: 4242,
            start_ticks: Some(1),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test-engine".into(),
        })
        .unwrap();
        db.finish_run_for_mailbox(
            &project.id,
            &jobs[0],
            &child_runs[0],
            "completed",
            "completed",
            "child complete",
        )
        .unwrap();
        assert_eq!(
            db.run_status(&child_runs[0]).unwrap().as_deref(),
            Some("completed")
        );
        assert!(db.active_processes().unwrap().is_empty());
    }

    #[test]
    fn recovery_abandons_unclaimed_batch_children_without_marking_them_running() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-before-claim-crash",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        let child_runs = db
            .begin_batch_run_with_children(
                &project.id,
                &jobs,
                "run-parent-before-claim",
                "test",
                &[],
                "batch snapshot",
                &[],
            )
            .unwrap();

        assert_eq!(db.recover_abandoned_jobs().unwrap(), 0);
        assert_eq!(
            db.run_status("run-parent-before-claim").unwrap().as_deref(),
            Some("abandoned")
        );
        for (job, child_run) in jobs.iter().zip(child_runs) {
            assert_eq!(db.mailbox_state(job).unwrap().as_deref(), Some("queued"));
            assert_eq!(
                db.run_status(&child_run).unwrap().as_deref(),
                Some("abandoned")
            );
        }

        db.begin_batch_run(
            &project.id,
            &jobs,
            "run-retry-after-before-claim",
            "test",
            &[],
        )
        .unwrap();
    }

    #[test]
    fn recovery_abandons_queued_and_claimed_batch_children_distinctly() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-partial-claim-crash",
                "source",
                "destination",
                &[
                    ("one".into(), "one".into()),
                    ("two".into(), "two".into()),
                    ("three".into(), "three".into()),
                ],
            )
            .unwrap();
        let child_runs = db
            .begin_batch_run_with_children(
                &project.id,
                &jobs,
                "run-parent-partial-claim",
                "test",
                &[],
                "batch snapshot",
                &[],
            )
            .unwrap();
        db.claim_batch_mailbox_for_child(
            &project.id,
            &jobs[0],
            "run-parent-partial-claim",
            &child_runs[0],
        )
        .unwrap();
        db.register_process(&ActiveProcess {
            run_id: child_runs[0].clone(),
            job_id: jobs[0].clone(),
            pid: 4242,
            start_ticks: Some(7),
            process_group: Some(4242),
            session_id: Some(4242),
            executable: "test".into(),
        })
        .unwrap();

        assert_eq!(db.recover_abandoned_jobs().unwrap(), 1);
        assert_eq!(
            db.mailbox_state(&jobs[0]).unwrap().as_deref(),
            Some("attention")
        );
        assert_eq!(
            db.mailbox_state(&jobs[1]).unwrap().as_deref(),
            Some("queued")
        );
        assert_eq!(
            db.mailbox_state(&jobs[2]).unwrap().as_deref(),
            Some("queued")
        );
        assert!(jobs.iter().all(|job| {
            db.latest_run(job)
                .unwrap()
                .is_some_and(|run| run.status == "abandoned")
        }));
        assert!(db.active_processes().unwrap().is_empty());

        db.begin_batch_run(
            &project.id,
            &jobs,
            "run-retry-after-partial-claim",
            "test",
            &[],
        )
        .unwrap();
    }

    #[test]
    fn begin_batch_run_checks_all_live_plans_in_one_boundary() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "batch-plans",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        db.set_preflight_plan(&jobs[0], "plan-one").unwrap();
        db.set_preflight_plan(&jobs[1], "plan-two").unwrap();
        let result = db.begin_batch_run(
            &project.id,
            &jobs,
            "run-batch-plans",
            "test",
            &["plan-one".into(), "stale-plan".into()],
        );
        assert!(result.is_err());
        assert!(
            jobs.iter()
                .all(|job| { db.mailbox_state(job).unwrap().as_deref() == Some("queued") })
        );
        assert!(db.run_status("run-batch-plans").unwrap().is_none());
    }

    #[test]
    fn output_events_are_committed_as_one_batch() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("events", "source", "destination")
            .unwrap();
        let details: Vec<String> = vec!["first".into(), "second".into(), "third".into()];
        let batch = details
            .iter()
            .map(|detail| (project.id.as_str(), "run_output", detail.as_str()))
            .collect::<Vec<_>>();
        db.record_events_batch(&batch).unwrap();
        let count: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE project_id=?1 AND kind='run_output'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn run_events_are_bound_to_their_durable_run() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("run-events", "source", "destination")
            .unwrap();
        let job = db
            .add_mailbox(&project.id, "source-user", "destination-user")
            .unwrap();
        db.begin_run(&project.id, &job, "run-events-1", "test")
            .unwrap();

        db.record_run_events_batch(
            "run-events-1",
            &[
                ("run_output", "transfer output"),
                ("verification_evidence", "aggregate"),
            ],
        )
        .unwrap();

        let rows: Vec<(String, String, String)> = db
            .connection
            .prepare("SELECT project_id, run_id, detail FROM events WHERE run_id=?1 ORDER BY id")
            .unwrap()
            .query_map(["run-events-1"], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|(project_id, run_id, _)| {
            project_id == &project.id && run_id == "run-events-1"
        }));
        assert!(
            rows.iter()
                .any(|(_, _, detail)| detail == "transfer output")
        );
    }

    #[test]
    fn verbose_output_is_retained_as_a_bounded_project_tail() {
        let db = StateStore::in_memory().unwrap();
        let project = db
            .create_project("retention", "source", "destination")
            .unwrap();
        let details = (0..=MAX_DURABLE_RUN_OUTPUT_EVENTS as usize)
            .map(|index| format!("line-{index}"))
            .collect::<Vec<_>>();
        let batch = details
            .iter()
            .map(|detail| (project.id.as_str(), "run_output", detail.as_str()))
            .collect::<Vec<_>>();

        db.record_events_batch(&batch).unwrap();

        let count: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE project_id=?1 AND kind='run_output'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        let oldest: String = db
            .connection
            .query_row(
                "SELECT detail FROM events WHERE project_id=?1 AND kind='run_output' ORDER BY id ASC LIMIT 1",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();
        let audit_count: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE project_id=?1 AND kind='project_created'",
                [&project.id],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(count, MAX_DURABLE_RUN_OUTPUT_EVENTS);
        assert_eq!(oldest, "line-1");
        assert_eq!(audit_count, 1);
    }

    #[test]
    fn evidence_rejects_orphaned_run_ids() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("audit", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 1,
            destination_bytes: 1,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
        };
        assert!(
            db.record_evidence_for_run(&job, "missing", &evidence)
                .is_err()
        );
    }

    #[test]
    fn evidence_rejects_a_run_for_a_different_mailbox() {
        let db = StateStore::in_memory().unwrap();
        let (project, jobs) = db
            .create_project_with_mailboxes(
                "evidence-ownership",
                "source",
                "destination",
                &[("one".into(), "one".into()), ("two".into(), "two".into())],
            )
            .unwrap();
        db.begin_run(&project.id, &jobs[0], "evidence-run-one", "test")
            .unwrap();
        let evidence = MailboxEvidence {
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 1,
            destination_bytes: 1,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
        };

        assert!(
            db.record_evidence_for_run(&jobs[1], "evidence-run-one", &evidence)
                .is_err()
        );
        assert!(db.evidence(&jobs[1]).unwrap().is_none());
    }
}
