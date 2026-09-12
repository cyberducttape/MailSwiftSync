use crate::core;

fn number_after(line: &str, marker: &str) -> Option<u64> {
    line.split_once(marker)?
        .1
        .split_whitespace()
        .find_map(|token| {
            token
                .trim_matches(|character: char| !character.is_ascii_digit())
                .parse()
                .ok()
        })
}

/// Extract the stable summary fields emitted by imapsync.  The parser only
/// receives summary markers; the process runner keeps the complete journal
/// streaming and bounded separately.
pub(crate) fn parse_imapsync_evidence(lines: &[String]) -> Option<core::MailboxEvidence> {
    let last = |marker: &str| {
        lines
            .iter()
            .rev()
            .find_map(|line| number_after(line, marker))
    };
    let source_folders = last("Host1 Nb folders:")?;
    let destination_folders = last("Host2 Nb folders:")?;
    let source_messages = last("Host1 Nb messages:")?;
    let destination_messages = last("Host2 Nb messages:")?;
    let source_bytes = last("Host1 Total size:")?;
    let destination_bytes = last("Host2 Total size:")?;
    let failed_messages = last("Detected ").unwrap_or(0);
    let matched = lines
        .iter()
        .any(|line| line.contains("The sync looks good"));
    Some(core::MailboxEvidence {
        source_messages,
        destination_messages,
        source_bytes,
        destination_bytes,
        unmatched_messages: if matched { 0 } else { 1 },
        failed_messages,
        source_folders,
        destination_folders,
        authoritative: matched,
    })
}

fn parse_dovecot_status(lines: &[String]) -> Option<(u64, u64, u64)> {
    let mut folders = 0;
    let mut messages = 0;
    let mut bytes = 0;
    for line in lines {
        let message_count = line
            .split_whitespace()
            .find_map(|token| token.strip_prefix("messages=")?.parse::<u64>().ok());
        let virtual_size = line
            .split_whitespace()
            .find_map(|token| token.strip_prefix("vsize=")?.parse::<u64>().ok());
        if let (Some(message_count), Some(virtual_size)) = (message_count, virtual_size) {
            folders += 1;
            messages += message_count;
            bytes += virtual_size;
        }
    }
    (folders > 0).then_some((folders, messages, bytes))
}

pub(crate) fn parse_dovecot_evidence(
    source: &[String],
    destination: &[String],
) -> Option<core::MailboxEvidence> {
    let (source_folders, source_messages, source_bytes) = parse_dovecot_status(source)?;
    let (destination_folders, destination_messages, destination_bytes) =
        parse_dovecot_status(destination)?;
    Some(core::MailboxEvidence {
        source_messages,
        destination_messages,
        source_bytes,
        destination_bytes,
        unmatched_messages: 0,
        failed_messages: 0,
        source_folders,
        destination_folders,
        authoritative: false,
    })
}
