use std::{
    collections::{HashMap, hash_map::Entry},
    sync::Arc,
};

#[cfg(test)]
use sha2::{Digest, Sha256};

/// Parse the live verifier's metadata-only FETCH response. Body fingerprints
/// intentionally use a separate parser/API and are not produced here.
#[cfg(test)]
pub(super) fn parse_message_fetch_metadata_response_bytes(
    response: &[u8],
    mailbox: &str,
    uidvalidity: Option<u64>,
) -> Result<crate::core::ExtractedMessages, String> {
    parse_message_fetch_metadata_response_bytes_with_mailbox(
        response,
        Arc::from(mailbox),
        uidvalidity,
    )
}

pub(super) fn parse_message_fetch_metadata_response_bytes_with_mailbox(
    response: &[u8],
    mailbox: Arc<str>,
    uidvalidity: Option<u64>,
) -> Result<crate::core::ExtractedMessages, String> {
    let mut messages = HashMap::new();
    let mut offset = 0;
    while offset < response.len() {
        let Some(relative_line_end) = response[offset..]
            .windows(2)
            .position(|pair| pair == b"\r\n")
        else {
            break;
        };
        let line_end = offset + relative_line_end;
        let line = &response[offset..line_end];
        let next_line = line_end + 2;
        if !is_fetch_record_line_bytes(line) {
            offset = next_fetch_record_start_bytes(response, offset);
            continue;
        }
        let record_end = next_fetch_record_start_bytes(response, next_line);
        let record = &response[offset..record_end.min(response.len())];
        let first_line_end = record
            .windows(2)
            .position(|pair| pair == b"\r\n")
            .ok_or_else(|| "IMAP FETCH record had no line terminator".to_owned())?;
        let first_line = String::from_utf8_lossy(&record[..first_line_end]);
        let uid = fetch_number(&first_line, "UID")
            .ok_or_else(|| "IMAP FETCH record omitted UID".to_owned())?
            .to_string();
        let key = crate::core::MailboxMessageKey::with_shared_mailbox(
            Arc::clone(&mailbox),
            uidvalidity,
            uid.clone(),
        );
        match messages.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(crate::core::ExtractedMessage {
                    message_id: fetch_message_id_bytes(record).filter(|value| !value.is_empty()),
                    // UID is already the canonical key; retaining a second
                    // owned copy would materially inflate large live scans.
                    uid: None,
                    size_bytes: fetch_number(&first_line, "RFC822.SIZE"),
                    internal_date: fetch_quoted(&first_line, "INTERNALDATE"),
                });
            }
            Entry::Occupied(entry) => {
                return Err(format!("duplicate FETCH UID {}", entry.key().uid));
            }
        }
        offset = record_end;
    }
    Ok(messages)
}

/// Extract bounded SHA-256 fingerprints from a FETCH response containing
/// `BODY[]` literals. This is deliberately separate from the default metadata
/// parser: callers must explicitly opt into downloading message bodies and
/// provide a per-message bound before content evidence can be produced.
#[cfg(test)]
pub(super) fn parse_message_fetch_body_hashes_response_bytes(
    response: &[u8],
    mailbox: &str,
    uidvalidity: Option<u64>,
    max_body_bytes: usize,
) -> Result<HashMap<crate::core::MailboxMessageKey, String>, String> {
    let mut fingerprints = HashMap::new();
    let mut offset = 0;
    while offset < response.len() {
        let Some(relative_line_end) = response[offset..]
            .windows(2)
            .position(|pair| pair == b"\r\n")
        else {
            break;
        };
        let line_end = offset + relative_line_end;
        let line = &response[offset..line_end];
        let next_line = line_end + 2;
        if !is_fetch_record_line_bytes(line) {
            offset = next_fetch_record_start_bytes(response, offset);
            continue;
        }
        let record_end = next_fetch_record_start_bytes(response, next_line);
        let record = &response[offset..record_end.min(response.len())];
        let first_line_end = record
            .windows(2)
            .position(|pair| pair == b"\r\n")
            .ok_or_else(|| "IMAP body FETCH record had no line terminator".to_owned())?;
        let first_line = String::from_utf8_lossy(&record[..first_line_end]);
        let uid = fetch_number(&first_line, "UID")
            .ok_or_else(|| "IMAP body FETCH record omitted UID".to_owned())?;
        let body = fetch_body_literal(record)
            .ok_or_else(|| format!("IMAP body FETCH UID {uid} omitted BODY[] literal"))?;
        if body.len() > max_body_bytes {
            return Err(format!(
                "IMAP body FETCH UID {uid} exceeded the {max_body_bytes}-byte body-hash bound"
            ));
        }
        let digest = Sha256::digest(body);
        let key = crate::core::MailboxMessageKey::with_shared_mailbox(
            Arc::from(mailbox),
            uidvalidity,
            uid.to_string(),
        );
        let fingerprint = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if fingerprints.insert(key, fingerprint).is_some() {
            return Err(format!("duplicate body FETCH UID {uid}"));
        }
        offset = record_end;
    }
    Ok(fingerprints)
}

