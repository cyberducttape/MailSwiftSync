use crate::core;

fn number_after(line: &str, marker: &str) -> Option<u64> {
    line.split_once(marker)?
        .1
        .split_whitespace()
        .find_map(|token| {
            token
                .trim_matches(|character: char| !character.is_ascii_digit())
                .parse()
                .ok()
        })
}

fn detected_error_count(line: &str) -> Option<u64> {
    let remainder = line.strip_prefix("Detected ")?;
    let count = remainder.split_whitespace().next()?.parse().ok()?;
    remainder
        .split_whitespace()
        .any(|word| {
            word.trim_matches(|character: char| !character.is_ascii_alphabetic()) == "errors"
        })
        .then_some(count)
}

/// Extract the stable summary fields emitted by imapsync.  The parser only
/// receives summary markers; the process runner keeps the complete journal
/// streaming and bounded separately.
pub(crate) fn parse_imapsync_evidence(lines: &[String]) -> Option<core::MailboxEvidence> {
    let last = |marker: &str| {
        lines
            .iter()
            .rev()
            .find_map(|line| number_after(line, marker))
    };
    let source_folders = last("Host1 Nb folders:")?;
    let destination_folders = last("Host2 Nb folders:")?;
    let source_messages = last("Host1 Nb messages:")?;
    let destination_messages = last("Host2 Nb messages:")?;
    let source_bytes = last("Host1 Total size:")?;
    let destination_bytes = last("Host2 Total size:")?;
    let failed_messages = lines
        .iter()
        .rev()
        .find_map(|line| detected_error_count(line))
        .unwrap_or(0);
    let matched = lines
        .iter()
        .any(|line| line.contains("The sync looks good"));
    Some(core::MailboxEvidence {
        source_messages,
        destination_messages,
        source_bytes,
        destination_bytes,
        unmatched_messages: if matched { 0 } else { 1 },
        failed_messages,
        source_folders,
        destination_folders,
        authoritative: matched,
    })
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DovecotStatusAccumulator {
    pub(crate) folders: u64,
    pub(crate) messages: u64,
    pub(crate) bytes: u64,
    /// Number of lines that looked like Dovecot mailbox status records but
    /// could not be parsed completely. Ignoring these would allow a partial
    /// inventory to accidentally compare equal and become Verified.
    pub(crate) malformed_lines: u64,
}

impl DovecotStatusAccumulator {
    pub(crate) fn observe(&mut self, line: &str) {
        let mut message_count = None;
        let mut virtual_size = None;
        let mut saw_status_field = false;
        for token in line.split_whitespace() {
            if let Some(value) = token.strip_prefix("messages=") {
                saw_status_field = true;
                if message_count.is_none() {
                    message_count = Some(value.parse::<u64>());
                }
            } else if let Some(value) = token.strip_prefix("vsize=") {
                saw_status_field = true;
                if virtual_size.is_none() {
                    virtual_size = Some(value.parse::<u64>());
                }
            }
        }
        if !saw_status_field {
            return;
        }
        if let (Some(Ok(message_count)), Some(Ok(virtual_size))) = (message_count, virtual_size) {
            self.folders = self.folders.saturating_add(1);
            self.messages = self.messages.saturating_add(message_count);
            self.bytes = self.bytes.saturating_add(virtual_size);
        } else {
            self.malformed_lines = self.malformed_lines.saturating_add(1);
        }
    }
}

pub(crate) fn dovecot_evidence_from_accumulators(
    source: &DovecotStatusAccumulator,
    destination: &DovecotStatusAccumulator,
) -> Option<core::MailboxEvidence> {
    (source.folders > 0
        && destination.folders > 0
        && source.malformed_lines == 0
        && destination.malformed_lines == 0)
        .then_some(core::MailboxEvidence {
            source_messages: source.messages,
            destination_messages: destination.messages,
            source_bytes: source.bytes,
            destination_bytes: destination.bytes,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: source.folders,
            destination_folders: destination.folders,
            authoritative: false,
        })
}

#[cfg(test)]
pub(crate) fn parse_dovecot_evidence(
    source: &[String],
    destination: &[String],
) -> Option<core::MailboxEvidence> {
    let mut source_status = DovecotStatusAccumulator::default();
    let mut destination_status = DovecotStatusAccumulator::default();
    source.iter().for_each(|line| source_status.observe(line));
    destination
        .iter()
        .for_each(|line| destination_status.observe(line));
    dovecot_evidence_from_accumulators(&source_status, &destination_status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dovecot_accumulator_reduces_large_reports_without_retaining_lines() {
        let mut source = DovecotStatusAccumulator::default();
        let mut destination = DovecotStatusAccumulator::default();
        for _ in 0..100_000 {
            source.observe("mailbox messages=2 vsize=40");
            destination.observe("mailbox messages=2 vsize=40");
        }
        let evidence = dovecot_evidence_from_accumulators(&source, &destination).unwrap();
        assert_eq!(evidence.source_folders, 100_000);
        assert_eq!(evidence.source_messages, 200_000);
        assert_eq!(evidence.source_bytes, 4_000_000);
        assert!(evidence.is_exact_match());
    }

    #[test]
    fn dovecot_accumulator_rejects_incomplete_status_lines() {
        let mut status = DovecotStatusAccumulator::default();
        status.observe("mailbox messages=not-a-number vsize=40");
        status.observe("mailbox messages=3");
        assert_eq!(status.folders, 0);
        assert_eq!(status.malformed_lines, 2);

        let valid = DovecotStatusAccumulator {
            folders: 1,
            messages: 3,
            bytes: 40,
            malformed_lines: 0,
        };
        assert!(dovecot_evidence_from_accumulators(&valid, &status).is_none());
    }
}
