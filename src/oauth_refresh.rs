//! Automatic OAuth 2.0 access-token refresh for the imapsync XOAUTH2 path.
//!
//! The operator registers their own OAuth application (client ID, and a
//! client secret for providers that require one). The initial refresh token
//! comes from `mailswiftsync oauth-authorize` (see `oauth_authorize`) or from
//! the provider's own tooling. What this module adds is the unattended part: given a token endpoint, client credentials, and a
//! long-lived refresh token, it exchanges them for a fresh short-lived access
//! token before each live launch so a multi-hour batch queue does not stall
//! on an expired token that only the operator could previously replace by
//! hand. Refresh-token rotation (a provider issuing a new refresh token with
//! every exchange) is honored and persisted by the caller.
use crate::credentials::SecretString;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::Read,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use zeroize::Zeroizing;

/// Bound on the token endpoint response so a hostile or misbehaving endpoint
/// cannot exhaust memory during an unattended refresh.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_REFRESH_CONFIG_BYTES: usize = 64 * 1024;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const REFRESH_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REFRESH_TOTAL_BUDGET: Duration = Duration::from_secs(30);

static OAUTH_HTTP_CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();

pub(crate) struct RefreshRequest<'a> {
    pub(crate) token_endpoint: &'a str,
    pub(crate) client_id: &'a str,
    pub(crate) client_secret: Option<&'a str>,
    pub(crate) refresh_token: &'a str,
}

/// The operator-supplied, provider-registered application credentials plus
/// refresh token that make unattended refresh possible. Stored as one JSON
/// blob in the OS keyring, next to (but under a distinct service name from)
/// the plain password/access-token entries, so an operator who never
/// configures automatic refresh sees no change to the existing credential
/// storage.
#[derive(Debug, Clone)]
pub(crate) struct OAuthRefreshConfig {
    pub(crate) token_endpoint: String,
    pub(crate) client_id: String,
    pub(crate) client_secret: SecretString,
    pub(crate) refresh_token: SecretString,
}

/// A rotated refresh token whose provider-side exchange succeeded but whose
/// local keyring write did not. This stays process-local and zeroizing until
/// the operator or a later worker can complete the keyring write; it is never
/// serialized into the ledger or emitted in diagnostics.
#[derive(Clone, Debug)]
pub(crate) struct OAuthRefreshRecovery {
    pub(crate) config: OAuthRefreshConfig,
    pub(crate) access_token: SecretString,
    pub(crate) expires_in: Option<u64>,
}

static PENDING_REFRESH_RECOVERIES: OnceLock<Mutex<HashMap<String, OAuthRefreshRecovery>>> =
    OnceLock::new();

fn pending_refresh_recoveries() -> &'static Mutex<HashMap<String, OAuthRefreshRecovery>> {
    PENDING_REFRESH_RECOVERIES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn take_refresh_recovery(key: &str) -> Result<Option<OAuthRefreshRecovery>, String> {
    pending_refresh_recoveries()
        .lock()
        .map_err(|_| "OAuth refresh recovery registry was poisoned".to_owned())
        .map(|mut recoveries| recoveries.remove(key))
}

pub(crate) fn put_refresh_recovery(
    key: String,
    recovery: OAuthRefreshRecovery,
) -> Result<(), String> {
    pending_refresh_recoveries()
        .lock()
        .map_err(|_| "OAuth refresh recovery registry was poisoned".to_owned())
        .map(|mut recoveries| {
            recoveries.insert(key, recovery);
        })
}

pub(crate) fn clear_refresh_recovery(key: &str) -> Result<(), String> {
    pending_refresh_recoveries()
        .lock()
        .map_err(|_| "OAuth refresh recovery registry was poisoned".to_owned())
        .map(|mut recoveries| {
            recoveries.remove(key);
        })
}

impl OAuthRefreshConfig {
    pub(crate) fn as_request(&self) -> RefreshRequest<'_> {
        RefreshRequest {
            token_endpoint: self.token_endpoint.trim(),
            client_id: self.client_id.trim(),
            client_secret: Some(self.client_secret.as_str()).filter(|s| !s.is_empty()),
            refresh_token: self.refresh_token.as_str(),
        }
    }
}

