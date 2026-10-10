//! Mismatch records, their construction, and verifier memory budgets.

use super::*;

/// Mismatch classifications currently supported by the executable verifier.
///
/// Exact and date/size matches are successful match outcomes represented in
/// `VerificationSummary`, not mismatch records. Content-hash and folder-aware
/// classifications are intentionally not represented here until extraction
/// supplies those fields and the live path persists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MismatchType {
    /// Message-ID matches but available metadata differs.
    MessageIdOnly,
    /// The message is present with matching metadata, but not in its expected
    /// destination folder.
    PresentWrongFolder,
    /// Present in source, absent in destination
    Missing,
    /// Present in destination, absent in source
    Extra,
    /// Multiple instances of same message-ID in destination
    Duplicated,
}

impl MismatchType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MessageIdOnly => "message_id_only",
            Self::PresentWrongFolder => "message_present_wrong_folder",
            Self::Missing => "missing",
            Self::Extra => "extra",
            Self::Duplicated => "duplicated",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "message_id_only" => Self::MessageIdOnly,
            "message_present_wrong_folder" => Self::PresentWrongFolder,
            "missing" => Self::Missing,
            "extra" => Self::Extra,
            "duplicated" => Self::Duplicated,
            _ => return None,
        })
    }
}

/// Mismatch record to persist in the database.
#[derive(Debug, Clone)]
pub struct MessageMismatch {
    pub id: String,
    pub job_id: Arc<str>,
    pub run_id: Arc<str>,
    pub mismatch_type: MismatchType,
    pub source_folder: Option<String>,
    pub destination_folder: Option<String>,
    pub source_uidvalidity: Option<u64>,
    pub destination_uidvalidity: Option<u64>,
    pub source_uid: Option<String>,
    pub dest_uid: Option<String>,
    pub source_message_id: Option<String>,
    pub dest_message_id: Option<String>,
    pub source_size_bytes: Option<u64>,
    pub dest_size_bytes: Option<u64>,
    pub source_date: Option<String>,
    pub dest_date: Option<String>,
    pub source_fingerprint: Option<String>,
    pub destination_fingerprint: Option<String>,
}

pub(super) fn mismatch_source_key<'a>(
    mismatch: &MessageMismatch,
    index: &HashMap<(Option<u64>, &str, &str), &'a MailboxMessageKey>,
) -> Option<&'a MailboxMessageKey> {
    let (uid, folder) = mismatch
        .source_uid
        .as_ref()
        .zip(mismatch.source_folder.as_ref())?;
    index
        .get(&(mismatch.source_uidvalidity, uid.as_str(), folder.as_str()))
        .copied()
}

pub(super) fn mismatch_destination_key<'a>(
    mismatch: &MessageMismatch,
    index: &HashMap<(Option<u64>, &str, &str), &'a MailboxMessageKey>,
) -> Option<&'a MailboxMessageKey> {
    let (uid, folder) = mismatch
        .dest_uid
        .as_ref()
        .zip(mismatch.destination_folder.as_ref())?;
    index
        .get(&(
            mismatch.destination_uidvalidity,
            uid.as_str(),
            folder.as_str(),
        ))
        .copied()
}

pub(super) fn estimated_verifier_record_bytes(
    key: &MailboxMessageKey,
    message: &ExtractedMessage,
) -> usize {
    512usize
        .saturating_add(key.mailbox.len())
        .saturating_add(key.uid.len())
        .saturating_add(message.message_id.as_deref().map_or(0, str::len))
        .saturating_add(message.internal_date.as_deref().map_or(0, str::len))
}

pub(super) fn estimated_verifier_state_bytes(
    source_messages: &ExtractedMessages,
    dest_messages: &ExtractedMessages,
) -> usize {
    let record_bytes = source_messages
        .iter()
        .chain(dest_messages)
        .map(|(key, message)| estimated_verifier_record_bytes(key, message))
        .fold(0usize, |total, record| {
            total.saturating_add(record.saturating_mul(2))
        });
    let record_count = source_messages.len().saturating_add(dest_messages.len());
    record_bytes.saturating_add(
        record_count.saturating_mul(ESTIMATED_RECONCILIATION_INDEX_BYTES_PER_RECORD),
    )
}

