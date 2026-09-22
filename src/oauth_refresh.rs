//! Automatic OAuth 2.0 access-token refresh for the imapsync XOAUTH2 path.
//!
//! MailSwiftSync does not perform provider consent; the operator must
//! register their own OAuth application (client ID, and a client secret for
//! providers that require one) and obtain an initial refresh token through
//! whatever flow that provider documents. What this module adds is the
//! unattended part: given a token endpoint, client credentials, and a
//! long-lived refresh token, it exchanges them for a fresh short-lived access
//! token before each live launch so a multi-hour batch queue does not stall
//! on an expired token that only the operator could previously replace by
//! hand. Refresh-token rotation (a provider issuing a new refresh token with
//! every exchange) is honored and persisted by the caller.
use crate::credentials::SecretString;
use serde::{Deserialize, Serialize};
use std::{io::Read, sync::OnceLock, time::Duration};
use zeroize::Zeroizing;

/// Bound on the token endpoint response so a hostile or misbehaving endpoint
/// cannot exhaust memory during an unattended refresh.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
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
#[derive(Debug)]
pub(crate) struct OAuthRefreshConfig {
    pub(crate) token_endpoint: String,
    pub(crate) client_id: String,
    pub(crate) client_secret: SecretString,
    pub(crate) refresh_token: SecretString,
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
/// JSON strings, not `SecretString`, so this never derives `Serialize` on the
/// secret type itself; the encoded text exists only as long as it takes to
/// hand it to the keyring backend, mirroring how the existing password path
/// hands `entry.set_password` a borrowed `&str`.
pub(crate) fn encode_refresh_config(config: &OAuthRefreshConfig) -> Zeroizing<String> {
    #[derive(Serialize)]
    struct StoredOAuthRefreshConfig<'a> {
        token_endpoint: &'a str,
        client_id: &'a str,
        client_secret: &'a SecretString,
        refresh_token: &'a SecretString,
    }

    Zeroizing::new(
        serde_json::to_string(&StoredOAuthRefreshConfig {
            token_endpoint: &config.token_endpoint,
            client_id: &config.client_id,
            client_secret: &config.client_secret,
            refresh_token: &config.refresh_token,
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
    /// refresh will fail with an already-consumed token.
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
    error: Option<String>,
    error_description: Option<String>,
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
    let endpoint = reqwest::Url::parse(request.token_endpoint)
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
        .header(reqwest::header::USER_AGENT, "mailswiftsync-oauth-refresh")
        .form(&refresh_form(request))
        .send()
        .map_err(|error| format!("{host}: token endpoint request failed: {error}"))?;
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
    parse_token_response(&format!("HTTP/1.1 {status}"), &response_body)
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

fn parse_token_response(status_line: &str, body: &str) -> Result<RefreshedToken, String> {
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("could not parse token endpoint status line: {status_line}"))?;
    let response: TokenResponse = serde_json::from_str(body)
        .map_err(|error| format!("token endpoint response was not valid JSON: {error}"))?;
    if status_code != "200" {
        let error_code = response.error.as_deref().unwrap_or("");
        let description = response.error_description.as_deref().unwrap_or("");
        return Err(format!(
            "token endpoint rejected the refresh request (HTTP {status_code}{}{})",
            if error_code.is_empty() { "" } else { ": " },
            if description.is_empty() {
                error_code
            } else {
                description
            }
        ));
    }
    let access_token = response
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "token endpoint response did not include an access_token".to_owned())?;
    let refresh_token = response.refresh_token.filter(|token| !token.is_empty());
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
        let refreshed = parse_token_response(status, body).unwrap();
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
        let refreshed = parse_token_response(status, body).unwrap();
        assert_eq!(refreshed.access_token.as_str(), "new-access");
        assert!(refreshed.refresh_token.is_none());
        assert_eq!(refreshed.expires_in, Some(3599));
    }

    #[test]
    fn rejects_error_response_with_provider_description() {
        let status = "HTTP/1.1 400 Bad Request";
        let body =
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#;
        let error = parse_token_response(status, body).unwrap_err();
        assert!(error.contains("Token has been expired or revoked."));
    }

    #[test]
    fn rejects_response_missing_access_token() {
        let status = "HTTP/1.1 200 OK";
        let body = r#"{"expires_in":3599}"#;
        let error = parse_token_response(status, body).unwrap_err();
        assert!(error.contains("access_token"));
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
    fn refresh_config_request_omits_blank_client_secret() {
        let config = OAuthRefreshConfig {
            token_endpoint: "https://x/token".into(),
            client_id: "public-client".into(),
            client_secret: SecretString::default(),
            refresh_token: SecretString::from("r"),
        };
        assert!(config.as_request().client_secret.is_none());
    }
}
