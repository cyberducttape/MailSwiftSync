//! Bounded UID search, FETCH page planning, and exact coverage validation.

use super::{MAX_MESSAGE_FETCH_RESPONSE_BYTES, MAX_UID_SET_BYTES, MESSAGE_FETCH_PAGE_SIZE};

use super::uid::validate_imap_uid;
use crate::core;
#[cfg(test)]
use crate::imap_protocol::is_untagged_response;
use std::collections::HashSet;

/// Adaptive UID FETCH page sizing. Growth is capped at 2x, contraction is
/// immediate, and metadata/body transfers use separate response envelopes.
#[derive(Debug, Clone)]
pub(super) struct FetchPagePlanner {
    size: usize,
    min: usize,
    max: usize,
    target_response_bytes: usize,
}

impl FetchPagePlanner {
    pub(super) fn metadata() -> Self {
        Self {
            size: MESSAGE_FETCH_PAGE_SIZE as usize,
            min: 32,
            max: 1024,
            target_response_bytes: 1024 * 1024,
        }
    }

    pub(super) fn body() -> Self {
        Self {
            size: 8,
            min: 1,
            max: 64,
            target_response_bytes: MAX_MESSAGE_FETCH_RESPONSE_BYTES / 4,
        }
    }

    pub(super) fn size(&self) -> usize {
        self.size
    }

    pub(super) fn observe(&mut self, messages: usize, response_bytes: usize) {
        if messages == 0 {
            return;
        }
        let per_message = (response_bytes / messages).max(1);
        let ideal = self.target_response_bytes / per_message;
        self.size = ideal
            .min(self.size.saturating_mul(2))
            .clamp(self.min, self.max);
    }
}

/// Encode the longest prefix of sorted, unique UIDs as a compact IMAP set.
pub(super) fn encode_uid_page(uids: &[u64], max_count: usize) -> (usize, String) {
    use std::fmt::Write as _;
    let mut set = String::new();
    let mut count = 0;
    while count < uids.len().min(max_count.max(1)) {
        let start = uids[count];
        let mut end_index = count;
        while end_index + 1 < uids.len().min(max_count.max(1)) {
            let Some(expected_next) = uids[end_index].checked_add(1) else {
                break;
            };
            if uids[end_index + 1] != expected_next {
                break;
            }
            end_index += 1;
        }
        let mut item = String::new();
        if end_index == count {
            let _ = write!(item, "{start}");
        } else {
            let _ = write!(item, "{start}:{}", uids[end_index]);
        }
        let separator = usize::from(!set.is_empty());
        if count > 0 && set.len() + separator + item.len() > MAX_UID_SET_BYTES {
            break;
        }
        if separator == 1 {
            set.push(',');
        }
        set.push_str(&item);
        count = end_index + 1;
    }
    (count, set)
}

#[cfg(test)]
pub(super) fn parse_uid_search_response(
    response: &str,
    host: &str,
    mailbox: &str,
) -> Result<Vec<u64>, String> {
    let line = response
        .lines()
        .find(|line| is_untagged_response(line, "SEARCH"))
        .ok_or_else(|| format!("{host}: SEARCH {mailbox} did not return a UID list"))?;
    let mut uids = line
        .split_whitespace()
        .skip(2)
        .map(|uid| {
            let uid = uid
                .parse::<u64>()
                .map_err(|_| format!("{host}: SEARCH {mailbox} returned invalid UID {uid}"))?;
            validate_imap_uid(uid, host, mailbox)
        })
        .collect::<Result<Vec<_>, _>>()?;
    uids.sort_unstable();
    Ok(uids)
}

/// Parse the UID values returned by a sequence-number `FETCH (... UID)`
/// page. Unlike UID SEARCH, this command's request size is based on EXISTS,
/// so sparse historical UID spaces do not create one request per empty UID
/// window.
pub(super) fn parse_uid_fetch_response(
    response: &str,
    host: &str,
    mailbox: &str,
    sequence_start: u64,
    sequence_end: u64,
) -> Result<Vec<u64>, String> {
    let mut sequence_uids = Vec::new();
    let mut sequences = HashSet::new();
    for line in response.lines() {
        let mut tokens = line.split_whitespace();
        if tokens.next() != Some("*") {
            continue;
        }
        let Some(sequence_token) = tokens.next() else {
            continue;
        };
        if sequence_token.eq_ignore_ascii_case("FETCH") {
            return Err(format!(
                "{host}: FETCH {mailbox} returned a FETCH record without a sequence number"
            ));
        }
        let Some(response_type) = tokens.next() else {
            if sequence_token.parse::<u64>().is_ok() {
                return Err(format!(
                    "{host}: FETCH {mailbox} returned a malformed untagged response"
                ));
            }
            continue;
        };
        if !response_type.eq_ignore_ascii_case("FETCH") {
            continue;
        }
        let sequence = sequence_token
            .parse::<u64>()
            .map_err(|_| format!("{host}: FETCH {mailbox} returned an invalid sequence number"))?;
        if !(sequence_start..=sequence_end).contains(&sequence) {
            return Err(format!(
                "{host}: FETCH {mailbox} returned sequence {sequence} outside requested range {sequence_start}:{sequence_end}"
            ));
        }
        if !sequences.insert(sequence) {
            return Err(format!(
                "{host}: FETCH {mailbox} returned duplicate sequence number {sequence}"
            ));
        }
        let data_start = line
            .find(response_type)
            .map(|index| index + response_type.len());
        let uid = data_start
            .and_then(|offset| parse_parenthesized_fetch_uid(&line[offset..]))
            .ok_or_else(|| {
                format!(
                    "{host}: FETCH {mailbox} sequence {sequence} returned a malformed or missing UID data item"
                )
            })?;
        sequence_uids.push((sequence, validate_imap_uid(uid, host, mailbox)?));
    }
    if sequence_start == 0 || sequence_end < sequence_start {
        return Err(format!(
            "{host}: FETCH {mailbox} was given an invalid sequence range {sequence_start}:{sequence_end}"
        ));
    }
    let expected_count = sequence_end
        .checked_sub(sequence_start)
        .and_then(|count| count.checked_add(1))
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| format!("{host}: FETCH {mailbox} sequence range is too large"))?;
    if sequences.len() != expected_count {
        return Err(format!(
            "{host}: FETCH {mailbox} returned {} of {expected_count} requested sequence numbers ({sequence_start}:{sequence_end})",
            sequences.len()
        ));
    }
    sequence_uids.sort_unstable_by_key(|(sequence, _)| *sequence);
    if sequence_uids.windows(2).any(|pair| pair[0].1 >= pair[1].1) {
        return Err(format!(
            "{host}: FETCH {mailbox} returned UIDs inconsistent with sequence order"
        ));
    }
    let uids = sequence_uids
        .into_iter()
        .map(|(_, uid)| uid)
        .collect::<Vec<_>>();
    Ok(uids)
}