pub(super) fn enforce_verifier_state_budget(estimated_state_bytes: usize) -> Result<(), String> {
    if estimated_state_bytes > MAX_ESTIMATED_VERIFIER_STATE_BYTES {
        return Err(
            crate::core::VerificationLimit::ReconciliationState.tag(format!(
                "verification state exceeds the estimated {}-byte aggregate verifier budget",
                MAX_ESTIMATED_VERIFIER_STATE_BYTES
            )),
        );
    }
    Ok(())
}

pub(super) fn estimated_mismatch_bytes(mismatch: &MessageMismatch) -> usize {
    384usize
        .saturating_add(mismatch.id.len())
        .saturating_add(mismatch.job_id.len())
        .saturating_add(mismatch.run_id.len())
        .saturating_add(mismatch.source_folder.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.destination_folder.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.source_uid.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.dest_uid.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.source_message_id.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.dest_message_id.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.source_date.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.dest_date.as_deref().map_or(0, str::len))
        .saturating_add(mismatch.source_fingerprint.as_deref().map_or(0, str::len))
        .saturating_add(
            mismatch
                .destination_fingerprint
                .as_deref()
                .map_or(0, str::len),
        )
}

pub(super) fn append_mismatch_with_budget(
    mismatches: &mut Vec<MessageMismatch>,
    mismatch: MessageMismatch,
    estimated_bytes: &mut usize,
) -> Result<(), String> {
    *estimated_bytes = estimated_bytes.saturating_add(estimated_mismatch_bytes(&mismatch));
    if *estimated_bytes > MAX_ESTIMATED_MISMATCH_DETAIL_BYTES {
        return Err(
            crate::core::VerificationLimit::ReconciliationState.tag(format!(
                "verification mismatch detail exceeds the estimated {}-byte evidence budget",
                MAX_ESTIMATED_MISMATCH_DETAIL_BYTES
            )),
        );
    }
    mismatches.push(mismatch);
    Ok(())
}

pub(super) fn make_mismatch(
    job_id: &Arc<str>,
    run_id: &Arc<str>,
    mismatch_type: MismatchType,
    source_key: Option<&MailboxMessageKey>,
    dest_key: Option<&MailboxMessageKey>,
    source: Option<&ExtractedMessage>,
    destination: Option<&ExtractedMessage>,
) -> MessageMismatch {
    MessageMismatch {
        id: format!(
            "{}-{}-{}",
            run_id,
            mismatch_type.as_str(),
            uuid::Uuid::new_v4()
        ),
        job_id: Arc::clone(job_id),
        run_id: Arc::clone(run_id),
        mismatch_type,
        source_folder: source_key.map(|key| key.mailbox.to_string()),
        destination_folder: dest_key.map(|key| key.mailbox.to_string()),
        source_uidvalidity: source_key.and_then(|key| key.uidvalidity),
        destination_uidvalidity: dest_key.and_then(|key| key.uidvalidity),
        source_uid: source_key.map(|key| key.uid.clone()),
        dest_uid: dest_key.map(|key| key.uid.clone()),
        source_message_id: source.and_then(|message| message.message_id.clone()),
        dest_message_id: destination.and_then(|message| message.message_id.clone()),
        source_size_bytes: source.and_then(|message| message.size_bytes),
        dest_size_bytes: destination.and_then(|message| message.size_bytes),
        source_date: source.and_then(|message| message.internal_date.clone()),
        dest_date: destination.and_then(|message| message.internal_date.clone()),
        source_fingerprint: None,
        destination_fingerprint: None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn verifier_state_budget_is_a_tagged_reconciliation_limit() {
        let error =
            super::enforce_verifier_state_budget(super::MAX_ESTIMATED_VERIFIER_STATE_BYTES + 1)
                .unwrap_err();
        assert_eq!(
            crate::core::VerificationLimit::from_detail(&error),
            Some(crate::core::VerificationLimit::ReconciliationState)
        );
    }
}

/// Columns of the stage table that holds one reconciliation's mismatches.
/// The stage is the same private, owner-only database that already holds the
/// fetched metadata, so staging mismatch detail adds no new exposure; the
/// ledger insert applies the privacy boundary (no Message-IDs, folder digests).
pub(crate) const STAGED_MISMATCHES_TABLE: &str = "CREATE TABLE staged_mismatches(
    ordinal INTEGER PRIMARY KEY,
    id TEXT NOT NULL,
    mismatch_type TEXT NOT NULL,
    source_folder TEXT, destination_folder TEXT,
    source_uidvalidity INTEGER, destination_uidvalidity INTEGER,
    source_uid TEXT, dest_uid TEXT,
    source_message_id TEXT, dest_message_id TEXT,
    source_size_bytes INTEGER, dest_size_bytes INTEGER,
    source_date TEXT, dest_date TEXT,
    source_fingerprint TEXT, destination_fingerprint TEXT)";

