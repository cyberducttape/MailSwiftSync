//! Pre-migration message census: per-folder `STATUS` counts summed for one
//! account, so verification capacity is visible before anything transfers.
//!
//! The census counts a message once per folder that holds it, exactly as
//! message-level verification fetches and counts records. It is bounded in
//! folders and time and reports itself incomplete rather than guessing.

use super::{
    MessageFetchBudget, imap_quote,
    literal_framing::{split_responses, tagged_completion},
    protocol::{read_imap_tagged_bytes_with_budget, write_imap_command},
};
use crate::core::MessageCensus;
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

/// Accounts with more selectable folders than this are not counted; the
/// readiness probe must stay quick, and the operator is told why.
pub(super) const MAX_CENSUS_FOLDERS: usize = 5_000;
/// Time the census may add to the readiness probe.
pub(super) const CENSUS_TIME_BUDGET: Duration = Duration::from_secs(30);
/// One STATUS response is one short line; anything larger is not one.
const MAX_STATUS_RESPONSE_BYTES: usize = 64 * 1024;

/// Count messages (and bytes, when `with_size`) across `folders`. Protocol
/// rejections and unparseable replies leave the census incomplete; I/O and
/// probe-budget failures are returned as errors because the session can no
/// longer be trusted for further commands.
pub(super) fn count_account_messages<S: Read + Write>(
    stream: &mut S,
    host: &str,
    folders: &[&str],
    with_size: bool,
    budget: &MessageFetchBudget<'_>,
    deadline: Instant,
) -> Result<MessageCensus, String> {
    let incomplete = |folders_counted, reason: String| MessageCensus {
        messages: None,
        bytes: None,
        folders_counted,
        incomplete_reason: Some(reason),
    };
    if folders.len() > MAX_CENSUS_FOLDERS {
        return Ok(incomplete(
            0,
            format!(
                "{} selectable folders exceed the {MAX_CENSUS_FOLDERS}-folder census limit",
                folders.len()
            ),
        ));
    }
    let attributes = if with_size {
        "(MESSAGES SIZE)"
    } else {
        "(MESSAGES)"
    };
    let mut buffer = [0; 4096];
    let mut messages = 0_u64;
    let mut bytes = with_size.then_some(0_u64);
    for (index, folder) in folders.iter().enumerate() {
        budget.check()?;
        if Instant::now() >= deadline {
            return Ok(incomplete(
                index,
                format!(
                    "counted {index} of {} folders before the {}-second census time limit",
                    folders.len(),
                    CENSUS_TIME_BUDGET.as_secs()
                ),
            ));
        }
        let Ok(quoted) = imap_quote(folder) else {
            return Ok(incomplete(
                index,
                "a folder name cannot be sent as a quoted STATUS argument".into(),
            ));
        };
        let tag = format!("c{:05}", index + 1);
        write_imap_command(
            stream,
            format!("{tag} STATUS {quoted} {attributes}\r\n").as_bytes(),
            Some(budget),
            &format!("{host}: could not write STATUS"),
        )?;
        let response = read_imap_tagged_bytes_with_budget(
            stream,
            &tag,
            &mut buffer,
            MAX_STATUS_RESPONSE_BYTES,
            budget,
        )
        .map_err(|error| format!("{host}: message census failed: {error}"))?;
        let (folder_messages, folder_bytes) = match parse_status_response(&response, &tag) {
            Ok(counts) => counts,
            Err(reason) => return Ok(incomplete(index, reason)),
        };
        messages = messages.saturating_add(folder_messages);
        bytes = match (bytes, folder_bytes) {
            (Some(total), Some(size)) => Some(total.saturating_add(size)),
            _ => None,
        };
    }
    Ok(MessageCensus {
        messages: Some(messages),
        bytes,
        folders_counted: folders.len(),
        incomplete_reason: None,
    })
}

