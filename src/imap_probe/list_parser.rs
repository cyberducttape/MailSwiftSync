//! Parsing helpers for non-literal IMAP LIST records.

/// Parse the final mailbox-name atom from a normal, non-literal LIST record.
/// Literal names are handled after their bounded bytes have been collected.
pub(super) fn mailbox_name(line: &str) -> Option<String> {
    let tokens = tokens(line)?;
    tokens
        .last()
        .filter(|value| !value.starts_with('{'))
        .cloned()
}

pub(super) fn delimiter(line: &str) -> Option<String> {
    let tokens = tokens(line)?;
    let mut index = 2;
    if tokens.get(index)?.starts_with('(') {
        while !tokens.get(index)?.ends_with(')') {
            index += 1;
        }
        index += 1;
    }
    let delimiter = tokens.get(index)?;
    (!delimiter.is_empty()).then(|| delimiter.to_owned())
}

/// Tokenize the bounded, non-literal portion of a LIST response. Quoted
/// strings may contain UTF-8, whitespace, and escaped quote/backslash bytes.
pub(super) fn tokens(line: &str) -> Option<Vec<String>> {
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
            offset += 1;
        }
        if offset == bytes.len() {
            break;
        }
        if bytes[offset] == b'"' {
            offset += 1;
            let mut value = Vec::new();
            let mut closed = false;
            while offset < bytes.len() {
                match bytes[offset] {
                    b'\\' => {
                        offset += 1;
                        value.push(*bytes.get(offset)?);
                        offset += 1;
                    }
                    b'"' => {
                        offset += 1;
                        closed = true;
                        break;
                    }
                    byte => {
                        value.push(byte);
                        offset += 1;
                    }
                }
            }
            if !closed {
                return None;
            }
            tokens.push(String::from_utf8(value).ok()?);
        } else {
            let start = offset;
            while bytes
                .get(offset)
                .is_some_and(|byte| !byte.is_ascii_whitespace())
            {
                offset += 1;
            }
            tokens.push(std::str::from_utf8(&bytes[start..offset]).ok()?.to_owned());
        }
    }
    Some(tokens)
}
