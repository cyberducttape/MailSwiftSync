use std::collections::HashMap;

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
pub struct ImapsyncMessageExtractor;

impl ImapsyncMessageExtractor {
    /// Parse imapsync copy-progress output to extract source UIDs and sizes.
    /// Returns a map of source UID -> ExtractedMessage.
    pub fn extract_from_output(output: &str) -> Result<HashMap<String, ExtractedMessage>, String> {
        let mut messages = HashMap::new();

        for line in output.lines() {
            if let Some(record) = Self::parse_copy_line(line) {
                messages.insert(
                    record.source_uid.clone(),
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
        let mut fields = line.split_whitespace();
        if fields.next()? != "msg" {
            return None;
        }
        let source = fields.next()?;
        let size_bytes = fields
            .next()?
            .strip_prefix('{')
            .and_then(|value| value.strip_suffix('}'))
            .and_then(|value| value.parse::<u64>().ok());
        if fields.next()? != "copied" || fields.next()? != "to" {
            return None;
        }
        let destination = fields.next()?;
        let (source_mailbox, source_uid) = split_mailbox_uid(source)?;
        let (destination_mailbox, destination_uid) = split_mailbox_uid(destination)?;
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
pub struct DovecotMessageExtractor;

impl DovecotMessageExtractor {
    /// Extract message UIDs and metadata from Dovecot using doveadm fetch.
    /// Command: doveadm -u user@example.com fetch -A "uid messageids" MAILBOX "INBOX"
    pub fn extract_from_doveadm_output(
        output: &str,
    ) -> Result<HashMap<String, ExtractedMessage>, String> {
        let mut messages = HashMap::new();

        for line in output.lines() {
            if let Some(extracted) = Self::parse_doveadm_line(line)
                && let Some(uid) = extracted.uid.clone()
            {
                messages.insert(uid, extracted);
            }
        }

        Ok(messages)
    }

    /// Parse doveadm fetch output line.
    /// Expected format: uid=123 messageid="<id@example.com>"
    fn parse_doveadm_line(line: &str) -> Option<ExtractedMessage> {
        let uid = line
            .split("uid=")
            .nth(1)
            .and_then(|part| part.split_whitespace().next())
            .map(|s| s.to_string());

        let message_id = line
            .split("messageid=\"")
            .nth(1)
            .and_then(|part| part.split('\"').next())
            .map(|s| s.to_string());

        Some(ExtractedMessage {
            message_id,
            uid,
            size_bytes: None, // Dovecot fetch would need additional flag
            internal_date: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imapsync_extractor_parses_real_copy_progress_and_uid_rewrite() {
        let output = "msg INBOX/5 {279010} copied to backup/INBOX/49 0.57 msgs/s 154.916 KiB/s 272.471 KiB copied\n";

        let messages = ImapsyncMessageExtractor::extract_from_output(output).unwrap();
        assert_eq!(messages["5"].size_bytes, Some(279010));

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
    fn doveadm_extractor_parses_fetch_output() {
        let output = "uid=123 messageid=\"<abc@example.com>\"\n\
                      uid=124 messageid=\"<def@example.com>\"\n";

        let messages = DovecotMessageExtractor::extract_from_doveadm_output(output).unwrap();

        assert_eq!(messages.len(), 2);
        assert!(messages.contains_key("123"));

        let msg = &messages["123"];
        assert_eq!(msg.uid, Some("123".to_string()));
        assert_eq!(msg.message_id, Some("<abc@example.com>".to_string()));
    }
}
