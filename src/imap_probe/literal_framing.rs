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

/// Return the octet count of the literal header that ends `line`.
///
/// Accepts the RFC 3501 synchronizing form `{N}` and the RFC 7888
/// non-synchronizing form `{N+}`. A literal8 header (`~{N}`, RFC 3516) is also
/// measured so framing scanners skip its payload; callers that interpret
/// response contents use [`literal_header`] to reject it explicitly. `N` must be
/// plain ASCII digits: signs, whitespace, and empty counts are not literals.
pub(super) fn literal_length(line: &[u8]) -> Option<usize> {
    literal_header(line).map(|header| header.length)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LiteralHeader {
    /// Offset of the header (`{` or the `~` of literal8) within the line.
    pub(super) start: usize,
    pub(super) length: usize,
    pub(super) binary: bool,
}

pub(super) fn literal_header(line: &[u8]) -> Option<LiteralHeader> {
    let body = line.strip_suffix(b"}")?;
    let open = body.iter().rposition(|byte| *byte == b'{')?;
    let count = &body[open + 1..];
    let digits = count.strip_suffix(b"+").unwrap_or(count);
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let length = std::str::from_utf8(digits).ok()?.parse::<usize>().ok()?;
    let binary = open > 0 && line[open - 1] == b'~';
    Some(LiteralHeader {
        start: if binary { open - 1 } else { open },
        length,
        binary,
    })
}

/// One complete IMAP server response with literal payloads separated from
/// protocol text. `protocol[i]` is the text preceding `literals[i]`, including
/// that literal's `{N}` header; the final protocol segment is the text after
/// the last literal, without the terminating CRLF. Bytes inside a literal are
/// never interpreted as protocol, so message content that looks like
/// `* 2 FETCH (` or `A001 OK` cannot change how responses are split.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ResponseFrame<'a> {
    pub(super) protocol: Vec<&'a [u8]>,
    pub(super) literals: Vec<&'a [u8]>,
}

impl<'a> ResponseFrame<'a> {
    /// Return the literal payload that directly follows `item` (matched
    /// ASCII case-insensitively as a whole FETCH item name in protocol text).
    pub(super) fn literal_after(&self, item: &[u8]) -> Option<&'a [u8]> {
        self.protocol
            .iter()
            .zip(&self.literals)
            .find_map(|(segment, literal)| {
                let header = literal_header(segment)?;
                let before = segment[..header.start].strip_suffix(b" ")?;
                let item_start = before.len().checked_sub(item.len())?;
                let boundary = item_start == 0 || matches!(before[item_start - 1], b' ' | b'(');
                (boundary && before[item_start..].eq_ignore_ascii_case(item)).then_some(*literal)
            })
    }
}

/// Split `response` into complete IMAP responses. A response ends at the
/// first CRLF that does not terminate a literal header; literal payloads are
/// consumed by length. A trailing partial line without literals is ignored
/// (it can only follow the tagged completion), but a literal whose declared
/// length runs past the buffer is an error rather than a shorter payload.
pub(super) fn split_responses(response: &[u8]) -> ResponseFrames<'_> {
    ResponseFrames {
        response,
        offset: 0,
    }
}

pub(super) struct ResponseFrames<'a> {
    response: &'a [u8],
    offset: usize,
}

impl<'a> Iterator for ResponseFrames<'a> {
    type Item = Result<ResponseFrame<'a>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        let response = self.response;
        let mut frame = ResponseFrame {
            protocol: Vec::new(),
            literals: Vec::new(),
        };
        let mut cursor = self.offset;
        loop {
            let Some(relative_end) = response
                .get(cursor..)?
                .windows(2)
                .position(|pair| pair == b"\r\n")
            else {
                self.offset = response.len();
                return (!frame.literals.is_empty()).then(|| {
                    Err("IMAP response ended before the line following a literal".into())
                });
            };
            let line_end = cursor + relative_end;
            let line = &response[cursor..line_end];
            frame.protocol.push(line);
            let Some(header) = literal_header(line) else {
                self.offset = line_end + 2;
                return Some(Ok(frame));
            };
            if header.binary {
                self.offset = response.len();
                return Some(Err(
                    "IMAP literal8 (~{N}) responses are not supported".into()
                ));
            }
            let start = line_end + 2;
            let Some(payload) = start
                .checked_add(header.length)
                .and_then(|end| response.get(start..end))
            else {
                self.offset = response.len();
                return Some(Err(format!(
                    "IMAP literal declared {} bytes but the response ended first",
                    header.length
                )));
            };
            frame.literals.push(payload);
            cursor = start + header.length;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_headers_accept_only_digit_counts() {
        assert_eq!(literal_length(b"BODY[] {5}"), Some(5));
        assert_eq!(literal_length(b"BODY[] {5+}"), Some(5));
        assert_eq!(literal_length(b"BINARY[] ~{5}"), Some(5));
        for invalid in [
            &b"{+5}"[..],
            b"{}",
            b"{+}",
            b"{ 5}",
            b"{5 }",
            b"{-1}",
            b"5}",
            b"{5}x",
        ] {
            assert_eq!(literal_length(invalid), None, "{invalid:?}");
        }
        let header = literal_header(b"X ~{2}").unwrap();
        assert!(header.binary);
        assert_eq!(header.start, 2);
    }

    #[test]
    fn response_frames_keep_literal_bytes_out_of_protocol_segments() {
        let response = b"* 1 FETCH (A {9}\r\n* BYE\r\n) B {2+}\r\n\r\n)\r\nv1 OK done\r\npartial";
        let frames = split_responses(response)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(
            frames[0].protocol,
            [&b"* 1 FETCH (A {9}"[..], b"B {2+}", b")"]
        );
        assert_eq!(frames[0].literals, [&b"* BYE\r\n) "[..], b"\r\n"]);
        assert_eq!(frames[0].literal_after(b"a"), Some(&b"* BYE\r\n) "[..]));
        assert_eq!(frames[0].literal_after(b"B"), Some(&b"\r\n"[..]));
        assert_eq!(frames[0].literal_after(b"FETCH"), None);
        assert_eq!(frames[1].protocol, [&b"v1 OK done"[..]]);
    }

    #[test]
    fn response_frames_reject_truncated_literals() {
        let mut frames = split_responses(b"* 1 FETCH (A {9}\r\nabc");
        assert!(frames.next().unwrap().is_err());
        assert!(frames.next().is_none());
        let mut frames = split_responses(b"* 1 FETCH (A {3}\r\nabc)");
        assert!(frames.next().unwrap().is_err());
    }
}
