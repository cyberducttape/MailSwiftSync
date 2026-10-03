//! Bounded IMAP LIST inventory parsing and folder metadata.

use super::{MAX_IMAP_LIST_INVENTORY_BYTES, MessageFetchBudget, read_with_deadline};
use crate::imap_probe::{list_parser, literal_framing};
use crate::imap_protocol::{atom_eq, is_tagged_response, is_untagged_response};
use std::{
    io::Read,
    time::{Duration, Instant},
};

pub(super) const MAX_IMAP_LIST_LINE_BYTES: usize = 64 * 1024;
pub(super) const MAX_IMAP_LIST_LITERAL_BYTES: usize = 1024 * 1024;
pub(super) const MAX_IMAP_LIST_MAILBOXES: usize = 100_000;
pub(super) const MAX_IMAP_LIST_DURATION: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MailboxDescriptor {
    pub(crate) wire_name: String,
    pub(crate) delimiter: Option<String>,
    pub(crate) special_use: Vec<String>,
    pub(crate) selectable: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ListInventorySummary {
    pub(super) mailbox_count: usize,
    pub(super) special_use_mailboxes: usize,
    pub(super) selectable_mailbox_count: usize,
}

/// Consume an authenticated LIST response without retaining the complete
/// inventory. Each record, literal, response duration, and optional retained
/// inventory has its own explicit bound.
pub(super) fn read_imap_list_response<S: Read>(
    stream: &mut S,
    tag: &str,
    buffer: &mut [u8; 4096],
) -> Result<ListInventorySummary, String> {
    read_imap_list_response_inner(stream, tag, buffer, None, None)
}

#[cfg(test)]
pub(super) fn read_imap_list_response_with_mailboxes<S: Read>(
    stream: &mut S,
    tag: &str,
    buffer: &mut [u8; 4096],
    mailboxes: &mut Vec<String>,
) -> Result<ListInventorySummary, String> {
    let mut details = Vec::new();
    let summary = read_imap_list_response_inner(stream, tag, buffer, Some(&mut details), None)?;
    mailboxes.extend(
        details
            .into_iter()
            .filter(|mailbox| mailbox.selectable)
            .map(|mailbox| mailbox.wire_name),
    );
    Ok(summary)
}

pub(super) fn read_imap_list_response_with_details<S: Read>(
    stream: &mut S,
    tag: &str,
    buffer: &mut [u8; 4096],
    mailboxes: &mut Vec<MailboxDescriptor>,
    budget: &MessageFetchBudget<'_>,
) -> Result<ListInventorySummary, String> {
    read_imap_list_response_inner(stream, tag, buffer, Some(mailboxes), Some(budget))
}

fn read_imap_list_response_inner<S: Read>(
    stream: &mut S,
    tag: &str,
    buffer: &mut [u8; 4096],
    mut mailbox_details: Option<&mut Vec<MailboxDescriptor>>,
    budget: Option<&MessageFetchBudget<'_>>,
) -> Result<ListInventorySummary, String> {
    let mut line = Vec::new();
    let started = Instant::now();
    let mut last_read = Instant::now();
    let mut literal_remaining = 0_usize;
    let mut literal_separator_remaining = 0_u8;
    let mut literal_header: Option<String> = None;
    let mut literal_value = Vec::new();
    let mut summary = ListInventorySummary::default();
    let mut inventory_bytes = 0_usize;
    const MAX_INTER_READ_STALL: Duration = Duration::from_secs(15);
    let deadline = started + MAX_IMAP_LIST_DURATION;
    loop {
        if let Some(budget) = budget {
            budget.check()?;
        }
        if last_read.elapsed() > MAX_INTER_READ_STALL {
            return Err("IMAP LIST response stalled (no data received for 15 seconds)".into());
        }
        let budget_deadline = budget.map_or(deadline, |budget| budget.deadline);
        let read_deadline = (last_read + MAX_INTER_READ_STALL)
            .min(deadline)
            .min(budget_deadline);
        let count = read_with_deadline(
            stream,
            buffer,
            read_deadline,
            budget.map(|budget| budget.cancel),
        )?;
        last_read = Instant::now();
        if count == 0 {
            return Err(format!("IMAP connection closed before {tag} completed"));
        }
        if started.elapsed() > MAX_IMAP_LIST_DURATION {
            return Err("IMAP LIST response exceeded the 60-second processing limit".into());
        }
        let mut offset = 0;
        while offset < count {
            if literal_remaining > 0 {
                let consumed = literal_remaining.min(count - offset);
                literal_value.extend_from_slice(&buffer[offset..offset + consumed]);
                literal_remaining -= consumed;
                offset += consumed;
                if literal_remaining == 0 {
                    if let Some(header) = literal_header.take() {
                        record_list_entry(
                            &header,
                            Some(&literal_value),
                            &mut summary,
                            &mut mailbox_details,
                            &mut inventory_bytes,
                        )?;
                    }
                    literal_value.clear();
                    literal_separator_remaining = 2;
                }
                continue;
            }
            if literal_separator_remaining > 0 {
                let consumed = literal_separator_remaining.min((count - offset) as u8);
                literal_separator_remaining -= consumed;
                offset += consumed as usize;
                continue;
            }
            let byte = buffer[offset];
            offset += 1;
            line.push(byte);
            if line.len() > MAX_IMAP_LIST_LINE_BYTES {
                return Err("IMAP LIST response record exceeded 64 KiB".into());
            }
            if byte != b'\n' {
                continue;
            }
            let text = String::from_utf8_lossy(&line);
            let text = text.trim_end_matches(['\r', '\n']);
            let literal_size = list_literal_size(text);
            if is_untagged_response(text, "LIST") && literal_size.is_none() {
                record_list_entry(
                    text,
                    None,
                    &mut summary,
                    &mut mailbox_details,
                    &mut inventory_bytes,
                )?;
            }
            if is_tagged_response(text, tag) {
                let status = text.split_whitespace().nth(1);
                if !status.is_some_and(|status| atom_eq(status, "OK")) {
                    let detail = text
                        .chars()
                        .map(|character| {
                            if character.is_control() {
                                ' '
                            } else {
                                character
                            }
                        })
                        .take(512)
                        .collect::<String>();
                    return Err(format!("IMAP LIST command {tag} failed: {detail}"));
                }
                return Ok(summary);
            }
            if let Some(literal_size) = list_literal_size(text)
                && literal_size > 0
            {
                if literal_size > MAX_IMAP_LIST_LITERAL_BYTES {
                    return Err("IMAP LIST literal exceeded 1 MiB".into());
                }
                if is_untagged_response(text, "LIST") {
                    literal_header = Some(text.to_owned());
                    literal_value.clear();
                }
                literal_remaining = literal_size;
            }
            line.clear();
        }
    }
}

pub(super) fn record_list_entry(
    line: &str,
    literal_name: Option<&[u8]>,
    summary: &mut ListInventorySummary,
    mailbox_details: &mut Option<&mut Vec<MailboxDescriptor>>,
    inventory_bytes: &mut usize,
) -> Result<(), String> {
    if !is_untagged_response(line, "LIST") {
        return Ok(());
    }
    summary.mailbox_count = summary.mailbox_count.saturating_add(1);
    if summary.mailbox_count > MAX_IMAP_LIST_MAILBOXES {
        return Err(format!(
            "IMAP LIST response exceeded the {MAX_IMAP_LIST_MAILBOXES}-mailbox limit"
        ));
    }
    let special_use = list_special_use(line);
    if !special_use.is_empty() {
        summary.special_use_mailboxes = summary.special_use_mailboxes.saturating_add(1);
    }
    let selectable = !crate::imap_protocol::list_has_attribute(line, r"\NOSELECT");
    if selectable {
        summary.selectable_mailbox_count = summary.selectable_mailbox_count.saturating_add(1);
    }
    if let Some(details) = mailbox_details.as_deref_mut() {
        let wire_name = match literal_name {
            Some(bytes) => String::from_utf8(bytes.to_vec())
                .map_err(|_| "IMAP LIST mailbox literal was not valid UTF-8".to_owned())?,
            None => list_parser::mailbox_name(line)
                .ok_or_else(|| "IMAP LIST mailbox name was missing or malformed".to_owned())?,
        };
        let descriptor_bytes = wire_name
            .len()
            .saturating_add(list_parser::delimiter(line).as_ref().map_or(0, String::len))
            .saturating_add(special_use.iter().map(String::len).sum::<usize>())
            .saturating_add(256);
        *inventory_bytes = inventory_bytes.saturating_add(descriptor_bytes);
        if *inventory_bytes > MAX_IMAP_LIST_INVENTORY_BYTES {
            return Err(format!(
                "IMAP LIST inventory exceeded the {MAX_IMAP_LIST_INVENTORY_BYTES}-byte limit"
            ));
        }
        details.push(MailboxDescriptor {
            wire_name,
            delimiter: list_parser::delimiter(line),
            special_use,
            selectable,
        });
    }
    Ok(())
}

fn list_special_use(line: &str) -> Vec<String> {
    [
        (r"\ALL", "all"),
        (r"\ARCHIVE", "archive"),
        (r"\DRAFTS", "drafts"),
        (r"\JUNK", "junk"),
        (r"\SENT", "sent"),
        (r"\TRASH", "trash"),
    ]
    .into_iter()
    .filter(|(attribute, _)| crate::imap_protocol::list_has_attribute(line, attribute))
    .map(|(_, kind)| kind.to_owned())
    .collect()
}

fn list_literal_size(line: &str) -> Option<usize> {
    literal_framing::literal_length(line.as_bytes())
}

pub(super) fn authenticated_list_command(request_special_use: bool) -> &'static [u8] {
    if request_special_use {
        b"a005 LIST \"\" \"*\" RETURN (SPECIAL-USE)\r\n"
    } else {
        b"a005 LIST \"\" \"*\"\r\n"
    }
}
