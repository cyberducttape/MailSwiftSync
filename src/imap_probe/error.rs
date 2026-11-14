//! Typed IMAP client failures.
//!
//! The IMAP client knows why it failed: its own deadline passed, the
//! connection dropped, a certificate was rejected, or the server answered NO.
//! Recording that at the point of failure means retry and attention policy
//! never have to recover it from the message text. Only `Server` and
//! `Unclassified` failures are classified from text, and for `Server` that
//! text is the server's own tagged reply, where provider vocabulary such as
//! RFC 5530 response codes legitimately lives.

use crate::core::VerificationLimit;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImapErrorKind {
    /// The operator cancelled the probe or verification.
    Cancelled,
    /// A client-side deadline passed, or the server stopped sending.
    Timeout,
    /// Name resolution, connect, or the established connection failed.
    Connection,
    /// TLS could not be negotiated for a reason other than the certificate.
    Tls,
    /// The server's certificate, its pin, or the configured CA bundle was
    /// rejected. Retrying cannot change the outcome.
    Certificate,
    /// The endpoint or request cannot work as configured.
    Configuration,
    /// The server sent something the client cannot use: a missing greeting
    /// or a response over its size bound.
    Protocol,
    /// A verification safety limit; carries which one.
    Limit(VerificationLimit),
    /// The destination reports its quota exhausted.
    QuotaExhausted,
    /// The server completed a command with NO or BAD. The detail carries
    /// its reply, which is classified with the provider's vocabulary.
    Server,
    /// Produced by code that does not type its failures yet; classified
    /// from text as before.
    Unclassified,
}

/// An IMAP client failure: what kind it is and the operator-facing detail.
/// The detail is exactly the text the client reported before errors were
/// typed, so durable run details and transcripts are unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImapError {
    kind: ImapErrorKind,
    detail: String,
}

impl ImapError {
    pub(crate) fn new(kind: ImapErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// A verification safety limit, tagged so durable details name it.
    pub(crate) fn limit(limit: VerificationLimit, detail: String) -> Self {
        Self::new(ImapErrorKind::Limit(limit), limit.tag(detail))
    }

    pub(crate) fn kind(&self) -> ImapErrorKind {
        self.kind
    }

    pub(crate) fn detail(&self) -> &str {
        &self.detail
    }

    /// Prefix the detail with an operation or host, keeping the kind.
    pub(crate) fn context(mut self, prefix: impl fmt::Display) -> Self {
        self.detail = format!("{prefix}: {}", self.detail);
        self
    }

    /// Type an I/O failure from the socket or the TLS layer beneath it.
    pub(crate) fn io(detail: impl Into<String>, error: &std::io::Error) -> Self {
        Self::new(io_kind(error), detail)
    }

    /// Whether the session's stream can no longer be used for another
    /// command: it may hold part of an unread response, or be closed.
    pub(crate) fn ends_session(&self) -> bool {
        matches!(
            self.kind,
            ImapErrorKind::Timeout
                | ImapErrorKind::Connection
                | ImapErrorKind::Tls
                | ImapErrorKind::Protocol
        )
    }
}

fn io_kind(error: &std::io::Error) -> ImapErrorKind {
    use std::io::ErrorKind;
    if let Some(tls) = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
    {
        return match tls {
            rustls::Error::InvalidCertificate(_) => ImapErrorKind::Certificate,
            _ => ImapErrorKind::Tls,
        };
    }
    match error.kind() {
        ErrorKind::TimedOut | ErrorKind::WouldBlock => ImapErrorKind::Timeout,
        ErrorKind::ConnectionRefused
        | ErrorKind::ConnectionReset
        | ErrorKind::ConnectionAborted
        | ErrorKind::NotConnected
        | ErrorKind::BrokenPipe
        | ErrorKind::UnexpectedEof
        | ErrorKind::AddrNotAvailable
        | ErrorKind::HostUnreachable
        | ErrorKind::NetworkUnreachable
        | ErrorKind::NetworkDown => ImapErrorKind::Connection,
        _ => ImapErrorKind::Unclassified,
    }
}

impl fmt::Display for ImapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl From<ImapError> for String {
    fn from(error: ImapError) -> Self {
        error.detail
    }
}

impl From<String> for ImapError {
    fn from(detail: String) -> Self {
        Self::new(ImapErrorKind::Unclassified, detail)
    }
}

impl From<&str> for ImapError {
    fn from(detail: &str) -> Self {
        Self::new(ImapErrorKind::Unclassified, detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_failures_are_typed_by_their_kind() {
        use std::io::{Error, ErrorKind};
        let kind = |error: Error| ImapError::io("x", &error).kind();
        assert_eq!(
            kind(Error::from(ErrorKind::TimedOut)),
            ImapErrorKind::Timeout
        );
        assert_eq!(
            kind(Error::from(ErrorKind::ConnectionReset)),
            ImapErrorKind::Connection
        );
        assert_eq!(
            kind(Error::new(
                ErrorKind::InvalidData,
                rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer),
            )),
            ImapErrorKind::Certificate
        );
        assert_eq!(
            kind(Error::new(
                ErrorKind::InvalidData,
                rustls::Error::AlertReceived(rustls::AlertDescription::HandshakeFailure),
            )),
            ImapErrorKind::Tls
        );
        assert_eq!(kind(Error::other("odd")), ImapErrorKind::Unclassified);
    }

    #[test]
    fn the_detail_is_the_text_reported_before_typing() {
        let error = ImapError::new(ImapErrorKind::Timeout, "IMAP read deadline exceeded")
            .context("mail.example.com");
        assert_eq!(
            String::from(error),
            "mail.example.com: IMAP read deadline exceeded"
        );
    }
}
