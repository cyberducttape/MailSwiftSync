//! Parsing helpers for non-literal IMAP LIST records.

/// Parse the mailbox-name field from a normal, non-literal LIST record.
/// Extended LIST data may follow this field; literal names are handled after
/// their bounded bytes have been collected.
pub(super) fn mailbox_name(line: &str) -> Option<String> {
    let (tokens, mailbox_index) = list_fields(line)?;
    tokens
        .get(mailbox_index)
        .filter(|value| !value.starts_with('{'))
        .cloned()
}

/// Determine whether a literal header is in the fixed mailbox-name field or
/// later extension data. `prefix` ends immediately before the literal header.
/// `Some(true)` means the mailbox field is next, `Some(false)` means it was
/// already present, and `None` means the fixed LIST prefix was malformed or
/// incomplete.
pub(super) fn literal_is_mailbox_name(prefix: &str) -> Option<bool> {
    let (tokens, mailbox_index) = list_field_layout(prefix)?;
    (tokens.len() >= mailbox_index).then_some(tokens.len() == mailbox_index)
}

pub(super) fn delimiter(line: &str) -> Option<String> {
    let (tokens, mailbox_index) = list_fields(line)?;
    let delimiter = tokens.get(mailbox_index - 1)?;
    (!delimiter.is_empty() && !delimiter.eq_ignore_ascii_case("NIL")).then(|| delimiter.to_owned())
}

/// Return tokens and the mailbox-name index for `* LIST (attrs) delimiter
/// mailbox [extended-data]`. Only the fixed LIST prefix determines the
/// mailbox name; RFC 5258 extension fields are deliberately ignored.
fn list_fields(line: &str) -> Option<(Vec<String>, usize)> {
    let (tokens, mailbox_index) = list_field_layout(line)?;
    (tokens.len() > mailbox_index).then_some((tokens, mailbox_index))
}

fn list_field_layout(line: &str) -> Option<(Vec<String>, usize)> {
    let tokens = tokens(line)?;
    if tokens.first()?.as_str() != "*" || !tokens.get(1)?.eq_ignore_ascii_case("LIST") {
        return None;
    }
    let mut index = 2;
    if tokens.get(index)?.starts_with('(') {
        while !tokens.get(index)?.ends_with(')') {
            index += 1;
        }
        index += 1;
    }
    // The delimiter and mailbox name are the next two fields. The LIST
    // grammar requires an attribute list, so the mailbox cannot precede it.
    if index < 3 {
        return None;
    }
    Some((tokens, index + 1))
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
