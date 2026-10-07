use std::{
    collections::{HashMap, hash_map::Entry},
    sync::Arc,
};

use sha2::{Digest, Sha256};

use super::literal_framing::{ResponseFrame, split_responses};

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
    for record in fetch_records(response) {
        let record = record?;
        let uid = fetch_number(&record.protocol_text, "UID")
            .ok_or_else(|| "IMAP FETCH record omitted UID".to_owned())?
            .to_string();
        let key = crate::core::MailboxMessageKey::with_shared_mailbox(
            Arc::clone(&mailbox),
            uidvalidity,
            uid,
        );
        match messages.entry(key) {
            Entry::Vacant(entry) => {
                let header = record
                    .frame
                    .literal_after(
                        b"BODY[HEADER.FIELDS (MESSAGE-ID FROM TO CC SUBJECT DATE CONTENT-TYPE)]",
                    )
                    .or_else(|| {
                        record
                            .frame
                            .literal_after(b"BODY[HEADER.FIELDS (MESSAGE-ID)]")
                    });
                let header_text = header.map(|value| String::from_utf8_lossy(value));
                let message_id = header_text
                    .as_deref()
                    .and_then(parse_message_id_header)
                    .filter(|value| !value.is_empty())
                    .or_else(|| header_text.as_deref().and_then(header_fingerprint));
                entry.insert(crate::core::ExtractedMessage {
                    message_id,
                    // UID is already the canonical key; retaining a second
                    // owned copy would materially inflate large live scans.
                    uid: None,
                    size_bytes: fetch_number(&record.protocol_text, "RFC822.SIZE"),
                    internal_date: fetch_quoted(&record.protocol_text, "INTERNALDATE"),
                    flags: fetch_flags(&record.protocol_text),
                });
            }
            Entry::Occupied(entry) => {
                return Err(format!("duplicate FETCH UID {}", entry.key().uid));
            }
        }
    }
    Ok(messages)
}

/// Extract bounded SHA-256 fingerprints from a FETCH response containing
/// `BODY[]` literals. This is deliberately separate from the default metadata
/// parser: callers must explicitly opt into downloading message bodies and
/// provide a per-message bound before content evidence can be produced.
pub(super) fn parse_message_fetch_body_hashes_response_bytes(
    response: &[u8],
    mailbox: &str,
    uidvalidity: Option<u64>,
    max_body_bytes: usize,
) -> Result<BodyHashPage, String> {
    let mailbox: Arc<str> = Arc::from(mailbox);
    let mut fingerprints = HashMap::new();
    let mut total_bytes = 0usize;
    for record in fetch_records(response) {
        let record = record?;
        let uid = fetch_number(&record.protocol_text, "UID")
            .ok_or_else(|| "IMAP body FETCH record omitted UID".to_owned())?;
        let body = record
            .frame
            .literal_after(b"BODY[]")
            .or_else(|| record.frame.literal_after(b"BODY.PEEK[]"))
            .ok_or_else(|| format!("IMAP body FETCH UID {uid} omitted BODY[] literal"))?;
        if body.len() > max_body_bytes {
            // `VerificationLimit::BodyHashMessageSize`, spelled out because
            // the fuzz harness compiles this parser without `core`.
            return Err(format!(
                "[verification_limit=body_hash_message_size] IMAP body FETCH UID {uid} exceeded the {max_body_bytes}-byte body-hash bound"
            ));
        }
        total_bytes = total_bytes.saturating_add(body.len());
        let digest = Sha256::digest(body);
        let key = crate::core::MailboxMessageKey::with_shared_mailbox(
            Arc::clone(&mailbox),
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
    }
    Ok(BodyHashPage {
        fingerprints,
        total_bytes,
    })
}

#[derive(Debug)]
pub(super) struct BodyHashPage {
    pub(super) fingerprints: HashMap<crate::core::MailboxMessageKey, String>,
    pub(super) total_bytes: usize,
}

/// An untagged FETCH response framed by the shared literal-aware splitter.
/// `protocol_text` holds only bytes outside literals, so item lookups can never
/// match message content.
struct FetchRecord<'a> {
    frame: ResponseFrame<'a>,
    protocol_text: String,
}

fn fetch_records(response: &[u8]) -> impl Iterator<Item = Result<FetchRecord<'_>, String>> {
    split_responses(response).filter_map(|frame| {
        let frame = match frame {
            Ok(frame) => frame,
            Err(error) => return Some(Err(error)),
        };
        let first = String::from_utf8_lossy(frame.protocol.first()?);
        if !is_fetch_record_line(&first) {
            return None;
        }
        let protocol_text = frame
            .protocol
            .iter()
            .map(|segment| String::from_utf8_lossy(segment))
            .collect::<Vec<_>>()
            .join(" ");
        Some(Ok(FetchRecord {
            frame,
            protocol_text,
        }))
    })
}

