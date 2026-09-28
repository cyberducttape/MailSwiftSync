//! Private SQLite staging for live metadata reconciliation.

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    path::PathBuf,
};

use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::{ExtractedMessage, ExtractedMessages, MailboxMessageKey, sqlite_i64};

pub(crate) const STAGE_BATCH_SIZE: i64 = 512;

pub(crate) fn durable_stage_path(state_path: &std::path::Path, job_id: &str) -> PathBuf {
    let safe_id = job_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .take(96)
        .collect::<String>();
    state_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("verification-stages")
        .join(format!("{safe_id}.sqlite"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StagedMessageSide {
    Source = 0,
    Destination = 1,
}

impl StagedMessageSide {
    pub(crate) fn as_i64(self) -> i64 {
        self as i64
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StagedMessage {
    pub(crate) rowid: i64,
    pub(crate) key: MailboxMessageKey,
    pub(crate) message: ExtractedMessage,
    pub(crate) date_key: Option<String>,
}

pub(crate) struct MessageMetadataStage {
    connection: Option<Connection>,
    path: Option<PathBuf>,
    retain_on_drop: bool,
    /// Body fingerprints are retained only for the explicitly enabled
    /// forensic verification path. Metadata-only verification remains fully
    /// staged in SQLite and does not retain content hashes in memory.
    content_fingerprints: HashMap<(StagedMessageSide, MailboxMessageKey), String>,
}

impl MessageMetadataStage {
    pub(crate) fn open_ephemeral() -> Result<Self, String> {
        // Put the database under the same private run-directory lifecycle as
        // credential files. If the process is killed before Drop runs, the
        // normal stale-run cleanup can remove the mailbox metadata later.
        let directory = crate::credentials::create_secret_directory()
            .map_err(|error| format!("could not create verification stage directory: {error}"))?;
        let path = directory.join(format!("verification-stage-{}.db", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        options
            .open(&path)
            .map_err(|error| format!("could not create verification stage: {error}"))?;
        let connection = match Connection::open(&path) {
            Ok(connection) => connection,
            Err(error) => {
                let _ = fs::remove_file(&path);
                let _ = fs::remove_dir(&directory);
                return Err(format!("could not open verification stage: {error}"));
            }
        };
        let mut stage = Self {
            connection: Some(connection),
            path: Some(path),
            retain_on_drop: false,
            content_fingerprints: HashMap::new(),
        };
        if let Err(error) = stage.initialize(false) {
            let path = stage.path.take();
            if let Some(connection) = stage.connection.take() {
                let _ = connection.close();
            }
            if let Some(path) = path {
                let _ = fs::remove_file(&path);
                if let Some(directory) = path.parent() {
                    let _ = fs::remove_dir(directory);
                }
            }
            return Err(format!("could not initialize verification stage: {error}"));
        }
        Ok(stage)
    }

    #[cfg(test)]
    pub(crate) fn open_in_memory() -> rusqlite::Result<Self> {
        let mut stage = Self {
            connection: Some(Connection::open_in_memory()?),
            path: None,
            retain_on_drop: false,
            content_fingerprints: HashMap::new(),
        };
        stage.initialize(false)?;
        Ok(stage)
    }

    /// Open the private, restartable staging database for one verification.
    /// The caller removes it after verification has produced durable evidence;
    /// retaining it on an interrupted run is what makes page-level recovery
    /// possible after the controller process is restarted.
    pub(crate) fn open_durable(path: PathBuf, identity: &str) -> Result<Self, String> {
        let parent = path
            .parent()
            .ok_or_else(|| "verification stage path has no parent directory".to_owned())?;
        crate::credentials::ensure_private_directory(parent)
            .map_err(|error| format!("could not secure verification stage directory: {error}"))?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && metadata.file_type().is_symlink()
        {
            return Err("verification stage path is a symbolic link".to_owned());
        }
        options
            .open(&path)
            .map_err(|error| format!("could not create durable verification stage: {error}"))?;
        let connection = Connection::open(&path)
            .map_err(|error| format!("could not open durable verification stage: {error}"))?;
        let mut stage = Self {
            connection: Some(connection),
            path: Some(path),
            retain_on_drop: true,
            content_fingerprints: HashMap::new(),
        };
        if let Err(error) = stage
            .initialize(true)
            .and_then(|_| stage.bind_identity(identity))
        {
            let path = stage.path.take();
            if let Some(connection) = stage.connection.take() {
                let _ = connection.close();
            }
            if let Some(path) = path {
                let _ = fs::remove_file(path);
            }
            return Err(format!(
                "could not initialize durable verification stage: {error}"
            ));
        }
        stage.load_content_fingerprints().map_err(|error| {
            format!("could not load durable verification fingerprints: {error}")
        })?;
        Ok(stage)
    }

    pub(crate) fn finish(&mut self) -> Result<(), String> {
        self.retain_on_drop = false;
        if let Some(path) = self.path.take() {
            if let Some(connection) = self.connection.take() {
                connection
                    .close()
                    .map_err(|(_, error)| format!("could not close verification stage: {error}"))?;
            }
            fs::remove_file(&path)
                .map_err(|error| format!("could not remove verification stage: {error}"))?;
            for suffix in ["-wal", "-shm"] {
                let sidecar = PathBuf::from(format!("{}{}", path.display(), suffix));
                let _ = fs::remove_file(sidecar);
            }
        }
        Ok(())
    }

    fn connection_ref(&self) -> &Connection {
        self.connection
            .as_ref()
            .expect("verification stage is open")
    }

    fn initialize(&mut self, durable: bool) -> rusqlite::Result<()> {
        if !durable {
            self.connection_ref()
                .execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;")?;
        }
        // Durable stages intentionally leave SQLite's default rollback
        // journal and FULL synchronization untouched. This avoids a runtime
        // PRAGMA mutation that some packaged SQLite builds reject while still
        // retaining crash-safe defaults.
        self.connection_ref().execute_batch(
            "CREATE TABLE IF NOT EXISTS staged_messages(
                 side INTEGER NOT NULL CHECK(side IN (0,1)),
                 mailbox TEXT NOT NULL,
                 match_mailbox TEXT NOT NULL,
                 uidvalidity INTEGER NOT NULL CHECK(uidvalidity >= -1),
                 uid TEXT NOT NULL,
                 message_id TEXT,
                 internal_date TEXT,
                 date_key TEXT,
                 size_bytes INTEGER CHECK(size_bytes IS NULL OR size_bytes >= 0),
                 PRIMARY KEY(side,mailbox,uidvalidity,uid)
             );
             CREATE INDEX IF NOT EXISTS staged_messages_id ON staged_messages(side,message_id,mailbox,uidvalidity,uid);
             CREATE INDEX IF NOT EXISTS staged_messages_metadata ON staged_messages(side,date_key,size_bytes,mailbox,uidvalidity,uid);
             CREATE INDEX IF NOT EXISTS staged_messages_match_metadata ON staged_messages(side,match_mailbox,date_key,size_bytes,uidvalidity,uid);
             CREATE TABLE IF NOT EXISTS stage_metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS stage_fingerprints(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uid TEXT NOT NULL, fingerprint TEXT NOT NULL, PRIMARY KEY(side,mailbox,uidvalidity,uid));
             CREATE TABLE IF NOT EXISTS stage_cursors(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, last_uid INTEGER NOT NULL CHECK(last_uid >= 0), completed INTEGER NOT NULL CHECK(completed IN (0,1)), PRIMARY KEY(side,mailbox));",
        )?;
        if !durable {
            // Ephemeral stages are disposable and may use an in-memory temp
            // store. Durable stages must not mutate connection settings: the
            // packaged engine can expose a read-only SQLite wrapper.
            self.connection_ref()
                .execute_batch("PRAGMA temp_store=MEMORY;")?;
        }
        Ok(())
    }

    fn bind_identity(&mut self, identity: &str) -> rusqlite::Result<()> {
        if identity.len() > 256 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let existing: Option<String> = self
            .connection_ref()
            .query_row(
                "SELECT value FROM stage_metadata WHERE key='identity'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if existing.as_deref() != Some(identity) {
            let tx = self.connection_ref().unchecked_transaction()?;
            tx.execute("DELETE FROM staged_messages", [])?;
            tx.execute("DELETE FROM stage_fingerprints", [])?;
            tx.execute("DELETE FROM stage_cursors", [])?;
            tx.execute("DELETE FROM stage_metadata", [])?;
            tx.execute(
                "INSERT INTO stage_metadata(key,value) VALUES('identity',?1)",
                [identity],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    fn load_content_fingerprints(&mut self) -> rusqlite::Result<()> {
        let mut statement = self
            .connection_ref()
            .prepare("SELECT side,mailbox,uidvalidity,uid,fingerprint FROM stage_fingerprints")?;
        let rows = statement.query_map([], |row| {
            let side: i64 = row.get(0)?;
            let side = match side {
                0 => StagedMessageSide::Source,
                1 => StagedMessageSide::Destination,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            let uidvalidity: i64 = row.get(2)?;
            let uidvalidity = u64::try_from(uidvalidity)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, uidvalidity))?;
            Ok((
                (
                    side,
                    MailboxMessageKey::with_shared_mailbox(
                        std::sync::Arc::from(row.get::<_, String>(1)?),
                        Some(uidvalidity),
                        row.get::<_, String>(3)?,
                    ),
                ),
                row.get::<_, String>(4)?,
            ))
        })?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for row in rows {
            let (key, fingerprint) = row;
            self.content_fingerprints.insert(key, fingerprint);
        }
        Ok(())
    }

    pub(crate) fn insert_messages(
        &mut self,
        side: StagedMessageSide,
        messages: &ExtractedMessages,
    ) -> Result<(), String> {
        self.insert_messages_with_fingerprints(side, messages, &HashMap::new())
    }

    pub(crate) fn insert_messages_with_fingerprints(
        &mut self,
        side: StagedMessageSide,
        messages: &ExtractedMessages,
        fingerprints: &HashMap<MailboxMessageKey, String>,
    ) -> Result<(), String> {
        if fingerprints.keys().any(|key| !messages.contains_key(key)) {
            return Err("body fingerprint did not have a matching staged message".to_owned());
        }
        let tx = self
            .connection_ref()
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        // HashMap iteration order is randomized. Persist each fetched page in
        // canonical key order so the staged verifier's row traversal and
        // duplicate-Message-ID pairing are repeatable.
        let mut ordered_messages = messages.iter().collect::<Vec<_>>();
        ordered_messages.sort_by(|(left, _), (right, _)| left.cmp(right));
        {
            let mut insert = tx.prepare_cached("INSERT OR REPLACE INTO staged_messages(side,mailbox,match_mailbox,uidvalidity,uid,message_id,internal_date,date_key,size_bytes) VALUES(?1,?2,?2,?3,?4,?5,?6,?7,?8)").map_err(|e| e.to_string())?;
            for (key, message) in ordered_messages {
                let uidvalidity = key
                    .uidvalidity
                    .map(sqlite_i64)
                    .transpose()
                    .map_err(|e| e.to_string())?
                    .unwrap_or(-1);
                let size = message
                    .size_bytes
                    .map(sqlite_i64)
                    .transpose()
                    .map_err(|e| e.to_string())?;
                insert
                    .execute(params![
                        side.as_i64(),
                        key.mailbox.as_ref(),
                        uidvalidity,
                        key.uid,
                        message.message_id,
                        message.internal_date,
                        metadata_date_key(message.internal_date.as_deref()),
                        size
                    ])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        for (key, fingerprint) in fingerprints {
            self.content_fingerprints
                .insert((side, key.clone()), fingerprint.clone());
            let uidvalidity = key
                .uidvalidity
                .map(sqlite_i64)
                .transpose()
                .map_err(|e| e.to_string())?
                .unwrap_or(-1);
            self.connection_ref()
                .execute(
                    "INSERT OR REPLACE INTO stage_fingerprints(side,mailbox,uidvalidity,uid,fingerprint) VALUES(?1,?2,?3,?4,?5)",
                    params![side.as_i64(), key.mailbox.as_ref(), uidvalidity, key.uid, fingerprint],
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub(crate) fn delete_mailbox(
        &mut self,
        side: StagedMessageSide,
        mailbox: &str,
    ) -> Result<(), String> {
        self.content_fingerprints
            .retain(|(fingerprint_side, key), _| {
                *fingerprint_side != side || key.mailbox.as_ref() != mailbox
            });
        self.connection_ref()
            .execute(
                "DELETE FROM staged_messages WHERE side=?1 AND mailbox=?2",
                params![side.as_i64(), mailbox],
            )
            .map_err(|e| e.to_string())?;
        self.connection_ref()
            .execute(
                "DELETE FROM stage_fingerprints WHERE side=?1 AND mailbox=?2",
                params![side.as_i64(), mailbox],
            )
            .map_err(|e| e.to_string())?;
        self.connection_ref()
            .execute(
                "DELETE FROM stage_cursors WHERE side=?1 AND mailbox=?2",
                params![side.as_i64(), mailbox],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(crate) fn resume_cursor(
        &self,
        side: StagedMessageSide,
        mailbox: &str,
        uidvalidity: u64,
    ) -> rusqlite::Result<Option<u64>> {
        self.connection_ref().query_row(
            "SELECT last_uid,completed FROM stage_cursors WHERE side=?1 AND mailbox=?2 AND uidvalidity=?3",
            params![side.as_i64(), mailbox, sqlite_i64(uidvalidity).map_err(|_| rusqlite::Error::InvalidQuery)?],
            |row| {
                let last_uid: i64 = row.get(0)?;
                let completed: i64 = row.get(1)?;
                let last_uid = u64::try_from(last_uid)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, last_uid))?;
                Ok(if completed == 1 { u64::MAX } else { last_uid })
            },
        ).optional()
    }

    pub(crate) fn checkpoint_page(
        &mut self,
        side: StagedMessageSide,
        mailbox: &str,
        uidvalidity: u64,
        last_uid: u64,
    ) -> Result<(), String> {
        self.connection_ref()
            .execute(
                "INSERT INTO stage_cursors(side,mailbox,uidvalidity,last_uid,completed) VALUES(?1,?2,?3,?4,0) ON CONFLICT(side,mailbox) DO UPDATE SET uidvalidity=excluded.uidvalidity,last_uid=excluded.last_uid,completed=0",
                params![side.as_i64(), mailbox, sqlite_i64(uidvalidity).map_err(|e| e.to_string())?, sqlite_i64(last_uid).map_err(|e| e.to_string())?],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub(crate) fn complete_mailbox(
        &mut self,
        side: StagedMessageSide,
        mailbox: &str,
        uidvalidity: u64,
        last_uid: u64,
    ) -> Result<(), String> {
        self.connection_ref()
            .execute(
                "INSERT INTO stage_cursors(side,mailbox,uidvalidity,last_uid,completed) VALUES(?1,?2,?3,?4,1) ON CONFLICT(side,mailbox) DO UPDATE SET uidvalidity=excluded.uidvalidity,last_uid=excluded.last_uid,completed=1",
                params![side.as_i64(), mailbox, sqlite_i64(uidvalidity).map_err(|e| e.to_string())?, sqlite_i64(last_uid).map_err(|e| e.to_string())?],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub(crate) fn reset_reconciliation(&mut self) -> Result<(), String> {
        self.connection_ref()
            .execute_batch(
                "DROP TABLE IF EXISTS staged_matched;
                 DROP TABLE IF EXISTS staged_folder_mapping;
                 DROP TABLE IF EXISTS staged_fingerprint_buckets;
                 DROP TABLE IF EXISTS staged_duplicate_ids;",
            )
            .map_err(|error| error.to_string())
    }

    pub(crate) fn count(&self, side: StagedMessageSide) -> rusqlite::Result<u64> {
        self.connection_ref().query_row(
            "SELECT COUNT(*) FROM staged_messages WHERE side=?1",
            [side.as_i64()],
            |row| {
                let value: i64 = row.get(0)?;
                u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
            },
        )
    }

    pub(crate) fn sum_bytes(&self, side: StagedMessageSide) -> rusqlite::Result<u64> {
        self.connection_ref().query_row(
            "SELECT COALESCE(SUM(size_bytes), 0) FROM staged_messages WHERE side=?1",
            [side.as_i64()],
            |row| {
                let value: i64 = row.get(0)?;
                u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
            },
        )
    }

    pub(crate) fn batch(
        &self,
        side: StagedMessageSide,
        after_rowid: i64,
        only_message_id: bool,
        only_metadata: bool,
    ) -> rusqlite::Result<Vec<StagedMessage>> {
        let condition_id = if only_message_id {
            " AND message_id IS NOT NULL"
        } else {
            ""
        };
        let condition_metadata = if only_metadata {
            " AND date_key IS NOT NULL AND size_bytes IS NOT NULL"
        } else {
            ""
        };
        let sql = format!(
            "SELECT rowid,mailbox,uidvalidity,uid,message_id,internal_date,date_key,size_bytes FROM staged_messages WHERE side=?1 AND rowid>?2 AND NOT EXISTS(SELECT 1 FROM staged_matched m WHERE m.side=staged_messages.side AND m.mailbox=staged_messages.mailbox AND m.uidvalidity=staged_messages.uidvalidity AND m.uid=staged_messages.uid){condition_id}{condition_metadata} ORDER BY rowid LIMIT {}",
            super::message_staging::STAGE_BATCH_SIZE
        );
        self.connection_ref()
            .prepare(&sql)?
            .query_map(params![side.as_i64(), after_rowid], staged_message_from_row)?
            .collect()
    }

    pub(crate) fn connection(&self) -> &Connection {
        self.connection_ref()
    }

    pub(crate) fn content_fingerprints(
        &self,
        side: StagedMessageSide,
    ) -> HashMap<MailboxMessageKey, String> {
        self.content_fingerprints
            .iter()
            .filter(|((fingerprint_side, _), _)| *fingerprint_side == side)
            .map(|((_, key), fingerprint)| (key.clone(), fingerprint.clone()))
            .collect()
    }

    pub(crate) fn all_messages(
        &self,
        side: StagedMessageSide,
    ) -> rusqlite::Result<ExtractedMessages> {
        let mut statement = self.connection_ref().prepare(
            "SELECT mailbox,uidvalidity,uid,message_id,internal_date,size_bytes FROM staged_messages WHERE side=?1 ORDER BY rowid",
        )?;
        let rows = statement.query_map([side.as_i64()], |row| {
            let uidvalidity: i64 = row.get(1)?;
            let uidvalidity = if uidvalidity >= 0 {
                Some(
                    u64::try_from(uidvalidity)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, uidvalidity))?,
                )
            } else {
                None
            };
            let uid: String = row.get(2)?;
            let size_bytes = row
                .get::<_, Option<i64>>(5)?
                .map(|value| {
                    u64::try_from(value)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, value))
                })
                .transpose()?;
            let key = MailboxMessageKey::with_shared_mailbox(
                std::sync::Arc::from(row.get::<_, String>(0)?),
                uidvalidity,
                uid.clone(),
            );
            Ok((
                key,
                ExtractedMessage {
                    message_id: row.get(3)?,
                    uid: Some(uid),
                    size_bytes,
                    internal_date: row.get(4)?,
                },
            ))
        })?;
        rows.collect()
    }
}

pub(crate) fn staged_message_from_row(row: &Row<'_>) -> rusqlite::Result<StagedMessage> {
    let uidvalidity: i64 = row.get(2)?;
    let uid: String = row.get(3)?;
    let uidvalidity = if uidvalidity >= 0 {
        Some(
            u64::try_from(uidvalidity)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, uidvalidity))?,
        )
    } else {
        None
    };
    let size_bytes = row
        .get::<_, Option<i64>>(7)?
        .map(|value| {
            u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, value))
        })
        .transpose()?;
    Ok(StagedMessage {
        rowid: row.get(0)?,
        key: MailboxMessageKey::with_shared_mailbox(
            std::sync::Arc::from(row.get::<_, String>(1)?),
            uidvalidity,
            uid.clone(),
        ),
        message: ExtractedMessage {
            message_id: row.get(4)?,
            uid: Some(uid),
            size_bytes,
            internal_date: row.get(5)?,
        },
        date_key: row.get(6)?,
    })
}

fn metadata_date_key(value: Option<&str>) -> Option<String> {
    value.map(|value| {
        match chrono::DateTime::<chrono::FixedOffset>::parse_from_str(
            value.trim(),
            "%d-%b-%Y %H:%M:%S %z",
        ) {
            Ok(date) => format!("e:{}", date.timestamp()),
            Err(_) => format!("r:{value}"),
        }
    })
}

impl Drop for MessageMetadataStage {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            let _ = connection.close();
        }
        if !self.retain_on_drop
            && let Some(path) = self.path.take()
        {
            let _ = fs::remove_file(&path);
            if let Some(directory) = path.parent() {
                let _ = fs::remove_dir(directory);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_batches_and_rolls_back_mailboxes() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        let messages = ExtractedMessages::from([(
            MailboxMessageKey::with_uidvalidity("INBOX", 7, "42"),
            ExtractedMessage {
                message_id: Some("<m@example.test>".into()),
                uid: Some("42".into()),
                size_bytes: Some(123),
                internal_date: Some("01-Jan-2024 00:00:00 +0000".into()),
            },
        )]);
        let key = messages.keys().next().unwrap().clone();
        let fingerprints = HashMap::from([(key.clone(), "a".repeat(64))]);
        stage
            .insert_messages_with_fingerprints(StagedMessageSide::Source, &messages, &fingerprints)
            .unwrap();
        assert_eq!(
            stage.content_fingerprints(StagedMessageSide::Source),
            fingerprints
        );
        assert_eq!(
            stage.all_messages(StagedMessageSide::Source).unwrap(),
            messages
        );
        stage.connection().execute_batch("CREATE TABLE staged_matched(side INTEGER,mailbox TEXT,uidvalidity INTEGER,uid TEXT,PRIMARY KEY(side,mailbox,uidvalidity,uid));").unwrap();
        let batch = stage
            .batch(StagedMessageSide::Source, 0, true, true)
            .unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].date_key.as_deref(), Some("e:1704067200"));
        stage
            .delete_mailbox(StagedMessageSide::Source, "INBOX")
            .unwrap();
        assert_eq!(stage.count(StagedMessageSide::Source).unwrap(), 0);
        assert!(
            stage
                .content_fingerprints(StagedMessageSide::Source)
                .is_empty()
        );
    }

    #[test]
    fn durable_stage_reopens_with_cursor_and_identity_binding() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        let key = MailboxMessageKey::with_uidvalidity("INBOX", 42, "7");
        let mut messages = ExtractedMessages::new();
        messages.insert(
            key.clone(),
            ExtractedMessage {
                message_id: Some("<seven@example.test>".into()),
                uid: Some("7".into()),
                size_bytes: Some(128),
                internal_date: None,
            },
        );
        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
        stage
            .insert_messages_with_fingerprints(
                StagedMessageSide::Source,
                &messages,
                &HashMap::from([(key.clone(), "abc".into())]),
            )
            .unwrap();
        stage
            .checkpoint_page(StagedMessageSide::Source, "INBOX", 42, 7)
            .unwrap();
        drop(stage);

        let stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
        assert_eq!(stage.count(StagedMessageSide::Source).unwrap(), 1);
        assert_eq!(
            stage
                .resume_cursor(StagedMessageSide::Source, "INBOX", 42)
                .unwrap(),
            Some(7)
        );
        assert_eq!(
            stage.content_fingerprints(StagedMessageSide::Source).len(),
            1
        );
        let mut stage = stage;
        stage.finish().unwrap();
        assert!(!path.exists());

        let stage = MessageMetadataStage::open_durable(path.clone(), "plan-b").unwrap();
        assert_eq!(stage.count(StagedMessageSide::Source).unwrap(), 0);
        assert_eq!(
            stage
                .resume_cursor(StagedMessageSide::Source, "INBOX", 42)
                .unwrap(),
            None
        );
        drop(stage);
        let _ = std::fs::remove_dir_all(directory);
    }
}
