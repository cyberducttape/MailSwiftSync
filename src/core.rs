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
            Self::Auto => "Automatic",
            Self::Dovecot => "Dovecot native",
            Self::ImapSync => "imapsync fallback",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Auto => {
                "Choose the safest available path; imapsync remains the conservative fallback."
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
pub struct MailboxEvidence {
    pub source_messages: u64,
    pub destination_messages: u64,
    pub source_bytes: u64,
    pub destination_bytes: u64,
    pub unmatched_messages: u64,
    pub failed_messages: u64,
    pub source_folders: u64,
    pub destination_folders: u64,
    /// True only when the engine supplied message-level/authoritative proof.
    /// Aggregate mailbox totals must never be presented as full verification.
    pub authoritative: bool,
}

impl MailboxEvidence {
    pub fn confidence_percent(&self) -> u8 {
        if !self.authoritative {
            return if self.unmatched_messages == 0 && self.failed_messages == 0 {
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

impl StateStore {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let path = path.as_ref();
        let store = Self {
            connection: Connection::open(path)?,
        };
        restrict_database_permissions(path)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        store.migrate()?;
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
        self.connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, source_endpoint TEXT NOT NULL, destination_endpoint TEXT NOT NULL, phase TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
          CREATE TABLE IF NOT EXISTS mailbox_jobs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), source_mailbox TEXT NOT NULL, destination_mailbox TEXT NOT NULL, state TEXT NOT NULL, attempt INTEGER NOT NULL DEFAULT 0, checkpoint TEXT, preflight_plan TEXT, config TEXT);
          CREATE TABLE IF NOT EXISTS evidence (job_id TEXT PRIMARY KEY REFERENCES mailbox_jobs(id), source_messages INTEGER NOT NULL, destination_messages INTEGER NOT NULL, source_bytes INTEGER NOT NULL, destination_bytes INTEGER NOT NULL, unmatched_messages INTEGER NOT NULL, failed_messages INTEGER NOT NULL, source_folders INTEGER NOT NULL DEFAULT 0, destination_folders INTEGER NOT NULL DEFAULT 0, authoritative INTEGER NOT NULL DEFAULT 0, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
          CREATE TABLE IF NOT EXISTS evidence_history (id INTEGER PRIMARY KEY, job_id TEXT NOT NULL REFERENCES mailbox_jobs(id), run_id TEXT NOT NULL, source_messages INTEGER NOT NULL, destination_messages INTEGER NOT NULL, source_bytes INTEGER NOT NULL, destination_bytes INTEGER NOT NULL, unmatched_messages INTEGER NOT NULL, failed_messages INTEGER NOT NULL, source_folders INTEGER NOT NULL, destination_folders INTEGER NOT NULL, authoritative INTEGER NOT NULL DEFAULT 0, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
          CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), job_id TEXT REFERENCES mailbox_jobs(id), engine TEXT NOT NULL, status TEXT NOT NULL, started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, finished_at TEXT, detail TEXT NOT NULL DEFAULT '');
          CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);")?;
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
        if current != phase
            && (current == Phase::Complete
                || current == Phase::Attention
                    && !matches!(phase, Phase::Preflight | Phase::Verification)
                || phase != Phase::Attention && phase_rank(phase) < phase_rank(current))
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
        self.connection.execute(
            "UPDATE projects SET phase=?1 WHERE id=?2",
            params![phase.as_str(), id],
        )?;
        self.event(id, "phase_changed", phase.as_str())
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
                | "completed"
                | "verified"
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
    /// exists. Running jobs are therefore made reviewable rather than left in
    /// a permanently active state.
    pub fn recover_abandoned_jobs(&self) -> rusqlite::Result<usize> {
        let projects = self
            .connection
            .prepare("SELECT DISTINCT project_id FROM mailbox_jobs WHERE state='running'")?
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
        for project in projects {
            tx.execute(
                "INSERT INTO events(project_id,kind,detail) VALUES(?1,'run_recovered','Running mailbox jobs moved to Attention after application restart')",
                [project],
            )?;
        }
        tx.commit()?;
        Ok(count)
    }
    pub fn record_event(&self, project_id: &str, kind: &str, detail: &str) -> rusqlite::Result<()> {
        self.event(project_id, kind, detail)
    }
    pub fn start_run(
        &self,
        project_id: &str,
        job_id: Option<&str>,
        run_id: &str,
        engine: &str,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO runs(id,project_id,job_id,engine,status) VALUES(?1,?2,?3,?4,'running')",
            params![run_id, project_id, job_id, engine],
        )?;
        Ok(())
    }
    pub fn finish_run(&self, run_id: &str, status: &str, detail: &str) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE runs SET status=?1,finished_at=CURRENT_TIMESTAMP,detail=?2 WHERE id=?3",
            params![status, detail, run_id],
        )?;
        Ok(())
    }
    pub fn run_status(&self, run_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row("SELECT status FROM runs WHERE id=?1", [run_id], |row| {
                row.get(0)
            })
            .optional()
    }
    pub fn record_evidence(&self, job_id: &str, value: &MailboxEvidence) -> rusqlite::Result<()> {
        self.record_evidence_for_run(job_id, "legacy", value)
    }
    pub fn record_evidence_for_run(
        &self,
        job_id: &str,
        run_id: &str,
        value: &MailboxEvidence,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        tx.execute("INSERT INTO evidence_history(job_id,run_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", params![job_id, run_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages, value.source_folders, value.destination_folders, value.authoritative])?;
        tx.execute("INSERT INTO evidence(job_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(job_id) DO UPDATE SET source_messages=excluded.source_messages,destination_messages=excluded.destination_messages,source_bytes=excluded.source_bytes,destination_bytes=excluded.destination_bytes,unmatched_messages=excluded.unmatched_messages,failed_messages=excluded.failed_messages,source_folders=excluded.source_folders,destination_folders=excluded.destination_folders,authoritative=excluded.authoritative,captured_at=CURRENT_TIMESTAMP", params![job_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages, value.source_folders, value.destination_folders, value.authoritative])?;
        tx.commit()?;
        Ok(())
    }
    pub fn evidence(&self, job_id: &str) -> rusqlite::Result<Option<MailboxEvidence>> {
        self.connection.query_row("SELECT source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative FROM evidence WHERE job_id=?1", [job_id], |r| Ok(MailboxEvidence { source_messages:r.get(0)?, destination_messages:r.get(1)?, source_bytes:r.get(2)?, destination_bytes:r.get(3)?, unmatched_messages:r.get(4)?, failed_messages:r.get(5)?, source_folders:r.get(6)?, destination_folders:r.get(7)?, authoritative:r.get::<_, i64>(8)? != 0 })).optional()
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
            "preflight" | "ready" | "running" | "failed" | "cancelled"
        ),
        "preflight" => matches!(next, "ready" | "running" | "failed" | "cancelled"),
        "ready" => matches!(next, "running" | "failed" | "cancelled"),
        "running" => matches!(
            next,
            "ready" | "completed" | "delta_required" | "failed" | "cancelled" | "attention"
        ),
        "delta_required" => matches!(next, "running" | "failed" | "cancelled"),
        "completed" => matches!(
            next,
            "verified" | "delta_required" | "running" | "attention"
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
        assert_eq!(evidence.confidence_percent(), 85);
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
    fn project_cannot_complete_without_verified_mailboxes() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.transition(&project.id, Phase::Preflight).unwrap();
        db.transition(&project.id, Phase::Verification).unwrap();
        assert!(db.transition(&project.id, Phase::Complete).is_err());
        db.set_mailbox_state(&job, "running").unwrap();
        db.set_mailbox_state(&job, "completed").unwrap();
        db.set_mailbox_state(&job, "verified").unwrap();
        db.transition(&project.id, Phase::Complete).unwrap();
    }

    #[test]
    fn running_jobs_are_recovered_for_operator_review() {
        let db = StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        db.set_mailbox_state(&job, "running").unwrap();
        db.start_run(&project.id, Some(&job), "run-1", "test")
            .unwrap();
        assert_eq!(db.recover_abandoned_jobs().unwrap(), 1);
        assert!(db.set_mailbox_state(&job, "queued").is_err());
        assert_eq!(
            db.run_status("run-1").unwrap().as_deref(),
            Some("abandoned")
        );
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
}
