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
//! - `MAILSWIFTSYNC_WEBHOOK_URL_FILE` environment variable (path to file containing the HTTPS URL)
//!
//! Do NOT embed secrets in the webhook URL itself: secrets in command-line arguments
//! leak to process listings (ps aux), shell history, /proc/<pid>/cmdline, systemd units,
//! cron logs, audit logs, and monitoring telemetry.
//!
//! Only `https://` targets are accepted: an operator-supplied migration
//! status is not secret, but a plaintext endpoint would still let anyone on
//! the network path observe and tamper with it in flight.
use crate::credentials::{SecretString, read_secret_file};
use reqwest::blocking::Response;
use reqwest::header::{HeaderName, HeaderValue};
use std::{io::Read, time::Duration};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const WEBHOOK_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const WEBHOOK_TOTAL_BUDGET: Duration = Duration::from_secs(30);

/// POST `body` (already-serialized JSON) to `url` using the shared reqwest
/// Rustls transport used by OAuth refresh. Authentication credentials are read
/// from environment variables, not from the URL itself. Returns the response
/// status code; the caller decides which codes count as success. This does not
/// retry — the CLI command this backs is meant to be invoked by the operator's
/// own automation, which already owns its retry policy.
pub(crate) fn post_json(url: &str, body: &str) -> Result<u16, String> {
    let url = load_webhook_url(url)?;
    let bearer_token = load_webhook_bearer_token()?;
    let custom_header = load_webhook_custom_header()?;
    let parsed_url = parse_https_url(&url)?;

    let client = reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(WEBHOOK_CONNECT_TIMEOUT)
        .timeout(WEBHOOK_TOTAL_BUDGET)
        .build()
        .map_err(|error| format!("could not build webhook client: {error}"))?;

    let mut request = client
        .post(parsed_url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "mailswiftsync-notify-webhook")
        .body(body.to_owned());
    if let Some(token) = bearer_token {
        let value = HeaderValue::from_str(&format!("Bearer {}", token.as_str()))
            .map_err(|error| format!("invalid webhook bearer token: {error}"))?;
        request = request.header(reqwest::header::AUTHORIZATION, value);
    }
    if let Some((name, value)) = custom_header {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("invalid webhook header name: {error}"))?;
        let value = HeaderValue::from_str(value.as_str())
            .map_err(|error| format!("invalid webhook header value: {error}"))?;
        request = request.header(name, value);
    }

    let mut response = request
        .send()
        .map_err(|error| format!("webhook request failed: {error}"))?;
    read_bounded_response(&mut response)?;
    Ok(response.status().as_u16())
}

fn read_bounded_response(response: &mut Response) -> Result<(), String> {
    let mut raw = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = match response.read(&mut buffer) {
            Ok(count) => count,
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
    Ok(())
}

fn parse_https_url(url: &str) -> Result<reqwest::Url, String> {
    if url.chars().any(char::is_control) {
        return Err("the webhook URL cannot contain control characters".into());
    }
    if let Some(rest) = url.strip_prefix("https://")
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return Err("the webhook URL is missing a host".into());
    }
    let parsed =
        reqwest::Url::parse(url).map_err(|error| format!("invalid webhook URL: {error}"))?;
    if parsed.scheme() != "https" {
        return Err("the webhook URL must use https://".into());
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err("the webhook URL is missing a host".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(
            "webhook URL credentials must be supplied through headers or environment files".into(),
        );
    }
    Ok(parsed)
}

/// Resolve the command-line URL, allowing operators to keep secret-bearing
/// webhook paths out of process listings, shell history, and audit logs.
/// The file form takes precedence when configured and is read with the same
/// owner-only/symlink-safe policy as other webhook credentials.
fn load_webhook_url(cli_url: &str) -> Result<String, String> {
    if let Ok(path) = std::env::var("MAILSWIFTSYNC_WEBHOOK_URL_FILE") {
        let url = read_secret_file(std::path::Path::new(&path))
            .map_err(|error| format!("Failed to read webhook URL file: {error}"))?;
        if url.is_empty() {
            return Err("webhook URL file is empty".into());
        }
        return Ok(url.as_str().to_owned());
    }
    Ok(cli_url.to_owned())
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
        let url = parse_https_url("https://hooks.example.com:8443/in/abc").unwrap();
        assert_eq!(url.host_str(), Some("hooks.example.com"));
        assert_eq!(url.port(), Some(8443));
        assert_eq!(url.path(), "/in/abc");
    }

    #[test]
    fn parses_https_url_defaulting_port_and_path() {
        let url = parse_https_url("https://hooks.example.com").unwrap();
        assert_eq!(url.host_str(), Some("hooks.example.com"));
        assert_eq!(url.port(), None);
        assert_eq!(url.path(), "/");
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
