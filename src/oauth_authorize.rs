//! Interactive OAuth 2.0 authorization for the imapsync XOAUTH2 path.
//!
//! This is the native-application authorization-code flow (RFC 8252) with
//! PKCE (RFC 7636): MailSwiftSync prints the provider's consent URL, waits on
//! a one-shot loopback listener for the redirect, checks the `state` value,
//! exchanges the code over the same bounded HTTPS client used for refresh,
//! and stores the resulting refresh configuration in the OS keyring entry
//! that live launches already read. The operator still registers their own
//! OAuth application; MailSwiftSync ships no client ID of its own.
use crate::credentials::SecretString;
use crate::oauth_refresh::{OAuthRefreshConfig, RefreshedToken, post_token_request};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};
use std::{
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

/// How long the operator has to complete consent in the browser.
pub(crate) const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(300);
const REDIRECT_READ_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECT_REQUEST_BYTES: usize = 16 * 1024;
const MAX_AUTHORIZATION_CODE_BYTES: usize = 4096;
/// Browsers commonly request `/favicon.ico` next to the redirect. Bound how
/// many unrelated requests are tolerated before the flow gives up.
const MAX_UNRELATED_REQUESTS: usize = 8;

/// Endpoints and request shape for one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderProfile {
    pub(crate) authorize_endpoint: String,
    pub(crate) token_endpoint: String,
    pub(crate) scope: String,
    pub(crate) extra_parameters: Vec<(&'static str, &'static str)>,
    /// Loopback host written into `redirect_uri`. It must match the host the
    /// operator registered with the provider.
    pub(crate) redirect_host: RedirectHost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedirectHost {
    Ipv4Loopback,
    Localhost,
}

impl RedirectHost {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "127.0.0.1" => Ok(Self::Ipv4Loopback),
            "localhost" => Ok(Self::Localhost),
            _ => Err("--redirect-host must be 127.0.0.1 or localhost".into()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Ipv4Loopback => "127.0.0.1",
            Self::Localhost => "localhost",
        }
    }
}

/// Operator-supplied overrides. Provider presets fill everything except the
/// client ID; `custom` requires the endpoints and scope explicitly.
#[derive(Debug, Default)]
pub(crate) struct ProviderOverrides {
    pub(crate) tenant: Option<String>,
    pub(crate) authorize_endpoint: Option<String>,
    pub(crate) token_endpoint: Option<String>,
    pub(crate) scope: Option<String>,
    pub(crate) redirect_host: Option<RedirectHost>,
}

