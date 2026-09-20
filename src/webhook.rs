//! Operator-configured outbound status webhook.
//!
//! MSPs and hosting admins typically track migration work in a PSA/ticketing
//! system (ConnectWise, Autotask, Halo, Syncro, or a generic automation
//! endpoint) rather than by polling MailSwiftSync directly. Rather than
//! building a vendor-specific integration for each of those, this sends the
//! same secret-free JSON `status --summary` already produces as an HTTPS
//! POST to one operator-configured URL; almost every PSA and automation
//! platform can ingest a generic webhook and route it from there.
//!
//! ## Authentication
//!
//! If the receiving endpoint requires authentication, pass credentials via:
//! - `MAILSWIFTSYNC_WEBHOOK_BEARER_TOKEN` environment variable (for Bearer tokens)
//! - `MAILSWIFTSYNC_WEBHOOK_BEARER_TOKEN_FILE` environment variable (path to file containing Bearer token)
//! - `MAILSWIFTSYNC_WEBHOOK_HEADER_NAME` and `MAILSWIFTSYNC_WEBHOOK_HEADER_VALUE` environment variables (for custom headers)
//! - `MAILSWIFTSYNC_WEBHOOK_HEADER_FILE` environment variable (path to file containing custom header as "Header-Name: value")
//!
//! Do NOT embed secrets in the webhook URL itself: secrets in command-line arguments
//! leak to process listings (ps aux), shell history, /proc/<pid>/cmdline, systemd units,
//! cron logs, audit logs, and monitoring telemetry.
//!
//! Only `https://` targets are accepted: an operator-supplied migration
//! status is not secret, but a plaintext endpoint would still let anyone on
//! the network path observe and tamper with it in flight.
use crate::credentials::{SecretString, read_secret_file};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::{Duration, Instant},
};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const WEBHOOK_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const WEBHOOK_READ_TIMEOUT: Duration = Duration::from_secs(20);
const WEBHOOK_TOTAL_BUDGET: Duration = Duration::from_secs(30);

/// POST `body` (already-serialized JSON) to `url` over a fresh
/// certificate-validated TLS connection. Authentication credentials are read
/// from environment variables, not from the URL itself. Returns the response
/// status code on any response MailSwiftSync could parse; the caller decides
/// which codes count as success. This does not retry — the CLI command this
/// backs is meant to be invoked by the operator's own automation (a scheduler,
/// a `supervise` wrapper script), which already owns its own retry policy.
pub(crate) fn post_json(url: &str, body: &str) -> Result<u16, String> {
    let bearer_token = load_webhook_bearer_token()?;
    let custom_header = load_webhook_custom_header()?;
    let (host, port, path) = parse_https_url(url)?;

    let address = format!("{host}:{port}");
    let sockets = address
        .to_socket_addrs()
        .map_err(|error| format!("{host}: {error}"))?
        .collect::<Vec<_>>();
    if sockets.is_empty() {
        return Err(format!("{host}: no address found for webhook URL"));
    }
    let mut last_error = None;
    let mut tcp = None;
    for socket in sockets {
        match TcpStream::connect_timeout(&socket, WEBHOOK_CONNECT_TIMEOUT) {
            Ok(stream) => {
                tcp = Some(stream);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let tcp = tcp.ok_or_else(|| {
        format!(
            "{host}: could not connect to the webhook URL: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown connection error".into())
        )
    })?;
    tcp.set_read_timeout(Some(WEBHOOK_READ_TIMEOUT))
        .map_err(|error| error.to_string())?;

    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from(host.clone())
        .map_err(|error| format!("{host}: invalid TLS server name: {error}"))?;
    let connection = ClientConnection::new(Arc::new(config), name)
        .map_err(|error| format!("{host}: TLS configuration failed: {error}"))?;
    let mut stream = StreamOwned::new(connection, tcp);
    stream
        .conn
        .complete_io(&mut stream.sock)
        .map_err(|error| format!("{host}: TLS handshake with the webhook URL failed: {error}"))?;

    let host_header = if port == 443 {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    let mut request_text = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host_header}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\
         User-Agent: mailswiftsync-notify-webhook\r\n",
        body.len()
    );

    if let Some(token) = bearer_token {
        request_text.push_str(&format!("Authorization: Bearer {}\r\n", token.as_str()));
    }
    if let Some((name, value)) = custom_header {
        request_text.push_str(&format!("{}: {}\r\n", name, value.as_str()));
    }
    request_text.push_str("\r\n");
    request_text.push_str(body);

    stream
        .write_all(request_text.as_bytes())
        .map_err(|error| format!("{host}: could not send the webhook request: {error}"))?;

    let raw = read_bounded_response(&mut stream, WEBHOOK_TOTAL_BUDGET)
        .map_err(|error| format!("{host}: {error}"))?;
    let status_line = raw
        .lines()
        .next()
        .ok_or_else(|| format!("{host}: webhook endpoint sent an empty response"))?;
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| {
            format!("{host}: could not parse webhook response status line: {status_line}")
        })
}

fn read_bounded_response<S: Read>(stream: &mut S, budget: Duration) -> Result<String, String> {
    let started = Instant::now();
    let mut raw = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if started.elapsed() > budget {
            return Err("webhook endpoint response exceeded the notification time budget".into());
        }
        let count = match stream.read(&mut buffer) {
            Ok(count) => count,
            // A peer that closes the raw connection right after its final
            // TLS record, without a closing `close_notify` alert, is common
            // for `Connection: close` responses and is not in itself
            // evidence of truncation: rustls already validates every
            // record's integrity before handing back plaintext, so bytes
            // read up to this point are exactly what the peer sent.
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.to_string()),
        };
        if count == 0 {
            break;
        }
        raw.extend_from_slice(&buffer[..count]);
        if raw.len() > MAX_RESPONSE_BYTES {
            return Err(format!(
                "webhook endpoint response exceeded {MAX_RESPONSE_BYTES} bytes"
            ));
        }
    }
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

