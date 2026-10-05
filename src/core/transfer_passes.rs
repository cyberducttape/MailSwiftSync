//! Durable transfer-pass provenance.
//!
//! MailSwiftSync delegates message copying to an external engine, so it
//! cannot checkpoint individual messages it never handles. What it can and
//! does own is an exact record of what it asked the engine to do and what was
//! observed afterwards. For every live engine attempt the ledger keeps:
//!
//! - which mailbox pair it was (as a digest) and which pass of that mailbox
//!   within the project it belongs to (a child run is one pass; retries
//!   inside it are attempts of that pass);
//! - the pass kind, engine, executable digest, and the secret-free argument
//!   vector that was launched, with its digest;
//! - the requested folder scope and source range (MailSwiftSync never
//!   narrows a pass below the whole mailbox; Dovecot resumes from a state
//!   token recorded only by digest);
//! - the outcome, delta requirement, engine completion counters, and the
//!   digest of any newly emitted resume state;
//! - the verification method and outcome, and per verified folder and side
//!   the observed snapshot (UIDVALIDITY, UIDNEXT, EXISTS), the UID the
//!   verifier reached, and whether the folder completed.
//!
//! Folder names never enter the ledger; they are recorded as project-scoped
//! digests, following the mailbox-metadata privacy boundary.

use super::StateStore;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// What MailSwiftSync asked the engine to do for one attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TransferPassIntent {
    /// Digest of the canonical source/destination mailbox pair.
    pub mailbox_digest: String,
    pub pass_kind: String,
    pub engine: String,
    pub executable_identity: String,
    /// The launched argument vector with credentials, private runtime file
    /// paths, and resume-state tokens replaced by placeholders.
    pub command: Vec<String>,
    pub folder_scope: String,
    pub source_range: String,
}

/// Engine-reported totals at the end of an attempt. Counts only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct EngineCompletionCounters {
    pub source_folders: u64,
    pub destination_folders: u64,
    pub source_messages: u64,
    pub destination_messages: u64,
    pub source_bytes: u64,
    pub destination_bytes: u64,
    pub unmatched_messages: Option<u64>,
}

/// What was observed when an attempt ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferPassCompletion {
    pub engine_counters: Option<EngineCompletionCounters>,
    pub emitted_state_sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PassSide {
    Source,
    Destination,
}

impl PassSide {
    fn as_i64(self) -> i64 {
        match self {
            Self::Source => 0,
            Self::Destination => 1,
        }
    }

    fn from_i64(value: i64) -> rusqlite::Result<Self> {
        match value {
            0 => Ok(Self::Source),
            1 => Ok(Self::Destination),
            _ => Err(rusqlite::Error::IntegralValueOutOfRange(0, value)),
        }
    }
}

/// One verified folder on one side, as the verifier observed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TransferPassFolder {
    pub side: PassSide,
    pub folder_digest: String,
    pub uidvalidity: u64,
    pub uidnext: u64,
    pub exists: u64,
    pub verified_through_uid: u64,
    pub staged_messages: u64,
    pub complete: bool,
}

/// One recorded attempt with everything the ledger knows about it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TransferPassRecord {
    pub run_id: String,
    pub attempt: u32,
    pub pass_sequence: u32,
    pub pass_kind: String,
    pub engine: String,
    pub executable_identity: String,
    pub command_sha256: String,
    pub command: Vec<String>,
    pub folder_scope: String,
    pub source_range: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: Option<String>,
    pub delta_required: Option<bool>,
    pub engine_counters: Option<EngineCompletionCounters>,
    pub emitted_state_sha256: Option<String>,
    pub verification_method: Option<String>,
    pub verification_outcome: Option<String>,
    pub verified_at: Option<String>,
    pub folders: Vec<TransferPassFolder>,
    /// Last durable, content-free engine progress observed for this attempt.
    pub progress: Option<TransferProgressSnapshot>,
}

/// Recovery-facing progress. `skipped` is optional because imapsync does not
/// emit a provider-independent skipped counter; `unresolved` is the engine's
/// remaining-work count when it reports one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct TransferProgressSnapshot {
    pub copied: u64,
    pub bytes_copied: u64,
    pub skipped: Option<u64>,
    pub unresolved: Option<u64>,
}

/// Project-scoped digest of a mailbox folder name. Equal names on both
/// sides of one project share a digest, so source and destination folders
/// can be compared without storing either name.
pub fn folder_digest(project_id: &str, folder: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"mailswiftsync-folder-v1\0");
    hasher.update(project_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(folder.as_bytes());
    hex(&hasher.finalize())
}