pub(crate) fn provider_profile(
    provider: &str,
    overrides: ProviderOverrides,
) -> Result<ProviderProfile, String> {
    let mut profile = match provider {
        "google" => {
            if overrides.tenant.is_some() {
                return Err("--tenant applies only to the microsoft provider".into());
            }
            ProviderProfile {
                authorize_endpoint: "https://accounts.google.com/o/oauth2/v2/auth".into(),
                token_endpoint: "https://oauth2.googleapis.com/token".into(),
                scope: "https://mail.google.com/".into(),
                // Google returns a refresh token only for offline access, and
                // only on a consent screen shown in this request.
                extra_parameters: vec![("access_type", "offline"), ("prompt", "consent")],
                redirect_host: RedirectHost::Ipv4Loopback,
            }
        }
        "microsoft" => {
            let tenant = overrides
                .tenant
                .as_deref()
                .ok_or("--tenant is required for the microsoft provider")?;
            validate_tenant(tenant)?;
            ProviderProfile {
                authorize_endpoint: format!(
                    "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize"
                ),
                token_endpoint: format!(
                    "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"
                ),
                scope: "https://outlook.office.com/IMAP.AccessAsUser.All offline_access".into(),
                extra_parameters: Vec::new(),
                redirect_host: RedirectHost::Localhost,
            }
        }
        "custom" => {
            if overrides.tenant.is_some() {
                return Err("--tenant applies only to the microsoft provider".into());
            }
            ProviderProfile {
                authorize_endpoint: overrides
                    .authorize_endpoint
                    .clone()
                    .ok_or("--authorize-url is required for the custom provider")?,
                token_endpoint: overrides
                    .token_endpoint
                    .clone()
                    .ok_or("--token-url is required for the custom provider")?,
                scope: overrides
                    .scope
                    .clone()
                    .ok_or("--scope is required for the custom provider")?,
                extra_parameters: Vec::new(),
                redirect_host: RedirectHost::Ipv4Loopback,
            }
        }
        _ => return Err("provider must be google, microsoft, or custom".into()),
    };
    if provider != "custom" {
        if let Some(endpoint) = overrides.authorize_endpoint {
            profile.authorize_endpoint = endpoint;
        }
        if let Some(endpoint) = overrides.token_endpoint {
            profile.token_endpoint = endpoint;
        }
        if let Some(scope) = overrides.scope {
            profile.scope = scope;
        }
    }
    if let Some(host) = overrides.redirect_host {
        profile.redirect_host = host;
    }
    for (name, endpoint) in [
        ("authorization", &profile.authorize_endpoint),
        ("token", &profile.token_endpoint),
    ] {
        let url = reqwest::Url::parse(endpoint)
            .map_err(|error| format!("invalid OAuth {name} endpoint: {error}"))?;
        if url.scheme() != "https" || url.host_str().is_none_or(str::is_empty) {
            return Err(format!("the OAuth {name} endpoint must be an https:// URL"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(format!(
                "the OAuth {name} endpoint must not contain user information"
            ));
        }
    }
    if profile.scope.trim().is_empty() || profile.scope.chars().any(char::is_control) {
        return Err("the OAuth scope must be non-empty and contain no control characters".into());
    }
    Ok(profile)
}

fn validate_tenant(tenant: &str) -> Result<(), String> {
    if tenant.is_empty()
        || tenant.len() > 256
        || !tenant
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_'))
    {
        return Err(
            "--tenant must be a tenant ID, verified domain, organizations, or common".into(),
        );
    }
    Ok(())
}

/// PKCE verifier and its S256 challenge.
pub(crate) struct Pkce {
    pub(crate) verifier: Zeroizing<String>,
    pub(crate) challenge: String,
}

pub(crate) fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_token(bytes: usize) -> Result<Zeroizing<String>, String> {
    let mut buffer = Zeroizing::new(vec![0_u8; bytes]);
    SystemRandom::new()
        .fill(&mut buffer)
        .map_err(|_| "the operating system random source is unavailable".to_owned())?;
    Ok(Zeroizing::new(URL_SAFE_NO_PAD.encode(&*buffer)))
}

pub(crate) fn new_pkce() -> Result<Pkce, String> {
    // 32 random bytes encode to a 43-character verifier, the RFC 7636 minimum
    // length, drawn entirely from the unreserved character set.
    let verifier = random_token(32)?;
    let challenge = pkce_challenge(&verifier);
    Ok(Pkce {
        verifier,
        challenge,
    })
}

pub(crate) fn new_state() -> Result<Zeroizing<String>, String> {
    random_token(32)
}

pub(crate) fn redirect_uri(host: RedirectHost, port: u16) -> String {
    format!("http://{}:{port}/", host.as_str())
}

pub(crate) struct AuthorizationRequest<'a> {
    pub(crate) profile: &'a ProviderProfile,
    pub(crate) client_id: &'a str,
    pub(crate) redirect_uri: &'a str,
    pub(crate) state: &'a str,
    pub(crate) code_challenge: &'a str,
    pub(crate) login_hint: Option<&'a str>,
}

pub(crate) fn authorization_url(request: &AuthorizationRequest<'_>) -> Result<String, String> {
    let mut url = reqwest::Url::parse(&request.profile.authorize_endpoint)
        .map_err(|error| format!("invalid OAuth authorization endpoint: {error}"))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", request.client_id)
            .append_pair("redirect_uri", request.redirect_uri)
            .append_pair("scope", &request.profile.scope)
            .append_pair("state", request.state)
            .append_pair("code_challenge", request.code_challenge)
            .append_pair("code_challenge_method", "S256");
        for (name, value) in &request.profile.extra_parameters {
            query.append_pair(name, value);
        }
        if let Some(hint) = request.login_hint {
            query.append_pair("login_hint", hint);
        }
    }
    Ok(url.into())
}

/// Loopback listeners for one redirect port. `localhost` may resolve to
/// either loopback family in the operator's browser, so both are bound on the
/// same port when that host is registered.
pub(crate) struct RedirectListener {
    listeners: Vec<TcpListener>,
    port: u16,
}

