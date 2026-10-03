//! Operator-configured outbound status webhook.
//!
//! MSPs and hosting admins typically track migration work in a PSA/ticketing
//! system (ConnectWise, Autotask, Halo, Syncro, or a generic automation
//! endpoint) rather than by polling MailSwiftSync directly. Rather than
//! building a vendor-specific integration for each of those, this sends the
//! a credential-free operational status projection as an HTTPS POST to one
//! operator-configured URL; almost every PSA and automation
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
//! - `MAILSWIFTSYNC_WEBHOOK_SIGNING_SECRET` or
//!   `MAILSWIFTSYNC_WEBHOOK_SIGNING_SECRET_FILE` (optional HMAC-SHA256 envelope signing)
//!
//! If no authentication variable is configured, the webhook is anonymous. If
//! any authentication variable is configured, the complete selected
//! authentication configuration must be valid; invalid or empty credentials
//! refuse delivery rather than silently falling back to anonymous access.
//!
//! Do NOT embed secrets in the webhook URL itself: secrets in command-line arguments
//! leak to process listings (ps aux), shell history, /proc/<pid>/cmdline, systemd units,
//! cron logs, audit logs, and monitoring telemetry.
//!
//! Only `https://` targets are accepted. Operational status can contain
//! sensitive customer metadata even though it contains no credentials.
use crate::credentials::{SecretString, read_secret_file};
use reqwest::blocking::Response;
use reqwest::header::{HeaderName, HeaderValue};
use ring::hmac;
use std::{io::Read, time::Duration};
use zeroize::Zeroizing;

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const WEBHOOK_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const WEBHOOK_TOTAL_BUDGET: Duration = Duration::from_secs(30);

/// Retain only identifiers, phases, and aggregate counts for the default
/// webhook representation. Names, endpoints, and process metadata require an
/// explicit operator opt-in at the CLI.
pub(crate) fn minimal_status_payload(status: serde_json::Value) -> serde_json::Value {
    let project_summaries = status
        .get("projects")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|project| {
            serde_json::json!({
                "id": project.get("id"),
                "phase": project.get("phase"),
                "mailbox_state_counts": project.get("mailbox_state_counts"),
                "attention_reason_counts": project.get("attention_reason_counts"),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "data_classification": "credential-free operational status",
        "schema_version": status.get("schema_version"),
        "returned_projects": status.get("returned_projects"),
        "total_projects": status.get("total_projects"),
        "projects_truncated": status.get("projects_truncated"),
        "aggregate_mailbox_state_counts": status.get("aggregate_mailbox_state_counts"),
        "projects": project_summaries,
    })
}

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
    let signing_secret = load_webhook_signing_secret()?;
    let parsed_url = parse_https_url(&url)?;
    let event_id = event_id(body);

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
        .header("X-MailSwiftSync-Event-Id", &event_id)
        .header("X-MailSwiftSync-Event-Type", "migration.status_snapshot")
        .header("Idempotency-Key", &event_id)
        .body(body.to_owned());
    if let Some(secret) = signing_secret {
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
        let signature = hmac::sign(&key, body.as_bytes());
        request = request.header(
            "X-MailSwiftSync-Signature",
            format!("sha256={}", hex_encode(signature.as_ref())),
        );
    }
    if let Some(token) = bearer_token {
        let authorization = Zeroizing::new(format!("Bearer {}", token.as_str()));
        let mut value = HeaderValue::from_str(&authorization)
            .map_err(|error| format!("invalid webhook bearer token: {error}"))?;
        value.set_sensitive(true);
        request = request.header(reqwest::header::AUTHORIZATION, value);
    }
    if let Some((name, value)) = custom_header {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("invalid webhook header name: {error}"))?;
        let mut value = HeaderValue::from_str(value.as_str())
            .map_err(|error| format!("invalid webhook header value: {error}"))?;
        value.set_sensitive(true);
        request = request.header(name, value);
    }

    // reqwest errors render the request URL, which may carry a secret path
    // supplied through MAILSWIFTSYNC_WEBHOOK_URL_FILE.
    let mut response = request
        .send()
        .map_err(|error| format!("webhook request failed: {}", error.without_url()))?;
    read_bounded_response(&mut response)?;
    Ok(response.status().as_u16())
}

pub(crate) fn endpoint_digest(url: &str) -> Result<String, String> {
    let resolved = load_webhook_url(url)?;
    Ok(hash_hex(resolved.as_bytes()))
}

pub(crate) fn event_id(body: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"mailswiftsync:webhook:migration.status_snapshot:v1\n");
    digest.update(body.as_bytes());
    hex_encode(&digest.finalize())
}

