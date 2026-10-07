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
