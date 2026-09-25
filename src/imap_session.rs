//! Encapsulation of an IMAP connection that can be reused across multiple mailboxes.
//!
//! This avoids the expensive pattern of opening a fresh TLS connection and
//! authenticating for each folder. The session tracks the currently selected
//! mailbox to minimize redundant SELECT commands.

use std::io::{Read, Write};
use crate::imap_probe::MessageFetchBudget;

/// A reusable IMAP session with state tracking for connection reuse.
pub struct ImapSession<S: Read + Write> {
    stream: S,
    next_tag: usize,
    current_mailbox: Option<String>,
}

impl<S: Read + Write> ImapSession<S> {
    /// Create a new IMAP session from an authenticated stream.
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            next_tag: 1,
            current_mailbox: None,
        }
    }

    /// Generate the next command tag. Tags must be unique per command.
    pub fn next_tag(&mut self) -> String {
        let tag = format!("a{:03}", self.next_tag);
        self.next_tag += 1;
        tag
    }

    /// Get mutable access to the underlying stream (for direct protocol use).
    pub fn stream_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    /// Get immutable access to the underlying stream.
    pub fn stream(&self) -> &S {
        &self.stream
    }

    /// Record that a mailbox has been selected. Used to track state.
    pub fn set_selected_mailbox(&mut self, mailbox: String) {
        self.current_mailbox = Some(mailbox);
    }

    /// Get the currently selected mailbox, if any.
    pub fn current_mailbox(&self) -> Option<&str> {
        self.current_mailbox.as_deref()
    }

    /// Send a LOGOUT command and close the session.
    pub fn logout(&mut self) -> Result<(), String> {
        let tag = self.next_tag();
        self.stream
            .write_all(format!("{tag} LOGOUT\r\n").as_bytes())
            .map_err(|e| format!("could not send LOGOUT: {e}"))?;
        Ok(())
    }
}