/// Serialize a refresh config for keyring storage. Field values are plain
/// JSON strings, not `SecretString`, so plaintext serialization is explicit at
/// this keyring boundary; the encoded text exists only as long as it takes to
/// hand it to the keyring backend, mirroring how the existing password path
/// hands `entry.set_password` a borrowed `&str`.
pub(crate) fn encode_refresh_config(config: &OAuthRefreshConfig) -> Zeroizing<String> {
    #[derive(Serialize)]
    struct StoredOAuthRefreshConfig<'a> {
        token_endpoint: &'a str,
        client_id: &'a str,
        client_secret: &'a str,
        refresh_token: &'a str,
    }

    Zeroizing::new(
        serde_json::to_string(&StoredOAuthRefreshConfig {
            token_endpoint: &config.token_endpoint,
            client_id: &config.client_id,
            client_secret: config.client_secret.as_str(),
            refresh_token: config.refresh_token.as_str(),
        })
        .expect("OAuth refresh configuration fields are serializable"),
    )
}

pub(crate) fn decode_refresh_config(json: &str) -> Result<OAuthRefreshConfig, String> {
    #[derive(Deserialize)]
    struct StoredOAuthRefreshConfig {
        token_endpoint: String,
        client_id: String,
        #[serde(default)]
        client_secret: SecretString,
        refresh_token: SecretString,
    }

    if json.len() > MAX_REFRESH_CONFIG_BYTES {
        return Err(format!(
            "stored OAuth refresh configuration exceeds the {MAX_REFRESH_CONFIG_BYTES}-byte limit"
        ));
    }
    let stored: StoredOAuthRefreshConfig = serde_json::from_str(json)
        .map_err(|error| format!("stored OAuth refresh configuration is corrupt: {error}"))?;
    if stored.token_endpoint.trim().is_empty() {
        return Err("stored OAuth refresh configuration has an empty token endpoint".into());
    }
    if stored.refresh_token.is_empty() || stored.refresh_token.as_str().trim().is_empty() {
        return Err("stored OAuth refresh configuration has an empty refresh token".into());
    }
    Ok(OAuthRefreshConfig {
        token_endpoint: stored.token_endpoint,
        client_id: stored.client_id,
        client_secret: stored.client_secret,
        refresh_token: stored.refresh_token,
    })
}

#[derive(Debug)]
pub(crate) struct RefreshedToken {
    pub(crate) access_token: SecretString,
    /// Present only when the provider rotates refresh tokens on use. The
    /// caller must persist this back to the credential store or the next
    /// refresh will fail with an already-consumed token. The Form refresh
    /// path retries that write and retains a process-local recovery record if
    /// the keyring remains unavailable.
    pub(crate) refresh_token: Option<SecretString>,
    pub(crate) expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<SecretString>,
    #[serde(default)]
    refresh_token: Option<SecretString>,
    #[serde(default)]
    expires_in: Option<ExpiresIn>,
    error: Option<SecretString>,
    error_description: Option<SecretString>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ExpiresIn {
    Number(u64),
    Text(String),
}

impl ExpiresIn {
    fn into_u64(self) -> Option<u64> {
        match self {
            Self::Number(value) => Some(value),
            Self::Text(value) => value.parse().ok(),
        }
    }
}

/// Exchange a refresh token for a fresh access token over a bounded HTTPS
/// client. Redirects are disabled so a token cannot be forwarded to another
/// origin, and Rustls provides certificate and hostname validation.
pub(crate) fn refresh_access_token(request: &RefreshRequest<'_>) -> Result<RefreshedToken, String> {
    post_token_request(
        request.token_endpoint,
        &refresh_form(request),
        "mailswiftsync-oauth-refresh",
    )
}

/// POST one form-encoded grant to an HTTPS token endpoint and parse the
/// bounded JSON response. Shared by refresh and authorization-code exchange so
/// both use the same redirect-free Rustls client and size limits.
pub(crate) fn post_token_request(
    token_endpoint: &str,
    form: &[(&str, &str)],
    user_agent: &str,
) -> Result<RefreshedToken, String> {
    let endpoint = reqwest::Url::parse(token_endpoint)
        .map_err(|error| format!("invalid OAuth token endpoint: {error}"))?;
    if endpoint.scheme() != "https" {
        return Err("the OAuth token endpoint must use https://".to_owned());
    }
    if endpoint.username() != "" || endpoint.password().is_some() {
        return Err("the OAuth token endpoint must not contain user information".to_owned());
    }
    let host = endpoint
        .host_str()
        .ok_or_else(|| "the OAuth token endpoint is missing a host".to_owned())?
        .to_owned();
    let client = oauth_http_client().map_err(|error| format!("{host}: {error}"))?;
    let mut response = client
        .post(endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, user_agent)
        .form(form)
        .send()
        .map_err(|error| {
            format!(
                "{host}: token endpoint request failed: {}",
                error.without_url()
            )
        })?;
    let status = response.status().as_u16();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(format!(
            "{host}: token endpoint response exceeded {MAX_RESPONSE_BYTES} bytes"
        ));
    }
    let mut raw = Zeroizing::new(Vec::new());
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
                "token endpoint response exceeded {MAX_RESPONSE_BYTES} bytes"
            ));
        }
    }
    let response_body = Zeroizing::new(String::from_utf8_lossy(&raw).into_owned());
    parse_token_response(&format!("HTTP/1.1 {status}"), &response_body, form)
        .map_err(|error| redact_token_request_secrets(error, form))
}

