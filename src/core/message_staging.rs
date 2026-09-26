//! Private SQLite staging for live metadata reconciliation.

use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
};

use rusqlite::{Connection, Row, params};
use uuid::Uuid;

use super::{ExtractedMessage, ExtractedMessages, MailboxMessageKey, sqlite_i64};

pub(crate) const STAGE_BATCH_SIZE: i64 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

impl MessageMetadataStage {
    pub(crate) fn open_ephemeral() -> Result<Self, String> {
        let directory = crate::credentials::secret_runtime_base();
        crate::credentials::ensure_private_directory(&directory)
            .map_err(|error| format!("could not secure verification stage directory: {error}"))?;
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
                return Err(format!("could not open verification stage: {error}"));
            }
        };
        let mut stage = Self {
            connection: Some(connection),
            path: Some(path),
        };
        if let Err(error) = stage.initialize() {
            let path = stage.path.take();
            if let Some(connection) = stage.connection.take() {
                let _ = connection.close();
            }
            if let Some(path) = path {
                let _ = fs::remove_file(path);
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
        };
        stage.initialize()?;
        Ok(stage)
    }

    fn connection_ref(&self) -> &Connection {
        self.connection
            .as_ref()
            .expect("verification stage is open")
    }

    fn initialize(&mut self) -> rusqlite::Result<()> {
        self.connection_ref().execute_batch(
            "PRAGMA journal_mode=OFF;
             PRAGMA synchronous=OFF;
             PRAGMA temp_store=FILE;
             CREATE TABLE staged_messages(
                 side INTEGER NOT NULL CHECK(side IN (0,1)),
                 mailbox TEXT NOT NULL,
                 uidvalidity INTEGER NOT NULL CHECK(uidvalidity >= -1),
                 uid TEXT NOT NULL,
                 message_id TEXT,
                 internal_date TEXT,
                 date_key TEXT,
                 size_bytes INTEGER CHECK(size_bytes IS NULL OR size_bytes >= 0),
                 PRIMARY KEY(side,mailbox,uidvalidity,uid)
             );
             CREATE INDEX staged_messages_id ON staged_messages(side,message_id,mailbox,uidvalidity,uid);
             CREATE INDEX staged_messages_metadata ON staged_messages(side,date_key,size_bytes,mailbox,uidvalidity,uid);",
        )
    }

    pub(crate) fn insert_messages(
        &mut self,
        side: StagedMessageSide,
        messages: &ExtractedMessages,
    ) -> Result<(), String> {
        let tx = self
            .connection_ref()
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        {
            let mut insert = tx.prepare_cached("INSERT INTO staged_messages(side,mailbox,uidvalidity,uid,message_id,internal_date,date_key,size_bytes) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)").map_err(|e| e.to_string())?;
            for (key, message) in messages {
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
        tx.commit().map_err(|e| e.to_string())
    }

    pub(crate) fn delete_mailbox(
        &mut self,
        side: StagedMessageSide,
        mailbox: &str,
    ) -> Result<(), String> {
        self.connection_ref()
            .execute(
                "DELETE FROM staged_messages WHERE side=?1 AND mailbox=?2",
                params![side.as_i64(), mailbox],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
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
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
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
        stage
            .insert_messages(StagedMessageSide::Source, &messages)
            .unwrap();
        stage.connection().execute_batch("CREATE TEMP TABLE staged_matched(side INTEGER,mailbox TEXT,uidvalidity INTEGER,uid TEXT,PRIMARY KEY(side,mailbox,uidvalidity,uid));").unwrap();
        let batch = stage
            .batch(StagedMessageSide::Source, 0, true, true)
            .unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].date_key.as_deref(), Some("e:1704067200"));
        stage
            .delete_mailbox(StagedMessageSide::Source, "INBOX")
            .unwrap();
        assert_eq!(stage.count(StagedMessageSide::Source).unwrap(), 0);
    }
}