pub fn sha256_hex(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn command_digest(command: &[String]) -> String {
    sha256_hex(command.join("\u{1f}").as_bytes())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl TransferPassIntent {
    /// A well-formed intent for tests that do not examine provenance.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self {
            mailbox_digest: sha256_hex(b"test-mailbox"),
            pass_kind: "imapsync_sync".into(),
            engine: "imapsync".into(),
            executable_identity: "sha256:test".into(),
            command: vec!["imapsync".into()],
            folder_scope: "all_selectable_folders;mapping=identity".into(),
            source_range: "full_mailbox".into(),
        }
    }

    fn validate(&self) -> rusqlite::Result<()> {
        if !valid_digest(&self.mailbox_digest)
            || self.pass_kind.is_empty()
            || self.engine.is_empty()
            || self.command.is_empty()
            || self.folder_scope.is_empty()
            || self.source_range.is_empty()
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(())
    }
}

impl StateStore {
    /// Insert the provenance row for a newly started attempt. Called inside
    /// the transaction that records `transfer_attempt_started`.
    pub(super) fn insert_transfer_pass(
        tx: &rusqlite::Transaction<'_>,
        project_id: &str,
        run_id: &str,
        attempt: u32,
        intent: &TransferPassIntent,
    ) -> rusqlite::Result<()> {
        intent.validate()?;
        // Retries inside one run are attempts of the same pass; a new run
        // for the mailbox is the next pass.
        let existing: Option<i64> = tx
            .query_row(
                "SELECT pass_sequence FROM transfer_passes WHERE run_id=?1 LIMIT 1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        let pass_sequence = match existing {
            Some(sequence) => sequence,
            None => tx.query_row(
                "SELECT COALESCE(MAX(pass_sequence),0)+1 FROM transfer_passes WHERE project_id=?1 AND mailbox_digest=?2",
                params![project_id, intent.mailbox_digest],
                |row| row.get(0),
            )?,
        };
        let command = serde_json::to_string(&intent.command)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        tx.execute(
            "INSERT INTO transfer_passes(run_id,attempt,project_id,mailbox_digest,pass_sequence,pass_kind,engine,executable_identity,command_sha256,command,folder_scope,source_range) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                run_id,
                attempt,
                project_id,
                intent.mailbox_digest,
                pass_sequence,
                intent.pass_kind,
                intent.engine,
                intent.executable_identity,
                command_digest(&intent.command),
                command,
                intent.folder_scope,
                intent.source_range,
            ],
        )?;
        Ok(())
    }

    /// Close the provenance row of a finished attempt.
    pub(super) fn finish_transfer_pass(
        tx: &rusqlite::Transaction<'_>,
        run_id: &str,
        attempt: u32,
        outcome: &str,
        completion: &TransferPassCompletion,
    ) -> rusqlite::Result<()> {
        if completion
            .emitted_state_sha256
            .as_deref()
            .is_some_and(|digest| !valid_digest(digest))
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let counters = completion
            .engine_counters
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let changed = tx.execute(
            "UPDATE transfer_passes SET finished_at=CURRENT_TIMESTAMP, outcome=?3, delta_required=?4, completion_evidence=?5, emitted_state_sha256=?6 WHERE run_id=?1 AND attempt=?2 AND outcome IS NULL",
            params![
                run_id,
                attempt,
                outcome,
                outcome == "delta_required",
                counters,
                completion.emitted_state_sha256,
            ],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(())
    }

    /// Attach verification results to the attempt whose transfer they
    /// examined. Only a successfully finished, not yet verified attempt of a
    /// running run accepts them.
    pub fn record_transfer_pass_verified(
        &self,
        run_id: &str,
        attempt: u32,
        method: &str,
        outcome: &str,
        folders: &[TransferPassFolder],
    ) -> rusqlite::Result<()> {
        if method.is_empty()
            || outcome.is_empty()
            || folders
                .iter()
                .any(|folder| !valid_digest(&folder.folder_digest))
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE transfer_passes SET verification_method=?3, verification_outcome=?4, verified_at=CURRENT_TIMESTAMP WHERE run_id=?1 AND attempt=?2 AND outcome IN ('completed','delta_required') AND verified_at IS NULL AND run_id IN (SELECT id FROM runs WHERE status IN ('running','queued'))",
            params![run_id, attempt, method, outcome],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO transfer_pass_folders(run_id,attempt,side,folder_digest,uidvalidity,uidnext,exists_count,verified_through_uid,staged_messages,complete) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            )?;
            for folder in folders {
                insert.execute(params![
                    run_id,
                    attempt,
                    folder.side.as_i64(),
                    folder.folder_digest,
                    sql_u64(folder.uidvalidity)?,
                    sql_u64(folder.uidnext)?,
                    sql_u64(folder.exists)?,
                    sql_u64(folder.verified_through_uid)?,
                    sql_u64(folder.staged_messages)?,
                    folder.complete,
                ])?;
            }
        }
        tx.commit()
    }

    /// Every recorded attempt of `run_id`, oldest first, with its folders.
    pub fn transfer_passes(&self, run_id: &str) -> rusqlite::Result<Vec<TransferPassRecord>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT run_id,attempt,pass_sequence,pass_kind,engine,executable_identity,command_sha256,command,folder_scope,source_range,started_at,finished_at,outcome,delta_required,completion_evidence,emitted_state_sha256,verification_method,verification_outcome,verified_at FROM transfer_passes WHERE run_id=?1 ORDER BY attempt",
        )?;
        let mut records = statement
            .query_map([run_id], |row| {
                let command: String = row.get(7)?;
                let counters: Option<String> = row.get(14)?;
                Ok(TransferPassRecord {
                    run_id: row.get(0)?,
                    attempt: row.get(1)?,
                    pass_sequence: row.get(2)?,
                    pass_kind: row.get(3)?,
                    engine: row.get(4)?,
                    executable_identity: row.get(5)?,
                    command_sha256: row.get(6)?,
                    command: serde_json::from_str(&command).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            7,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    folder_scope: row.get(8)?,
                    source_range: row.get(9)?,
                    started_at: row.get(10)?,
                    finished_at: row.get(11)?,
                    outcome: row.get(12)?,
                    delta_required: row.get(13)?,
                    engine_counters: counters
                        .map(|value| serde_json::from_str(&value))
                        .transpose()
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                14,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                    emitted_state_sha256: row.get(15)?,
                    verification_method: row.get(16)?,
                    verification_outcome: row.get(17)?,
                    verified_at: row.get(18)?,
                    folders: Vec::new(),
                    progress: None,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut folders = self.connection.prepare_cached(
            "SELECT side,folder_digest,uidvalidity,uidnext,exists_count,verified_through_uid,staged_messages,complete FROM transfer_pass_folders WHERE run_id=?1 AND attempt=?2 ORDER BY side,folder_digest",
        )?;
        for record in &mut records {
            let progress_json: Option<String> = self.connection.query_row(
                "SELECT detail FROM events WHERE run_id=?1 AND kind='transfer_progress_checkpoint' AND detail LIKE ?2 ORDER BY id DESC LIMIT 1",
                params![run_id, format!("%\"attempt\":{}%", record.attempt)],
                |row| row.get(0),
            ).optional()?;
            record.progress = progress_json.and_then(|detail| {
                let value: serde_json::Value = serde_json::from_str(&detail).ok()?;
                let progress = value.get("progress")?;
                Some(TransferProgressSnapshot {
                    copied: progress.get("messages_copied")?.as_u64()?,
                    bytes_copied: progress.get("bytes_copied")?.as_u64()?,
                    skipped: progress
                        .get("source_messages")
                        .and_then(serde_json::Value::as_u64)
                        .zip(
                            progress
                                .get("messages_left")
                                .and_then(serde_json::Value::as_u64),
                        )
                        .map(|(source, left)| {
                            source.saturating_sub(left).saturating_sub(
                                progress
                                    .get("messages_copied")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(0),
                            )
                        }),
                    unresolved: progress
                        .get("messages_left")
                        .and_then(serde_json::Value::as_u64),
                })
            });
            record.folders = folders
                .query_map(params![run_id, record.attempt], |row| {
                    Ok(TransferPassFolder {
                        side: PassSide::from_i64(row.get(0)?)?,
                        folder_digest: row.get(1)?,
                        uidvalidity: row.get::<_, i64>(2)? as u64,
                        uidnext: row.get::<_, i64>(3)? as u64,
                        exists: row.get::<_, i64>(4)? as u64,
                        verified_through_uid: row.get::<_, i64>(5)? as u64,
                        staged_messages: row.get::<_, i64>(6)? as u64,
                        complete: row.get(7)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
        }
        Ok(records)
    }
}

fn sql_u64(value: u64) -> rusqlite::Result<i64> {
    i64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, i64::MAX))
}