fn is_fetch_record_line(line: &str) -> bool {
    let mut fields = line.split_whitespace();
    fields.next() == Some("*")
        && fields
            .next()
            .is_some_and(|number| number.parse::<u64>().is_ok())
        && fields
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("FETCH"))
        && fields.next().is_some_and(|items| items.starts_with('('))
}

fn fetch_number(text: &str, field: &str) -> Option<u64> {
    let value = text[find_item(text, field)?..].trim_start();
    let end = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    (end > 0).then(|| value[..end].parse().ok()).flatten()
}

fn fetch_quoted(text: &str, field: &str) -> Option<String> {
    let value = text[find_item(text, field)?..]
        .trim_start()
        .strip_prefix('"')?;
    let end = value.find('"')?;
    Some(value[..end].to_owned())
}

/// Read a record's FLAGS item into the canonical flag-set form. Custom
/// keywords are ordinary FLAGS atoms in IMAP; there is no portable KEYWORDS
/// FETCH item. `None` means the record carried no parseable FLAGS list.
fn fetch_flags(text: &str) -> Option<String> {
    let value = text[find_item(text, "FLAGS")?..].trim_start();
    let value = value.strip_prefix('(')?;
    // Flag atoms cannot contain `)` (an atom-special), so the first closing
    // parenthesis ends the list.
    let end = value.find(')')?;
    Some(canonical_flag_set(value[..end].split_whitespace()))
}

/// Normalize IMAP flags for comparison: flags are case-insensitive, so
/// the five RFC 9051 system flags take their canonical spelling and every
/// other atom is ASCII-lowercased. `\Recent` is session state that a client
/// can never store and is dropped. The result is sorted, de-duplicated, and
/// space-separated; an empty string is a known-empty flag set.
pub(crate) fn canonical_flag_set<'a>(atoms: impl IntoIterator<Item = &'a str>) -> String {
    const SYSTEM: [&str; 5] = ["\\Answered", "\\Deleted", "\\Draft", "\\Flagged", "\\Seen"];
    let mut flags = atoms
        .into_iter()
        .filter(|atom| !atom.is_empty() && !atom.eq_ignore_ascii_case("\\Recent"))
        .map(|atom| {
            SYSTEM
                .iter()
                .find(|system| system.eq_ignore_ascii_case(atom))
                .map_or_else(|| atom.to_ascii_lowercase(), |system| (*system).to_owned())
        })
        .collect::<Vec<_>>();
    flags.sort_unstable();
    flags.dedup();
    flags.join(" ")
}

/// Return the offset just past a whole FETCH item name. The name must start
/// the item list or follow a space, and be followed by a space, so `UID` never
/// matches inside another atom.
fn find_item(text: &str, field: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let field = field.as_bytes();
    (0..=bytes.len().checked_sub(field.len())?)
        .find(|&start| {
            bytes[start..start + field.len()].eq_ignore_ascii_case(field)
                && (start == 0 || matches!(bytes[start - 1], b' ' | b'('))
                && bytes.get(start + field.len()) == Some(&b' ')
        })
        .map(|start| start + field.len())
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

/// Produce a deterministic fallback identity from selected RFC822 headers.
/// The prefix keeps this value distinct from a real Message-ID and makes the
/// evidence level visible to callers. Duplicate messages with identical
/// selected headers remain ambiguous and are handled by the existing multiset
/// reconciliation.
pub(super) fn header_fingerprint(body: &str) -> Option<String> {
    const SELECTED: [&str; 7] = [
        "from",
        "to",
        "cc",
        "subject",
        "date",
        "message-id",
        "content-type",
    ];
    let mut values = HashMap::<&str, String>::new();
    let mut current: Option<&str> = None;
    for line in body.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(name) = current {
                values.entry(name).and_modify(|value| {
                    value.push(' ');
                    value.push_str(line.trim());
                });
            }
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            current = None;
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let Some(name) = SELECTED.iter().copied().find(|selected| *selected == name) else {
            current = None;
            continue;
        };
        current = Some(name);
        values.insert(name, value.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    if values.is_empty() {
        return None;
    }
    let canonical = SELECTED
        .iter()
        .map(|name| {
            format!(
                "{name}:{}",
                values.get(name).map(String::as_str).unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let digest = Sha256::digest(canonical.as_bytes());
    Some(format!(
        "header-fingerprint:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}