fn next_fetch_record_start_bytes(response: &[u8], mut offset: usize) -> usize {
    while offset < response.len() {
        let Some(relative_line_end) = response[offset..]
            .windows(2)
            .position(|pair| pair == b"\r\n")
        else {
            return response.len();
        };
        let line_end = offset + relative_line_end;
        let line = &response[offset..line_end];
        if is_fetch_record_line_bytes(line) {
            return offset;
        }
        offset = (line_end + 2).saturating_add(imap_literal_size_bytes(line).unwrap_or(0));
    }
    response.len()
}

fn is_fetch_record_line(line: &str) -> bool {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    fields.len() >= 4
        && fields[0] == "*"
        && fields[1].parse::<u64>().is_ok()
        && fields[2].eq_ignore_ascii_case("FETCH")
        && fields[3].starts_with('(')
}

fn is_fetch_record_line_bytes(line: &[u8]) -> bool {
    std::str::from_utf8(line).is_ok_and(is_fetch_record_line)
}

fn imap_literal_size_bytes(line: &[u8]) -> Option<usize> {
    let line = line.strip_suffix(b"}")?;
    let start = line.iter().rposition(|byte| *byte == b'{')?;
    std::str::from_utf8(&line[start + 1..]).ok()?.parse().ok()
}

fn fetch_number(line: &str, field: &str) -> Option<u64> {
    let start = find_ascii_case_insensitive(line, field)? + field.len();
    let value = line[start..].trim_start();
    let end = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    (end > 0).then(|| value[..end].parse().ok()).flatten()
}

fn fetch_quoted(line: &str, field: &str) -> Option<String> {
    let start = find_ascii_case_insensitive(line, field)? + field.len();
    let value = line[start..].trim_start().strip_prefix('"')?;
    let end = value.find('"')?;
    Some(value[..end].to_owned())
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|part| part.eq_ignore_ascii_case(needle.as_bytes()))
}

fn fetch_message_id_bytes(record: &[u8]) -> Option<String> {
    let marker = b"BODY[HEADER.FIELDS (MESSAGE-ID)]";
    let marker_start = record
        .windows(marker.len())
        .position(|part| part.eq_ignore_ascii_case(marker))?
        + marker.len();
    let literal = record[marker_start..].strip_prefix(b" ")?;
    let literal_end = literal.windows(3).position(|part| part == b"}\r\n")?;
    let size = std::str::from_utf8(&literal[1..literal_end])
        .ok()?
        .parse::<usize>()
        .ok()?;
    let body_start = marker_start
        + record[marker_start..]
            .windows(2)
            .position(|part| part == b"\r\n")?
        + 2;
    let body = record.get(body_start..body_start + size)?;
    parse_message_id_header(&String::from_utf8_lossy(body))
}

#[cfg(test)]
fn fetch_body_literal(record: &[u8]) -> Option<&[u8]> {
    let marker = b"BODY[]";
    let marker_start = record
        .windows(marker.len())
        .position(|part| part.eq_ignore_ascii_case(marker))?
        + marker.len();
    let literal = record[marker_start..].strip_prefix(b" ")?;
    let literal_end = literal.windows(3).position(|part| part == b"}\r\n")?;
    let size = std::str::from_utf8(&literal[1..literal_end])
        .ok()?
        .parse::<usize>()
        .ok()?;
    let body_start = marker_start
        + record[marker_start..]
            .windows(2)
            .position(|part| part == b"\r\n")?
        + 2;
    record.get(body_start..body_start.checked_add(size)?)
}

pub(super) fn parse_message_id_header(body: &str) -> Option<String> {
    let mut message_id: Option<String> = None;
    for line in body.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(value) = message_id.as_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("message-id") {
            message_id = Some(value.trim().to_owned());
        }
    }
    message_id.map(|value| value.split_whitespace().collect::<Vec<_>>().join(" "))
}
