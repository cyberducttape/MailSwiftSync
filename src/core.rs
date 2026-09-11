//! Durable migration-control-plane primitives.
//!
//! The GUI may be replaced, but project state and verification evidence remain
//! portable SQLite data. No credentials or message content belong in this store.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

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
pub struct MailboxEvidence {
    pub source_messages: u64,
    pub destination_messages: u64,
    pub source_bytes: u64,
    pub destination_bytes: u64,
    pub unmatched_messages: u64,
    pub failed_messages: u64,
}

impl MailboxEvidence {
    pub fn confidence_percent(&self) -> u8 {
        if self.source_messages == 0 && self.destination_messages == 0 {
            return 100;
        }
        let count_ok = self.source_messages == self.destination_messages;
        let bytes_ok = self.source_bytes == self.destination_bytes;
        if count_ok && bytes_ok && self.unmatched_messages == 0 && self.failed_messages == 0 {
            100
        } else if self.unmatched_messages == 0 && self.failed_messages == 0 {
            85
        } else {
            0
        }
    }
}

pub struct StateStore {
    connection: Connection,
}

impl StateStore {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let store = Self {
            connection: Connection::open(path)?,
        };
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
          CREATE TABLE IF NOT EXISTS mailbox_jobs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), source_mailbox TEXT NOT NULL, destination_mailbox TEXT NOT NULL, state TEXT NOT NULL, attempt INTEGER NOT NULL DEFAULT 0, checkpoint TEXT);
          CREATE TABLE IF NOT EXISTS evidence (job_id TEXT PRIMARY KEY REFERENCES mailbox_jobs(id), source_messages INTEGER NOT NULL, destination_messages INTEGER NOT NULL, source_bytes INTEGER NOT NULL, destination_bytes INTEGER NOT NULL, unmatched_messages INTEGER NOT NULL, failed_messages INTEGER NOT NULL, captured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
          CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id), kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);")
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
    pub fn transition(&self, id: &str, phase: Phase) -> rusqlite::Result<()> {
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
    pub fn record_evidence(&self, job_id: &str, value: &MailboxEvidence) -> rusqlite::Result<()> {
        self.connection.execute("INSERT INTO evidence(job_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(job_id) DO UPDATE SET source_messages=excluded.source_messages,destination_messages=excluded.destination_messages,source_bytes=excluded.source_bytes,destination_bytes=excluded.destination_bytes,unmatched_messages=excluded.unmatched_messages,failed_messages=excluded.failed_messages,captured_at=CURRENT_TIMESTAMP", params![job_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages])?;
        Ok(())
    }
    pub fn evidence(&self, job_id: &str) -> rusqlite::Result<Option<MailboxEvidence>> {
        self.connection.query_row("SELECT source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages FROM evidence WHERE job_id=?1", [job_id], |r| Ok(MailboxEvidence { source_messages:r.get(0)?, destination_messages:r.get(1)?, source_bytes:r.get(2)?, destination_bytes:r.get(3)?, unmatched_messages:r.get(4)?, failed_messages:r.get(5)? })).optional()
    }
    pub fn project(&self, id: &str) -> rusqlite::Result<Option<Project>> {
        self.connection.query_row("SELECT id,name,source_endpoint,destination_endpoint,phase FROM projects WHERE id=?1", [id], |r| Ok(Project { id:r.get(0)?, name:r.get(1)?, source_endpoint:r.get(2)?, destination_endpoint:r.get(3)?, phase: Phase::parse(&r.get::<_,String>(4)?)? })).optional()
    }
    fn event(&self, id: &str, kind: &str, detail: &str) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,?2,?3)",
            params![id, kind, detail],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        };
        db.record_evidence(&job, &e).unwrap();
        assert_eq!(
            db.evidence(&job).unwrap().unwrap().confidence_percent(),
            100
        );
        db.transition(&project.id, Phase::Verification).unwrap();
        assert_eq!(
            db.project(&project.id).unwrap().unwrap().phase,
            Phase::Verification
        );
    }
}
