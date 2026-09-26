use std::{collections::HashMap, sync::Arc};

/// A message's local identity. IMAP UIDs are unique only within a mailbox
/// and UIDVALIDITY context, so a bare UID is never a valid map key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MailboxMessageKey {
    pub mailbox: Arc<str>,
    pub uidvalidity: Option<u64>,
    pub uid: String,
}

impl MailboxMessageKey {
    #[cfg(test)]
    pub fn new(mailbox: impl Into<String>, uid: impl Into<String>) -> Self {
        Self {
            mailbox: Arc::from(mailbox.into()),
            uidvalidity: None,
            uid: uid.into(),
        }
    }

    #[cfg(test)]
    pub fn with_uidvalidity(
        mailbox: impl Into<String>,
        uidvalidity: u64,
        uid: impl Into<String>,
    ) -> Self {
        Self {
            mailbox: Arc::from(mailbox.into()),
            uidvalidity: Some(uidvalidity),
            uid: uid.into(),
        }
    }

    pub fn with_shared_mailbox(
        mailbox: Arc<str>,
        uidvalidity: Option<u64>,
        uid: impl Into<String>,
    ) -> Self {
        Self {
            mailbox,
            uidvalidity,
            uid: uid.into(),
        }
    }
}

pub type ExtractedMessages = HashMap<MailboxMessageKey, ExtractedMessage>;

/// Extracted message-level details from a migration source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedMessage {
    pub message_id: Option<String>,
    pub uid: Option<String>,
    pub size_bytes: Option<u64>,
    pub internal_date: Option<String>,
}

/// A source-to-destination mapping emitted by imapsync after a successful
/// APPEND/COPY. Both UIDs are mailbox-local and must not be used as a
/// cross-mailbox message identity.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub struct ImapsyncCopyRecord {
    pub source_mailbox: String,
    pub source_uid: String,
    pub destination_mailbox: String,
    pub destination_uid: String,
    pub size_bytes: Option<u64>,
}

/// Message extraction from imapsync output.
///
/// The parser accepts the transfer grammar emitted by the pinned engine, for
/// example: `msg INBOX/5 {279010} copied to backup/INBOX/49 ...`.
/// Message-ID, INTERNALDATE, and content fingerprints must come from explicit
/// IMAP FETCH operations; they are not fabricated from progress text.
#[cfg(test)]
pub struct ImapsyncMessageExtractor;

#[cfg(test)]
impl ImapsyncMessageExtractor {
    /// Parse imapsync copy-progress output to extract source UIDs and sizes.
    /// Returns a map keyed by source mailbox and local UID.
    pub fn extract_from_output(output: &str) -> Result<ExtractedMessages, String> {
        let mut messages = HashMap::new();

        for line in output.lines() {
            if let Some(record) = Self::parse_copy_line(line) {
                messages.insert(
                    MailboxMessageKey::new(
                        record.source_mailbox.clone(),
                        record.source_uid.clone(),
                    ),
                    ExtractedMessage {
                        message_id: None,
                        uid: Some(record.source_uid),
                        size_bytes: record.size_bytes,
                        internal_date: None,
                    },
                );
            }
        }

        Ok(messages)
    }

    /// Parse imapsync's actual copy-progress line and retain both local UIDs.
    fn parse_copy_line(line: &str) -> Option<ImapsyncCopyRecord> {
        let rest = line.strip_prefix("msg ")?;
        let (source, rest) = rest.split_once(" {")?;
        let (size, rest) = rest.split_once("} copied to ")?;
        let size_bytes = size.parse::<u64>().ok();
        let (source_mailbox, source_uid) = split_mailbox_uid(unquote_path(source.trim()))?;
        let (destination_mailbox, destination_uid) = split_destination_path(rest)?;
        Some(ImapsyncCopyRecord {
            source_mailbox,
            source_uid,
            destination_mailbox,
            destination_uid,
            size_bytes,
        })
    }

    /// Parse copy-progress mappings when the caller needs both mailbox-local
    /// UIDs rather than a source message map.
    pub fn extract_copy_records(output: &str) -> Vec<ImapsyncCopyRecord> {
        output.lines().filter_map(Self::parse_copy_line).collect()
    }
}

/// Extract the destination path from the copy-progress suffix. The destination
/// may contain spaces and the line may continue with throughput statistics,
/// so whitespace tokenization cannot identify its end. The final `/digits`
/// component is the destination UID; everything before it is the mailbox.
#[cfg(test)]
fn split_destination_path(value: &str) -> Option<(String, String)> {
    let value = value.trim_start();
    if let Some(quoted) = value.strip_prefix('"')
        && let Some(end) = quoted.find('"')
    {
        return split_mailbox_uid(&quoted[..end]);
    }
    for (index, character) in value.char_indices().rev() {
        if character != '/' {
            continue;
        }
        let uid_start = index + character.len_utf8();
        let Some(uid) = value[uid_start..]
            .split_whitespace()
            .next()
            .filter(|uid| !uid.is_empty() && uid.bytes().all(|byte| byte.is_ascii_digit()))
        else {
            continue;
        };
        let path_end = uid_start + uid.len();
        let remainder = &value[path_end..];
        if !remainder.is_empty() && !remainder.chars().next().is_some_and(char::is_whitespace) {
            continue;
        }
        return split_mailbox_uid(unquote_path(&value[..path_end]));
    }
    None
}

