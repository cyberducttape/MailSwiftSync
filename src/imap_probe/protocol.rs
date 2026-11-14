//! Deadline-bound IMAP command I/O and tagged response framing.

use super::{
    ImapError, ImapErrorKind, MAX_IMAP_COMMAND_DURATION, MessageFetchBudget, read_with_deadline,
};
use crate::imap_probe::literal_framing;
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

pub(super) use literal_framing::TaggedResponseScanner;

pub(super) fn read_imap_tagged<S: Read>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
) -> Result<(), ImapError> {
    read_imap_tagged_with_limit(stream, tag, response, buffer, 1_048_576)
}

pub(super) fn write_imap_command<S: Write>(
    stream: &mut S,
    bytes: &[u8],
    budget: Option<&MessageFetchBudget<'_>>,
    operation: &str,
) -> Result<(), ImapError> {
    let Some(budget) = budget else {
        return stream
            .write_all(bytes)
            .map_err(|error| ImapError::io(format!("{operation}: {error}"), &error));
    };
    let mut offset = 0;
    while offset < bytes.len() {
        budget.check()?;
        match stream.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(ImapError::new(
                    ImapErrorKind::Connection,
                    format!("{operation}: IMAP connection closed while writing"),
                ));
            }
            Ok(written) => offset = offset.saturating_add(written),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                super::wait_for_io_retry(budget.deadline, Some(budget.cancel));
            }
            Err(error) => return Err(ImapError::io(format!("{operation}: {error}"), &error)),
        }
    }
    Ok(())
}

fn read_imap_tagged_with_limit<S: Read>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
    max_bytes: usize,
) -> Result<(), ImapError> {
    let mut raw_response = Vec::new();
    let mut scanner = TaggedResponseScanner::new(tag);
    let deadline = Instant::now() + MAX_IMAP_COMMAND_DURATION;
    loop {
        let count = read_with_deadline(stream, buffer, deadline, None)?;
        if count == 0 {
            return Err(closed_before(tag));
        }
        raw_response.extend_from_slice(&buffer[..count]);
        if raw_response.len() > max_bytes {
            return Err(ImapError::new(
                ImapErrorKind::Protocol,
                format!("IMAP response for {tag} exceeded the {max_bytes}-byte safety limit"),
            ));
        }
        if scanner.scan(&raw_response) {
            response.clear();
            response.push_str(&String::from_utf8_lossy(&raw_response));
            return Ok(());
        }
    }
}

pub(super) fn read_imap_tagged_with_budget<S: Read>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
    max_bytes: usize,
    budget: &MessageFetchBudget<'_>,
) -> Result<(), ImapError> {
    let mut raw_response = Vec::new();
    let mut scanner = TaggedResponseScanner::new(tag);
    let mut no_progress_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        budget.check()?;
        if Instant::now() >= no_progress_deadline {
            return Err(ImapError::new(
                ImapErrorKind::Timeout,
                format!("IMAP response for {tag} stalled for 15 seconds"),
            ));
        }
        match stream.read(buffer) {
            Ok(0) => return Err(closed_before(tag)),
            Ok(count) => {
                no_progress_deadline = Instant::now() + Duration::from_secs(15);
                raw_response.extend_from_slice(&buffer[..count]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                super::wait_for_io_retry(
                    no_progress_deadline.min(budget.deadline),
                    Some(budget.cancel),
                );
            }
            Err(error) => return Err(ImapError::io(error.to_string(), &error)),
        }
        if raw_response.len() > max_bytes {
            return Err(budget.limit_error(
                crate::core::VerificationLimit::ResponseSize,
                format!("IMAP response for {tag} exceeded the {max_bytes}-byte safety limit"),
            ));
        }
        if scanner.scan(&raw_response) {
            response.clear();
            response.push_str(&String::from_utf8_lossy(&raw_response));
            return Ok(());
        }
    }
}

pub(super) fn read_imap_tagged_with_optional_budget<S: Read>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
    budget: Option<&MessageFetchBudget<'_>>,
) -> Result<(), ImapError> {
    match budget {
        Some(budget) => {
            read_imap_tagged_with_budget(stream, tag, response, buffer, 1_048_576, budget)
        }
        None => read_imap_tagged(stream, tag, response, buffer),
    }
}

pub(super) fn read_imap_tagged_bytes_with_budget<S: Read>(
    stream: &mut S,
    tag: &str,
    buffer: &mut [u8; 4096],
    max_bytes: usize,
    budget: &MessageFetchBudget<'_>,
) -> Result<Vec<u8>, ImapError> {
    let mut response = Vec::new();
    let mut scanner = TaggedResponseScanner::new(tag);
    let mut no_progress_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        budget.check()?;
        if Instant::now() >= no_progress_deadline {
            return Err(ImapError::new(
                ImapErrorKind::Timeout,
                format!("IMAP response for {tag} stalled for 15 seconds"),
            ));
        }
        match stream.read(buffer) {
            Ok(0) => return Err(closed_before(tag)),
            Ok(count) => {
                no_progress_deadline = Instant::now() + Duration::from_secs(15);
                response.extend_from_slice(&buffer[..count]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                super::wait_for_io_retry(
                    no_progress_deadline.min(budget.deadline),
                    Some(budget.cancel),
                );
            }
            Err(error) => return Err(ImapError::io(error.to_string(), &error)),
        }
        if response.len() > max_bytes {
            return Err(budget.limit_error(
                crate::core::VerificationLimit::ResponseSize,
                format!("IMAP response for {tag} exceeded the {max_bytes}-byte safety limit"),
            ));
        }
        if scanner.scan(&response) {
            return Ok(response);
        }
    }
}

fn closed_before(tag: &str) -> ImapError {
    ImapError::new(
        ImapErrorKind::Connection,
        format!("IMAP connection closed before {tag} completed"),
    )
}

#[cfg(test)]
pub(super) fn tagged_response_outside_literals(response: &[u8], tag: &str) -> bool {
    TaggedResponseScanner::new(tag).scan(response)
}