fn redact_token_request_secrets(error: String, form: &[(&str, &str)]) -> String {
    redact_token_text(&error, form).to_string()
}

fn sanitize_token_error_description(description: &str, form: &[(&str, &str)]) -> String {
    // Redact before truncating: clipping an echoed long token first could
    // otherwise leave a sensitive prefix that no longer matches the secret.
    redact_token_text(description, form)
        .chars()
        .filter(|character| !character.is_control())
        .take(512)
        .collect()
}

fn redact_token_text(text: &str, form: &[(&str, &str)]) -> Zeroizing<String> {
    let mut redacted = Zeroizing::new(text.to_owned());
    let mut request_secrets = form
        .iter()
        .filter_map(|(name, value)| {
            matches!(
                *name,
                "client_secret"
                    | "refresh_token"
                    | "code"
                    | "code_verifier"
                    | "access_token"
                    | "assertion"
                    | "client_assertion"
            )
            .then_some(*value)
            .filter(|value| !value.is_empty())
        })
        .collect::<Vec<_>>();
    request_secrets.sort_unstable_by_key(|secret| std::cmp::Reverse(secret.len()));
    for secret in request_secrets {
        if redacted.contains(secret) {
            // Zeroize each secret-bearing intermediate before replacing it;
            // only the already-redacted text is later copied into an error.
            let previous = Zeroizing::new(std::mem::take(&mut *redacted));
            *redacted = previous.replace(secret, "[REDACTED]");
        }
    }
    redacted
}

fn oauth_http_client() -> Result<&'static reqwest::blocking::Client, String> {
    match OAUTH_HTTP_CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .use_rustls_tls()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(REFRESH_CONNECT_TIMEOUT)
            .timeout(REFRESH_TOTAL_BUDGET)
            .build()
            .map_err(|error| format!("could not build HTTPS client: {error}"))
    }) {
        Ok(client) => Ok(client),
        Err(error) => Err(error.clone()),
    }
}

fn parse_token_response(
    status_line: &str,
    body: &str,
    request_form: &[(&str, &str)],
) -> Result<RefreshedToken, String> {
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("could not parse token endpoint status line: {status_line}"))?;
    if status_code != "200" {
        // Outages often return an HTML or empty error page; report the HTTP
        // status rather than a JSON parse error that hides it.
        let Ok(response) = serde_json::from_str::<TokenResponse>(body) else {
            return Err(format!(
                "token endpoint rejected the token request (HTTP {status_code})"
            ));
        };
        let error_code = response
            .error
            .as_ref()
            .map(SecretString::as_str)
            .unwrap_or("")
            .chars()
            .filter(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
            })
            .take(80)
            .collect::<String>();
        let description = sanitize_token_error_description(
            response
                .error_description
                .as_ref()
                .map(SecretString::as_str)
                .unwrap_or(""),
            request_form,
        );
        let diagnostic = match (error_code.is_empty(), description.is_empty()) {
            (false, false) => format!(": {error_code}: {description}"),
            (false, true) => format!(": {error_code}"),
            (true, false) => format!(": {description}"),
            (true, true) => String::new(),
        };
        return Err(format!(
            "token endpoint rejected the token request (HTTP {status_code}{diagnostic})"
        ));
    }
    let response: TokenResponse = serde_json::from_str(body)
        .map_err(|error| format!("token endpoint response was not valid JSON: {error}"))?;
    let access_token = response
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "token endpoint response did not include an access_token".to_owned())?;
    if access_token.as_bytes().len() > MAX_TOKEN_BYTES {
        return Err(format!(
            "token endpoint access_token exceeds the {MAX_TOKEN_BYTES}-byte limit"
        ));
    }
    let refresh_token = response.refresh_token.filter(|token| !token.is_empty());
    if refresh_token
        .as_ref()
        .is_some_and(|token| token.as_bytes().len() > MAX_TOKEN_BYTES)
    {
        return Err(format!(
            "token endpoint refresh_token exceeds the {MAX_TOKEN_BYTES}-byte limit"
        ));
    }
    let expires_in = response.expires_in.and_then(ExpiresIn::into_u64);
    Ok(RefreshedToken {
        access_token,
        refresh_token,
        expires_in,
    })
}

