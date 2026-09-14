//! Bounded line storage shared by operator journals and process tails.

use std::{collections::VecDeque, ops::Deref};

pub(crate) struct BoundedLineBuffer {
    lines: VecDeque<String>,
    bytes: usize,
}

impl BoundedLineBuffer {
    pub(crate) fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
        }
    }

    pub(crate) fn push_bounded(&mut self, line: String, max_lines: usize, max_bytes: usize) {
        if max_lines == 0 || max_bytes == 0 {
            return;
        }
        let line = truncate_to_bytes(line, max_bytes);
        while self.lines.len() >= max_lines || self.bytes.saturating_add(line.len()) > max_bytes {
            let Some(removed) = self.lines.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(removed.len());
        }
        self.bytes = self.bytes.saturating_add(line.len());
        self.lines.push_back(line);
    }

    pub(crate) fn push_front_bounded(&mut self, line: String, max_lines: usize, max_bytes: usize) {
        if max_lines == 0 || max_bytes == 0 {
            return;
        }
        let line = truncate_to_bytes(line, max_bytes);
        while self.lines.len() >= max_lines || self.bytes.saturating_add(line.len()) > max_bytes {
            let Some(removed) = self.lines.pop_back() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(removed.len());
        }
        self.bytes = self.bytes.saturating_add(line.len());
        self.lines.push_front(line);
    }

    pub(crate) fn from_one(line: String) -> Self {
        let mut buffer = Self::new();
        buffer.push_bounded(
            line,
            crate::MAX_VISIBLE_OUTPUT_LINES,
            crate::MAX_VISIBLE_OUTPUT_BYTES,
        );
        buffer
    }

    pub(crate) fn clear(&mut self) {
        self.lines.clear();
        self.bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
}

fn truncate_to_bytes(mut line: String, max_bytes: usize) -> String {
    if line.len() > max_bytes {
        let mut boundary = max_bytes;
        while !line.is_char_boundary(boundary) {
            boundary -= 1;
        }
        line.truncate(boundary);
    }
    line
}

impl Deref for BoundedLineBuffer {
    type Target = VecDeque<String>;

    fn deref(&self) -> &Self::Target {
        &self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedLineBuffer;

    #[test]
    fn oversized_lines_are_truncated_to_the_byte_budget() {
        let mut buffer = BoundedLineBuffer::new();
        buffer.push_bounded("é".repeat(100), 10, 7);
        assert!(buffer.bytes() <= 7);
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.front().map(String::len), Some(6));
    }

    #[test]
    fn front_insertion_also_respects_the_byte_budget() {
        let mut buffer = BoundedLineBuffer::new();
        buffer.push_front_bounded("x".repeat(20), 10, 5);
        assert_eq!(buffer.bytes(), 5);
        assert_eq!(buffer.front().map(String::len), Some(5));
    }
}