fn sqlite_optional_u64_to_i64(value: Option<u64>) -> Result<Option<i64>, String> {
    value
        .map(|value| {
            i64::try_from(value).map_err(|_| "a mismatch number exceeds SQLite range".to_owned())
        })
        .transpose()
}

/// Append one mismatch to the stage table (no in-memory accumulation).
pub(crate) fn insert_staged_mismatch(
    connection: &rusqlite::Connection,
    mismatch: &MessageMismatch,
) -> Result<(), String> {
    crate::core::stage_sql::prepare_cached(
        connection,
        "INSERT INTO staged_mismatches(id,mismatch_type,source_folder,destination_folder,source_uidvalidity,destination_uidvalidity,source_uid,dest_uid,source_message_id,dest_message_id,source_size_bytes,dest_size_bytes,source_date,dest_date,source_fingerprint,destination_fingerprint) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
    )
    .and_then(|mut statement| {
        statement.execute(params![
            mismatch.id,
            mismatch.mismatch_type.as_str(),
            mismatch.source_folder,
            mismatch.destination_folder,
            sqlite_optional_u64_to_i64(mismatch.source_uidvalidity)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
            sqlite_optional_u64_to_i64(mismatch.destination_uidvalidity)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
            mismatch.source_uid,
            mismatch.dest_uid,
            mismatch.source_message_id,
            mismatch.dest_message_id,
            sqlite_optional_u64_to_i64(mismatch.source_size_bytes)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
            sqlite_optional_u64_to_i64(mismatch.dest_size_bytes)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
            mismatch.source_date,
            mismatch.dest_date,
            mismatch.source_fingerprint,
            mismatch.destination_fingerprint,
        ])
    })
    .map(|_| ())
    .map_err(|error| format!("could not stage message mismatch: {error}"))
}

/// Visit staged mismatches in reconciliation order, one row at a time.
pub(crate) fn visit_staged_mismatches(
    connection: &rusqlite::Connection,
    job_id: &Arc<str>,
    run_id: &Arc<str>,
    visit: &mut dyn FnMut(&MessageMismatch) -> rusqlite::Result<()>,
) -> rusqlite::Result<()> {
    let mut statement = connection.prepare(
        "SELECT id,mismatch_type,source_folder,destination_folder,source_uidvalidity,destination_uidvalidity,source_uid,dest_uid,source_message_id,dest_message_id,source_size_bytes,dest_size_bytes,source_date,dest_date,source_fingerprint,destination_fingerprint FROM staged_mismatches ORDER BY ordinal",
    )?;
    let mut rows = statement.query([])?;
    let unsigned = |value: Option<i64>| -> rusqlite::Result<Option<u64>> {
        value
            .map(|value| u64::try_from(value).map_err(|_| rusqlite::Error::InvalidQuery))
            .transpose()
    };
    while let Some(row) = rows.next()? {
        let mismatch_type =
            MismatchType::parse(&row.get::<_, String>(1)?).ok_or(rusqlite::Error::InvalidQuery)?;
        let mismatch = MessageMismatch {
            id: row.get(0)?,
            job_id: Arc::clone(job_id),
            run_id: Arc::clone(run_id),
            mismatch_type,
            source_folder: row.get(2)?,
            destination_folder: row.get(3)?,
            source_uidvalidity: unsigned(row.get(4)?)?,
            destination_uidvalidity: unsigned(row.get(5)?)?,
            source_uid: row.get(6)?,
            dest_uid: row.get(7)?,
            source_message_id: row.get(8)?,
            dest_message_id: row.get(9)?,
            source_size_bytes: unsigned(row.get(10)?)?,
            dest_size_bytes: unsigned(row.get(11)?)?,
            source_date: row.get(12)?,
            dest_date: row.get(13)?,
            source_fingerprint: row.get(14)?,
            destination_fingerprint: row.get(15)?,
        };
        visit(&mismatch)?;
    }
    Ok(())
}