/// Parse one UID from the parenthesized FETCH data list, rejecting malformed
/// framing and duplicate UID items instead of allowing a partial record to
/// contribute to exact sequence coverage.
fn parse_parenthesized_fetch_uid(data: &str) -> Option<u64> {
    let data = data.trim();
    let bytes = data.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }

    let mut offset = 1usize;
    let mut field: Option<&[u8]> = None;
    let mut uid = None;
    let mut closed = false;
    while offset < bytes.len() {
        skip_fetch_whitespace(bytes, &mut offset);
        if bytes.get(offset) == Some(&b')') {
            offset += 1;
            if field.is_some() {
                return None;
            }
            closed = true;
            break;
        }
        let component = parse_fetch_data_component(bytes, &mut offset)?;
        if let Some(name) = field.take() {
            if name.eq_ignore_ascii_case(b"UID") {
                let FetchDataComponent::Atom(value) = component else {
                    return None;
                };
                if uid.is_some() || !value.iter().all(u8::is_ascii_digit) {
                    return None;
                }
                uid = Some(std::str::from_utf8(value).ok()?.parse::<u64>().ok()?);
            }
        } else {
            let FetchDataComponent::Atom(name) = component else {
                return None;
            };
            field = Some(name);
        }
    }
    if !closed || !bytes[offset..].iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    uid
}

enum FetchDataComponent<'a> {
    Atom(&'a [u8]),
    Quoted,
    List,
}

fn parse_fetch_data_component<'a>(
    bytes: &'a [u8],
    offset: &mut usize,
) -> Option<FetchDataComponent<'a>> {
    match *bytes.get(*offset)? {
        b'(' => {
            let mut depth = 0usize;
            let mut quoted = false;
            let mut escaped = false;
            while let Some(&byte) = bytes.get(*offset) {
                *offset += 1;
                if quoted {
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        quoted = false;
                    }
                    continue;
                }
                match byte {
                    b'"' => quoted = true,
                    b'(' => depth = depth.checked_add(1)?,
                    b')' => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            return Some(FetchDataComponent::List);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        b'"' => {
            *offset += 1;
            let mut escaped = false;
            while let Some(&byte) = bytes.get(*offset) {
                *offset += 1;
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    return Some(FetchDataComponent::Quoted);
                }
            }
            None
        }
        b')' => None,
        _ => {
            let start = *offset;
            while bytes.get(*offset).is_some_and(|byte| {
                !byte.is_ascii_whitespace() && !matches!(byte, b'(' | b')' | b'"')
            }) {
                *offset += 1;
            }
            (*offset > start).then_some(FetchDataComponent::Atom(&bytes[start..*offset]))
        }
    }
}

fn skip_fetch_whitespace(bytes: &[u8], offset: &mut usize) {
    while bytes.get(*offset).is_some_and(u8::is_ascii_whitespace) {
        *offset += 1;
    }
}

pub(super) fn validate_fetch_page_coverage(
    messages: &core::ExtractedMessages,
    requested_uids: &[u64],
    host: &str,
    mailbox: &str,
) -> Result<(), String> {
    let parsed_uids = messages
        .keys()
        .map(|key| {
            if key.mailbox.as_ref() != mailbox {
                return Err(format!(
                    "{host}: folder {mailbox}: FETCH returned a record for mailbox {}",
                    key.mailbox
                ));
            }
            key.uid.parse::<u64>().map_err(|_| {
                format!(
                    "{host}: folder {mailbox}: FETCH returned invalid UID {}",
                    key.uid
                )
            })
        })
        .collect::<Result<HashSet<_>, _>>()?;
    let requested_uids = requested_uids.iter().copied().collect::<HashSet<_>>();
    if parsed_uids != requested_uids {
        return Err(format!(
            "{host}: folder {mailbox}: FETCH coverage mismatch (requested {}, parsed {})",
            requested_uids.len(),
            parsed_uids.len()
        ));
    }
    Ok(())
}
