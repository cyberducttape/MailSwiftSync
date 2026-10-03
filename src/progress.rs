//! Content-free transfer progress derived from imapsync's own output.
//!
//! imapsync reports the source and destination sizes before synchronizing
//! (`Host1 Nb messages:` / `Host1 Total size:`) and one line per copied
//! message (`msg <folder>/<uid> {<bytes>} copied to <folder>/<uid> … N/M msgs
//! left`). Only counts and byte totals are kept; folder names, UIDs, and any
//! other message-derived text are discarded at the parser boundary, so the
//! telemetry can be shown and logged without exposing mailbox metadata.
//!
//! The parser runs on the lossless engine reader thread, never on the
//! best-effort presentation line stream, so dropped UI lines cannot make the
//! counters under-report.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TransferProgress {
    pub(crate) messages_copied: u64,
    pub(crate) bytes_copied: u64,
    /// Source totals reported before synchronization starts.
    pub(crate) source_messages: Option<u64>,
    pub(crate) source_bytes: Option<u64>,
    /// Destination totals reported before synchronization starts.
    pub(crate) destination_messages_at_start: Option<u64>,
    pub(crate) destination_bytes_at_start: Option<u64>,
    /// imapsync's own count of messages still to copy in this run.
    pub(crate) messages_left: Option<u64>,
}

impl TransferProgress {
    /// Fold one engine stdout line into the counters. Returns whether any
    /// counter changed.
    pub(crate) fn observe(&mut self, line: &str) -> bool {
        let line = line.trim();
        if let Some(bytes) = copied_message_bytes(line) {
            self.messages_copied = self.messages_copied.saturating_add(1);
            self.bytes_copied = self.bytes_copied.saturating_add(bytes);
            if let Some(left) = messages_left(line) {
                self.messages_left = Some(left);
            }
            return true;
        }
        let (slot, marker, unit) = if line.starts_with("Host1 Nb messages:") {
            (&mut self.source_messages, "Host1 Nb messages:", "messages")
        } else if line.starts_with("Host1 Total size:") {
            (&mut self.source_bytes, "Host1 Total size:", "bytes")
        } else if line.starts_with("Host2 Nb messages:") {
            (
                &mut self.destination_messages_at_start,
                "Host2 Nb messages:",
                "messages",
            )
        } else if line.starts_with("Host2 Total size:") {
            (
                &mut self.destination_bytes_at_start,
                "Host2 Total size:",
                "bytes",
            )
        } else {
            return false;
        };
        // imapsync repeats these totals in its final statistics; only the
        // pre-synchronization values describe the work to be done.
        if slot.is_some() {
            return false;
        }
        match leading_count(line.strip_prefix(marker).unwrap_or_default(), unit) {
            Some(value) => {
                *slot = Some(value);
                true
            }
            None => false,
        }
    }

    /// Bytes still expected to move, when the engine reported sizes. This is
    /// an estimate: messages already present at the destination are skipped
    /// by the engine and duplicates can make the destination larger.
    pub(crate) fn estimated_remaining_bytes(&self) -> Option<u64> {
        let source = self.source_bytes?;
        let already_there = self.destination_bytes_at_start.unwrap_or(0);
        Some(
            source
                .saturating_sub(already_there)
                .saturating_sub(self.bytes_copied),
        )
    }

    /// Bytes expected for the whole run (copied plus estimated remaining).
    pub(crate) fn estimated_total_bytes(&self) -> Option<u64> {
        self.estimated_remaining_bytes()
            .map(|remaining| remaining.saturating_add(self.bytes_copied))
    }

    pub(crate) fn has_activity(&self) -> bool {
        self.messages_copied > 0 || self.source_bytes.is_some() || self.source_messages.is_some()
    }
}

/// `msg <folder>/<uid> {<bytes>} copied to …` → `<bytes>`. Dry-run lines
/// ("not really") describe no transfer and are ignored.
fn copied_message_bytes(line: &str) -> Option<u64> {
    if !line.starts_with("msg ") || line.contains("not really") {
        return None;
    }
    let (head, _) = line.split_once(" copied to ")?;
    // Folder names may contain braces; the size is the last `{…}` group.
    let open = head.rfind('{')?;
    let close = head[open..].find('}')? + open;
    let digits = &head[open + 1..close];
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// `… <left>/<total> msgs left` → `<left>`.
fn messages_left(line: &str) -> Option<u64> {
    let before = line.strip_suffix("msgs left")?.trim_end();
    let token = before.rsplit(char::is_whitespace).next()?;
    let (left, total) = token.split_once('/')?;
    total.parse::<u64>().ok()?;
    left.parse().ok()
}

fn leading_count(rest: &str, unit: &str) -> Option<u64> {
    let mut tokens = rest.split_whitespace();
    let value = tokens.next()?;
    if tokens.next()? != unit || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::TransferProgress;

    #[test]
    fn copied_messages_accumulate_bytes_and_engine_left_count() {
        let mut progress = TransferProgress::default();
        assert!(progress.observe(
            "msg INBOX/12 {2054}            copied to INBOX/40        3.47 msgs/s  6.951 KiB/s 2.007 KiB copied ETA: Thu Oct  1 10:00:00 2026  12 s  9/10 msgs left"
        ));
        assert!(progress.observe(
            "msg Projects {2026}/7 {1000} copied to Projects {2026}/3  1.00 msgs/s  1 KiB/s 3 KiB copied ETA: Thu Oct  1 10:00:01 2026  9 s  8/10 msgs left"
        ));
        assert_eq!(progress.messages_copied, 2);
        assert_eq!(progress.bytes_copied, 3054);
        assert_eq!(progress.messages_left, Some(8));
    }

    #[test]
    fn dry_run_and_unrelated_lines_are_ignored() {
        let mut progress = TransferProgress::default();
        assert!(
            !progress.observe("msg INBOX/1 {2054} copied to INBOX/(not really since --dry mode)")
        );
        assert!(!progress.observe("msg INBOX/1 {abc} copied to INBOX/2"));
        assert!(!progress.observe("Folder INBOX: 3 messages"));
        assert_eq!(progress, TransferProgress::default());
    }

    #[test]
    fn only_pre_synchronization_totals_are_kept() {
        let mut progress = TransferProgress::default();
        for line in [
            "Host1 Nb messages:           42 messages",
            "Host1 Total size:            100000 bytes (97.656 KiB)",
            "Host2 Nb messages:           2 messages",
            "Host2 Total size:            10000 bytes (9.766 KiB)",
        ] {
            assert!(progress.observe(line), "{line}");
        }
        // Final statistics repeat the totals after the transfer.
        assert!(!progress.observe("Host1 Total size:            999999 bytes"));
        assert_eq!(progress.source_messages, Some(42));
        assert_eq!(progress.source_bytes, Some(100_000));
        assert_eq!(progress.destination_bytes_at_start, Some(10_000));
        progress.observe("msg INBOX/1 {30000} copied to INBOX/9  1 msgs/s");
        assert_eq!(progress.estimated_remaining_bytes(), Some(60_000));
        assert_eq!(progress.estimated_total_bytes(), Some(90_000));
    }

    #[test]
    fn remaining_bytes_never_underflow() {
        let progress = TransferProgress {
            source_bytes: Some(100),
            destination_bytes_at_start: Some(500),
            bytes_copied: 50,
            ..TransferProgress::default()
        };
        assert_eq!(progress.estimated_remaining_bytes(), Some(0));
        assert!(
            TransferProgress::default()
                .estimated_remaining_bytes()
                .is_none()
        );
    }
}
