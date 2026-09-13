use crate::{credentials::SecretString, imap_protocol::is_tagged_response};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use std::io::{Read, Write};

/// Build the RFC 7628 XOAUTH2 client response. The bearer token is kept in a
/// zeroizing intermediate and the returned encoded value remains zeroizing
/// until it is written to the socket.
pub(crate) fn xoauth2_payload(user: &str, access_token: &str) -> SecretString {
    let auth = SecretString::new(format!(
        "user={}\x01auth=Bearer {}\x01\x01",
        user, access_token
    ));
    SecretString::new(BASE64_STANDARD.encode(auth.as_bytes()))
}

/// Wait for the server's SASL continuation response before sending the
/// bearer payload. A bounded response prevents a hostile endpoint from
/// consuming unbounded memory during readiness probing.
pub(crate) fn read_auth_continuation<S: Read + Write>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
) -> Result<(), String> {
    loop {
        let count = stream.read(buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("IMAP connection closed during OAuth authentication".into());
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..count]));
        if let Some(line) = response
            .lines()
            .find(|line| line.trim_start().starts_with('+'))
        {
            // XOAUTH2 normally begins with a blank continuation. Gmail and
            // other providers may instead send the RFC 7628 error JSON here
            // when the token is already known to be invalid or expired.
            // A non-empty continuation is cancelled with an empty response;
            // do not send the access token again. The tagged result is still
            // consumed below so the AUTH exchange is complete before the
            // caller closes the connection.
            if line
                .trim_start()
                .strip_prefix('+')
                .is_some_and(|value| !value.trim().is_empty())
            {
                stream.write_all(b"\r\n").map_err(|e| e.to_string())?;
                consume_auth_error_result(stream, tag, response, buffer)?;
                return Err(format!(
                    "IMAP OAuth authentication rejected before client response for {tag}"
                ));
            }
            return Ok(());
        }
        if response.len() > 65_536 {
            return Err("IMAP OAuth authentication challenge exceeded 64 KiB".into());
        }
    }
}

fn consume_auth_error_result<S: Read>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
) -> Result<(), String> {
    loop {
        if response.lines().any(|line| is_tagged_response(line, tag)) {
            return Ok(());
        }
        let count = stream.read(buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("IMAP connection closed during OAuth authentication".into());
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..count]));
        if response.len() > 65_536 {
            return Err("IMAP OAuth authentication response exceeded 64 KiB".into());
        }
    }
}

/// Finish an AUTHENTICATE exchange, including the RFC 7628 error path.
///
/// A server may return a base64-encoded JSON error as a SASL continuation
/// after the client response. IMAP requires the client to acknowledge that
/// continuation with an empty response before the server sends the tagged
/// `NO`. Treating the continuation as ordinary text leaves the exchange
/// incomplete and can block until the socket timeout.
pub(crate) fn read_auth_result<S: Read + Write>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
) -> Result<(), String> {
    let mut acknowledged_continuations = 0;
    loop {
        let count = stream.read(buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("IMAP connection closed during OAuth authentication".into());
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..count]));
        let continuation_count = response
            .lines()
            .filter(|line| line.trim_start().starts_with('+'))
            .count();
        while acknowledged_continuations < continuation_count {
            stream.write_all(b"\r\n").map_err(|e| e.to_string())?;
            acknowledged_continuations += 1;
        }
        if response.lines().any(|line| is_tagged_response(line, tag)) {
            return Ok(());
        }
        if response.len() > 65_536 {
            return Err("IMAP OAuth authentication response exceeded 64 KiB".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct ScriptedStream {
        reads: VecDeque<Vec<u8>>,
        writes: Vec<u8>,
    }

    impl Read for ScriptedStream {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let Some(chunk) = self.reads.pop_front() else {
                return Ok(0);
            };
            let count = chunk.len().min(buffer.len());
            buffer[..count].copy_from_slice(&chunk[..count]);
            if count < chunk.len() {
                self.reads.push_front(chunk[count..].to_vec());
            }
            Ok(count)
        }
    }

    impl Write for ScriptedStream {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.writes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn oauth_error_continuation_is_acknowledged_before_tagged_result() {
        let mut stream = ScriptedStream {
            reads: VecDeque::from([
                b"+ eyJzdGF0dXMiOiI0MDEifA==\r\na002 NO authentication failed\r\n".to_vec(),
            ]),
            writes: Vec::new(),
        };
        let mut response = String::new();
        let mut buffer = [0; 4096];
        read_auth_result(&mut stream, "a002", &mut response, &mut buffer).unwrap();
        assert_eq!(stream.writes, b"\r\n");
        assert!(response.contains("a002 NO"));
    }

    #[test]
    fn oauth_initial_error_continuation_is_cancelled_and_tagged_result_consumed() {
        let mut stream = ScriptedStream {
            reads: VecDeque::from([
                b"+ eyJzdGF0dXMiOiI0MDEifA==\r\n".to_vec(),
                b"a002 NO authentication failed\r\n".to_vec(),
            ]),
            writes: Vec::new(),
        };
        let mut response = String::new();
        let mut buffer = [0; 4096];

        let error = read_auth_continuation(&mut stream, "a002", &mut response, &mut buffer)
            .expect_err("an initial OAuth error must fail authentication");

        assert!(error.contains("rejected before client response"));
        assert_eq!(stream.writes, b"\r\n");
        assert!(response.contains("a002 NO"));
    }
}
