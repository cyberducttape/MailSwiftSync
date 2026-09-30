//! Incremental IMAP tagged-response framing, aware of literal payloads.

/// Locate a tagged completion line without interpreting bytes inside IMAP
/// literals as protocol framing. The scanner owns offsets and literal state;
/// it never rescans bytes already consumed, so chunked input remains O(n).
pub(super) struct TaggedResponseScanner<'a> {
    tag: &'a [u8],
    cursor: usize,
    line_start: usize,
    literal_remaining: usize,
}

impl<'a> TaggedResponseScanner<'a> {
    pub(super) fn new(tag: &'a str) -> Self {
        Self {
            tag: tag.as_bytes(),
            cursor: 0,
            line_start: 0,
            literal_remaining: 0,
        }
    }

    pub(super) fn scan(&mut self, response: &[u8]) -> bool {
        while self.cursor < response.len() {
            if self.literal_remaining > 0 {
                let consumed = self
                    .literal_remaining
                    .min(response.len().saturating_sub(self.cursor));
                self.cursor += consumed;
                self.literal_remaining -= consumed;
                if self.literal_remaining == 0 {
                    // Literal bytes are payload, not part of the following
                    // protocol line. Multiple literals can occur in one line.
                    self.line_start = self.cursor;
                }
                continue;
            }

            if response[self.cursor] != b'\r' {
                self.cursor += 1;
                continue;
            }
            if self.cursor + 1 >= response.len() {
                // Keep a trailing CR for the next chunk, when LF may arrive.
                break;
            }
            if response[self.cursor + 1] != b'\n' {
                self.cursor += 1;
                continue;
            }

            let line_end = self.cursor;
            let line = &response[self.line_start..line_end];
            if line.starts_with(self.tag)
                && line
                    .get(self.tag.len())
                    .is_some_and(|byte| byte.is_ascii_whitespace())
            {
                return true;
            }
            self.literal_remaining = literal_length(line).unwrap_or(0);
            self.cursor += 2;
            self.line_start = self.cursor;
        }
        false
    }
}

pub(super) fn literal_length(line: &[u8]) -> Option<usize> {
    line.strip_suffix(b"}")
        .and_then(|line| line.iter().rposition(|byte| *byte == b'{'))
        .and_then(|start| std::str::from_utf8(&line[start + 1..line.len() - 1]).ok())
        .and_then(|length| {
            length
                .strip_suffix('+')
                .unwrap_or(length)
                .parse::<usize>()
                .ok()
        })
}
