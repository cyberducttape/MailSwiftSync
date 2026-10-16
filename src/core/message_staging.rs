//! Private SQLite staging for live metadata reconciliation.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::{ExtractedMessage, ExtractedMessages, MailboxMessageKey, sqlite_i64};

pub(crate) const STAGE_BATCH_SIZE: i64 = 512;
/// Keep a free-space reserve on the filesystem receiving SQLite stage pages.
/// This is a safety floor, not a claim that the remaining space is enough for
/// the full verification workload.
pub(crate) const STAGE_FREE_SPACE_RESERVE_BYTES: u64 = 512 * 1024 * 1024;

fn validate_stage_free_space(available: u64) -> Result<(), String> {
    if available < STAGE_FREE_SPACE_RESERVE_BYTES {
        return Err(crate::core::VerificationLimit::StagingDiskSpace.tag(format!(
            "verification staging stopped with {available} bytes available; it preserves a minimum free-space reserve of {STAGE_FREE_SPACE_RESERVE_BYTES} bytes"
        )));
    }
    Ok(())
}

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
#[cfg(windows)]
type FileIdentity = crate::windows_private::FileIdentity;
#[cfg(all(not(unix), not(windows)))]
type FileIdentity = ();

#[cfg(unix)]
fn path_identity(path: &std::path::Path) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn path_identity(path: &std::path::Path) -> std::io::Result<FileIdentity> {
    crate::windows_private::path_identity(path)
}

#[cfg(all(not(unix), not(windows)))]
fn path_identity(_: &std::path::Path) -> std::io::Result<FileIdentity> {
    Ok(())
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
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    {
        // Ownership is checked and the DACL replaced through the handle.
        crate::credentials::restrict_open_file_permissions(file)?;
        crate::windows_private::handle_identity(file)
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = metadata;
        Ok(())
    }
}

fn stage_file_options(create_new: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
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
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
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
    /// Present only when the server supplied a tracked CONDSTORE watermark.
    pub(crate) highest_modseq: Option<u64>,
}

impl FolderSnapshot {
    fn to_sql(self) -> rusqlite::Result<(i64, i64, i64, Option<i64>)> {
        Ok((
            sqlite_i64(self.uidvalidity)?,
            sqlite_i64(self.uidnext)?,
            sqlite_i64(self.exists)?,
            self.highest_modseq.map(sqlite_i64).transpose()?,
        ))
    }
}

