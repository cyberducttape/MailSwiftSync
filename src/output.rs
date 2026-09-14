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

impl Deref for BoundedLineBuffer {
    type Target = VecDeque<String>;

    fn deref(&self) -> &Self::Target {
        &self.lines
    }
}