fn hash_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex_encode(&digest.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
    let direct = configured_env("MAILSWIFTSYNC_WEBHOOK_BEARER_TOKEN")?;
    let file_path = configured_env("MAILSWIFTSYNC_WEBHOOK_BEARER_TOKEN_FILE")?;
    let file_token = if let Some(path) = file_path {
        let token = read_secret_file(std::path::Path::new(&path))
            .map_err(|error| format!("Failed to read webhook bearer token file: {error}"))?;
        Some(token.as_str().to_owned())
    } else {
        None
    };
    resolve_bearer_token(direct, file_token)
}

/// Load custom header from environment variables.
/// Supports MAILSWIFTSYNC_WEBHOOK_HEADER_NAME + MAILSWIFTSYNC_WEBHOOK_HEADER_VALUE or
/// MAILSWIFTSYNC_WEBHOOK_HEADER_FILE (path to file containing "Header-Name: value").
fn load_webhook_custom_header() -> Result<Option<(String, SecretString)>, String> {
    let name = configured_env("MAILSWIFTSYNC_WEBHOOK_HEADER_NAME")?;
    let value = configured_env("MAILSWIFTSYNC_WEBHOOK_HEADER_VALUE")?;
    let file_path = configured_env("MAILSWIFTSYNC_WEBHOOK_HEADER_FILE")?;
    let file_header = if let Some(path) = file_path {
        let content = read_secret_file(std::path::Path::new(&path))
            .map_err(|error| format!("Failed to read webhook header file: {error}"))?;
        Some(content.as_str().to_owned())
    } else {
        None
    };
    resolve_custom_header(name, value, file_header)
}

/// Load the optional HMAC signing secret from an environment value or an
/// owner-only file. It is deliberately independent from endpoint auth so an
/// operator can use a bearer token and still give the receiver an integrity
/// signature with a different secret.
fn load_webhook_signing_secret() -> Result<Option<SecretString>, String> {
    let direct = configured_env("MAILSWIFTSYNC_WEBHOOK_SIGNING_SECRET")?;
    let file_path = configured_env("MAILSWIFTSYNC_WEBHOOK_SIGNING_SECRET_FILE")?;
    let file_secret = if let Some(path) = file_path {
        Some(
            read_secret_file(std::path::Path::new(&path))
                .map_err(|error| format!("Failed to read webhook signing secret file: {error}"))?
                .as_str()
                .to_owned(),
        )
    } else {
        None
    };
    match (direct, file_secret) {
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Err(
            "configure only one of MAILSWIFTSYNC_WEBHOOK_SIGNING_SECRET and MAILSWIFTSYNC_WEBHOOK_SIGNING_SECRET_FILE".into(),
        ),
        (Some(secret), None) | (None, Some(secret)) => {
            if secret.is_empty() {
                return Err("webhook signing secret is configured but empty".into());
            }
            Ok(Some(SecretString::from(secret)))
        }
    }
}

fn configured_env(name: &str) -> Result<Option<String>, String> {
    if std::env::var_os(name).is_none() {
        return Ok(None);
    }
    std::env::var(name)
        .map(Some)
        .map_err(|_| format!("webhook environment variable {name} is not valid UTF-8"))
}

fn resolve_bearer_token(
    direct: Option<String>,
    file_token: Option<String>,
) -> Result<Option<SecretString>, String> {
    match (direct, file_token) {
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Err(
            "configure only one of MAILSWIFTSYNC_WEBHOOK_BEARER_TOKEN and MAILSWIFTSYNC_WEBHOOK_BEARER_TOKEN_FILE".into(),
        ),
        (Some(token), None) => {
            if token.is_empty() {
                return Err("webhook bearer token is configured but empty".into());
            }
            validate_header_value(&token)?;
            Ok(Some(SecretString::from(token)))
        }
        (None, Some(token)) => {
            if token.is_empty() {
                return Err("webhook bearer token file is empty".into());
            }
            validate_header_value(&token)?;
            Ok(Some(SecretString::from(token)))
        }
    }
}

