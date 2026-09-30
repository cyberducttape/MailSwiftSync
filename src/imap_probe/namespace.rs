//! Bounded parser for the optional RFC 2342 NAMESPACE response.

use crate::core::{NamespaceEntry, NamespaceInfo};

pub(super) fn parse(response: &str) -> Option<NamespaceInfo> {
    let marker = response.match_indices('*').find_map(|(start, _)| {
        if start > 0 && response.as_bytes().get(start - 1) != Some(&b'\n') {
            return None;
        }
        let remainder = response.get(start..)?;
        let after_star = remainder.strip_prefix('*')?.trim_start_matches([' ', '\t']);
        let keyword = after_star.get(..9)?;
        if !crate::imap_protocol::atom_eq(keyword, "NAMESPACE") {
            return None;
        }
        let data_offset = start + 1 + (remainder.len() - after_star.len()) + keyword.len();
        response
            .get(data_offset..)
            .filter(|tail| tail.starts_with([' ', '\t']))
            .map(|tail| tail.trim_start_matches([' ', '\t']))
    })?;
    let data = marker;
    let mut parser = Parser {
        bytes: data.as_bytes(),
        offset: 0,
    };
    Some(NamespaceInfo {
        personal: parser.list()?,
        shared: parser.list()?,
        other_users: parser.list()?,
    })
}

struct Parser<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Parser<'_> {
    fn whitespace(&mut self) {
        while self
            .bytes
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
    }

    fn punctuation(&mut self, expected: u8) -> bool {
        self.whitespace();
        if self.bytes.get(self.offset) == Some(&expected) {
            self.offset += 1;
            true
        } else {
            false
        }
    }

    /// `None` represents protocol NIL; `Some(value)` represents an astring.
    fn value(&mut self) -> Option<Option<String>> {
        self.whitespace();
        match *self.bytes.get(self.offset)? {
            b'"' => {
                self.offset += 1;
                let mut value = Vec::new();
                loop {
                    let byte = *self.bytes.get(self.offset)?;
                    self.offset += 1;
                    match byte {
                        b'"' => return String::from_utf8(value).ok().map(Some),
                        b'\\' => {
                            let escaped = *self.bytes.get(self.offset)?;
                            self.offset += 1;
                            value.push(escaped);
                        }
                        _ => value.push(byte),
                    }
                }
            }
            b'{' => {
                self.offset += 1;
                let start = self.offset;
                while self.bytes.get(self.offset).is_some_and(u8::is_ascii_digit) {
                    self.offset += 1;
                }
                let length = std::str::from_utf8(&self.bytes[start..self.offset])
                    .ok()?
                    .parse::<usize>()
                    .ok()?;
                if self.bytes.get(self.offset..self.offset.checked_add(3)?) != Some(b"}\r\n") {
                    return None;
                }
                self.offset += 3;
                let end = self.offset.checked_add(length)?;
                let value = std::str::from_utf8(self.bytes.get(self.offset..end)?)
                    .ok()?
                    .to_owned();
                self.offset = end;
                Some(Some(value))
            }
            b'(' | b')' => None,
            _ => {
                let start = self.offset;
                while self
                    .bytes
                    .get(self.offset)
                    .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'(' | b')'))
                {
                    self.offset += 1;
                }
                let atom = std::str::from_utf8(&self.bytes[start..self.offset]).ok()?;
                if atom.eq_ignore_ascii_case("NIL") {
                    Some(None)
                } else {
                    Some(Some(atom.to_owned()))
                }
            }
        }
    }

    fn list(&mut self) -> Option<Vec<NamespaceEntry>> {
        self.whitespace();
        if self
            .bytes
            .get(self.offset..self.offset.checked_add(3)?)
            .is_some_and(|atom| atom.eq_ignore_ascii_case(b"NIL"))
        {
            self.offset += 3;
            return Some(Vec::new());
        }
        if !self.punctuation(b'(') {
            return None;
        }
        let mut entries = Vec::new();
        loop {
            self.whitespace();
            if self.punctuation(b')') {
                return Some(entries);
            }
            if !self.punctuation(b'(') {
                return None;
            }
            let prefix = self.value()??;
            let delimiter = match self.value()? {
                None => None,
                Some(value) => {
                    let mut chars = value.chars();
                    let delimiter = chars.next()?;
                    if chars.next().is_some() {
                        return None;
                    }
                    Some(delimiter)
                }
            };
            if !self.punctuation(b')') {
                return None;
            }
            entries.push(NamespaceEntry { prefix, delimiter });
        }
    }
}