impl RedirectListener {
    pub(crate) fn bind(host: RedirectHost) -> Result<Self, String> {
        let ipv4 = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("could not open the loopback redirect listener: {error}"))?;
        let port = ipv4
            .local_addr()
            .map_err(|error| format!("could not read the redirect listener port: {error}"))?
            .port();
        let mut listeners = vec![ipv4];
        if host == RedirectHost::Localhost {
            // Best effort: a host without IPv6 loopback still serves 127.0.0.1.
            if let Ok(ipv6) = TcpListener::bind(("::1", port)) {
                listeners.push(ipv6);
            }
        }
        for listener in &listeners {
            listener
                .set_nonblocking(true)
                .map_err(|error| format!("could not configure the redirect listener: {error}"))?;
        }
        Ok(Self { listeners, port })
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// Wait for the provider's redirect and return the authorization code.
    pub(crate) fn wait_for_code(
        &self,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<Zeroizing<String>, String> {
        let deadline = Instant::now() + timeout;
        let mut unrelated = 0_usize;
        loop {
            let mut accepted = None;
            for listener in &self.listeners {
                match listener.accept() {
                    Ok((stream, _)) => {
                        accepted = Some(stream);
                        break;
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                    Err(error) => {
                        return Err(format!("redirect listener failed: {error}"));
                    }
                }
            }
            let Some(mut stream) = accepted else {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "no authorization redirect arrived within {} seconds",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };
            match handle_redirect(&mut stream, expected_state) {
                RedirectOutcome::Code(code) => {
                    respond(
                        &mut stream,
                        "200 OK",
                        "MailSwiftSync received the authorization. You can close this window.",
                    );
                    return Ok(code);
                }
                RedirectOutcome::Rejected(error) => {
                    respond(
                        &mut stream,
                        "400 Bad Request",
                        "MailSwiftSync did not accept this authorization. Return to the terminal for details.",
                    );
                    return Err(error);
                }
                RedirectOutcome::Unrelated => {
                    respond(&mut stream, "404 Not Found", "Not found.");
                    unrelated += 1;
                    if unrelated > MAX_UNRELATED_REQUESTS {
                        return Err(
                            "too many unrelated requests reached the redirect listener".into()
                        );
                    }
                }
            }
        }
    }
}

enum RedirectOutcome {
    Code(Zeroizing<String>),
    Rejected(String),
    Unrelated,
}

fn handle_redirect(stream: &mut TcpStream, expected_state: &str) -> RedirectOutcome {
    let head = match read_request_head(stream) {
        Ok(head) => head,
        Err(error) => return RedirectOutcome::Rejected(error),
    };
    parse_redirect_request(&head, expected_state)
}

fn read_request_head(stream: &mut TcpStream) -> Result<Zeroizing<String>, String> {
    stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(REDIRECT_READ_TIMEOUT)))
        .map_err(|error| format!("could not read the authorization redirect: {error}"))?;
    let mut head = Zeroizing::new(Vec::new());
    let mut buffer = [0_u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| format!("could not read the authorization redirect: {error}"))?;
        if count == 0 {
            break;
        }
        head.extend_from_slice(&buffer[..count]);
        if head.len() > MAX_REDIRECT_REQUEST_BYTES {
            return Err("the authorization redirect request is too large".into());
        }
    }
    String::from_utf8(head.to_vec())
        .map(Zeroizing::new)
        .map_err(|_| "the authorization redirect request is not valid UTF-8".into())
}

