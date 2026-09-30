//! Private SQLite staging for live metadata reconciliation.

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
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

#[cfg(unix)]
type FileIdentity = (u64, u64);
#[cfg(not(unix))]
type FileIdentity = ();

fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
    }
}

/// Require an opened stage descriptor to be a regular file owned by this
/// user, and make it owner-only through the descriptor rather than a path.
fn secure_stage_file(file: &fs::File) -> std::io::Result<FileIdentity> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "verification stage is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "verification stage is owned by another user",
            ));
        }
        crate::credentials::restrict_open_file_permissions(file)?;
    }
    Ok(file_identity(&metadata))
}

/// Apply the stage's owner-only contract to a sidecar left by an earlier
/// process. SQLite creates new sidecars with the database file's mode.
fn secure_existing_sidecar(path: &std::path::Path) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(file) => secure_stage_file(&file).map(drop),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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

/// The SELECT state that produced a folder's staged pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FolderSnapshot {
    pub(crate) uidvalidity: u64,
    pub(crate) uidnext: u64,
    pub(crate) exists: u64,
}

impl FolderSnapshot {
    fn to_sql(self) -> rusqlite::Result<(i64, i64, i64)> {
        Ok((
            sqlite_i64(self.uidvalidity)?,
            sqlite_i64(self.uidnext)?,
            sqlite_i64(self.exists)?,
        ))
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
    remove_parent_on_cleanup: bool,
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
            remove_parent_on_cleanup: true,
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
            remove_parent_on_cleanup: false,
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
        let file = options
            .open(&path)
            .map_err(|error| format!("could not create durable verification stage: {error}"))?;
        // The stage holds mail-derived metadata and optional body
        // fingerprints. `mode(0o600)` only applies at creation, and the
        // directory contract allows readable bits on an existing directory,
        // so repair the file and any SQLite sidecars on every open.
        let expected_identity = secure_stage_file(&file)
            .map_err(|error| format!("could not secure durable verification stage: {error}"))?;
        for suffix in ["-journal", "-wal", "-shm"] {
            secure_existing_sidecar(&PathBuf::from(format!("{}{suffix}", path.display())))
                .map_err(|error| {
                    format!("could not secure durable verification stage sidecar: {error}")
                })?;
        }
        let connection = Connection::open(&path)
            .map_err(|error| format!("could not open durable verification stage: {error}"))?;
        // SQLite reopens by pathname; require it to be the descriptor we
        // secured.
        let opened_identity = fs::symlink_metadata(&path)
            .map_err(|error| format!("could not inspect durable verification stage: {error}"))
            .map(|metadata| file_identity(&metadata))?;
        if opened_identity != expected_identity {
            return Err("durable verification stage path changed while opening".to_owned());
        }
        drop(file);
        let mut stage = Self {
            connection: Some(connection),
            path: Some(path),
            retain_on_drop: true,
            remove_parent_on_cleanup: false,
            content_fingerprints: HashMap::new(),
        };
        if let Err(error) = stage
            .initialize(true)
            .map_err(|error| format!("schema initialization: {error}"))
            .and_then(|()| {
                stage
                    .bind_identity(identity)
                    .map_err(|error| format!("identity binding: {error}"))
            })
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
        let mut errors = Vec::new();
        if let Some(connection) = self.connection.take() {
            match connection.close() {
                Ok(()) => {}
                Err((connection, error)) => {
                    self.connection = Some(connection);
                    errors.push(format!("could not close verification stage: {error}"));
                }
            }
        }
        if let Some(path) = self.path.as_ref() {
            errors.extend(cleanup_stage_files(path, self.remove_parent_on_cleanup));
        }
        if errors.is_empty() {
            self.path = None;
        } else {
            return Err(errors.join("; "));
        }
        if self.connection.is_some() {
            return Err("verification stage connection remains open after cleanup".to_owned());
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
             CREATE TABLE IF NOT EXISTS stage_cursors(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uidnext INTEGER NOT NULL CHECK(uidnext >= 0), exists_count INTEGER NOT NULL CHECK(exists_count >= 0), last_uid INTEGER NOT NULL CHECK(last_uid >= 0), completed INTEGER NOT NULL CHECK(completed IN (0,1)), PRIMARY KEY(side,mailbox));",
        )?;
        // Stages written before cursors recorded the full SELECT snapshot
        // cannot prove their pages still describe the server. They are a
        // disposable cache, so discard them and rescan.
        let snapshot_cursors: bool = self.connection_ref().query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('stage_cursors') WHERE name='exists_count')",
            [],
            |row| row.get(0),
        )?;
        if !snapshot_cursors {
            self.connection_ref().execute_batch(
                "DROP TABLE stage_cursors;
                 DELETE FROM staged_messages;
                 DELETE FROM stage_fingerprints;
                 CREATE TABLE stage_cursors(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uidnext INTEGER NOT NULL CHECK(uidnext >= 0), exists_count INTEGER NOT NULL CHECK(exists_count >= 0), last_uid INTEGER NOT NULL CHECK(last_uid >= 0), completed INTEGER NOT NULL CHECK(completed IN (0,1)), PRIMARY KEY(side,mailbox));",
            )?;
        }
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
            // Fingerprints share the page's transaction: a page is staged
            // with all of its body hashes or not at all, and a durable stage
            // syncs once per page rather than once per message.
            let mut insert_fingerprint = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO stage_fingerprints(side,mailbox,uidvalidity,uid,fingerprint) VALUES(?1,?2,?3,?4,?5)",
                )
                .map_err(|e| e.to_string())?;
            for (key, fingerprint) in fingerprints {
                let uidvalidity = key
                    .uidvalidity
                    .map(sqlite_i64)
                    .transpose()
                    .map_err(|e| e.to_string())?
                    .unwrap_or(-1);
                insert_fingerprint
                    .execute(params![
                        side.as_i64(),
                        key.mailbox.as_ref(),
                        uidvalidity,
                        key.uid,
                        fingerprint
                    ])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        for (key, fingerprint) in fingerprints {
            self.content_fingerprints
                .insert((side, key.clone()), fingerprint.clone());
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

    /// Prepare one folder for a (possibly resumed) scan and return the UID
    /// after which staged pages may be reused.
    ///
    /// Staged rows are reusable only while the server still reports the exact
    /// SELECT snapshot (UIDVALIDITY, UIDNEXT, EXISTS) that produced them. An
    /// interrupted run cannot observe expunges or deliveries that happen
    /// before the restart, so any difference discards the folder's rows,
    /// fingerprints, and cursor and the scan restarts from zero. The new
    /// snapshot is recorded in the same transaction, before any page is
    /// accepted.
    pub(crate) fn resume_mailbox(
        &mut self,
        side: StagedMessageSide,
        mailbox: &str,
        snapshot: FolderSnapshot,
    ) -> Result<Option<u64>, String> {
        let (uidvalidity, uidnext, exists) = snapshot.to_sql().map_err(|e| e.to_string())?;
        let cursor: Option<(i64, i64, i64, i64)> = self
            .connection_ref()
            .query_row(
                "SELECT uidvalidity,uidnext,exists_count,last_uid FROM stage_cursors WHERE side=?1 AND mailbox=?2",
                params![side.as_i64(), mailbox],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some((stored_validity, stored_next, stored_exists, last_uid)) = cursor
            && (stored_validity, stored_next, stored_exists) == (uidvalidity, uidnext, exists)
        {
            return u64::try_from(last_uid)
                .map(Some)
                .map_err(|_| "verification stage cursor is corrupt".to_owned());
        }
        let tx = self
            .connection_ref()
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        for table in ["staged_messages", "stage_fingerprints", "stage_cursors"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE side=?1 AND mailbox=?2"),
                params![side.as_i64(), mailbox],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute(
            "INSERT INTO stage_cursors(side,mailbox,uidvalidity,uidnext,exists_count,last_uid,completed) VALUES(?1,?2,?3,?4,?5,0,0)",
            params![side.as_i64(), mailbox, uidvalidity, uidnext, exists],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        self.content_fingerprints
            .retain(|(fingerprint_side, key), _| {
                *fingerprint_side != side || key.mailbox.as_ref() != mailbox
            });
        Ok(None)
    }

    pub(crate) fn checkpoint_page(
        &mut self,
        side: StagedMessageSide,
        mailbox: &str,
        snapshot: FolderSnapshot,
        last_uid: u64,
    ) -> Result<(), String> {
        self.advance_cursor(side, mailbox, snapshot, last_uid, false)
    }

    /// Mark a folder fully staged. The staged population must equal the
    /// snapshot's EXISTS; anything else means rows from another snapshot
    /// survived and the folder cannot be trusted on a later resume.
    pub(crate) fn complete_mailbox(
        &mut self,
        side: StagedMessageSide,
        mailbox: &str,
        snapshot: FolderSnapshot,
        last_uid: u64,
    ) -> Result<(), String> {
        let staged = self
            .count_mailbox(side, mailbox, snapshot.uidvalidity)
            .map_err(|e| e.to_string())?;
        if staged != snapshot.exists {
            return Err(format!(
                "folder {mailbox}: staged rows ({staged}) differ from snapshot EXISTS {}",
                snapshot.exists
            ));
        }
        self.advance_cursor(side, mailbox, snapshot, last_uid, true)
    }

    fn advance_cursor(
        &mut self,
        side: StagedMessageSide,
        mailbox: &str,
        snapshot: FolderSnapshot,
        last_uid: u64,
        completed: bool,
    ) -> Result<(), String> {
        let (uidvalidity, uidnext, exists) = snapshot.to_sql().map_err(|e| e.to_string())?;
        let updated = self
            .connection_ref()
            .execute(
                "UPDATE stage_cursors SET last_uid=?6,completed=?7 WHERE side=?1 AND mailbox=?2 AND uidvalidity=?3 AND uidnext=?4 AND exists_count=?5",
                params![
                    side.as_i64(),
                    mailbox,
                    uidvalidity,
                    uidnext,
                    exists,
                    sqlite_i64(last_uid).map_err(|e| e.to_string())?,
                    i64::from(completed)
                ],
            )
            .map_err(|e| e.to_string())?;
        if updated != 1 {
            return Err(format!(
                "folder {mailbox}: verification cursor does not belong to the current snapshot"
            ));
        }
        Ok(())
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

    pub(crate) fn count_mailbox(
        &self,
        side: StagedMessageSide,
        mailbox: &str,
        uidvalidity: u64,
    ) -> rusqlite::Result<u64> {
        self.connection_ref().query_row(
            "SELECT COUNT(*) FROM staged_messages WHERE side=?1 AND mailbox=?2 AND uidvalidity=?3",
            params![side.as_i64(), mailbox, sqlite_i64(uidvalidity)?],
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
        if let Some(connection) = self.connection.take()
            && let Err((connection, _)) = connection.close()
        {
            drop(connection);
        }
        if !self.retain_on_drop
            && let Some(path) = self.path.as_ref()
        {
            let _ = cleanup_stage_files(path, self.remove_parent_on_cleanup);
        }
    }
}

fn cleanup_stage_files(path: &Path, remove_parent: bool) -> Vec<String> {
    let mut errors = Vec::new();
    let mut artifacts = vec![(path.to_path_buf(), "verification stage")];
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        artifacts.push((PathBuf::from(sidecar), "verification stage sidecar"));
    }
    for (artifact, description) in artifacts {
        if let Err(error) = fs::remove_file(&artifact)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            errors.push(format!(
                "could not remove {description} {}: {error}",
                artifact.display()
            ));
        }
    }
    if remove_parent
        && let Some(directory) = path.parent()
        && let Err(error) = fs::remove_dir(directory)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        errors.push(format!(
            "could not remove verification stage directory {}: {error}",
            directory.display()
        ));
    }
    errors
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
        let fingerprints = HashMap::from([(key, "a".repeat(64))]);
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

    fn snapshot(uidvalidity: u64, uidnext: u64, exists: u64) -> FolderSnapshot {
        FolderSnapshot {
            uidvalidity,
            uidnext,
            exists,
        }
    }

    fn page(uidvalidity: u64, uids: impl IntoIterator<Item = u64>) -> ExtractedMessages {
        uids.into_iter()
            .map(|uid| {
                (
                    MailboxMessageKey::with_uidvalidity("INBOX", uidvalidity, uid.to_string()),
                    ExtractedMessage {
                        message_id: Some(format!("<{uid}@example.test>")),
                        uid: Some(uid.to_string()),
                        size_bytes: Some(64),
                        internal_date: None,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn durable_stage_reopens_with_cursor_and_identity_binding() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        let folder = snapshot(42, 8, 1);
        let messages = page(42, [7]);
        let key = messages.keys().next().unwrap().clone();
        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
        let side = StagedMessageSide::Source;
        assert_eq!(stage.resume_mailbox(side, "INBOX", folder).unwrap(), None);
        stage
            .insert_messages_with_fingerprints(
                side,
                &messages,
                &HashMap::from([(key, "abc".into())]),
            )
            .unwrap();
        stage.checkpoint_page(side, "INBOX", folder, 7).unwrap();
        drop(stage);

        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
        assert_eq!(stage.count(side).unwrap(), 1);
        assert_eq!(stage.content_fingerprints(side).len(), 1);
        assert_eq!(
            stage.resume_mailbox(side, "INBOX", folder).unwrap(),
            Some(7)
        );
        stage.finish().unwrap();
        assert!(!path.exists());

        let mut stage = MessageMetadataStage::open_durable(path, "plan-b").unwrap();
        assert_eq!(stage.count(side).unwrap(), 0);
        assert_eq!(stage.resume_mailbox(side, "INBOX", folder).unwrap(), None);
        drop(stage);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn finish_attempts_every_artifact_and_retains_state_for_retry() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan").unwrap();
        let journal = PathBuf::from(format!("{}-journal", path.display()));
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        std::fs::create_dir(&journal).unwrap();
        std::fs::create_dir(&wal).unwrap();
        std::fs::create_dir(&shm).unwrap();

        let error = stage.finish().unwrap_err();
        assert!(error.contains("-journal"), "{error}");
        assert!(error.contains("-wal"), "{error}");
        assert!(error.contains("-shm"), "{error}");
        assert!(!path.exists(), "main database removal was still attempted");
        assert!(
            stage.path.is_some(),
            "failed cleanup state must be retained"
        );
        assert!(stage.connection.is_none(), "connection closed successfully");

        std::fs::remove_dir(journal).unwrap();
        std::fs::remove_dir(wal).unwrap();
        std::fs::remove_dir(shm).unwrap();
        stage.finish().unwrap();
        assert!(stage.path.is_none());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn resume_rebuilds_a_folder_whose_snapshot_changed_between_runs() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        let side = StagedMessageSide::Destination;
        let original = snapshot(42, 33, 32);
        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan").unwrap();
        assert_eq!(stage.resume_mailbox(side, "INBOX", original).unwrap(), None);
        let messages = page(42, 1..=32);
        let fingerprints = messages
            .keys()
            .map(|key| (key.clone(), "f".repeat(64)))
            .collect::<HashMap<_, _>>();
        stage
            .insert_messages_with_fingerprints(side, &messages, &fingerprints)
            .unwrap();
        stage.checkpoint_page(side, "INBOX", original, 32).unwrap();
        drop(stage);

        // Before the restart UID 10 is expunged and UID 33 delivered: EXISTS
        // and UIDVALIDITY are unchanged, only UIDNEXT reveals the change.
        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan").unwrap();
        let current = snapshot(42, 34, 32);
        assert_eq!(stage.resume_mailbox(side, "INBOX", current).unwrap(), None);
        assert_eq!(stage.count(side).unwrap(), 0);
        assert!(stage.content_fingerprints(side).is_empty());
        // The stale cursor cannot be advanced under the old snapshot.
        assert!(stage.checkpoint_page(side, "INBOX", original, 32).is_err());
        drop(stage);

        // Nor are discarded fingerprints resurrected from disk.
        let stage = MessageMetadataStage::open_durable(path, "plan").unwrap();
        assert!(stage.content_fingerprints(side).is_empty());
        drop(stage);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn completion_requires_staged_rows_to_equal_snapshot_exists() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        let side = StagedMessageSide::Source;
        let folder = snapshot(42, 4, 2);
        stage.resume_mailbox(side, "INBOX", folder).unwrap();
        stage.insert_messages(side, &page(42, [1, 2, 3])).unwrap();
        assert!(stage.complete_mailbox(side, "INBOX", folder, 3).is_err());
        stage.delete_mailbox(side, "INBOX").unwrap();
        stage.resume_mailbox(side, "INBOX", folder).unwrap();
        stage.insert_messages(side, &page(42, [1, 3])).unwrap();
        stage.complete_mailbox(side, "INBOX", folder, 3).unwrap();
        // An unchanged completed folder resumes past every staged UID.
        assert_eq!(
            stage.resume_mailbox(side, "INBOX", folder).unwrap(),
            Some(3)
        );
        assert_eq!(stage.count(side).unwrap(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn reopening_a_stage_repairs_permissive_file_modes() {
        use std::os::unix::fs::PermissionsExt;
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        drop(MessageMetadataStage::open_durable(path.clone(), "plan").unwrap());
        let journal = PathBuf::from(format!("{}-journal", path.display()));
        std::fs::write(&journal, b"").unwrap();
        for file in [&path, &journal] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
        }

        drop(MessageMetadataStage::open_durable(path.clone(), "plan").unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        if let Ok(metadata) = std::fs::metadata(&journal) {
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn legacy_stage_without_snapshot_cursors_is_discarded() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE staged_messages(side INTEGER NOT NULL, mailbox TEXT NOT NULL, match_mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uid TEXT NOT NULL, message_id TEXT, internal_date TEXT, date_key TEXT, size_bytes INTEGER, PRIMARY KEY(side,mailbox,uidvalidity,uid));
                     CREATE TABLE stage_metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);
                     INSERT INTO stage_metadata VALUES('identity','plan');
                     CREATE TABLE stage_fingerprints(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uid TEXT NOT NULL, fingerprint TEXT NOT NULL, PRIMARY KEY(side,mailbox,uidvalidity,uid));
                     CREATE TABLE stage_cursors(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, last_uid INTEGER NOT NULL, completed INTEGER NOT NULL, PRIMARY KEY(side,mailbox));
                     INSERT INTO staged_messages VALUES(0,'INBOX','INBOX',42,'7',NULL,NULL,NULL,1);
                     INSERT INTO stage_fingerprints VALUES(0,'INBOX',42,'7','abc');
                     INSERT INTO stage_cursors VALUES(0,'INBOX',42,7,1);",
                )
                .unwrap();
        }
        let mut stage = MessageMetadataStage::open_durable(path, "plan").unwrap();
        let side = StagedMessageSide::Source;
        assert_eq!(stage.count(side).unwrap(), 0);
        assert!(stage.content_fingerprints(side).is_empty());
        assert_eq!(
            stage
                .resume_mailbox(side, "INBOX", snapshot(42, 8, 1))
                .unwrap(),
            None
        );
        drop(stage);
        let _ = std::fs::remove_dir_all(directory);
    }
}