#[cfg(test)]
fn unquote_path(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

#[cfg(test)]
fn split_mailbox_uid(value: &str) -> Option<(String, String)> {
    let separator = value.rfind('/')?;
    let (mailbox, uid) = value.split_at(separator);
    let uid = uid.strip_prefix('/')?;
    if mailbox.is_empty() || uid.is_empty() || !uid.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((mailbox.to_owned(), uid.to_owned()))
}

/// Message extraction from Dovecot using doveadm.
#[cfg(test)]
pub struct DovecotMessageExtractor;

#[cfg(test)]
impl DovecotMessageExtractor {
    /// Fields requested from `doveadm fetch`. The explicit tab formatter is
    /// part of this parser contract; default human-oriented output is not
    /// accepted.
    const FETCH_FIELDS: &'static str = "uid hdr.message-id size.virtual date.received.unixtime";

    /// Build arguments for one user's mailbox. UIDVALIDITY is obtained from a
    /// separate mailbox-status query and supplied to the parser below; it is
    /// not fabricated from fetch output.
    pub fn fetch_command_args(user: &str, mailbox: &str) -> Vec<String> {
        vec![
            "-f".to_owned(),
            "tab".to_owned(),
            "fetch".to_owned(),
            "-u".to_owned(),
            user.to_owned(),
            Self::FETCH_FIELDS.to_owned(),
            "mailbox".to_owned(),
            mailbox.to_owned(),
        ]
    }

    /// Extract message metadata from the tab formatter selected by
    /// `fetch_command_args`. A caller may attach UIDVALIDITY only when it was
    /// actually returned by a separate mailbox-status operation.
    pub fn extract_from_doveadm_tab(
        mailbox: &str,
        uidvalidity: Option<u64>,
        output: &str,
    ) -> Result<ExtractedMessages, String> {
        let mut lines = output.lines();
        let header = lines
            .next()
            .ok_or_else(|| "doveadm tab output is missing its header".to_owned())?;
        let expected_header = [
            "uid",
            "hdr.message-id",
            "size.virtual",
            "date.received.unixtime",
        ];
        if header
            .trim_end_matches('\r')
            .split('\t')
            .collect::<Vec<_>>()
            != expected_header
        {
            return Err("unexpected doveadm tab header".to_owned());
        }
        let mut messages = HashMap::new();

        for (index, line) in lines.enumerate() {
            let fields = line.trim_end_matches('\r').split('\t').collect::<Vec<_>>();
            if fields.len() != expected_header.len() {
                return Err(format!(
                    "doveadm fetch record {index} has {} fields; expected {}",
                    fields.len(),
                    expected_header.len()
                ));
            }
            let uid = fields[0].to_owned();
            if uid.parse::<u64>().is_err() {
                return Err(format!("doveadm fetch record {index} has invalid uid"));
            }
            let size_bytes = parse_optional_u64(fields[2], "size.virtual", index)?;
            let internal_date = parse_optional_u64(fields[3], "date.received.unixtime", index)?
                .map(|value| value.to_string());
            let message_id = nonempty(fields[1])
                .map(normalize_message_id)
                .filter(|value| !value.is_empty());
            let key = match uidvalidity {
                Some(value) => MailboxMessageKey::with_uidvalidity(mailbox, value, uid.clone()),
                None => MailboxMessageKey::new(mailbox, uid.clone()),
            };
            if messages
                .insert(
                    key,
                    ExtractedMessage {
                        message_id,
                        uid: Some(uid),
                        size_bytes,
                        internal_date,
                    },
                )
                .is_some()
            {
                return Err(format!(
                    "doveadm returned a duplicate local UID for mailbox {mailbox}"
                ));
            }
        }

        Ok(messages)
    }
}

#[cfg(test)]
fn nonempty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
fn parse_optional_u64(value: &str, field: &str, index: usize) -> Result<Option<u64>, String> {
    nonempty(value)
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| format!("doveadm fetch record {index} has invalid {field}"))
        })
        .transpose()
}