fn refresh_form<'a>(request: &'a RefreshRequest<'a>) -> Vec<(&'a str, &'a str)> {
    let mut pairs = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", request.refresh_token),
        ("client_id", request.client_id),
    ];
    if let Some(secret) = request.client_secret.filter(|s| !s.is_empty()) {
        pairs.push(("client_secret", secret));
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_form_includes_all_credentials_without_manual_encoding() {
        let request = RefreshRequest {
            token_endpoint: "https://oauth2.googleapis.com/token",
            client_id: "client id/with space",
            client_secret: Some("s&cret+val=ue"),
            refresh_token: "refresh/token+value",
        };
        assert_eq!(
            refresh_form(&request),
            vec![
                ("grant_type", "refresh_token"),
                ("refresh_token", "refresh/token+value"),
                ("client_id", "client id/with space"),
                ("client_secret", "s&cret+val=ue"),
            ]
        );
    }

    #[test]
    fn refresh_body_omits_empty_client_secret() {
        let request = RefreshRequest {
            token_endpoint: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            client_id: "public-client",
            client_secret: Some(""),
            refresh_token: "token",
        };
        assert!(
            !refresh_form(&request)
                .iter()
                .any(|(name, _)| *name == "client_secret")
        );
    }

    #[test]
    fn parses_successful_token_response_with_rotation() {
        let status = "HTTP/1.1 200 OK";
        let body =
            r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3599}"#;
        let refreshed = parse_token_response(status, body, &[]).unwrap();
        assert_eq!(refreshed.access_token.as_str(), "new-access");
        assert_eq!(
            refreshed.refresh_token.as_ref().map(SecretString::as_str),
            Some("new-refresh")
        );
        assert_eq!(refreshed.expires_in, Some(3599));
    }

    #[test]
    fn parses_successful_token_response_without_rotation() {
        let status = "HTTP/1.1 200 OK";
        let body = r#"{"access_token":"new-access","expires_in":"3599"}"#;
        let refreshed = parse_token_response(status, body, &[]).unwrap();
        assert_eq!(refreshed.access_token.as_str(), "new-access");
        assert!(refreshed.refresh_token.is_none());
        assert_eq!(refreshed.expires_in, Some(3599));
    }

    #[test]
    fn rejects_error_response_with_provider_description() {
        let status = "HTTP/1.1 400 Bad Request";
        let body =
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#;
        let error = parse_token_response(status, body, &[]).unwrap_err();
        assert!(error.contains("Token has been expired or revoked."));
    }

    #[test]
    fn oauth_error_description_is_bounded_cleaned_and_redacts_request_secrets() {
        let body = format!(
            r#"{{"error":"invalid_grant","error_description":"Rejected refresh-secret-value; client client-secret-value.\n{}"}}"#,
            "x".repeat(600)
        );
        let request = [
            ("refresh_token", "refresh-secret-value"),
            ("client_secret", "client-secret-value"),
        ];
        let safe_error =
            parse_token_response("HTTP/1.1 400 Bad Request", &body, &request).unwrap_err();

        assert!(!safe_error.contains("refresh-secret-value"), "{safe_error}");
        assert!(!safe_error.contains("client-secret-value"), "{safe_error}");
        assert!(!safe_error.contains('\n'), "{safe_error}");
        assert!(safe_error.len() < 700, "{} bytes", safe_error.len());
    }

    #[test]
    fn oauth_error_truncation_cannot_leak_a_prefix_of_a_long_echoed_token() {
        let long_refresh_token = "sensitive-token-prefix-".to_owned() + &"x".repeat(600);
        let description = format!("{}{}", "a".repeat(500), long_refresh_token);
        let body = serde_json::json!({
            "error": "invalid_grant",
            "error_description": description,
        })
        .to_string();
        let request = [("refresh_token", long_refresh_token.as_str())];
        let error = parse_token_response("HTTP/1.1 400 Bad Request", &body, &request).unwrap_err();

        assert!(!error.contains("sensitive-token-prefix"), "{error}");
        assert!(error.contains("[REDACTED]"), "{error}");
        assert!(error.len() < 600, "{} bytes", error.len());
    }

    #[test]
    fn non_json_error_page_reports_the_http_status() {
        let error =
            parse_token_response("HTTP/1.1 503 Service Unavailable", "<html>down</html>", &[])
                .unwrap_err();
        assert!(error.contains("HTTP 503"), "{error}");
        assert!(!error.contains("JSON"), "{error}");
    }

    #[test]
    fn rejects_response_missing_access_token() {
        let status = "HTTP/1.1 200 OK";
        let body = r#"{"expires_in":3599}"#;
        let error = parse_token_response(status, body, &[]).unwrap_err();
        assert!(error.contains("access_token"));
    }

    #[test]
    fn rejects_oversized_access_tokens() {
        let body = format!(
            r#"{{"access_token":"{}"}}"#,
            "x".repeat(MAX_TOKEN_BYTES + 1)
        );
        let error = parse_token_response("HTTP/1.1 200 OK", &body, &[]).unwrap_err();
        assert!(error.contains("access_token") && error.contains("exceeds"));
    }

    #[test]
    fn refresh_config_round_trips_through_json_encoding() {
        let config = OAuthRefreshConfig {
            token_endpoint: "https://oauth2.googleapis.com/token".into(),
            client_id: "client-123".into(),
            client_secret: SecretString::from("s3cr3t"),
            refresh_token: SecretString::from("refresh-abc"),
        };
        let encoded = encode_refresh_config(&config);
        let decoded = decode_refresh_config(&encoded).unwrap();
        assert_eq!(decoded.token_endpoint, config.token_endpoint);
        assert_eq!(decoded.client_id, config.client_id);
        assert_eq!(decoded.client_secret.as_str(), "s3cr3t");
        assert_eq!(decoded.refresh_token.as_str(), "refresh-abc");
    }

    #[test]
    fn refresh_config_decoding_rejects_missing_refresh_token() {
        let error =
            decode_refresh_config(r#"{"token_endpoint":"https://x/token","client_id":"c"}"#)
                .unwrap_err();
        assert!(error.contains("refresh_token"));
    }

    #[test]
    fn refresh_config_decoding_rejects_oversized_keyring_values() {
        let oversized = "x".repeat(MAX_REFRESH_CONFIG_BYTES + 1);
        let error = decode_refresh_config(&oversized).unwrap_err();
        assert!(error.contains("exceeds"));
    }

    #[test]
    fn refresh_config_request_omits_blank_client_secret() {
        let config = OAuthRefreshConfig {
            token_endpoint: "https://x/token".into(),
            client_id: "public-client".into(),
            client_secret: SecretString::default(),
            refresh_token: SecretString::from("r"),
        };
        assert!(config.as_request().client_secret.is_none());
    }

    #[test]
    fn refresh_recovery_round_trips_without_exposing_secret_material() {
        let key = format!("test-recovery-{}", uuid::Uuid::new_v4());
        let recovery = OAuthRefreshRecovery {
            config: OAuthRefreshConfig {
                token_endpoint: "https://oauth.example.test/token".into(),
                client_id: "client".into(),
                client_secret: SecretString::from("client-secret"),
                refresh_token: SecretString::from("rotated-refresh"),
            },
            access_token: SecretString::from("access-token"),
            expires_in: Some(3600),
        };
        put_refresh_recovery(key.clone(), recovery).unwrap();
        let recovered = take_refresh_recovery(&key).unwrap().unwrap();
        assert_eq!(recovered.config.refresh_token.as_str(), "rotated-refresh");
        assert_eq!(recovered.access_token.as_str(), "access-token");
        assert_eq!(recovered.expires_in, Some(3600));
        assert!(!format!("{recovered:?}").contains("access-token"));
    }
}