/// Parse one tagged STATUS exchange into (MESSAGES, SIZE). The status
/// attribute list is the last parenthesized group of the untagged response,
/// after the mailbox name, so a name that itself contains parentheses (or is
/// sent as a literal) cannot supply the counts.
pub(super) fn parse_status_response(
    response: &[u8],
    tag: &str,
) -> Result<(u64, Option<u64>), String> {
    let completion =
        tagged_completion(response, tag).ok_or("STATUS reply has no tagged completion")?;
    let status = completion
        .get(tag.len() + 1..)
        .and_then(|rest| rest.split(|byte| *byte == b' ').next())
        .unwrap_or_default();
    if !status.eq_ignore_ascii_case(b"OK") {
        return Err(format!(
            "the server refused STATUS: {}",
            String::from_utf8_lossy(completion)
                .chars()
                .filter(|character| !character.is_control())
                .take(200)
                .collect::<String>()
        ));
    }
    let mut counts = None;
    for frame in split_responses(response) {
        let frame = frame?;
        let Some(first) = frame.protocol.first() else {
            continue;
        };
        if !first
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"* STATUS "))
        {
            continue;
        }
        if counts.is_some() {
            return Err("STATUS reply carried more than one STATUS response".into());
        }
        let last = frame.protocol.last().copied().unwrap_or_default();
        let last = std::str::from_utf8(last).map_err(|_| "STATUS reply is not valid UTF-8")?;
        let list = last
            .trim_end()
            .strip_suffix(')')
            .and_then(|body| body.rfind('(').map(|open| &body[open + 1..]))
            .ok_or("STATUS reply has no attribute list")?;
        let mut messages = None;
        let mut size = None;
        let mut items = list.split_whitespace();
        while let Some(name) = items.next() {
            let value = items
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or("STATUS attribute has no numeric value")?;
            if name.eq_ignore_ascii_case("MESSAGES") {
                messages = Some(value);
            } else if name.eq_ignore_ascii_case("SIZE") {
                size = Some(value);
            }
        }
        counts = Some((messages.ok_or("STATUS reply omitted MESSAGES")?, size));
    }
    counts.ok_or_else(|| "STATUS reply carried no STATUS response".into())
}

#[cfg(test)]
mod tests {
    use super::{count_account_messages, parse_status_response};
    use std::{
        io::{Cursor, Read, Write},
        sync::atomic::AtomicBool,
        time::{Duration, Instant},
    };

    #[test]
    fn status_counts_come_from_the_trailing_attribute_list() {
        assert_eq!(
            parse_status_response(
                b"* STATUS \"Archive (MESSAGES 1)\" (MESSAGES 42 SIZE 9000)\r\nc00001 OK done\r\n",
                "c00001"
            ),
            Ok((42, Some(9000)))
        );
        assert_eq!(
            parse_status_response(
                b"* STATUS {14}\r\nx (MESSAGES 1) (MESSAGES 7)\r\nc00002 OK\r\n",
                "c00002"
            ),
            Ok((7, None))
        );
        assert_eq!(
            parse_status_response(b"* status INBOX (messages 3)\r\nc00003 ok\r\n", "c00003"),
            Ok((3, None))
        );
    }

    #[test]
    fn refused_or_malformed_status_replies_are_errors() {
        for (response, tag) in [
            (&b"c00001 NO [NONEXISTENT] no such folder\r\n"[..], "c00001"),
            (
                &b"* STATUS INBOX (UIDNEXT 4)\r\nc00001 OK\r\n"[..],
                "c00001",
            ),
            (
                &b"* STATUS INBOX (MESSAGES x)\r\nc00001 OK\r\n"[..],
                "c00001",
            ),
            (
                &b"* STATUS INBOX (MESSAGES 1)\r\n* STATUS Sent (MESSAGES 2)\r\nc00001 OK\r\n"[..],
                "c00001",
            ),
            (&b"c00001 OK nothing\r\n"[..], "c00001"),
        ] {
            assert!(
                parse_status_response(response, tag).is_err(),
                "{}",
                String::from_utf8_lossy(response)
            );
        }
    }

    /// Replies to each STATUS in turn and records what was sent.
    struct ScriptedServer {
        replies: Vec<Vec<u8>>,
        pending: Cursor<Vec<u8>>,
        sent: Vec<u8>,
    }