fn resolve_custom_header(
    name: Option<String>,
    value: Option<String>,
    file_header: Option<String>,
) -> Result<Option<(String, SecretString)>, String> {
    if file_header.is_some() && (name.is_some() || value.is_some()) {
        return Err(
            "configure either MAILSWIFTSYNC_WEBHOOK_HEADER_NAME/VALUE or MAILSWIFTSYNC_WEBHOOK_HEADER_FILE, not both".into(),
        );
    }
    if let Some(content) = file_header {
        let (name, value) = content
            .split_once(':')
            .ok_or_else(|| "webhook header file must contain Header-Name: value".to_string())?;
        let name = name.trim().to_owned();
        let value = value.trim().to_owned();
        if name.is_empty() || value.is_empty() {
            return Err("webhook header file must contain a non-empty name and value".into());
        }
        validate_header_name(&name)?;
        validate_header_value(&value)?;
        return Ok(Some((name, SecretString::from(value))));
    }
    match (name, value) {
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => {
            Err("webhook custom header requires both name and value".into())
        }
        (Some(name), Some(value)) => {
            if name.is_empty() || value.is_empty() {
                return Err("webhook custom header name and value must be non-empty".into());
            }
            validate_header_name(&name)?;
            validate_header_value(&value)?;
            Ok(Some((name, SecretString::from(value))))
        }
    }
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
    fn event_id_is_stable_for_idempotent_delivery_and_changes_with_body() {
        assert_eq!(event_id("{}"), event_id("{}"));
        assert_ne!(event_id("{}"), event_id("{\"phase\":\"complete\"}"));
        assert_eq!(event_id("{}").len(), 64);
    }

    #[test]
    fn default_status_projection_omits_customer_and_process_metadata() {
        let status = serde_json::json!({
            "schema_version": 12,
            "active_processes": [{"pid": 42}],
            "active_processes_truncated": false,
            "returned_projects": 1,
            "total_projects": 1,
            "projects_truncated": false,
            "aggregate_mailbox_state_counts": {"completed": 3},
            "projects": [{
                "id": "project-id",
                "name": "Customer name",
                "batch": true,
                "source_endpoint": "imap.customer.example",
                "destination_endpoint": "imap.destination.example",
                "phase": "completed",
                "mailbox_state_counts": {"completed": 3},
                "attention_reason_counts": {"verification_incomplete": 1}
            }]
        });
        let payload = minimal_status_payload(status);
        assert_eq!(
            payload["data_classification"],
            "credential-free operational status"
        );
        assert_eq!(payload["projects"][0]["id"], "project-id");
        assert_eq!(payload["projects"][0]["phase"], "completed");
        assert!(payload["projects"][0].get("mailbox_state_counts").is_some());
        assert_eq!(
            payload["projects"][0]["attention_reason_counts"]["verification_incomplete"],
            1
        );
        for field in ["name", "batch", "source_endpoint", "destination_endpoint"] {
            assert!(payload["projects"][0].get(field).is_none());
        }
        assert!(payload.get("active_processes").is_none());
    }

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

    #[test]
    fn anonymous_webhook_is_allowed_only_when_no_auth_is_configured() {
        assert!(resolve_bearer_token(None, None).unwrap().is_none());
        assert!(resolve_custom_header(None, None, None).unwrap().is_none());
    }

    #[test]
    fn empty_bearer_configuration_fails_closed() {
        let error = resolve_bearer_token(Some(String::new()), None).unwrap_err();
        assert!(error.contains("configured but empty"));
        let error = resolve_bearer_token(None, Some(String::new())).unwrap_err();
        assert!(error.contains("file is empty"));
    }

    #[test]
    fn partial_or_empty_custom_header_configuration_fails_closed() {
        assert!(resolve_custom_header(Some("X-Auth".into()), None, None).is_err());
        assert!(resolve_custom_header(None, Some("secret".into()), None).is_err());
        assert!(resolve_custom_header(Some(String::new()), Some("secret".into()), None).is_err());
        assert!(resolve_custom_header(None, None, Some("not-a-header".into())).is_err());
        assert!(resolve_custom_header(None, None, Some("X-Auth: ".into())).is_err());
    }

    #[test]
    fn conflicting_auth_sources_fail_closed() {
        assert!(resolve_bearer_token(Some("direct".into()), Some("file".into())).is_err());
        assert!(
            resolve_custom_header(
                Some("X-Auth".into()),
                Some("direct".into()),
                Some("X-Other: file".into())
            )
            .is_err()
        );
    }

    #[test]
    fn transport_failure_does_not_echo_secret_url_path() {
        // Port 1 on loopback refuses immediately; the error must not reveal
        // the secret-bearing path an operator kept out of process listings.
        let error = post_json("https://127.0.0.1:1/hooks/secret-token-path", "{}")
            .expect_err("closed loopback port must fail");
        assert!(error.starts_with("webhook request failed"), "{error}");
        assert!(!error.contains("secret-token-path"), "{error}");
    }
}
