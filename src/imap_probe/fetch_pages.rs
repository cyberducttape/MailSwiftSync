//! Bounded UID search, FETCH page planning, and exact coverage validation.

use super::{MAX_MESSAGE_FETCH_RESPONSE_BYTES, MAX_UID_SET_BYTES, MESSAGE_FETCH_PAGE_SIZE};
use crate::{core, imap_protocol::is_untagged_response};
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
        while end_index + 1 < uids.len().min(max_count.max(1))
            && uids[end_index + 1] == uids[end_index] + 1
        {
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
            uid.parse::<u64>()
                .map_err(|_| format!("{host}: SEARCH {mailbox} returned invalid UID {uid}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    uids.sort_unstable();
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
