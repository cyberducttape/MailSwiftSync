use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use std::io::Read;
use zeroize::Zeroizing;

/// Build the RFC 7628 XOAUTH2 client response. The bearer token is kept in a
/// zeroizing intermediate and the returned encoded value is zeroized by the
/// caller before it is written to the socket.
pub(crate) fn xoauth2_payload(user: &str, access_token: &str) -> String {
    let auth = Zeroizing::new(format!(
        "user={}\x01auth=Bearer {}\x01\x01",
        user, access_token
    ));
    BASE64_STANDARD.encode(auth.as_bytes())
}

/// Wait for the server's SASL continuation response before sending the
/// bearer payload. A bounded response prevents a hostile endpoint from
/// consuming unbounded memory during readiness probing.
pub(crate) fn read_auth_continuation<S: Read>(
    stream: &mut S,
    response: &mut String,
    buffer: &mut [u8; 4096],
) -> Result<(), String> {
    loop {
        let count = stream.read(buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("IMAP connection closed during OAuth authentication".into());
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..count]));
        if response.lines().any(|line| line.starts_with('+')) {
            return Ok(());
        }
        if response.len() > 65_536 {
            return Err("IMAP OAuth authentication challenge exceeded 64 KiB".into());
        }
    }
}