/// A reconciliation's mismatches: in memory for small or ephemeral stages, or
/// left in a durable verification stage and streamed into the ledger by the
/// terminal commit, so mismatch volume never has to fit in memory.
#[derive(Debug, Clone)]
pub enum MismatchSet {
    InMemory(Vec<MessageMismatch>),
    Staged {
        stage_path: std::path::PathBuf,
        job_id: Arc<str>,
        run_id: Arc<str>,
        count: u64,
        /// Distinct folder names named by the mismatches (bounded by the
        /// account's folder count), for this session's drill-down labels.
        folders: Vec<String>,
    },
}

impl Default for MismatchSet {
    fn default() -> Self {
        Self::InMemory(Vec::new())
    }
}

impl MismatchSet {
    /// Folder names referenced by these mismatches.
    pub fn folder_names(&self) -> Vec<String> {
        match self {
            Self::InMemory(mismatches) => {
                let mut names = mismatches
                    .iter()
                    .flat_map(|mismatch| {
                        [
                            mismatch.source_folder.clone(),
                            mismatch.destination_folder.clone(),
                        ]
                    })
                    .flatten()
                    .collect::<Vec<_>>();
                names.sort_unstable();
                names.dedup();
                names
            }
            Self::Staged { folders, .. } => folders.clone(),
        }
    }
}

/// A source of mismatches the ledger can count and stream without requiring
/// them all in memory.
pub trait MismatchSource {
    fn mismatch_count(&self) -> u64;
    fn try_for_each_mismatch(
        &self,
        visit: &mut dyn FnMut(&MessageMismatch) -> rusqlite::Result<()>,
    ) -> rusqlite::Result<()>;
}

impl MismatchSource for [MessageMismatch] {
    fn mismatch_count(&self) -> u64 {
        self.len() as u64
    }
    fn try_for_each_mismatch(
        &self,
        visit: &mut dyn FnMut(&MessageMismatch) -> rusqlite::Result<()>,
    ) -> rusqlite::Result<()> {
        self.iter().try_for_each(visit)
    }
}

impl<const N: usize> MismatchSource for [MessageMismatch; N] {
    fn mismatch_count(&self) -> u64 {
        self.as_slice().mismatch_count()
    }
    fn try_for_each_mismatch(
        &self,
        visit: &mut dyn FnMut(&MessageMismatch) -> rusqlite::Result<()>,
    ) -> rusqlite::Result<()> {
        self.as_slice().try_for_each_mismatch(visit)
    }
}

impl MismatchSource for Vec<MessageMismatch> {
    fn mismatch_count(&self) -> u64 {
        self.as_slice().mismatch_count()
    }
    fn try_for_each_mismatch(
        &self,
        visit: &mut dyn FnMut(&MessageMismatch) -> rusqlite::Result<()>,
    ) -> rusqlite::Result<()> {
        self.as_slice().try_for_each_mismatch(visit)
    }
}

impl MismatchSource for MismatchSet {
    fn mismatch_count(&self) -> u64 {
        match self {
            Self::InMemory(mismatches) => mismatches.len() as u64,
            Self::Staged { count, .. } => *count,
        }
    }
    fn try_for_each_mismatch(
        &self,
        visit: &mut dyn FnMut(&MessageMismatch) -> rusqlite::Result<()>,
    ) -> rusqlite::Result<()> {
        match self {
            Self::InMemory(mismatches) => mismatches.try_for_each_mismatch(visit),
            Self::Staged {
                stage_path,
                job_id,
                run_id,
                count,
                ..
            } => {
                let connection = rusqlite::Connection::open_with_flags(
                    stage_path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )?;
                let mut visited = 0_u64;
                visit_staged_mismatches(&connection, job_id, run_id, &mut |mismatch| {
                    visited = visited.saturating_add(1);
                    visit(mismatch)
                })?;
                // The stage must still hold exactly what reconciliation
                // produced; anything else is not the evidence being committed.
                if visited != *count {
                    return Err(crate::core::ledger_rejection(
                        "staged mismatch evidence changed before its commit",
                    ));
                }
                Ok(())
            }
        }
    }
}