    impl Read for ScriptedServer {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.pending.read(buffer)
        }
    }

    impl Write for ScriptedServer {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.sent.extend_from_slice(data);
            if data.ends_with(b"\r\n") && !self.replies.is_empty() {
                self.pending = Cursor::new(self.replies.remove(0));
            }
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn census(
        replies: Vec<&[u8]>,
        folders: &[&str],
        with_size: bool,
        deadline: Instant,
    ) -> (Result<crate::core::MessageCensus, String>, String) {
        let cancel = AtomicBool::new(false);
        let budget = super::super::MessageFetchBudget::for_probe(Duration::from_secs(30), &cancel);
        let mut server = ScriptedServer {
            replies: replies.into_iter().map(<[u8]>::to_vec).collect(),
            pending: Cursor::new(Vec::new()),
            sent: Vec::new(),
        };
        let result =
            count_account_messages(&mut server, "host", folders, with_size, &budget, deadline);
        (result, String::from_utf8(server.sent).unwrap())
    }

    #[test]
    fn census_sums_messages_and_sizes_across_folders() {
        let (result, sent) = census(
            vec![
                b"* STATUS INBOX (MESSAGES 10 SIZE 1000)\r\nc00001 OK\r\n",
                b"* STATUS \"Sent Items\" (MESSAGES 5 SIZE 500)\r\nc00002 OK\r\n",
            ],
            &["INBOX", "Sent Items"],
            true,
            Instant::now() + Duration::from_secs(30),
        );
        let census = result.unwrap();
        assert_eq!(census.messages, Some(15));
        assert_eq!(census.bytes, Some(1500));
        assert_eq!(census.folders_counted, 2);
        assert_eq!(census.incomplete_reason, None);
        assert_eq!(
            sent,
            "c00001 STATUS \"INBOX\" (MESSAGES SIZE)\r\nc00002 STATUS \"Sent Items\" (MESSAGES SIZE)\r\n"
        );
    }

    #[test]
    fn a_refused_folder_leaves_the_census_incomplete_not_undercounted() {
        let (result, _) = census(
            vec![
                b"* STATUS INBOX (MESSAGES 10)\r\nc00001 OK\r\n",
                b"c00002 NO [NONEXISTENT] gone\r\n",
            ],
            &["INBOX", "Gone"],
            false,
            Instant::now() + Duration::from_secs(30),
        );
        let census = result.unwrap();
        assert_eq!(census.messages, None);
        assert_eq!(census.folders_counted, 1);
        assert!(census.incomplete_reason.unwrap().contains("refused STATUS"));
    }

    #[test]
    fn missing_sizes_or_an_expired_time_budget_are_reported() {
        let (result, _) = census(
            vec![
                b"* STATUS INBOX (MESSAGES 1 SIZE 10)\r\nc00001 OK\r\n",
                b"* STATUS Sent (MESSAGES 2)\r\nc00002 OK\r\n",
            ],
            &["INBOX", "Sent"],
            true,
            Instant::now() + Duration::from_secs(30),
        );
        let census = result.unwrap();
        assert_eq!((census.messages, census.bytes), (Some(3), None));

        let (result, sent) = census_expired();
        let census = result.unwrap();
        assert_eq!(census.messages, None);
        assert!(census.incomplete_reason.unwrap().contains("time limit"));
        assert!(sent.is_empty());
    }

    fn census_expired() -> (Result<crate::core::MessageCensus, String>, String) {
        census(Vec::new(), &["INBOX"], false, Instant::now())
    }

    #[test]
    fn too_many_folders_are_not_counted() {
        let folders = vec!["F"; super::MAX_CENSUS_FOLDERS + 1];
        let (result, sent) = census(
            Vec::new(),
            &folders,
            false,
            Instant::now() + Duration::from_secs(30),
        );
        assert!(
            result
                .unwrap()
                .incomplete_reason
                .unwrap()
                .contains("census limit")
        );
        assert!(sent.is_empty());
    }
}
