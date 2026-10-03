//! IMAP authentication and post-authentication readiness checks.

use super::*;

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