/// One folder's verification cursor and how many rows it staged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FolderCursor {
    pub(crate) side: StagedMessageSide,
    pub(crate) mailbox: String,
    pub(crate) snapshot: FolderSnapshot,
    pub(crate) last_uid: u64,
    pub(crate) completed: bool,
    pub(crate) staged: u64,
    /// UTC timestamps for the interval in which this folder was inspected.
    /// A missing value is possible only for a legacy or incomplete stage.
    pub(crate) scan_started_at: Option<String>,
    pub(crate) scan_completed_at: Option<String>,
    /// Post-destination source cursor check timestamp, only after a matching
    /// read-only EXAMINE of this folder's snapshot.
    pub(crate) source_revalidated_at: Option<String>,
    /// Final destination cursor recheck timestamp, performed after the source
    /// recheck. This narrows—but cannot eliminate—the cross-server race.
    pub(crate) destination_revalidated_at: Option<String>,
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
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        options
            .open(&path)
            .map_err(|error| format!("could not create verification stage: {error}"))?;
        let connection = match Connection::open(&path) {
            Ok(connection) => connection,
            Err(error) => {
                let _ = fs::remove_file(&path);
                let _ = crate::credentials::remove_empty_secret_directory(&directory);
                return Err(format!("could not open verification stage: {error}"));
            }
        };
        let mut stage = Self {
            connection: Some(connection),
            path: Some(path),
            retain_on_drop: false,
            remove_parent_on_cleanup: true,
        };
        if let Err(error) = stage.initialize(false) {
            let path = stage.path.take();
            if let Some(connection) = stage.connection.take() {
                let _ = connection.close();
            }
            if let Some(path) = path {
                let _ = fs::remove_file(&path);
                if let Some(directory) = path.parent() {
                    let _ = crate::credentials::remove_empty_secret_directory(directory);
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
        };
        stage.initialize(false)?;
        Ok(stage)
    }

    /// Open the private staging database for one verification attempt.
    /// The caller removes it after verification has produced durable evidence;
    /// an interrupted attempt retains diagnostic/progress context, but every
    /// retry invalidates its persisted scan pages before fetching again.
    pub(crate) fn open_durable(path: PathBuf, identity: &str) -> Result<Self, String> {
        let parent = path
            .parent()
            .ok_or_else(|| "verification stage path has no parent directory".to_owned())?;
        crate::credentials::ensure_private_directory(parent)
            .map_err(|error| format!("could not secure verification stage directory: {error}"))?;
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && metadata.file_type().is_symlink()
        {
            return Err("verification stage path is a symbolic link".to_owned());
        }
        // `create_new` tells us atomically whether this invocation owns a new
        // file. A stage that existed before this attempt must survive every
        // schema/identity initialization error for recovery or inspection.
        let (file, created_new) = match stage_file_options(true).open(&path) {
            Ok(file) => (file, true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let file = stage_file_options(false).open(&path).map_err(|error| {
                    format!("could not open durable verification stage: {error}")
                })?;
                (file, false)
            }
            Err(error) => {
                return Err(format!(
                    "could not create durable verification stage: {error}"
                ));
            }
        };
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
        let opened_identity = path_identity(&path)
            .map_err(|error| format!("could not inspect durable verification stage: {error}"))?;
        if opened_identity != expected_identity {
            return Err("durable verification stage path changed while opening".to_owned());
        }
        drop(file);
        let mut stage = Self {
            connection: Some(connection),
            path: Some(path),
            retain_on_drop: true,
            remove_parent_on_cleanup: false,
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
            if let Some(connection) = stage.connection.take() {
                let _ = connection.close();
            }
            if created_new && let Some(path) = stage.path.take() {
                let _ = cleanup_stage_files(&path, false);
            }
            return Err(format!(
                "could not initialize durable verification stage: {error}"
            ));
        }
        Ok(stage)
    }

    #[allow(dead_code)]
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

    /// Remove a completed durable stage only after its evidence has been
    /// committed to the project ledger. Failed or interrupted verification
    /// must leave the stage available for a retry; a retry rescans folders.
    pub(crate) fn cleanup_durable_stage(path: &Path) -> Result<(), String> {
        let errors = cleanup_stage_files(path, false);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    /// The open stage connection. Using a stage after `finish` is a
    /// lifecycle error that must fail the verification, not the controller.
    fn connection_ref(&self) -> rusqlite::Result<&Connection> {
        self.connection.as_ref().ok_or_else(|| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
                Some("verification stage is closed".to_owned()),
            )
        })
    }

    fn initialize(&mut self, durable: bool) -> rusqlite::Result<()> {
        // Verification staging is private to one run, but backups and
        // recovery inspection can briefly contend for the file. Bound the
        // wait instead of turning ordinary lock contention into an immediate
        // unexplained verification failure. I/O errors, including SQLITE_FULL,
        // still propagate from the owning transaction and retain the stage.
        self.connection_ref()?
            .execute_batch("PRAGMA busy_timeout=5000;")?;
        if !durable {
            self.connection_ref()?
                .execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;")?;
        }
        // Durable stages intentionally leave SQLite's default rollback
        // journal and FULL synchronization untouched. This avoids a runtime
        // PRAGMA mutation that some packaged SQLite builds reject while still
        // retaining crash-safe defaults.
        self.connection_ref()?.execute_batch(
            "CREATE TABLE IF NOT EXISTS staged_messages(
                 side INTEGER NOT NULL CHECK(side IN (0,1)),
                 mailbox TEXT NOT NULL,
                 match_mailbox TEXT NOT NULL,
                 uidvalidity INTEGER NOT NULL CHECK(uidvalidity >= -1),
                 uid TEXT NOT NULL,
                 message_id TEXT,
                 identity_provenance TEXT NOT NULL DEFAULT 'missing',
                 internal_date TEXT,
                 date_key TEXT,
                 size_bytes INTEGER CHECK(size_bytes IS NULL OR size_bytes >= 0),
                 flags TEXT,
                 PRIMARY KEY(side,mailbox,uidvalidity,uid)
             );
             CREATE INDEX IF NOT EXISTS staged_messages_id ON staged_messages(side,message_id,mailbox,uidvalidity,uid);
             CREATE INDEX IF NOT EXISTS staged_messages_metadata ON staged_messages(side,date_key,size_bytes,mailbox,uidvalidity,uid);
             CREATE INDEX IF NOT EXISTS staged_messages_match_metadata ON staged_messages(side,match_mailbox,date_key,size_bytes,uidvalidity,uid);
             CREATE INDEX IF NOT EXISTS staged_messages_exact_source ON staged_messages(message_id,match_mailbox,date_key,size_bytes) WHERE side=0 AND message_id IS NOT NULL AND date_key IS NOT NULL AND size_bytes IS NOT NULL;
             CREATE INDEX IF NOT EXISTS staged_messages_exact_destination ON staged_messages(message_id,mailbox,date_key,size_bytes,uidvalidity,uid) WHERE side=1 AND message_id IS NOT NULL AND date_key IS NOT NULL AND size_bytes IS NOT NULL;
             DROP INDEX IF EXISTS staged_messages_uid_numeric;
             CREATE TABLE IF NOT EXISTS stage_metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS stage_fingerprints(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uid TEXT NOT NULL, fingerprint TEXT NOT NULL, PRIMARY KEY(side,mailbox,uidvalidity,uid));
             CREATE INDEX IF NOT EXISTS stage_fingerprints_content ON stage_fingerprints(side,fingerprint);
             CREATE TABLE IF NOT EXISTS stage_cursors(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uidnext INTEGER NOT NULL CHECK(uidnext >= 0), exists_count INTEGER NOT NULL CHECK(exists_count >= 0), highest_modseq INTEGER CHECK(highest_modseq IS NULL OR highest_modseq > 0), last_uid INTEGER NOT NULL CHECK(last_uid >= 0), completed INTEGER NOT NULL CHECK(completed IN (0,1)), scan_started_at TEXT, scan_completed_at TEXT, source_revalidated_at TEXT, destination_revalidated_at TEXT, PRIMARY KEY(side,mailbox));
             CREATE TABLE IF NOT EXISTS stage_permanent_flags(side INTEGER NOT NULL, mailbox TEXT NOT NULL, flags TEXT, PRIMARY KEY(side,mailbox));",
        )?;
        let staged_identity_provenance: bool = self.connection_ref()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('staged_messages') WHERE name='identity_provenance')",
            [],
            |row| row.get(0),
        )?;
        if !staged_identity_provenance {
            self.connection_ref()?.execute_batch(
                "DELETE FROM staged_messages;
                 DELETE FROM stage_fingerprints;
                 DELETE FROM stage_cursors;
                 ALTER TABLE staged_messages ADD COLUMN identity_provenance TEXT NOT NULL DEFAULT 'missing';",
            )?;
        }
        // Stages written before flags were staged would leave every retried
        // row without flags and silently shrink flag-verification coverage.
        // They are a disposable cache, so discard their rows and rescan.
        let staged_flags: bool = self.connection_ref()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('staged_messages') WHERE name='flags')",
            [],
            |row| row.get(0),
        )?;
        if !staged_flags {
            self.connection_ref()?.execute_batch(
                "DELETE FROM staged_messages;
                 DELETE FROM stage_fingerprints;
                 DELETE FROM stage_cursors;
                 ALTER TABLE staged_messages ADD COLUMN flags TEXT;",
            )?;
        }
        // Stages written before cursors recorded the full SELECT snapshot
        // cannot prove their pages still describe the server. They are a
        // disposable cache, so discard them and rescan.
        let snapshot_cursors: bool = self.connection_ref()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('stage_cursors') WHERE name='exists_count')",
            [],
            |row| row.get(0),
        )?;
        if !snapshot_cursors {
            self.connection_ref()?.execute_batch(
                "DROP TABLE stage_cursors;
                 DELETE FROM staged_messages;
                 DELETE FROM stage_fingerprints;
                 CREATE TABLE stage_cursors(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uidnext INTEGER NOT NULL CHECK(uidnext >= 0), exists_count INTEGER NOT NULL CHECK(exists_count >= 0), highest_modseq INTEGER CHECK(highest_modseq IS NULL OR highest_modseq > 0), last_uid INTEGER NOT NULL CHECK(last_uid >= 0), completed INTEGER NOT NULL CHECK(completed IN (0,1)), source_revalidated_at TEXT, destination_revalidated_at TEXT, PRIMARY KEY(side,mailbox));",
            )?;
        }
        for column in ["scan_started_at", "scan_completed_at"] {
            let present: bool = self.connection_ref()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('stage_cursors') WHERE name=?1)",
                [column],
                |row| row.get(0),
            )?;
            if !present {
                self.connection_ref()?.execute_batch(&format!(
                    "ALTER TABLE stage_cursors ADD COLUMN {column} TEXT;"
                ))?;
            }
        }
        let destination_revalidation_column: bool = self.connection_ref()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('stage_cursors') WHERE name='destination_revalidated_at')",
            [],
            |row| row.get(0),
        )?;
        if !destination_revalidation_column {
            self.connection_ref()?.execute_batch(
                "ALTER TABLE stage_cursors ADD COLUMN destination_revalidated_at TEXT;",
            )?;
        }
        let highest_modseq: bool = self.connection_ref()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('stage_cursors') WHERE name='highest_modseq')",
            [],
            |row| row.get(0),
        )?;
        if !highest_modseq {
            self.connection_ref()?.execute_batch(
                "ALTER TABLE stage_cursors ADD COLUMN highest_modseq INTEGER CHECK(highest_modseq IS NULL OR highest_modseq > 0);",
            )?;
        }
        let revalidation_column: bool = self.connection_ref()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('stage_cursors') WHERE name='source_revalidated_at')",
            [],
            |row| row.get(0),
        )?;
        if !revalidation_column {
            self.connection_ref()?.execute_batch(
                "ALTER TABLE stage_cursors ADD COLUMN source_revalidated_at TEXT;",
            )?;
        }
        if !durable {
            // Ephemeral stages are disposable and may use an in-memory temp
            // store. Durable stages must not mutate connection settings: the
            // packaged engine can expose a read-only SQLite wrapper.
            self.connection_ref()?
                .execute_batch("PRAGMA temp_store=MEMORY;")?;
        }
        Ok(())
    }

    fn bind_identity(&mut self, identity: &str) -> rusqlite::Result<()> {
        if identity.len() > 256 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let existing: Option<String> = self
            .connection_ref()?
            .query_row(
                "SELECT value FROM stage_metadata WHERE key='identity'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if existing.as_deref() != Some(identity) {
            let tx = self.connection_ref()?.unchecked_transaction()?;
            tx.execute("DELETE FROM staged_messages", [])?;
            tx.execute("DELETE FROM stage_fingerprints", [])?;
            tx.execute("DELETE FROM stage_cursors", [])?;
            tx.execute("DELETE FROM stage_permanent_flags", [])?;
            tx.execute("DELETE FROM stage_metadata", [])?;
            tx.execute(
                "INSERT INTO stage_metadata(key,value) VALUES('identity',?1)",
                [identity],
            )?;
            tx.commit()?;
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
        self.insert_page_with_fingerprints(side, messages, fingerprints, None)
    }

    pub(crate) fn insert_page_with_fingerprints(
        &mut self,
        side: StagedMessageSide,
        messages: &ExtractedMessages,
        fingerprints: &HashMap<MailboxMessageKey, String>,
        checkpoint: Option<(&str, FolderSnapshot, u64)>,
    ) -> Result<(), String> {
        self.ensure_free_space()?;
        if fingerprints.keys().any(|key| !messages.contains_key(key)) {
            return Err("body fingerprint did not have a matching staged message".to_owned());
        }
        let tx = self
            .connection()?
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        // HashMap iteration order is randomized. Persist each fetched page in
        // canonical key order so the staged verifier's row traversal and
        // duplicate-Message-ID pairing are repeatable.
        let mut ordered_messages = messages.iter().collect::<Vec<_>>();
        ordered_messages.sort_by(|(left, _), (right, _)| left.cmp(right));
        {
            let mut insert = tx.prepare_cached("INSERT OR REPLACE INTO staged_messages(side,mailbox,match_mailbox,uidvalidity,uid,message_id,identity_provenance,internal_date,date_key,size_bytes,flags) VALUES(?1,?2,?2,?3,?4,?5,?6,?7,?8,?9,?10)").map_err(|e| e.to_string())?;
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
                        message.identity_provenance().as_str(),
                        message.internal_date,
                        metadata_date_key(message.internal_date.as_deref()),
                        size,
                        message.flags
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
        if let Some((mailbox, snapshot, last_uid)) = checkpoint {
            let (uidvalidity, uidnext, exists, highest_modseq) =
                snapshot.to_sql().map_err(|error| error.to_string())?;
            let updated = tx
                .execute(
                    "UPDATE stage_cursors SET last_uid=?7,completed=0 WHERE side=?1 AND mailbox=?2 AND uidvalidity=?3 AND uidnext=?4 AND exists_count=?5 AND highest_modseq IS ?6",
                    params![
                        side.as_i64(),
                        mailbox,
                        uidvalidity,
                        uidnext,
                        exists,
                        highest_modseq,
                        sqlite_i64(last_uid).map_err(|error| error.to_string())?
                    ],
                )
                .map_err(|error| error.to_string())?;
            if updated != 1 {
                return Err(format!(
                    "folder {mailbox}: verification cursor does not belong to the current snapshot"
                ));
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Check the filesystem receiving staged rows before each page write.
    /// This high-watermark guard reduces the chance that verification exhausts
    /// the filesystem; it cannot reserve space against concurrent writers.
    pub(crate) fn ensure_free_space(&self) -> Result<(), String> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        let available = fs2::available_space(directory).map_err(|error| {
            crate::core::VerificationLimit::StagingDiskSpace.tag(format!(
                "could not determine free space for the verification stage: {error}"
            ))
        })?;
        validate_stage_free_space(available)
    }

    pub(crate) fn record_permanent_flags(
        &mut self,
        side: StagedMessageSide,
        mailbox: &str,
        permanent_flags: Option<&str>,
    ) -> Result<(), String> {
        self.connection()?
            .execute(
                "INSERT OR REPLACE INTO stage_permanent_flags(side,mailbox,flags) VALUES(?1,?2,?3)",
                params![side.as_i64(), mailbox, permanent_flags],
            )
            .map(|_| ())
            .map_err(|error| format!("could not stage PERMANENTFLAGS: {error}"))
    }

    pub(crate) fn delete_mailbox(
        &mut self,
        side: StagedMessageSide,
        mailbox: &str,
    ) -> Result<(), String> {
        let tx = self
            .connection()?
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        for table in [
            "staged_messages",
            "stage_fingerprints",
            "stage_cursors",
            "stage_permanent_flags",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE side=?1 AND mailbox=?2"),
                params![side.as_i64(), mailbox],
            )
            .map_err(|error| error.to_string())?;
        }
        tx.commit().map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Start a new folder scan, invalidating every row and fingerprint staged
    /// by an earlier scan even when the SELECT snapshot is unchanged.
    ///
    /// UIDVALIDITY, UIDNEXT, and EXISTS cannot prove that mutable FLAGS or
    /// previously captured body fingerprints are still valid after a process
    /// interruption. Cursors are recovery/progress metadata only; they never
    /// authorize reuse of fetched evidence. Clearing old state and recording
    /// the new snapshot happen atomically before any new page is accepted.
    pub(crate) fn begin_mailbox_scan(
        &mut self,
        side: StagedMessageSide,
        mailbox: &str,
        snapshot: FolderSnapshot,
    ) -> Result<(), String> {
        let (uidvalidity, uidnext, exists, highest_modseq) =
            snapshot.to_sql().map_err(|e| e.to_string())?;
        let tx = self
            .connection()?
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        for table in [
            "staged_messages",
            "stage_fingerprints",
            "stage_cursors",
            "stage_permanent_flags",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE side=?1 AND mailbox=?2"),
                params![side.as_i64(), mailbox],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute(
            "INSERT INTO stage_cursors(side,mailbox,uidvalidity,uidnext,exists_count,highest_modseq,last_uid,completed,scan_started_at) VALUES(?1,?2,?3,?4,?5,?6,0,0,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            params![side.as_i64(), mailbox, uidvalidity, uidnext, exists, highest_modseq],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
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
    /// survived and this scan cannot be trusted.
    /// Every folder's verification cursor with its staged row count. Folder
    /// names stay in process; callers persist only digests of them.
    pub(crate) fn folder_cursors(&self) -> rusqlite::Result<Vec<FolderCursor>> {
        let mut statement = self.connection_ref()?.prepare(
            "SELECT c.side,c.mailbox,c.uidvalidity,c.uidnext,c.exists_count,c.highest_modseq,c.last_uid,c.completed,(SELECT COUNT(*) FROM staged_messages m WHERE m.side=c.side AND m.mailbox=c.mailbox AND m.uidvalidity=c.uidvalidity),c.scan_started_at,c.scan_completed_at,c.source_revalidated_at,c.destination_revalidated_at FROM stage_cursors c ORDER BY c.side,c.mailbox",
        )?;
        statement
            .query_map([], |row| {
                let side = match row.get::<_, i64>(0)? {
                    0 => StagedMessageSide::Source,
                    1 => StagedMessageSide::Destination,
                    value => return Err(rusqlite::Error::IntegralValueOutOfRange(0, value)),
                };
                let unsigned = |index| -> rusqlite::Result<u64> {
                    let value: i64 = row.get(index)?;
                    u64::try_from(value)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
                };
                Ok(FolderCursor {
                    side,
                    mailbox: row.get(1)?,
                    snapshot: FolderSnapshot {
                        uidvalidity: unsigned(2)?,
                        uidnext: unsigned(3)?,
                        exists: unsigned(4)?,
                        highest_modseq: row
                            .get::<_, Option<i64>>(5)?
                            .map(|value| {
                                u64::try_from(value)
                                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, value))
                            })
                            .transpose()?,
                    },
                    last_uid: unsigned(6)?,
                    completed: row.get(7)?,
                    staged: unsigned(8)?,
                    scan_started_at: row.get(9)?,
                    scan_completed_at: row.get(10)?,
                    source_revalidated_at: row.get(11)?,
                    destination_revalidated_at: row.get(12)?,
                })
            })?
            .collect()
    }

    /// Persist the source-side revalidation times only after every expected
    /// source folder was reselected and matched its scanned snapshot.
    pub(crate) fn record_source_revalidation(
        &mut self,
        observations: &HashMap<String, String>,
    ) -> Result<(), String> {
        self.ensure_free_space()?;
        let expected = self
            .connection_ref()
            .map_err(|error| error.to_string())?
            .query_row(
                "SELECT COUNT(*) FROM stage_cursors WHERE side=0 AND completed=1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?;
        if expected <= 0 || usize::try_from(expected).ok() != Some(observations.len()) {
            return Err("source revalidation did not cover every completed source folder".into());
        }
        let tx = self
            .connection()?
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        for (mailbox, timestamp) in observations {
            let changed = tx
                .execute(
                    "UPDATE stage_cursors SET source_revalidated_at=?1 WHERE side=0 AND mailbox=?2 AND completed=1",
                    params![timestamp, mailbox],
                )
                .map_err(|error| error.to_string())?;
            if changed != 1 {
                return Err(format!(
                    "source revalidation observation has no completed source folder {mailbox}"
                ));
            }
        }
        tx.commit().map_err(|error| error.to_string())
    }

    /// Persist destination cursor rechecks after every completed destination
    /// folder was reselected and matched its scanned snapshot.
    pub(crate) fn record_destination_revalidation(
        &mut self,
        observations: &HashMap<String, String>,
    ) -> Result<(), String> {
        self.ensure_free_space()?;
        let expected = self
            .connection_ref()
            .map_err(|error| error.to_string())?
            .query_row(
                "SELECT COUNT(*) FROM stage_cursors WHERE side=1 AND completed=1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?;
        if expected <= 0 || usize::try_from(expected).ok() != Some(observations.len()) {
            return Err(
                "destination revalidation did not cover every completed destination folder".into(),
            );
        }
        let tx = self
            .connection()?
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        for (mailbox, timestamp) in observations {
            let changed = tx
                .execute(
                    "UPDATE stage_cursors SET destination_revalidated_at=?1 WHERE side=1 AND mailbox=?2 AND completed=1",
                    params![timestamp, mailbox],
                )
                .map_err(|error| error.to_string())?;
            if changed != 1 {
                return Err(format!(
                    "destination revalidation observation has no completed destination folder {mailbox}"
                ));
            }
        }
        tx.commit().map_err(|error| error.to_string())
    }

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
        let (uidvalidity, uidnext, exists, highest_modseq) =
            snapshot.to_sql().map_err(|e| e.to_string())?;
        let updated = self
            .connection()?
            .execute(
                "UPDATE stage_cursors SET last_uid=?7,completed=?8,scan_completed_at=CASE WHEN ?8=1 THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') ELSE scan_completed_at END WHERE side=?1 AND mailbox=?2 AND uidvalidity=?3 AND uidnext=?4 AND exists_count=?5 AND highest_modseq IS ?6",
                params![
                    side.as_i64(),
                    mailbox,
                    uidvalidity,
                    uidnext,
                    exists,
                    highest_modseq,
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
        // Every intermediate the reconciler creates. An ephemeral stage runs
        // with journal_mode=OFF, where an aborted pass cannot roll its DDL
        // back, so a leftover table would make the next pass fail on CREATE.
        self.connection()?
            .execute_batch(
                "DROP TABLE IF EXISTS staged_matched;
                 DROP TABLE IF EXISTS staged_folder_mapping;
                 DROP TABLE IF EXISTS staged_fingerprint_buckets;
                 DROP TABLE IF EXISTS staged_exact_source_ranked;
                 DROP TABLE IF EXISTS staged_exact_destination_ranked;
                 DROP TABLE IF EXISTS staged_exact_pairs;
                 DROP TABLE IF EXISTS staged_changed_source_ranked;
                 DROP TABLE IF EXISTS staged_changed_destination_ranked;
                 DROP TABLE IF EXISTS staged_changed_pairs;
                 DROP TABLE IF EXISTS staged_probable_source;
                 DROP TABLE IF EXISTS staged_probable_destination;
                 DROP TABLE IF EXISTS staged_probable_pairs;
                 DROP TABLE IF EXISTS staged_duplicate_ids;
                 DROP TABLE IF EXISTS staged_mismatches;",
            )
            .map_err(|error| error.to_string())
    }

    /// Start a new verification attempt. Reconciliation artifacts and
    /// persisted body proofs never cross attempt boundaries. Persisted folder
    /// rows are pruned against each endpoint's fresh LIST and then invalidated
    /// folder-by-folder before refetch, avoiding a mailbox-wide delete journal.
    pub(crate) fn begin_verification_attempt(&mut self) -> Result<(), String> {
        self.reset_reconciliation()?;
        self.connection()?
            .execute("DELETE FROM stage_fingerprints", [])
            .map_err(|error| format!("could not invalidate prior body fingerprints: {error}"))?;
        Ok(())
    }

    /// Keep staged data only for selectable folders in the current endpoint
    /// inventory. All retained folders are still fully invalidated by
    /// `begin_mailbox_scan` before they are fetched; this removes only orphaned
    /// folders without journaling a second account-wide deletion.
    pub(crate) fn retain_mailboxes(
        &mut self,
        side: StagedMessageSide,
        current_mailboxes: &HashSet<String>,
    ) -> Result<(), String> {
        let tx = self
            .connection()?
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS stage_current_mailboxes(
                mailbox TEXT PRIMARY KEY
             ) WITHOUT ROWID;
             DELETE FROM stage_current_mailboxes;",
        )
        .map_err(|error| format!("could not prepare current folder inventory: {error}"))?;
        {
            let mut insert = tx
                .prepare_cached("INSERT INTO stage_current_mailboxes(mailbox) VALUES(?1)")
                .map_err(|error| format!("could not prepare current folder inventory: {error}"))?;
            for mailbox in current_mailboxes {
                insert.execute([mailbox]).map_err(|error| {
                    format!("could not stage current folder inventory: {error}")
                })?;
            }
        }
        for table in [
            "staged_messages",
            "stage_fingerprints",
            "stage_cursors",
            "stage_permanent_flags",
        ] {
            tx.execute(
                &format!(
                    "DELETE FROM {table} WHERE side=?1 AND mailbox NOT IN \
                     (SELECT mailbox FROM stage_current_mailboxes)"
                ),
                [side.as_i64()],
            )
            .map_err(|error| format!("could not prune stale verification folders: {error}"))?;
        }
        tx.commit()
            .map_err(|error| format!("could not commit verification folder inventory: {error}"))?;
        Ok(())
    }

    /// Distinct folder names referenced by the staged mismatches (bounded by
    /// the account's folder count), for this session's drill-down labels.
    pub(crate) fn staged_mismatch_folders(&self) -> Result<Vec<String>, String> {
        let connection = self.connection_ref().map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT source_folder FROM staged_mismatches WHERE source_folder IS NOT NULL
                 UNION SELECT destination_folder FROM staged_mismatches WHERE destination_folder IS NOT NULL",
            )
            .map_err(|error| format!("could not read staged mismatch folders: {error}"))?;
        let folders = statement
            .query_map([], |row| row.get(0))
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<String>>>())
            .map_err(|error| format!("could not read staged mismatch folders: {error}"))?;
        Ok(folders)
    }

    pub(crate) fn count(&self, side: StagedMessageSide) -> rusqlite::Result<u64> {
        self.connection_ref()?.query_row(
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
        self.connection_ref()?.query_row(
            "SELECT COUNT(*) FROM staged_messages WHERE side=?1 AND mailbox=?2 AND uidvalidity=?3",
            params![side.as_i64(), mailbox, sqlite_i64(uidvalidity)?],
            |row| {
                let value: i64 = row.get(0)?;
                u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
            },
        )
    }

    pub(crate) fn sum_bytes(&self, side: StagedMessageSide) -> rusqlite::Result<u64> {
        self.connection_ref()?.query_row(
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
        // Walk the rowid range directly. With a `side` index SQLite read and
        // sorted the whole side for every 512-row page, so draining many
        // unmatched rows grew with the square of their number.
        let sql = format!(
            "SELECT rowid,mailbox,uidvalidity,uid,message_id,internal_date,date_key,size_bytes FROM staged_messages NOT INDEXED WHERE side=?1 AND rowid>?2 AND NOT EXISTS(SELECT 1 FROM staged_matched m WHERE m.side=staged_messages.side AND m.mailbox=staged_messages.mailbox AND m.uidvalidity=staged_messages.uidvalidity AND m.uid=staged_messages.uid){condition_id}{condition_metadata} ORDER BY rowid LIMIT {}",
            super::message_staging::STAGE_BATCH_SIZE
        );
        super::stage_sql::prepare(self.connection_ref()?, &sql)?
            .query_map(params![side.as_i64(), after_rowid], staged_message_from_row)?
            .collect()
    }

    pub(crate) fn connection(&self) -> Result<&Connection, String> {
        self.connection_ref().map_err(|error| error.to_string())
    }

    pub(crate) fn is_durable(&self) -> bool {
        self.retain_on_drop
    }

    pub(crate) fn try_content_fingerprints(
        &self,
        side: StagedMessageSide,
    ) -> Result<HashMap<MailboxMessageKey, String>, String> {
        let mut statement = self
            .connection_ref()
            .map_err(|error| error.to_string())?
            .prepare(
                "SELECT mailbox,uidvalidity,uid,fingerprint FROM stage_fingerprints WHERE side=?1 ORDER BY rowid",
            )
            .map_err(|error| format!("could not query staged content fingerprints: {error}"))?;
        let mut fingerprints = HashMap::new();
        let rows = statement
            .query_map([side.as_i64()], |row| {
                let uidvalidity: i64 = row.get(1)?;
                let uidvalidity =
                    if uidvalidity >= 0 {
                        Some(u64::try_from(uidvalidity).map_err(|_| {
                            rusqlite::Error::IntegralValueOutOfRange(1, uidvalidity)
                        })?)
                    } else {
                        None
                    };
                Ok((
                    MailboxMessageKey::with_shared_mailbox(
                        std::sync::Arc::from(row.get::<_, String>(0)?),
                        uidvalidity,
                        row.get::<_, String>(2)?,
                    ),
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|error| format!("could not read staged content fingerprints: {error}"))?;
        for row in rows {
            let (key, fingerprint) = row
                .map_err(|error| format!("could not decode staged content fingerprint: {error}"))?;
            fingerprints.insert(key, fingerprint);
        }
        Ok(fingerprints)
    }

    #[cfg(test)]
    pub(crate) fn content_fingerprints(
        &self,
        side: StagedMessageSide,
    ) -> HashMap<MailboxMessageKey, String> {
        self.try_content_fingerprints(side)
            .expect("test stage query")
    }

    pub(crate) fn all_messages(
        &self,
        side: StagedMessageSide,
    ) -> rusqlite::Result<ExtractedMessages> {
        let mut statement = self.connection_ref()?.prepare(
            "SELECT mailbox,uidvalidity,uid,message_id,internal_date,size_bytes,flags FROM staged_messages WHERE side=?1 ORDER BY rowid",
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
                    flags: row.get(6)?,
                },
            ))
        })?;
        rows.collect()
    }
}

pub(crate) fn staged_message_from_row(row: &Row<'_>) -> rusqlite::Result<StagedMessage> {
    staged_message_from_row_at(row, 0)
}

pub(crate) fn staged_message_pair_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<(StagedMessage, StagedMessage)> {
    Ok((
        staged_message_from_row_at(row, 0)?,
        staged_message_from_row_at(row, 8)?,
    ))
}

fn staged_message_from_row_at(row: &Row<'_>, offset: usize) -> rusqlite::Result<StagedMessage> {
    let uidvalidity: i64 = row.get(offset + 2)?;
    let uid: String = row.get(offset + 3)?;
    let uidvalidity = if uidvalidity >= 0 {
        Some(u64::try_from(uidvalidity).map_err(|_| {
            rusqlite::Error::IntegralValueOutOfRange((offset + 2) as _, uidvalidity)
        })?)
    } else {
        None
    };
    let size_bytes = row
        .get::<_, Option<i64>>(offset + 7)?
        .map(|value| {
            u64::try_from(value)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange((offset + 7) as _, value))
        })
        .transpose()?;
    Ok(StagedMessage {
        rowid: row.get(offset)?,
        key: MailboxMessageKey::with_shared_mailbox(
            std::sync::Arc::from(row.get::<_, String>(offset + 1)?),
            uidvalidity,
            uid.clone(),
        ),
        message: ExtractedMessage {
            message_id: row.get(offset + 4)?,
            uid: Some(uid),
            size_bytes,
            internal_date: row.get(offset + 5)?,
            // Reconciliation never reads flags; flag verification is a
            // separate pass over the stage (`verify_staged_flags`).
            flags: None,
        },
        date_key: row.get(offset + 6)?,
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
        && let Err(error) = crate::credentials::remove_empty_secret_directory(directory)
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
    fn stage_free_space_guard_preserves_the_configured_floor() {
        assert!(validate_stage_free_space(STAGE_FREE_SPACE_RESERVE_BYTES).is_ok());
        let error = validate_stage_free_space(STAGE_FREE_SPACE_RESERVE_BYTES - 1).unwrap_err();
        assert_eq!(
            crate::core::VerificationLimit::from_detail(&error),
            Some(crate::core::VerificationLimit::StagingDiskSpace)
        );
    }

    #[test]
    fn destination_revalidation_is_persisted_only_for_the_destination_cursor() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        let empty_snapshot = snapshot(42, 1, 0);
        stage
            .begin_mailbox_scan(StagedMessageSide::Source, "INBOX", empty_snapshot)
            .unwrap();
        stage
            .complete_mailbox(StagedMessageSide::Source, "INBOX", empty_snapshot, 0)
            .unwrap();
        stage
            .begin_mailbox_scan(StagedMessageSide::Destination, "INBOX", empty_snapshot)
            .unwrap();
        stage
            .complete_mailbox(StagedMessageSide::Destination, "INBOX", empty_snapshot, 0)
            .unwrap();

        stage
            .record_destination_revalidation(&HashMap::from([(
                "INBOX".to_owned(),
                "2026-10-14T10:05:00.000Z".to_owned(),
            )]))
            .unwrap();
        let cursors = stage.folder_cursors().unwrap();
        let source = cursors
            .iter()
            .find(|cursor| cursor.side == StagedMessageSide::Source)
            .unwrap();
        let destination = cursors
            .iter()
            .find(|cursor| cursor.side == StagedMessageSide::Destination)
            .unwrap();
        assert_eq!(source.destination_revalidated_at, None);
        assert_eq!(
            destination.destination_revalidated_at.as_deref(),
            Some("2026-10-14T10:05:00.000Z")
        );
    }

    #[test]
    fn final_source_revalidation_replaces_the_earlier_observation_time() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        let empty_snapshot = snapshot(42, 1, 0);
        stage
            .begin_mailbox_scan(StagedMessageSide::Source, "INBOX", empty_snapshot)
            .unwrap();
        stage
            .complete_mailbox(StagedMessageSide::Source, "INBOX", empty_snapshot, 0)
            .unwrap();

        stage
            .record_source_revalidation(&HashMap::from([(
                "INBOX".to_owned(),
                "2026-10-14T10:05:00.000Z".to_owned(),
            )]))
            .unwrap();
        stage
            .record_source_revalidation(&HashMap::from([(
                "INBOX".to_owned(),
                "2026-10-14T10:06:00.000Z".to_owned(),
            )]))
            .unwrap();

        let source = stage
            .folder_cursors()
            .unwrap()
            .into_iter()
            .find(|cursor| cursor.side == StagedMessageSide::Source)
            .unwrap();
        assert_eq!(
            source.source_revalidated_at.as_deref(),
            Some("2026-10-14T10:06:00.000Z")
        );
    }

    #[test]
    fn stage_file_replaced_after_validation_is_detected() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        let replacement = directory.join("replacement.sqlite");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let secured = secure_stage_file(&file).unwrap();
        assert_eq!(secured, path_identity(&path).unwrap());
        drop(file);
        std::fs::write(&replacement, b"replacement").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_ne!(secured, path_identity(&path).unwrap());
        std::fs::remove_dir_all(directory).unwrap();
    }

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
                flags: None,
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
        stage.connection().unwrap().execute_batch("CREATE TABLE staged_matched(side INTEGER,mailbox TEXT,uidvalidity INTEGER,uid TEXT,PRIMARY KEY(side,mailbox,uidvalidity,uid));").unwrap();
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
            highest_modseq: None,
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
                        flags: None,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn staging_configures_a_bounded_sqlite_busy_timeout() {
        let stage = MessageMetadataStage::open_in_memory().unwrap();
        let timeout: i64 = stage
            .connection_ref()
            .unwrap()
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
    }

    #[test]
    fn beginning_a_fresh_folder_scan_invalidates_old_permanent_flags() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        let side = StagedMessageSide::Source;
        stage
            .record_permanent_flags(side, "INBOX", Some("\\Seen \\*"))
            .unwrap();

        stage
            .begin_mailbox_scan(side, "INBOX", snapshot(42, 8, 0))
            .unwrap();
        let rows: i64 = stage
            .connection_ref()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM stage_permanent_flags WHERE side=?1 AND mailbox='INBOX'",
                [side.as_i64()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);

        stage
            .record_permanent_flags(side, "INBOX", Some("\\Seen \\*"))
            .unwrap();
        let flags: Option<String> = stage
            .connection_ref()
            .unwrap()
            .query_row(
                "SELECT flags FROM stage_permanent_flags WHERE side=?1 AND mailbox='INBOX'",
                [side.as_i64()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(flags.as_deref(), Some("\\Seen \\*"));
    }

    #[test]
    fn durable_stage_reopen_invalidates_rows_and_content_fingerprints() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        let folder = snapshot(42, 8, 1);
        let messages = page(42, [7]);
        let key = messages.keys().next().unwrap().clone();
        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
        let side = StagedMessageSide::Source;
        stage.begin_mailbox_scan(side, "INBOX", folder).unwrap();
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
        // Opening a retry retains disk-backed metadata only as inert state:
        // body fingerprints are not reloaded and no old cursor is used to
        // skip a page. The fresh inventory/scan performs invalidation.
        stage.begin_verification_attempt().unwrap();
        assert_eq!(stage.count(side).unwrap(), 1);
        assert!(stage.content_fingerprints(side).is_empty());
        stage.finish().unwrap();
        assert!(!path.exists());

        let mut stage = MessageMetadataStage::open_durable(path, "plan-b").unwrap();
        assert_eq!(stage.count(side).unwrap(), 0);
        stage.begin_mailbox_scan(side, "INBOX", folder).unwrap();
        drop(stage);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn initialization_failure_preserves_a_preexisting_stage_for_recovery() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        let side = StagedMessageSide::Source;
        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan-a").unwrap();
        stage
            .begin_mailbox_scan(side, "INBOX", snapshot(42, 8, 1))
            .unwrap();
        stage.insert_messages(side, &page(42, [7])).unwrap();
        drop(stage);

        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_identity_rebind
                 BEFORE INSERT ON stage_metadata
                 BEGIN SELECT RAISE(FAIL, 'injected identity binding failure'); END;",
            )
            .unwrap();
        drop(connection);

        let error = MessageMetadataStage::open_durable(path.clone(), "plan-b")
            .err()
            .expect("injected identity failure should reject the stage");
        assert!(error.contains("identity binding"));
        assert!(path.exists(), "failed reopen must preserve the old stage");

        let connection = Connection::open(&path).unwrap();
        let identity: String = connection
            .query_row(
                "SELECT value FROM stage_metadata WHERE key='identity'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM staged_messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(identity, "plan-a");
        assert_eq!(rows, 1);
        drop(connection);
        let _ = cleanup_stage_files(&path, true);
    }

    #[test]
    fn page_rows_and_progress_cursor_roll_back_together() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        let mut stage = MessageMetadataStage::open_durable(path, "plan").unwrap();
        let side = StagedMessageSide::Source;
        let folder = snapshot(42, 8, 1);
        stage.begin_mailbox_scan(side, "INBOX", folder).unwrap();
        stage
            .connection_ref()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_page_cursor
                 BEFORE UPDATE ON stage_cursors
                 BEGIN SELECT RAISE(ROLLBACK, 'injected cursor failure'); END;",
            )
            .unwrap();

        let error = stage
            .insert_page_with_fingerprints(
                side,
                &page(42, [7]),
                &HashMap::new(),
                Some(("INBOX", folder, 7)),
            )
            .unwrap_err();
        assert!(error.contains("injected cursor failure"));
        assert_eq!(stage.count_mailbox(side, "INBOX", 42).unwrap(), 0);
        assert_eq!(stage.folder_cursors().unwrap()[0].last_uid, 0);
        stage.finish().unwrap();
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn sqlite_full_rolls_back_page_and_retains_durable_stage_for_recovery() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        let folder = snapshot(42, 2, 1);
        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan").unwrap();
        stage
            .begin_mailbox_scan(StagedMessageSide::Source, "INBOX", folder)
            .unwrap();

        let connection = stage.connection_ref().unwrap();
        let pages: i64 = connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        let maximum: i64 = connection
            .query_row(&format!("PRAGMA max_page_count={pages}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(maximum, pages);

        let oversized = ExtractedMessages::from([(
            MailboxMessageKey::with_uidvalidity("INBOX", 42, "1"),
            ExtractedMessage {
                // Force a real SQLite page-allocation failure without relying
                // on the host filesystem's free-space state.
                message_id: Some("x".repeat(1024 * 1024)),
                uid: Some("1".into()),
                size_bytes: Some(64),
                internal_date: None,
                flags: None,
            },
        )]);
        let error = stage
            .insert_messages(StagedMessageSide::Source, &oversized)
            .unwrap_err();
        assert!(
            error.contains("database or disk is full"),
            "expected SQLITE_FULL, got {error}"
        );
        assert_eq!(stage.count(StagedMessageSide::Source).unwrap(), 0);
        drop(stage);
        assert!(
            path.exists(),
            "failed stage was deleted instead of retained"
        );

        let mut recovered = MessageMetadataStage::open_durable(path.clone(), "plan").unwrap();
        assert_eq!(recovered.count(StagedMessageSide::Source).unwrap(), 0);
        recovered
            .connection_ref()
            .unwrap()
            .query_row("PRAGMA max_page_count=2147483646", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        let recovered_page = page(42, [1]);
        recovered
            .insert_messages(StagedMessageSide::Source, &recovered_page)
            .unwrap();
        assert_eq!(recovered.count(StagedMessageSide::Source).unwrap(), 1);
        recovered.finish().unwrap();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn fresh_inventory_prunes_retired_folders_and_live_scans_replace_stale_state() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        let snapshot = snapshot(42, 8, 1);
        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan").unwrap();
        for side in [StagedMessageSide::Source, StagedMessageSide::Destination] {
            let retired = ExtractedMessages::from([(
                MailboxMessageKey::with_uidvalidity("Retired", 42, "7"),
                ExtractedMessage {
                    message_id: Some("<retired@example.test>".into()),
                    uid: Some("7".into()),
                    size_bytes: Some(64),
                    internal_date: Some("01-Jan-2024 00:00:00 +0000".into()),
                    flags: Some("\\Seen".into()),
                },
            )]);
            let key = retired.keys().next().unwrap().clone();
            stage.begin_mailbox_scan(side, "Retired", snapshot).unwrap();
            stage
                .insert_messages_with_fingerprints(
                    side,
                    &retired,
                    &HashMap::from([(key, "a".repeat(64))]),
                )
                .unwrap();
            stage
                .record_permanent_flags(side, "Retired", Some("\\Seen \\*"))
                .unwrap();
            stage
                .complete_mailbox(side, "Retired", snapshot, 7)
                .unwrap();
        }
        drop(stage);

        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan").unwrap();
        stage.begin_verification_attempt().unwrap();
        assert_eq!(stage.count(StagedMessageSide::Source).unwrap(), 1);
        assert_eq!(stage.count(StagedMessageSide::Destination).unwrap(), 1);
        assert!(
            stage
                .content_fingerprints(StagedMessageSide::Source)
                .is_empty()
        );
        let current_inventory = HashSet::from(["INBOX".to_owned()]);
        for side in [StagedMessageSide::Source, StagedMessageSide::Destination] {
            stage.retain_mailboxes(side, &current_inventory).unwrap();
        }
        assert_eq!(stage.count(StagedMessageSide::Source).unwrap(), 0);
        assert_eq!(stage.count(StagedMessageSide::Destination).unwrap(), 0);
        assert!(stage.folder_cursors().unwrap().is_empty());
        let fingerprint_rows: i64 = stage
            .connection_ref()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM stage_fingerprints", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(fingerprint_rows, 0);
        let permanent_flag_rows: i64 = stage
            .connection_ref()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM stage_permanent_flags", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(permanent_flag_rows, 0);

        // The replacement inventory contains only INBOX. A subsequent scan
        // must therefore reconcile only current data, not the retired folder.
        for side in [StagedMessageSide::Source, StagedMessageSide::Destination] {
            let current = page(42, [7]);
            stage.begin_mailbox_scan(side, "INBOX", snapshot).unwrap();
            stage.insert_messages(side, &current).unwrap();
            stage.complete_mailbox(side, "INBOX", snapshot, 7).unwrap();
            let messages = stage.all_messages(side).unwrap();
            assert_eq!(messages.len(), 1);
            assert!(messages.keys().all(|key| key.mailbox.as_ref() == "INBOX"));
        }
        stage.finish().unwrap();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn folder_cursors_report_snapshot_progress_and_staged_rows() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        let source = StagedMessageSide::Source;
        let destination = StagedMessageSide::Destination;
        let inbox = FolderSnapshot {
            highest_modseq: Some(17),
            ..snapshot(42, 9, 2)
        };
        stage.begin_mailbox_scan(source, "INBOX", inbox).unwrap();
        stage.insert_messages(source, &page(42, [7, 8])).unwrap();
        stage.complete_mailbox(source, "INBOX", inbox, 8).unwrap();
        let archive = snapshot(5, 30, 3);
        stage
            .begin_mailbox_scan(destination, "Archive", archive)
            .unwrap();
        let archive_page = page(5, [10])
            .into_values()
            .map(|message| {
                (
                    MailboxMessageKey::with_uidvalidity("Archive", 5, "10".to_owned()),
                    message,
                )
            })
            .collect();
        stage.insert_messages(destination, &archive_page).unwrap();
        stage
            .checkpoint_page(destination, "Archive", archive, 10)
            .unwrap();

        let cursors = stage.folder_cursors().unwrap();
        assert_eq!(cursors.len(), 2);
        assert_eq!(cursors[0].snapshot.highest_modseq, Some(17));
        assert_eq!(
            (
                cursors[0].side,
                cursors[0].mailbox.as_str(),
                cursors[0].snapshot,
                cursors[0].last_uid,
                cursors[0].completed,
                cursors[0].staged,
            ),
            (source, "INBOX", inbox, 8, true, 2)
        );
        let started = cursors[0].scan_started_at.as_deref().unwrap();
        let completed = cursors[0].scan_completed_at.as_deref().unwrap();
        assert!(started.ends_with('Z') && started.contains('T'));
        assert!(completed.ends_with('Z') && completed.contains('T'));
        assert_eq!(
            (
                cursors[1].side,
                cursors[1].mailbox.as_str(),
                cursors[1].snapshot,
                cursors[1].last_uid,
                cursors[1].completed,
                cursors[1].staged,
            ),
            (destination, "Archive", archive, 10, false, 1)
        );
        assert!(cursors[1].scan_started_at.is_some());
        assert_eq!(cursors[1].scan_completed_at, None);
    }

    #[test]
    fn durable_stage_adds_modseq_to_existing_cursor_schema() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        stage
            .connection_ref()
            .unwrap()
            .execute("ALTER TABLE stage_cursors DROP COLUMN highest_modseq", [])
            .unwrap();
        stage.initialize(true).unwrap();
        let has_modseq: bool = stage
            .connection_ref()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('stage_cursors') WHERE name='highest_modseq')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_modseq);
    }

    #[test]
    fn using_a_finished_stage_is_an_error_not_a_panic() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        stage.finish().unwrap();
        let error = stage.all_messages(StagedMessageSide::Source).unwrap_err();
        assert!(
            error.to_string().contains("verification stage is closed"),
            "{error}"
        );
        assert!(
            stage
                .insert_messages(StagedMessageSide::Source, &ExtractedMessages::new())
                .is_err()
        );
        assert!(stage.connection().is_err());
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
    fn new_scan_rebuilds_a_folder_whose_snapshot_changed_between_runs() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        let side = StagedMessageSide::Destination;
        let original = snapshot(42, 33, 32);
        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan").unwrap();
        stage.begin_mailbox_scan(side, "INBOX", original).unwrap();
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
        stage.begin_mailbox_scan(side, "INBOX", current).unwrap();
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
    fn restart_discards_checkpointed_and_uncheckpointed_rows_and_fingerprints() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        let side = StagedMessageSide::Source;
        let folder = snapshot(42, 33, 32);
        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan").unwrap();
        stage.begin_mailbox_scan(side, "INBOX", folder).unwrap();
        let checkpointed = page(42, 1..=16);
        let checkpointed_fingerprints = checkpointed
            .keys()
            .map(|key| (key.clone(), format!("fingerprint-{}", key.uid)))
            .collect::<HashMap<_, _>>();
        stage
            .insert_messages_with_fingerprints(side, &checkpointed, &checkpointed_fingerprints)
            .unwrap();
        stage.checkpoint_page(side, "INBOX", folder, 16).unwrap();

        // Simulate a crash after the next page's rows are committed but before
        // its cursor checkpoint reaches disk.
        let uncheckpointed = page(42, 17..=32);
        let uncheckpointed_fingerprints = uncheckpointed
            .keys()
            .map(|key| (key.clone(), format!("fingerprint-{}", key.uid)))
            .collect::<HashMap<_, _>>();
        stage
            .insert_messages_with_fingerprints(side, &uncheckpointed, &uncheckpointed_fingerprints)
            .unwrap();
        assert_eq!(stage.count(side).unwrap(), 32);
        assert_eq!(stage.content_fingerprints(side).len(), 32);
        drop(stage);

        let mut stage = MessageMetadataStage::open_durable(path.clone(), "plan").unwrap();
        stage.begin_mailbox_scan(side, "INBOX", folder).unwrap();
        assert_eq!(stage.count(side).unwrap(), 0);
        assert!(stage.content_fingerprints(side).is_empty());
        let refreshed = page(42, 1..=32);
        let refreshed_fingerprints = refreshed
            .keys()
            .map(|key| (key.clone(), format!("refreshed-{}", key.uid)))
            .collect::<HashMap<_, _>>();
        stage
            .insert_messages_with_fingerprints(side, &refreshed, &refreshed_fingerprints)
            .unwrap();
        assert_eq!(stage.count(side).unwrap(), 32);
        assert_eq!(stage.content_fingerprints(side), refreshed_fingerprints);
        stage.finish().unwrap();
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn completion_requires_staged_rows_to_equal_snapshot_exists() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        let side = StagedMessageSide::Source;
        let folder = snapshot(42, 4, 2);
        stage.begin_mailbox_scan(side, "INBOX", folder).unwrap();
        stage.insert_messages(side, &page(42, [1, 2, 3])).unwrap();
        assert!(stage.complete_mailbox(side, "INBOX", folder, 3).is_err());
        stage.delete_mailbox(side, "INBOX").unwrap();
        stage.begin_mailbox_scan(side, "INBOX", folder).unwrap();
        stage.insert_messages(side, &page(42, [1, 3])).unwrap();
        stage.complete_mailbox(side, "INBOX", folder, 3).unwrap();
        // A repeated scan invalidates even a complete unchanged folder.
        stage.begin_mailbox_scan(side, "INBOX", folder).unwrap();
        assert_eq!(stage.count(side).unwrap(), 0);
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
        stage
            .begin_mailbox_scan(side, "INBOX", snapshot(42, 8, 1))
            .unwrap();
        drop(stage);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn stage_without_staged_flags_is_discarded_for_a_full_rescan() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let path = directory.join("verification.sqlite");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE staged_messages(side INTEGER NOT NULL, mailbox TEXT NOT NULL, match_mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uid TEXT NOT NULL, message_id TEXT, internal_date TEXT, date_key TEXT, size_bytes INTEGER, PRIMARY KEY(side,mailbox,uidvalidity,uid));
                     CREATE TABLE stage_metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);
                     INSERT INTO stage_metadata VALUES('identity','plan');
                     CREATE TABLE stage_cursors(side INTEGER NOT NULL, mailbox TEXT NOT NULL, uidvalidity INTEGER NOT NULL, uidnext INTEGER NOT NULL, exists_count INTEGER NOT NULL, last_uid INTEGER NOT NULL, completed INTEGER NOT NULL, PRIMARY KEY(side,mailbox));
                     INSERT INTO staged_messages VALUES(0,'INBOX','INBOX',42,'7','<a@x>',NULL,NULL,1);
                     INSERT INTO stage_cursors VALUES(0,'INBOX',42,8,1,7,1);",
                )
                .unwrap();
        }
        let mut stage = MessageMetadataStage::open_durable(path, "plan").unwrap();
        let side = StagedMessageSide::Source;
        // A legacy staged row without flags is invalidated before scanning,
        // so it cannot silently shrink flag-verification coverage.
        assert_eq!(stage.count(side).unwrap(), 0);
        stage
            .begin_mailbox_scan(side, "INBOX", snapshot(42, 8, 1))
            .unwrap();
        let mut page = page(42, [7]);
        page.values_mut()
            .for_each(|message| message.flags = Some("\\Seen".into()));
        stage.insert_messages(side, &page).unwrap();
        assert_eq!(
            stage
                .all_messages(side)
                .unwrap()
                .values()
                .next()
                .unwrap()
                .flags
                .as_deref(),
            Some("\\Seen")
        );
        drop(stage);
        let _ = std::fs::remove_dir_all(directory);
    }
}
