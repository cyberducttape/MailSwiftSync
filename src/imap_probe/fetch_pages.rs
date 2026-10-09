//! Bounded UID search, FETCH page planning, and exact coverage validation.

use super::{MAX_MESSAGE_FETCH_RESPONSE_BYTES, MAX_UID_SET_BYTES, MESSAGE_FETCH_PAGE_SIZE};

const MAX_IMAP_UID: u64 = u32::MAX as u64;
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

fn validate_imap_uid(uid: u64, host: &str, mailbox: &str) -> Result<u64, String> {
    if uid == 0 || uid > MAX_IMAP_UID {
        return Err(format!(
            "{host}: folder {mailbox} returned UID {uid} outside the IMAP 1..={} domain",
            MAX_IMAP_UID
        ));
    }
    Ok(uid)
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
    let mut uids = Vec::new();
    for line in response.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("*")
            || fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .is_none()
            || !fields
                .next()
                .is_some_and(|value| value.eq_ignore_ascii_case("FETCH"))
        {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let _asterisk = tokens.next();
        let sequence = tokens
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                format!("{host}: FETCH {mailbox} returned an invalid sequence number")
            })?;
        if !(sequence_start..=sequence_end).contains(&sequence) {
            return Err(format!(
                "{host}: FETCH {mailbox} returned sequence {sequence} outside requested range {sequence_start}:{sequence_end}"
            ));
        }
        let mut uid = None;
        while let Some(token) = tokens.next() {
            if token.trim_start_matches('(').eq_ignore_ascii_case("UID") {
                uid = tokens
                    .next()
                    .and_then(|value| value.trim_end_matches([')', ',']).parse::<u64>().ok());
                break;
            }
        }
        let uid = uid.ok_or_else(|| {
            format!("{host}: FETCH {mailbox} sequence {sequence} did not return a valid UID")
        })?;
        uids.push(validate_imap_uid(uid, host, mailbox)?);
    }
    uids.sort_unstable();
    if uids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(format!("{host}: FETCH {mailbox} returned duplicate UIDs"));
    }
    Ok(uids)
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
