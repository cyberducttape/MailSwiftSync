use crate::core;

fn number_after(line: &str, marker: &str) -> Option<u64> {
    let token = line.split_once(marker)?.1.split_whitespace().next()?;
    if token.is_empty() || !token.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    token.parse().ok()
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

#[derive(Clone, Debug, Default)]
pub(crate) struct ImapsyncEvidenceAccumulator {
    source_folders: Option<u64>,
    destination_folders: Option<u64>,
    source_messages: Option<u64>,
    destination_messages: Option<u64>,
    source_bytes: Option<u64>,
    destination_bytes: Option<u64>,
    failed_messages: Option<u64>,
    sync_good: bool,
}

impl ImapsyncEvidenceAccumulator {
    pub(crate) fn observe(&mut self, line: &str) {
        if let Some(value) = number_after(line, "Host1 Nb folders:") {
            self.source_folders = Some(value);
        }
        if let Some(value) = number_after(line, "Host2 Nb folders:") {
            self.destination_folders = Some(value);
        }
        if let Some(value) = number_after(line, "Host1 Nb messages:") {
            self.source_messages = Some(value);
        }
        if let Some(value) = number_after(line, "Host2 Nb messages:") {
            self.destination_messages = Some(value);
        }
        if let Some(value) = number_after(line, "Host1 Total size:") {
            self.source_bytes = Some(value);
        }
        if let Some(value) = number_after(line, "Host2 Total size:") {
            self.destination_bytes = Some(value);
        }
        if let Some(value) = detected_error_count(line) {
            self.failed_messages = Some(value);
            if value > 0 {
                // A prior success marker must not survive a later summary
                // that reports actual engine errors. A subsequent explicit
                // success marker may establish success again, but silence
                // cannot turn a failed summary back into authoritative proof.
                self.sync_good = false;
            }
        }
        self.sync_good |= line.contains("The sync looks good");
    }

    pub(crate) fn evidence(&self) -> Option<core::MailboxEvidence> {
        Some(core::MailboxEvidence {
            source_messages: self.source_messages?,
            destination_messages: self.destination_messages?,
            source_bytes: self.source_bytes?,
            destination_bytes: self.destination_bytes?,
            unmatched_messages: if self.sync_good { 0 } else { 1 },
            failed_messages: self.failed_messages.unwrap_or(0),
            source_folders: self.source_folders?,
            destination_folders: self.destination_folders?,
            authoritative: self.sync_good,
        })
    }
}

/// Extract the stable summary fields emitted by imapsync. The accumulator is
/// constant-memory and always keeps the latest value from a noisy run.
pub(crate) fn parse_imapsync_evidence(lines: &[String]) -> Option<core::MailboxEvidence> {
    let mut accumulator = ImapsyncEvidenceAccumulator::default();
    for line in lines {
        accumulator.observe(line);
    }
    accumulator.evidence()
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
    /// Saturation must never turn an overflowing inventory into an apparently
    /// exact aggregate match.
    pub(crate) overflowed: bool,
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
            let Some(folders) = self.folders.checked_add(1) else {
                self.overflowed = true;
                return;
            };
            let Some(messages) = self.messages.checked_add(message_count) else {
                self.overflowed = true;
                return;
            };
            let Some(bytes) = self.bytes.checked_add(virtual_size) else {
                self.overflowed = true;
                return;
            };
            self.folders = folders;
            self.messages = messages;
            self.bytes = bytes;
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
        .then_some(())
        .filter(|_| !source.overflowed && !destination.overflowed)
        .map(|_| core::MailboxEvidence {
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
    fn imapsync_accumulator_keeps_latest_summary_after_noisy_output() {
        let mut accumulator = ImapsyncEvidenceAccumulator::default();
        for line in [
            "Host1 Nb folders: 1 folders",
            "Host2 Nb folders: 1 folders",
            "Host1 Nb messages: 2 messages",
            "Host2 Nb messages: 2 messages",
            "Host1 Total size: 100 bytes",
            "Host2 Total size: 100 bytes",
            "The sync looks good",
        ] {
            accumulator.observe(line);
        }
        for _ in 0..10_000 {
            accumulator.observe("verbose diagnostic output");
        }
        for line in [
            "Host1 Nb folders: 3 folders",
            "Host2 Nb folders: 3 folders",
            "Host1 Nb messages: 42 messages",
            "Host2 Nb messages: 42 messages",
            "Host1 Total size: 1000 bytes",
            "Host2 Total size: 1000 bytes",
        ] {
            accumulator.observe(line);
        }
        let evidence = accumulator.evidence().unwrap();
        assert_eq!(evidence.source_folders, 3);
        assert_eq!(evidence.source_messages, 42);
        assert_eq!(evidence.destination_messages, 42);
        assert!(evidence.authoritative);
    }

    #[test]
    fn later_imapsync_errors_revoke_an_earlier_success_marker() {
        let mut accumulator = ImapsyncEvidenceAccumulator::default();
        for line in [
            "Host1 Nb folders: 1 folders",
            "Host2 Nb folders: 1 folders",
            "Host1 Nb messages: 2 messages",
            "Host2 Nb messages: 2 messages",
            "Host1 Total size: 100 bytes",
            "Host2 Total size: 100 bytes",
            "The sync looks good",
            "Detected 2 errors",
        ] {
            accumulator.observe(line);
        }
        let evidence = accumulator.evidence().unwrap();
        assert!(!evidence.authoritative);
        assert_eq!(evidence.failed_messages, 2);
        assert_eq!(evidence.unmatched_messages, 1);
    }

    #[test]
    fn imapsync_parser_rejects_non_numeric_summary_values() {
        let lines = [
            "Host1 Nb folders: not-a-number folders".into(),
            "Host2 Nb folders: 1 folders".into(),
            "Host1 Nb messages: 1 messages".into(),
            "Host2 Nb messages: 1 messages".into(),
            "Host1 Total size: 10 bytes".into(),
            "Host2 Total size: 10 bytes".into(),
            "The sync looks good".into(),
        ];
        assert!(parse_imapsync_evidence(&lines).is_none());
    }

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
            overflowed: false,
        };
        assert!(dovecot_evidence_from_accumulators(&valid, &status).is_none());
    }

    #[test]
    fn dovecot_accumulator_rejects_counter_overflow() {
        let mut status = DovecotStatusAccumulator {
            folders: u64::MAX,
            messages: 0,
            bytes: 0,
            malformed_lines: 0,
            overflowed: false,
        };
        status.observe("mailbox messages=1 vsize=1");
        assert!(status.overflowed);

        let valid = DovecotStatusAccumulator {
            folders: 1,
            messages: 1,
            bytes: 1,
            malformed_lines: 0,
            overflowed: false,
        };
        assert!(dovecot_evidence_from_accumulators(&valid, &status).is_none());
    }
}