#[cfg(test)]
fn normalize_message_id(value: &str) -> String {
    value
        .trim()
        .strip_prefix("Message-ID:")
        .unwrap_or(value.trim())
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imapsync_extractor_parses_real_copy_progress_and_uid_rewrite() {
        let output = "msg INBOX/5 {279010} copied to backup/INBOX/49 0.57 msgs/s 154.916 KiB/s 272.471 KiB copied\n";

        let messages = ImapsyncMessageExtractor::extract_from_output(output).unwrap();
        assert_eq!(
            messages[&MailboxMessageKey::new("INBOX", "5")].size_bytes,
            Some(279010)
        );

        let records = ImapsyncMessageExtractor::extract_copy_records(output);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source_mailbox, "INBOX");
        assert_eq!(records[0].source_uid, "5");
        assert_eq!(records[0].destination_mailbox, "backup/INBOX");
        assert_eq!(records[0].destination_uid, "49");
    }

    #[test]
    fn imapsync_extractor_ignores_non_copy_progress() {
        let output = "msg INBOX/5 {279010} failed to copy\nreconnecting to host2\n";
        assert!(
            ImapsyncMessageExtractor::extract_from_output(output)
                .unwrap()
                .is_empty()
        );
        assert!(ImapsyncMessageExtractor::extract_copy_records(output).is_empty());
    }

    #[test]
    fn imapsync_extractor_accepts_quoted_space_and_unicode_mailboxes() {
        let output = "msg \"Sent Items/5\" {100} copied to \"backup/Sent Items/50\" 1.0 msgs/s\n\
                      msg \"客户邮件/6\" {200} copied to \"backup/客户邮件/60\" 1.0 msgs/s\n";

        let records = ImapsyncMessageExtractor::extract_copy_records(output);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].source_mailbox, "Sent Items");
        assert_eq!(records[0].destination_mailbox, "backup/Sent Items");
        assert_eq!(records[1].source_mailbox, "客户邮件");
        assert_eq!(records[1].destination_mailbox, "backup/客户邮件");
    }

    #[test]
    fn imapsync_extractor_accepts_unquoted_space_in_source_and_destination() {
        let output = "msg INBOX/Project Alpha/7 {300} copied to backup/INBOX/Project Alpha/70\n";

        let records = ImapsyncMessageExtractor::extract_copy_records(output);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source_mailbox, "INBOX/Project Alpha");
        assert_eq!(records[0].destination_mailbox, "backup/INBOX/Project Alpha");
        assert_eq!(records[0].source_uid, "7");
        assert_eq!(records[0].destination_uid, "70");
    }

    #[test]
    fn doveadm_extractor_parses_explicit_tab_fetch_output() {
        let output = "uid\thdr.message-id\tsize.virtual\tdate.received.unixtime\n\
                      123\t<abc@example.com>\t4096\t1704067200\n\
                      124\tMessage-ID: <def@example.com>\t8192\t1704153600\n";

        let messages =
            DovecotMessageExtractor::extract_from_doveadm_tab("INBOX", Some(9876), output).unwrap();

        assert_eq!(messages.len(), 2);
        let key = MailboxMessageKey::with_uidvalidity("INBOX", 9876, "123");
        assert!(messages.contains_key(&key));

        let msg = &messages[&key];
        assert_eq!(msg.uid, Some("123".to_string()));
        assert_eq!(msg.message_id, Some("<abc@example.com>".to_string()));
        assert_eq!(msg.size_bytes, Some(4096));
        assert_eq!(msg.internal_date, Some("1704067200".to_string()));
    }

    #[test]
    fn doveadm_fetch_contract_selects_tab_for_one_user() {
        assert_eq!(
            DovecotMessageExtractor::fetch_command_args("user@example.com", "Sent Items"),
            [
                "-f",
                "tab",
                "fetch",
                "-u",
                "user@example.com",
                "uid hdr.message-id size.virtual date.received.unixtime",
                "mailbox",
                "Sent Items",
            ]
        );
    }

    #[test]
    fn doveadm_extractor_rejects_default_human_output() {
        let output = "uid=123 messageid=\"<abc@example.com>\"\n";
        let error =
            DovecotMessageExtractor::extract_from_doveadm_tab("INBOX", None, output).unwrap_err();
        assert_eq!(error, "unexpected doveadm tab header");
    }

    #[test]
    fn imapsync_extractor_preserves_same_uid_in_different_mailboxes() {
        let output = "msg INBOX/5 {100} copied to backup/INBOX/50\n\
                      msg Sent/5 {200} copied to backup/Sent/51\n";

        let messages = ImapsyncMessageExtractor::extract_from_output(output).unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[&MailboxMessageKey::new("INBOX", "5")].size_bytes,
            Some(100)
        );
        assert_eq!(
            messages[&MailboxMessageKey::new("Sent", "5")].size_bytes,
            Some(200)
        );
    }

    #[test]
    fn doveadm_extractor_preserves_same_uid_in_different_mailboxes() {
        let output = "uid\thdr.message-id\tsize.virtual\tdate.received.unixtime\n\
                      5\t<sent@example.com>\t\t\n";
        let inbox =
            DovecotMessageExtractor::extract_from_doveadm_tab("INBOX", None, output).unwrap();
        let sent = DovecotMessageExtractor::extract_from_doveadm_tab("Sent", None, output).unwrap();

        assert_ne!(inbox.keys().next(), sent.keys().next());
        assert_eq!(inbox.len(), 1);
        assert_eq!(sent.len(), 1);
    }
}
