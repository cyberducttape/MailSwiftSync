//! IMAP authentication and post-authentication readiness checks.

use super::{
    MessageFetchBudget, imap_command_failure, imap_command_succeeded, imap_quote,
    is_untagged_response, read_imap_tagged_with_optional_budget, write_imap_command,
};
use crate::credentials::SecretString;
use crate::oauth::{read_auth_continuation_with_deadline, read_auth_result_with_deadline};
use std::io::{Read, Write};

pub(super) fn authenticate_imap_stream<S: Read + Write>(
    mut stream: S,
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    greeting: String,
    budget: Option<&MessageFetchBudget<'_>>,
) -> Result<(S, String), String> {
    let mut response = String::new();
    let mut buffer = [0; 4096];
    write_imap_command(
        &mut stream,
        b"a001 CAPABILITY\r\n",
        budget,
        "could not write pre-auth CAPABILITY",
    )?;
    read_imap_tagged_with_optional_budget(&mut stream, "a001", &mut response, &mut buffer, budget)?;
    if !imap_command_succeeded(&response, "a001") {
        return Err(imap_command_failure(
            &response,
            "a001",
            "pre-auth CAPABILITY",
            host,
        ));
    }
    let preauth = greeting
        .lines()
        .any(|line| is_untagged_response(line, "PREAUTH"));
    if !preauth {
        if auth_method == "oauth2" {
            let encoded = crate::oauth::xoauth2_payload(user, credential);
            write_imap_command(
                &mut stream,
                b"a002 AUTHENTICATE XOAUTH2\r\n",
                budget,
                "could not write XOAUTH2 authentication command",
            )?;
            response.clear();
            let (deadline, cancel) = budget
                .map(|budget| (Some(budget.deadline), Some(budget.cancel)))
                .unwrap_or((None, None));
            read_auth_continuation_with_deadline(
                &mut stream,
                "a002",
                &mut response,
                &mut buffer,
                deadline,
                cancel,
            )?;
            write_imap_command(
                &mut stream,
                encoded.as_bytes(),
                budget,
                "could not write XOAUTH2 payload",
            )?;
            write_imap_command(
                &mut stream,
                b"\r\n",
                budget,
                "could not finish XOAUTH2 payload",
            )?;
            response.clear();
            read_auth_result_with_deadline(
                &mut stream,
                "a002",
                &mut response,
                &mut buffer,
                deadline,
                cancel,
            )?;
        } else {
            let quoted_password = SecretString::new(imap_quote(credential)?);
            let login = format!(
                "a002 LOGIN {} {}\r\n",
                imap_quote(user)?,
                quoted_password.as_str()
            );
            let login = SecretString::new(login);
            write_imap_command(
                &mut stream,
                login.as_bytes(),
                budget,
                "could not write IMAP authentication",
            )?;
            read_imap_tagged_with_optional_budget(
                &mut stream,
                "a002",
                &mut response,
                &mut buffer,
                budget,
            )?;
        }
        if !imap_command_succeeded(&response, "a002") {
            return Err(imap_command_failure(
                &response,
                "a002",
                "IMAP authentication",
                host,
            ));
        }
    }
    // RFC 9051 permits capabilities to change after authentication, so the
    // post-auth response is authoritative for readiness decisions.
    write_imap_command(
        &mut stream,
        b"a003 CAPABILITY\r\n",
        budget,
        "could not write post-auth CAPABILITY",
    )?;
    let mut post_auth_response = String::new();
    read_imap_tagged_with_optional_budget(
        &mut stream,
        "a003",
        &mut post_auth_response,
        &mut buffer,
        budget,
    )?;
    if !imap_command_succeeded(&post_auth_response, "a003") {
        return Err(imap_command_failure(
            &post_auth_response,
            "a003",
            "post-auth CAPABILITY",
            host,
        ));
    }
    Ok((stream, post_auth_response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Debug)]
    struct ScriptedStream {
        incoming: VecDeque<Vec<u8>>,
        outgoing: Vec<u8>,
    }

    impl ScriptedStream {
        fn new(responses: &[&str]) -> Self {
            Self {
                incoming: responses
                    .iter()
                    .map(|response| response.as_bytes().to_vec())
                    .collect(),
                outgoing: Vec::new(),
            }
        }

        fn sent(&self) -> &str {
            std::str::from_utf8(&self.outgoing).expect("IMAP commands are UTF-8/ASCII")
        }
    }

    impl Read for ScriptedStream {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let Some(response) = self.incoming.pop_front() else {
                return Ok(0);
            };
            if response.len() > buffer.len() {
                return Err(std::io::Error::other("test response exceeds read buffer"));
            }
            buffer[..response.len()].copy_from_slice(&response);
            Ok(response.len())
        }
    }

    impl Write for ScriptedStream {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.outgoing.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    const PRE_AUTH_CAPABILITIES: &str =
        "* CAPABILITY IMAP4rev1 AUTH=PLAIN\r\na001 OK capabilities\r\n";
    const POST_AUTH_CAPABILITIES: &str =
        "* CAPABILITY IMAP4rev1 AUTH=PLAIN\r\na003 OK capabilities\r\n";

    #[test]
    fn password_login_quotes_credentials_and_refreshes_capabilities() {
        let stream = ScriptedStream::new(&[
            PRE_AUTH_CAPABILITIES,
            "a002 OK authenticated\r\n",
            POST_AUTH_CAPABILITIES,
        ]);

        let (stream, capabilities) = authenticate_imap_stream(
            stream,
            "imap.example.test",
            "operator name",
            "p\"ass\\word",
            "password",
            "* OK ready\r\n".to_owned(),
            None,
        )
        .expect("valid LOGIN transcript should authenticate");

        assert!(
            stream
                .sent()
                .contains("a002 LOGIN \"operator name\" \"p\\\"ass\\\\word\"\r\n")
        );
        assert!(stream.sent().contains("a003 CAPABILITY\r\n"));
        assert!(capabilities.contains("a003 OK"));
    }

    #[test]
    fn oauth_login_sends_xoauth2_payload_and_waits_for_tagged_success() {
        let stream = ScriptedStream::new(&[
            PRE_AUTH_CAPABILITIES,
            "+\r\n",
            "a002 OK authenticated\r\n",
            POST_AUTH_CAPABILITIES,
        ]);

        let (stream, capabilities) = authenticate_imap_stream(
            stream,
            "imap.example.test",
            "operator@example.test",
            "access-token",
            "oauth2",
            "* OK ready\r\n".to_owned(),
            None,
        )
        .expect("valid XOAUTH2 transcript should authenticate");

        let payload = crate::oauth::xoauth2_payload("operator@example.test", "access-token");
        assert!(stream.sent().contains("a002 AUTHENTICATE XOAUTH2\r\n"));
        assert!(stream.sent().contains(&format!("{}\r\n", payload.as_str())));
        assert!(stream.sent().contains("a003 CAPABILITY\r\n"));
        assert!(capabilities.contains("a003 OK"));
    }

    #[test]
    fn preauthenticated_greeting_skips_credentials_but_refreshes_capabilities() {
        let stream = ScriptedStream::new(&[PRE_AUTH_CAPABILITIES, POST_AUTH_CAPABILITIES]);

        let (stream, _) = authenticate_imap_stream(
            stream,
            "imap.example.test",
            "operator@example.test",
            "must-not-be-sent",
            "password",
            "* PREAUTH ready\r\n".to_owned(),
            None,
        )
        .expect("PREAUTH should skip LOGIN");

        assert!(!stream.sent().contains("LOGIN"));
        assert!(!stream.sent().contains("must-not-be-sent"));
        assert!(stream.sent().contains("a003 CAPABILITY\r\n"));
    }

    #[test]
    fn authentication_rejection_fails_before_post_auth_capability_request() {
        let stream =
            ScriptedStream::new(&[PRE_AUTH_CAPABILITIES, "a002 NO invalid credentials\r\n"]);

        let error = authenticate_imap_stream(
            stream,
            "imap.example.test",
            "operator@example.test",
            "secret-value",
            "password",
            "* OK ready\r\n".to_owned(),
            None,
        )
        .expect_err("rejected LOGIN must fail closed");

        assert!(error.contains("invalid credentials"));
        assert!(!error.contains("secret-value"));
    }
}