/// A minimal `https://host[:port]/path` parser, matching the same
/// intentionally narrow approach as the OAuth token-refresh path: this is an
/// operator-configured URL, not a general web request, so pulling in a URL
/// crate would be a disproportionate dependency addition.
fn parse_https_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| "the webhook URL must use https://".to_owned())?;
    if rest.is_empty() {
        return Err("the webhook URL is missing a host".into());
    }
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.chars().any(char::is_control) || path.chars().any(char::is_control) {
        return Err("the webhook URL cannot contain control characters".into());
    }
    if authority.is_empty() {
        return Err("the webhook URL is missing a host".into());
    }
    let (host, port) = crate::endpoint::parts(authority, 443)
        .map_err(|error| format!("invalid webhook URL host: {error}"))?;
    Ok((host, port, path.to_owned()))
}

/// Load webhook Bearer token from environment variable or file.
/// Supports MAILSWIFTSYNC_WEBHOOK_BEARER_TOKEN (direct) or
/// MAILSWIFTSYNC_WEBHOOK_BEARER_TOKEN_FILE (path to file).
/// File contents are trimmed of trailing whitespace.
fn load_webhook_bearer_token() -> Result<Option<SecretString>, String> {
    if let Ok(token) = std::env::var("MAILSWIFTSYNC_WEBHOOK_BEARER_TOKEN")
        && !token.is_empty()
    {
        validate_header_value(&token)?;
        return Ok(Some(SecretString::from(token)));
    }
    if let Ok(path) = std::env::var("MAILSWIFTSYNC_WEBHOOK_BEARER_TOKEN_FILE") {
        let token = read_secret_file(std::path::Path::new(&path))
            .map_err(|error| format!("Failed to read webhook bearer token file: {error}"))?;
        if !token.is_empty() {
            validate_header_value(token.as_str())?;
            return Ok(Some(token));
        }
    }
    Ok(None)
}

/// Load custom header from environment variables.
/// Supports MAILSWIFTSYNC_WEBHOOK_HEADER_NAME + MAILSWIFTSYNC_WEBHOOK_HEADER_VALUE or
/// MAILSWIFTSYNC_WEBHOOK_HEADER_FILE (path to file containing "Header-Name: value").
fn load_webhook_custom_header() -> Result<Option<(String, SecretString)>, String> {
    if let (Ok(name), Ok(value)) = (
        std::env::var("MAILSWIFTSYNC_WEBHOOK_HEADER_NAME"),
        std::env::var("MAILSWIFTSYNC_WEBHOOK_HEADER_VALUE"),
    ) && !name.is_empty()
        && !value.is_empty()
    {
        validate_header_name(&name)?;
        validate_header_value(&value)?;
        return Ok(Some((name, SecretString::from(value))));
    }
    if let Ok(path) = std::env::var("MAILSWIFTSYNC_WEBHOOK_HEADER_FILE") {
        let content = read_secret_file(std::path::Path::new(&path))
            .map_err(|error| format!("Failed to read webhook header file: {error}"))?;
        if let Some((name, value)) = content.as_str().split_once(':') {
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if !name.is_empty() && !value.is_empty() {
                validate_header_name(&name)?;
                validate_header_value(&value)?;
                return Ok(Some((name, SecretString::from(value))));
            }
        }
    }
    Ok(None)
}

fn validate_header_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        ..=b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
                )
        })
    {
        return Err("webhook header name is not a valid HTTP token".into());
    }
    Ok(())
}

fn validate_header_value(value: &str) -> Result<(), String> {
    if value
        .chars()
        .any(|character| character == '\r' || character == '\n')
    {
        return Err("webhook header values cannot contain CR or LF".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_url_with_explicit_port_and_path() {
        let (host, port, path) = parse_https_url("https://hooks.example.com:8443/in/abc").unwrap();
        assert_eq!(host, "hooks.example.com");
        assert_eq!(port, 8443);
        assert_eq!(path, "/in/abc");
    }

    #[test]
    fn parses_https_url_defaulting_port_and_path() {
        let (host, port, path) = parse_https_url("https://hooks.example.com").unwrap();
        assert_eq!(host, "hooks.example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/");
    }

    #[test]
    fn rejects_non_https_webhook_url() {
        let error = parse_https_url("http://hooks.example.com/in").unwrap_err();
        assert!(error.contains("https://"));
    }

    #[test]
    fn rejects_webhook_url_missing_a_host() {
        assert!(parse_https_url("https://").is_err());
        assert!(parse_https_url("https:///path").is_err());
    }

    #[test]
    fn rejects_control_characters_in_webhook_url() {
        assert!(parse_https_url("https://hooks.example.com/path\r\nX-Injected: yes").is_err());
    }

    #[test]
    fn rejects_header_injection() {
        assert!(validate_header_name("X-Test\r\nInjected: yes").is_err());
        assert!(validate_header_value("safe\nInjected: yes").is_err());
        assert!(validate_header_name("X-Test").is_ok());
        assert!(validate_header_value("safe value").is_ok());
    }
}