fn parse_redirect_request(head: &str, expected_state: &str) -> RedirectOutcome {
    let Some(request_line) = head.lines().next() else {
        return RedirectOutcome::Rejected("the authorization redirect request was empty".into());
    };
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return RedirectOutcome::Rejected("the authorization redirect request is malformed".into());
    };
    let Ok(url) = reqwest::Url::parse(&format!("http://loopback{target}")) else {
        return RedirectOutcome::Unrelated;
    };
    if method != "GET" || url.path() != "/" {
        return RedirectOutcome::Unrelated;
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut error_description = None;
    for (name, value) in url.query_pairs() {
        let slot = match name.as_ref() {
            "code" => &mut code,
            "state" => &mut state,
            "error" => &mut error,
            "error_description" => &mut error_description,
            _ => continue,
        };
        if slot.replace(Zeroizing::new(value.into_owned())).is_some() {
            return RedirectOutcome::Rejected(format!(
                "the authorization redirect repeated the {name} parameter"
            ));
        }
    }
    if code.is_none() && state.is_none() && error.is_none() {
        return RedirectOutcome::Unrelated;
    }
    // Check state before anything else: an unauthenticated request to the
    // loopback port must not be able to report a provider error either.
    if !state
        .as_deref()
        .is_some_and(|state| constant_time_eq(state.as_bytes(), expected_state.as_bytes()))
    {
        return RedirectOutcome::Rejected(
            "the authorization redirect state did not match this request; nothing was stored"
                .into(),
        );
    }
    if let Some(error) = error {
        let description = error_description
            .as_deref()
            .map(|value| {
                value
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(512)
                    .collect::<String>()
            })
            .filter(|value| !value.is_empty());
        return RedirectOutcome::Rejected(match description {
            Some(description) => format!(
                "the provider declined authorization ({}): {description}",
                error.as_str()
            ),
            None => format!("the provider declined authorization ({})", error.as_str()),
        });
    }
    match code {
        Some(code) if !code.is_empty() && code.len() <= MAX_AUTHORIZATION_CODE_BYTES => {
            RedirectOutcome::Code(code)
        }
        _ => RedirectOutcome::Rejected(
            "the authorization redirect did not carry a usable code".into(),
        ),
    }
}

/// Compare without an early exit on the first differing byte. The expected
/// state has a fixed public length, so only the contents need protection.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}

fn respond(stream: &mut TcpStream, status: &str, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>MailSwiftSync</title><p>{message}</p>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

pub(crate) struct CodeExchange<'a> {
    pub(crate) token_endpoint: &'a str,
    pub(crate) client_id: &'a str,
    pub(crate) client_secret: Option<&'a str>,
    pub(crate) code: &'a str,
    pub(crate) redirect_uri: &'a str,
    pub(crate) code_verifier: &'a str,
}

pub(crate) fn code_exchange_form<'a>(exchange: &'a CodeExchange<'a>) -> Vec<(&'a str, &'a str)> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", exchange.code),
        ("redirect_uri", exchange.redirect_uri),
        ("client_id", exchange.client_id),
        ("code_verifier", exchange.code_verifier),
    ];
    if let Some(secret) = exchange.client_secret.filter(|secret| !secret.is_empty()) {
        form.push(("client_secret", secret));
    }
    form
}

/// Exchange the authorization code and build the refresh configuration that
/// live launches consume. A response without a refresh token is refused:
/// storing only a short-lived access token would fail on the next launch.
pub(crate) fn exchange_code(exchange: &CodeExchange<'_>) -> Result<OAuthRefreshConfig, String> {
    let token = post_token_request(
        exchange.token_endpoint,
        &code_exchange_form(exchange),
        "mailswiftsync-oauth-authorize",
    )?;
    refresh_config_from_token(exchange, token)
}

fn refresh_config_from_token(
    exchange: &CodeExchange<'_>,
    token: RefreshedToken,
) -> Result<OAuthRefreshConfig, String> {
    let refresh_token = token.refresh_token.ok_or(
        "the provider issued no refresh token; request offline access (Google) or the offline_access scope (Microsoft) and consent again",
    )?;
    Ok(OAuthRefreshConfig {
        token_endpoint: exchange.token_endpoint.to_owned(),
        client_id: exchange.client_id.to_owned(),
        client_secret: SecretString::from(exchange.client_secret.unwrap_or_default().to_owned()),
        refresh_token,
    })
}

/// Store the configuration under an OAuth refresh keyring ID and read it back,
/// so a keyring backend that silently discards writes is reported now rather
/// than at the next live launch.
pub(crate) fn store_refresh_config(
    keyring_id: &str,
    config: &OAuthRefreshConfig,
) -> Result<(), String> {
    let entry = keyring::Entry::new(crate::Form::OAUTH_REFRESH_KEYRING_SERVICE, keyring_id)
        .map_err(|error| format!("Could not open OS keyring entry `{keyring_id}`: {error}"))?;
    entry
        .set_password(&crate::oauth_refresh::encode_refresh_config(config))
        .map_err(|error| format!("Could not store the OAuth refresh configuration: {error}"))?;
    let stored = Zeroizing::new(entry.get_password().map_err(|error| {
        format!("The OAuth refresh configuration could not be read back from the keyring: {error}")
    })?);
    let decoded = crate::oauth_refresh::decode_refresh_config(&stored)?;
    if decoded.refresh_token.as_str() != config.refresh_token.as_str() {
        return Err("The OS keyring returned a different OAuth refresh configuration".into());
    }
    Ok(())
}

