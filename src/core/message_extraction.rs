use super::*;
use std::collections::HashMap;

/// Extracted message-level details from a migration source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedMessage {
    pub message_id: Option<String>,
    pub uid: Option<String>,
    pub size_bytes: Option<u64>,
    pub internal_date: Option<String>,
}

/// Message extraction from imapsync debug output.
/// Imapsync `--debug 2` output includes per-message details:
/// ```
/// Msg # 1 {size=1234 date="2024-01-01 12:00:00"} ...
/// Msg # 1 -> 2 (new UID on destination)
/// ```
pub struct ImapsyncMessageExtractor;

impl ImapsyncMessageExtractor {
    /// Parse imapsync debug output to extract message UIDs and metadata.
    /// Returns a map of source UID -> ExtractedMessage.
    pub fn extract_from_output(output: &str) -> Result<HashMap<String, ExtractedMessage>, String> {
        let mut messages = HashMap::new();

        for line in output.lines() {
            // Parse lines like: "Msg # 1 {size=1234 date="2024-01-01 12:00:00"}"
            if line.contains("Msg #") && line.contains("size=") {
                if let Some(extracted) = Self::parse_message_line(line) {
                    if let Some(uid) = extracted.uid.clone() {
                        messages.insert(uid, extracted);
                    }
                }
            }
        }

        Ok(messages)
    }

    /// Parse a single imapsync message line to extract metadata.
    fn parse_message_line(line: &str) -> Option<ExtractedMessage> {
        // Extract UID: "Msg # 1" -> uid = "1"
        let uid = line
            .split("Msg #")
            .nth(1)
            .and_then(|part| part.split_whitespace().next())
            .map(|s| s.to_string());

        // Extract size: size=1234
        let size_bytes = line
            .split("size=")
            .nth(1)
            .and_then(|part| part.split_whitespace().next())
            .and_then(|s| s.parse::<u64>().ok());

        // Extract date: date="2024-01-01 12:00:00"
        let internal_date = line
            .split("date=\"")
            .nth(1)
            .and_then(|part| part.split('\"').next())
            .map(|s| s.to_string());

        Some(ExtractedMessage {
            message_id: None, // Would need additional parsing or IMAP FETCH
            uid,
            size_bytes,
            internal_date,
        })
    }
}

/// Message extraction from Dovecot using doveadm.
pub struct DovecotMessageExtractor;

impl DovecotMessageExtractor {
    /// Extract message UIDs and metadata from Dovecot using doveadm fetch.
    /// Command: doveadm -u user@example.com fetch -A "uid messageids" MAILBOX "INBOX"
    pub fn extract_from_doveadm_output(output: &str) -> Result<HashMap<String, ExtractedMessage>, String> {
        let mut messages = HashMap::new();

        for line in output.lines() {
            if let Some(extracted) = Self::parse_doveadm_line(line) {
                if let Some(uid) = extracted.uid.clone() {
                    messages.insert(uid, extracted);
                }
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
    fn imapsync_extractor_parses_message_lines() {
        let output = "Msg # 1 {size=1234 date=\"2024-01-01 12:00:00\"} X-Gmail-Labels:\n\
                      Msg # 2 {size=5678 date=\"2024-01-02 14:30:00\"} Subject: Test\n";

        let messages = ImapsyncMessageExtractor::extract_from_output(output).unwrap();

        assert_eq!(messages.len(), 2);
        assert!(messages.contains_key("1"));
        assert!(messages.contains_key("2"));

        let msg1 = &messages["1"];
        assert_eq!(msg1.uid, Some("1".to_string()));
        assert_eq!(msg1.size_bytes, Some(1234));
        assert_eq!(msg1.internal_date, Some("2024-01-01 12:00:00".to_string()));
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