pub(crate) fn validate_keyring_id(id: &str) -> Result<(), String> {
    if id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err("the keyring ID must be 1-256 characters without control characters".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn google() -> ProviderProfile {
        provider_profile("google", ProviderOverrides::default()).unwrap()
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636_appendix_b() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_pkce_verifier_meets_rfc_length_and_alphabet() {
        let pkce = new_pkce().unwrap();
        assert!((43..=128).contains(&pkce.verifier.len()));
        assert!(
            pkce.verifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        );
        assert_eq!(pkce.challenge, pkce_challenge(&pkce.verifier));
        assert_ne!(new_state().unwrap().as_str(), new_state().unwrap().as_str());
    }

    #[test]
    fn provider_presets_and_overrides_are_validated() {
        let google = google();
        assert_eq!(google.scope, "https://mail.google.com/");
        assert!(
            google
                .extra_parameters
                .contains(&("access_type", "offline"))
        );
        assert_eq!(google.redirect_host, RedirectHost::Ipv4Loopback);

        let microsoft = provider_profile(
            "microsoft",
            ProviderOverrides {
                tenant: Some("contoso.onmicrosoft.com".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            microsoft.token_endpoint,
            "https://login.microsoftonline.com/contoso.onmicrosoft.com/oauth2/v2.0/token"
        );
        assert!(microsoft.scope.contains("offline_access"));
        assert_eq!(microsoft.redirect_host, RedirectHost::Localhost);

        assert!(provider_profile("microsoft", ProviderOverrides::default()).is_err());
        for tenant in ["", "a/b", "x?y=1", "tenant id"] {
            let result = provider_profile(
                "microsoft",
                ProviderOverrides {
                    tenant: Some(tenant.into()),
                    ..Default::default()
                },
            );
            assert!(result.is_err(), "{tenant}");
        }
        assert!(provider_profile("custom", ProviderOverrides::default()).is_err());
        assert!(
            provider_profile(
                "custom",
                ProviderOverrides {
                    authorize_endpoint: Some("http://idp.example/authorize".into()),
                    token_endpoint: Some("https://idp.example/token".into()),
                    scope: Some("imap".into()),
                    ..Default::default()
                },
            )
            .is_err(),
            "plain-HTTP endpoints are refused"
        );
        assert!(provider_profile("yahoo", ProviderOverrides::default()).is_err());
    }

    #[test]
    fn authorization_url_carries_pkce_state_and_provider_parameters() {
        let profile = google();
        let url = authorization_url(&AuthorizationRequest {
            profile: &profile,
            client_id: "client id",
            redirect_uri: "http://127.0.0.1:4242/",
            state: "state-value",
            code_challenge: "challenge",
            login_hint: Some("user@example.com"),
        })
        .unwrap();
        let parsed = reqwest::Url::parse(&url).unwrap();
        let query = parsed
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(parsed.host_str(), Some("accounts.google.com"));
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["client_id"], "client id");
        assert_eq!(query["redirect_uri"], "http://127.0.0.1:4242/");
        assert_eq!(query["scope"], "https://mail.google.com/");
        assert_eq!(query["state"], "state-value");
        assert_eq!(query["code_challenge"], "challenge");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["access_type"], "offline");
        assert_eq!(query["login_hint"], "user@example.com");
    }

    fn redirect_with(request: &'static [u8], state: &str) -> Result<Zeroizing<String>, String> {
        let listener = RedirectListener::bind(RedirectHost::Ipv4Loopback).unwrap();
        let port = listener.port();
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream.write_all(request).unwrap();
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            response
        });
        let result = listener.wait_for_code(state, Duration::from_secs(5));
        client.join().unwrap();
        result
    }

    #[test]
    fn redirect_with_matching_state_yields_the_code() {
        let code = redirect_with(
            b"GET /?state=expected&code=4%2F0Abc HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "expected",
        )
        .unwrap();
        assert_eq!(code.as_str(), "4/0Abc");
    }

    #[test]
    fn redirect_with_wrong_or_missing_state_is_refused() {
        let error =
            redirect_with(b"GET /?state=forged&code=abc HTTP/1.1\r\n\r\n", "expected").unwrap_err();
        assert!(error.contains("state did not match"), "{error}");
        let error = redirect_with(b"GET /?code=abc HTTP/1.1\r\n\r\n", "expected").unwrap_err();
        assert!(error.contains("state did not match"), "{error}");
    }

    #[test]
    fn provider_error_is_reported_only_with_a_valid_state() {
        let error = redirect_with(
            b"GET /?state=expected&error=access_denied&error_description=User%20declined HTTP/1.1\r\n\r\n",
            "expected",
        )
        .unwrap_err();
        assert!(
            error.contains("access_denied") && error.contains("User declined"),
            "{error}"
        );
    }

    #[test]
    fn repeated_parameters_are_refused() {
        let error = redirect_with(
            b"GET /?state=expected&code=a&code=b HTTP/1.1\r\n\r\n",
            "expected",
        )
        .unwrap_err();
        assert!(error.contains("repeated"), "{error}");
    }

    #[test]
    fn unrelated_requests_are_answered_and_skipped() {
        let listener = RedirectListener::bind(RedirectHost::Ipv4Loopback).unwrap();
        let port = listener.port();
        let client = thread::spawn(move || {
            for request in [
                &b"GET /favicon.ico HTTP/1.1\r\n\r\n"[..],
                &b"GET /?state=expected&code=good HTTP/1.1\r\n\r\n"[..],
            ] {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                stream.write_all(request).unwrap();
                let mut response = String::new();
                let _ = stream.read_to_string(&mut response);
            }
        });
        let code = listener
            .wait_for_code("expected", Duration::from_secs(5))
            .unwrap();
        client.join().unwrap();
        assert_eq!(code.as_str(), "good");
    }

    #[test]
    fn redirect_wait_times_out() {
        let listener = RedirectListener::bind(RedirectHost::Ipv4Loopback).unwrap();
        let error = listener
            .wait_for_code("expected", Duration::from_millis(200))
            .unwrap_err();
        assert!(error.contains("no authorization redirect"), "{error}");
    }

    #[test]
    fn code_exchange_sends_verifier_and_optional_secret() {
        let exchange = CodeExchange {
            token_endpoint: "https://oauth2.googleapis.com/token",
            client_id: "client",
            client_secret: Some("secret"),
            code: "code",
            redirect_uri: "http://127.0.0.1:4242/",
            code_verifier: "verifier",
        };
        let form = code_exchange_form(&exchange);
        assert!(form.contains(&("grant_type", "authorization_code")));
        assert!(form.contains(&("code_verifier", "verifier")));
        assert!(form.contains(&("redirect_uri", "http://127.0.0.1:4242/")));
        assert!(form.contains(&("client_secret", "secret")));
        let public = CodeExchange {
            client_secret: None,
            ..exchange
        };
        assert!(
            !code_exchange_form(&public)
                .iter()
                .any(|(name, _)| *name == "client_secret")
        );
    }

    #[test]
    fn token_without_refresh_token_is_refused() {
        let exchange = CodeExchange {
            token_endpoint: "https://oauth2.googleapis.com/token",
            client_id: "client",
            client_secret: None,
            code: "code",
            redirect_uri: "http://127.0.0.1:4242/",
            code_verifier: "verifier",
        };
        let token = RefreshedToken {
            access_token: SecretString::from("access".to_owned()),
            refresh_token: None,
            expires_in: Some(3600),
        };
        let error = refresh_config_from_token(&exchange, token).unwrap_err();
        assert!(error.contains("no refresh token"), "{error}");

        let token = RefreshedToken {
            access_token: SecretString::from("access".to_owned()),
            refresh_token: Some(SecretString::from("refresh".to_owned())),
            expires_in: None,
        };
        let config = refresh_config_from_token(&exchange, token).unwrap();
        assert_eq!(config.refresh_token.as_str(), "refresh");
        assert_eq!(config.token_endpoint, "https://oauth2.googleapis.com/token");
    }
}
